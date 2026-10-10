//! Catalog errors.
//!
//! Errors are specific to storage; translation into the stable public errors
//! (`vm_busy`, `state_conflict`, `resource_limit`, ...) happens in the
//! supervisor through [`StorageError::public_code`].

use std::path::PathBuf;

/// Result with [`StorageError`].
pub type Result<T> = std::result::Result<T, StorageError>;

/// Reason why a managed path was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRejection {
    /// Empty string.
    Empty,
    /// Absolute or rooted path (`/x`, `\x`, `C:\x`, `C:x`).
    Absolute,
    /// UNC path (`\\server\share`) or device path (`\\?\`, `\\.\`).
    UncOrDevice,
    /// `..` or `.` component.
    Traversal,
    /// Windows reserved name (`CON`, `NUL`, `COM1`, ...).
    ReservedName,
    /// Disallowed character (control, `<>:"|?*`, backslash).
    InvalidCharacter,
    /// Component ending with a dot or space (normalized by Windows).
    TrailingDotOrSpace,
    /// Path too long.
    TooLong,
    /// An existing component is a symbolic link or a reparse point
    /// (junctions included).
    Link,
    /// The path is not under the expected directory (for example `tmp/`).
    OutsideArea,
}

/// Storage error.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// SQLite error.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// I/O error on a managed path.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Original error.
        source: std::io::Error,
    },
    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Another instance already holds the catalog lock.
    #[error("catalog already in use by another instance: {0}")]
    CatalogLocked(PathBuf),
    /// The database has a schema version newer than the supported one.
    #[error(
        "catalog schema v{found} newer than the supported version v{supported}: downgrade not allowed"
    )]
    SchemaTooNew {
        /// Version found in the database.
        found: u32,
        /// Highest version known to this binary.
        supported: u32,
    },
    /// An already applied migration does not match the embedded one.
    #[error("migration v{version} applied with content different from the embedded one")]
    MigrationMismatch {
        /// Diverging version.
        version: u32,
    },
    /// Managed path rejected.
    #[error("managed path rejected ({reason:?}): {path}")]
    InvalidPath {
        /// Proposed path (never read nor opened).
        path: String,
        /// Reason.
        reason: PathRejection,
    },
    /// Deletion prevented by a reference (baseline, disk, media).
    #[error("object is referenced, operation prevented: {0}")]
    Referenced(&'static str),
    /// Violation of a catalog rule enforced by the database.
    #[error("catalog constraint violated: {0}")]
    Constraint(String),
    /// Object not found.
    #[error("{kind} not found: {id}")]
    NotFound {
        /// Object kind.
        kind: &'static str,
        /// Identifier.
        id: String,
    },
    /// Another journaled operation is in progress on the same VM.
    #[error("operation in progress on VM {0}")]
    VmBusy(String),
    /// Transition not allowed from the current state.
    #[error("incompatible state: {0}")]
    StateConflict(String),
    /// Storage budget or host reserve exceeded.
    #[error("storage limit: {0}")]
    ResourceLimit(String),
    /// Required capability not available (for example a hard OS quota).
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    /// The content of a managed file does not match what was recorded
    /// (hash or size): baseline or media altered outside the catalog.
    #[error("integrity check failed for {path}: {detail}")]
    IntegrityMismatch {
        /// Managed path.
        path: String,
        /// Detail (expected and observed value).
        detail: String,
    },
}

impl StorageError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid_path(path: impl Into<String>, reason: PathRejection) -> Self {
        Self::InvalidPath {
            path: path.into(),
            reason,
        }
    }

    /// Closest stable public code (ROADMAP 6.3), if any.
    /// Internal errors (I/O, SQLite, migrations) have no public code.
    pub fn public_code(&self) -> Option<&'static str> {
        match self {
            Self::VmBusy(_) | Self::CatalogLocked(_) => Some("vm_busy"),
            Self::StateConflict(_)
            | Self::Referenced(_)
            | Self::Constraint(_)
            | Self::IntegrityMismatch { .. } => Some("state_conflict"),
            Self::ResourceLimit(_) => Some("resource_limit"),
            Self::Unsupported(_) => Some("unsupported"),
            Self::InvalidPath { .. } => Some("permission_denied"),
            _ => None,
        }
    }
}

/// Converts the triggers' `RAISE(ABORT, 'lightvm:...')` errors into named errors.
pub(crate) fn map_sqlite(err: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::SqliteFailure(code, Some(msg)) = &err {
        if let Some(tag) = msg.strip_prefix("lightvm:") {
            return match tag {
                "baseline_referenced" => StorageError::Referenced("baseline_referenced"),
                "disk_referenced" => StorageError::Referenced("disk_referenced"),
                "media_referenced" => StorageError::Referenced("media_referenced"),
                "baseline_protected" => StorageError::Referenced("baseline_protected"),
                other => StorageError::Constraint(other.to_owned()),
            };
        }
        if code.code == rusqlite::ErrorCode::ConstraintViolation {
            return StorageError::Constraint(msg.clone());
        }
    }
    StorageError::Sqlite(err)
}

/// Extension to map rusqlite results with [`map_sqlite`].
pub(crate) trait SqlResultExt<T> {
    fn db(self) -> Result<T>;
}

impl<T> SqlResultExt<T> for std::result::Result<T, rusqlite::Error> {
    fn db(self) -> Result<T> {
        self.map_err(map_sqlite)
    }
}
