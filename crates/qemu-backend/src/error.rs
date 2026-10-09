//! Errori del backend e corrispondenza con gli errori pubblici stabili di
//! `lightvm-proto` ([`ErrorCode`], [`PublicError`]; ROADMAP 6.3).
//!
//! Il messaggio pubblico non contiene percorsi host né estratti di stderr che
//! potrebbero contenerli: il dettaglio completo resta nel `Display` di
//! [`BackendError`], destinato ai log locali del supervisor.

use std::fmt;
use std::io;

use lightvm_proto::{ErrorCode, PublicError};

use crate::probe::AcceleratorStatus;
use crate::qmp::QmpError;

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
    /// Binario QEMU con hash SHA-256 assente dalla lista approvata (ADR-0010).
    UnapprovedBinary {
        /// Binario (`qemu-system-x86_64` o `qemu-img`).
        binary: &'static str,
        /// SHA-256 calcolato.
        sha256: String,
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
    /// Interfaccia di rete inattesa nel guest (profilo offline, ADR-0004).
    UnexpectedNetwork(String),
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

    /// Codice pubblico stabile.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidArgument { .. } => ErrorCode::InvalidArgument,
            Self::Unsupported { .. }
            | Self::UnapprovedBinary { .. }
            | Self::AcceleratorUnavailable(_)
            | Self::Probe(_) => ErrorCode::Unsupported,
            Self::VmBusy(_) => ErrorCode::VmBusy,
            Self::ResourceLimit(_) => ErrorCode::ResourceLimit,
            Self::StateConflict(_) => ErrorCode::StateConflict,
            Self::Spawn(_)
            | Self::StartupFailed { .. }
            | Self::UnexpectedNetwork(_)
            | Self::Qmp(_)
            | Self::Timeout(_)
            | Self::Io(_) => ErrorCode::InfrastructureError,
        }
    }

    /// Errore pubblico stabile, senza percorsi host né estratti di stderr.
    pub fn to_public(&self) -> PublicError {
        let code = self.code();
        let msg = match self {
            Self::InvalidArgument { field, reason } => {
                format!("parametro `{field}` rifiutato: {reason}")
            }
            Self::Unsupported { what, hint } => format!("non supportato: {what}. {hint}"),
            Self::UnapprovedBinary { binary, .. } => format!(
                "il binario {binary} non corrisponde a una build QEMU approvata (hash SHA-256 \
                 diverso da quelli registrati): avvio rifiutato"
            ),
            Self::AcceleratorUnavailable(status) => status.public_message(),
            Self::Probe(_) => "rilevamento delle capacità QEMU non riuscito".to_owned(),
            Self::Spawn(_) => "avvio del processo QEMU non riuscito".to_owned(),
            Self::StartupFailed { exit_code, .. } => match exit_code {
                Some(c) => format!("QEMU è terminato durante l'avvio (codice {c:#x})"),
                None => "QEMU non ha completato l'avvio".to_owned(),
            },
            Self::UnexpectedNetwork(_) => {
                "interfaccia di rete inattesa nel guest: avvio interrotto (profilo offline)"
                    .to_owned()
            }
            Self::Qmp(e) => format!("canale di controllo QEMU: {}", e.public_summary()),
            Self::StateConflict(m) => format!("stato incompatibile: {m}"),
            Self::VmBusy(_) => "file della VM in uso da un altro processo".to_owned(),
            Self::ResourceLimit(m) => format!("limite superato: {m}"),
            Self::Timeout(what) => format!("tempo scaduto: {what}"),
            Self::Io(_) => "errore di I/O del backend QEMU".to_owned(),
        };
        let retryable = matches!(self, Self::VmBusy(_) | Self::Timeout(_));
        PublicError::new(code, msg).with_retryable(retryable)
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument { field, reason } => {
                write!(f, "parametro `{field}` rifiutato: {reason}")
            }
            Self::Unsupported { what, hint } => write!(f, "non supportato: {what}. {hint}"),
            Self::UnapprovedBinary { binary, sha256 } => write!(
                f,
                "binario {binary} non approvato: SHA-256 {sha256} assente dalla lista approvata"
            ),
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
            Self::UnexpectedNetwork(m) => write!(f, "interfaccia di rete inattesa: {m}"),
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

impl From<PublicError> for BackendError {
    /// Errori di validazione dei tipi di `lightvm-proto` (geometria, coordinate).
    fn from(e: PublicError) -> Self {
        match e.code {
            ErrorCode::ResourceLimit => Self::ResourceLimit(e.message),
            ErrorCode::StateConflict => Self::StateConflict(e.message),
            _ => Self::invalid("input", e.message),
        }
    }
}

impl From<&BackendError> for PublicError {
    fn from(e: &BackendError) -> Self {
        e.to_public()
    }
}

/// Risultato del backend.
pub type Result<T, E = BackendError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmdline::Accelerator;
    use crate::probe::UnavailableReason;

    #[test]
    fn codici_pubblici_stabili() {
        assert_eq!(
            BackendError::invalid("x", "y").code().as_str(),
            "invalid_argument"
        );
        assert_eq!(BackendError::VmBusy("a".into()).code().as_str(), "vm_busy");
        assert_eq!(
            BackendError::Unsupported {
                what: "w".into(),
                hint: "h".into()
            }
            .code()
            .as_str(),
            "unsupported"
        );
        assert_eq!(
            BackendError::Timeout("t").code(),
            ErrorCode::InfrastructureError
        );
    }

    #[test]
    fn messaggio_pubblico_senza_percorsi() {
        let e = BackendError::VmBusy(r"`E:\segreto\disco.qcow2` aperto".into());
        let p = e.to_public();
        assert_eq!(p.code, ErrorCode::VmBusy);
        assert!(!p.message.contains("segreto"));
        assert!(p.retryable);
        let e = BackendError::StartupFailed {
            exit_code: Some(1),
            stderr_tail: r"C:\Users\x\qemu.exe: errore".into(),
        };
        assert!(!e.to_public().message.contains("Users"));
        let e = BackendError::UnapprovedBinary {
            binary: "qemu-system-x86_64",
            sha256: "00".repeat(32),
        };
        assert_eq!(e.to_public().code, ErrorCode::Unsupported);
    }

    #[test]
    fn acceleratore_assente_errore_chiaro() {
        let e = BackendError::AcceleratorUnavailable(AcceleratorStatus::Unavailable {
            accelerator: Accelerator::Whpx,
            reason: UnavailableReason::NotCompiled {
                available: vec!["tcg".into()],
            },
        });
        let p = e.to_public();
        assert_eq!(p.code, ErrorCode::Unsupported);
        assert!(p.message.contains("WHPX"), "{}", p.message);
        assert!(p.message.contains("TCG"), "{}", p.message);
    }
}
