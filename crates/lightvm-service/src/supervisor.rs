//! Ciclo principale del supervisor: un thread dedicato possiede `Catalog` e
//! `Backend` (unico proprietario, nessun lock condiviso sullo stato VM) e
//! serve i comandi in ordine. L'Emergency Stop NON passa da qui: usa
//! [`EmergencyStop`], ottenuto dal backend all'avvio e chiamabile da qualunque
//! task (ROADMAP 7.3, ADR-0009).

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

use tokio::sync::{OwnedSemaphorePermit, broadcast, oneshot, watch};

use crate::compat::{
    Capabilities, ErrorCode, ReplyError, ResponseBody, SUPPORTED_PROTOCOL_VERSIONS, ServiceEvent,
    ServiceState, StopReason, VmState, VmSummary, op_error,
};

/// Catalogo delle VM.
///
/// TODO(F1): implementazione reale in `vm-storage` (SQLite, manifest).
pub trait Catalog: Send + 'static {
    /// Elenco delle VM.
    fn list_vms(&mut self) -> Result<Vec<VmSummary>, ReplyError>;
}

/// Backend di esecuzione.
///
/// TODO(F1): implementazione reale in `qemu-backend` (processi, QMP, Job Object).
pub trait Backend: Send + 'static {
    /// Nome del backend.
    fn name(&self) -> String;
    /// Acceleratore rilevato, se presente.
    fn accelerator(&self) -> Option<String>;
    /// Gestore dell'Emergency Stop, chiamabile fuori dal thread del supervisor.
    fn emergency_handle(&self) -> Arc<dyn EmergencyStop>;
    /// Arresta le sessioni agente: nessun nuovo inoltro, coda dell'agente scartata.
    fn stop_agent_sessions(&mut self, reason: StopReason);
    /// Hook per la sorte delle VM manuali dopo la perdita della UI.
    ///
    /// ADR-0015 parte B è in sospeso: il servizio chiama questo hook e poi
    /// applica [`OrphanPolicy`]; il backend reale deciderà se arrestare o
    /// mantenere le VM manuali.
    fn on_parent_lost(&mut self, policy: OrphanPolicy);
    /// Arresto controllato delle VM possedute (chiamato durante lo shutdown).
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

/// Comportamento dopo la perdita del processo genitore (UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrphanPolicy {
    /// Arresto controllato del servizio dopo l'arresto delle sessioni agente.
    /// Predefinito: senza UI la credenziale iniziale non è recuperabile, quindi
    /// nessun ricollegamento è possibile in F1.
    #[default]
    ShutdownService,
    /// Le VM manuali restano; il servizio resta attivo (ADR-0015 parte B).
    KeepManualVms,
}

/// Richieste servite dal thread del supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupervisorRequest {
    Ping,
    GetCapabilities,
    ListVms,
}

pub(crate) type Reply = oneshot::Sender<Result<ResponseBody, ReplyError>>;

pub(crate) enum Command {
    Request {
        req: SupervisorRequest,
        /// Revoca della connessione: ricontrollata immediatamente prima di
        /// eseguire (doppio controllo, ROADMAP 7.3).
        revoked: watch::Receiver<bool>,
        reply: Reply,
        /// Posto in coda: rilasciato quando il comando è servito.
        _permit: OwnedSemaphorePermit,
    },
    ParentLost {
        policy: OrphanPolicy,
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
            Self::ParentLost { policy, .. } => write!(f, "ParentLost({policy:?})"),
            Self::Shutdown { .. } => write!(f, "Shutdown"),
        }
    }
}

pub(crate) struct SupervisorThread {
    pub(crate) catalog: Box<dyn Catalog>,
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) events: broadcast::Sender<ServiceEvent>,
    pub(crate) max_frame: u32,
}

impl SupervisorThread {
    pub(crate) fn run(mut self, rx: mpsc::Receiver<Command>) {
        let _ = self.events.send(ServiceEvent::ServiceState {
            state: ServiceState::Running,
        });
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::Request {
                    req,
                    revoked,
                    reply,
                    _permit,
                } => {
                    let res = if *revoked.borrow() {
                        Err(op_error(
                            ErrorCode::PermissionDenied,
                            "connessione revocata",
                        ))
                    } else {
                        self.handle(req)
                    };
                    let _ = reply.send(res);
                }
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
                                let _ = reply.send(Err(op_error(
                                    ErrorCode::StateConflict,
                                    "servizio in arresto",
                                )));
                            }
                            Command::ParentLost { done, .. } | Command::Shutdown { done } => {
                                let _ = done.send(());
                            }
                        }
                    }
                    self.backend.stop_agent_sessions(StopReason::Shutdown);
                    self.backend.shutdown();
                    let _ = done.send(());
                    return;
                }
            }
        }
    }

    fn handle(&mut self, req: SupervisorRequest) -> Result<ResponseBody, ReplyError> {
        match req {
            SupervisorRequest::Ping => Ok(ResponseBody::Pong),
            SupervisorRequest::GetCapabilities => Ok(ResponseBody::Capabilities(Capabilities {
                protocol_versions: SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
                backend: self.backend.name(),
                accelerator: self.backend.accelerator(),
                max_frame_bytes: self.max_frame,
            })),
            SupervisorRequest::ListVms => self
                .catalog
                .list_vms()
                .map(|vms| ResponseBody::VmList { vms }),
        }
    }
}

/// Implementazioni finte per lo skeleton e per i test. Non avviano processi.
pub mod fake {
    use super::*;

    /// Registro delle chiamate ricevute dalle implementazioni finte.
    pub type Calls = Arc<Mutex<Vec<String>>>;

    /// Cancello che blocca `list_vms` finché non viene aperto (per i test di coda).
    #[derive(Debug, Default)]
    pub struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
    }

    impl Gate {
        /// Cancello chiuso.
        pub fn closed() -> Arc<Self> {
            Arc::new(Self::default())
        }

        /// Apre il cancello.
        pub fn open(&self) {
            let mut g = self.open.lock().unwrap_or_else(|e| e.into_inner());
            *g = true;
            self.cv.notify_all();
        }

        fn wait(&self) {
            let mut g = self.open.lock().unwrap_or_else(|e| e.into_inner());
            while !*g {
                g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        }
    }

    /// Catalogo finto con VM fisse.
    #[derive(Debug, Default)]
    pub struct FakeCatalog {
        /// VM restituite.
        pub vms: Vec<VmSummary>,
        /// Cancello opzionale su `list_vms`.
        pub gate: Option<Arc<Gate>>,
    }

    impl FakeCatalog {
        /// Catalogo con una VM di esempio spenta.
        pub fn sample() -> Self {
            Self {
                vms: vec![VmSummary {
                    vm_id: "vm-0001".into(),
                    name: "esempio".into(),
                    state: VmState::Stopped,
                }],
                gate: None,
            }
        }
    }

    impl Catalog for FakeCatalog {
        fn list_vms(&mut self) -> Result<Vec<VmSummary>, ReplyError> {
            if let Some(g) = &self.gate {
                g.wait();
            }
            Ok(self.vms.clone())
        }
    }

    /// Backend finto: registra le chiamate.
    #[derive(Debug, Default)]
    pub struct FakeBackend {
        /// Chiamate ricevute.
        pub calls: Calls,
    }

    #[derive(Debug)]
    struct FakeEmergency {
        calls: Calls,
    }

    impl EmergencyStop for FakeEmergency {
        fn emergency_stop_all(&self) -> EmergencyOutcome {
            push(&self.calls, "emergency_stop_all");
            EmergencyOutcome {
                processes_terminated: 0,
                completed: true,
            }
        }
    }

    fn push(calls: &Calls, s: impl Into<String>) {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(s.into());
    }

    impl Backend for FakeBackend {
        fn name(&self) -> String {
            "fake".into()
        }
        fn accelerator(&self) -> Option<String> {
            None
        }
        fn emergency_handle(&self) -> Arc<dyn EmergencyStop> {
            Arc::new(FakeEmergency {
                calls: self.calls.clone(),
            })
        }
        fn stop_agent_sessions(&mut self, reason: StopReason) {
            push(&self.calls, format!("stop_agent_sessions:{reason:?}"));
        }
        fn on_parent_lost(&mut self, policy: OrphanPolicy) {
            push(&self.calls, format!("on_parent_lost:{policy:?}"));
        }
        fn shutdown(&mut self) {
            push(&self.calls, "shutdown");
        }
    }
}
