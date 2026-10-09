//! Errori del backend e corrispondenza con gli errori pubblici stabili
//! (ROADMAP 6.3: `permission_denied`, `session_expired`, `vm_busy`,
//! `unsupported`, `resource_limit`, `state_conflict`).

use std::fmt;
use std::io;

use crate::probe::AcceleratorStatus;
use crate::qmp::QmpError;

/// Categoria pubblica stabile di un errore.
///
/// TODO(F1): sostituire con il tipo di `lightvm-proto` quando sarà
/// disponibile (vedi `compat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PublicErrorKind {
    /// Operazione non autorizzata o percorso/parametro rifiutato.
    PermissionDenied,
    /// Sessione scaduta (non prodotto da questo crate, presente per completezza).
    SessionExpired,
    /// Risorsa occupata, per esempio file della VM aperto da un altro processo.
    VmBusy,
    /// Capacità non disponibile sull'host (acceleratore, dispositivo, firmware).
    Unsupported,
    /// Limite di risorse superato.
    ResourceLimit,
    /// Stato incompatibile con l'operazione.
    StateConflict,
}

impl PublicErrorKind {
    /// Codice stabile in `snake_case`.
    pub fn code(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::SessionExpired => "session_expired",
            Self::VmBusy => "vm_busy",
            Self::Unsupported => "unsupported",
            Self::ResourceLimit => "resource_limit",
            Self::StateConflict => "state_conflict",
        }
    }
}

/// Errore del backend QEMU.
#[derive(Debug)]
#[non_exhaustive]
pub enum BackendError {
    /// Parametro rifiutato dalla validazione (grammatica, percorso, intervallo).
    InvalidArgument {
        /// Campo interessato.
        field: &'static str,
        /// Motivo leggibile.
        reason: String,
    },
    /// Capacità non disponibile sull'host.
    Unsupported {
        /// Che cosa manca.
        what: String,
        /// Suggerimento per l'utente.
        hint: String,
    },
    /// Acceleratore non disponibile: nessun fallback TCG (ADR-0007).
    AcceleratorUnavailable(AcceleratorStatus),
    /// Probe non riuscito (binario non eseguibile, output non riconosciuto).
    Probe(String),
    /// Avvio del processo non riuscito.
    Spawn(io::Error),
    /// QEMU è uscito durante l'avvio.
    StartupFailed {
        /// Codice d'uscita, se noto.
        exit_code: Option<u32>,
        /// Ultime righe di stderr.
        stderr_tail: String,
    },
    /// Errore del canale QMP.
    Qmp(QmpError),
    /// Stato incompatibile con l'operazione richiesta.
    StateConflict(String),
    /// File della VM in uso da un altro processo.
    VmBusy(String),
    /// Limite superato (dimensione screenshot, memoria, vCPU).
    ResourceLimit(String),
    /// Operazione non conclusa entro il tempo massimo.
    Timeout(&'static str),
    /// Errore di I/O.
    Io(io::Error),
}

impl BackendError {
    /// Crea un errore di validazione.
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field,
            reason: reason.into(),
        }
    }

    /// Categoria pubblica stabile.
    pub fn public_kind(&self) -> PublicErrorKind {
        match self {
            Self::InvalidArgument { .. } => PublicErrorKind::PermissionDenied,
            Self::Unsupported { .. } | Self::AcceleratorUnavailable(_) | Self::Probe(_) => {
                PublicErrorKind::Unsupported
            }
            Self::VmBusy(_) => PublicErrorKind::VmBusy,
            Self::ResourceLimit(_) => PublicErrorKind::ResourceLimit,
            Self::StateConflict(_)
            | Self::Spawn(_)
            | Self::StartupFailed { .. }
            | Self::Qmp(_)
            | Self::Timeout(_)
            | Self::Io(_) => PublicErrorKind::StateConflict,
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument { field, reason } => {
                write!(f, "parametro `{field}` rifiutato: {reason}")
            }
            Self::Unsupported { what, hint } => write!(f, "non supportato: {what}. {hint}"),
            Self::AcceleratorUnavailable(status) => write!(f, "{status}"),
            Self::Probe(m) => write!(f, "rilevamento delle capacità QEMU non riuscito: {m}"),
            Self::Spawn(e) => write!(f, "avvio di QEMU non riuscito: {e}"),
            Self::StartupFailed {
                exit_code,
                stderr_tail,
            } => {
                write!(f, "QEMU è terminato durante l'avvio")?;
                if let Some(c) = exit_code {
                    write!(f, " (codice {c:#x})")?;
                }
                if !stderr_tail.is_empty() {
                    write!(f, ": {stderr_tail}")?;
                }
                Ok(())
            }
            Self::Qmp(e) => write!(f, "{e}"),
            Self::StateConflict(m) => write!(f, "stato incompatibile: {m}"),
            Self::VmBusy(m) => write!(f, "risorsa occupata: {m}"),
            Self::ResourceLimit(m) => write!(f, "limite superato: {m}"),
            Self::Timeout(what) => write!(f, "tempo scaduto: {what}"),
            Self::Io(e) => write!(f, "errore di I/O: {e}"),
        }
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(e) | Self::Io(e) => Some(e),
            Self::Qmp(e) => Some(e),
            _ => None,
        }
    }
}

impl From<QmpError> for BackendError {
    fn from(e: QmpError) -> Self {
        Self::Qmp(e)
    }
}

impl From<io::Error> for BackendError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Risultato del backend.
pub type Result<T, E = BackendError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codici_pubblici_stabili() {
        assert_eq!(
            BackendError::invalid("x", "y").public_kind().code(),
            "permission_denied"
        );
        assert_eq!(
            BackendError::VmBusy("a".into()).public_kind().code(),
            "vm_busy"
        );
        assert_eq!(
            BackendError::Unsupported {
                what: "w".into(),
                hint: "h".into()
            }
            .public_kind()
            .code(),
            "unsupported"
        );
    }
}
