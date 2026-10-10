//! Motore delle VM del supervisor: `vm_core::Core` con le porte reali o finte.
//!
//! Il thread del supervisor ([`crate::supervisor`]) possiede un
//! `Box<dyn VmEngine>` e lo serve in ordine. [`CoreEngine`] collega il core
//! (macchina a stati, coda per VM, policy, audit) a un catalogo
//! ([`vm_core::ports::CatalogStore`]), a un backend ([`BackendPort`]) e a un
//! ispettore dei media ([`vm_core::ports::MediaInspector`]). Gli effetti del
//! backend avvengono solo tramite il core: ogni creazione, avvio, arresto,
//! Emergency Stop e diniego produce eventi di audit nella destinazione del core.
//!
//! L'Emergency Stop non passa da qui: il percorso prioritario termina i
//! processi con il gestore del backend ([`crate::supervisor::EmergencyStop`]);
//! dopo, [`VmEngine::after_emergency_stop`] registra l'arresto nel core
//! (revoca delle sessioni, coda scartata, audit, fine processo osservata).

use std::collections::BTreeMap;

use lightvm_proto::ipc::VmSummary;
use lightvm_proto::{
    Actor, Admission, BackendObservation, Caller, OperationRecord, PolicyLayer, PublicError,
    ResourceLimits, VmAction, VmConfig, VmId, VmRecord, VmRequest, VmState,
};
use vm_core::ports::{AuditSink, CatalogStore, IdGenerator, MediaInspector, VmBackend};
use vm_core::{Core, CoreConfig, VmStatus};

use crate::adapters::{SystemClock, SystemIds};

/// Esito di `submit_vm`: accettazione ed eventuale registro dopo l'esecuzione.
pub type SubmitOutcome = (Admission, Option<OperationRecord>);

/// Operazioni sulle VM servite dal thread del supervisor.
pub trait VmEngine: Send + 'static {
    /// Elenco delle VM visibili al chiamante.
    fn list_vms(&mut self, caller: &Caller) -> Result<Vec<VmSummary>, PublicError>;
    /// Stato di una VM.
    fn vm_status(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmSummary, PublicError>;
    /// Creazione di una VM.
    fn create_vm(&mut self, caller: &Caller, config: VmConfig) -> Result<VmRecord, PublicError>;
    /// Modifica della configurazione di una VM spenta.
    fn update_vm(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError>;
    /// Azione su una VM: accettazione nella coda del core ed esecuzione.
    fn submit(&mut self, caller: &Caller, req: VmRequest) -> Result<SubmitOutcome, PublicError>;
    /// Registra nel core un Emergency Stop già eseguito dal percorso prioritario.
    fn after_emergency_stop(&mut self);
    /// Arresto delle VM in esecuzione (arresto controllato del servizio).
    fn stop_all(&mut self);
    /// Cambi di stato dall'ultima chiamata, per gli eventi `vm_state`.
    fn state_changes(&mut self) -> Vec<(VmId, VmState)>;
}

/// Backend delle VM visto dal motore: porta `VmBackend` del core più le
/// osservazioni asincrone (uscita del processo non richiesta dal core).
pub trait BackendPort: VmBackend + Send + 'static {
    /// Osservazioni accumulate da applicare al core con `Core::observe`.
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)>;
}

/// Configurazione del core per il servizio.
pub fn core_config(host: lightvm_proto::HostCapacity) -> CoreConfig {
    CoreConfig {
        global_limits: ResourceLimits::MVP_DEFAULT,
        global_policy: PolicyLayer::permissive(),
        host,
    }
}

fn summary(s: VmStatus) -> VmSummary {
    VmSummary {
        vm_id: s.vm_id,
        name: s.name,
        state: s.state,
        run_id: s.run_id,
        config: s.config,
        config_revision: s.config_revision,
    }
}

/// Motore su `vm_core::Core` con orologio e identificativi di sistema.
pub struct CoreEngine<S, A, B, M>
where
    S: CatalogStore,
    A: AuditSink,
{
    core: Core<S, SystemClock, SystemIds, A>,
    backend: B,
    inspector: M,
    ids: SystemIds,
    last: BTreeMap<VmId, VmState>,
}

impl<S, A, B, M> std::fmt::Debug for CoreEngine<S, A, B, M>
where
    S: CatalogStore,
    A: AuditSink,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreEngine")
            .field("vms", &self.last.len())
            .finish_non_exhaustive()
    }
}

impl<S, A, B, M> CoreEngine<S, A, B, M>
where
    S: CatalogStore + Send + 'static,
    A: AuditSink + Send + 'static,
    B: BackendPort,
    M: MediaInspector + Send + 'static,
{
    /// Costruisce il core e carica il catalogo (le VM con processo nello stato
    /// persistito entrano in `recovering`).
    pub fn new(
        store: S,
        audit: A,
        backend: B,
        inspector: M,
        config: CoreConfig,
    ) -> Result<Self, PublicError> {
        let mut core = Core::new(store, SystemClock, SystemIds::default(), audit, config);
        core.load_catalog()?;
        // Un supervisor appena avviato non possiede processi: quelli di un
        // supervisor precedente sono terminati con il suo Job Object (ADR-0009).
        let riconciliate = core.reconcile_after_restart()?;
        if !riconciliate.is_empty() {
            crate::diag!(
                "lightvm-service: {} VM riportate a stopped dopo il riavvio (nessun processo né intento aperto)",
                riconciliate.len()
            );
        }
        let mut e = Self {
            core,
            backend,
            inspector,
            ids: SystemIds::default(),
            last: BTreeMap::new(),
        };
        let _ = e.state_changes();
        Ok(e)
    }

    /// Core (sola lettura, per test e diagnostica).
    pub fn core(&self) -> &Core<S, SystemClock, SystemIds, A> {
        &self.core
    }

    fn apply_observations(&mut self) {
        for (vm_id, obs) in self.backend.drain_observations() {
            let _ = self.core.observe(vm_id, obs);
        }
    }

    fn run_queue(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        while self
            .core
            .execute_next(vm_id, &mut self.backend, &mut self.inspector)?
            .is_some()
        {}
        Ok(())
    }
}

impl<S, A, B, M> VmEngine for CoreEngine<S, A, B, M>
where
    S: CatalogStore + Send + 'static,
    A: AuditSink + Send + 'static,
    B: BackendPort,
    M: MediaInspector + Send + 'static,
{
    fn list_vms(&mut self, caller: &Caller) -> Result<Vec<VmSummary>, PublicError> {
        self.apply_observations();
        Ok(self
            .core
            .list_vms(caller)?
            .into_iter()
            .map(summary)
            .collect())
    }

    fn vm_status(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmSummary, PublicError> {
        self.apply_observations();
        self.core.vm_status(caller, vm_id).map(summary)
    }

    fn create_vm(&mut self, caller: &Caller, config: VmConfig) -> Result<VmRecord, PublicError> {
        self.core.create_vm(caller, config)
    }

    fn update_vm(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        self.apply_observations();
        self.core
            .update_vm_config(caller, vm_id, expected_revision, config)
    }

    fn submit(&mut self, caller: &Caller, req: VmRequest) -> Result<SubmitOutcome, PublicError> {
        self.apply_observations();
        let vm_id = req.vm_id;
        let op = req.operation_id;
        let admission = self.core.submit(caller, req)?;
        // L'esecuzione è sincrona sul thread del supervisor: un solo
        // proprietario del core e del backend (ARCHITECTURE 5).
        self.run_queue(vm_id)?;
        let record = self.core.operation(caller, op).ok();
        Ok((admission, record))
    }

    fn after_emergency_stop(&mut self) {
        // Prima la registrazione dell'Emergency Stop, poi le altre osservazioni:
        // un'uscita già osservata non deve trasformarsi in un arresto ordinario.
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return;
        };
        for vm in vms.into_iter().filter(|v| v.state.has_process()) {
            // Il processo è già terminato dal percorso prioritario: `force_stop`
            // conferma la fine osservata e il core registra l'esito.
            let _ = self
                .core
                .emergency_stop_with(Actor::User, vm.vm_id, &mut self.backend);
        }
        self.apply_observations();
    }

    fn stop_all(&mut self) {
        self.apply_observations();
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return;
        };
        for vm in vms {
            let action = match vm.state {
                VmState::Running => VmAction::Stop,
                VmState::Starting | VmState::Stopping => VmAction::ForceStop,
                _ => continue,
            };
            let req = VmRequest {
                operation_id: self.ids.operation_id(),
                vm_id: vm.vm_id,
                expected_run_id: None,
                action,
            };
            if self.core.submit(&Caller::User, req).is_ok() {
                let _ = self.run_queue(vm.vm_id);
            }
        }
    }

    fn state_changes(&mut self) -> Vec<(VmId, VmState)> {
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for vm in vms {
            if self.last.insert(vm.vm_id, vm.state) != Some(vm.state) {
                out.push((vm.vm_id, vm.state));
            }
        }
        out
    }
}
