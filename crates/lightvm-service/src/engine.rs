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
//! F3: baselines with the VM off. Creation and restore are long storage
//! operations in three steps like the media import
//! ([`VmEngine::begin_baseline_job`] on the supervisor thread: core checks,
//! journal intents and guards on the catalog; [`StorageJob::run`] on a worker:
//! `qemu-img` through the backend's [`ImageTool`], hashes; then
//! [`VmEngine::finish_baseline_job`] or [`VmEngine::abort_baseline_job`]); the
//! VM is busy in between. A start of a VM whose current state is an overlay
//! receives the verified, guarded files from the catalog
//! ([`BaselinePort::start_disks`] â†’ [`BackendPort::set_start_disks`]); a
//! refusal there leaves the VM `stopped` ([`vm_core::Core::refuse_start`]).
//!
//! The Emergency Stop does not go through here: the priority path terminates
//! the processes with the backend handler ([`crate::supervisor::EmergencyStop`]);
//! afterwards, [`VmEngine::after_emergency_stop`] records the stop in the core
//! (session revocation, queue discarded, audit, observed process end).
//!
//! F4: agent sessions. [`VmEngine::create_session`] issues the grant approved
//! in the app; [`VmEngine::agent_call`] serves the calls of a session through
//! the core (two checks, quotas, audit); screenshots of a session keep their
//! PNG here and are read back in chunks; the input owner of each VM lives in
//! the core and every change releases the pressed keys
//! ([`BackendPort::release_input`]); [`VmEngine::tick_sessions`] terminates
//! expired sessions and Emergency-Stops their VMs with OS primitives
//! ([`VmBackend::force_stop`]).

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightvm_proto::ipc::{
    AgentCall, AgentVmView, BaselineSummary, InputOwner, ResponseBody, SCREENSHOT_CHUNK_BYTES,
    SessionInfo, SnapshotSummary, SnapshotTree, StorageOperation, VcpuHalt, VmBaselines,
    VmSnapshots, VmSummary,
};
use lightvm_proto::{
    Actor, Admission, AuditAction, AuditOutcome, BackendObservation, BaselineId, Caller,
    Capability, DisplayGeometry, ErrorCode, Grant, HostCapacity, InputEvent, Label, Lease,
    MediaEntry, MediaId, MediaKind, MediaRecord, OperationId, OperationKind, OperationRecord,
    OperationStatus, PolicyLayer, PublicError, ResourceLimits, RevocationReport, RunId, Screenshot,
    SessionId, SessionState, SessionToken, SnapshotId, TerminationCause, VmAction, VmConfig, VmId,
    VmRecord, VmRequest, VmState,
};
use vm_core::ports::{AuditSink, CatalogStore, IdGenerator, VmBackend};
use vm_core::{Core, CoreConfig, DispatchOutcome, VmStatus};

use crate::adapters::{SystemClock, SystemIds};
pub use crate::start::{
    GuardedMedia, MediaGuardPort, ProgressCell, StartDone, StartGuard, StartJob,
};
use crate::start::{PrecomputedInspector, StartFailure, cancelled_error};

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
    /// within one command, `running â†’ stopped â†’ running`, is a change).
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
    /// Deletion of an imported medium no VM configuration uses (a managed
    /// copy is removed with it, a file used in place is kept).
    fn delete_media(&mut self, _caller: &Caller, _media_id: MediaId) -> Result<(), PublicError> {
        Err(PublicError::unsupported("media deletion not available"))
    }
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
    /// Baselines of a VM (F3).
    fn list_baselines(
        &mut self,
        _caller: &Caller,
        _vm_id: VmId,
    ) -> Result<Vec<BaselineSummary>, PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// First step of a baseline creation or restore (F3): core and catalog
    /// checks, journal intents and guards; returns the work to run outside
    /// the supervisor thread. `cancel` is polled by the work (Emergency
    /// Stop, revocation, shutdown).
    fn begin_baseline_job(
        &mut self,
        _caller: &Caller,
        _req: BaselineRequest,
        _cancel: CancelCheck,
    ) -> Result<StorageJob, PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// Last step: publication on the catalog, audit, VM free again.
    fn finish_baseline_job(&mut self, _done: StorageDone) -> Result<BaselineOutcome, PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// Discards a finished job (cancellation): nothing is published, the
    /// operation is audited as failed with `reason`, which is returned.
    fn abort_baseline_job(&mut self, _done: StorageDone, reason: PublicError) -> PublicError {
        reason
    }
    /// Deletion of a baseline (F3).
    fn delete_baseline(
        &mut self,
        _caller: &Caller,
        _vm_id: VmId,
        _baseline_id: BaselineId,
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// User snapshots of a VM as a tree (UI only, ADR-0032). Take, revert,
    /// delete and promote are storage jobs ([`Self::begin_baseline_job`]).
    fn list_snapshots(
        &mut self,
        _caller: &Caller,
        _vm_id: VmId,
    ) -> Result<SnapshotTree, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// New name and description of a user snapshot (UI only).
    fn update_snapshot(
        &mut self,
        _caller: &Caller,
        _vm_id: VmId,
        _snapshot_id: SnapshotId,
        _name: Label,
        _description: String,
    ) -> Result<SnapshotSummary, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// Thumbnail PNG of a user snapshot, if one was kept (UI only).
    fn snapshot_thumbnail(
        &mut self,
        _caller: &Caller,
        _vm_id: VmId,
        _snapshot_id: SnapshotId,
    ) -> Result<Option<Vec<u8>>, PublicError> {
        Ok(None)
    }
    /// `true`: the file checks of a start (media and baseline hashes) are
    /// left to the caller as [`StartJob`]s ([`Self::take_start_jobs`]) and
    /// completed with [`Self::finish_start`]; `false` (default): they run
    /// inline in [`Self::submit`]. The supervisor thread turns it on.
    fn offload_start_checks(&mut self, _on: bool) {}
    /// Start checks begun since the last call (offload on), to run on a
    /// worker thread.
    fn take_start_jobs(&mut self) -> Vec<StartJob> {
        Vec::new()
    }
    /// Last step of a start whose checks ran on a worker: launch, or record
    /// the failure (`abort` = cancelled by the caller with that reason).
    /// Returns the final record of the start operation.
    fn finish_start(
        &mut self,
        _done: StartDone,
        _abort: Option<PublicError>,
    ) -> Result<OperationRecord, PublicError> {
        Err(PublicError::unsupported("deferred start not available"))
    }

    // ----- agent sessions (F4) ---------------------------------------------

    /// Creates the session approved by the user (core grant and lease) and,
    /// with `give_input`, hands it the input of its VM.
    fn create_session(&mut self, _new: NewSession) -> Result<CreatedSession, PublicError> {
        Err(PublicError::unsupported("agent sessions not available"))
    }
    /// Sessions for the UI: live first, then the most recent terminated.
    fn list_sessions(&mut self) -> Vec<SessionInfo> {
        Vec::new()
    }
    /// Gate changes made outside the supervisor thread (F4): applied to the
    /// core before every admission and forwarding.
    fn attach_gate(&mut self, _gate: Arc<dyn SessionGate>) {}
    /// Agent pause (core): mutations refused, observation allowed.
    fn pause_session(&mut self, _id: SessionId) -> Result<SessionInfo, PublicError> {
        Err(PublicError::unsupported("agent sessions not available"))
    }
    /// Resume after a pause.
    fn resume_session(&mut self, _id: SessionId) -> Result<SessionInfo, PublicError> {
        Err(PublicError::unsupported("agent sessions not available"))
    }
    /// Terminates a session with `cause`; with `stop` its running VM is
    /// Emergency-Stopped (expiry, client loss, UI loss: ROADMAP 7.3).
    fn end_session(
        &mut self,
        _id: SessionId,
        _cause: TerminationCause,
        _stop: bool,
    ) -> Result<RevocationReport, PublicError> {
        Err(PublicError::unsupported("agent sessions not available"))
    }
    /// Terminates every live session.
    fn end_all_sessions(&mut self, _cause: TerminationCause, _stop: bool) {}
    /// Grant and lease expiries: expired sessions end and their VMs are
    /// Emergency-Stopped.
    fn tick_sessions(&mut self) {}
    /// Input owner of a VM (decided by the user).
    fn set_input_owner(&mut self, _vm_id: VmId, _owner: InputOwner) -> Result<(), PublicError> {
        Err(PublicError::unsupported("agent sessions not available"))
    }
    /// Call of an agent session (the caller carries session and token).
    fn agent_call(
        &mut self,
        _caller: &Caller,
        _call: AgentCall,
    ) -> Result<ResponseBody, PublicError> {
        Err(PublicError::unsupported("agent sessions not available"))
    }
    /// Session state changes since the last call (`session_changed` events
    /// and the service gate).
    fn session_changes(&mut self) -> Vec<(SessionId, SessionState, Option<TerminationCause>)> {
        Vec::new()
    }
    /// Input owner changes since the last call (`input_owner` events).
    fn input_owner_changes(&mut self) -> Vec<(VmId, InputOwner)> {
        Vec::new()
    }
    /// Audit of a session lifecycle step decided by the service.
    /// Facts of a VM for a session report (F5): name, configuration and
    /// revision, current baseline, current run.
    fn report_context(&mut self, _vm_id: VmId) -> Option<ReportContext> {
        None
    }

    /// PNG of the last screenshot of an agent session, if it is
    /// `operation_id` (copy for the session report, F5).
    fn session_screenshot(
        &self,
        _session_id: SessionId,
        _operation_id: OperationId,
    ) -> Option<(Screenshot, Vec<u8>)> {
        None
    }

    /// Core checks of an agent call served outside the queue (report,
    /// declaration, tool note, logs; F5): a live session presenting its
    /// token; with `capability`, also authorized on `vm_id`.
    fn agent_check(
        &mut self,
        _caller: &Caller,
        _vm_id: Option<VmId>,
        _capability: Option<Capability>,
    ) -> Result<SessionId, PublicError> {
        Err(PublicError::unsupported("agent reports not available"))
    }

    /// QEMU diagnostics of the VM's current run (raw text, last part).
    fn qemu_log(&mut self, _vm_id: VmId) -> Option<String> {
        None
    }

    // ----- VMware import (ADR-0030) ---------------------------------------

    /// First step of a VMware preview or import (supervisor thread): user
    /// only (a refusal is audited `vm_import` denied); the context for the
    /// examination on a worker.
    fn vmware_context(
        &mut self,
        _caller: &Caller,
    ) -> Result<crate::vmware::VmwareContext, PublicError> {
        Err(PublicError::unsupported("VMware import not available"))
    }
    /// An import refused by the examination of the VMware files (audited
    /// `vm_import` denied).
    fn vmware_denied(&mut self, _caller: &Caller, _error: &PublicError) {}
    /// Core checks of the configuration, storage budget, then the journal
    /// intent of the import; returns where to write the converted disk.
    fn vmware_begin(
        &mut self,
        _caller: &Caller,
        _config: &VmConfig,
        _required_bytes: u64,
        _free_bytes: Option<u64>,
    ) -> Result<VmImportSlot, PublicError> {
        Err(PublicError::unsupported("VMware import not available"))
    }
    /// Publishes the converted disk with the VM row and registers the VM.
    fn vmware_finish(
        &mut self,
        _caller: &Caller,
        _slot: VmImportSlot,
        _config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        Err(PublicError::unsupported("VMware import not available"))
    }
    /// Rolls an import back (cancellation, failure); audited `failed`;
    /// returns `reason`.
    fn vmware_abort(
        &mut self,
        _caller: &Caller,
        _slot: VmImportSlot,
        reason: PublicError,
    ) -> PublicError {
        reason
    }

    fn record_event(
        &mut self,
        _actor: Actor,
        _action: AuditAction,
        _outcome: AuditOutcome,
        _session_id: Option<SessionId>,
        _error: Option<ErrorCode>,
    ) {
    }
}

/// Change of an agent session decided by the service on its priority path
/// (pause, revocation, Emergency Stop, client or UI loss), for the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateChange {
    /// Agent pause.
    Pause,
    /// Termination with the cause; `stop` = Emergency Stop of its VM.
    Terminate {
        /// Cause.
        cause: TerminationCause,
        /// Stop the running VM of the session.
        stop: bool,
    },
}

/// Source of the gate changes made outside the supervisor thread: the
/// engine applies them to the core before every admission and every
/// forwarding ([`VmEngine::attach_gate`]).
pub trait SessionGate: Send + Sync + 'static {
    /// Changes since the last call, in order.
    fn take_changes(&self) -> Vec<(SessionId, GateChange)>;
}

/// Session approved in the app, for [`VmEngine::create_session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// The single VM of the grant.
    pub vm_id: VmId,
    /// Granted capabilities.
    pub capabilities: lightvm_proto::CapabilitySet,
    /// Granted quotas.
    pub quotas: lightvm_proto::Quotas,
    /// Absolute expiry of the grant.
    pub expires_at: lightvm_proto::UtcTimestamp,
    /// Lease duration in ms.
    pub lease_ms: i64,
    /// Hand the input of the VM to the session at once.
    pub give_input: bool,
    /// Session secret generated by the service (never logged).
    pub token: SessionToken,
    /// Agent connection.
    pub connection_id: u64,
    /// Client name declared by the agent.
    pub client_name: String,
    /// Purpose declared by the agent.
    pub purpose: String,
}

/// Facts of a VM for a session report, see [`VmEngine::report_context`].
#[derive(Debug, Clone)]
pub struct ReportContext {
    /// Name.
    pub name: String,
    /// Configuration.
    pub config: VmConfig,
    /// Configuration revision.
    pub config_revision: u64,
    /// Baseline the current state is an overlay of.
    pub baseline: Option<BaselineSummary>,
    /// Current run.
    pub run_id: Option<RunId>,
}

/// Outcome of [`VmEngine::create_session`].
#[derive(Debug, Clone)]
pub struct CreatedSession {
    /// Session for the UI.
    pub info: SessionInfo,
    /// Grant for the agent.
    pub grant: Grant,
    /// First lease.
    pub lease: Lease,
}

/// Cancellation check of a long storage operation, polled by its work.
pub type CancelCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Disk image operations for baselines (in the product `qemu-img`, run by
/// the backend in its own Job Object, cancellable). Paths are absolute
/// catalog paths decided by the storage.
pub trait ImageTool: Send + Sync + std::fmt::Debug + 'static {
    /// Standalone copy of `src` (backing chain flattened) into the new file
    /// `dst`.
    fn convert(&self, src: &Path, dst: &Path, cancel: &dyn Fn() -> bool)
    -> Result<(), PublicError>;
    /// `true` if the guest-visible contents are identical.
    fn compare(&self, a: &Path, b: &Path, cancel: &dyn Fn() -> bool) -> Result<bool, PublicError>;
    /// Consistency check of an image (no errors, no leaks).
    fn check(&self, p: &Path, cancel: &dyn Fn() -> bool) -> Result<(), PublicError>;
    /// Virtual size of an image.
    fn virtual_size(&self, p: &Path) -> Result<u64, PublicError>;
    /// New overlay `dst` of `size` bytes whose backing reference is
    /// `backing_ref` (relative to the overlay's final directory), format
    /// `qcow2`, backing not opened.
    fn create_overlay(&self, backing_ref: &str, size: u64, dst: &Path) -> Result<(), PublicError>;
    /// Template of the UEFI variables of the approved firmware: a baseline
    /// of a VM that never started gets its variables from it (a baseline
    /// always carries its firmware state, ROADMAP 8).
    fn firmware_vars_template(&self) -> Option<PathBuf> {
        None
    }
    /// `qemu-img info --backing-chain --output=json -f vmdk` of a VMware
    /// disk whose chain was validated (ADR-0030).
    fn vmdk_chain_info(
        &self,
        _top: &Path,
        _cancel: &dyn Fn() -> bool,
    ) -> Result<String, PublicError> {
        Err(PublicError::unsupported(
            "VMware import not available on this backend",
        ))
    }
    /// Conversion of a VMware disk (chain flattened) into the new QCOW2
    /// file `dst`, with the percentage of progress (ADR-0030).
    fn convert_vmdk(
        &self,
        _src: &Path,
        _dst: &Path,
        _cancel: &dyn Fn() -> bool,
        _progress: &mut dyn FnMut(u8),
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "VMware import not available on this backend",
        ))
    }
    /// Free bytes of the volume holding `dir`, if measurable.
    fn free_bytes(&self, _dir: &Path) -> Option<u64> {
        None
    }
    /// `qemu-img snapshot -c/-a/-d <tag>` on the VM's own disk with the VM
    /// off (ADR-0032).
    fn internal_snapshot(
        &self,
        _disk: &Path,
        _action: DiskSnapshotAction,
        _tag: &str,
        _cancel: &dyn Fn() -> bool,
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "snapshots not available on this backend",
        ))
    }
    /// Standalone copy of the internal snapshot `tag` of `src` into the new
    /// file `dst` (promotion of a snapshot to a baseline, ADR-0032).
    fn convert_snapshot(
        &self,
        _src: &Path,
        _tag: &str,
        _dst: &Path,
        _cancel: &dyn Fn() -> bool,
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "snapshots not available on this backend",
        ))
    }
}

/// Internal snapshot operation on a disk with the VM off (ADR-0032).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskSnapshotAction {
    /// New internal snapshot of the current state.
    Create,
    /// The current state becomes the snapshot's.
    Apply,
    /// Removal of the snapshot.
    Delete,
}

/// Wraps the result of a snapshot's disk operation for the finishing step
/// (ADR-0032).
pub type SnapshotFinish = Box<dyn FnOnce(Result<(), PublicError>) -> Box<dyn Any + Send> + Send>;

/// First step of a user snapshot operation on the catalog (ADR-0032): the
/// journal intent is recorded; the disk operation on `disk` with `tag` is
/// then done by the engine (QMP while the VM runs, `qemu-img` on a worker
/// with the VM off) and its result handed to `finish`, whose output goes to
/// [`BaselinePort::finish_storage_job`] or
/// [`BaselinePort::abort_storage_job`] (both settle from the disk).
pub struct SnapshotStep {
    /// The VM's own disk.
    pub disk: PathBuf,
    /// Name of the internal snapshot in the disk.
    pub tag: String,
    /// Wraps the result of the disk operation for the finishing step.
    pub finish: SnapshotFinish,
}

impl std::fmt::Debug for SnapshotStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SnapshotStep({})", self.tag)
    }
}

/// Catalog side of a VMware import (ADR-0030): journal intent `import_vm`,
/// then the converted disk and the VM row published together. The default
/// is a catalog without imports.
pub trait VmImportPort {
    /// Catalog root (the converted disk lands below it).
    fn catalog_root(&self) -> Option<PathBuf> {
        None
    }
    /// Records the import of VM `vm` and returns where the converted disk
    /// is written.
    fn begin_vm_import(&mut self, _vm: VmId) -> Result<VmImportSlot, PublicError> {
        Err(PublicError::unsupported(
            "VMware import not available on this catalog",
        ))
    }
    /// Publishes the converted disk of `slot` and writes the VM row.
    fn finish_vm_import(&mut self, _slot: VmImportSlot, _vm: &VmRecord) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "VMware import not available on this catalog",
        ))
    }
    /// Rolls the import back (temporary and VM directory removed).
    fn abort_vm_import(&mut self, _slot: VmImportSlot) {}
}

/// A VMware import recorded in the catalog, between
/// [`VmImportPort::begin_vm_import`] and its end.
pub struct VmImportSlot {
    /// VM being created.
    pub vm_id: VmId,
    /// Where the converted disk is written (journal temporary).
    pub temp_path: PathBuf,
    /// Catalog-specific state.
    pub inner: Box<dyn Any + Send>,
}

impl std::fmt::Debug for VmImportSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VmImportSlot({})", self.vm_id)
    }
}

/// Verified and guarded files of a VM's current state for a start (F3).
pub struct StartDisks {
    /// Overlay of the current state.
    pub overlay: PathBuf,
    /// UEFI variables of the current state.
    pub vars: Option<PathBuf>,
    /// Base disk of the baseline (explicit read-only backing node).
    pub base: PathBuf,
    /// UEFI variables of the baseline (protected read-only during the run).
    pub base_vars: Option<PathBuf>,
    /// Catalog guard (baseline read-only, overlay not deletable), opened
    /// before the baseline's hash is verified through it
    /// ([`StartGuard::verify`], on the start worker): kept by the backend
    /// until the run is released.
    pub guard: Box<dyn StartGuard>,
}

impl std::fmt::Debug for StartDisks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartDisks")
            .field("overlay", &self.overlay)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// Baseline request served as a long storage operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineRequest {
    /// `create_baseline`.
    Create {
        /// VM.
        vm_id: VmId,
        /// Label.
        label: Label,
        /// Accept an unverified baseline after an unclean stop.
        allow_unclean: bool,
    },
    /// `restore_baseline`.
    Restore {
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
    /// `take_snapshot` (ADR-0032).
    TakeSnapshot {
        /// VM.
        vm_id: VmId,
        /// Name.
        name: Label,
        /// Description.
        description: String,
    },
    /// `revert_snapshot` (ADR-0032).
    RevertSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
    },
    /// `delete_snapshot` (ADR-0032).
    DeleteSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
    },
    /// `promote_snapshot` (ADR-0032): a baseline from a snapshot taken off.
    PromoteSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
        /// Label of the new baseline.
        label: Label,
    },
}

impl BaselineRequest {
    /// VM of the request.
    pub fn vm_id(&self) -> VmId {
        match self {
            Self::Create { vm_id, .. }
            | Self::Restore { vm_id, .. }
            | Self::TakeSnapshot { vm_id, .. }
            | Self::RevertSnapshot { vm_id, .. }
            | Self::DeleteSnapshot { vm_id, .. }
            | Self::PromoteSnapshot { vm_id, .. } => *vm_id,
        }
    }

    /// Kind of the core operation.
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::Create { .. } | Self::PromoteSnapshot { .. } => OperationKind::Snapshot,
            Self::Restore { .. } => OperationKind::Restore,
            Self::TakeSnapshot { .. } => OperationKind::UserSnapshot,
            Self::RevertSnapshot { .. } => OperationKind::UserSnapshotRevert,
            Self::DeleteSnapshot { .. } => OperationKind::UserSnapshotDelete,
        }
    }
}

/// Work of a catalog storage operation, opaque to the engine.
pub type StorageWork = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>;

/// Long storage operation to run on a worker thread.
pub struct StorageJob {
    vm_id: VmId,
    operation_id: OperationId,
    work: StorageWork,
}

impl StorageJob {
    /// VM.
    pub fn vm_id(&self) -> VmId {
        self.vm_id
    }

    /// Runs the work (blocking).
    pub fn run(self) -> StorageDone {
        StorageDone {
            vm_id: self.vm_id,
            operation_id: self.operation_id,
            result: (self.work)(),
        }
    }
}

impl std::fmt::Debug for StorageJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StorageJob({}, {})", self.vm_id, self.operation_id)
    }
}

/// Result of a [`StorageJob`], back to the supervisor thread.
pub struct StorageDone {
    vm_id: VmId,
    operation_id: OperationId,
    result: Box<dyn Any + Send>,
}

impl StorageDone {
    /// VM.
    pub fn vm_id(&self) -> VmId {
        self.vm_id
    }
}

impl std::fmt::Debug for StorageDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StorageDone({}, {})", self.vm_id, self.operation_id)
    }
}

/// Successful outcome of a baseline job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineOutcome {
    /// New baseline.
    Created(BaselineSummary),
    /// Baseline restored.
    Restored {
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
    /// User snapshot taken (ADR-0032).
    SnapshotTaken(SnapshotSummary),
    /// VM reverted to a user snapshot: `record` is the VM's new record (the
    /// snapshot's configuration, new revision).
    SnapshotReverted {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
        /// New record of the VM.
        record: Box<VmRecord>,
    },
    /// User snapshot deleted.
    SnapshotDeleted {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
    },
}

/// Baselines on a catalog (F3). The defaults describe a catalog without
/// baselines: listings empty, operations `unsupported`.
pub trait BaselinePort {
    /// Baselines of a VM, oldest first.
    fn list_baselines(&self, _vm: VmId) -> Result<Vec<BaselineSummary>, PublicError> {
        Ok(Vec::new())
    }
    /// Baseline facts of a VM for [`VmSummary`] (operation excluded).
    fn baseline_facts(&self, _vm: VmId) -> VmBaselines {
        VmBaselines::default()
    }
    /// First step of a baseline creation: checks (clean stop, budget, free
    /// space), guards and intents; returns the work for the worker thread.
    fn begin_create_baseline(
        &mut self,
        _vm: &VmRecord,
        _label: &Label,
        _allow_unclean: bool,
        _tool: Arc<dyn ImageTool>,
        _cancel: CancelCheck,
    ) -> Result<StorageWork, PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// First step of a restore.
    fn begin_restore_baseline(
        &mut self,
        _vm: &VmRecord,
        _baseline: BaselineId,
        _tool: Arc<dyn ImageTool>,
        _cancel: CancelCheck,
    ) -> Result<StorageWork, PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// Publishes the result of a work.
    fn finish_storage_job(
        &mut self,
        _done: Box<dyn Any + Send>,
    ) -> Result<BaselineOutcome, PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// Discards the result of a work. A user snapshot operation is settled
    /// from the disk instead: `Some` when its disk operation had completed
    /// and the result is kept.
    fn abort_storage_job(
        &mut self,
        _done: Box<dyn Any + Send>,
    ) -> Result<Option<BaselineOutcome>, PublicError> {
        Ok(None)
    }
    /// Deletes a baseline with its files.
    fn delete_baseline(&mut self, _vm: VmId, _baseline: BaselineId) -> Result<(), PublicError> {
        Err(PublicError::unsupported("baselines not available"))
    }
    /// Guarded files of the VM's current state for run `run`, `None` if the
    /// VM runs on its standalone disk. Cheap (supervisor thread): the guard
    /// handles are opened and the headers checked, the baseline's hash is
    /// verified afterwards through the same handles by
    /// [`StartGuard::verify`] (start worker), before QEMU is launched.
    fn start_disks(&self, _vm: VmId, _run: RunId) -> Result<Option<StartDisks>, PublicError> {
        Ok(None)
    }
    /// Records whether a run ended with an observed clean guest shutdown.
    fn record_exit(&self, _run: RunId, _clean: bool) {}

    // ----- user snapshots (ADR-0032) ---------------------------------------
    // Settled by `finish_storage_job` / `abort_storage_job` like the
    // baseline jobs; the defaults describe a catalog without snapshots.

    /// Snapshot tree of a VM (`available` and the reason filled by the
    /// catalog's own checks: overlay of a baseline, no disk yet, TPM).
    fn snapshot_tree(&self, _vm: &VmRecord) -> Result<SnapshotTree, PublicError> {
        Ok(SnapshotTree {
            available: false,
            unavailable_reason: Some("snapshots not available on this catalog".into()),
            ..SnapshotTree::default()
        })
    }
    /// Snapshot facts of a VM for [`VmSummary`].
    fn snapshot_facts(&self, _vm: VmId) -> VmSnapshots {
        VmSnapshots::default()
    }
    /// Configuration saved with a snapshot, and whether it was taken while
    /// the VM ran.
    fn snapshot_config(
        &self,
        _vm: VmId,
        _snapshot: SnapshotId,
    ) -> Result<(VmConfig, bool), PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// First step of a take: budget and free space, journal intent, copy of
    /// the UEFI variables (the run's file while running) and thumbnail.
    fn begin_take_snapshot(
        &mut self,
        _vm: &VmRecord,
        _name: &Label,
        _description: &str,
        _running: bool,
        _thumbnail: Option<Vec<u8>>,
        _reserve_bytes: u64,
    ) -> Result<SnapshotStep, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// First step of a revert (VM off): journal intent with the new record
    /// of the VM (`record`, computed by the core), copy of the snapshot's
    /// UEFI variables.
    fn begin_revert_snapshot(
        &mut self,
        _vm: &VmRecord,
        _snapshot: SnapshotId,
        _record: VmRecord,
    ) -> Result<SnapshotStep, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// First step of a deletion.
    fn begin_delete_snapshot(
        &mut self,
        _vm: &VmRecord,
        _snapshot: SnapshotId,
    ) -> Result<SnapshotStep, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// New name and description.
    fn update_snapshot(
        &mut self,
        _vm: VmId,
        _snapshot: SnapshotId,
        _name: &Label,
        _description: &str,
    ) -> Result<SnapshotSummary, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
    /// Thumbnail PNG of a snapshot, if one was kept.
    fn snapshot_thumbnail(
        &self,
        _vm: VmId,
        _snapshot: SnapshotId,
    ) -> Result<Option<Vec<u8>>, PublicError> {
        Ok(None)
    }
    /// First step of the promotion of a snapshot taken off to a baseline
    /// (ADR-0005 rules): as [`Self::begin_create_baseline`] with the
    /// snapshot's disk state, UEFI variables and configuration.
    fn begin_promote_snapshot(
        &mut self,
        _vm: &VmRecord,
        _snapshot: SnapshotId,
        _label: &Label,
        _tool: Arc<dyn ImageTool>,
        _cancel: CancelCheck,
    ) -> Result<StorageWork, PublicError> {
        Err(PublicError::unsupported("snapshots not available"))
    }
}

/// Worker step of a media import (open, checks, hash: ADR-0029, nothing is
/// copied), to run on a worker thread; its result goes back
/// to [`VmEngine::finish_import`] on the supervisor thread.
pub struct ImportJob(Box<dyn FnOnce() -> ImportCopied + Send>);

impl ImportJob {
    /// Job from a closure.
    pub fn new(f: impl FnOnce() -> ImportCopied + Send + 'static) -> Self {
        Self(Box::new(f))
    }

    /// Runs the step (blocking).
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

/// Media import in two steps on a catalog (the hash runs between them,
/// outside the catalog).
pub trait MediaImportPort {
    /// Reserves the import; the returned job opens, checks and hashes the
    /// source.
    fn begin_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &Label,
    ) -> Result<ImportJob, PublicError>;
    /// Records the medium checked and hashed by the job.
    fn finish_import(&mut self, copied: ImportCopied) -> Result<MediaEntry, PublicError>;
    /// Discards the outcome of the job without recording anything.
    fn abort_import(&mut self, copied: ImportCopied) -> Result<(), PublicError>;
    /// A start found the file of a medium used in place missing (ADR-0029):
    /// the registry marks it `missing`. Best effort, no outcome.
    fn mark_media_missing(&self, _id: MediaId) {}
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

/// Automatic reset decided by the backend for a guest whose virtual CPUs
/// stopped on an unrecoverable error (audit `vm_auto_reset`, actor `system`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoReset {
    /// VM.
    pub vm_id: VmId,
    /// Run.
    pub run_id: RunId,
    /// `succeeded` (reset and resumed), `failed` (QMP refused) or `denied`
    /// (limit reached: the guest is left stopped).
    pub outcome: AuditOutcome,
    /// Code of a failure or denial.
    pub error: Option<ErrorCode>,
}

/// VM backend as seen by the engine: the core's `VmBackend` port plus the
/// asynchronous observations (process exit not requested by the core).
pub trait BackendPort: VmBackend + Send + 'static {
    /// Accumulated observations to apply to the core with `Core::observe`.
    fn drain_observations(&mut self) -> Vec<(VmId, BackendObservation)>;
    /// The guest of the run is stopped on a virtual CPU error and was not
    /// restarted automatically (the process is alive): shown in
    /// [`VmSummary::vcpu_halt`].
    fn vcpu_halt(&self, _vm_id: VmId, _run_id: RunId) -> Option<VcpuHalt> {
        None
    }
    /// Automatic resets decided since the last call, for the audit.
    fn take_auto_resets(&mut self) -> Vec<AutoReset> {
        Vec::new()
    }
    /// Guests this backend can run on this host (ADR-0007, ADR-0028): by
    /// default the ones the host accelerates
    /// ([`lightvm_proto::GuestOs::runs_on_this_host_with`]).
    fn guest_support(&self) -> vm_core::resources::GuestSupport {
        lightvm_proto::GuestOs::runs_on_this_host_with
    }
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
    /// Image bytes of the last screenshot of the run (F4: agent screenshots),
    /// with its metadata; taken once.
    /// QEMU diagnostics (stderr) of a run, last part, raw (F5 `vm.logs`;
    /// the service removes host paths).
    fn qemu_log(&mut self, _vm_id: VmId, _run_id: RunId) -> Option<String> {
        None
    }

    /// Disk-only internal snapshot `tag` of the running VM's own disk (QMP
    /// `blockdev-snapshot-internal-sync`, ADR-0032).
    fn snapshot_disk(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _tag: &str,
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "snapshots of a running VM not available on this backend",
        ))
    }
    /// Deletion of the internal snapshot `tag` of the running VM's disk.
    fn delete_disk_snapshot(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _tag: &str,
    ) -> Result<(), PublicError> {
        Err(PublicError::unsupported(
            "snapshots of a running VM not available on this backend",
        ))
    }
    /// Small PNG of the screen of a running VM for a snapshot thumbnail.
    fn snapshot_thumbnail(&mut self, _vm_id: VmId, _run_id: RunId) -> Option<Vec<u8>> {
        None
    }
    fn take_screenshot(&mut self, _vm_id: VmId, _run_id: RunId) -> Option<(Screenshot, Vec<u8>)> {
        None
    }
    /// Disk image tool for baselines (F3), if the backend has one.
    fn image_tool(&self) -> Option<Arc<dyn ImageTool>> {
        None
    }
    /// Files of the current state for the start of `run_id` that follows
    /// (F3): the backend uses them instead of the standalone disk and keeps
    /// the guard until the run is released. A backend without support
    /// ignores them (and starts the standalone disk).
    fn set_start_disks(&mut self, _vm_id: VmId, _run_id: RunId, _disks: StartDisks) {}
    /// Protected handles of the approved media verified for the start of
    /// `run_id` that follows: the backend keeps them until the run is
    /// released (dropped if the start fails), so the files QEMU opens are
    /// the files hashed through them. A backend without processes may drop
    /// them.
    fn set_start_media(
        &mut self,
        _vm_id: VmId,
        _run_id: RunId,
        _guards: Vec<Box<dyn GuardedMedia>>,
    ) {
    }
    /// Runs ended since the last call, with `true` for an observed clean
    /// guest shutdown (F3: a baseline is "verified" only after one).
    fn take_exits(&mut self) -> Vec<(VmId, RunId, bool)> {
        Vec::new()
    }
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

fn summary(s: VmStatus, disk_used_bytes: Option<u64>, mut baselines: VmBaselines) -> VmSummary {
    baselines.operation = match s.storage_operation {
        Some(OperationKind::Snapshot) => Some(StorageOperation::CreateBaseline),
        Some(OperationKind::Restore) => Some(StorageOperation::RestoreBaseline),
        Some(OperationKind::UserSnapshot) => Some(StorageOperation::TakeSnapshot),
        Some(OperationKind::UserSnapshotRevert) => Some(StorageOperation::RevertSnapshot),
        Some(OperationKind::UserSnapshotDelete) => Some(StorageOperation::DeleteSnapshot),
        _ => None,
    };
    VmSummary {
        vm_id: s.vm_id,
        name: s.name,
        state: s.state,
        run_id: s.run_id,
        config: s.config,
        config_revision: s.config_revision,
        disk_used_bytes,
        baselines,
        snapshots: VmSnapshots::default(),
        start: None,
        vcpu_halt: None,
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
    /// VMs whose guest was stopped on a vCPU error at the last
    /// [`VmEngine::state_changes`].
    last_halted: BTreeSet<VmId>,
    /// Last display geometry refresh per VM (run, instant): debounce.
    last_refresh: BTreeMap<VmId, (RunId, std::time::Instant)>,
    /// Graceful stops forwarded and still running in the backend: VM â†’
    /// (operation, run). The operation stays forwarded in the core (VM in
    /// `stopping`) until the backend reports the outcome.
    pending_stops: BTreeMap<VmId, (OperationId, RunId)>,
    /// Start checks left to a worker ([`VmEngine::offload_start_checks`]).
    offload: bool,
    /// Starts whose checks are running: the forwarded start, its progress
    /// and its cancellation flag. The VM is `starting` meanwhile.
    pending_starts: BTreeMap<VmId, PendingStart>,
    /// Start checks begun and not yet taken by the supervisor.
    start_jobs: Vec<StartJob>,
    /// Service-side data of the agent sessions (F4).
    session_meta: BTreeMap<SessionId, SessionMeta>,
    /// Last screenshot of each agent session, read back in chunks.
    shots: BTreeMap<SessionId, StoredShot>,
    /// Session states at the last [`VmEngine::session_changes`].
    last_sessions: BTreeMap<SessionId, (SessionState, Option<TerminationCause>)>,
    /// Input owners at the last [`VmEngine::input_owner_changes`].
    last_owners: BTreeMap<VmId, InputOwner>,
    /// Gate of the service (pause and revocation decided outside).
    gate: Option<Arc<dyn SessionGate>>,
    /// Reports of sessions terminated through the gate, for the reply of
    /// the core revocation that follows.
    gate_reports: BTreeMap<SessionId, RevocationReport>,
    /// Storage operations of agent sessions in progress (F5): operation →
    /// session, to record the baselines a session created.
    agent_storage: BTreeMap<OperationId, SessionId>,
}

/// Service-side data of an agent session.
#[derive(Debug, Clone)]
struct SessionMeta {
    vm_id: VmId,
    connection_id: u64,
    client_name: String,
    purpose: String,
    lease_ms: i64,
    /// Baseline the VM's current state was an overlay of at approval: the
    /// session's protected initial baseline (F5), restorable by the agent.
    initial_baseline: Option<BaselineId>,
    /// Baselines created by the session (`vm.snapshot`), restorable too.
    created: BTreeSet<BaselineId>,
}

/// Screenshot of an agent session kept for its chunked read.
#[derive(Debug)]
struct StoredShot {
    operation: OperationRecord,
    meta: Screenshot,
    png: Vec<u8>,
}

/// SHA-256 of a byte buffer.
fn sha256(bytes: &[u8]) -> lightvm_proto::Sha256Digest {
    use sha2::Digest as _;
    lightvm_proto::Sha256Digest::from_bytes(sha2::Sha256::digest(bytes).into())
}

/// Terminated sessions listed for the UI after the live ones.
const TERMINATED_SESSIONS_LISTED: usize = 16;

/// Start forwarded by the core whose file checks are running.
struct PendingStart {
    dispatch: vm_core::Dispatch,
    progress: Arc<ProgressCell>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
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
    S: CatalogStore + MediaImportPort + BaselinePort + VmImportPort + Send + 'static,
    A: AuditSink + Send + 'static,
    B: BackendPort,
    M: MediaGuardPort + Send + 'static,
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
        core.set_guest_support(backend.guest_support());
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
            last_halted: BTreeSet::new(),
            last_refresh: BTreeMap::new(),
            pending_stops: BTreeMap::new(),
            offload: false,
            pending_starts: BTreeMap::new(),
            start_jobs: Vec::new(),
            session_meta: BTreeMap::new(),
            shots: BTreeMap::new(),
            last_sessions: BTreeMap::new(),
            last_owners: BTreeMap::new(),
            gate: None,
            gate_reports: BTreeMap::new(),
            agent_storage: BTreeMap::new(),
        };
        let _ = e.state_changes();
        Ok(e)
    }

    /// Core (read-only, for tests and diagnostics).
    pub fn core(&self) -> &Core<S, SystemClock, SystemIds, A> {
        &self.core
    }

    /// Applies to the core the pauses and terminations decided by the
    /// service gate since the last call (priority path, ROADMAP 7.3): from
    /// here on the core's own checks refuse the session.
    fn sync_gates(&mut self) {
        let Some(gate) = self.gate.clone() else {
            return;
        };
        let changes = gate.take_changes();
        if changes.is_empty() {
            return;
        }
        let mut ended = Vec::new();
        for (id, change) in changes {
            match change {
                GateChange::Pause => {
                    let _ = self.core.pause_session(id);
                }
                GateChange::Terminate { cause, stop } => {
                    let r = if stop {
                        self.core.terminate_session_with_stop(id, cause)
                    } else {
                        self.core.revoke_session(id, cause)
                    };
                    if let Ok(t) = r {
                        self.gate_reports.insert(id, t.report.clone());
                        ended.push(t);
                    }
                }
            }
        }
        self.execute_stops(&ended);
    }

    fn apply_observations(&mut self) {
        self.sync_gates();
        self.record_exits();
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
        for r in self.backend.take_auto_resets() {
            self.core
                .record_auto_reset(r.vm_id, r.run_id, r.outcome, r.error);
        }
        self.record_exits();
        self.settle_pending_stops();
    }

    /// F6, recovery after a crash: a VM left `failed` by its run (QEMU
    /// crashed or killed from outside, start failed) has no process any more
    /// (the backend observed the exit). Once that state was published (the
    /// UI and the reports saw `failed`), the next observation brings it back
    /// to `stopped` through `recovering`, as a restarted supervisor does; the
    /// catalog keeps the run's end cause (`crash`, `start_failed`) and the
    /// audit the state changes. Without this a crashed VM stayed `failed`
    /// (no start, restore or deletion) until the service restarted.
    fn recover_failed(&mut self) {
        let failed: Vec<VmId> = self
            .last
            .iter()
            .filter(|(_, (s, _))| *s == VmState::Failed)
            .map(|(id, _)| *id)
            .collect();
        for vm_id in failed {
            let still_failed = self
                .core
                .vm_status(&Caller::User, vm_id)
                .is_ok_and(|st| st.state == VmState::Failed);
            if !still_failed {
                continue;
            }
            let r = self
                .core
                .begin_recovery(vm_id)
                .and_then(|()| self.core.finish_recovery(vm_id));
            if let Err(e) = r {
                crate::diag!(
                    "lightvm-service: failed VM not brought back to stopped: {}",
                    e.message
                );
            }
        }
    }

    /// Clean or unclean ends of the runs, recorded in the catalog (F3).
    fn record_exits(&mut self) {
        for (_, run, clean) in self.backend.take_exits() {
            self.core.store().record_exit(run, clean);
        }
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
        let baselines = self.core.store().baseline_facts(s.vm_id);
        let start = match self.pending_starts.get(&s.vm_id) {
            Some(p) if s.state == VmState::Starting && s.run_id == Some(p.dispatch.run_id) => {
                Some(p.progress.snapshot())
            }
            _ => None,
        };
        let vcpu_halt = match (s.state, s.run_id) {
            (VmState::Running, Some(run)) => self.backend.vcpu_halt(s.vm_id, run),
            _ => None,
        };
        let snapshots = self.core.store().snapshot_facts(s.vm_id);
        let mut out = summary(s, used, baselines);
        out.snapshots = snapshots;
        out.start = start;
        out.vcpu_halt = vcpu_halt;
        out
    }

    /// Guests stopped on a virtual CPU error, for the change detection of
    /// [`VmEngine::state_changes`].
    fn halted(&self, vm: &VmStatus) -> bool {
        matches!((vm.state, vm.run_id), (VmState::Running, Some(run))
            if self.backend.vcpu_halt(vm.vm_id, run).is_some())
    }

    /// Starts in progress (checks running on a worker).
    pub fn pending_starts(&self) -> usize {
        self.pending_starts.len()
    }

    /// First step of a start forwarded by the core (supervisor thread):
    /// opens the files to verify with their guards (cheap) and returns the
    /// checks to run. A refusal here ends the start at once: current-state
    /// files refused â†’ VM back to `stopped` (as before F3 offload); a medium
    /// that cannot be opened â†’ start failed (as an inspection error).
    fn begin_start(&mut self, d: vm_core::Dispatch) -> Result<Option<StartJob>, PublicError> {
        let vm_id = d.vm_id;
        let disks = match self.core.store().start_disks(vm_id, d.run_id) {
            Ok(disks) => disks,
            Err(e) => {
                self.core.refuse_start(vm_id, d.operation_id, e)?;
                return Ok(None);
            }
        };
        let entries = d
            .start
            .as_ref()
            .map(|p| p.media.clone())
            .unwrap_or_default();
        let mut media = Vec::with_capacity(entries.len());
        for entry in entries {
            match self.inspector.open_for_start(&entry) {
                Ok(g) => media.push((entry, g)),
                Err(e) => {
                    if e.code == ErrorCode::NotFound {
                        self.core.store().mark_media_missing(entry.record.id);
                    }
                    drop(disks);
                    self.core.finish_dispatch(vm_id, d.operation_id, Err(e))?;
                    return Ok(None);
                }
            }
        }
        let job = StartJob::new(vm_id, d.run_id, d.operation_id, media, disks);
        self.pending_starts.insert(
            vm_id,
            PendingStart {
                dispatch: d,
                progress: job.progress.clone(),
                cancel: job.cancel.clone(),
            },
        );
        Ok(Some(job))
    }

    /// Last step of a start (supervisor thread): launch if the run is still
    /// current and every check passed, otherwise record the failure.
    fn complete_start(
        &mut self,
        done: StartDone,
        abort: Option<PublicError>,
    ) -> Result<OperationRecord, PublicError> {
        let vm_id = done.vm_id;
        let pending = match self.pending_starts.get(&vm_id) {
            Some(p) if p.dispatch.operation_id == done.operation_id => {
                self.pending_starts.remove(&vm_id)
            }
            _ => None,
        };
        let Some(pending) = pending else {
            return Err(PublicError::not_found("start not pending"));
        };
        let d = pending.dispatch;
        let current = self
            .core
            .vm_status(&Caller::User, vm_id)
            .is_ok_and(|s| s.state == VmState::Starting && s.run_id == Some(done.run_id));
        let StartDone {
            run_id,
            facts,
            guards,
            disks,
            outcome,
            ..
        } = done;
        let r = if !current {
            // Forced stop or Emergency Stop during the checks: the run is
            // already closed, nothing is launched.
            drop((guards, disks));
            self.core
                .finish_dispatch(vm_id, d.operation_id, Err(cancelled_error()))
        } else if let Some(e) = abort {
            drop((guards, disks));
            self.core.refuse_start(vm_id, d.operation_id, e)
        } else {
            match outcome {
                Err(StartFailure::Cancelled) => {
                    drop((guards, disks));
                    self.core
                        .refuse_start(vm_id, d.operation_id, cancelled_error())
                }
                Err(StartFailure::Baseline(e)) => {
                    drop((guards, disks));
                    self.core.refuse_start(vm_id, d.operation_id, e)
                }
                // The core compares the facts and records the failure.
                Err(StartFailure::Media) => {
                    drop((guards, disks));
                    let mut pre = PrecomputedInspector(facts);
                    self.core.execute_dispatch(d, &mut self.backend, &mut pre)
                }
                Ok(()) => {
                    if let Some(disks) = disks {
                        self.backend.set_start_disks(vm_id, run_id, disks);
                    }
                    if !guards.is_empty() {
                        self.backend.set_start_media(vm_id, run_id, guards);
                    }
                    let mut pre = PrecomputedInspector(facts);
                    self.core.execute_dispatch(d, &mut self.backend, &mut pre)
                }
            }
        };
        // Operations queued behind the start.
        let _ = self.run_queue(vm_id);
        r
    }

    /// Cancels the checks of a pending start of the VM (forced stop,
    /// Emergency Stop): the worker stops within one block.
    fn cancel_pending_start(&self, vm_id: VmId) {
        if let Some(p) = self.pending_starts.get(&vm_id) {
            p.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
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

    /// Releases the pressed keys of the runs whose input owner changed
    /// (core decision, ROADMAP 6.4).
    fn flush_input_releases(&mut self) {
        for (vm_id, run_id) in self.core.take_input_releases() {
            self.backend.release_input(vm_id, run_id);
        }
    }

    /// Kills the runs planned by the core for terminated sessions (expiry,
    /// client loss, UI loss) with OS primitives, outside the queue, and
    /// records the observed end.
    fn execute_stops(&mut self, terminations: &[vm_core::SessionTermination]) {
        for t in terminations {
            for plan in &t.emergency_stops {
                self.cancel_pending_start(plan.vm_id);
                if let Some(run) = plan.run_id {
                    let killed = self.backend.force_stop(plan.vm_id, run);
                    if let Err(e) = &killed {
                        crate::diag!(
                            "lightvm-service: stop of a terminated session's VM: {}",
                            e.message
                        );
                    }
                    let _ = self
                        .core
                        .finish_emergency_stop(Actor::System, plan.vm_id, run, killed);
                }
            }
            self.shots.remove(&t.session_id);
        }
        if !terminations.is_empty() {
            self.flush_input_releases();
            self.apply_observations();
        }
    }

    /// Session for the UI.
    fn session_info(&self, id: SessionId) -> Option<SessionInfo> {
        let s = self.core.sessions().get(id)?;
        let meta = self.session_meta.get(&id);
        let vm_id = meta
            .map(|m| m.vm_id)
            .or_else(|| s.grant.vm_allowlist.iter().next().copied())?;
        let live = s.state != SessionState::Terminated;
        Some(SessionInfo {
            session_id: id,
            vm_id,
            connection_id: meta.filter(|_| live).map(|m| m.connection_id),
            client_name: meta.map(|m| m.client_name.clone()).unwrap_or_default(),
            purpose: meta.map(|m| m.purpose.clone()).unwrap_or_default(),
            state: s.state,
            termination: s.termination,
            capabilities: s.grant.capabilities.clone(),
            quotas: s.grant.quotas,
            usage: lightvm_proto::ipc::SessionUsage {
                operations: s.usage.operations,
                input_events: s.usage.input_events,
                screenshots: s.usage.screenshots,
            },
            created_at: s.created_at,
            expires_at: s.grant.expires_at,
            lease_expires_at: s.lease.expires_at,
            input_owner: self.core.input_owner(vm_id) == InputOwner::Agent { session_id: id },
        })
    }

    fn agent_view(&self, st: VmStatus, session: SessionId) -> AgentVmView {
        AgentVmView {
            vm_id: st.vm_id,
            name: st.name,
            state: st.state,
            run_id: st.run_id,
            geometry: st.geometry,
            input_owner: self.core.input_owner(st.vm_id)
                == InputOwner::Agent {
                    session_id: session,
                },
        }
    }

    /// `true` if `caller` presents the right token of a live session.
    fn session_token_ok(&self, session_id: SessionId, token: Option<&SessionToken>) -> bool {
        self.core.sessions().get(session_id).is_some_and(|s| {
            s.state != SessionState::Terminated && token.is_some_and(|t| s.token_matches(t))
        })
    }

    /// Agent screenshot: through the core queue (two checks, quota, byte
    /// limit of the grant), then the PNG from the backend, verified against
    /// the metadata, kept for the chunked read.
    fn agent_screenshot(
        &mut self,
        caller: &Caller,
        session_id: SessionId,
        operation_id: OperationId,
        vm_id: VmId,
        expected_run_id: RunId,
    ) -> Result<ResponseBody, PublicError> {
        if let Some(shot) = self.shots.get(&session_id)
            && shot.operation.id == operation_id
        {
            // Idempotent repeat of the same screenshot request.
            return Ok(shot_reply(shot));
        }
        let req = VmRequest {
            operation_id,
            vm_id,
            expected_run_id: Some(expected_run_id),
            action: VmAction::Screenshot,
        };
        let (_, record) = self.submit(caller, req)?;
        let record =
            record.ok_or_else(|| PublicError::infrastructure("screenshot operation not found"))?;
        match record.status {
            OperationStatus::Succeeded => {}
            OperationStatus::Failed | OperationStatus::Discarded => {
                let code = record.error.unwrap_or(ErrorCode::InfrastructureError);
                return Err(PublicError::new(code, "screenshot not taken"));
            }
            _ => return Err(PublicError::vm_busy("screenshot not completed")),
        }
        let (meta, png) = self
            .backend
            .take_screenshot(vm_id, expected_run_id)
            .ok_or_else(|| PublicError::infrastructure("screenshot bytes not available"))?;
        if meta.vm_id != vm_id
            || meta.run_id != expected_run_id
            || meta.byte_len != png.len() as u64
            || sha256(&png) != meta.sha256
        {
            return Err(PublicError::infrastructure(
                "screenshot bytes inconsistent with the metadata",
            ));
        }
        let shot = StoredShot {
            operation: record,
            meta,
            png,
        };
        let reply = shot_reply(&shot);
        self.shots.insert(session_id, shot);
        Ok(reply)
    }

    /// Forwards the queued operations of the VM. A graceful stop is started
    /// in the backend and left forwarded (the supervisor thread is not held
    /// for the ACPI wait); a forced stop queued meanwhile is forwarded at
    /// once by the core and wins.
    fn run_queue(&mut self, vm_id: VmId) -> Result<(), PublicError> {
        loop {
            // The gate may have changed while the previous operation was in
            // the backend: the core sees it before the next forwarding.
            self.sync_gates();
            let Some(d) = self.core.begin_dispatch(vm_id)? else {
                break;
            };
            if d.action == VmAction::Start {
                // Files to verify opened with their guards here; the hashes
                // run on a worker (offload) or inline, then `complete_start`
                // launches QEMU (see `crate::start`). F3: the current state
                // may be an overlay of a baseline, refused before any
                // process (the VM stays stopped).
                if let Some(job) = self.begin_start(d)? {
                    if self.offload {
                        self.start_jobs.push(job);
                    } else {
                        let done = job.run(&|| false);
                        self.complete_start(done, None)?;
                    }
                }
                continue;
            }
            if d.action == VmAction::ForceStop {
                // A forced stop during the checks of a start cancels them.
                self.cancel_pending_start(vm_id);
            }
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
            // Not a start: no medium to inspect.
            self.core.execute_dispatch(
                d,
                &mut self.backend,
                &mut PrecomputedInspector::default(),
            )?;
        }
        self.settle_pending_stops();
        Ok(())
    }
}

impl<S, A, B, M> CoreEngine<S, A, B, M>
where
    S: CatalogStore + MediaImportPort + BaselinePort + VmImportPort + Send + 'static,
    A: AuditSink + Send + 'static,
    B: BackendPort,
    M: MediaGuardPort + Send + 'static,
{
    /// User snapshot take, revert or delete (ADR-0032): core checks (user
    /// only, no agent session on the VM, VM off, or running for take and
    /// delete), journal intent on the catalog, then the disk operation:
    /// while the VM runs it is done here through QMP (the guest is paused
    /// by QEMU for its duration) and the returned work only hands the result
    /// over; with the VM off the work runs `qemu-img` on the worker. Either
    /// way the catalog settles it from the disk when the work comes back.
    fn begin_snapshot_job(
        &mut self,
        caller: &Caller,
        req: BaselineRequest,
        tool: Option<Arc<dyn ImageTool>>,
        cancel: CancelCheck,
    ) -> Result<StorageJob, PublicError> {
        let vm_id = req.vm_id();
        let kind = req.kind();
        if let BaselineRequest::TakeSnapshot { description, .. } = &req {
            lightvm_proto::ipc::validate_snapshot_description(description).map_err(|m| {
                PublicError::invalid_argument(m).with_detail("field", "description")
            })?;
        }
        // A revert gives the VM the snapshot's configuration: computed (and
        // checked against the current limits) before anything is written.
        let reverted = match &req {
            BaselineRequest::RevertSnapshot { snapshot_id, .. }
                if matches!(caller, Caller::User) =>
            {
                let (config, _) = self.core.store().snapshot_config(vm_id, *snapshot_id)?;
                Some(self.core.reverted_record(vm_id, config)?)
            }
            _ => None,
        };
        let reserve = self
            .core
            .config()
            .host
            .reserved_storage_gib
            .saturating_mul(1 << 30);
        // The thumbnail of a running VM is taken before the operation.
        let thumbnail = match (&req, self.core.vm_status(&Caller::User, vm_id)) {
            (BaselineRequest::TakeSnapshot { .. }, Ok(st))
                if st.state == VmState::Running && matches!(caller, Caller::User) =>
            {
                st.run_id
                    .and_then(|run| self.backend.snapshot_thumbnail(vm_id, run))
            }
            _ => None,
        };
        let backend = &mut self.backend;
        let (operation_id, work) =
            self.core
                .begin_snapshot_operation(caller, vm_id, kind, |store, vm, run| {
                    let step = match &req {
                        BaselineRequest::TakeSnapshot {
                            name, description, ..
                        } => store.begin_take_snapshot(
                            vm,
                            name,
                            description,
                            run.is_some(),
                            thumbnail,
                            reserve,
                        )?,
                        BaselineRequest::RevertSnapshot { snapshot_id, .. } => {
                            let record = reverted.ok_or_else(|| {
                                PublicError::infrastructure("reverted configuration missing")
                            })?;
                            store.begin_revert_snapshot(vm, *snapshot_id, record)?
                        }
                        BaselineRequest::DeleteSnapshot { snapshot_id, .. } => {
                            store.begin_delete_snapshot(vm, *snapshot_id)?
                        }
                        _ => return Err(PublicError::invalid_argument("not a snapshot operation")),
                    };
                    let SnapshotStep { disk, tag, finish } = step;
                    let work: StorageWork = match run {
                        Some(run) => {
                            let r = match &req {
                                BaselineRequest::TakeSnapshot { .. } => {
                                    backend.snapshot_disk(vm.id, run, &tag)
                                }
                                _ => backend.delete_disk_snapshot(vm.id, run, &tag),
                            };
                            Box::new(move || finish(r))
                        }
                        None => {
                            let action = match &req {
                                BaselineRequest::TakeSnapshot { .. } => DiskSnapshotAction::Create,
                                BaselineRequest::RevertSnapshot { .. } => DiskSnapshotAction::Apply,
                                _ => DiskSnapshotAction::Delete,
                            };
                            match tool {
                                Some(tool) => Box::new(move || {
                                    let c = || cancel();
                                    finish(tool.internal_snapshot(&disk, action, &tag, &c))
                                }),
                                None => {
                                    let e = PublicError::unsupported(
                                        "snapshots need qemu-img: configure the QEMU folder",
                                    );
                                    Box::new(move || finish(Err(e)))
                                }
                            }
                        }
                    };
                    Ok(work)
                })?;
        Ok(StorageJob {
            vm_id,
            operation_id,
            work,
        })
    }
}

fn chunk_count(len: usize) -> u32 {
    u32::try_from(len.div_ceil(SCREENSHOT_CHUNK_BYTES as usize)).unwrap_or(u32::MAX)
}

fn shot_reply(shot: &StoredShot) -> ResponseBody {
    ResponseBody::AgentScreenshot {
        operation: shot.operation.clone(),
        screenshot: shot.meta.clone(),
        chunk_bytes: SCREENSHOT_CHUNK_BYTES,
        chunks: chunk_count(shot.png.len()),
    }
}

impl<S, A, B, M> VmEngine for CoreEngine<S, A, B, M>
where
    S: CatalogStore + MediaImportPort + BaselinePort + VmImportPort + Send + 'static,
    A: AuditSink + Send + 'static,
    B: BackendPort,
    M: MediaGuardPort + Send + 'static,
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
        // The capture of the geometry may have taken a while.
        self.sync_gates();
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
        // Start checks still running are cancelled: their VM is stopped below.
        for vm in self.pending_starts.keys() {
            self.cancel_pending_start(*vm);
        }
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
        // Sessions on VMs without a process end too: the Emergency Stop
        // revokes every agent session (F4).
        let _ = self
            .core
            .terminate_all_sessions(TerminationCause::EmergencyStop, false);
        self.shots.clear();
        self.flush_input_releases();
        self.apply_observations();
        self.settle_pending_stops();
    }

    fn observe(&mut self) {
        self.apply_observations();
        self.recover_failed();
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
        // escalation (ACPI wait â†’ `quit` â†’ termination): wait for them.
        let deadline = std::time::Instant::now() + STOP_ALL_LIMIT;
        while !self.pending_stops.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.apply_observations();
        }
    }

    fn state_changes(&mut self) -> Vec<(VmId, VmState)> {
        self.flush_input_releases();
        let Ok(vms) = self.core.list_vms(&Caller::User) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for vm in vms {
            seen.insert(vm.vm_id);
            let now = (vm.state, vm.run_id);
            // A guest that stops on a vCPU error (or runs again) keeps the
            // `running` state: the change is published as a state event so
            // the clients re-read the summary (`vcpu_halt`).
            let halted = self.halted(&vm);
            let halt_changed = if halted {
                self.last_halted.insert(vm.vm_id)
            } else {
                self.last_halted.remove(&vm.vm_id)
            };
            if self.last.insert(vm.vm_id, now) != Some(now) || halt_changed {
                out.push((vm.vm_id, vm.state));
            }
        }
        self.last.retain(|id, _| seen.contains(id));
        self.last_halted.retain(|id| seen.contains(id));
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

    fn delete_media(&mut self, caller: &Caller, media_id: MediaId) -> Result<(), PublicError> {
        // Configurations are current: a VM created or changed just before
        // is seen by the core's check.
        self.apply_observations();
        self.core.delete_media(caller, media_id)
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

    fn list_baselines(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
    ) -> Result<Vec<BaselineSummary>, PublicError> {
        self.apply_observations();
        self.core
            .with_visible_vm(caller, vm_id, |store| store.list_baselines(vm_id))
    }

    fn begin_baseline_job(
        &mut self,
        caller: &Caller,
        req: BaselineRequest,
        cancel: CancelCheck,
    ) -> Result<StorageJob, PublicError> {
        self.apply_observations();
        if let Caller::Agent { session_id, .. } = caller {
            // F5: an agent restores only its session's initial baseline or a
            // baseline it created (never one of the user's other baselines).
            if let BaselineRequest::Restore { baseline_id, .. } = &req {
                let allowed = self.session_meta.get(session_id).is_some_and(|m| {
                    m.initial_baseline == Some(*baseline_id) || m.created.contains(baseline_id)
                });
                if !allowed {
                    return Err(PublicError::permission_denied(
                        "the session may restore only its initial baseline or the ones it created",
                    )
                    .with_detail("reason", "baseline_not_restorable"));
                }
            }
        }
        let tool = self.backend.image_tool();
        let vm_id = req.vm_id();
        let kind = req.kind();
        if matches!(
            req,
            BaselineRequest::TakeSnapshot { .. }
                | BaselineRequest::RevertSnapshot { .. }
                | BaselineRequest::DeleteSnapshot { .. }
        ) {
            return self.begin_snapshot_job(caller, req, tool, cancel);
        }
        if matches!(req, BaselineRequest::PromoteSnapshot { .. })
            && matches!(caller, Caller::Agent { .. })
        {
            return Err(PublicError::permission_denied(
                "snapshots are not available to agent sessions",
            ));
        }
        let (operation_id, work) =
            self.core
                .begin_storage_operation(caller, vm_id, kind, |store, vm, _op| {
                    let tool = tool.ok_or_else(|| {
                        PublicError::unsupported("baselines not available on this backend")
                    })?;
                    match &req {
                        BaselineRequest::Create {
                            label,
                            allow_unclean,
                            ..
                        } => store.begin_create_baseline(vm, label, *allow_unclean, tool, cancel),
                        BaselineRequest::Restore { baseline_id, .. } => {
                            store.begin_restore_baseline(vm, *baseline_id, tool, cancel)
                        }
                        BaselineRequest::PromoteSnapshot {
                            snapshot_id, label, ..
                        } => store.begin_promote_snapshot(vm, *snapshot_id, label, tool, cancel),
                        _ => Err(PublicError::invalid_argument("not a baseline operation")),
                    }
                })?;
        if let Caller::Agent { session_id, .. } = caller {
            self.agent_storage.insert(operation_id, *session_id);
        }
        Ok(StorageJob {
            vm_id,
            operation_id,
            work,
        })
    }

    fn finish_baseline_job(&mut self, done: StorageDone) -> Result<BaselineOutcome, PublicError> {
        let StorageDone {
            vm_id,
            operation_id,
            result,
        } = done;
        let session = self.agent_storage.remove(&operation_id);
        // A revert to a user snapshot gives the VM the snapshot's
        // configuration: it becomes the core's record (ADR-0032).
        let r = self
            .core
            .finish_snapshot_operation(vm_id, operation_id, |store| {
                let outcome = store.finish_storage_job(result)?;
                let record = match &outcome {
                    BaselineOutcome::SnapshotReverted { record, .. } => Some((**record).clone()),
                    _ => None,
                };
                Ok((outcome, record))
            });
        if let (Ok(BaselineOutcome::Created(b)), Some(s)) = (&r, session)
            && let Some(m) = self.session_meta.get_mut(&s)
        {
            m.created.insert(b.baseline_id);
        }
        r
    }

    fn abort_baseline_job(&mut self, done: StorageDone, reason: PublicError) -> PublicError {
        let StorageDone {
            vm_id,
            operation_id,
            result,
        } = done;
        self.agent_storage.remove(&operation_id);
        let r = self
            .core
            .finish_snapshot_operation(vm_id, operation_id, |store| {
                match store.abort_storage_job(result) {
                    Err(e) => {
                        crate::diag!(
                            "lightvm-service: cancelled storage operation not rolled back: {e}"
                        );
                        Err(reason)
                    }
                    Ok(None) => Err(reason),
                    // A user snapshot operation is settled from the disk: a
                    // disk operation that completed before the cancellation
                    // is kept (and audited as succeeded).
                    Ok(Some(outcome)) => {
                        let record = match &outcome {
                            BaselineOutcome::SnapshotReverted { record, .. } => {
                                Some((**record).clone())
                            }
                            _ => None,
                        };
                        Ok((outcome, record))
                    }
                }
            });
        match r {
            Err(e) => e,
            Ok(_) => PublicError::state_conflict(
                "operation cancelled after the disk operation had completed: its result is kept",
            )
            .with_detail("reason", "completed_before_cancel"),
        }
    }

    fn delete_baseline(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        baseline_id: BaselineId,
    ) -> Result<(), PublicError> {
        self.apply_observations();
        self.core.delete_baseline_with(caller, vm_id, |store| {
            store.delete_baseline(vm_id, baseline_id)
        })
    }

    fn list_snapshots(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
    ) -> Result<SnapshotTree, PublicError> {
        self.apply_observations();
        let reason = self.core.snapshot_unavailable_reason(vm_id);
        let mut tree = self.core.snapshot_access(caller, vm_id, false, |store| {
            let vm = store
                .get_vm(vm_id)?
                .ok_or_else(|| PublicError::not_found("VM does not exist"))?;
            store.snapshot_tree(&vm)
        })?;
        if let Some(r) = reason {
            tree.available = false;
            tree.unavailable_reason = Some(r.to_owned());
        }
        Ok(tree)
    }

    fn update_snapshot(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        snapshot_id: SnapshotId,
        name: Label,
        description: String,
    ) -> Result<SnapshotSummary, PublicError> {
        lightvm_proto::ipc::validate_snapshot_description(&description)
            .map_err(|m| PublicError::invalid_argument(m).with_detail("field", "description"))?;
        self.core.snapshot_access(caller, vm_id, true, |store| {
            store.update_snapshot(vm_id, snapshot_id, &name, &description)
        })
    }

    fn snapshot_thumbnail(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        snapshot_id: SnapshotId,
    ) -> Result<Option<Vec<u8>>, PublicError> {
        self.core.snapshot_access(caller, vm_id, false, |store| {
            store.snapshot_thumbnail(vm_id, snapshot_id)
        })
    }

    fn offload_start_checks(&mut self, on: bool) {
        self.offload = on;
    }

    fn attach_gate(&mut self, gate: Arc<dyn SessionGate>) {
        self.gate = Some(gate);
    }

    fn take_start_jobs(&mut self) -> Vec<StartJob> {
        std::mem::take(&mut self.start_jobs)
    }

    fn finish_start(
        &mut self,
        done: StartDone,
        abort: Option<PublicError>,
    ) -> Result<OperationRecord, PublicError> {
        self.apply_observations();
        self.complete_start(done, abort)
    }

    fn create_session(&mut self, new: NewSession) -> Result<CreatedSession, PublicError> {
        self.apply_observations();
        let spec = vm_core::session::SessionSpec {
            vm_allowlist: [new.vm_id].into_iter().collect(),
            capabilities: new.capabilities,
            quotas: new.quotas,
            expires_at: new.expires_at,
            lease_millis: new.lease_ms,
        };
        let (grant, lease) = self.core.create_session(spec, new.token)?;
        let id = grant.session_id;
        let initial_baseline = self.core.store().baseline_facts(new.vm_id).current;
        self.session_meta.insert(
            id,
            SessionMeta {
                vm_id: new.vm_id,
                connection_id: new.connection_id,
                client_name: new.client_name,
                purpose: new.purpose,
                lease_ms: new.lease_ms,
                initial_baseline,
                created: BTreeSet::new(),
            },
        );
        if new.give_input
            && let Err(e) = self.core.set_input_owner(
                Actor::User,
                new.vm_id,
                InputOwner::Agent { session_id: id },
            )
        {
            // The grant has no `vm.input`: the session stays, without input.
            crate::diag!("lightvm-service: input not handed over: {}", e.message);
        }
        self.flush_input_releases();
        let info = self
            .session_info(id)
            .ok_or_else(|| PublicError::infrastructure("session not registered"))?;
        Ok(CreatedSession { info, grant, lease })
    }

    fn list_sessions(&mut self) -> Vec<SessionInfo> {
        let mut all: Vec<SessionInfo> = self
            .core
            .sessions()
            .all()
            .filter_map(|s| self.session_info(s.id))
            .collect();
        all.sort_by(|a, b| {
            let live = |s: &SessionInfo| s.state != SessionState::Terminated;
            live(b)
                .cmp(&live(a))
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        let live = all
            .iter()
            .filter(|s| s.state != SessionState::Terminated)
            .count();
        all.truncate(live + TERMINATED_SESSIONS_LISTED);
        all
    }

    fn pause_session(&mut self, id: SessionId) -> Result<SessionInfo, PublicError> {
        // A pause decided on the service gate is applied by the sync.
        self.apply_observations();
        let paused = self
            .core
            .sessions()
            .get(id)
            .is_some_and(|s| s.state == SessionState::Paused);
        if !paused {
            self.core.pause_session(id)?;
        }
        self.session_info(id)
            .ok_or_else(|| PublicError::not_found("session does not exist"))
    }

    fn resume_session(&mut self, id: SessionId) -> Result<SessionInfo, PublicError> {
        self.core.resume_session(id)?;
        self.session_info(id)
            .ok_or_else(|| PublicError::not_found("session does not exist"))
    }

    fn end_session(
        &mut self,
        id: SessionId,
        cause: TerminationCause,
        stop: bool,
    ) -> Result<RevocationReport, PublicError> {
        self.apply_observations();
        // Already terminated through the gate: the report of that moment.
        if let Some(report) = self.gate_reports.remove(&id) {
            return Ok(report);
        }
        let t = if stop {
            self.core.terminate_session_with_stop(id, cause)?
        } else {
            self.core.revoke_session(id, cause)?
        };
        let report = t.report.clone();
        self.shots.remove(&id);
        self.execute_stops(std::slice::from_ref(&t));
        self.flush_input_releases();
        Ok(report)
    }

    fn end_all_sessions(&mut self, cause: TerminationCause, stop: bool) {
        self.apply_observations();
        let t = self.core.terminate_all_sessions(cause, stop);
        self.execute_stops(&t);
        self.shots.clear();
        self.flush_input_releases();
    }

    fn tick_sessions(&mut self) {
        self.sync_gates();
        match self.core.tick() {
            Ok(t) => self.execute_stops(&t),
            Err(e) => crate::diag!("lightvm-service: session expiry check: {}", e.message),
        }
    }

    fn set_input_owner(&mut self, vm_id: VmId, owner: InputOwner) -> Result<(), PublicError> {
        self.apply_observations();
        let r = self.core.set_input_owner(Actor::User, vm_id, owner);
        self.flush_input_releases();
        r
    }

    fn agent_call(
        &mut self,
        caller: &Caller,
        call: AgentCall,
    ) -> Result<ResponseBody, PublicError> {
        let Caller::Agent { session_id, token } = caller else {
            return Err(PublicError::permission_denied(
                "agent calls need an agent session",
            ));
        };
        let session_id = *session_id;
        self.sync_gates();
        match call {
            AgentCall::ListVms => {
                self.apply_observations();
                let vms = self.core.list_vms(caller)?;
                Ok(ResponseBody::AgentVms {
                    vms: vms
                        .into_iter()
                        .map(|s| self.agent_view(s, session_id))
                        .collect(),
                })
            }
            AgentCall::VmStatus { vm_id } => {
                self.apply_observations();
                let st = self.core.vm_status(caller, vm_id)?;
                Ok(ResponseBody::AgentVm {
                    vm: self.agent_view(st, session_id),
                })
            }
            AgentCall::Submit { request } => {
                if request.action == VmAction::Screenshot {
                    return Err(PublicError::invalid_argument(
                        "screenshots use the screenshot call",
                    ));
                }
                let (admission, operation) = self.submit(caller, request)?;
                Ok(ResponseBody::Operation {
                    admission,
                    operation,
                })
            }
            AgentCall::Screenshot {
                operation_id,
                vm_id,
                expected_run_id,
            } => self.agent_screenshot(caller, session_id, operation_id, vm_id, expected_run_id),
            AgentCall::ScreenshotChunk {
                operation_id,
                index,
            } => {
                let vm_id = self
                    .shots
                    .get(&session_id)
                    .filter(|s| s.operation.id == operation_id)
                    .map(|s| s.meta.vm_id);
                // The core checks the session even when the shot is unknown
                // (no data about other sessions or VMs).
                let check_vm =
                    vm_id.or_else(|| self.session_meta.get(&session_id).map(|m| m.vm_id));
                let Some(check_vm) = check_vm else {
                    return Err(crate::agents::unauthenticated());
                };
                self.core.read_screenshot(caller, check_vm, operation_id)?;
                let shot = self
                    .shots
                    .get(&session_id)
                    .filter(|s| s.operation.id == operation_id)
                    .ok_or_else(|| PublicError::not_found("screenshot not available"))?;
                let chunks = chunk_count(shot.png.len());
                if index >= chunks.max(1) {
                    return Err(PublicError::invalid_argument("chunk index out of range"));
                }
                let size = SCREENSHOT_CHUNK_BYTES as usize;
                let start = index as usize * size;
                let end = (start + size).min(shot.png.len());
                Ok(ResponseBody::ScreenshotChunk {
                    operation_id,
                    index,
                    chunks,
                    data: lightvm_proto::ipc::base64_encode(&shot.png[start..end]),
                })
            }
            AgentCall::Operation { operation_id } => {
                let operation = self.core.operation(caller, operation_id)?;
                Ok(ResponseBody::OperationStatus { operation })
            }
            AgentCall::RenewLease => {
                let token = token.as_ref().ok_or_else(crate::agents::unauthenticated)?;
                let ms = self
                    .session_meta
                    .get(&session_id)
                    .map_or(lightvm_proto::LEASE_MAX_MILLIS, |m| m.lease_ms);
                let lease = self.core.renew_lease(session_id, token, ms)?;
                Ok(ResponseBody::Lease { lease })
            }
            AgentCall::EndSession => {
                if !self.session_token_ok(session_id, token.as_ref()) {
                    // Expired sessions are already terminated by the tick.
                    return Err(crate::agents::unauthenticated());
                }
                let report = self.end_session(session_id, TerminationCause::Completed, false)?;
                Ok(ResponseBody::SessionEnded { session_id, report })
            }
            AgentCall::ListBaselines { vm_id } => {
                self.apply_observations();
                let list = self
                    .core
                    .with_visible_vm(caller, vm_id, |store| store.list_baselines(vm_id))?;
                let meta = self.session_meta.get(&session_id);
                let baselines = list
                    .into_iter()
                    .map(|b| {
                        let initial = meta.and_then(|m| m.initial_baseline) == Some(b.baseline_id);
                        let created_by_session =
                            meta.is_some_and(|m| m.created.contains(&b.baseline_id));
                        lightvm_proto::report::AgentBaselineView {
                            baseline_id: b.baseline_id,
                            label: b.label.to_string(),
                            created_at: b.created_at,
                            clean_shutdown: b.clean_shutdown,
                            initial,
                            created_by_session,
                            current: b.current,
                            restorable: initial || created_by_session,
                        }
                    })
                    .collect();
                Ok(ResponseBody::AgentBaselines { vm_id, baselines })
            }
            // Served by the supervisor (storage jobs, reports).
            AgentCall::Snapshot { .. }
            | AgentCall::Restore { .. }
            | AgentCall::Logs { .. }
            | AgentCall::Declare { .. }
            | AgentCall::ToolNote { .. }
            | AgentCall::Report { .. } => {
                Err(PublicError::infrastructure("agent call not dispatched"))
            }
        }
    }

    fn report_context(&mut self, vm_id: VmId) -> Option<ReportContext> {
        let st = self.vm_status(&Caller::User, vm_id).ok()?;
        let baseline = st.baselines.current.and_then(|cur| {
            self.core
                .store()
                .list_baselines(vm_id)
                .ok()?
                .into_iter()
                .find(|b| b.baseline_id == cur)
        });
        Some(ReportContext {
            name: st.name.to_string(),
            config: st.config,
            config_revision: st.config_revision,
            baseline,
            run_id: st.run_id,
        })
    }

    fn session_screenshot(
        &self,
        session_id: SessionId,
        operation_id: OperationId,
    ) -> Option<(Screenshot, Vec<u8>)> {
        self.shots
            .get(&session_id)
            .filter(|s| s.operation.id == operation_id)
            .map(|s| (s.meta.clone(), s.png.clone()))
    }

    fn agent_check(
        &mut self,
        caller: &Caller,
        vm_id: Option<VmId>,
        capability: Option<Capability>,
    ) -> Result<SessionId, PublicError> {
        let Caller::Agent { session_id, token } = caller else {
            return Err(PublicError::permission_denied(
                "agent calls need an agent session",
            ));
        };
        self.sync_gates();
        if !self.session_token_ok(*session_id, token.as_ref()) {
            return Err(crate::agents::unauthenticated());
        }
        if let (Some(cap), Some(vm)) = (capability, vm_id)
            && !self.core.is_authorized(caller, vm, cap)
        {
            return Err(PublicError::permission_denied(
                "action not authorized by the effective policy",
            )
            .with_detail("capability", cap.as_str()));
        }
        Ok(*session_id)
    }

    fn qemu_log(&mut self, vm_id: VmId) -> Option<String> {
        let run = self.core.vm_status(&Caller::User, vm_id).ok()?.run_id?;
        self.backend.qemu_log(vm_id, run)
    }

    fn vmware_context(
        &mut self,
        caller: &Caller,
    ) -> Result<crate::vmware::VmwareContext, PublicError> {
        if matches!(caller, Caller::Agent { .. }) {
            let e = PublicError::permission_denied("VM import not available to agent sessions");
            self.core.record_import_denied(caller, &e);
            return Err(e);
        }
        let tool = self.backend.image_tool().ok_or_else(|| {
            PublicError::unsupported("VMware import needs qemu-img: configure the QEMU folder")
        })?;
        let catalog_root = self.core.store().catalog_root().ok_or_else(|| {
            PublicError::unsupported("VMware import not available on this catalog")
        })?;
        let l = self.core.creation_limits();
        Ok(crate::vmware::VmwareContext {
            tool,
            limits: vmware_import::HostLimits {
                max_vcpus: l.max_vcpus,
                max_memory_mib: l.max_memory_mib,
                max_disk_gib: l.max_disk_gib,
            },
            catalog_root,
        })
    }

    fn vmware_denied(&mut self, caller: &Caller, error: &PublicError) {
        self.core.record_import_denied(caller, error);
    }

    fn vmware_begin(
        &mut self,
        caller: &Caller,
        config: &VmConfig,
        required_bytes: u64,
        free_bytes: Option<u64>,
    ) -> Result<VmImportSlot, PublicError> {
        self.apply_observations();
        let reserve = self
            .core
            .config()
            .host
            .reserved_storage_gib
            .saturating_mul(1 << 30);
        let (_, slot) = self.core.begin_import(caller, config, |store, vm_id| {
            crate::vmware::check_space(required_bytes, free_bytes, reserve)?;
            store.begin_vm_import(vm_id)
        })?;
        Ok(slot)
    }

    fn vmware_finish(
        &mut self,
        caller: &Caller,
        slot: VmImportSlot,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        let vm_id = slot.vm_id;
        self.core
            .finish_import(caller, vm_id, config, |store, rec| {
                store.finish_vm_import(slot, rec)
            })
    }

    fn vmware_abort(
        &mut self,
        caller: &Caller,
        slot: VmImportSlot,
        reason: PublicError,
    ) -> PublicError {
        let vm_id = slot.vm_id;
        self.core
            .abort_import(caller, vm_id, |store| store.abort_vm_import(slot), reason)
    }

    fn session_changes(&mut self) -> Vec<(SessionId, SessionState, Option<TerminationCause>)> {
        let mut out = Vec::new();
        for s in self.core.sessions().all() {
            let now = (s.state, s.termination);
            if self.last_sessions.insert(s.id, now) != Some(now) {
                out.push((s.id, s.state, s.termination));
            }
        }
        for (id, state, _) in &out {
            if *state == SessionState::Terminated {
                self.shots.remove(id);
            }
        }
        out
    }

    fn input_owner_changes(&mut self) -> Vec<(VmId, InputOwner)> {
        let mut out = Vec::new();
        let vms: Vec<VmId> = self.last.keys().copied().collect();
        for vm in vms {
            let owner = self.core.input_owner(vm);
            let prev = self.last_owners.get(&vm).copied().unwrap_or_default();
            if owner != prev {
                out.push((vm, owner));
            }
            if owner == InputOwner::User {
                self.last_owners.remove(&vm);
            } else {
                self.last_owners.insert(vm, owner);
            }
        }
        out
    }

    fn record_event(
        &mut self,
        actor: Actor,
        action: AuditAction,
        outcome: AuditOutcome,
        session_id: Option<SessionId>,
        error: Option<ErrorCode>,
    ) {
        self.core
            .record_event(actor, action, outcome, None, session_id, error);
    }
}
