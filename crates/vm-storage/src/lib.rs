//! `vm-storage`: catalogo SQLite, migrazioni, manifest, journal e riconciliazione.
//!
//! Responsabilità (ROADMAP 6.3, 8; ARCHITECTURE §1): metadati in SQLite, dischi e
//! artefatti su file, manifest JSON versionati, baseline a VM spenta con overlay
//! QCOW2 e stato firmware, journal a intenti e riconciliazione per operazioni
//! che attraversano database e filesystem, registro dei media approvati,
//! contabilità del budget storage.
//!
//! Punti fermi:
//! - SQLite incorporato (`rusqlite` con `bundled`), WAL, `foreign_keys=ON`,
//!   `busy_timeout`; migrazioni versionate incorporate, rifiuto dei downgrade,
//!   backup prima di migrare ([`migrate`]).
//! - Un'istanza per catalogo, garantita da un lock su file ([`CatalogLock`]).
//! - Percorsi sempre relativi alla radice e validati ([`ManagedPath`]).
//! - Una transazione SQL non rende atomico uno spostamento di file: ogni
//!   operazione DB + filesystem passa dal journal (`begin_intent` →
//!   `mark_applied` → `commit`/`abort`) e [`Catalog::recover`] la riconcilia.
//! - Lo storage non avvia processi e non interpreta configurazioni come
//!   opzioni host: i dischi sono prodotti dal chiamante (`qemu-backend`).
//!
//! Stato: F1. Identificativi provvisori in [`compat`] (vedi `TODO(F1)`).

mod budget;
mod catalog;
pub mod compat;
mod enums;
mod error;
mod layout;
mod manifest;
pub mod migrate;
mod ops;
mod paths;
mod records;
mod recover;
mod time;

pub use budget::{BudgetCheck, BudgetConfig, Enforcement, StorageUsage};
pub use catalog::{Catalog, Intent, IntentSpec, OpenReport};
pub use enums::{
    ArtifactKind, AuditOutcome, DiskRole, DiskStatus, FirmwareScope, IntentKind, IntentState,
    Lifecycle, MediaKind, MediaSlot, MediaStatus, OperationState, RunState, SessionState,
    TargetKind, TerminationCause,
};
pub use error::{PathRejection, Result, StorageError};
pub use layout::{CatalogLayout, CatalogLock, DB_FILE, LOCK_FILE, TMP_DIR, TmpCleanup};
pub use manifest::{
    BaselineManifest, Divergence, MANIFEST_FORMAT_VERSION, ManifestFile, ManifestMedia, VmManifest,
    sha256_file,
};
pub use ops::PromoteBaseline;
pub use paths::ManagedPath;
pub use records::{
    ArtifactRecord, AuditEvent, AuditRecord, BaselineRecord, DiskRecord, FirmwareRecord,
    GrantRecord, MediaRecord, NewVm, OperationRecord, RunRecord, SessionRecord, Tx, VmRecord,
};
pub use recover::{IntegrityIssue, IntentRecovery, RecoveryOutcome, RecoveryReport};
pub use time::{UtcMillis, now_rfc3339};

/// Esadecimale minuscolo.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    s
}
