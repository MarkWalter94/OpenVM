//! Supervisor VM engine: `vm_core::Core` with the real or fake ports.
//!
//! The supervisor thread ([`crate::supervisor`]) owns a `Box<dyn VmEngine>`
//! and serves it in order. [`CoreEngine`] connects the core (state machine,
//! per-VM queue, policy, audit) to a catalog ([`vm_core::ports::CatalogStore`]),
//! a backend ([`BackendPort`]) and a media inspector
//! ([`vm_core::ports::MediaInspector`]). Backend effects happen only through
//! the core: every creation, start, stop, Emergency Stop and denial produces
//! audit events in the core's sink.
//!
//! The Emergency Stop does not go through here: the priority path terminates
//! the processes with the backend handler ([`crate::supervisor::EmergencyStop`]);
//! afterwards, [`VmEngine::after_emergency_stop`] records the stop in the core
//! (session revocation, queue discarded, audit, observed process end).

use std::collections::BTreeMap;

use lightvm_proto::ipc::VmSummary;
use lightvm_proto::{
    Actor, Admission, BackendObservation, Caller, OperationRecord, PolicyLayer, PublicError,
    ResourceLimits, VmAction, VmConfig, VmId, VmRecord, VmRequest, VmState,
};
use vm_core::ports::{AuditSink, CatalogStore, IdGenerator, MediaInspector, VmBackend};
use vm_core::{Core, CoreConfig, VmStatus};

use crate::adapters::{SystemClock, SystemIds};

/// Outcome of `submit_vm`: admission and optional record after execution.
pub type SubmitOutcome = (Admission, Option<OperationRecord>);

/// VM operations served by the supervisor thread.
pub trait VmEngine: Send + 'static {
    /// List of the VMs visible to the caller.
    fn list_vms(&mut self, caller: &Caller) -> Result<Vec<VmSummary>, PublicError>;
    /// Status of a VM.
    fn vm_status(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmSummary, PublicError>;
    /// Creation of a VM.
    fn create_vm(&mut self, caller: &Caller, config: VmConfig) -> Result<VmRecord, PublicError>;
    /// Change of the configuration of a powered-off VM.
    fn update_vm(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError>;
    /// Action on a VM: admission into the core queue and execution.
    fn submit(&mut self, caller: &Caller, req: VmRequest) -> Result<SubmitOutcome, PublicError>;
    /// Records in the core an Emergency Stop already executed by the priority
    /// path; `actor` is who decided it (the user, or the supervisor when the UI
    /// is lost).
    fn after_emergency_stop(&mut self, actor: Actor);
    /// Applies the backend observations to the core (unrequested process exits),
    /// so that `vm_state` events arrive even without requests.
    fn observe(&mut self);
    /// VMs with a process (starting, running, stopping), according to the last known state.
    fn vms_with_process(&self) -> usize;
    /// Stops the running VMs (controlled service shutdown).
    fn stop_all(&mut self);
    /// State changes since the last call, for the `vm_state` events.
    fn state_changes(&mut self) -> Vec<(VmId, VmState)>;
}

/// VM backend as seen by the engine: the core's `VmBackend` port plus the
/// asynchronous observations (process exit not requested by the core).
pub trait BackendPort: VmBackend + Send + 'static {
    /// Accumulated observations to apply to the core with `Core::observe`.
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)>;
}

/// Core configuration for the service.
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

/// Engine over `vm_core::Core` with the system clock and identifiers.
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
    /// Builds the core and loads the catalog (VMs with a process in the persisted
    /// state enter `recovering`).
    pub fn new(
        store: S,
        audit: A,
        backend: B,
        inspector: M,
        config: CoreConfig,
    ) -> Result<Self, PublicError> {
        let mut core = Core::new(store, SystemClock, SystemIds::default(), audit, config);
        core.load_catalog()?;
        // A freshly started supervisor owns no processes: those of a previous
        // supervisor terminated with its Job Object (ADR-0009).
        let reconciled = core.reconcile_after_restart()?;
        if !reconciled.is_empty() {
            crate::diag!(
                "lightvm-service: {} VMs brought back to stopped after the restart (no process or open intent)",
                reconciled.len()
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

    /// Core (read-only, for tests and diagnostics).
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
        // Execution is synchronous on the supervisor thread: a single owner of
        // the core and the backend (ARCHITECTURE 5).
        self.run_queue(vm_id)?;
        let record = self.core.operation(caller, op).ok();
        Ok((admission, record))
    }

    fn after_emergency_stop(&mut self, actor: Actor) {
        // First record the Emergency Stop, then the other observations: an exit
        // already observed must not turn into an ordinary stop.
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return;
        };
        for vm in vms.into_iter().filter(|v| v.state.has_process()) {
            // The process was already terminated by the priority path: `force_stop`
            // confirms the observed end and the core records the outcome.
            let _ = self
                .core
                .emergency_stop_with(actor, vm.vm_id, &mut self.backend);
        }
        self.apply_observations();
    }

    fn observe(&mut self) {
        self.apply_observations();
    }

    fn vms_with_process(&self) -> usize {
        self.last.values().filter(|s| s.has_process()).count()
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
