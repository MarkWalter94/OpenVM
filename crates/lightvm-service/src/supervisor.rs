//! Supervisor main loop: a dedicated thread owns the VM engine ([`VmEngine`],
//! with `vm_core::Core`) and the host ([`Backend`]) as sole owner (no shared
//! lock on VM state) and serves commands in order. The Emergency Stop does NOT
//! go through here: it uses [`EmergencyStop`], obtained from the host at
//! startup and callable from any task (ROADMAP 7.3, ADR-0009); only after the
//! termination does the supervisor record it in the core.
//!
//! Robustness of the loop:
//! - **panic containment**: each command (and each observation round) is
//!   served under `catch_unwind`; a panic is logged, the waiting request
//!   receives an `infrastructure_error`, and the thread keeps running with
//!   the engine and the host it owns (runs, processes and Job Objects are
//!   never dropped because of a bug in one request);
//! - **observation deadline**: process exits are observed at least every
//!   [`OBSERVE_INTERVAL`], whatever the request traffic;
//! - **revocation**: a request whose connection was revoked or closed (the
//!   revocation sender gone counts as revoked) is not executed, and a
//!   mutation queued before an Emergency Stop is discarded (media imports
//!   included, also when their hash ends after the revocation or the
//!   Emergency Stop: the outcome is then discarded and the import audited as
//!   failed);
//! - **baselines (F3)**: creation and restore run on a worker thread like
//!   the media import ([`SupervisorThread::start_storage_job`]); their work
//!   polls a cancellation check (Emergency Stop generation, revocation or
//!   closing of the connection, service shutdown) and stops `qemu-img`;
//!   the result is then discarded and audited as failed. The service
//!   shutdown waits (bounded) for the jobs still running, so nothing is
//!   left half published (the catalog journal covers a crash anyway);
//! - **start checks**: the hash of the approved media and of the baseline,
//!   re-verified at every start (ADR-0008, ADR-0025), runs on a worker
//!   thread ([`SupervisorThread::spawn_start_job`], [`crate::start`]) through
//!   handles opened here and kept until the run ends; the VM is `starting`
//!   with `VmSummary::start` showing the phase. The `submit_vm(start)` reply
//!   is held until QEMU is launched (or the start fails). Forced stop,
//!   Emergency Stop, revocation of the requesting connection and shutdown
//!   cancel the checks; nothing is launched and the VM ends `stopped`;
//! - **agent sessions (F4)**: a request of an agent session carries its
//!   session; right before executing, the service gate
//!   ([`crate::agents::AgentRegistry::check`]) is checked again (pause and
//!   revocation flip it outside this thread), then the core checks the
//!   session twice. Every [`OBSERVE_INTERVAL`] the grant and lease expiries
//!   are checked ([`VmEngine::tick_sessions`]); sessions terminated by the
//!   core for any cause close their gate and connection, and their changes
//!   (and the input owner changes) are published as events.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lightvm_proto::ipc::{
    Capabilities, ConsoleTicket, ResponseBody, SUPPORTED_PROTOCOL_VERSIONS, ServiceEvent,
    ServiceState, StopReason, VmwareImportPhase,
};
use lightvm_proto::{
    Actor, Admission, BaselineId, Caller, DisplayAdapter, Label, MediaId, MediaKind,
    OperationStatus, PublicError, ReplyError, RunId, SnapshotId, TerminationCause, VmConfig, VmId,
    VmName, VmRequest,
};
use tokio::sync::{OwnedSemaphorePermit, broadcast, oneshot, watch};

use lightvm_proto::ipc::{AgentCall, InputOwner};
use lightvm_proto::{AuditAction, AuditOutcome, ErrorCode, SessionId, SessionState};

use crate::agents::AgentRegistry;
use crate::display::DisplayRegistry;
use crate::engine::{
    BaselineOutcome, BaselineRequest, CancelCheck, ImportCopied, NewSession, StartDone, StartJob,
    StorageDone, VmEngine, VmImportSlot,
};
use crate::reports::{CallRecord, OpenReport, ReportStore};
use lightvm_proto::report::{ReportFormat, SessionVerifier};

/// Media imports hashing at the same time (each holds a worker thread).
pub const MAX_CONCURRENT_IMPORTS: usize = 2;

/// Longest wait of the service shutdown for baseline jobs still running
/// (they are cancelled first: `qemu-img` is terminated within 100 ms).
pub const STORAGE_SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Service host: processes, agent sessions and priority path.
///
/// Implementations: [`crate::adapters::backend::QemuHost`] (real) and
/// [`crate::fakes::FakeHost`] (tests).
pub trait Backend: Send + 'static {
    /// Backend name.
    fn name(&self) -> String;
    /// Detected accelerator, if any.
    fn accelerator(&self) -> Option<String>;
    /// Emergency Stop handler, callable outside the supervisor thread.
    fn emergency_handle(&self) -> Arc<dyn EmergencyStop>;
    /// Stops agent sessions: no new forwarding, the agent queue is discarded.
    fn stop_agent_sessions(&mut self, reason: StopReason);
    /// Hook called when the UI is lost, before the service applies
    /// [`ParentLossPolicy`] (ADR-0015 part B): with `shutdown_vms` the backend
    /// adopts the given ACPI wait for the shutdown that follows.
    fn on_parent_lost(&mut self, policy: ParentLossPolicy);
    /// Called at the start of the controlled service shutdown, before the
    /// engine stops the VMs: the backend adopts an orderly shutdown that fits
    /// within the service timeout (unless a policy has already chosen one).
    fn prepare_shutdown(&mut self) {}
    /// Final release of the host (called during shutdown, after the engine has
    /// stopped the VMs).
    fn shutdown(&mut self);
}

/// Priority stop path.
pub trait EmergencyStop: Send + Sync + 'static {
    /// Terminates the process tree with OS primitives without waiting for QMP.
    /// Must return only after observing the end of the processes or a timeout.
    fn emergency_stop_all(&self) -> EmergencyOutcome;
}

/// Observed outcome of the Emergency Stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmergencyOutcome {
    /// Processes whose end was observed.
    pub processes_terminated: u32,
    /// True only if the end of all processes was observed.
    pub completed: bool,
}

pub use lightvm_proto::ipc::ParentLossPolicy;

/// Requests served by the supervisor thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SupervisorRequest {
    Ping,
    GetCapabilities,
    ListVms,
    VmStatus(VmId),
    CreateVm(Box<VmConfig>),
    UpdateVm {
        vm_id: VmId,
        expected_revision: u64,
        config: Box<VmConfig>,
    },
    SubmitVm(Box<VmRequest>),
    ListMedia,
    ImportMedia {
        source_path: String,
        media_kind: MediaKind,
        declared_origin: String,
    },
    DeleteMedia(MediaId),
    DeleteVm {
        vm_id: VmId,
        expected_revision: u64,
    },
    OpenConsole(VmId),
    ListBaselines(VmId),
    CreateBaseline {
        vm_id: VmId,
        label: Label,
        allow_unclean: bool,
    },
    RestoreBaseline {
        vm_id: VmId,
        baseline_id: BaselineId,
    },
    DeleteBaseline {
        vm_id: VmId,
        baseline_id: BaselineId,
    },
    /// User snapshots (UI, ADR-0032).
    ListSnapshots(VmId),
    TakeSnapshot {
        vm_id: VmId,
        name: Label,
        description: String,
    },
    RevertSnapshot {
        vm_id: VmId,
        snapshot_id: SnapshotId,
    },
    DeleteSnapshot {
        vm_id: VmId,
        snapshot_id: SnapshotId,
    },
    UpdateSnapshot {
        vm_id: VmId,
        snapshot_id: SnapshotId,
        name: Label,
        description: String,
    },
    SnapshotThumbnail {
        vm_id: VmId,
        snapshot_id: SnapshotId,
    },
    PromoteSnapshot {
        vm_id: VmId,
        snapshot_id: SnapshotId,
        label: Label,
    },
    /// Session approved in the app (F4).
    CreateSession(Box<NewSession>),
    ListSessions,
    PauseSession(SessionId),
    ResumeSession(SessionId),
    EndSession {
        session_id: SessionId,
        cause: TerminationCause,
        stop: bool,
    },
    EndAllSessions {
        cause: TerminationCause,
        stop: bool,
    },
    SetInputOwner {
        vm_id: VmId,
        owner: InputOwner,
    },
    /// Call of an agent session (the caller carries the session).
    Agent(Box<AgentCall>),
    /// Session reports (UI, F5).
    Report(Box<ReportRequest>),
    /// Examination of a VMware VM (UI, ADR-0030).
    PreviewVmware {
        vmx_path: String,
    },
    /// Import of a VMware VM by conversion (UI, ADR-0030).
    ImportVmware {
        vmx_path: String,
        name: VmName,
        display: DisplayAdapter,
    },
    /// Cancellation of the VMware import in progress (UI).
    CancelVmwareImport,
}

/// VMware imports (ADR-0030): at most one import at a time, a few
/// previews.
#[derive(Debug, Default)]
pub(crate) struct VmwareState {
    /// Cancellation flag of the import in progress, if any.
    pub(crate) import: Option<Arc<AtomicBool>>,
    /// Previews examining a VM on a worker thread.
    pub(crate) previews: Arc<AtomicUsize>,
}

/// VMware previews running at the same time.
pub const MAX_CONCURRENT_VMWARE_PREVIEWS: usize = 2;

/// Requests of the UI on the session reports (F5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReportRequest {
    List,
    Header(SessionId),
    Events {
        session_id: SessionId,
        offset: u32,
        limit: u32,
    },
    ShotChunk {
        session_id: SessionId,
        shot: u32,
        index: u32,
    },
    Verdict {
        session_id: SessionId,
        outcome: lightvm_proto::TestOutcome,
        rationale: String,
    },
    Verify {
        session_id: SessionId,
        verifier: SessionVerifier,
    },
    Export {
        session_id: SessionId,
        format: ReportFormat,
        destination: String,
        overwrite: bool,
        include_screenshots: bool,
    },
    Delete(SessionId),
}

impl SupervisorRequest {
    /// Request that modifies VMs or the catalog: discarded if queued before an
    /// Emergency Stop.
    fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::CreateVm(_)
                | Self::UpdateVm { .. }
                | Self::SubmitVm(_)
                | Self::ImportMedia { .. }
                | Self::DeleteMedia(_)
                | Self::DeleteVm { .. }
                | Self::OpenConsole(_)
                | Self::CreateBaseline { .. }
                | Self::RestoreBaseline { .. }
                | Self::DeleteBaseline { .. }
                | Self::TakeSnapshot { .. }
                | Self::RevertSnapshot { .. }
                | Self::DeleteSnapshot { .. }
                | Self::UpdateSnapshot { .. }
                | Self::PromoteSnapshot { .. }
                | Self::CreateSession(_)
                | Self::SetInputOwner { .. }
                | Self::ImportVmware { .. }
        ) || matches!(self, Self::Agent(call) if call.is_mutation())
    }

    /// Baseline request served as a long storage operation (F3; F5 for the
    /// agent calls `snapshot` and `restore`).
    fn baseline_job(&self) -> Option<Result<BaselineRequest, PublicError>> {
        match self {
            Self::Agent(call) => match call.as_ref() {
                AgentCall::Snapshot { vm_id, label, .. } => Some(
                    Label::parse(format!(
                        "agent: {}",
                        crate::reports::plain(label.trim(), 200)
                    ))
                    .map(|label| BaselineRequest::Create {
                        vm_id: *vm_id,
                        label,
                        // No hidden decision: an unclean last stop refuses
                        // the checkpoint (`unclean_shutdown`).
                        allow_unclean: false,
                    })
                    .map_err(|_| {
                        PublicError::invalid_argument("label not valid")
                            .with_detail("field", "label")
                    }),
                ),
                AgentCall::Restore {
                    vm_id, baseline_id, ..
                } => Some(Ok(BaselineRequest::Restore {
                    vm_id: *vm_id,
                    baseline_id: *baseline_id,
                })),
                _ => None,
            },
            Self::CreateBaseline {
                vm_id,
                label,
                allow_unclean,
            } => Some(Ok(BaselineRequest::Create {
                vm_id: *vm_id,
                label: label.clone(),
                allow_unclean: *allow_unclean,
            })),
            Self::RestoreBaseline { vm_id, baseline_id } => Some(Ok(BaselineRequest::Restore {
                vm_id: *vm_id,
                baseline_id: *baseline_id,
            })),
            Self::TakeSnapshot {
                vm_id,
                name,
                description,
            } => Some(Ok(BaselineRequest::TakeSnapshot {
                vm_id: *vm_id,
                name: name.clone(),
                description: description.clone(),
            })),
            Self::RevertSnapshot { vm_id, snapshot_id } => {
                Some(Ok(BaselineRequest::RevertSnapshot {
                    vm_id: *vm_id,
                    snapshot_id: *snapshot_id,
                }))
            }
            Self::DeleteSnapshot { vm_id, snapshot_id } => {
                Some(Ok(BaselineRequest::DeleteSnapshot {
                    vm_id: *vm_id,
                    snapshot_id: *snapshot_id,
                }))
            }
            Self::PromoteSnapshot {
                vm_id,
                snapshot_id,
                label,
            } => Some(Ok(BaselineRequest::PromoteSnapshot {
                vm_id: *vm_id,
                snapshot_id: *snapshot_id,
                label: label.clone(),
            })),
            _ => None,
        }
    }
}

pub(crate) type Reply = oneshot::Sender<Result<ResponseBody, ReplyError>>;

/// Reply of a `submit_vm(start)` held while the start's file checks run.
pub(crate) struct StartHolder {
    reply: Reply,
    admission: Admission,
    /// Revocation of the requesting connection: cancels the checks.
    revoked: watch::Receiver<bool>,
}

pub(crate) enum Command {
    Request {
        req: SupervisorRequest,
        /// Caller towards the core; `None` for roles without VM permissions.
        caller: Option<Caller>,
        /// Connection that sent the request (owner of a console ticket).
        connection_id: u64,
        /// Connection revocation: checked again immediately before executing
        /// (double check, ROADMAP 7.3). A closed channel counts as revoked.
        revoked: watch::Receiver<bool>,
        /// Emergency Stop generation at the time of queueing.
        epoch: u64,
        /// Agent session of the request and whether it mutates a VM: the
        /// service gate is checked again right before executing (F4).
        agent: Option<(SessionId, bool)>,
        reply: Reply,
        /// Queue slot: released when the command is served.
        _permit: OwnedSemaphorePermit,
    },
    /// Session control of the UI (F4): served like a request of the user,
    /// without a queue slot (see `server::supervisor_call`).
    Control {
        req: SupervisorRequest,
        /// Emergency Stop generation at the time of sending.
        epoch: u64,
        reply: Reply,
    },
    /// The Emergency Stop has already terminated the processes: record it in the
    /// core with the actor who decided it.
    EmergencyReconcile(Actor),
    ParentLost {
        policy: ParentLossPolicy,
        done: oneshot::Sender<()>,
    },
    Shutdown {
        done: oneshot::Sender<()>,
    },
    /// Copy of a media import finished on its worker thread.
    FinishImport {
        caller: Caller,
        copied: ImportCopied,
        reply: Reply,
        /// Revocation of the connection that asked for the import.
        revoked: watch::Receiver<bool>,
        /// Emergency Stop generation when the import was queued.
        epoch: u64,
    },
    /// Work of a baseline job finished on its worker thread (F3).
    FinishStorage {
        done: StorageDone,
        reply: Reply,
        /// Revocation of the connection that asked for the operation.
        revoked: watch::Receiver<bool>,
        /// Emergency Stop generation when the operation was queued.
        epoch: u64,
    },
    /// File checks of a start finished on their worker thread: launch or
    /// failure, then the reply held for the request that started it.
    FinishStart {
        done: StartDone,
        /// Reply of the `submit_vm` that asked for the start, if held.
        holder: Option<StartHolder>,
        /// Emergency Stop generation when the checks began.
        epoch: u64,
    },
    /// Console tickets revoked outside the supervisor thread (UI connection
    /// revoked or closed, Emergency Stop): audit and, for a lost console,
    /// release of the keys and buttons still pressed on these runs.
    ConsolesRevoked {
        runs: Vec<(VmId, RunId)>,
        actor: Actor,
        cause: TerminationCause,
        release_input: bool,
    },
    /// Audit of a session lifecycle step decided by the service (F4).
    Record {
        actor: Actor,
        action: AuditAction,
        outcome: AuditOutcome,
        session_id: Option<SessionId>,
        error: Option<ErrorCode>,
    },
    /// Sessions to terminate without a reply (agent connection lost: cause
    /// `client_lost`, VM stopped, ROADMAP 7.3).
    EndSessions {
        ids: Vec<SessionId>,
        cause: TerminationCause,
        stop: bool,
    },
    /// An agent call and its outcome, for the session report (F5). Sent by
    /// the connection right before the reply, so it is recorded before any
    /// later request of the same agent.
    AgentCallDone(Box<CallRecord>),
    /// Examination of a VMware VM to import finished on its worker
    /// (ADR-0030): checks, storage intent, then the conversion.
    VmwareAnalyzed(Box<VmwareStep<Result<crate::vmware::Analysis, PublicError>>>),
    /// Conversion of a VMware import finished on its worker: publication
    /// or rollback.
    VmwareConverted(Box<VmwareStep<Converted>>),
}

/// Outcome of the conversion of a VMware import: the catalog slot, the
/// configuration to create and the result of `qemu-img`.
pub(crate) type Converted = (VmImportSlot, VmConfig, Result<(), PublicError>);

/// A step of a VMware import coming back from a worker.
pub(crate) struct VmwareStep<T> {
    caller: Caller,
    display: DisplayAdapter,
    outcome: T,
    reply: Reply,
    revoked: watch::Receiver<bool>,
    epoch: u64,
    cancel: CancelCheck,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request { req, .. } => write!(f, "Request({req:?})"),
            Self::Control { req, .. } => write!(f, "Control({req:?})"),
            Self::EmergencyReconcile(a) => write!(f, "EmergencyReconcile({a:?})"),
            Self::ParentLost { policy, .. } => write!(f, "ParentLost({policy:?})"),
            Self::Shutdown { .. } => write!(f, "Shutdown"),
            Self::FinishImport { .. } => write!(f, "FinishImport"),
            Self::FinishStorage { done, .. } => write!(f, "FinishStorage({done:?})"),
            Self::FinishStart { done, .. } => write!(f, "FinishStart({done:?})"),
            Self::ConsolesRevoked { runs, cause, .. } => {
                write!(f, "ConsolesRevoked({}, {cause:?})", runs.len())
            }
            Self::Record {
                action, outcome, ..
            } => write!(f, "Record({action:?}, {outcome:?})"),
            Self::EndSessions { ids, cause, .. } => {
                write!(f, "EndSessions({}, {cause:?})", ids.len())
            }
            Self::AgentCallDone(c) => write!(f, "AgentCallDone({})", c.call),
            Self::VmwareAnalyzed(_) => write!(f, "VmwareAnalyzed"),
            Self::VmwareConverted(_) => write!(f, "VmwareConverted"),
        }
    }
}

/// Error for a credential or connection that is no longer valid.
pub(crate) fn unauthenticated(message: &str) -> ReplyError {
    ReplyError::Operational(PublicError::unauthenticated(message))
}

/// `true` if the connection was revoked, or closed (its revocation sender
/// is gone: nobody can revoke it any more, so nothing queued by it runs).
pub(crate) fn is_revoked(rx: &watch::Receiver<bool>) -> bool {
    *rx.borrow() || rx.has_changed().is_err()
}

pub(crate) struct SupervisorThread {
    pub(crate) engine: Box<dyn VmEngine>,
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) events: broadcast::Sender<ServiceEvent>,
    pub(crate) max_frame: u32,
    pub(crate) emergency_epoch: Arc<AtomicU64>,
    pub(crate) parent_loss_policy: Arc<Mutex<ParentLossPolicy>>,
    /// VMs with a process, read by the shutdown driver for its budget.
    pub(crate) vms_with_process: Arc<AtomicUsize>,
    /// Console tickets and display proxies.
    pub(crate) display: Arc<DisplayRegistry>,
    /// Sender towards this same thread (end of a media import hash).
    pub(crate) self_tx: mpsc::Sender<Command>,
    /// Media imports whose hash is in progress.
    pub(crate) imports: usize,
    /// Baseline jobs whose work is in progress (F3).
    pub(crate) storage_jobs: usize,
    /// Raised at the service shutdown: cancels the baseline jobs and the
    /// start checks.
    pub(crate) storage_cancel: Arc<AtomicBool>,
    /// Start checks running on worker threads.
    pub(crate) start_jobs: usize,
    /// Agent sessions on the service side (gate, connections), F4.
    pub(crate) agents: Arc<AgentRegistry>,
    /// Host suspend/resume detection (F6, ROADMAP 7.3).
    pub(crate) resume: crate::resume::ResumeDetector,
    /// Session reports (F5).
    pub(crate) reports: ReportStore,
    /// VMware imports and previews (ADR-0030).
    pub(crate) vmware: VmwareState,
}

/// Maximum interval between two observations of the process exits (shutdown
/// from the guest, crash) and publications of their events, with or without
/// request traffic.
pub(crate) const OBSERVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Outcome of serving one command.
enum Served {
    Continue,
    Exit,
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> &str {
    p.downcast_ref::<&str>()
        .copied()
        .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-textual panic")
}

impl SupervisorThread {
    pub(crate) fn run(mut self, rx: mpsc::Receiver<Command>) {
        // The file checks of a start (hash of a multi-GiB ISO) run on worker
        // threads: this thread keeps serving the other requests.
        self.engine.offload_start_checks(true);
        // Pauses and revocations decided on the service's priority path
        // reach the core before its next admission or forwarding (F4).
        self.engine.attach_gate(self.agents.clone());
        let mut next_observe = Instant::now() + OBSERVE_INTERVAL;
        loop {
            let wait = next_observe.saturating_duration_since(Instant::now());
            self.resume.mark();
            let received = rx.recv_timeout(wait);
            // Before serving anything and before the expiry check: a host
            // resume ends the agent sessions first (ROADMAP 7.3).
            self.check_resume();
            match received {
                Ok(cmd) => {
                    let what = format!("{cmd:?}");
                    let exiting = matches!(cmd, Command::Shutdown { .. });
                    let mut slot: Option<Reply> = None;
                    let r = catch_unwind(AssertUnwindSafe(|| self.serve(cmd, &rx, &mut slot)));
                    match r {
                        Ok(Served::Continue) => {}
                        Ok(Served::Exit) => return,
                        Err(p) => {
                            crate::diag!(
                                "lightvm-service: supervisor: panic while serving {what}: {}; the supervisor keeps running",
                                panic_text(p.as_ref())
                            );
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(Err(ReplyError::Operational(
                                    PublicError::infrastructure(
                                        "internal error while serving the request",
                                    ),
                                )));
                            }
                            if exiting {
                                return;
                            }
                        }
                    }
                    // Start checks begun by the command and not yet spawned.
                    let r = catch_unwind(AssertUnwindSafe(|| {
                        for job in self.engine.take_start_jobs() {
                            self.spawn_start_job(job, None);
                        }
                    }));
                    if let Err(p) = r {
                        crate::diag!(
                            "lightvm-service: supervisor: panic while starting the start checks: {}",
                            panic_text(p.as_ref())
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            let observe = Instant::now() >= next_observe;
            if observe {
                next_observe = Instant::now() + OBSERVE_INTERVAL;
            }
            let r = catch_unwind(AssertUnwindSafe(|| {
                if observe {
                    self.engine.observe();
                    // Grant and lease expiries (ROADMAP 7.3): expired
                    // sessions end and their VMs are Emergency-Stopped.
                    self.engine.tick_sessions();
                }
                self.publish_state_changes();
                if observe {
                    self.reports.flush();
                }
            }));
            if let Err(p) = r {
                crate::diag!(
                    "lightvm-service: supervisor: panic while observing the VMs: {}",
                    panic_text(p.as_ref())
                );
            }
        }
    }

    /// F6 / ROADMAP 7.3: after a host suspend/resume (or a forward jump of
    /// the wall clock) the agent sessions do not continue: they end with
    /// `host_resumed`, their VMs are stopped on the emergency path and
    /// their gates and connections close before any queued request is
    /// served. Resuming the agent needs a new approval.
    fn check_resume(&mut self) {
        let Some(gap) = self.resume.check() else {
            return;
        };
        let r = catch_unwind(AssertUnwindSafe(|| {
            crate::diag!(
                "lightvm-service: host resume detected (supervisor idle for {:.1} s): agent sessions terminated, their VMs stopped",
                gap.as_secs_f64()
            );
            self.engine
                .end_all_sessions(TerminationCause::HostResumed, true);
            self.publish_state_changes();
            self.reports.flush();
        }));
        if let Err(p) = r {
            crate::diag!(
                "lightvm-service: supervisor: panic while handling a host resume: {}",
                panic_text(p.as_ref())
            );
        }
    }

    /// Serves one command. A reply sender waiting for the outcome is kept in
    /// `slot` while the command runs, so that after a panic the caller can
    /// still answer it.
    fn serve(
        &mut self,
        cmd: Command,
        rx: &mpsc::Receiver<Command>,
        slot: &mut Option<Reply>,
    ) -> Served {
        match cmd {
            Command::Request {
                req,
                caller,
                connection_id,
                revoked,
                epoch,
                agent,
                reply,
                _permit,
            } => {
                *slot = Some(reply);
                let gate = agent.map_or(Ok(()), |(sid, mutation)| {
                    self.agents.check(connection_id, sid, mutation)
                });
                let res = if is_revoked(&revoked) {
                    Err(unauthenticated("connection revoked"))
                } else if let Err(e) = gate {
                    // Second check of the service gate, right before executing.
                    Err(ReplyError::Operational(e))
                } else if req.is_mutation() && epoch != self.emergency_epoch.load(Ordering::SeqCst)
                {
                    Err(ReplyError::Operational(PublicError::state_conflict(
                        "request discarded by the Emergency Stop",
                    )))
                } else if let Some(job) = req.baseline_job() {
                    // The reply is sent when the work ends.
                    if let Some(reply) = slot.take() {
                        match job {
                            Ok(job) => {
                                self.start_storage_job(caller, job, (reply, revoked, epoch));
                            }
                            Err(e) => {
                                let _ = reply.send(Err(ReplyError::Operational(e)));
                            }
                        }
                    }
                    return Served::Continue;
                } else if let SupervisorRequest::ImportMedia {
                    source_path,
                    media_kind,
                    declared_origin,
                } = req
                {
                    // The reply is sent when the hash ends.
                    if let Some(reply) = slot.take() {
                        self.start_import(
                            caller,
                            &source_path,
                            media_kind,
                            &declared_origin,
                            (reply, revoked, epoch),
                        );
                    }
                    return Served::Continue;
                } else if let SupervisorRequest::PreviewVmware { vmx_path } = req {
                    // The reply is sent by the worker that examines the VM.
                    if let Some(reply) = slot.take() {
                        self.start_vmware_preview(caller, vmx_path, reply);
                    }
                    return Served::Continue;
                } else if let SupervisorRequest::ImportVmware {
                    vmx_path,
                    name,
                    display,
                } = req
                {
                    // The reply is sent when the VM exists (or the import
                    // ended otherwise).
                    if let Some(reply) = slot.take() {
                        self.start_vmware_import(
                            caller,
                            vmx_path,
                            name,
                            display,
                            (reply, revoked, epoch),
                        );
                    }
                    return Served::Continue;
                } else {
                    self.handle(req, caller.as_ref(), connection_id)
                };
                // A start whose file checks were left to a worker: the
                // reply waits for the launch (same answer as before, the
                // supervisor stays free meanwhile).
                let mut jobs = self.engine.take_start_jobs();
                if let Ok(ResponseBody::Operation {
                    admission,
                    operation: Some(op),
                }) = &res
                    && op.kind == lightvm_proto::OperationKind::Start
                    && op.status == OperationStatus::Forwarded
                    && let Some(i) = jobs.iter().position(|j| j.operation_id() == op.id)
                    && let Some(reply) = slot.take()
                {
                    let job = jobs.remove(i);
                    let holder = StartHolder {
                        reply,
                        admission: admission.clone(),
                        revoked,
                    };
                    self.spawn_start_job(job, Some(holder));
                }
                for job in jobs {
                    self.spawn_start_job(job, None);
                }
                if let Some(reply) = slot.take() {
                    let _ = reply.send(res);
                }
            }
            Command::FinishStart {
                done,
                holder,
                epoch,
            } => {
                self.start_jobs = self.start_jobs.saturating_sub(1);
                self.finish_start(done, holder, epoch, slot);
            }
            Command::Control { req, epoch, reply } => {
                *slot = Some(reply);
                let res =
                    if req.is_mutation() && epoch != self.emergency_epoch.load(Ordering::SeqCst) {
                        Err(ReplyError::Operational(PublicError::state_conflict(
                            "request discarded by the Emergency Stop",
                        )))
                    } else {
                        self.handle(req, Some(&Caller::User), 0)
                    };
                if let Some(reply) = slot.take() {
                    let _ = reply.send(res);
                }
            }
            Command::FinishImport {
                caller,
                copied,
                reply,
                revoked,
                epoch,
            } => {
                *slot = Some(reply);
                self.imports = self.imports.saturating_sub(1);
                let res = if is_revoked(&revoked) {
                    Err(self.engine.abort_import(
                        &caller,
                        copied,
                        PublicError::unauthenticated("connection revoked: import cancelled"),
                    ))
                } else if epoch != self.emergency_epoch.load(Ordering::SeqCst) {
                    Err(self.engine.abort_import(
                        &caller,
                        copied,
                        PublicError::state_conflict("import cancelled by the Emergency Stop"),
                    ))
                } else {
                    self.engine.finish_import(&caller, copied)
                };
                let res = res
                    .map(|media| ResponseBody::MediaImported { media })
                    .map_err(ReplyError::Operational);
                if let Some(reply) = slot.take() {
                    let _ = reply.send(res);
                }
            }
            Command::FinishStorage {
                done,
                reply,
                revoked,
                epoch,
            } => {
                *slot = Some(reply);
                self.storage_jobs = self.storage_jobs.saturating_sub(1);
                let vm_id = done.vm_id();
                let res = if is_revoked(&revoked) {
                    Err(self.engine.abort_baseline_job(
                        done,
                        PublicError::unauthenticated("connection revoked: operation cancelled"),
                    ))
                } else if epoch != self.emergency_epoch.load(Ordering::SeqCst) {
                    Err(self.engine.abort_baseline_job(
                        done,
                        PublicError::state_conflict("operation cancelled by the Emergency Stop"),
                    ))
                } else if self.storage_cancel.load(Ordering::SeqCst) {
                    Err(self.engine.abort_baseline_job(
                        done,
                        PublicError::state_conflict("service shutting down"),
                    ))
                } else {
                    self.engine.finish_baseline_job(done).map(|o| match o {
                        BaselineOutcome::Created(baseline) => {
                            ResponseBody::BaselineCreated { baseline }
                        }
                        BaselineOutcome::Restored { vm_id, baseline_id } => {
                            ResponseBody::BaselineRestored { vm_id, baseline_id }
                        }
                        BaselineOutcome::SnapshotTaken(snapshot) => {
                            ResponseBody::Snapshot { snapshot }
                        }
                        BaselineOutcome::SnapshotReverted {
                            vm_id,
                            snapshot_id,
                            record,
                        } => ResponseBody::SnapshotReverted {
                            vm_id,
                            snapshot_id,
                            config_revision: record.config_revision,
                        },
                        BaselineOutcome::SnapshotDeleted { vm_id, snapshot_id } => {
                            ResponseBody::SnapshotDeleted { vm_id, snapshot_id }
                        }
                    })
                };
                let _ = self.events.send(ServiceEvent::BaselinesChanged { vm_id });
                let _ = self.events.send(ServiceEvent::SnapshotsChanged { vm_id });
                if let Some(reply) = slot.take() {
                    let _ = reply.send(res.map_err(ReplyError::Operational));
                }
            }
            Command::ConsolesRevoked {
                runs,
                actor,
                cause,
                release_input,
            } => {
                self.engine.console_revoked(actor, &runs, cause);
                if release_input {
                    for (vm_id, run_id) in runs {
                        self.engine.release_input(vm_id, run_id);
                    }
                }
            }
            Command::EmergencyReconcile(actor) => self.engine.after_emergency_stop(actor),
            Command::Record {
                actor,
                action,
                outcome,
                session_id,
                error,
            } => self
                .engine
                .record_event(actor, action, outcome, session_id, error),
            Command::EndSessions { ids, cause, stop } => {
                for id in ids {
                    if let Err(e) = self.engine.end_session(id, cause, stop) {
                        crate::diag!("lightvm-service: session not ended: {}", e.message);
                    }
                }
            }
            Command::AgentCallDone(rec) => self.record_call(&rec),
            Command::VmwareAnalyzed(step) => {
                self.storage_jobs = self.storage_jobs.saturating_sub(1);
                self.vmware_analyzed(*step);
            }
            Command::VmwareConverted(step) => {
                self.storage_jobs = self.storage_jobs.saturating_sub(1);
                self.vmware_converted(*step);
            }
            Command::ParentLost { policy, done } => {
                // ROADMAP 7.3: UI lost → the agent sessions end and their VMs
                // are stopped through the emergency path.
                self.engine
                    .end_all_sessions(TerminationCause::UiClosed, true);
                self.publish_state_changes();
                self.backend.stop_agent_sessions(StopReason::ParentLost);
                self.backend.on_parent_lost(policy);
                let _ = self.events.send(ServiceEvent::AgentSessionsStopped {
                    reason: StopReason::ParentLost,
                });
                let _ = done.send(());
            }
            Command::Shutdown { done } => {
                self.shutdown(rx);
                let _ = done.send(());
                return Served::Exit;
            }
        }
        Served::Continue
    }

    fn shutdown(&mut self, rx: &mpsc::Receiver<Command>) {
        let _ = self.events.send(ServiceEvent::ServiceState {
            state: ServiceState::Stopping,
        });
        // Baseline jobs still running are cancelled (`qemu-img` stopped).
        self.storage_cancel.store(true, Ordering::SeqCst);
        // Requests still in the queue are not executed.
        while let Ok(pending) = rx.try_recv() {
            self.refuse_at_shutdown(pending);
        }
        // Bounded wait for the cancelled baseline jobs: their result is
        // discarded (the journal would roll it back anyway).
        let deadline = Instant::now() + STORAGE_SHUTDOWN_WAIT;
        while self.storage_jobs > 0 || self.start_jobs > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(cmd) => self.refuse_at_shutdown(cmd),
                Err(_) => break,
            }
        }
        let runs = self.display.revoke_all();
        self.engine
            .console_revoked(Actor::System, &runs, TerminationCause::Completed);
        // No session survives the service (they live in memory only).
        self.engine
            .end_all_sessions(TerminationCause::UiClosed, false);
        self.backend.stop_agent_sessions(StopReason::Shutdown);
        self.backend.prepare_shutdown();
        self.engine.stop_all();
        self.publish_state_changes();
        self.reports.flush();
        self.backend.shutdown();
    }

    /// A command received during the service shutdown: nothing is executed.
    fn refuse_at_shutdown(&mut self, pending: Command) {
        {
            match pending {
                Command::Request { reply, .. } | Command::Control { reply, .. } => {
                    let _ = reply.send(Err(ReplyError::Operational(PublicError::state_conflict(
                        "service shutting down",
                    ))));
                }
                Command::EmergencyReconcile(actor) => {
                    self.engine.after_emergency_stop(actor);
                }
                Command::ParentLost { done, .. } | Command::Shutdown { done } => {
                    let _ = done.send(());
                }
                Command::FinishImport {
                    caller,
                    copied,
                    reply,
                    ..
                } => {
                    // Nothing is recorded.
                    let e = self.engine.abort_import(
                        &caller,
                        copied,
                        PublicError::state_conflict("service shutting down"),
                    );
                    let _ = reply.send(Err(ReplyError::Operational(e)));
                }
                Command::FinishStorage { done, reply, .. } => {
                    self.storage_jobs = self.storage_jobs.saturating_sub(1);
                    let e = self.engine.abort_baseline_job(
                        done,
                        PublicError::state_conflict("service shutting down"),
                    );
                    let _ = reply.send(Err(ReplyError::Operational(e)));
                }
                Command::FinishStart {
                    done,
                    holder,
                    epoch,
                } => {
                    // Nothing is launched: the shutdown cancels the start
                    // (VM back to `stopped`, handles closed).
                    self.start_jobs = self.start_jobs.saturating_sub(1);
                    let mut slot = None;
                    self.finish_start(done, holder, epoch, &mut slot);
                }
                Command::ConsolesRevoked {
                    runs, actor, cause, ..
                } => self.engine.console_revoked(actor, &runs, cause),
                Command::Record {
                    actor,
                    action,
                    outcome,
                    session_id,
                    error,
                } => self
                    .engine
                    .record_event(actor, action, outcome, session_id, error),
                Command::EndSessions { ids, cause, stop } => {
                    for id in ids {
                        let _ = self.engine.end_session(id, cause, stop);
                    }
                }
                Command::AgentCallDone(rec) => self.record_call(&rec),
                Command::VmwareAnalyzed(step) => {
                    // Nothing was recorded yet: the import just ends.
                    self.storage_jobs = self.storage_jobs.saturating_sub(1);
                    self.vmware.import = None;
                    let e = PublicError::state_conflict("service shutting down");
                    self.engine.vmware_denied(&step.caller, &e);
                    let _ = step.reply.send(Err(ReplyError::Operational(e)));
                }
                Command::VmwareConverted(step) => {
                    self.storage_jobs = self.storage_jobs.saturating_sub(1);
                    self.vmware.import = None;
                    let (slot, _, _) = step.outcome;
                    let e = self.engine.vmware_abort(
                        &step.caller,
                        slot,
                        PublicError::state_conflict("service shutting down"),
                    );
                    let _ = step.reply.send(Err(ReplyError::Operational(e)));
                }
            }
        }
    }

    /// Reason to stop a VMware import step: cancellation by the user,
    /// revocation or closing of the requesting connection, Emergency Stop,
    /// shutdown.
    fn vmware_stop_reason(
        &self,
        revoked: &watch::Receiver<bool>,
        epoch: u64,
        cancel: &CancelCheck,
    ) -> Option<PublicError> {
        if is_revoked(revoked) {
            Some(PublicError::unauthenticated(
                "connection revoked: import cancelled",
            ))
        } else if epoch != self.emergency_epoch.load(Ordering::SeqCst) {
            Some(PublicError::state_conflict(
                "import cancelled by the Emergency Stop",
            ))
        } else if self.storage_cancel.load(Ordering::SeqCst) {
            Some(PublicError::state_conflict("service shutting down"))
        } else if cancel() {
            Some(PublicError::state_conflict("VMware import cancelled"))
        } else {
            None
        }
    }

    /// Examination of a VMware VM for the UI (ADR-0030), on a worker
    /// thread: nothing is written, the reply carries the preview.
    fn start_vmware_preview(&mut self, caller: Option<Caller>, vmx: String, reply: Reply) {
        let Some(caller) = caller else {
            let _ = reply.send(Err(ReplyError::Operational(
                PublicError::permission_denied("no VM permissions for this role"),
            )));
            return;
        };
        let previews = self.vmware.previews.clone();
        if previews.load(Ordering::SeqCst) >= MAX_CONCURRENT_VMWARE_PREVIEWS {
            let _ = reply.send(Err(ReplyError::Operational(PublicError::resource_limit(
                "too many VMware examinations in progress",
            ))));
            return;
        }
        let ctx = match self.engine.vmware_context(&caller) {
            Ok(c) => c,
            Err(e) => {
                let _ = reply.send(Err(ReplyError::Operational(e)));
                return;
            }
        };
        let shutdown = self.storage_cancel.clone();
        previews.fetch_add(1, Ordering::SeqCst);
        let counter = previews.clone();
        let spawned = std::thread::Builder::new()
            .name("lightvm-vmware-preview".into())
            .spawn(move || {
                let cancel = move || shutdown.load(Ordering::SeqCst);
                let r = crate::vmware::analyze(&ctx, &vmx, None, &cancel)
                    .map(|a| ResponseBody::VmwareImportPreview {
                        preview: Box::new(a.preview()),
                    })
                    .map_err(ReplyError::Operational);
                counter.fetch_sub(1, Ordering::SeqCst);
                let _ = reply.send(r);
            });
        if let Err(e) = spawned {
            previews.fetch_sub(1, Ordering::SeqCst);
            crate::diag!("lightvm-service: VMware preview thread not started: {e}");
        }
    }

    /// First step of a VMware import (ADR-0030): the examination runs on a
    /// worker thread and comes back as [`Command::VmwareAnalyzed`].
    fn start_vmware_import(
        &mut self,
        caller: Option<Caller>,
        vmx: String,
        name: VmName,
        display: DisplayAdapter,
        (reply, revoked, epoch): (Reply, watch::Receiver<bool>, u64),
    ) {
        let Some(caller) = caller else {
            let _ = reply.send(Err(ReplyError::Operational(
                PublicError::permission_denied("no VM permissions for this role"),
            )));
            return;
        };
        if self.vmware.import.is_some() {
            let _ = reply.send(Err(ReplyError::Operational(PublicError::vm_busy(
                "another VMware import is in progress",
            ))));
            return;
        }
        let ctx = match self.engine.vmware_context(&caller) {
            Ok(c) => c,
            Err(e) => {
                let _ = reply.send(Err(ReplyError::Operational(e)));
                return;
            }
        };
        let flag = Arc::new(AtomicBool::new(false));
        self.vmware.import = Some(flag.clone());
        let cancel: CancelCheck = {
            let current = self.emergency_epoch.clone();
            let shutdown = self.storage_cancel.clone();
            let rv = revoked.clone();
            Arc::new(move || {
                flag.load(Ordering::SeqCst)
                    || shutdown.load(Ordering::SeqCst)
                    || current.load(Ordering::SeqCst) != epoch
                    || is_revoked(&rv)
            })
        };
        let _ = self.events.send(ServiceEvent::VmwareImportProgress {
            phase: VmwareImportPhase::Checking,
            percent: 0,
        });
        let tx = self.self_tx.clone();
        let c2 = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("lightvm-vmware-check".into())
            .spawn(move || {
                let outcome = crate::vmware::analyze(&ctx, &vmx, Some(name), &|| c2());
                let _ = tx.send(Command::VmwareAnalyzed(Box::new(VmwareStep {
                    caller,
                    display,
                    outcome,
                    reply,
                    revoked,
                    epoch,
                    cancel,
                })));
            });
        match spawned {
            Ok(_) => self.storage_jobs += 1,
            Err(e) => {
                self.vmware.import = None;
                crate::diag!("lightvm-service: VMware import thread not started: {e}");
            }
        }
    }

    /// Examination done: checks of the configuration and the budget, the
    /// journal intent, then the conversion on a worker thread.
    fn vmware_analyzed(&mut self, step: VmwareStep<Result<crate::vmware::Analysis, PublicError>>) {
        let VmwareStep {
            caller,
            display,
            outcome,
            reply,
            revoked,
            epoch,
            cancel,
        } = step;
        let fail = |this: &mut Self, e: PublicError, reply: Reply| {
            this.vmware.import = None;
            this.engine.vmware_denied(&caller, &e);
            let _ = reply.send(Err(ReplyError::Operational(e)));
        };
        let stop = self.vmware_stop_reason(&revoked, epoch, &cancel);
        let analysis = match (stop, outcome) {
            (Some(e), _) | (None, Err(e)) => return fail(self, e, reply),
            (None, Ok(a)) => a,
        };
        let config = match analysis.config(display) {
            Ok(c) => c,
            Err(e) => return fail(self, e, reply),
        };
        // Core checks (audited when refused) and the journal intent.
        let slot = match self.engine.vmware_begin(
            &caller,
            &config,
            analysis.required_bytes,
            analysis.free_bytes,
        ) {
            Ok(s) => s,
            Err(e) => {
                self.vmware.import = None;
                let _ = reply.send(Err(ReplyError::Operational(e)));
                return;
            }
        };
        let _ = self.events.send(ServiceEvent::VmwareImportProgress {
            phase: VmwareImportPhase::Converting,
            percent: 0,
        });
        let tx = self.self_tx.clone();
        let events = self.events.clone();
        let dst = slot.temp_path.clone();
        let c2 = cancel.clone();
        let who = caller.clone();
        // The worker owns the slot and the reply; if it cannot start they
        // come back here and the import is rolled back.
        let (job_tx, job_rx) = mpsc::channel::<(VmImportSlot, Reply)>();
        let spawned = std::thread::Builder::new()
            .name("lightvm-vmware-convert".into())
            .spawn(move || {
                let Ok((slot, reply)) = job_rx.recv() else {
                    return;
                };
                let mut progress = |p: u8| {
                    let _ = events.send(ServiceEvent::VmwareImportProgress {
                        phase: VmwareImportPhase::Converting,
                        percent: p.min(100),
                    });
                };
                let r = crate::vmware::convert(analysis, &dst, &|| c2(), &mut progress);
                let _ = tx.send(Command::VmwareConverted(Box::new(VmwareStep {
                    caller: who,
                    display,
                    outcome: (slot, config, r),
                    reply,
                    revoked,
                    epoch,
                    cancel,
                })));
            });
        let lost = match spawned {
            Ok(_) => match job_tx.send((slot, reply)) {
                Ok(()) => {
                    self.storage_jobs += 1;
                    return;
                }
                Err(mpsc::SendError(back)) => back,
            },
            Err(e) => {
                crate::diag!("lightvm-service: VMware conversion thread not started: {e}");
                (slot, reply)
            }
        };
        self.vmware.import = None;
        let (slot, reply) = lost;
        let e = self.engine.vmware_abort(
            &caller,
            slot,
            PublicError::infrastructure("worker thread not available"),
        );
        let _ = reply.send(Err(ReplyError::Operational(e)));
    }

    /// Conversion done: publication of the disk with the VM row, or
    /// rollback.
    fn vmware_converted(&mut self, step: VmwareStep<Converted>) {
        self.vmware.import = None;
        let stop = self.vmware_stop_reason(&step.revoked, step.epoch, &step.cancel);
        let VmwareStep {
            caller,
            outcome: (slot, config, r),
            reply,
            ..
        } = step;
        let res = match (stop, r) {
            (Some(e), _) | (None, Err(e)) => Err(self.engine.vmware_abort(&caller, slot, e)),
            (None, Ok(())) => {
                let _ = self.events.send(ServiceEvent::VmwareImportProgress {
                    phase: VmwareImportPhase::Finishing,
                    percent: 100,
                });
                self.engine.vmware_finish(&caller, slot, config)
            }
        };
        self.publish_state_changes();
        let _ = reply.send(
            res.map(|vm| ResponseBody::VmwareImported { vm })
                .map_err(ReplyError::Operational),
        );
    }

    /// Records an agent call in its session report; a successful screenshot
    /// is copied (PNG) from the engine into the session artefacts.
    fn record_call(&mut self, rec: &CallRecord) {
        if !self.reports.is_live(rec.session_id) {
            return;
        }
        self.reports.call(rec);
        if rec.screenshot
            && let Some(op) = rec.operation_id
            && let Some((meta, png)) = self.engine.session_screenshot(rec.session_id, op)
        {
            self.reports.screenshot(rec.session_id, &meta, &png);
        }
    }

    /// Opens the report of a session just approved.
    fn open_report(&mut self, c: &crate::engine::CreatedSession) {
        let info = &c.info;
        let Some(ctx) = self.engine.report_context(info.vm_id) else {
            crate::diag!("lightvm-service: reports: VM facts not available");
            return;
        };
        let manifest = {
            use sha2::Digest as _;
            let bytes = serde_json::to_vec(&ctx.config).unwrap_or_default();
            lightvm_proto::Sha256Digest::from_bytes(sha2::Sha256::digest(&bytes).into())
        };
        self.reports.open(OpenReport {
            session_id: info.session_id,
            vm_id: info.vm_id,
            vm_name: ctx.name,
            client_name: info.client_name.clone(),
            objective: info.purpose.clone(),
            started_at: info.created_at,
            capabilities: c.grant.capabilities.clone(),
            quotas: c.grant.quotas,
            expires_at: c.grant.expires_at,
            policy_revision: c.grant.policy_revision,
            guest_os: ctx.config.guest_os,
            vm_manifest_sha256: manifest,
            vm_config_revision: ctx.config_revision,
            baseline: ctx.baseline.map(|b| lightvm_proto::report::ReportBaseline {
                baseline_id: b.baseline_id,
                label: b.label.to_string(),
                created_at: b.created_at,
                clean_shutdown: b.clean_shutdown,
                disk_sha256: b.disk_sha256,
                firmware_sha256: b.firmware_sha256,
            }),
            run_id: ctx.run_id,
            versions: lightvm_proto::report::ReportVersions {
                service: env!("CARGO_PKG_VERSION").to_owned(),
                ipc_protocol: lightvm_proto::PROTOCOL_VERSION,
                mcp_protocol: None,
                backend: Some(self.backend.name()),
                accelerator: self.backend.accelerator(),
            },
        });
        let _ = self.events.send(ServiceEvent::SessionReportChanged {
            session_id: info.session_id,
        });
    }

    /// Agent calls served from the reports and the core's checks, outside
    /// the VM queue (F5): logs, declaration, tool note, report summary.
    fn report_agent_call(
        &mut self,
        caller: &Caller,
        call: &AgentCall,
    ) -> Option<Result<ResponseBody, PublicError>> {
        use lightvm_proto::Capability;
        let r = match call {
            AgentCall::Logs {
                vm_id,
                after_seq,
                limit,
            } => self
                .engine
                .agent_check(caller, Some(*vm_id), Some(Capability::VmLogs))
                .map(|sid| {
                    let (entries, last_seq) = self.reports.log_entries(sid, *after_seq, *limit);
                    let now = lightvm_proto::UtcTimestamp::now();
                    let qemu = self
                        .engine
                        .qemu_log(*vm_id)
                        .map(|raw| crate::reports::sanitize_qemu_lines(&raw))
                        .unwrap_or_default()
                        .into_iter()
                        .enumerate()
                        .map(|(i, text)| lightvm_proto::report::LogEntry {
                            seq: i as u64 + 1,
                            at: now,
                            source: lightvm_proto::report::LogSource::Qemu,
                            text,
                        })
                        .collect();
                    ResponseBody::AgentLogs {
                        vm_id: *vm_id,
                        entries,
                        last_seq,
                        qemu,
                    }
                }),
            AgentCall::Declare {
                claimed_outcome,
                summary,
            } => self
                .engine
                .agent_check(caller, None, None)
                .map(|sid| ResponseBody::Recorded {
                    seq: self.reports.declare(sid, *claimed_outcome, summary),
                }),
            AgentCall::ToolNote {
                tool,
                ok,
                error_code,
                detail,
            } => self
                .engine
                .agent_check(caller, None, None)
                .map(|sid| ResponseBody::Recorded {
                    seq: self.reports.tool_note(
                        sid,
                        tool,
                        *ok,
                        error_code.as_deref(),
                        detail.clone(),
                    ),
                }),
            AgentCall::Report { session_id } => {
                self.engine.agent_check(caller, None, None).and_then(|sid| {
                    if session_id.is_some_and(|s| s != sid) {
                        return Err(PublicError::not_found("session report not found"));
                    }
                    self.reports
                        .summary(sid)
                        .map(|report| ResponseBody::AgentReport { report })
                        .ok_or_else(|| PublicError::not_found("session report not found"))
                })
            }
            _ => return None,
        };
        Some(r)
    }

    /// Requests of the UI on the reports (the caller is the user).
    fn report_request(&mut self, req: ReportRequest) -> Result<ResponseBody, PublicError> {
        let audit = |engine: &mut Box<dyn VmEngine>,
                     action: AuditAction,
                     sid: SessionId,
                     r: &Result<ResponseBody, PublicError>| {
            let (outcome, error) = match r {
                Ok(_) => (AuditOutcome::Succeeded, None),
                Err(e) => (AuditOutcome::Failed, Some(e.code)),
            };
            engine.record_event(Actor::User, action, outcome, Some(sid), error);
        };
        match req {
            ReportRequest::List => Ok(ResponseBody::SessionReports {
                reports: self.reports.list(),
            }),
            ReportRequest::Header(id) => {
                self.reports
                    .header(id)
                    .map(|r| ResponseBody::SessionReport {
                        report: Box::new(r),
                    })
            }
            ReportRequest::Events {
                session_id,
                offset,
                limit,
            } => self
                .reports
                .events(session_id, offset, limit)
                .map(|(total, events)| ResponseBody::SessionReportEvents {
                    session_id,
                    total,
                    offset,
                    events,
                }),
            ReportRequest::ShotChunk {
                session_id,
                shot,
                index,
            } => self
                .reports
                .screenshot_png(session_id, shot)
                .and_then(|(png, sha256)| {
                    let size = lightvm_proto::ipc::SCREENSHOT_CHUNK_BYTES as usize;
                    let chunks = u32::try_from(png.len().div_ceil(size)).unwrap_or(u32::MAX);
                    if index >= chunks.max(1) {
                        return Err(PublicError::invalid_argument("chunk index out of range"));
                    }
                    let start = index as usize * size;
                    let end = (start + size).min(png.len());
                    Ok(ResponseBody::ReportScreenshotChunk {
                        session_id,
                        shot,
                        index,
                        chunks,
                        sha256,
                        data: lightvm_proto::ipc::base64_encode(&png[start..end]),
                    })
                }),
            ReportRequest::Verdict {
                session_id,
                outcome,
                rationale,
            } => {
                if rationale.chars().count() > lightvm_proto::report::MAX_RATIONALE_CHARS {
                    return Err(PublicError::invalid_argument("rationale too long"));
                }
                let r = self
                    .reports
                    .set_verdict(
                        session_id,
                        lightvm_proto::report::Verdict {
                            outcome,
                            source: lightvm_proto::report::OutcomeSource::User,
                            verifier: None,
                            decided_by: "local user".into(),
                            rationale,
                            at: lightvm_proto::UtcTimestamp::now(),
                        },
                    )
                    .map(|r| ResponseBody::SessionReport {
                        report: Box::new(r),
                    });
                audit(
                    &mut self.engine,
                    AuditAction::SessionVerdict,
                    session_id,
                    &r,
                );
                if r.is_ok() {
                    let _ = self
                        .events
                        .send(ServiceEvent::SessionReportChanged { session_id });
                }
                r
            }
            ReportRequest::Verify {
                session_id,
                verifier,
            } => {
                let r = self.reports.verify(session_id, verifier).map(|r| {
                    ResponseBody::SessionReport {
                        report: Box::new(r),
                    }
                });
                audit(
                    &mut self.engine,
                    AuditAction::SessionVerdict,
                    session_id,
                    &r,
                );
                if r.is_ok() {
                    let _ = self
                        .events
                        .send(ServiceEvent::SessionReportChanged { session_id });
                }
                r
            }
            ReportRequest::Export {
                session_id,
                format,
                destination,
                overwrite,
                include_screenshots,
            } => {
                let r = self
                    .reports
                    .export(
                        session_id,
                        format,
                        &destination,
                        overwrite,
                        include_screenshots,
                    )
                    .map(
                        |(bytes, sha256, screenshots)| ResponseBody::SessionReportExported {
                            session_id,
                            format,
                            bytes,
                            sha256,
                            screenshots,
                        },
                    );
                audit(&mut self.engine, AuditAction::ReportExport, session_id, &r);
                r
            }
            ReportRequest::Delete(session_id) => {
                let r = self
                    .reports
                    .delete(session_id)
                    .map(|()| ResponseBody::SessionReportDeleted { session_id });
                audit(&mut self.engine, AuditAction::ReportDelete, session_id, &r);
                if r.is_ok() {
                    let _ = self
                        .events
                        .send(ServiceEvent::SessionReportChanged { session_id });
                }
                r
            }
        }
    }

    pub(crate) fn publish_state_changes(&mut self) {
        let changes = self.engine.state_changes();
        // The display ticket lives only as long as its run: every ticket of
        // a run other than the VM's current one (run ended, or replaced by
        // another run within the same command) is revoked, as are the
        // tickets of VMs that no longer exist. During a graceful stop
        // (`stopping`) the run is still current and the console stays open.
        let runs = self.engine.current_runs();
        let mut revoked = Vec::new();
        for (vm_id, run) in &runs {
            revoked.extend(self.display.revoke_other_runs(*vm_id, *run));
        }
        for (vm_id, _) in self.display.active() {
            if !runs.iter().any(|(v, _)| *v == vm_id) {
                revoked.extend(self.display.revoke_vm(vm_id));
            }
        }
        if !revoked.is_empty() {
            self.engine
                .console_revoked(Actor::System, &revoked, TerminationCause::VmTerminated);
        }
        for (vm_id, state) in changes {
            let run = runs.iter().find(|(v, _)| *v == vm_id).and_then(|(_, r)| *r);
            self.reports.vm_state(vm_id, state, run);
            let _ = self.events.send(ServiceEvent::VmState { vm_id, state });
        }
        // F4: sessions terminated by the core for any cause close their gate
        // and their connection; changes and input owners are published.
        for (session_id, state, termination) in self.engine.session_changes() {
            if state == SessionState::Terminated {
                self.agents.terminated(session_id);
            }
            if self.reports.session_changed(session_id, state, termination) {
                let _ = self
                    .events
                    .send(ServiceEvent::SessionReportChanged { session_id });
            }
            let _ = self.events.send(ServiceEvent::SessionChanged {
                session_id,
                state,
                termination,
            });
        }
        for (vm_id, owner) in self.engine.input_owner_changes() {
            let _ = self.events.send(ServiceEvent::InputOwner { vm_id, owner });
        }
        self.vms_with_process
            .store(self.engine.vms_with_process(), Ordering::SeqCst);
    }

    /// First step of a media import on this thread (text checks and
    /// reservation, no access to the file), hash on a worker thread (open,
    /// file checks, SHA-256; nothing is copied, ADR-0029), last step back
    /// here with [`Command::FinishImport`]: the
    /// supervisor keeps serving the VMs (console input included) while a
    /// multi-GiB ISO is hashed, and a slow source never blocks it.
    fn start_import(
        &mut self,
        caller: Option<Caller>,
        source: &str,
        kind: MediaKind,
        origin: &str,
        (reply, revoked, epoch): (Reply, watch::Receiver<bool>, u64),
    ) {
        let Some(caller) = caller else {
            let _ = reply.send(Err(ReplyError::Operational(
                PublicError::permission_denied("no VM permissions for this role"),
            )));
            return;
        };
        if self.imports >= MAX_CONCURRENT_IMPORTS {
            let _ = reply.send(Err(ReplyError::Operational(PublicError::resource_limit(
                "too many media imports in progress",
            ))));
            return;
        }
        let job = match self.engine.begin_import(&caller, source, kind, origin) {
            Ok(job) => job,
            Err(e) => {
                let _ = reply.send(Err(ReplyError::Operational(e)));
                return;
            }
        };
        let tx = self.self_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("lightvm-media-import".into())
            .spawn(move || {
                let copied = job.run();
                // If the supervisor is gone nothing is recorded: an import
                // leaves nothing to roll back.
                let _ = tx.send(Command::FinishImport {
                    caller,
                    copied,
                    reply,
                    revoked,
                    epoch,
                });
            });
        match spawned {
            Ok(_) => self.imports += 1,
            Err(e) => crate::diag!("lightvm-service: media import thread not started: {e}"),
        }
    }

    /// First step of a baseline creation or restore on this thread (core
    /// and catalog checks, journal intents, guards), work on a worker thread
    /// (`qemu-img`, hashes; cancellable), last step back here with
    /// [`Command::FinishStorage`]: the supervisor keeps serving the other VMs.
    fn start_storage_job(
        &mut self,
        caller: Option<Caller>,
        req: BaselineRequest,
        (reply, revoked, epoch): (Reply, watch::Receiver<bool>, u64),
    ) {
        let Some(caller) = caller else {
            let _ = reply.send(Err(ReplyError::Operational(
                PublicError::permission_denied("no VM permissions for this role"),
            )));
            return;
        };
        let vm_id = req.vm_id();
        let cancel: CancelCheck = {
            let current = self.emergency_epoch.clone();
            let shutdown = self.storage_cancel.clone();
            let rv = revoked.clone();
            Arc::new(move || {
                shutdown.load(Ordering::SeqCst)
                    || current.load(Ordering::SeqCst) != epoch
                    || is_revoked(&rv)
            })
        };
        let job = match self.engine.begin_baseline_job(&caller, req, cancel) {
            Ok(job) => job,
            Err(e) => {
                let _ = reply.send(Err(ReplyError::Operational(e)));
                return;
            }
        };
        let _ = self.events.send(ServiceEvent::BaselinesChanged { vm_id });
        let _ = self.events.send(ServiceEvent::SnapshotsChanged { vm_id });
        let tx = self.self_tx.clone();
        // The worker owns the job: if the thread cannot start, the work
        // runs here (blocking) so that the operation still ends.
        let (job_tx, job_rx) = mpsc::channel::<crate::engine::StorageJob>();
        let spawned = std::thread::Builder::new()
            .name("lightvm-baseline".into())
            .spawn(move || {
                let Ok(job) = job_rx.recv() else { return };
                let done = job.run();
                // If the supervisor is gone the result stays unpublished and
                // leaves nothing to roll back.
                let _ = tx.send(Command::FinishStorage {
                    done,
                    reply,
                    revoked,
                    epoch,
                });
            });
        match spawned {
            Ok(_) => {
                self.storage_jobs += 1;
                if let Err(mpsc::SendError(job)) = job_tx.send(job) {
                    crate::diag!("lightvm-service: baseline worker gone: running inline");
                    let done = job.run();
                    let _ = self
                        .engine
                        .abort_baseline_job(done, PublicError::infrastructure("worker lost"));
                    self.storage_jobs = self.storage_jobs.saturating_sub(1);
                }
            }
            Err(e) => {
                crate::diag!("lightvm-service: baseline thread not started: {e}");
                let done = job.run();
                let _ = self.engine.abort_baseline_job(
                    done,
                    PublicError::infrastructure("worker thread not available"),
                );
            }
        }
    }

    /// File checks of a start (media and baseline hashes, see
    /// [`crate::start`]) on a worker thread; the result comes back as
    /// [`Command::FinishStart`]. Cancelled by the engine (forced stop or
    /// Emergency Stop of the VM), by a new Emergency Stop generation, by the
    /// revocation or closing of the requesting connection, by the shutdown.
    fn spawn_start_job(&mut self, job: StartJob, holder: Option<StartHolder>) {
        let epoch = self.emergency_epoch.load(Ordering::SeqCst);
        let cancel: CancelCheck = {
            let current = self.emergency_epoch.clone();
            let shutdown = self.storage_cancel.clone();
            let rv = holder.as_ref().map(|h| h.revoked.clone());
            Arc::new(move || {
                shutdown.load(Ordering::SeqCst)
                    || current.load(Ordering::SeqCst) != epoch
                    || rv.as_ref().is_some_and(is_revoked)
            })
        };
        let tx = self.self_tx.clone();
        // The worker owns the job (its handles) and the held reply: if the
        // thread cannot start, the checks run here so that the start ends.
        let (job_tx, job_rx) = mpsc::channel::<(StartJob, Option<StartHolder>)>();
        let spawned = std::thread::Builder::new()
            .name("lightvm-start-check".into())
            .spawn(move || {
                let Ok((job, holder)) = job_rx.recv() else {
                    return;
                };
                let done = job.run(&|| cancel());
                // If the supervisor is gone nothing is launched: the
                // handles close with the result.
                let _ = tx.send(Command::FinishStart {
                    done,
                    holder,
                    epoch,
                });
            });
        let inline = match spawned {
            Ok(_) => match job_tx.send((job, holder)) {
                Ok(()) => {
                    self.start_jobs += 1;
                    None
                }
                Err(mpsc::SendError(back)) => Some(back),
            },
            Err(e) => {
                crate::diag!("lightvm-service: start check thread not started: {e}");
                Some((job, holder))
            }
        };
        if let Some((job, holder)) = inline {
            let done = job.run(&|| false);
            let mut slot = None;
            self.finish_start(done, holder, epoch, &mut slot);
        }
    }

    /// Last step of a start (see [`Command::FinishStart`]).
    fn finish_start(
        &mut self,
        done: StartDone,
        holder: Option<StartHolder>,
        epoch: u64,
        slot: &mut Option<Reply>,
    ) {
        let abort = self.start_abort_reason(holder.as_ref(), epoch);
        let admission = holder.as_ref().map(|h| h.admission.clone());
        *slot = holder.map(|h| h.reply);
        let res = self.engine.finish_start(done, abort);
        match (slot.take(), admission) {
            (Some(reply), Some(admission)) => {
                let _ = reply.send(
                    res.map(|op| ResponseBody::Operation {
                        admission,
                        operation: Some(op),
                    })
                    .map_err(ReplyError::Operational),
                );
            }
            _ => {
                if let Err(e) = res {
                    crate::diag!("lightvm-service: start not completed: {}", e.message);
                }
            }
        }
        // Starts queued behind this one, dispatched by the engine.
        for job in self.engine.take_start_jobs() {
            self.spawn_start_job(job, None);
        }
    }

    /// Reason to cancel a start whose checks ended: revocation of the
    /// requesting connection, Emergency Stop, shutdown.
    fn start_abort_reason(&self, holder: Option<&StartHolder>, epoch: u64) -> Option<PublicError> {
        if holder.is_some_and(|h| is_revoked(&h.revoked)) {
            Some(PublicError::unauthenticated(
                "connection revoked: start cancelled",
            ))
        } else if epoch != self.emergency_epoch.load(Ordering::SeqCst) {
            Some(PublicError::state_conflict(
                "start cancelled by the Emergency Stop",
            ))
        } else if self.storage_cancel.load(Ordering::SeqCst) {
            Some(PublicError::state_conflict("service shutting down"))
        } else {
            None
        }
    }

    /// Console ticket: the core checks the caller and the VM state, the
    /// display registry starts the per-run proxy owned by the connection;
    /// the outcome is audited only once the proxy is up (or failed).
    fn open_console(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        connection_id: u64,
    ) -> Result<ResponseBody, PublicError> {
        let target = self.engine.open_console(caller, vm_id)?;
        let run_id = target.run_id;
        let issued = (|| {
            let socket = target
                .display_socket
                .as_deref()
                .ok_or_else(|| PublicError::unsupported("console not available on this backend"))?;
            let ticket = self
                .display
                .issue(vm_id, run_id, connection_id, socket)
                .map_err(|e| {
                    crate::diag!("lightvm-service: display proxy not started: {e}");
                    PublicError::infrastructure("display proxy not available")
                })?;
            if let Some(prev) = ticket.replaced {
                self.engine.console_revoked(
                    caller.actor(),
                    &[(vm_id, prev)],
                    TerminationCause::Completed,
                );
            }
            let Some(socket_path) = ticket.socket_path.to_str().map(str::to_owned) else {
                self.display.revoke_vm(vm_id);
                return Err(PublicError::infrastructure("display socket path not UTF-8"));
            };
            Ok((socket_path, ticket.token))
        })();
        let (socket_path, token) = match issued {
            Ok(t) => t,
            Err(e) => {
                self.engine
                    .console_opened(caller, vm_id, run_id, Err(e.code));
                return Err(e);
            }
        };
        self.engine.console_opened(caller, vm_id, run_id, Ok(()));
        Ok(ResponseBody::Console {
            ticket: ConsoleTicket {
                vm_id,
                run_id,
                socket_path,
                token,
                geometry: target.geometry,
            },
        })
    }

    pub(crate) fn handle(
        &mut self,
        req: SupervisorRequest,
        caller: Option<&Caller>,
        connection_id: u64,
    ) -> Result<ResponseBody, ReplyError> {
        let who = || {
            caller.ok_or_else(|| PublicError::permission_denied("no VM permissions for this role"))
        };
        let r = match req {
            SupervisorRequest::Ping => Ok(ResponseBody::Pong),
            SupervisorRequest::GetCapabilities => Ok(ResponseBody::Capabilities(Capabilities {
                protocol_versions: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                backend: self.backend.name(),
                accelerator: self.backend.accelerator(),
                max_frame_bytes: self.max_frame,
                host: self.engine.host_capacity(),
                parent_loss_policy: *self
                    .parent_loss_policy
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            })),
            SupervisorRequest::ListVms => self
                .engine
                .list_vms(who()?)
                .map(|vms| ResponseBody::VmList { vms }),
            SupervisorRequest::VmStatus(vm_id) => self
                .engine
                .vm_status(who()?, vm_id)
                .map(|vm| ResponseBody::Vm { vm }),
            SupervisorRequest::CreateVm(config) => self
                .engine
                .create_vm(who()?, *config)
                .map(|vm| ResponseBody::VmCreated { vm }),
            SupervisorRequest::UpdateVm {
                vm_id,
                expected_revision,
                config,
            } => self
                .engine
                .update_vm(who()?, vm_id, expected_revision, *config)
                .map(|vm| ResponseBody::VmUpdated { vm }),
            SupervisorRequest::SubmitVm(req) => {
                self.engine
                    .submit(who()?, *req)
                    .map(|(admission, operation)| ResponseBody::Operation {
                        admission,
                        operation,
                    })
            }
            SupervisorRequest::ListMedia => self
                .engine
                .list_media(who()?)
                .map(|media| ResponseBody::MediaList { media }),
            SupervisorRequest::DeleteMedia(media_id) => self
                .engine
                .delete_media(who()?, media_id)
                .map(|()| ResponseBody::MediaDeleted { media_id }),
            SupervisorRequest::DeleteVm {
                vm_id,
                expected_revision,
            } => self
                .engine
                .delete_vm(who()?, vm_id, expected_revision)
                .map(|()| ResponseBody::VmDeleted { vm_id }),
            SupervisorRequest::OpenConsole(vm_id) => {
                let c = who()?;
                self.open_console(c, vm_id, connection_id)
            }
            // Served by `start_import` (reply after the hash).
            SupervisorRequest::ImportMedia { .. } => {
                Err(PublicError::infrastructure("media import not dispatched"))
            }
            // Served by `start_vmware_preview` / `start_vmware_import`.
            SupervisorRequest::PreviewVmware { .. } | SupervisorRequest::ImportVmware { .. } => {
                Err(PublicError::infrastructure("VMware import not dispatched"))
            }
            SupervisorRequest::CancelVmwareImport => {
                who()?;
                let cancelled = self.vmware.import.as_ref().is_some_and(|flag| {
                    flag.store(true, Ordering::SeqCst);
                    true
                });
                Ok(ResponseBody::VmwareImportCancel { cancelled })
            }
            SupervisorRequest::ListBaselines(vm_id) => self
                .engine
                .list_baselines(who()?, vm_id)
                .map(|baselines| ResponseBody::Baselines { vm_id, baselines }),
            SupervisorRequest::DeleteBaseline { vm_id, baseline_id } => {
                let r = self.engine.delete_baseline(who()?, vm_id, baseline_id);
                if r.is_ok() {
                    let _ = self.events.send(ServiceEvent::BaselinesChanged { vm_id });
                }
                r.map(|()| ResponseBody::BaselineDeleted { vm_id, baseline_id })
            }
            // Served by `start_storage_job` (reply after the work).
            SupervisorRequest::CreateBaseline { .. }
            | SupervisorRequest::RestoreBaseline { .. }
            | SupervisorRequest::TakeSnapshot { .. }
            | SupervisorRequest::RevertSnapshot { .. }
            | SupervisorRequest::DeleteSnapshot { .. }
            | SupervisorRequest::PromoteSnapshot { .. } => Err(PublicError::infrastructure(
                "storage operation not dispatched",
            )),
            SupervisorRequest::ListSnapshots(vm_id) => self
                .engine
                .list_snapshots(who()?, vm_id)
                .map(|tree| ResponseBody::Snapshots { vm_id, tree }),
            SupervisorRequest::UpdateSnapshot {
                vm_id,
                snapshot_id,
                name,
                description,
            } => {
                let r = self
                    .engine
                    .update_snapshot(who()?, vm_id, snapshot_id, name, description);
                if r.is_ok() {
                    let _ = self.events.send(ServiceEvent::SnapshotsChanged { vm_id });
                }
                r.map(|snapshot| ResponseBody::Snapshot { snapshot })
            }
            SupervisorRequest::SnapshotThumbnail { vm_id, snapshot_id } => self
                .engine
                .snapshot_thumbnail(who()?, vm_id, snapshot_id)
                .map(|png| ResponseBody::SnapshotThumbnail {
                    snapshot_id,
                    png_base64: png.map(|p| lightvm_proto::ipc::base64_encode(&p)),
                }),
            SupervisorRequest::CreateSession(new) => {
                who()?;
                // The token goes back to the service only to be delivered to
                // the waiting agent (never logged: `Debug` redacts it).
                let token = new.token.clone();
                self.engine.create_session(*new).map(|c| {
                    self.open_report(&c);
                    ResponseBody::SessionGranted {
                        session_id: c.grant.session_id,
                        token,
                        grant: c.grant,
                        lease: c.lease,
                    }
                })
            }
            SupervisorRequest::ListSessions => {
                who()?;
                Ok(ResponseBody::Sessions {
                    sessions: self.engine.list_sessions(),
                })
            }
            SupervisorRequest::PauseSession(id) => {
                who()?;
                self.engine
                    .pause_session(id)
                    .map(|session| ResponseBody::Session { session })
            }
            SupervisorRequest::ResumeSession(id) => {
                who()?;
                self.engine
                    .resume_session(id)
                    .map(|session| ResponseBody::Session { session })
            }
            SupervisorRequest::EndSession {
                session_id,
                cause,
                stop,
            } => self
                .engine
                .end_session(session_id, cause, stop)
                .map(|report| ResponseBody::SessionRevoked { session_id, report }),
            SupervisorRequest::EndAllSessions { cause, stop } => {
                self.engine.end_all_sessions(cause, stop);
                Ok(ResponseBody::Pong)
            }
            SupervisorRequest::SetInputOwner { vm_id, owner } => {
                who()?;
                self.engine
                    .set_input_owner(vm_id, owner)
                    .map(|()| ResponseBody::InputOwner { vm_id, owner })
            }
            SupervisorRequest::Report(req) => {
                who()?;
                self.report_request(*req)
            }
            SupervisorRequest::Agent(call) => {
                let c = who()?;
                if let Some(r) = self.report_agent_call(c, &call) {
                    return r.map_err(ReplyError::Operational);
                }
                let end = matches!(*call, AgentCall::EndSession);
                let r = self.engine.agent_call(c, *call);
                if end
                    && r.is_ok()
                    && let Caller::Agent { session_id, .. } = c
                {
                    // Ordinary end by the agent: the gate closes, the
                    // connection stays (a new request is possible).
                    let _ = self.agents.terminate(*session_id, true, None);
                }
                r
            }
        };
        r.map_err(ReplyError::Operational)
    }
}

#[cfg(test)]
mod start_tests;
#[cfg(test)]
mod tests;
