//! Richieste tipizzate verso la coda per VM (ROADMAP 6.3, 7.3).
//!
//! Questi tipi sono il contenuto delle richieste IPC di controllo VM; l'involucro
//! di trasporto (versione, autenticazione del canale, correlazione) appartiene a
//! `lightvm-service`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Capability, InputBatch, OperationId, OperationKind, OperationStatus, RunId, SessionId,
    SessionToken, VmId,
};

/// Chi richiede un'azione.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caller {
    /// Utente locale tramite la UI (soggetto a limiti globali, policy VM e capacità host).
    User,
    /// Agente con sessione: soggetto anche al grant. Il token è verificato a ogni
    /// chiamata e subito prima dell'inoltro.
    Agent {
        /// Sessione dichiarata.
        session_id: SessionId,
        /// Segreto di sessione; assente = diniego `unauthenticated`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<SessionToken>,
    },
}

/// Attore registrato nell'audit (senza segreti).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    /// Utente locale.
    User,
    /// Sessione agente.
    Agent {
        /// Sessione.
        session_id: SessionId,
    },
    /// Supervisor (scadenze, riconciliazione, osservazioni).
    System,
}

impl Caller {
    /// Attore corrispondente, senza il segreto.
    #[must_use]
    pub fn actor(&self) -> Actor {
        match self {
            Caller::User => Actor::User,
            Caller::Agent { session_id, .. } => Actor::Agent {
                session_id: *session_id,
            },
        }
    }
}

/// Azione richiesta su una VM esistente.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VmAction {
    /// Avvio.
    Start,
    /// Arresto ordinario.
    Stop,
    /// Arresto forzato con primitive OS.
    ForceStop,
    /// Reset del guest.
    Reset,
    /// Screenshot.
    Screenshot,
    /// Input tipizzato.
    Input(InputBatch),
}

impl VmAction {
    /// Tipo di operazione.
    #[must_use]
    pub fn kind(&self) -> OperationKind {
        match self {
            VmAction::Start => OperationKind::Start,
            VmAction::Stop => OperationKind::Stop,
            VmAction::ForceStop => OperationKind::ForceStop,
            VmAction::Reset => OperationKind::Reset,
            VmAction::Screenshot => OperationKind::Screenshot,
            VmAction::Input(_) => OperationKind::Input,
        }
    }

    /// Capability richiesta.
    #[must_use]
    pub fn capability(&self) -> Capability {
        match self {
            VmAction::Start => Capability::VmStart,
            VmAction::Stop | VmAction::ForceStop => Capability::VmStop,
            VmAction::Reset => Capability::VmReset,
            VmAction::Screenshot => Capability::VmScreenshot,
            VmAction::Input(_) => Capability::VmInput,
        }
    }
}

/// Richiesta di azione su una VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmRequest {
    /// Chiave di idempotenza scelta dal chiamante: la stessa richiesta ripetuta con
    /// lo stesso `operation_id` non duplica l'effetto.
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Esecuzione attesa: obbligatoria per reset, screenshot e input; se presente
    /// e diversa dall'esecuzione corrente la richiesta è rifiutata (`state_conflict`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_run_id: Option<RunId>,
    /// Azione (oggetto con discriminante `type`).
    pub action: VmAction,
}

/// Esito dell'accettazione di una richiesta (prima dell'inoltro).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Admission {
    /// Operazione.
    pub operation_id: OperationId,
    /// Stato corrente dell'operazione.
    pub status: OperationStatus,
    /// `true` se la richiesta ripete un `operation_id` già noto (nessun nuovo effetto).
    pub duplicate: bool,
}
