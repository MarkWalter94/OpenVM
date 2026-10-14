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
    InputEvent, Label, MediaEntry, MediaId, MediaKind, MediaLocation, MediaRecord, NetworkProfile,
    OperationId, OperationRecord, PublicError, RunEndReason, RunId, RunRecord, Screenshot,
    Sha256Digest, UtcTimestamp, VmConfig, VmId, VmName, VmRecord, VmState,
};
use vm_core::media::{ObservedMediaFacts, VerifiedMedia};
use vm_core::ports::{AuditSink, CatalogStore, MediaInspector, VmBackend};

use crate::engine::{
    BackendPort, BaselinePort, CoreEngine, GuardedMedia, ImageTool, ImportCopied, ImportJob,
    MediaGuardPort, MediaImportPort, StartDisks, VmEngine, core_config,
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

    /// Closes the gate again: the fake backend blocks at its next
    /// observation (a stuck supervisor, F4 stop-latency tests).
    pub fn close(&self) {
        *lock(&self.open) = false;
    }

    /// `true` once opened.
    pub fn is_open(&self) -> bool {
        *lock(&self.open)
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
        imported: None,
    }
}

/// Test configuration for `guest_os` with `firmware` (ADR-0027) and the
/// suggested resources of that guest (within the MVP limits).
pub fn sample_guest_config(
    name: &str,
    guest_os: GuestOs,
    firmware: lightvm_proto::Firmware,
) -> VmConfig {
    let (vcpus, memory_mib, disk_gib) = guest_os.suggested_resources();
    VmConfig {
        guest_os,
        firmware,
        vcpus,
        memory_mib,
        disk_gib,
        ..sample_config(name)
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
    fn remove_media(&mut self, id: MediaId) -> Result<(), PublicError> {
        self.0
            .media
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| PublicError::not_found("medium not found"))
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

/// No baselines in memory: listings empty, operations `unsupported` (the
/// baseline tests use the SQLite catalog with [`FakeImageTool`]).
impl BaselinePort for MemoryStore {}

/// No VMware imports in memory (the import tests use the SQLite catalog).
impl crate::engine::VmImportPort for MemoryStore {}

/// Image tool without `qemu-img` for the baseline tests (F3): files with a
/// minimal QCOW2 v3 header (4 KiB) followed by the "guest data". `convert`
/// writes the guest-visible content of the source (an overlay without data
/// shows its base's) as a standalone image; `compare` compares contents;
/// `create_overlay` writes a header with the backing reference. Optional
/// delay per `convert` (cancellation tests) and failure injection.
#[derive(Debug, Default)]
pub struct FakeImageTool {
    /// Delay of `convert`, polled for cancellation every 10 ms.
    pub convert_delay: std::time::Duration,
    /// Calls received.
    pub calls: Calls,
    /// F6 disk-full injection: while set, `convert` writes part of the
    /// output and fails as `qemu-img` does on a full volume (ENOSPC).
    pub disk_full: Arc<std::sync::atomic::AtomicBool>,
    /// VMware import tests (ADR-0030): the real `qemu-img` of the approved
    /// build and a private directory for its outputs; the VMware operations
    /// (`info`, `convert -p`) and the `check`/`virtual_size` of their
    /// results then run on real images. `None` = VMware import unsupported.
    pub vmdk_tool: Option<Arc<crate::adapters::backend::QemuImageTool>>,
    /// Delay before each VMware conversion, polled for cancellation every
    /// 10 ms (cancellation and crash tests).
    pub vmdk_delay: std::time::Duration,
    /// Free bytes reported for the catalog volume (budget tests); `None` =
    /// the real free space when `vmdk_tool` is set.
    pub free_bytes: Option<u64>,
}

/// Minimal QCOW2 v3 header with an optional backing reference (format
/// `qcow2`), followed by `payload`.
pub fn fake_qcow2(backing: Option<&str>, payload: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 4096];
    h[0..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xfb]);
    h[4..8].copy_from_slice(&3u32.to_be_bytes());
    h[100..104].copy_from_slice(&104u32.to_be_bytes());
    if let Some(name) = backing {
        let fmt = b"qcow2";
        h[104..108].copy_from_slice(&0xe279_2acau32.to_be_bytes());
        h[108..112].copy_from_slice(&5u32.to_be_bytes());
        h[112..117].copy_from_slice(fmt);
        let off = 128usize;
        h[8..16].copy_from_slice(&(off as u64).to_be_bytes());
        let len = u32::try_from(name.len()).unwrap_or(0);
        h[16..20].copy_from_slice(&len.to_be_bytes());
        h[off..off + name.len()].copy_from_slice(name.as_bytes());
    }
    h.extend_from_slice(payload);
    h
}

/// `true` for a QCOW2 file written by the real `qemu-img` (a fake image is
/// a bare 4 KiB header followed by its payload, without a cluster size).
fn is_real_qcow2(p: &Path) -> bool {
    use std::io::Read as _;
    let mut h = [0u8; 24];
    std::fs::File::open(p)
        .and_then(|mut f| f.read_exact(&mut h))
        .is_ok_and(|()| h[..4] == [0x51, 0x46, 0x49, 0xfb] && h[20..24] != [0, 0, 0, 0])
}

/// Guest-visible content of a fake image (see [`FakeImageTool`]).
pub fn fake_content(p: &Path) -> Result<Vec<u8>, PublicError> {
    let bytes = std::fs::read(p).map_err(|_| PublicError::infrastructure("fake image read"))?;
    let payload = bytes.get(4096..).unwrap_or_default().to_vec();
    if !payload.is_empty() {
        return Ok(payload);
    }
    match vm_storage::qcow2_backing(p) {
        Ok(Some((r, _))) => {
            let base = r
                .split('/')
                .fold(p.parent().unwrap_or(p).to_path_buf(), |b, c| b.join(c));
            fake_content(&base)
        }
        _ => Ok(payload),
    }
}

impl ImageTool for FakeImageTool {
    fn convert(
        &self,
        src: &Path,
        dst: &Path,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), PublicError> {
        push(&self.calls, "convert");
        let t0 = std::time::Instant::now();
        while t0.elapsed() < self.convert_delay {
            if cancel() {
                return Err(PublicError::state_conflict("operation cancelled"));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if cancel() {
            return Err(PublicError::state_conflict("operation cancelled"));
        }
        let content = fake_content(src)?;
        if self.disk_full.load(std::sync::atomic::Ordering::SeqCst) {
            // Partial output, then the error of the real tool for
            // "No space left on device".
            let _ = std::fs::write(dst, &fake_qcow2(None, &content)[..2048]);
            return Err(qemu_backend::BackendError::ResourceLimit(
                "not enough free space on the catalog volume (qemu-img convert)".into(),
            )
            .to_public());
        }
        std::fs::write(dst, fake_qcow2(None, &content))
            .map_err(|_| PublicError::infrastructure("fake convert"))
    }
    fn compare(&self, a: &Path, b: &Path, _cancel: &dyn Fn() -> bool) -> Result<bool, PublicError> {
        push(&self.calls, "compare");
        Ok(fake_content(a)? == fake_content(b)?)
    }
    fn check(&self, p: &Path, cancel: &dyn Fn() -> bool) -> Result<(), PublicError> {
        push(&self.calls, "check");
        match &self.vmdk_tool {
            Some(t) if is_real_qcow2(p) => t.check(p, cancel),
            _ => Ok(()),
        }
    }
    fn virtual_size(&self, p: &Path) -> Result<u64, PublicError> {
        match &self.vmdk_tool {
            Some(t) if is_real_qcow2(p) => t.virtual_size(p),
            _ => Ok(32 << 30),
        }
    }
    fn vmdk_chain_info(&self, top: &Path, cancel: &dyn Fn() -> bool) -> Result<String, PublicError> {
        push(&self.calls, "vmdk_info");
        match &self.vmdk_tool {
            Some(t) => t.vmdk_chain_info(top, cancel),
            None => Err(PublicError::unsupported("no qemu-img in this test")),
        }
    }
    fn convert_vmdk(
        &self,
        src: &Path,
        dst: &Path,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(u8),
    ) -> Result<(), PublicError> {
        push(&self.calls, "convert_vmdk");
        let t0 = std::time::Instant::now();
        while t0.elapsed() < self.vmdk_delay {
            if cancel() {
                return Err(PublicError::state_conflict("operation cancelled"));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match &self.vmdk_tool {
            Some(t) => t.convert_vmdk(src, dst, cancel, progress),
            None => Err(PublicError::unsupported("no qemu-img in this test")),
        }
    }
    fn free_bytes(&self, dir: &Path) -> Option<u64> {
        self.free_bytes
            .or_else(|| self.vmdk_tool.as_ref().and_then(|t| t.free_bytes(dir)))
    }
    fn create_overlay(&self, backing_ref: &str, _size: u64, dst: &Path) -> Result<(), PublicError> {
        push(&self.calls, "create_overlay");
        std::fs::write(dst, fake_qcow2(Some(backing_ref), b""))
            .map_err(|_| PublicError::infrastructure("fake overlay"))
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
            detected_guest: None,
            location: Some(source.to_string_lossy().into_owned()),
            missing: false,
        },
        // ADR-0029: the file is used where it is.
        location: MediaLocation::Referenced(source.to_path_buf()),
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

/// Fake media inspector: reports the registered facts (no file). For the
/// start checks ([`MediaGuardPort`]) the "hash" can take time (`delay`),
/// wait for a gate, and report a tampered copy; every step is logged in
/// `calls` (`media_open`, `media_hash_done`, `media_hash_cancelled`,
/// `media_guard_dropped`).
#[derive(Debug, Default, Clone)]
pub struct FakeInspector {
    /// Duration of the hash of each medium (cancellation polled every 5 ms).
    pub delay: std::time::Duration,
    /// Gate the hash waits for before ending (cancellation polled).
    pub gate: Option<Arc<Gate>>,
    /// Raised: the copy is observed with a hash different from the registry.
    pub tampered: Arc<std::sync::atomic::AtomicBool>,
    /// Calls received.
    pub calls: Calls,
}

impl FakeInspector {
    fn facts(&self, entry: &MediaEntry) -> ObservedMediaFacts {
        let mut sha256 = entry.record.sha256;
        if self.tampered.load(std::sync::atomic::Ordering::SeqCst) {
            sha256 = Sha256Digest::from_bytes([0xee; 32]);
        }
        ObservedMediaFacts {
            sha256,
            size_bytes: entry.record.size_bytes,
            file_identity: entry.file_identity,
            read_only: true,
        }
    }
}

impl MediaInspector for FakeInspector {
    fn inspect(&mut self, entry: &MediaEntry) -> Result<ObservedMediaFacts, PublicError> {
        Ok(self.facts(entry))
    }
}

/// Medium "held open" by the fake inspector for a start.
#[derive(Debug)]
struct FakeGuardedMedia {
    entry: MediaEntry,
    cfg: FakeInspector,
}

impl Drop for FakeGuardedMedia {
    fn drop(&mut self) {
        push(&self.cfg.calls, "media_guard_dropped");
    }
}

impl GuardedMedia for FakeGuardedMedia {
    fn size_bytes(&self) -> u64 {
        self.entry.record.size_bytes
    }

    fn observe(
        &mut self,
        cancel: &dyn Fn() -> bool,
        progress: &dyn Fn(u64),
    ) -> Result<ObservedMediaFacts, PublicError> {
        let t0 = std::time::Instant::now();
        loop {
            if cancel() {
                push(&self.cfg.calls, "media_hash_cancelled");
                return Err(PublicError::state_conflict("verification cancelled"));
            }
            let gate_open = self.cfg.gate.as_ref().is_none_or(|g| g.is_open());
            if gate_open && t0.elapsed() >= self.cfg.delay {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        progress(self.entry.record.size_bytes);
        push(&self.cfg.calls, "media_hash_done");
        Ok(self.cfg.facts(&self.entry))
    }
}

impl MediaGuardPort for FakeInspector {
    fn open_for_start(&mut self, entry: &MediaEntry) -> Result<Box<dyn GuardedMedia>, PublicError> {
        push(&self.calls, "media_open");
        Ok(Box::new(FakeGuardedMedia {
            entry: entry.clone(),
            cfg: self.clone(),
        }))
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
    /// Image tool for baselines (F3); `None` = baselines unsupported.
    pub image_tool: Option<Arc<FakeImageTool>>,
    /// Ends of runs to report (`true` = clean guest shutdown): a graceful
    /// stop is clean, a forced stop is not.
    pub exits: Vec<(VmId, RunId, bool)>,
    /// Current-state files received for the next start, with their guard,
    /// kept like the real backend until the next stop.
    pub start_disks: BTreeMap<VmId, (RunId, StartDisks)>,
    /// Media handles received for the next start, kept like the real
    /// backend until the next stop.
    pub start_media: BTreeMap<VmId, (RunId, Vec<Box<dyn GuardedMedia>>)>,
    /// Bytes of every screenshot (a stand-in for the PNG; F4 chunked read).
    pub screenshot_png: Vec<u8>,
    /// Last screenshot per VM, taken once by the engine.
    pub last_shots: BTreeMap<VmId, (Screenshot, Vec<u8>)>,
    /// Delay of every input forwarding (stop-latency measurements: a slow
    /// backend keeps the supervisor busy).
    pub input_delay: Option<std::time::Duration>,
    /// Guests stopped on a vCPU error, injected by the test; cleared by a
    /// reset, as the QEMU backend does.
    pub vcpu_halts: Arc<Mutex<BTreeMap<VmId, lightvm_proto::ipc::VcpuHalt>>>,
    /// Automatic resets to report, injected by the test.
    pub auto_resets: Arc<Mutex<Vec<crate::engine::AutoReset>>>,
}

/// Outcomes of background graceful stops injected by a test.
pub type StopResults = Arc<Mutex<Vec<(VmId, Result<(), PublicError>)>>>;

impl VmBackend for FakeVmBackend {
    fn start(
        &mut self,
        vm: &VmRecord,
        run_id: RunId,
        media: &[VerifiedMedia],
    ) -> Result<(), PublicError> {
        let disks = self
            .start_disks
            .get(&vm.id)
            .filter(|(r, _)| *r == run_id)
            .map(|(_, d)| d);
        // A current state without UEFI variables (legacy BIOS VM, ADR-0027).
        let no_vars = disks.is_some_and(|d| d.vars.is_none());
        push(
            &self.calls,
            format!(
                "vm_start:{}:media={}{}{}",
                vm.config.name,
                media.len(),
                if no_vars { ":no_vars" } else { "" },
                if disks.is_some() { ":overlay" } else { "" }
            ),
        );
        Ok(())
    }
    fn shutdown(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_shutdown");
        self.start_disks.remove(&vm_id);
        self.start_media.remove(&vm_id);
        self.exits.push((vm_id, run_id, true));
        Ok(())
    }
    fn force_stop(&mut self, vm_id: VmId, run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_force_stop");
        self.start_disks.remove(&vm_id);
        self.start_media.remove(&vm_id);
        self.exits.push((vm_id, run_id, false));
        self.stopping.remove(&vm_id);
        lock(&self.emergency_killed).remove(&vm_id);
        Ok(())
    }
    fn reset(&mut self, vm_id: VmId, _run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_reset");
        lock(&self.vcpu_halts).remove(&vm_id);
        Ok(())
    }
    fn screenshot(
        &mut self,
        vm_id: VmId,
        run_id: RunId,
        max_bytes: u64,
    ) -> Result<Screenshot, PublicError> {
        push(&self.calls, "vm_screenshot");
        let png = self.screenshot_png.clone();
        if png.len() as u64 > max_bytes {
            return Err(PublicError::resource_limit(
                "screenshot over the byte limit",
            ));
        }
        let shot = Screenshot {
            vm_id,
            run_id,
            geometry: FAKE_GEOMETRY,
            format: ImageFormat::Png,
            sha256: {
                use sha2::Digest as _;
                Sha256Digest::from_bytes(sha2::Sha256::digest(&png).into())
            },
            byte_len: png.len() as u64,
            captured_at: UtcTimestamp::now(),
        };
        self.last_shots.insert(vm_id, (shot.clone(), png));
        Ok(shot)
    }
    fn send_input(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        geometry: DisplayGeometry,
        events: &[InputEvent],
    ) -> Result<(), PublicError> {
        if let Some(d) = self.input_delay {
            std::thread::sleep(d);
        }
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
    /// The fake runs every guest, whatever the host (service tests create
    /// x86 VMs on Apple Silicon too).
    fn guest_support(&self) -> vm_core::resources::GuestSupport {
        vm_core::resources::any_guest
    }
    fn image_tool(&self) -> Option<Arc<dyn ImageTool>> {
        self.image_tool.clone().map(|t| t as Arc<dyn ImageTool>)
    }
    fn set_start_disks(&mut self, vm_id: VmId, run_id: RunId, disks: StartDisks) {
        push(&self.calls, "vm_start_disks");
        self.start_disks.insert(vm_id, (run_id, disks));
    }
    fn set_start_media(&mut self, vm_id: VmId, run_id: RunId, guards: Vec<Box<dyn GuardedMedia>>) {
        push(&self.calls, format!("vm_start_media:{}", guards.len()));
        self.start_media.insert(vm_id, (run_id, guards));
    }
    fn take_exits(&mut self) -> Vec<(VmId, RunId, bool)> {
        std::mem::take(&mut self.exits)
    }
    fn vcpu_halt(&self, vm_id: VmId, _run_id: RunId) -> Option<lightvm_proto::ipc::VcpuHalt> {
        lock(&self.vcpu_halts).get(&vm_id).copied()
    }
    fn take_auto_resets(&mut self) -> Vec<crate::engine::AutoReset> {
        std::mem::take(&mut *lock(&self.auto_resets))
    }
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
    fn qemu_log(&mut self, _vm_id: VmId, run_id: RunId) -> Option<String> {
        // A host path, as a real QEMU message would carry: the service must
        // remove it before an agent sees it (F5 `vm.logs`).
        Some(format!(
            "C:\\lightvm\\qemu\\qemu-system-x86_64.exe: fake run {run_id} started\n\
             qemu-system-x86_64.exe: warning: C:\\Users\\someone\\catalog\\vms\\disk.qcow2 opened\n"
        ))
    }
    fn take_screenshot(&mut self, vm_id: VmId, run_id: RunId) -> Option<(Screenshot, Vec<u8>)> {
        let (meta, png) = self.last_shots.remove(&vm_id)?;
        (meta.run_id == run_id).then_some((meta, png))
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
        FakeInspector::default(),
        core_config(host_capacity()),
    )
    .expect("in-memory core");
    e.create_vm(&Caller::User, sample_config("sample"))
        .expect("sample VM");
    let _ = e.state_changes();
    e
}
