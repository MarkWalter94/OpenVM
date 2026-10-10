//! Eventi di audit minimi (ROADMAP 7.2 punto 9, 9; F1: creazione, avvio,
//! arresto e dinieghi). Solo tipo azione, identificativi, esito e ordine: nessun
//! payload sensibile (testo digitato, token, percorsi host, contenuto guest).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Actor, Capability, ErrorCode, OperationId, RunId, SessionId, TerminationCause, UtcTimestamp,
    VmId,
};

/// Tipo di azione registrata.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// Creazione VM.
    VmCreate,
    /// Avvio VM (richiesta inoltrata o esito).
    VmStart,
    /// Arresto VM (ordinario o forzato).
    VmStop,
    /// Reset VM.
    VmReset,
    /// Screenshot.
    VmScreenshot,
    /// Input (solo conteggio implicito, mai il contenuto).
    VmInput,
    /// Cambio di stato osservato (processo/QMP).
    VmStateObserved,
    /// Emergency Stop.
    EmergencyStop,
    /// Creazione sessione.
    SessionCreate,
    /// Pausa sessione.
    SessionPause,
    /// Ripresa sessione.
    SessionResume,
    /// Terminazione sessione (revoca, scadenza...).
    SessionTerminate,
    /// Operazione scartata prima dell'inoltro.
    OperationDiscard,
    /// Modifica della configurazione di una VM (risorse e limiti).
    VmConfigure,
}

/// Esito registrato.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Accettata (in coda).
    Accepted,
    /// Inoltrata al backend.
    Forwarded,
    /// Completata.
    Succeeded,
    /// Fallita.
    Failed,
    /// Negata: nessun effetto.
    Denied,
    /// Scartata prima dell'inoltro: nessun effetto.
    Discarded,
}

/// Evento di audit. `seq` è l'ordine totale assegnato dal core, strettamente crescente.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEvent {
    /// Ordine (da 1, strettamente crescente).
    pub seq: u64,
    /// Istante UTC.
    pub at: UtcTimestamp,
    /// Attore.
    pub actor: Actor,
    /// Azione.
    pub action: AuditAction,
    /// Esito.
    pub outcome: AuditOutcome,
    /// VM coinvolta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_id: Option<VmId>,
    /// Esecuzione coinvolta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Operazione coinvolta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
    /// Sessione coinvolta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Capability valutata (per i dinieghi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    /// Codice d'errore pubblico (per dinieghi e fallimenti).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
    /// Causa di terminazione (per `session_terminate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationCause>,
}
