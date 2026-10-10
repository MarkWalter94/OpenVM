//! Supervisor main loop: a dedicated thread owns the VM engine ([`VmEngine`],
//! with `vm_core::Core`) and the host ([`Backend`]) as sole owner (no shared
//! lock on VM state) and serves commands in order. The Emergency Stop does NOT
//! go through here: it uses [`EmergencyStop`], obtained from the host at
//! startup and callable from any task (ROADMAP 7.3, ADR-0009); only after the
//! termination does the supervisor record it in the core.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use lightvm_proto::ipc::{
    Capabilities, ConsoleTicket, ResponseBody, SUPPORTED_PROTOCOL_VERSIONS, ServiceEvent,
    ServiceState, StopReason,
};
use lightvm_proto::{
    Actor, Caller, MediaKind, PublicError, ReplyError, RunId, VmConfig, VmId, VmRequest,
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
        /// (double check, ROADMAP 7.3).
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
    },
    /// Console lost (ticket revoked with its UI connection): release the
    /// keys and buttons still pressed on these runs.
    ReleaseInput(Vec<(VmId, RunId)>),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request { req, .. } => write!(f, "Request({req:?})"),
            Self::EmergencyReconcile(a) => write!(f, "EmergencyReconcile({a:?})"),
            Self::ParentLost { policy, .. } => write!(f, "ParentLost({policy:?})"),
            Self::Shutdown { .. } => write!(f, "Shutdown"),
            Self::FinishImport { .. } => write!(f, "FinishImport"),
            Self::ReleaseInput(v) => write!(f, "ReleaseInput({})", v.len()),
        }
    }
}

/// Error for a credential or connection that is no longer valid.
pub(crate) fn unauthenticated(message: &str) -> ReplyError {
    ReplyError::Operational(PublicError::unauthenticated(message))
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

/// Interval at which, without commands, the supervisor observes process exits
/// (shutdown from the guest, crash) and publishes their events.
const OBSERVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

impl SupervisorThread {
    pub(crate) fn run(mut self, rx: mpsc::Receiver<Command>) {
        loop {
            let cmd = match rx.recv_timeout(OBSERVE_INTERVAL) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.engine.observe();
                    self.publish_state_changes();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
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
                    let res = if *revoked.borrow() {
                        Err(unauthenticated("connection revoked"))
                    } else if req.is_mutation()
                        && epoch != self.emergency_epoch.load(Ordering::SeqCst)
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
                        self.start_import(
                            caller,
                            &source_path,
                            media_kind,
                            &declared_origin,
                            reply,
                        );
                        self.publish_state_changes();
                        continue;
                    } else {
                        self.handle(req, caller.as_ref(), connection_id)
                    };
                    let _ = reply.send(res);
                }
                Command::FinishImport {
                    caller,
                    copied,
                    reply,
                } => {
                    self.imports = self.imports.saturating_sub(1);
                    let res = self
                        .engine
                        .finish_import(&caller, copied)
                        .map(|media| ResponseBody::MediaImported { media })
                        .map_err(ReplyError::Operational);
                    let _ = reply.send(res);
                }
                Command::ReleaseInput(runs) => {
                    for (vm_id, run_id) in runs {
                        self.engine.release_input(vm_id, run_id);
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
                    let _ = self.events.send(ServiceEvent::ServiceState {
                        state: ServiceState::Stopping,
                    });
                    // Requests still in the queue are not executed.
                    while let Ok(pending) = rx.try_recv() {
                        match pending {
                            Command::Request { reply, .. } => {
                                let _ = reply.send(Err(ReplyError::Operational(
                                    PublicError::state_conflict("service shutting down"),
                                )));
                            }
                            Command::EmergencyReconcile(actor) => {
                                self.engine.after_emergency_stop(actor);
                            }
                            Command::ParentLost { done, .. } | Command::Shutdown { done } => {
                                let _ = done.send(());
                            }
                            Command::FinishImport { reply, .. } => {
                                // The copy is not published: the open intent is
                                // rolled back by the next catalog open.
                                let _ = reply.send(Err(ReplyError::Operational(
                                    PublicError::state_conflict("service shutting down"),
                                )));
                            }
                            Command::ReleaseInput(_) => {}
                        }
                    }
                    self.display.revoke_all();
                    self.backend.stop_agent_sessions(StopReason::Shutdown);
                    self.backend.prepare_shutdown();
                    self.engine.stop_all();
                    self.publish_state_changes();
                    self.backend.shutdown();
                    let _ = done.send(());
                    return;
                }
            }
            self.publish_state_changes();
        }
    }

    fn publish_state_changes(&mut self) {
        for (vm_id, state) in self.engine.state_changes() {
            // The display ticket lives only as long as the run: a state
            // without a process revokes it (stopped, failed, reconciliation);
            // during a graceful stop (`stopping`) the console stays open.
            if !state.has_process() {
                self.display.revoke_vm(vm_id);
            }
            let _ = self.events.send(ServiceEvent::VmState { vm_id, state });
        }
        self.vms_with_process
            .store(self.engine.vms_with_process(), Ordering::SeqCst);
    }

    /// First step of a media import on this thread, copy on a worker
    /// thread, last step back here with [`Command::FinishImport`]: the
    /// supervisor keeps serving the VMs (console input included) while a
    /// multi-GiB ISO is copied.
    fn start_import(
        &mut self,
        caller: Option<Caller>,
        source: &str,
        kind: MediaKind,
        origin: &str,
        reply: Reply,
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
                });
            });
        match spawned {
            Ok(_) => self.imports += 1,
            Err(e) => crate::diag!("lightvm-service: media import thread not started: {e}"),
        }
    }

    /// Console ticket: the core checks the caller and the VM state, the
    /// display registry starts the per-run proxy owned by the connection.
    fn open_console(
        &mut self,
        caller: &Caller,
        vm_id: VmId,
        connection_id: u64,
    ) -> Result<ResponseBody, PublicError> {
        let target = self.engine.open_console(caller, vm_id)?;
        let socket = target
            .display_socket
            .ok_or_else(|| PublicError::unsupported("console not available on this backend"))?;
        let ticket = self
            .display
            .issue(vm_id, target.run_id, connection_id, &socket)
            .map_err(|e| {
                crate::diag!("lightvm-service: display proxy not started: {e}");
                PublicError::infrastructure("display proxy not available")
            })?;
        let socket_path = ticket
            .socket_path
            .to_str()
            .ok_or_else(|| PublicError::infrastructure("display socket path not UTF-8"))?
            .to_owned();
        Ok(ResponseBody::Console {
            ticket: ConsoleTicket {
                vm_id,
                run_id: target.run_id,
                socket_path,
                token: ticket.token,
                geometry: target.geometry,
            },
        })
    }

    fn handle(
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
