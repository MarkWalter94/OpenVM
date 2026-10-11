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
//!   included, also when their copy ends after the revocation or the
//!   Emergency Stop: the copy is then discarded and the import audited as
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
    ServiceState, StopReason,
};
use lightvm_proto::{
    Actor, Admission, BaselineId, Caller, Label, MediaKind, OperationStatus, PublicError,
    ReplyError, RunId, TerminationCause, VmConfig, VmId, VmRequest,
};
use tokio::sync::{OwnedSemaphorePermit, broadcast, oneshot, watch};

use lightvm_proto::ipc::{AgentCall, InputOwner};
use lightvm_proto::{AuditAction, AuditOutcome, ErrorCode, SessionId, SessionState};

use crate::agents::AgentRegistry;
use crate::display::DisplayRegistry;
use crate::engine::{
    BaselineOutcome, BaselineRequest, CancelCheck, ImportCopied, NewSession, StartDone, StartJob,
    StorageDone, VmEngine,
};

/// Media imports copying at the same time (each holds a worker thread).
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
                | Self::DeleteVm { .. }
                | Self::OpenConsole(_)
                | Self::CreateBaseline { .. }
                | Self::RestoreBaseline { .. }
                | Self::DeleteBaseline { .. }
                | Self::CreateSession(_)
                | Self::SetInputOwner { .. }
        ) || matches!(self, Self::Agent(call) if call.is_mutation())
    }

    /// Baseline request served as a long storage operation (F3).
    fn baseline_job(&self) -> Option<BaselineRequest> {
        match self {
            Self::CreateBaseline {
                vm_id,
                label,
                allow_unclean,
            } => Some(BaselineRequest::Create {
                vm_id: *vm_id,
                label: label.clone(),
                allow_unclean: *allow_unclean,
            }),
            Self::RestoreBaseline { vm_id, baseline_id } => Some(BaselineRequest::Restore {
                vm_id: *vm_id,
                baseline_id: *baseline_id,
            }),
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
    /// Sender towards this same thread (end of a media import copy).
    pub(crate) self_tx: mpsc::Sender<Command>,
    /// Media imports whose copy is in progress.
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
            match rx.recv_timeout(wait) {
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
            }));
            if let Err(p) = r {
                crate::diag!(
                    "lightvm-service: supervisor: panic while observing the VMs: {}",
                    panic_text(p.as_ref())
                );
            }
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
                        self.start_storage_job(caller, job, (reply, revoked, epoch));
                    }
                    return Served::Continue;
                } else if let SupervisorRequest::ImportMedia {
                    source_path,
                    media_kind,
                    declared_origin,
                } = req
                {
                    // The reply is sent when the copy ends.
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
                    })
                };
                let _ = self.events.send(ServiceEvent::BaselinesChanged { vm_id });
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
                    // The copy is not published.
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
            let _ = self.events.send(ServiceEvent::VmState { vm_id, state });
        }
        // F4: sessions terminated by the core for any cause close their gate
        // and their connection; changes and input owners are published.
        for (session_id, state, termination) in self.engine.session_changes() {
            if state == SessionState::Terminated {
                self.agents.terminated(session_id);
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

    /// First step of a media import on this thread (text checks and journal
    /// intent, no access to the file), copy on a worker thread (open, file
    /// checks, copy), last step back here with [`Command::FinishImport`]: the
    /// supervisor keeps serving the VMs (console input included) while a
    /// multi-GiB ISO is copied, and a slow source never blocks it.
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
                // If the supervisor is gone the copy stays unpublished and
                // the catalog rolls it back at the next open.
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
                // the catalog rolls it back at the next open.
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
            // Served by `start_import` (reply after the copy).
            SupervisorRequest::ImportMedia { .. } => {
                Err(PublicError::infrastructure("media import not dispatched"))
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
            | SupervisorRequest::RestoreBaseline { .. } => Err(PublicError::infrastructure(
                "baseline operation not dispatched",
            )),
            SupervisorRequest::CreateSession(new) => {
                who()?;
                // The token goes back to the service only to be delivered to
                // the waiting agent (never logged: `Debug` redacts it).
                let token = new.token.clone();
                self.engine
                    .create_session(*new)
                    .map(|c| ResponseBody::SessionGranted {
                        session_id: c.grant.session_id,
                        token,
                        grant: c.grant,
                        lease: c.lease,
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
            SupervisorRequest::Agent(call) => {
                let c = who()?;
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
