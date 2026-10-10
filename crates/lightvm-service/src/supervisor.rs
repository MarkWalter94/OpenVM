//! Ciclo principale del supervisor: un thread dedicato possiede il motore
//! delle VM ([`VmEngine`], con `vm_core::Core`) e l'host ([`Backend`]) come
//! unico proprietario (nessun lock condiviso sullo stato VM) e serve i comandi
//! in ordine. L'Emergency Stop NON passa da qui: usa [`EmergencyStop`],
//! ottenuto dall'host all'avvio e chiamabile da qualunque task (ROADMAP 7.3,
//! ADR-0009); solo dopo la terminazione il supervisor la registra nel core.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use lightvm_proto::ipc::{
    Capabilities, ResponseBody, SUPPORTED_PROTOCOL_VERSIONS, ServiceEvent, ServiceState, StopReason,
};
use lightvm_proto::{Caller, PublicError, ReplyError, VmConfig, VmId, VmRequest};
use tokio::sync::{OwnedSemaphorePermit, broadcast, oneshot, watch};

use crate::engine::VmEngine;

/// Host del servizio: processi, sessioni agente e percorso prioritario.
///
/// Implementazioni: [`crate::adapters::backend::QemuHost`] (reale) e
/// [`crate::fakes::FakeHost`] (test).
pub trait Backend: Send + 'static {
    /// Nome del backend.
    fn name(&self) -> String;
    /// Acceleratore rilevato, se presente.
    fn accelerator(&self) -> Option<String>;
    /// Gestore dell'Emergency Stop, chiamabile fuori dal thread del supervisor.
    fn emergency_handle(&self) -> Arc<dyn EmergencyStop>;
    /// Arresta le sessioni agente: nessun nuovo inoltro, coda dell'agente scartata.
    fn stop_agent_sessions(&mut self, reason: StopReason);
    /// Hook chiamato alla perdita della UI, prima che il servizio applichi
    /// [`ParentLossPolicy`] (ADR-0015 parte B): con `shutdown_vms` il backend
    /// adotta l'attesa ACPI indicata per l'arresto che segue.
    fn on_parent_lost(&mut self, policy: ParentLossPolicy);
    /// Chiamato all'inizio dell'arresto controllato del servizio, prima che
    /// il motore arresti le VM: il backend adotta un arresto ordinato che
    /// stia nel timeout del servizio (se una politica non ne ha già scelto uno).
    fn prepare_shutdown(&mut self) {}
    /// Rilascio finale dell'host (chiamato durante lo shutdown, dopo l'arresto
    /// delle VM da parte del motore).
    fn shutdown(&mut self);
}

/// Percorso prioritario di arresto.
pub trait EmergencyStop: Send + Sync + 'static {
    /// Termina l'albero dei processi con primitive OS senza attendere QMP.
    /// Deve restituire solo dopo aver osservato la fine dei processi o un timeout.
    fn emergency_stop_all(&self) -> EmergencyOutcome;
}

/// Esito osservato dell'Emergency Stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmergencyOutcome {
    /// Processi di cui è stata osservata la fine.
    pub processes_terminated: u32,
    /// Vero solo se la fine di tutti i processi è stata osservata.
    pub completed: bool,
}

pub use lightvm_proto::ipc::ParentLossPolicy;

/// Richieste servite dal thread del supervisor.
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
    /// Richiesta che modifica VM o catalogo: scartata se accodata prima di
    /// un Emergency Stop.
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
        /// Chiamante verso il core; `None` per i ruoli senza permessi VM.
        caller: Option<Caller>,
        /// Revoca della connessione: ricontrollata immediatamente prima di
        /// eseguire (doppio controllo, ROADMAP 7.3).
        revoked: watch::Receiver<bool>,
        /// Generazione dell'Emergency Stop al momento dell'accodamento.
        epoch: u64,
        reply: Reply,
        /// Posto in coda: rilasciato quando il comando è servito.
        _permit: OwnedSemaphorePermit,
    },
    /// L'Emergency Stop ha già terminato i processi: registrazione nel core.
    EmergencyReconcile,
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
            Self::EmergencyReconcile => write!(f, "EmergencyReconcile"),
            Self::ParentLost { policy, .. } => write!(f, "ParentLost({policy:?})"),
            Self::Shutdown { .. } => write!(f, "Shutdown"),
        }
    }
}

/// Errore per credenziale o connessione non più valida.
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
}

impl SupervisorThread {
    pub(crate) fn run(mut self, rx: mpsc::Receiver<Command>) {
        while let Ok(cmd) = rx.recv() {
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
                        Err(unauthenticated("connessione revocata"))
                    } else if req.is_mutation()
                        && epoch != self.emergency_epoch.load(Ordering::SeqCst)
                    {
                        Err(ReplyError::Operational(PublicError::state_conflict(
                            "richiesta scartata dall'Emergency Stop",
                        )))
                    } else {
                        self.handle(req, caller.as_ref())
                    };
                    let _ = reply.send(res);
                }
                Command::EmergencyReconcile => self.engine.after_emergency_stop(),
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
                    // Le richieste ancora in coda non vengono eseguite.
                    while let Ok(pending) = rx.try_recv() {
                        match pending {
                            Command::Request { reply, .. } => {
                                let _ = reply.send(Err(ReplyError::Operational(
                                    PublicError::state_conflict("servizio in arresto"),
                                )));
                            }
                            Command::EmergencyReconcile => self.engine.after_emergency_stop(),
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
    }

    fn handle(
        &mut self,
        req: SupervisorRequest,
        caller: Option<&Caller>,
    ) -> Result<ResponseBody, ReplyError> {
        let who = || {
            caller.ok_or_else(|| {
                PublicError::permission_denied("nessun permesso sulle VM per questo ruolo")
            })
        };
        let r = match req {
            SupervisorRequest::Ping => Ok(ResponseBody::Pong),
            SupervisorRequest::GetCapabilities => Ok(ResponseBody::Capabilities(Capabilities {
                protocol_versions: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                backend: self.backend.name(),
                accelerator: self.backend.accelerator(),
                max_frame_bytes: self.max_frame,
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
