//! Backend errors and their mapping to the stable public errors of
//! `lightvm-proto` ([`ErrorCode`], [`PublicError`]; ROADMAP 6.3).
//!
//! The public message contains neither host paths nor stderr excerpts that
//! could contain them: the full detail stays in the `Display` of
//! [`BackendError`], meant for the supervisor's local logs.

use std::fmt;
use std::io;

use lightvm_proto::{ErrorCode, PublicError};

use crate::probe::AcceleratorStatus;
use crate::qmp::QmpError;

/// QEMU backend error.
#[derive(Debug)]
#[non_exhaustive]
pub enum BackendError {
    /// Parameter rejected by validation (grammar, path, range).
    InvalidArgument {
        /// Field concerned.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// Capability not available on the host.
    Unsupported {
        /// What is missing.
        what: String,
        /// Hint for the user.
        hint: String,
    },
    /// QEMU binary whose SHA-256 hash is not in the approved list (ADR-0010).
    UnapprovedBinary {
        /// Binary (`qemu-system-x86_64` or `qemu-img`).
        binary: &'static str,
        /// Computed SHA-256.
        sha256: String,
    },
    /// Accelerator not available: no TCG fallback (ADR-0007).
    AcceleratorUnavailable(AcceleratorStatus),
    /// Probe failed (binary not executable, unrecognised output).
    Probe(String),
    /// Process spawn failed.
    Spawn(io::Error),
    /// QEMU exited during startup.
    StartupFailed {
        /// Exit code, if known.
        exit_code: Option<u32>,
        /// Last lines of stderr.
        stderr_tail: String,
    },
    /// Unexpected network interface in the guest (offline profile, ADR-0004).
    UnexpectedNetwork(String),
    /// QMP channel error.
    Qmp(QmpError),
    /// State incompatible with the requested operation.
    StateConflict(String),
    /// VM file in use by another process.
    VmBusy(String),
    /// Limit exceeded (screenshot size, memory, vCPUs).
    ResourceLimit(String),
    /// Operation not completed within the maximum time.
    Timeout(&'static str),
    /// I/O error.
    Io(io::Error),
}

impl BackendError {
    /// Creates a validation error.
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field,
            reason: reason.into(),
        }
    }

    /// Stable public code.
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

    /// Stable public error, without host paths or stderr excerpts.
    pub fn to_public(&self) -> PublicError {
        let code = self.code();
        let msg = match self {
            Self::InvalidArgument { field, reason } => {
                format!("parameter `{field}` rejected: {reason}")
            }
            Self::Unsupported { what, hint } => format!("not supported: {what}. {hint}"),
            Self::UnapprovedBinary { binary, .. } => format!(
                "the {binary} binary does not match an approved QEMU build (SHA-256 hash \
                 differs from the registered ones): launch refused"
            ),
            Self::AcceleratorUnavailable(status) => status.public_message(),
            Self::Probe(_) => "QEMU capability detection failed".to_owned(),
            Self::Spawn(_) => "failed to start the QEMU process".to_owned(),
            Self::StartupFailed { exit_code, .. } => match exit_code {
                Some(c) => format!("QEMU terminated during startup (code {c:#x})"),
                None => "QEMU did not complete startup".to_owned(),
            },
            Self::UnexpectedNetwork(_) => {
                "unexpected network interface in the guest: launch aborted (offline profile)"
                    .to_owned()
            }
            Self::Qmp(e) => format!("QEMU control channel: {}", e.public_summary()),
            Self::StateConflict(m) => format!("incompatible state: {m}"),
            Self::VmBusy(_) => "VM file in use by another process".to_owned(),
            Self::ResourceLimit(m) => format!("limit exceeded: {m}"),
            Self::Timeout(what) => format!("timed out: {what}"),
            Self::Io(_) => "QEMU backend I/O error".to_owned(),
        };
        let retryable = matches!(self, Self::VmBusy(_) | Self::Timeout(_));
        PublicError::new(code, msg).with_retryable(retryable)
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument { field, reason } => {
                write!(f, "parameter `{field}` rejected: {reason}")
            }
            Self::Unsupported { what, hint } => write!(f, "not supported: {what}. {hint}"),
            Self::UnapprovedBinary { binary, sha256 } => write!(
                f,
                "binary {binary} not approved: SHA-256 {sha256} not in the approved list"
            ),
            Self::AcceleratorUnavailable(status) => write!(f, "{status}"),
            Self::Probe(m) => write!(f, "QEMU capability detection failed: {m}"),
            Self::Spawn(e) => write!(f, "failed to start QEMU: {e}"),
            Self::StartupFailed {
                exit_code,
                stderr_tail,
            } => {
                write!(f, "QEMU terminated during startup")?;
                if let Some(c) = exit_code {
                    write!(f, " (code {c:#x})")?;
                }
                if !stderr_tail.is_empty() {
                    write!(f, ": {stderr_tail}")?;
                }
                Ok(())
            }
            Self::UnexpectedNetwork(m) => write!(f, "unexpected network interface: {m}"),
            Self::Qmp(e) => write!(f, "{e}"),
            Self::StateConflict(m) => write!(f, "incompatible state: {m}"),
            Self::VmBusy(m) => write!(f, "resource busy: {m}"),
            Self::ResourceLimit(m) => write!(f, "limit exceeded: {m}"),
            Self::Timeout(what) => write!(f, "timed out: {what}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
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
    /// Validation errors of the `lightvm-proto` types (geometry, coordinates).
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

/// Backend result.
pub type Result<T, E = BackendError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmdline::Accelerator;
    use crate::probe::UnavailableReason;

    #[test]
    fn stable_public_codes() {
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
    fn public_message_without_paths() {
        let e = BackendError::VmBusy(r"`E:\secret\disk.qcow2` open".into());
        let p = e.to_public();
        assert_eq!(p.code, ErrorCode::VmBusy);
        assert!(!p.message.contains("secret"));
        assert!(p.retryable);
        let e = BackendError::StartupFailed {
            exit_code: Some(1),
            stderr_tail: r"C:\Users\x\qemu.exe: error".into(),
        };
        assert!(!e.to_public().message.contains("Users"));
        let e = BackendError::UnapprovedBinary {
            binary: "qemu-system-x86_64",
            sha256: "00".repeat(32),
        };
        assert_eq!(e.to_public().code, ErrorCode::Unsupported);
    }

    #[test]
    fn missing_accelerator_clear_error() {
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
