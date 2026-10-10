//! Fake implementations for headless tests: no QEMU process, no VM files.
//! The core (`vm_core::Core`) is real; the VM backend, the host (Emergency
//! Stop, agent sessions) and, optionally, the catalog and audit in memory are
//! fake. They are not part of the product: the binary always uses the real
//! adapters ([`crate::adapters`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use lightvm_proto::ipc::StopReason;
use lightvm_proto::{
    AuditEvent, BackendObservation, Caller, DisplayGeometry, GuestOs, HostCapacity, ImageFormat,
    InputEvent, Label, ManagedRelPath, MediaEntry, MediaId, MediaKind, MediaRecord, NetworkProfile,
    OperationId, OperationRecord, PublicError, RunEndReason, RunId, RunRecord, Screenshot,
    Sha256Digest, UtcTimestamp, VmConfig, VmId, VmName, VmRecord, VmState,
};
use vm_core::media::{ObservedMediaFacts, VerifiedMedia};
use vm_core::ports::{AuditSink, CatalogStore, MediaInspector, VmBackend};

use crate::engine::{
    BackendPort, CoreEngine, ImportCopied, ImportJob, MediaImportPort, VmEngine, core_config,
};
use crate::supervisor::{Backend, EmergencyOutcome, EmergencyStop, ParentLossPolicy};

/// Log of the calls received by the fake implementations.
pub type Calls = Arc<Mutex<Vec<String>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn push(calls: &Calls, s: impl Into<String>) {
    lock(calls).push(s.into());
}

/// Gate that blocks the fake backend until it is opened (queue tests).
#[derive(Debug, Default)]
pub struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    /// Closed gate.
    pub fn closed() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Opens the gate.
    pub fn open(&self) {
        *lock(&self.open) = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let mut g = lock(&self.open);
        while !*g {
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Fake host capacity: accelerator, capture and input available.
pub fn host_capacity() -> HostCapacity {
    HostCapacity {
        logical_cpus: 8,
        total_memory_mib: 16 * 1024,
        reserved_memory_mib: 2 * 1024,
        free_storage_gib: 512,
        reserved_storage_gib: 16,
        accelerator_available: true,
        screenshot_available: true,
        input_available: true,
        baseline_available: false,
    }
}

/// Geometry of the fake display.
pub const FAKE_GEOMETRY: DisplayGeometry = DisplayGeometry {
    width: 1280,
    height: 800,
};

/// Test configuration valid for the MVP limits.
pub fn sample_config(name: &str) -> VmConfig {
    VmConfig {
        name: VmName::parse(name).expect("valid test name"),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: lightvm_proto::Firmware::Uefi,
        limits: Default::default(),
    }
}

#[derive(Debug, Default)]
struct MemoryInner {
    vms: BTreeMap<VmId, VmRecord>,
    media: BTreeMap<MediaId, MediaEntry>,
    ops: BTreeMap<OperationId, OperationRecord>,
    runs: BTreeMap<RunId, RunRecord>,
}

/// In-memory catalog.
#[derive(Debug, Default)]
pub struct MemoryStore(MemoryInner);

impl CatalogStore for MemoryStore {
    fn insert_vm(&mut self, vm: &VmRecord) -> Result<(), PublicError> {
        self.0.vms.insert(vm.id, vm.clone());
        Ok(())
    }
    fn get_vm(&self, id: VmId) -> Result<Option<VmRecord>, PublicError> {
        Ok(self.0.vms.get(&id).cloned())
    }
    fn update_vm(&mut self, vm: &VmRecord, expected_revision: u64) -> Result<(), PublicError> {
        match self.0.vms.get_mut(&vm.id) {
            Some(v) if v.config_revision == expected_revision => {
                *v = vm.clone();
                Ok(())
            }
            Some(_) => Err(PublicError::state_conflict("different revision")),
            None => Err(PublicError::not_found("VM not found")),
        }
    }
    fn list_vms(&self) -> Result<Vec<VmRecord>, PublicError> {
        Ok(self.0.vms.values().cloned().collect())
    }
    fn set_persisted_state(&mut self, id: VmId, state: VmState) -> Result<(), PublicError> {
        if let Some(v) = self.0.vms.get_mut(&id) {
            v.persisted_state = state;
        }
        Ok(())
    }
    fn insert_media(&mut self, entry: &MediaEntry) -> Result<(), PublicError> {
        self.0.media.insert(entry.record.id, entry.clone());
        Ok(())
    }
    fn get_media(&self, id: MediaId) -> Result<Option<MediaEntry>, PublicError> {
        Ok(self.0.media.get(&id).cloned())
    }
    fn upsert_operation(&mut self, op: &OperationRecord) -> Result<(), PublicError> {
        self.0.ops.insert(op.id, op.clone());
        Ok(())
    }
    fn get_operation(&self, id: OperationId) -> Result<Option<OperationRecord>, PublicError> {
        Ok(self.0.ops.get(&id).cloned())
    }
    fn insert_run(&mut self, run: &RunRecord) -> Result<(), PublicError> {
        self.0.runs.insert(run.id, run.clone());
        Ok(())
    }
    fn finish_run(
        &mut self,
        id: RunId,
        ended_at: UtcTimestamp,
        reason: RunEndReason,
    ) -> Result<(), PublicError> {
        if let Some(r) = self.0.runs.get_mut(&id) {
            r.ended_at = Some(ended_at);
            r.end_reason = Some(reason);
        }
        Ok(())
    }
    fn list_media(&self) -> Result<Vec<MediaRecord>, PublicError> {
        Ok(self.0.media.values().map(|e| e.record.clone()).collect())
    }
    fn remove_vm(&mut self, id: VmId) -> Result<(), PublicError> {
        self.0.vms.remove(&id);
        self.0.runs.retain(|_, r| r.vm_id != id);
        self.0.ops.retain(|_, o| o.vm_id != id);
        Ok(())
    }
    fn vm_disk_usage(&self, id: VmId) -> Option<u64> {
        self.0.vms.contains_key(&id).then_some(0)
    }
}

/// Import without a managed copy: hash and size of the source computed on
/// the worker thread, registry row in memory.
impl MediaImportPort for MemoryStore {
    fn begin_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &Label,
    ) -> Result<ImportJob, PublicError> {
        let source = source.to_path_buf();
        let origin = declared_origin.clone();
        Ok(ImportJob::new(move || {
            let r = fake_entry(&source, kind, origin);
            ImportCopied(Box::new(r))
        }))
    }

    fn finish_import(&mut self, copied: ImportCopied) -> Result<MediaEntry, PublicError> {
        let r = copied
            .0
            .downcast::<Result<MediaEntry, PublicError>>()
            .map_err(|_| PublicError::infrastructure("foreign import"))?;
        let entry = (*r)?;
        self.insert_media(&entry)?;
        Ok(entry)
    }

    fn abort_import(&mut self, copied: ImportCopied) -> Result<(), PublicError> {
        copied
            .0
            .downcast::<Result<MediaEntry, PublicError>>()
            .map(|_| ())
            .map_err(|_| PublicError::infrastructure("foreign import"))
    }
}

fn fake_entry(
    source: &Path,
    kind: MediaKind,
    declared_origin: Label,
) -> Result<MediaEntry, PublicError> {
    let size = std::fs::metadata(source)
        .map_err(|_| PublicError::not_found("media file not found"))?
        .len();
    let hex = qemu_backend::probe::sha256_file(source)
        .map_err(|_| PublicError::infrastructure("hash"))?;
    let id = MediaId::new_v7();
    Ok(MediaEntry {
        record: MediaRecord {
            id,
            kind,
            sha256: Sha256Digest::parse_hex(&hex)
                .map_err(|_| PublicError::infrastructure("hash"))?,
            size_bytes: size,
            declared_origin,
            approved_at: UtcTimestamp::now(),
        },
        managed_path: ManagedRelPath::parse(format!("media/{id}.iso"))
            .map_err(|_| PublicError::infrastructure("path"))?,
        file_identity: None,
    })
}

/// In-memory audit, shared with the test.
#[derive(Debug, Clone, Default)]
pub struct MemoryAudit(pub Arc<Mutex<Vec<AuditEvent>>>);

impl MemoryAudit {
    /// Copy of the recorded events.
    pub fn events(&self) -> Vec<AuditEvent> {
        lock(&self.0).clone()
    }
}

impl AuditSink for MemoryAudit {
    fn append(&mut self, event: &AuditEvent) -> Result<(), PublicError> {
        lock(&self.0).push(event.clone());
        Ok(())
    }
}

/// Fake media inspector: reports the registered facts (no file).
#[derive(Debug, Default)]
pub struct FakeInspector;

impl MediaInspector for FakeInspector {
    fn inspect(&mut self, entry: &MediaEntry) -> Result<ObservedMediaFacts, PublicError> {
        Ok(ObservedMediaFacts {
            sha256: entry.record.sha256,
            size_bytes: entry.record.size_bytes,
            file_identity: entry.file_identity,
            read_only: true,
        })
    }
}

/// Fake VM backend: logs the calls and simulates processes that are always
/// ready. An optional gate blocks reading the observations (done on every
/// request that touches the engine) for queue tests.
#[derive(Debug, Default)]
pub struct FakeVmBackend {
    /// Received calls (shared with [`FakeHost`]).
    pub calls: Calls,
    /// Optional gate.
    pub gate: Option<Arc<Gate>>,
    /// Observations to deliver to the core (simulated exits).
    pub observations: Arc<Mutex<Vec<(VmId, BackendObservation)>>>,
    /// Display socket returned for every run (a fake RFB server in tests).
    pub display_socket: Option<PathBuf>,
    /// Graceful stops in the background (like QEMU): `stop` returns at
    /// once and the outcome is injected by the test in `stop_results`.
    pub async_stop: bool,
    /// Outcomes of the background stops, injected by the test.
    pub stop_results: StopResults,
    /// Background stops in progress (filled by the backend).
    pub stopping: BTreeMap<VmId, RunId>,
    /// VMs whose process the test declares terminated by the priority path
    /// (Emergency Stop): as with QEMU, their stop outcome and exit are not
    /// reported, they are left to `after_emergency_stop` (`force_stop`).
    pub emergency_killed: Arc<Mutex<std::collections::BTreeSet<VmId>>>,
}

/// Outcomes of background graceful stops injected by a test.
pub type StopResults = Arc<Mutex<Vec<(VmId, Result<(), PublicError>)>>>;

impl VmBackend for FakeVmBackend {
    fn start(
        &mut self,
        vm: &VmRecord,
        _run_id: RunId,
        media: &[VerifiedMedia],
    ) -> Result<(), PublicError> {
        push(
            &self.calls,
            format!("vm_start:{}:media={}", vm.config.name, media.len()),
        );
        Ok(())
    }
    fn shutdown(&mut self, _vm_id: VmId, _run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_shutdown");
        Ok(())
    }
    fn force_stop(&mut self, vm_id: VmId, _run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_force_stop");
        self.stopping.remove(&vm_id);
        lock(&self.emergency_killed).remove(&vm_id);
        Ok(())
    }
    fn reset(&mut self, _vm_id: VmId, _run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_reset");
        Ok(())
    }
    fn screenshot(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        _max_bytes: u64,
    ) -> Result<Screenshot, PublicError> {
        push(&self.calls, "vm_screenshot");
        Ok(Screenshot {
            vm_id,
            run_id,
            geometry: FAKE_GEOMETRY,
            format: ImageFormat::Png,
            sha256: Sha256Digest::from_bytes([0; 32]),
            byte_len: 0,
            captured_at: UtcTimestamp::now(),
        })
    }
    fn send_input(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<(), PublicError> {
        push(
            &self.calls,
            format!(
                "vm_input:{}x{}:{}",
                geometry.width,
                geometry.height,
                events.len()
            ),
        );
        Ok(())
    }
}

impl BackendPort for FakeVmBackend {
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)> {
        if let Some(g) = &self.gate {
            g.wait();
        }
        let killed = lock(&self.emergency_killed).clone();
        let mut obs = lock(&self.observations);
        let (keep, out): (Vec<_>, Vec<_>) = obs.drain(..).partition(|(vm, _)| killed.contains(vm));
        *obs = keep;
        out
    }
    fn display_socket(&self, _vm_id: VmId, _run_id: RunId) -> Option<PathBuf> {
        self.display_socket.clone()
    }
    fn refresh_geometry(&mut self, _vm_id: VmId, _run_id: RunId) -> Option<DisplayGeometry> {
        push(&self.calls, "vm_refresh_geometry");
        Some(FAKE_GEOMETRY)
    }
    fn release_input(&mut self, _vm_id: VmId, _run_id: RunId) {
        push(&self.calls, "vm_release_input");
    }
    fn begin_shutdown(&mut self, vm_id: VmId, run_id: RunId) -> Option<Result<(), PublicError>> {
        if !self.async_stop {
            return None;
        }
        push(&self.calls, "vm_shutdown_begin");
        self.stopping.insert(vm_id, run_id);
        Some(Ok(()))
    }
    fn finished_shutdowns(&mut self) -> Vec<(VmId, RunId, Result<(), PublicError>)> {
        let killed = lock(&self.emergency_killed).clone();
        let injected = std::mem::take(&mut *lock(&self.stop_results));
        injected
            .into_iter()
            .filter_map(|(vm, r)| {
                let run = self.stopping.remove(&vm)?;
                // Overtaken by the Emergency Stop: left to `force_stop`.
                (!killed.contains(&vm)).then_some((vm, run, r))
            })
            .collect()
    }
}

/// Fake host (service): logs the calls.
#[derive(Debug, Default)]
pub struct FakeHost {
    /// Received calls.
    pub calls: Calls,
}

#[derive(Debug)]
struct FakeEmergency {
    calls: Calls,
}

impl EmergencyStop for FakeEmergency {
    fn emergency_stop_all(&self) -> EmergencyOutcome {
        push(&self.calls, "emergency_stop_all");
        EmergencyOutcome {
            processes_terminated: 0,
            completed: true,
        }
    }
}

impl Backend for FakeHost {
    fn name(&self) -> String {
        "fake".into()
    }
    fn accelerator(&self) -> Option<String> {
        None
    }
    fn emergency_handle(&self) -> Arc<dyn EmergencyStop> {
        Arc::new(FakeEmergency {
            calls: self.calls.clone(),
        })
    }
    fn stop_agent_sessions(&mut self, reason: StopReason) {
        push(&self.calls, format!("stop_agent_sessions:{reason:?}"));
    }
    fn on_parent_lost(&mut self, policy: ParentLossPolicy) {
        push(&self.calls, format!("on_parent_lost:{policy:?}"));
    }
    fn shutdown(&mut self) {
        push(&self.calls, "shutdown");
    }
}

/// Engine with a real core, in-memory catalog and audit, fake backend.
pub type MemoryEngine = CoreEngine<MemoryStore, MemoryAudit, FakeVmBackend, FakeInspector>;

/// In-memory engine with a powered-off sample VM (`sample`).
pub fn sample_engine(backend: FakeVmBackend, audit: MemoryAudit) -> MemoryEngine {
    let mut e = CoreEngine::new(
        MemoryStore::default(),
        audit,
        backend,
        FakeInspector,
        core_config(host_capacity()),
    )
    .expect("in-memory core");
    e.create_vm(&Caller::User, sample_config("sample"))
        .expect("sample VM");
    let _ = e.state_changes();
    e
}
