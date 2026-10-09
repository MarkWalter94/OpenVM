//! Errori pubblici stabili (ROADMAP 6.3, 9).
//!
//! Due famiglie separate:
//! - [`PublicError`]: errore **operativo** di una richiesta ben formata (permessi,
//!   stato, limiti). Il codice è stabile e fa parte del contratto pubblico.
//! - [`ProtocolError`]: la richiesta non è stata compresa (formato, versione,
//!   dimensione). Nessun effetto è stato tentato.
//!
//! I messaggi sono per persone e non contengono segreti né percorsi host; i
//! client devono decidere in base a `code` e `retryable`, mai al testo.
//!
//! # Autenticazione e autorizzazione
//!
//! Tre codici operativi distinti, valutati in quest'ordine:
//!
//! | Codice | Significato | Esempi |
//! | --- | --- | --- |
//! | [`ErrorCode::Unauthenticated`] | nessuna credenziale valida: il chiamante non è identificato | credenziale IPC o token di sessione assente, malformato, sconosciuto, revocato o riusato dopo un riavvio del supervisor; richiesta prima dell'handshake |
//! | [`ErrorCode::SessionExpired`] | credenziale riconosciuta ma scaduta (grant o lease) | lease non rinnovato entro 10 s, grant oltre la scadenza |
//! | [`ErrorCode::PermissionDenied`] | chiamante identificato ma non autorizzato | VM fuori allowlist, capability mancante nella policy effettiva, sessione in pausa per le mutazioni, ruolo IPC insufficiente, creazione VM da agente, supporto diverso da quello approvato |
//!
//! Dentro `unauthenticated` i motivi non sono distinti verso il client (stesso
//! codice e stesso messaggio per token assente, errato o revocato), per non
//! rivelare quali sessioni esistono. Il codice di protocollo
//! [`ProtocolErrorCode::Unauthenticated`] resta nel contratto per compatibilità
//! ma non si emette per le credenziali: una credenziale rifiutata è l'esito
//! operativo di una richiesta compresa, quindi si usa [`ErrorCode::Unauthenticated`].

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Codice stabile di un errore operativo. I nomi serializzati non cambiano mai;
/// nuovi codici si aggiungono soltanto.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Chiamante identificato (credenziale valida) ma non autorizzato all'azione:
    /// VM fuori allowlist, capability mancante, sessione in pausa, ruolo
    /// insufficiente. Credenziali assenti o non valide: [`ErrorCode::Unauthenticated`].
    PermissionDenied,
    /// Sessione o lease scaduti: serve una nuova autorizzazione nell'app.
    SessionExpired,
    /// Un'altra mutazione è in corso o in coda sulla stessa VM: ritentabile.
    VmBusy,
    /// Capacità non disponibile su questo host, build o fase.
    Unsupported,
    /// L'azione non è ammessa nello stato corrente (o riferisce un `run_id` superato).
    StateConflict,
    /// Limite di risorse, quota o dimensione superato.
    ResourceLimit,
    /// Argomento non valido (formato, intervallo, nome, percorso rifiutato).
    InvalidArgument,
    /// Oggetto inesistente o non visibile al chiamante.
    NotFound,
    /// Guasto dell'infrastruttura (QEMU, QMP, storage, OS) non imputabile al chiamante.
    InfrastructureError,
    /// Nessuna credenziale valida: credenziale IPC o token di sessione assente,
    /// malformato, sconosciuto, revocato o riusato dopo un riavvio del supervisor.
    /// Serve una nuova autorizzazione nell'app. Aggiunto in coda ai primi nove
    /// codici, che restano invariati.
    Unauthenticated,
}

impl ErrorCode {
    /// Tutti i codici, nell'ordine congelato (i nuovi codici si aggiungono in coda).
    pub const ALL: [ErrorCode; 10] = [
        ErrorCode::PermissionDenied,
        ErrorCode::SessionExpired,
        ErrorCode::VmBusy,
        ErrorCode::Unsupported,
        ErrorCode::StateConflict,
        ErrorCode::ResourceLimit,
        ErrorCode::InvalidArgument,
        ErrorCode::NotFound,
        ErrorCode::InfrastructureError,
        ErrorCode::Unauthenticated,
    ];

    /// Nome stabile (uguale alla serializzazione).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::SessionExpired => "session_expired",
            ErrorCode::VmBusy => "vm_busy",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::StateConflict => "state_conflict",
            ErrorCode::ResourceLimit => "resource_limit",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::NotFound => "not_found",
            ErrorCode::InfrastructureError => "infrastructure_error",
            ErrorCode::Unauthenticated => "unauthenticated",
        }
    }

    /// Valore predefinito di `retryable`: ritentare la *stessa* richiesta con lo
    /// stesso `operation_id` può riuscire. Vale solo per operazioni idempotenti:
    /// gli input GUI non si ripetono mai automaticamente (ROADMAP 6.3).
    #[must_use]
    pub const fn default_retryable(self) -> bool {
        matches!(self, ErrorCode::VmBusy | ErrorCode::InfrastructureError)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errore operativo pubblico.
///
/// Forma JSON: `{"code": "...", "message": "...", "details": {..}?, "retryable": bool}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicError {
    /// Codice stabile.
    pub code: ErrorCode,
    /// Messaggio per persone, senza segreti né percorsi host.
    pub message: String,
    /// Dettagli strutturati opzionali (chiavi e valori testuali, senza segreti).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, String>>,
    /// `true` se ritentare la stessa richiesta idempotente può riuscire.
    pub retryable: bool,
}

impl PublicError {
    /// Nuovo errore con `retryable` predefinito per il codice.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            retryable: code.default_retryable(),
        }
    }

    /// Aggiunge un dettaglio `chiave = valore`.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }

    /// Imposta esplicitamente `retryable`.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Scorciatoia per `permission_denied`.
    #[must_use]
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }
    /// Scorciatoia per `session_expired`.
    #[must_use]
    pub fn session_expired(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionExpired, message)
    }
    /// Scorciatoia per `vm_busy`.
    #[must_use]
    pub fn vm_busy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::VmBusy, message)
    }
    /// Scorciatoia per `unsupported`.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unsupported, message)
    }
    /// Scorciatoia per `state_conflict`.
    #[must_use]
    pub fn state_conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::StateConflict, message)
    }
    /// Scorciatoia per `resource_limit`.
    #[must_use]
    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ResourceLimit, message)
    }
    /// Scorciatoia per `invalid_argument`.
    #[must_use]
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }
    /// Scorciatoia per `not_found`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
    /// Scorciatoia per `infrastructure_error`.
    #[must_use]
    pub fn infrastructure(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InfrastructureError, message)
    }
    /// Scorciatoia per `unauthenticated`.
    #[must_use]
    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthenticated, message)
    }
}

impl fmt::Display for PublicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PublicError {}

/// Codice di un errore di protocollo (richiesta non compresa, nessun effetto tentato).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolErrorCode {
    /// Messaggio non decodificabile o non conforme allo schema.
    MalformedMessage,
    /// Versione di protocollo non supportata.
    UnsupportedVersion,
    /// Metodo o tipo di richiesta sconosciuto.
    UnknownMethod,
    /// Messaggio oltre la dimensione massima ammessa.
    MessageTooLarge,
    /// Mantenuto per compatibilità del contratto ma **non emesso**: una credenziale
    /// assente o non valida produce l'errore operativo [`ErrorCode::Unauthenticated`]
    /// (vedi la documentazione del modulo).
    Unauthenticated,
}

impl ProtocolErrorCode {
    /// Tutti i codici, nell'ordine congelato.
    pub const ALL: [ProtocolErrorCode; 5] = [
        ProtocolErrorCode::MalformedMessage,
        ProtocolErrorCode::UnsupportedVersion,
        ProtocolErrorCode::UnknownMethod,
        ProtocolErrorCode::MessageTooLarge,
        ProtocolErrorCode::Unauthenticated,
    ];

    /// Nome stabile (uguale alla serializzazione).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ProtocolErrorCode::MalformedMessage => "malformed_message",
            ProtocolErrorCode::UnsupportedVersion => "unsupported_version",
            ProtocolErrorCode::UnknownMethod => "unknown_method",
            ProtocolErrorCode::MessageTooLarge => "message_too_large",
            ProtocolErrorCode::Unauthenticated => "unauthenticated",
        }
    }
}

/// Errore di protocollo: distinto dagli errori operativi (ROADMAP 9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolError {
    /// Codice stabile.
    pub code: ProtocolErrorCode,
    /// Messaggio per persone, senza segreti.
    pub message: String,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ProtocolError {}

/// Errore di risposta: operativo oppure di protocollo, mai confusi.
///
/// Forma JSON: `{"kind": "operational", ...PublicError}` oppure
/// `{"kind": "protocol", ...ProtocolError}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyError {
    /// Richiesta compresa ma rifiutata o fallita.
    Operational(PublicError),
    /// Richiesta non compresa: nessun effetto tentato.
    Protocol(ProtocolError),
}

impl From<PublicError> for ReplyError {
    fn from(e: PublicError) -> Self {
        Self::Operational(e)
    }
}

impl From<ProtocolError> for ReplyError {
    fn from(e: ProtocolError) -> Self {
        Self::Protocol(e)
    }
}
