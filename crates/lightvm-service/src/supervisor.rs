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
//!   failed).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lightvm_proto::ipc::{
    Capabilities, ConsoleTicket, ResponseBody, SUPPORTED_PROTOCOL_VERSIONS, ServiceEvent,
    ServiceState, StopReason,
};
use lightvm_proto::{
    Actor, Caller, MediaKind, PublicError, ReplyError, RunId, TerminationCause, VmConfig, VmId,
    VmRequest,
};
use tokio::sync::{OwnedSemaphorePermit, broadcast, oneshot, watch};

use crate::display::DisplayRegistry;
use crate::engine::{ImportCopied, VmEngine};

/// Media imports copying at the same time (each holds a worker thread).
pub const MAX_CONCURRENT_IMPORTS: usize = 2;

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
        )
    }
}

pub(crate) type Reply = oneshot::Sender<Result<ResponseBody, ReplyError>>;

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
        reply: Reply,
        /// Queue slot: released when the command is served.
        _permit: OwnedSemaphorePermit,
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
    /// Console tickets revoked outside the supervisor thread (UI connection
    /// revoked or closed, Emergency Stop): audit and, for a lost console,
    /// release of the keys and buttons still pressed on these runs.
    ConsolesRevoked {
        runs: Vec<(VmId, RunId)>,
        actor: Actor,
        cause: TerminationCause,
        release_input: bool,
    },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request { req, .. } => write!(f, "Request({req:?})"),
            Self::EmergencyReconcile(a) => write!(f, "EmergencyReconcile({a:?})"),
            Self::ParentLost { policy, .. } => write!(f, "ParentLost({policy:?})"),
            Self::Shutdown { .. } => write!(f, "Shutdown"),
            Self::FinishImport { .. } => write!(f, "FinishImport"),
            Self::ConsolesRevoked { runs, cause, .. } => {
                write!(f, "ConsolesRevoked({}, {cause:?})", runs.len())
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
                reply,
                _permit,
            } => {
                *slot = Some(reply);
                let res = if is_revoked(&revoked) {
                    Err(unauthenticated("connection revoked"))
                } else if req.is_mutation() && epoch != self.emergency_epoch.load(Ordering::SeqCst)
                {
                    Err(ReplyError::Operational(PublicError::state_conflict(
                        "request discarded by the Emergency Stop",
                    )))
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
            Command::ParentLost { policy, done } => {
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
        // Requests still in the queue are not executed.
        while let Ok(pending) = rx.try_recv() {
            match pending {
                Command::Request { reply, .. } => {
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
                Command::ConsolesRevoked {
                    runs, actor, cause, ..
                } => self.engine.console_revoked(actor, &runs, cause),
            }
        }
        let runs = self.display.revoke_all();
        self.engine
            .console_revoked(Actor::System, &runs, TerminationCause::Completed);
        self.backend.stop_agent_sessions(StopReason::Shutdown);
        self.backend.prepare_shutdown();
        self.engine.stop_all();
        self.publish_state_changes();
        self.backend.shutdown();
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
        };
        r.map_err(ReplyError::Operational)
    }
}

#[cfg(test)]
mod tests;
