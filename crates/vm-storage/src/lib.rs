//! `vm-storage`: SQLite catalog, migrations, manifests, journal and reconciliation.
//!
//! Responsibilities (ROADMAP 6.3, 8; ARCHITECTURE §1): metadata in SQLite, disks
//! and artifacts in files, versioned JSON manifests, baselines taken with the VM
//! off with QCOW2 overlays and firmware state, intent journal and reconciliation
//! for operations that span database and filesystem, registry of approved media,
//! storage budget accounting, protection of disks while running.
//!
//! Fixed points:
//! - Embedded SQLite (`rusqlite` with `bundled`), WAL, `foreign_keys=ON`,
//!   `busy_timeout`; embedded versioned migrations, downgrades rejected,
//!   backup before migrating ([`migrate`]).
//! - One instance per catalog, guaranteed by a file lock ([`CatalogLock`]).
//! - Paths always relative to the root and validated ([`ManagedPath`]); links,
//!   junctions and paths outside the root are rejected.
//! - An SQL transaction does not make a file move atomic: every
//!   DB + filesystem operation goes through the journal (`begin_intent` →
//!   `mark_applied` → `commit`/`abort`) and [`Catalog::recover`] reconciles it;
//!   whatever reconciliation cannot resolve stays "needs review"
//!   ([`IntentState::NeedsReview`]).
//! - New files created with `CREATE_NEW` and published without overwriting.
//! - Baseline opened with `FILE_SHARE_READ` only during preparation and
//!   execution, hash verified through the handle before every start
//!   ([`Catalog::guard_run`], ADR-0025).
//! - Storage does not start processes and does not interpret configurations as
//!   host options: disks are produced by the caller (`qemu-backend`).
//!
//! Identifiers: protocol ones come from `lightvm-proto` and are re-exported
//! here; [`DiskId`], [`FirmwareId`] and [`ArtifactId`] belong to storage.
//! The crate contains no `unsafe` code.

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
mod vm_delete;

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
pub use media::{
    CopiedMedia, MAX_DECLARED_ORIGIN, MediaCheck, MediaImportJob, MediaImportReservation,
    MediaLimits, NOT_OPTICAL_IMAGE,
};
pub use ops::PromoteBaseline;
pub use paths::ManagedPath;
pub use records::{
    ArtifactRecord, AuditEvent, AuditRecord, BaselineRecord, DiskRecord, FirmwareRecord,
    GrantRecord, MediaRecord, NewVm, OperationRecord, RunRecord, SessionRecord, Tx, VmRecord,
};
pub use recover::{IntegrityIssue, IntentRecovery, RecoveryOutcome, RecoveryReport};
pub use time::{UtcMillis, now_rfc3339};
pub use vm_delete::VmDeletionReport;

/// Lowercase hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    s
}
