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
//! F2: media import (copy on a worker thread between [`VmEngine::begin_import`]
//! and [`VmEngine::finish_import`]), VM deletion, console target for the
//! display ticket, display geometry refreshed from the backend for console
//! input ([`BackendPort::refresh_geometry`]).
//!
//! The Emergency Stop does not go through here: the priority path terminates
//! the processes with the backend handler ([`crate::supervisor::EmergencyStop`]);
//! afterwards, [`VmEngine::after_emergency_stop`] records the stop in the core
//! (session revocation, queue discarded, audit, observed process end).

use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lightvm_proto::ipc::VmSummary;
use lightvm_proto::{
    Actor, Admission, BackendObservation, Caller, DisplayGeometry, ErrorCode, HostCapacity,
    InputEvent, Label, MediaEntry, MediaKind, MediaRecord, OperationId, OperationRecord,
    PolicyLayer, PublicError, ResourceLimits, RunId, TerminationCause, VmAction, VmConfig, VmId,
    VmRecord, VmRequest, VmState,
};
use vm_core::ports::{AuditSink, CatalogStore, IdGenerator, MediaInspector, VmBackend};
use vm_core::{Core, CoreConfig, DispatchOutcome, VmStatus};

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
    /// State changes since the last call, for the `vm_state` events: a VM
    /// is reported when its state **or its run** changed (a run replaced
    /// within one command, `running → stopped → running`, is a change).
    fn state_changes(&mut self) -> Vec<(VmId, VmState)>;
    /// Current run of every VM with a process, `None` for the others,
    /// according to the last [`Self::state_changes`]: the supervisor revokes
    /// the console tickets of any other run.
    fn current_runs(&self) -> Vec<(VmId, Option<RunId>)>;
    /// Approved media of the registry.
    fn list_media(&mut self, caller: &Caller) -> Result<Vec<MediaRecord>, PublicError>;
    /// First step of a media import (checks of the path chosen by the user,
    /// source opened, journal intent): returns the copy to run outside the
    /// supervisor thread.
    fn begin_import(
        &mut self,
        caller: &Caller,
        source: &str,
        kind: MediaKind,
        declared_origin: &str,
    ) -> Result<ImportJob, PublicError>;
    /// Last step of a media import: publication and registry row.
    fn finish_import(
        &mut self,
        caller: &Caller,
        copied: ImportCopied,
    ) -> Result<MediaRecord, PublicError>;
    /// Cancels a media import whose copy ended (connection revoked or
    /// closed, Emergency Stop): the copy is discarded (intent aborted,
    /// temporary removed), the import is audited as failed; returns `reason`.
    fn abort_import(
        &mut self,
        caller: &Caller,
        copied: ImportCopied,
        reason: PublicError,
    ) -> PublicError;
    /// Deletion of a stopped VM with its files.
    fn delete_vm(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
    ) -> Result<(), PublicError>;
    /// Console of a running VM: run, QEMU display socket, geometry. Only a
    /// refusal is audited here; the outcome of issuing the ticket is
    /// recorded with [`Self::console_opened`].
    fn open_console(&mut self, caller: &Caller, vm_id: VmId) -> Result<ConsoleTarget, PublicError>;
    /// Audit of the outcome of issuing a console ticket (`console_open`).
    fn console_opened(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        run_id: RunId,
        result: Result<(), ErrorCode>,
    );
    /// Audit of console tickets revoked (`console_revoke`, with the cause).
    fn console_revoked(&mut self, actor: Actor, runs: &[(VmId, RunId)], cause: TerminationCause);
    /// Releases the keys and buttons still pressed on a run (console lost).
    fn release_input(&mut self, vm_id: VmId, run_id: RunId);
    /// Host capacity used by the core.
    fn host_capacity(&self) -> Option<HostCapacity>;
}

/// Copy of a media import, to run on a worker thread; its result goes back
/// to [`VmEngine::finish_import`] on the supervisor thread.
pub struct ImportJob(Box<dyn FnOnce() -> ImportCopied + Send>);

impl ImportJob {
    /// Job from a closure.
    pub fn new(f: impl FnOnce() -> ImportCopied + Send + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Runs the copy (blocking).
    pub fn run(self) -> ImportCopied {
        (self.0)()
    }
}

impl std::fmt::Debug for ImportJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImportJob")
    }
}

/// Opaque result of an [`ImportJob`], specific to the catalog that made it.
pub struct ImportCopied(pub Box<dyn Any + Send>);

impl std::fmt::Debug for ImportCopied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImportCopied")
    }
}

/// Media import in two steps on a catalog (the copy runs between them,
/// outside the catalog).
pub trait MediaImportPort {
    /// Opens the source, checks it and records the intent.
    fn begin_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &Label,
    ) -> Result<ImportJob, PublicError>;
    /// Publishes the copy made by the job.
    fn finish_import(&mut self, copied: ImportCopied) -> Result<MediaEntry, PublicError>;
    /// Discards the copy made by the job without publishing it.
    fn abort_import(&mut self, copied: ImportCopied) -> Result<(), PublicError>;
}

/// Data for a console ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleTarget {
    /// Current run.
    pub run_id: RunId,
    /// QEMU display socket of the run (never sent to clients).
    pub display_socket: Option<PathBuf>,
    /// Current display geometry, if known.
    pub geometry: Option<DisplayGeometry>,
}

/// VM backend as seen by the engine: the core's `VmBackend` port plus the
/// asynchronous observations (process exit not requested by the core).
pub trait BackendPort: VmBackend + Send + 'static {
    /// Accumulated observations to apply to the core with `Core::observe`.
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)>;
    /// Display socket of the run (QEMU `vnc.sock`), if the backend has one.
    fn display_socket(&self, _vm_id: VmId, _run_id: RunId) -> Option<PathBuf> {
        None
    }
    /// Current display geometry read from the backend (cheap capture), if
    /// available.
    fn refresh_geometry(&mut self, _vm_id: VmId, _run_id: RunId) -> Option<DisplayGeometry> {
        None
    }
    /// Releases the keys and buttons still pressed on the run.
    fn release_input(&mut self, _vm_id: VmId, _run_id: RunId) {}
    /// Starts a graceful stop without waiting for it: ACPI powerdown, then
    /// the escalation (wait, `quit`, termination) runs in the background and
    /// its outcome is reported by [`Self::finished_shutdowns`]. `None` if the
    /// backend only supports the blocking [`VmBackend::shutdown`].
    fn begin_shutdown(&mut self, _vm_id: VmId, _run_id: RunId) -> Option<Result<(), PublicError>> {
        None
    }
    /// Graceful stops started with [`Self::begin_shutdown`] that ended
    /// (`Ok` = process exit observed and run released). A run whose stop is in
    /// progress is not reported by [`Self::drain_observations`].
    fn finished_shutdowns(&mut self) -> Vec<(VmId, RunId, Result<(), PublicError>)> {
        Vec::new()
    }
}

/// Safety bound of [`VmEngine::stop_all`] beyond the escalation of each stop.
const STOP_ALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Minimum interval between two display geometry refreshes (a capture) for
/// the same run, whatever the input traffic.
pub const GEOMETRY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Core configuration for the service.
pub fn core_config(host: HostCapacity) -> CoreConfig {
    CoreConfig {
        global_limits: ResourceLimits::MVP_DEFAULT,
        global_policy: PolicyLayer::permissive(),
        host,
    }
}

fn summary(s: VmStatus, disk_used_bytes: Option<u64>) -> VmSummary {
    VmSummary {
        vm_id: s.vm_id,
        name: s.name,
        state: s.state,
        run_id: s.run_id,
        config: s.config,
        config_revision: s.config_revision,
        disk_used_bytes,
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
    /// State and run of every VM at the last [`VmEngine::state_changes`].
    last: BTreeMap<VmId, (VmState, Option<RunId>)>,
    /// Last display geometry refresh per VM (run, instant): debounce.
    last_refresh: BTreeMap<VmId, (RunId, std::time::Instant)>,
    /// Graceful stops forwarded and still running in the backend: VM →
    /// (operation, run). The operation stays forwarded in the core (VM in
    /// `stopping`) until the backend reports the outcome.
    pending_stops: BTreeMap<VmId, (OperationId, RunId)>,
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
    S: CatalogStore + MediaImportPort + Send + 'static,
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
            last_refresh: BTreeMap::new(),
            pending_stops: BTreeMap::new(),
        };
        let _ = e.state_changes();
        Ok(e)
    }

    /// Core (read-only, for tests and diagnostics).
    pub fn core(&self) -> &Core<S, SystemClock, SystemIds, A> {
        &self.core
    }

    fn apply_observations(&mut self) {
        for (vm_id, run_id, result) in self.backend.finished_shutdowns() {
            if self.pending_stops.get(&vm_id).map(|(_, r)| *r) == Some(run_id)
                && let Some((op, _)) = self.pending_stops.remove(&vm_id)
            {
                let _ =
                    self.core
                        .finish_dispatch(vm_id, op, result.map(|()| DispatchOutcome::Done));
            }
        }
        for (vm_id, obs) in self.backend.drain_observations() {
            let _ = self.core.observe(vm_id, obs);
        }
        self.settle_pending_stops();
    }

    /// Graceful stops whose run ended otherwise (forced stop, Emergency Stop,
    /// VM deleted): the operation is closed as superseded.
    fn settle_pending_stops(&mut self) {
        let stale: Vec<(VmId, OperationId)> = self
            .pending_stops
            .iter()
            .filter(|(vm, (_, run))| {
                self.core
                    .vm_status(&Caller::User, **vm)
                    .map_or(true, |st| st.run_id != Some(*run))
            })
            .map(|(vm, (op, _))| (*vm, *op))
            .collect();
        for (vm, op) in stale {
            self.pending_stops.remove(&vm);
            let _ = self.core.finish_dispatch(
                vm,
                op,
                Err(PublicError::state_conflict(
                    "graceful stop superseded by a forced stop",
                )),
            );
        }
    }

    /// Graceful stops in progress.
    pub fn pending_stops(&self) -> usize {
        self.pending_stops.len()
    }

    fn with_usage(&self, s: VmStatus) -> VmSummary {
        let used = self.core.store().vm_disk_usage(s.vm_id);
        summary(s, used)
    }

    /// Input with pointer coordinates on a geometry different from the one
    /// known to the core: the geometry is re-read from the backend once (the
    /// guest may have changed resolution); the core then still rejects a
    /// batch whose geometry differs from the current one (stale coordinates).
    ///
    /// The re-read is a capture (`screendump`): it runs only for a caller
    /// the core authorizes for input on the VM (side-effect-free check, the
    /// admission repeats it with audit) and at most once per
    /// [`GEOMETRY_REFRESH_INTERVAL`] per run, so neither an unauthorized
    /// caller nor a flood of stale batches can drive captures.
    fn refresh_geometry_for(&mut self, caller: &Caller, req: &VmRequest) {
        let VmAction::Input(batch) = &req.action else {
            return;
        };
        if !batch
            .events
            .iter()
            .any(|e| matches!(e, InputEvent::PointerMove(_)))
        {
            return;
        }
        if !self
            .core
            .is_authorized(caller, req.vm_id, req.action.capability())
        {
            return;
        }
        let Ok(st) = self.core.vm_status(&Caller::User, req.vm_id) else {
            return;
        };
        if !vm_core::input_allowed_for(caller, st.state)
            || st.run_id != Some(batch.run_id)
            || st.geometry == Some(batch.geometry)
        {
            return;
        }
        let now = std::time::Instant::now();
        if let Some((run, at)) = self.last_refresh.get(&req.vm_id)
            && *run == batch.run_id
            && now.duration_since(*at) < GEOMETRY_REFRESH_INTERVAL
        {
            return;
        }
        self.last_refresh.insert(req.vm_id, (batch.run_id, now));
        if let Some(g) = self.backend.refresh_geometry(req.vm_id, batch.run_id) {
            let _ = self.core.observe_geometry(req.vm_id, batch.run_id, g);
        }
    }

    /// Forwards the queued operations of the VM. A graceful stop is started
    /// in the backend and left forwarded (the supervisor thread is not held
    /// for the ACPI wait); a forced stop queued meanwhile is forwarded at
    /// once by the core and wins.
    fn run_queue(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        while let Some(d) = self.core.begin_dispatch(vm_id)? {
            if d.action == VmAction::Stop {
                match self.backend.begin_shutdown(vm_id, d.run_id) {
                    Some(Ok(())) => {
                        self.pending_stops.insert(vm_id, (d.operation_id, d.run_id));
                        continue;
                    }
                    Some(Err(e)) => {
                        self.core.finish_dispatch(vm_id, d.operation_id, Err(e))?;
                        continue;
                    }
                    None => {}
                }
            }
            self.core
                .execute_dispatch(d, &mut self.backend, &mut self.inspector)?;
        }
        self.settle_pending_stops();
        Ok(())
    }
}

impl<S, A, B, M> VmEngine for CoreEngine<S, A, B, M>
where
    S: CatalogStore + MediaImportPort + Send + 'static,
    A: AuditSink + Send + 'static,
    B: BackendPort,
    M: MediaInspector + Send + 'static,
{
    fn list_vms(&mut self, caller: &Caller) -> Result<Vec<VmSummary>, PublicError> {
        self.apply_observations();
        let vms = self.core.list_vms(caller)?;
        Ok(vms.into_iter().map(|s| self.with_usage(s)).collect())
    }

    fn vm_status(&mut self, caller: &Caller, vm_id: VmId) -> Result<VmSummary, PublicError> {
        self.apply_observations();
        let s = self.core.vm_status(caller, vm_id)?;
        Ok(self.with_usage(s))
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
        self.refresh_geometry_for(caller, &req);
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
        self.settle_pending_stops();
    }

    fn observe(&mut self) {
        self.apply_observations();
    }

    fn vms_with_process(&self) -> usize {
        self.last.values().filter(|(s, _)| s.has_process()).count()
    }

    fn stop_all(&mut self) {
        self.apply_observations();
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return;
        };
        for vm in vms {
            let action = match vm.state {
                VmState::Running => VmAction::Stop,
                // A graceful stop already in progress is awaited below.
                VmState::Stopping if self.pending_stops.contains_key(&vm.vm_id) => continue,
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
        // The stops run in parallel in the backend, each bounded by its own
        // escalation (ACPI wait → `quit` → termination): wait for them.
        let deadline = std::time::Instant::now() + STOP_ALL_LIMIT;
        while !self.pending_stops.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.apply_observations();
        }
    }

    fn state_changes(&mut self) -> Vec<(VmId, VmState)> {
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for vm in vms {
            seen.insert(vm.vm_id);
            let now = (vm.state, vm.run_id);
            if self.last.insert(vm.vm_id, now) != Some(now) {
                out.push((vm.vm_id, vm.state));
            }
        }
        self.last.retain(|id, _| seen.contains(id));
        out
    }

    fn current_runs(&self) -> Vec<(VmId, Option<RunId>)> {
        self.last
            .iter()
            .map(|(id, (state, run))| (*id, if state.has_process() { *run } else { None }))
            .collect()
    }

    fn list_media(&mut self, caller: &Caller) -> Result<Vec<MediaRecord>, PublicError> {
        self.core.list_media(caller)
    }

    fn begin_import(
        &mut self,
        caller: &Caller,
        source: &str,
        kind: MediaKind,
        declared_origin: &str,
    ) -> Result<ImportJob, PublicError> {
        self.core.begin_media_import(caller, |store| {
            let origin = Label::parse(declared_origin)
                .map_err(|_| PublicError::invalid_argument("invalid declared origin"))?;
            // Text only here: the file is opened and checked by the job, on
            // the import worker thread.
            let path = crate::adapters::catalog::validate_import_path(source)?;
            store.begin_import(&path, kind, &origin)
        })
    }

    fn finish_import(
        &mut self,
        caller: &Caller,
        copied: ImportCopied,
    ) -> Result<MediaRecord, PublicError> {
        self.core
            .finish_media_import(caller, |store| store.finish_import(copied))
    }

    fn abort_import(
        &mut self,
        caller: &Caller,
        copied: ImportCopied,
        reason: PublicError,
    ) -> PublicError {
        let r = self.core.finish_media_import(caller, |store| {
            if let Err(e) = store.abort_import(copied) {
                crate::diag!("lightvm-service: cancelled import not rolled back: {e}");
            }
            Err(reason)
        });
        match r {
            Err(e) => e,
            Ok(_) => PublicError::infrastructure("import cancelled"),
        }
    }

    fn delete_vm(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        expected_revision: u64,
    ) -> Result<(), PublicError> {
        self.apply_observations();
        self.core.delete_vm(caller, vm_id, expected_revision)?;
        self.last.remove(&vm_id);
        Ok(())
    }

    fn open_console(&mut self, caller: &Caller, vm_id: VmId) -> Result<ConsoleTarget, PublicError> {
        self.apply_observations();
        let st = self.core.open_console(caller, vm_id)?;
        let run_id = st
            .run_id
            .ok_or_else(|| PublicError::state_conflict("the console requires a running VM"))?;
        let mut geometry = st.geometry;
        if geometry.is_none()
            && let Some(g) = self.backend.refresh_geometry(vm_id, run_id)
            && self
                .core
                .observe_geometry(vm_id, run_id, g)
                .unwrap_or(false)
        {
            geometry = Some(g);
        }
        Ok(ConsoleTarget {
            run_id,
            display_socket: self.backend.display_socket(vm_id, run_id),
            geometry,
        })
    }

    fn console_opened(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        run_id: RunId,
        result: Result<(), ErrorCode>,
    ) {
        self.core.record_console_open(caller, vm_id, run_id, result);
    }

    fn console_revoked(&mut self, actor: Actor, runs: &[(VmId, RunId)], cause: TerminationCause) {
        for (vm_id, run_id) in runs {
            self.core
                .record_console_revoke(actor, *vm_id, *run_id, cause);
        }
    }

    fn release_input(&mut self, vm_id: VmId, run_id: RunId) {
        self.backend.release_input(vm_id, run_id);
    }

    fn host_capacity(&self) -> Option<HostCapacity> {
        Some(self.core.config().host)
    }
}
