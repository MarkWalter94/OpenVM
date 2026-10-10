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
    Capabilities, ResponseBody, SUPPORTED_PROTOCOL_VERSIONS, ServiceEvent, ServiceState, StopReason,
};
use lightvm_proto::{Actor, Caller, PublicError, ReplyError, VmConfig, VmId, VmRequest};
use tokio::sync::{OwnedSemaphorePermit, broadcast, oneshot, watch};

use crate::engine::VmEngine;

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
}

impl SupervisorRequest {
    /// Request that modifies VMs or the catalog: discarded if queued before an
    /// Emergency Stop.
    fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::CreateVm(_) | Self::UpdateVm { .. } | Self::SubmitVm(_)
        )
    }
}

pub(crate) type Reply = oneshot::Sender<Result<ResponseBody, ReplyError>>;

pub(crate) enum Command {
    Request {
        req: SupervisorRequest,
        /// Caller towards the core; `None` for roles without VM permissions.
        caller: Option<Caller>,
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
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request { req, .. } => write!(f, "Request({req:?})"),
            Self::EmergencyReconcile(a) => write!(f, "EmergencyReconcile({a:?})"),
            Self::ParentLost { policy, .. } => write!(f, "ParentLost({policy:?})"),
            Self::Shutdown { .. } => write!(f, "Shutdown"),
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
                    } else {
                        self.handle(req, caller.as_ref())
                    };
                    let _ = reply.send(res);
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
                        }
                    }
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
            let _ = self.events.send(ServiceEvent::VmState { vm_id, state });
        }
        self.vms_with_process
            .store(self.engine.vms_with_process(), Ordering::SeqCst);
    }

    fn handle(
        &mut self,
        req: SupervisorRequest,
        caller: Option<&Caller>,
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
                host: None,
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
        };
        r.map_err(ReplyError::Operational)
    }
}
