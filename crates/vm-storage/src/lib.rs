//! `vm-storage`: catalogo SQLite, migrazioni, manifest, journal e riconciliazione.
//!
//! Responsabilità (ROADMAP 6.3, 8; ARCHITECTURE §1): metadati in SQLite, dischi e
//! artefatti su file, manifest JSON versionati, baseline a VM spenta con overlay
//! QCOW2 e stato firmware, journal a intenti e riconciliazione per operazioni
//! che attraversano database e filesystem, registro dei media approvati,
//! contabilità del budget storage, protezione dei dischi durante l'esecuzione.
//!
//! Punti fermi:
//! - SQLite incorporato (`rusqlite` con `bundled`), WAL, `foreign_keys=ON`,
//!   `busy_timeout`; migrazioni versionate incorporate, rifiuto dei downgrade,
//!   backup prima di migrare ([`migrate`]).
//! - Un'istanza per catalogo, garantita da un lock su file ([`CatalogLock`]).
//! - Percorsi sempre relativi alla radice e validati ([`ManagedPath`]); rifiuto
//!   di collegamenti, junction e percorsi fuori dalla radice.
//! - Una transazione SQL non rende atomico uno spostamento di file: ogni
//!   operazione DB + filesystem passa dal journal (`begin_intent` →
//!   `mark_applied` → `commit`/`abort`) e [`Catalog::recover`] la riconcilia;
//!   ciò che la riconciliazione non risolve resta "da verificare"
//!   ([`IntentState::NeedsReview`]).
//! - File nuovi creati con `CREATE_NEW` e pubblicati senza sovrascrittura.
//! - Baseline aperta con il solo `FILE_SHARE_READ` durante preparazione ed
//!   esecuzione, hash verificato attraverso l'handle prima di ogni avvio
//!   ([`Catalog::guard_run`], ADR-0025).
//! - Lo storage non avvia processi e non interpreta configurazioni come
//!   opzioni host: i dischi sono prodotti dal chiamante (`qemu-backend`).
//!
//! Identificativi: quelli del protocollo vengono da `lightvm-proto` e sono
//! riesportati qui; [`DiskId`], [`FirmwareId`] e [`ArtifactId`] sono propri
//! dello storage. Il crate non contiene codice `unsafe`.

mod budget;
mod catalog;
mod enums;
mod error;
mod fsx;
mod guard;
mod ids;
mod layout;
mod manifest;
mod media;
pub mod migrate;
mod ops;
mod paths;
mod records;
mod recover;
mod time;

pub use budget::{
    BUDGET_MANIFEST_FILE, BaselineUsage, BudgetCheck, BudgetConfig, BudgetManifest, Enforcement,
    RunBudgetRequest, RunUsage, StorageUsage, VmBudget,
};
pub use catalog::{Catalog, Intent, IntentSpec, OpenReport};
pub use enums::{
    ArtifactKind, AuditOutcome, DiskRole, DiskStatus, FirmwareScope, IntentKind, IntentState,
    Lifecycle, MediaKind, MediaSlot, MediaStatus, OperationState, RunState, SessionState,
    TargetKind, TerminationCause,
};
pub use error::{PathRejection, Result, StorageError};
pub use guard::{BaselineGuard, GuardReport, RunGuard, WriterProbe};
pub use ids::{
    ArtifactId, BaselineId, DiskId, FirmwareId, GrantId, MediaId, OperationId, RunId, SessionId,
    VmId,
};
pub use layout::{CatalogLayout, CatalogLock, DB_FILE, LOCK_FILE, TMP_DIR, TmpCleanup};
pub use manifest::{
    BaselineManifest, Divergence, MANIFEST_FORMAT_VERSION, ManifestFile, ManifestMedia, VmManifest,
    sha256_file,
};
pub use media::{MAX_DECLARED_ORIGIN, MediaCheck};
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
