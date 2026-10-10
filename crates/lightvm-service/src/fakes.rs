//! Implementazioni finte per i test headless: nessun processo QEMU, nessun
//! file VM. Il core (`vm_core::Core`) è reale; sono finti il backend delle VM,
//! l'host (Emergency Stop, sessioni agente) e, a scelta, catalogo e audit in
//! memoria. Non fanno parte del prodotto: il binario usa sempre gli adattatori
//! reali ([`crate::adapters`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use lightvm_proto::ipc::StopReason;
use lightvm_proto::{
    AuditEvent, BackendObservation, Caller, DisplayGeometry, GuestOs, HostCapacity, InputEvent,
    MediaEntry, MediaId, NetworkProfile, OperationId, OperationRecord, PublicError, RunEndReason,
    RunId, RunRecord, Screenshot, UtcTimestamp, VmConfig, VmId, VmName, VmRecord, VmState,
};
use vm_core::media::{ObservedMediaFacts, VerifiedMedia};
use vm_core::ports::{AuditSink, CatalogStore, MediaInspector, VmBackend};

use crate::engine::{BackendPort, CoreEngine, VmEngine, core_config};
use crate::supervisor::{Backend, EmergencyOutcome, EmergencyStop, ParentLossPolicy};

/// Registro delle chiamate ricevute dalle implementazioni finte.
pub type Calls = Arc<Mutex<Vec<String>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn push(calls: &Calls, s: impl Into<String>) {
    lock(calls).push(s.into());
}

/// Cancello che blocca il backend finto finché non viene aperto (test di coda).
#[derive(Debug, Default)]
pub struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    /// Cancello chiuso.
    pub fn closed() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Apre il cancello.
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

/// Capacità host finte: acceleratore disponibile, nessuna cattura né input.
pub fn host_capacity() -> HostCapacity {
    HostCapacity {
        logical_cpus: 8,
        total_memory_mib: 16 * 1024,
        reserved_memory_mib: 2 * 1024,
        free_storage_gib: 512,
        reserved_storage_gib: 16,
        accelerator_available: true,
        screenshot_available: false,
        input_available: false,
        baseline_available: false,
    }
}

/// Configurazione di prova valida per i limiti del MVP.
pub fn sample_config(name: &str) -> VmConfig {
    VmConfig {
        name: VmName::parse(name).expect("nome di prova valido"),
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

/// Catalogo in memoria.
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
            Some(_) => Err(PublicError::state_conflict("revisione diversa")),
            None => Err(PublicError::not_found("VM inesistente")),
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
}

/// Audit in memoria, condiviso con il test.
#[derive(Debug, Clone, Default)]
pub struct MemoryAudit(pub Arc<Mutex<Vec<AuditEvent>>>);

impl MemoryAudit {
    /// Copia degli eventi registrati.
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

/// Ispettore dei media finto: riporta i fatti registrati (nessun file).
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

/// Backend delle VM finto: registra le chiamate e simula processi sempre
/// pronti. Un cancello opzionale blocca la lettura delle osservazioni (usata a
/// ogni richiesta che tocca il motore) per i test di coda.
#[derive(Debug, Default)]
pub struct FakeVmBackend {
    /// Chiamate ricevute (condivise con [`FakeHost`]).
    pub calls: Calls,
    /// Cancello opzionale.
    pub gate: Option<Arc<Gate>>,
    /// Osservazioni da consegnare al core (uscite simulate).
    pub observations: Arc<Mutex<Vec<(VmId, BackendObservation)>>>,
}

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
    fn force_stop(&mut self, _vm_id: VmId, _run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_force_stop");
        Ok(())
    }
    fn reset(&mut self, _vm_id: VmId, _run_id: RunId) -> Result<(), PublicError> {
        push(&self.calls, "vm_reset");
        Ok(())
    }
    fn screenshot(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _max_bytes: u64,
    ) -> Result<Screenshot, PublicError> {
        Err(PublicError::unsupported(
            "screenshot non disponibile nel backend finto",
        ))
    }
    fn send_input(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _geometry: DisplayGeometry,
        _events: &[InputEvent],
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "input non disponibile nel backend finto",
        ))
    }
}

impl BackendPort for FakeVmBackend {
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)> {
        if let Some(g) = &self.gate {
            g.wait();
        }
        std::mem::take(&mut *lock(&self.observations))
    }
}

/// Host finto (servizio): registra le chiamate.
#[derive(Debug, Default)]
pub struct FakeHost {
    /// Chiamate ricevute.
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

/// Motore con core reale, catalogo e audit in memoria, backend finto.
pub type MemoryEngine = CoreEngine<MemoryStore, MemoryAudit, FakeVmBackend, FakeInspector>;

/// Motore in memoria con una VM di esempio spenta (`esempio`).
pub fn sample_engine(backend: FakeVmBackend, audit: MemoryAudit) -> MemoryEngine {
    let mut e = CoreEngine::new(
        MemoryStore::default(),
        audit,
        backend,
        FakeInspector,
        core_config(host_capacity()),
    )
    .expect("core in memoria");
    e.create_vm(&Caller::User, sample_config("esempio"))
        .expect("VM di esempio");
    let _ = e.state_changes();
    e
}
