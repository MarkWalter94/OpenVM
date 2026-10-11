//! Catalog records and SQL operations inside a transaction.
//!
//! All writes go through [`Tx`]: an SQLite transaction opened by
//! [`crate::Catalog::with_tx`] or, for journal intents, by
//! [`crate::Catalog::commit`], which records the transition to `committed` in
//! the same transaction.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::enums::{
    ArtifactKind, AuditOutcome, DiskRole, DiskStatus, FirmwareScope, Lifecycle, MediaKind,
    MediaSlot, MediaStatus, OperationState, RunState, SessionState, TerminationCause,
};
use crate::error::{Result, SqlResultExt, StorageError};
use crate::ids::{
    ArtifactId, BaselineId, DiskId, FirmwareId, GrantId, IdSql, MediaId, OperationId, RunId,
    SessionId, VmId, idc,
};
use crate::paths::ManagedPath;
use crate::time::now_rfc3339;

fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StorageError::Constraint(format!("value out of range: {v}")))
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn path_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<ManagedPath> {
    let s: String = row.get(idx)?;
    ManagedPath::parse(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Data for creating a VM.
#[derive(Debug, Clone)]
pub struct NewVm {
    /// Unique name (case-insensitive).
    pub name: String,
    /// Declared guest operating system.
    pub guest_os: String,
    /// Configuration schema version.
    pub config_version: u32,
    /// Configuration serialized as JSON (never interpreted as host options).
    pub config: serde_json::Value,
}

/// VM in the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct VmRecord {
    /// Identifier.
    pub vm_id: VmId,
    /// Name.
    pub name: String,
    /// Guest operating system.
    pub guest_os: String,
    /// Last persisted state.
    pub lifecycle: Lifecycle,
    /// Configuration schema version.
    pub config_version: u32,
    /// Configuration.
    pub config: serde_json::Value,
    /// Revision (optimistic concurrency control).
    pub revision: u64,
    /// SHA-256 of the on-disk manifest.
    pub manifest_sha256: Option<String>,
    /// Creation time (UTC).
    pub created_at: String,
    /// Last update time (UTC).
    pub updated_at: String,
}

/// Media in the approved registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRecord {
    /// Identifier.
    pub media_id: MediaId,
    /// SHA-256 of the managed copy.
    pub sha256: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Kind.
    pub kind: MediaKind,
    /// User-declared origin (text, never a path to open).
    pub declared_origin: String,
    /// Approval time (UTC).
    pub approved_at: String,
    /// Managed copy, relative to the root.
    pub managed_path: ManagedPath,
    /// Status.
    pub status: MediaStatus,
}

/// Managed disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRecord {
    /// Identifier.
    pub disk_id: DiskId,
    /// Owning VM.
    pub vm_id: VmId,
    /// Role.
    pub role: DiskRole,
    /// Base of an overlay.
    pub backing_disk_id: Option<DiskId>,
    /// Virtual size.
    pub virtual_size_bytes: u64,
    /// Measured allocated bytes.
    pub allocated_bytes: u64,
    /// Managed path.
    pub managed_path: ManagedPath,
    /// SHA-256 (for bases).
    pub sha256: Option<String>,
    /// Status.
    pub status: DiskStatus,
    /// Creation time (UTC).
    pub created_at: String,
}

/// Managed firmware state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareRecord {
    /// Identifier.
    pub fw_id: FirmwareId,
    /// VM.
    pub vm_id: VmId,
    /// Scope.
    pub scope: FirmwareScope,
    /// Source of the copy.
    pub source_fw_id: Option<FirmwareId>,
    /// Managed path.
    pub managed_path: ManagedPath,
    /// SHA-256.
    pub sha256: Option<String>,
    /// Size.
    pub size_bytes: u64,
    /// Creation time (UTC).
    pub created_at: String,
}

/// Baseline taken with the VM powered off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineRecord {
    /// Identifier.
    pub baseline_id: BaselineId,
    /// VM.
    pub vm_id: VmId,
    /// Immutable base disk.
    pub base_disk_id: DiskId,
    /// Firmware state of the baseline.
    pub vars_fw_id: Option<FirmwareId>,
    /// Label.
    pub label: String,
    /// SHA-256 of the manifest.
    pub manifest_sha256: String,
    /// Clean shutdown observed (a forced stop is never "verified").
    pub clean_shutdown: bool,
    /// Protected initial baseline (cannot be deleted).
    pub protected: bool,
    /// Creation time (UTC).
    pub created_at: String,
}

/// Current state of a VM after a restore (schema v6): overlay of a baseline
/// plus a writable copy of the baseline's UEFI variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentStateRecord {
    /// VM.
    pub vm_id: VmId,
    /// Baseline the overlay is backed by.
    pub baseline_id: BaselineId,
    /// Overlay disk (role `overlay`).
    pub overlay_disk_id: DiskId,
    /// UEFI variables (scope `vm`), if the baseline has them.
    pub vars_fw_id: Option<FirmwareId>,
    /// Restore time (UTC).
    pub restored_at: String,
}

/// Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    /// Identifier.
    pub run_id: RunId,
    /// VM.
    pub vm_id: VmId,
    /// Starting baseline.
    pub baseline_id: Option<BaselineId>,
    /// Overlay of the run.
    pub overlay_disk_id: Option<DiskId>,
    /// Vars of the run.
    pub vars_fw_id: Option<FirmwareId>,
    /// Status.
    pub state: RunState,
    /// Start time (UTC).
    pub started_at: String,
    /// End time (UTC).
    pub ended_at: Option<String>,
    /// Termination cause.
    pub termination_cause: Option<TerminationCause>,
}

/// Operation with revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    /// Identifier.
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Run.
    pub run_id: Option<RunId>,
    /// Requesting agent session (`None` for the user).
    pub session_id: Option<SessionId>,
    /// Kind (stable text defined by the core).
    pub kind: String,
    /// Status.
    pub state: OperationState,
    /// VM revision expected by the operation.
    pub revision: u64,
    /// Public error code.
    pub error_code: Option<String>,
    /// Creation time (UTC).
    pub created_at: String,
    /// Update time (UTC).
    pub updated_at: String,
}

/// Agent session (persistence only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Identifier.
    pub session_id: SessionId,
    /// Declared objective.
    pub objective: String,
    /// Status.
    pub state: SessionState,
    /// Revision of the applied policy.
    pub policy_revision: u64,
    /// Creation time (UTC).
    pub created_at: String,
    /// Expiry time (UTC).
    pub expires_at: String,
    /// End time (UTC).
    pub ended_at: Option<String>,
    /// Termination cause.
    pub termination_cause: Option<String>,
}

/// Session grant on a VM (persistence only; no credentials).
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRecord {
    /// Identifier.
    pub grant_id: GrantId,
    /// Session.
    pub session_id: SessionId,
    /// Authorized VM.
    pub vm_id: VmId,
    /// Approved capabilities.
    pub capabilities: serde_json::Value,
    /// Approved quotas.
    pub quotas: serde_json::Value,
    /// Creation time (UTC).
    pub created_at: String,
    /// Expiry time (UTC).
    pub expires_at: String,
    /// Revocation time (UTC).
    pub revoked_at: Option<String>,
}

/// Audit event to record. No payload: only kind, ids and outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Action kind (stable text, e.g. `vm.start`).
    pub action: String,
    /// Outcome.
    pub outcome: Option<AuditOutcome>,
    /// VM.
    pub vm_id: Option<VmId>,
    /// Run.
    pub run_id: Option<RunId>,
    /// Session.
    pub session_id: Option<SessionId>,
    /// Operation.
    pub operation_id: Option<OperationId>,
    /// Public error code.
    pub error_code: Option<String>,
}

/// Recorded audit event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// Monotonic sequence number.
    pub seq: i64,
    /// Timestamp (UTC).
    pub at: String,
    /// Event.
    pub event: AuditEvent,
}

/// Artifact of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// Identifier.
    pub artifact_id: ArtifactId,
    /// Run.
    pub run_id: RunId,
    /// Kind.
    pub kind: ArtifactKind,
    /// Managed path.
    pub managed_path: ManagedPath,
    /// SHA-256.
    pub sha256: String,
    /// Size.
    pub size_bytes: u64,
    /// Creation time (UTC).
    pub created_at: String,
    /// Retained until (UTC).
    pub retain_until: Option<String>,
}

/// Catalog transaction.
#[derive(Debug)]
pub struct Tx<'c> {
    pub(crate) inner: rusqlite::Transaction<'c>,
}

impl Tx<'_> {
    pub(crate) fn conn(&self) -> &rusqlite::Connection {
        &self.inner
    }

    // ---------------------------------------------------------------- VM

    /// Creates a VM. The name is unique, case-insensitively.
    pub fn insert_vm(&self, vm: &NewVm) -> Result<VmId> {
        self.insert_vm_with_id(&VmId::new_v7(), vm)
    }

    /// Creates a VM with a given identifier (the core chooses it through its
    /// own generator, F3: the files of the VM then live under a single
    /// `vms/<vm_id>/`).
    pub fn insert_vm_with_id(&self, id: &VmId, vm: &NewVm) -> Result<VmId> {
        let id = *id;
        let now = now_rfc3339();
        self.inner
            .execute(
                "INSERT INTO vms(vm_id, name, guest_os, config_version, config_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![id.sql(), vm.name, vm.guest_os, vm.config_version, vm.config.to_string(), now],
            )
            .db()?;
        Ok(id)
    }

    /// Reads a VM.
    pub fn vm(&self, id: &VmId) -> Result<VmRecord> {
        self.inner
            .query_row(
                "SELECT vm_id, name, guest_os, lifecycle, config_version, config_json, revision,
                        manifest_sha256, created_at, updated_at FROM vms WHERE vm_id = ?1",
                params![id.sql()],
                |r| {
                    let cfg: String = r.get(5)?;
                    Ok(VmRecord {
                        vm_id: idc(r, 0)?,
                        name: r.get(1)?,
                        guest_os: r.get(2)?,
                        lifecycle: r.get(3)?,
                        config_version: r.get(4)?,
                        config: serde_json::from_str(&cfg).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        revision: to_u64(r.get(6)?),
                        manifest_sha256: r.get(7)?,
                        created_at: r.get(8)?,
                        updated_at: r.get(9)?,
                    })
                },
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "vm",
                id: id.to_string(),
            })
    }

    /// List of VM identifiers, sorted by name.
    pub fn vm_ids(&self) -> Result<Vec<VmId>> {
        let mut st = self
            .inner
            .prepare("SELECT vm_id FROM vms ORDER BY name")
            .db()?;
        let rows = st.query_map([], |r| idc(r, 0)).db()?;
        rows.collect::<rusqlite::Result<Vec<VmId>>>().db()
    }

    /// Updates the configuration if the expected revision matches; returns the
    /// new revision. Different revision: [`StorageError::StateConflict`].
    pub fn update_vm_config(
        &self,
        id: &VmId,
        expected_revision: u64,
        config_version: u32,
        config: &serde_json::Value,
    ) -> Result<u64> {
        let n = self
            .inner
            .execute(
                "UPDATE vms SET config_json = ?1, config_version = ?2, revision = revision + 1,
                        updated_at = ?3
                 WHERE vm_id = ?4 AND revision = ?5",
                params![
                    config.to_string(),
                    config_version,
                    now_rfc3339(),
                    id.sql(),
                    to_i64(expected_revision)?
                ],
            )
            .db()?;
        if n == 0 {
            let current = self.vm(id)?.revision;
            return Err(StorageError::StateConflict(format!(
                "expected revision {expected_revision}, current {current}"
            )));
        }
        Ok(expected_revision + 1)
    }

    /// Updates the last persisted lifecycle state.
    pub fn set_vm_lifecycle(&self, id: &VmId, state: Lifecycle) -> Result<()> {
        let n = self
            .inner
            .execute(
                "UPDATE vms SET lifecycle = ?1, updated_at = ?2 WHERE vm_id = ?3",
                params![state, now_rfc3339(), id.sql()],
            )
            .db()?;
        if n == 0 {
            return Err(StorageError::NotFound {
                kind: "vm",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn set_vm_manifest_hash(&self, id: &VmId, sha: &str) -> Result<()> {
        self.inner
            .execute(
                "UPDATE vms SET manifest_sha256 = ?1 WHERE vm_id = ?2",
                params![sha, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Deletes a VM that has no disks, baselines, runs or firmware.
    pub fn delete_vm(&self, id: &VmId) -> Result<()> {
        self.inner
            .execute("DELETE FROM vms WHERE vm_id = ?1", params![id.sql()])
            .map_err(|e| match crate::error::map_sqlite(e) {
                StorageError::Constraint(_) => StorageError::Referenced("vm_referenced"),
                other => other,
            })?;
        Ok(())
    }

    // ------------------------------------------------------------- media

    /// Records an approved media (the managed copy must be produced with an
    /// `import_media` intent).
    pub fn insert_media(&self, m: &MediaRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO media(media_id, sha256, size_bytes, kind, declared_origin, approved_at,
                                   managed_path, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    m.media_id.sql(),
                    m.sha256,
                    to_i64(m.size_bytes)?,
                    m.kind,
                    m.declared_origin,
                    m.approved_at,
                    m.managed_path.as_str(),
                    m.status
                ],
            )
            .db()?;
        Ok(())
    }

    /// Reads a media.
    pub fn media(&self, id: &MediaId) -> Result<MediaRecord> {
        self.inner
            .query_row(
                "SELECT media_id, sha256, size_bytes, kind, declared_origin, approved_at,
                        managed_path, status FROM media WHERE media_id = ?1",
                params![id.sql()],
                |r| {
                    Ok(MediaRecord {
                        media_id: idc(r, 0)?,
                        sha256: r.get(1)?,
                        size_bytes: to_u64(r.get(2)?),
                        kind: r.get(3)?,
                        declared_origin: r.get(4)?,
                        approved_at: r.get(5)?,
                        managed_path: path_col(r, 6)?,
                        status: r.get(7)?,
                    })
                },
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "media",
                id: id.to_string(),
            })
    }

    /// Identifiers of every media of the registry, in approval order.
    pub fn media_ids(&self) -> Result<Vec<MediaId>> {
        let mut st = self
            .inner
            .prepare("SELECT media_id FROM media ORDER BY approved_at, media_id")
            .db()?;
        let rows = st.query_map([], |r| idc(r, 0)).db()?;
        rows.collect::<rusqlite::Result<Vec<MediaId>>>().db()
    }

    /// Attaches an approved media to a VM.
    pub fn attach_media(&self, vm: &VmId, slot: MediaSlot, media: &MediaId) -> Result<()> {
        let m = self.media(media)?;
        if m.status != MediaStatus::Approved {
            return Err(StorageError::StateConflict(format!(
                "media {media} not approved ({})",
                m.status
            )));
        }
        self.inner
            .execute(
                "INSERT INTO vm_media(vm_id, slot, media_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT(vm_id, slot) DO UPDATE SET media_id = excluded.media_id",
                params![vm.sql(), slot, media.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Detaches the media from a slot.
    pub fn detach_media(&self, vm: &VmId, slot: MediaSlot) -> Result<()> {
        self.inner
            .execute(
                "DELETE FROM vm_media WHERE vm_id = ?1 AND slot = ?2",
                params![vm.sql(), slot],
            )
            .db()?;
        Ok(())
    }

    /// Media attached to a VM.
    pub fn vm_media(&self, vm: &VmId) -> Result<Vec<(MediaSlot, MediaId)>> {
        let mut st = self
            .inner
            .prepare("SELECT slot, media_id FROM vm_media WHERE vm_id = ?1 ORDER BY slot")
            .db()?;
        let rows = st
            .query_map(params![vm.sql()], |r| Ok((r.get(0)?, idc(r, 1)?)))
            .db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    /// Deletes the row of an unreferenced media.
    pub fn delete_media(&self, id: &MediaId) -> Result<()> {
        self.inner
            .execute("DELETE FROM media WHERE media_id = ?1", params![id.sql()])
            .db()?;
        Ok(())
    }

    // -------------------------------------------------------------- disks

    /// Records a managed disk.
    pub fn insert_disk(&self, d: &DiskRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO disks(disk_id, vm_id, role, backing_disk_id, virtual_size_bytes,
                                   allocated_bytes, managed_path, sha256, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    d.disk_id.sql(),
                    d.vm_id.sql(),
                    d.role,
                    d.backing_disk_id.sql(),
                    to_i64(d.virtual_size_bytes)?,
                    to_i64(d.allocated_bytes)?,
                    d.managed_path.as_str(),
                    d.sha256,
                    d.status,
                    d.created_at
                ],
            )
            .db()?;
        Ok(())
    }

    /// Reads a disk.
    pub fn disk(&self, id: &DiskId) -> Result<DiskRecord> {
        self.disk_opt(id)?.ok_or_else(|| StorageError::NotFound {
            kind: "disk",
            id: id.to_string(),
        })
    }

    pub(crate) fn disk_opt(&self, id: &DiskId) -> Result<Option<DiskRecord>> {
        self.inner
            .query_row(
                &format!("{DISK_SELECT} WHERE disk_id = ?1"),
                params![id.sql()],
                disk_row,
            )
            .optional()
            .db()
    }

    /// Disks of a VM.
    pub fn disks_of_vm(&self, vm: &VmId) -> Result<Vec<DiskRecord>> {
        let mut st = self
            .inner
            .prepare(&format!(
                "{DISK_SELECT} WHERE vm_id = ?1 ORDER BY created_at"
            ))
            .db()?;
        let rows = st.query_map(params![vm.sql()], disk_row).db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    pub(crate) fn all_disks(&self) -> Result<Vec<DiskRecord>> {
        let mut st = self.inner.prepare(DISK_SELECT).db()?;
        let rows = st.query_map([], disk_row).db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    /// Updates the status of a disk.
    pub fn set_disk_status(&self, id: &DiskId, status: DiskStatus) -> Result<()> {
        self.inner
            .execute(
                "UPDATE disks SET status = ?1 WHERE disk_id = ?2",
                params![status, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Updates the measured allocated bytes.
    pub fn set_disk_allocated(&self, id: &DiskId, bytes: u64) -> Result<()> {
        self.inner
            .execute(
                "UPDATE disks SET allocated_bytes = ?1 WHERE disk_id = ?2",
                params![to_i64(bytes)?, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Deletes the row of an unreferenced disk.
    pub fn delete_disk(&self, id: &DiskId) -> Result<()> {
        self.inner
            .execute("DELETE FROM disks WHERE disk_id = ?1", params![id.sql()])
            .db()?;
        Ok(())
    }

    // ----------------------------------------------------------- firmware

    /// Records a managed firmware state.
    pub fn insert_firmware(&self, f: &FirmwareRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO firmware_state(fw_id, vm_id, scope, source_fw_id, managed_path, sha256,
                                            size_bytes, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    f.fw_id.sql(),
                    f.vm_id.sql(),
                    f.scope,
                    f.source_fw_id.sql(),
                    f.managed_path.as_str(),
                    f.sha256,
                    to_i64(f.size_bytes)?,
                    f.created_at
                ],
            )
            .db()?;
        Ok(())
    }

    /// Reads a firmware state.
    pub fn firmware(&self, id: &FirmwareId) -> Result<FirmwareRecord> {
        self.firmware_opt(id)?
            .ok_or_else(|| StorageError::NotFound {
                kind: "firmware",
                id: id.to_string(),
            })
    }

    pub(crate) fn firmware_opt(&self, id: &FirmwareId) -> Result<Option<FirmwareRecord>> {
        self.inner
            .query_row(
                &format!("{FW_SELECT} WHERE fw_id = ?1"),
                params![id.sql()],
                fw_row,
            )
            .optional()
            .db()
    }

    pub(crate) fn all_firmware(&self) -> Result<Vec<FirmwareRecord>> {
        let mut st = self.inner.prepare(FW_SELECT).db()?;
        let rows = st.query_map([], fw_row).db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    /// Deletes the row of an unreferenced firmware state.
    pub fn delete_firmware(&self, id: &FirmwareId) -> Result<()> {
        self.inner
            .execute(
                "DELETE FROM firmware_state WHERE fw_id = ?1",
                params![id.sql()],
            )
            .map_err(|e| match crate::error::map_sqlite(e) {
                StorageError::Constraint(_) => StorageError::Referenced("firmware_referenced"),
                other => other,
            })?;
        Ok(())
    }

    // ----------------------------------------------------------- baseline

    /// Records a baseline. The VM must be powered off (`stopped`): a baseline
    /// is created only while the VM is powered off (section 8, ADR-0005).
    pub fn insert_baseline(&self, b: &BaselineRecord) -> Result<()> {
        let vm = self.vm(&b.vm_id)?;
        if vm.lifecycle != Lifecycle::Stopped {
            return Err(StorageError::StateConflict(format!(
                "baseline requested with VM in state {}",
                vm.lifecycle
            )));
        }
        let disk = self.disk(&b.base_disk_id)?;
        if disk.role != DiskRole::Base || disk.vm_id != b.vm_id {
            return Err(StorageError::StateConflict(
                "the baseline disk must be a base disk of the same VM".into(),
            ));
        }
        self.inner
            .execute(
                "INSERT INTO baselines(baseline_id, vm_id, base_disk_id, vars_fw_id, label,
                                       manifest_sha256, created_powered_off, clean_shutdown,
                                       protected, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)",
                params![
                    b.baseline_id.sql(),
                    b.vm_id.sql(),
                    b.base_disk_id.sql(),
                    b.vars_fw_id.sql(),
                    b.label,
                    b.manifest_sha256,
                    b.clean_shutdown,
                    b.protected,
                    b.created_at
                ],
            )
            .db()?;
        Ok(())
    }

    /// Reads a baseline.
    pub fn baseline(&self, id: &BaselineId) -> Result<BaselineRecord> {
        self.baseline_opt(id)?
            .ok_or_else(|| StorageError::NotFound {
                kind: "baseline",
                id: id.to_string(),
            })
    }

    pub(crate) fn baseline_opt(&self, id: &BaselineId) -> Result<Option<BaselineRecord>> {
        self.inner
            .query_row(
                &format!("{BASELINE_SELECT} WHERE baseline_id = ?1"),
                params![id.sql()],
                baseline_row,
            )
            .optional()
            .db()
    }

    /// Baselines of a VM.
    pub fn baselines_of_vm(&self, vm: &VmId) -> Result<Vec<BaselineRecord>> {
        let mut st = self
            .inner
            .prepare(&format!(
                "{BASELINE_SELECT} WHERE vm_id = ?1 ORDER BY created_at"
            ))
            .db()?;
        let rows = st.query_map(params![vm.sql()], baseline_row).db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    /// `true` if the baseline is referenced: by an open run, by an overlay
    /// backed by its base or by the VM's current state (F3). Ended runs do
    /// not count: their reference is cleared when the baseline is deleted.
    pub fn baseline_referenced(&self, id: &BaselineId) -> Result<bool> {
        self.inner
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM runs WHERE baseline_id = ?1 AND ended_at IS NULL)
                     OR EXISTS (SELECT 1 FROM disks d JOIN baselines b ON d.backing_disk_id = b.base_disk_id
                                WHERE b.baseline_id = ?1)
                     OR EXISTS (SELECT 1 FROM vm_current_state WHERE baseline_id = ?1)",
                params![id.sql()],
                |r| r.get(0),
            )
            .db()
    }

    /// Deletes an unreferenced, unprotected baseline together with its base
    /// disk and its firmware state. References: [`StorageError::Referenced`].
    pub fn delete_baseline(&self, id: &BaselineId) -> Result<()> {
        let b = self.baseline(id)?;
        // History of ended runs: the reference is cleared (the audit keeps
        // the trace); an open run still blocks the deletion (trigger).
        self.inner
            .execute(
                "UPDATE runs SET baseline_id = NULL WHERE baseline_id = ?1 AND ended_at IS NOT NULL",
                params![id.sql()],
            )
            .db()?;
        self.inner
            .execute(
                "DELETE FROM baselines WHERE baseline_id = ?1",
                params![id.sql()],
            )
            .db()?;
        self.delete_disk(&b.base_disk_id)?;
        if let Some(fw) = &b.vars_fw_id {
            self.delete_firmware(fw)?;
        }
        Ok(())
    }

    /// Baseline the source state of `id` was an overlay of (schema v6).
    pub fn baseline_derived_from(&self, id: &BaselineId) -> Result<Option<BaselineId>> {
        let v: Option<String> = self
            .inner
            .query_row(
                "SELECT derived_from FROM baselines WHERE baseline_id = ?1",
                params![id.sql()],
                |r| r.get(0),
            )
            .db()?;
        Ok(v.and_then(|s| s.parse().ok()))
    }

    /// VM configuration revision recorded at the creation of a baseline
    /// (schema v6; `None` for older baselines).
    pub fn baseline_config_revision(&self, id: &BaselineId) -> Result<Option<u64>> {
        let v: Option<i64> = self
            .inner
            .query_row(
                "SELECT config_revision FROM baselines WHERE baseline_id = ?1",
                params![id.sql()],
                |r| r.get(0),
            )
            .db()?;
        Ok(v.map(to_u64))
    }

    /// Records the configuration revision of a baseline (schema v6).
    pub fn set_baseline_config_revision(&self, id: &BaselineId, revision: u64) -> Result<()> {
        self.inner
            .execute(
                "UPDATE baselines SET config_revision = ?1 WHERE baseline_id = ?2",
                params![to_i64(revision)?, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Records the parent of a baseline (schema v6).
    pub fn set_baseline_derived_from(&self, id: &BaselineId, parent: &BaselineId) -> Result<()> {
        self.inner
            .execute(
                "UPDATE baselines SET derived_from = ?1 WHERE baseline_id = ?2",
                params![parent.sql(), id.sql()],
            )
            .db()?;
        Ok(())
    }

    // ------------------------------------------------------- current state

    /// Current state of a VM (overlay of a baseline after a restore, schema
    /// v6); `None` = the VM runs on its standalone working disk.
    pub fn current_state(&self, vm: &VmId) -> Result<Option<CurrentStateRecord>> {
        self.inner
            .query_row(
                "SELECT vm_id, baseline_id, overlay_disk_id, vars_fw_id, restored_at
                 FROM vm_current_state WHERE vm_id = ?1",
                params![vm.sql()],
                |r| {
                    Ok(CurrentStateRecord {
                        vm_id: idc(r, 0)?,
                        baseline_id: idc(r, 1)?,
                        overlay_disk_id: idc(r, 2)?,
                        vars_fw_id: idc(r, 3)?,
                        restored_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .db()
    }

    /// Sets (or replaces) the current state of a VM.
    pub fn set_current_state(&self, c: &CurrentStateRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO vm_current_state(vm_id, baseline_id, overlay_disk_id, vars_fw_id, restored_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(vm_id) DO UPDATE SET baseline_id = excluded.baseline_id,
                     overlay_disk_id = excluded.overlay_disk_id, vars_fw_id = excluded.vars_fw_id,
                     restored_at = excluded.restored_at",
                params![
                    c.vm_id.sql(),
                    c.baseline_id.sql(),
                    c.overlay_disk_id.sql(),
                    c.vars_fw_id.sql(),
                    c.restored_at
                ],
            )
            .db()?;
        Ok(())
    }

    /// Clears the references of ended runs to a disk and a firmware state
    /// about to be discarded (history only; an open run blocks the discard).
    pub fn clear_ended_run_refs(
        &self,
        disk: Option<&DiskId>,
        fw: Option<&FirmwareId>,
    ) -> Result<()> {
        if let Some(d) = disk {
            self.inner
                .execute(
                    "UPDATE runs SET overlay_disk_id = NULL
                     WHERE overlay_disk_id = ?1 AND ended_at IS NOT NULL",
                    params![d.sql()],
                )
                .db()?;
        }
        if let Some(f) = fw {
            self.inner
                .execute(
                    "UPDATE runs SET vars_fw_id = NULL WHERE vars_fw_id = ?1 AND ended_at IS NOT NULL",
                    params![f.sql()],
                )
                .db()?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- runs

    /// Records the start of a run.
    pub fn insert_run(&self, r: &RunRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO runs(run_id, vm_id, baseline_id, overlay_disk_id, vars_fw_id, state,
                                  started_at, ended_at, termination_cause)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    r.run_id.sql(),
                    r.vm_id.sql(),
                    r.baseline_id.sql(),
                    r.overlay_disk_id.sql(),
                    r.vars_fw_id.sql(),
                    r.state,
                    r.started_at,
                    r.ended_at,
                    r.termination_cause
                ],
            )
            .db()?;
        Ok(())
    }

    /// Reads a run.
    pub fn run(&self, id: &RunId) -> Result<RunRecord> {
        self.inner
            .query_row(
                &format!("{RUN_SELECT} WHERE run_id = ?1"),
                params![id.sql()],
                run_row,
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "run",
                id: id.to_string(),
            })
    }

    /// All runs of a VM, finished or not, in identifier order.
    pub fn run_ids_of_vm(&self, vm: &VmId) -> Result<Vec<RunId>> {
        let mut st = self
            .inner
            .prepare("SELECT run_id FROM runs WHERE vm_id = ?1 ORDER BY run_id")
            .db()?;
        let rows = st.query_map(params![vm.sql()], |r| idc(r, 0)).db()?;
        rows.collect::<rusqlite::Result<Vec<RunId>>>().db()
    }

    /// Unfinished runs of a VM (`ended_at` absent).
    pub fn open_runs(&self, vm: &VmId) -> Result<Vec<RunId>> {
        let mut st = self
            .inner
            .prepare("SELECT run_id FROM runs WHERE vm_id = ?1 AND ended_at IS NULL")
            .db()?;
        let rows = st.query_map(params![vm.sql()], |r| idc(r, 0)).db()?;
        rows.collect::<rusqlite::Result<Vec<RunId>>>().db()
    }

    /// Records whether the run ended with an observed clean guest shutdown
    /// (schema v6).
    pub fn set_run_clean_exit(&self, id: &RunId, clean: bool) -> Result<()> {
        self.inner
            .execute(
                "UPDATE runs SET clean_exit = ?1 WHERE run_id = ?2",
                params![clean, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// End of the VM's most recent run: `None` = the VM never ran;
    /// `Some(None)` = unknown (not observed, e.g. supervisor restart);
    /// `Some(Some(clean))` otherwise.
    pub fn last_run_clean_exit(&self, vm: &VmId) -> Result<Option<Option<bool>>> {
        self.inner
            .query_row(
                "SELECT clean_exit FROM runs WHERE vm_id = ?1
                 ORDER BY started_at DESC, run_id DESC LIMIT 1",
                params![vm.sql()],
                |r| r.get::<_, Option<bool>>(0),
            )
            .optional()
            .db()
    }

    /// Updates the state of an unfinished run.
    pub fn set_run_state(&self, id: &RunId, state: RunState) -> Result<()> {
        self.inner
            .execute(
                "UPDATE runs SET state = ?1 WHERE run_id = ?2",
                params![state, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Closes a run with a termination cause.
    pub fn end_run(&self, id: &RunId, state: RunState, cause: TerminationCause) -> Result<()> {
        let n = self
            .inner
            .execute(
                "UPDATE runs SET state = ?1, ended_at = ?2, termination_cause = ?3
                 WHERE run_id = ?4 AND ended_at IS NULL",
                params![state, now_rfc3339(), cause, id.sql()],
            )
            .db()?;
        if n == 0 {
            return Err(StorageError::StateConflict(format!(
                "run {id} missing or already closed"
            )));
        }
        Ok(())
    }

    // ---------------------------------------------------------- operations

    /// Records an operation.
    pub fn insert_operation(&self, o: &OperationRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO operations(operation_id, vm_id, run_id, kind, state, revision,
                                        error_code, created_at, updated_at, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    o.operation_id.sql(),
                    o.vm_id.sql(),
                    o.run_id.sql(),
                    o.kind,
                    o.state,
                    to_i64(o.revision)?,
                    o.error_code,
                    o.created_at,
                    o.updated_at,
                    o.session_id.sql()
                ],
            )
            .db()?;
        Ok(())
    }

    /// Updates the state of an operation.
    pub fn set_operation_state(
        &self,
        id: &OperationId,
        state: OperationState,
        error_code: Option<&str>,
    ) -> Result<()> {
        self.inner
            .execute(
                "UPDATE operations SET state = ?1, error_code = ?2, updated_at = ?3
                 WHERE operation_id = ?4",
                params![state, error_code, now_rfc3339(), id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Reads an operation.
    pub fn operation(&self, id: &OperationId) -> Result<OperationRecord> {
        self.inner
            .query_row(
                "SELECT operation_id, vm_id, run_id, kind, state, revision, error_code, created_at,
                        updated_at, session_id FROM operations WHERE operation_id = ?1",
                params![id.sql()],
                |r| {
                    Ok(OperationRecord {
                        operation_id: idc(r, 0)?,
                        vm_id: idc(r, 1)?,
                        run_id: idc(r, 2)?,
                        session_id: idc(r, 9)?,
                        kind: r.get(3)?,
                        state: r.get(4)?,
                        revision: to_u64(r.get(5)?),
                        error_code: r.get(6)?,
                        created_at: r.get(7)?,
                        updated_at: r.get(8)?,
                    })
                },
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "operation",
                id: id.to_string(),
            })
    }

    // ------------------------------------------------------ sessions/grants

    /// Records a session.
    pub fn insert_session(&self, s: &SessionRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO sessions(session_id, objective, state, policy_revision, created_at,
                                      expires_at, ended_at, termination_cause)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    s.session_id.sql(),
                    s.objective,
                    s.state,
                    to_i64(s.policy_revision)?,
                    s.created_at,
                    s.expires_at,
                    s.ended_at,
                    s.termination_cause
                ],
            )
            .db()?;
        Ok(())
    }

    /// Updates the state of a session; final states record the end time.
    pub fn set_session_state(
        &self,
        id: &SessionId,
        state: SessionState,
        cause: Option<&str>,
    ) -> Result<()> {
        let ended = matches!(
            state,
            SessionState::Revoked | SessionState::Expired | SessionState::Closed
        )
        .then(now_rfc3339);
        self.inner
            .execute(
                "UPDATE sessions SET state = ?1, ended_at = coalesce(ended_at, ?2),
                        termination_cause = coalesce(?3, termination_cause)
                 WHERE session_id = ?4",
                params![state, ended, cause, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Reads a session.
    pub fn session(&self, id: &SessionId) -> Result<SessionRecord> {
        self.inner
            .query_row(
                "SELECT session_id, objective, state, policy_revision, created_at, expires_at,
                        ended_at, termination_cause FROM sessions WHERE session_id = ?1",
                params![id.sql()],
                |r| {
                    Ok(SessionRecord {
                        session_id: idc(r, 0)?,
                        objective: r.get(1)?,
                        state: r.get(2)?,
                        policy_revision: to_u64(r.get(3)?),
                        created_at: r.get(4)?,
                        expires_at: r.get(5)?,
                        ended_at: r.get(6)?,
                        termination_cause: r.get(7)?,
                    })
                },
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "session",
                id: id.to_string(),
            })
    }

    /// Records a grant.
    pub fn insert_grant(&self, g: &GrantRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO grants(grant_id, session_id, vm_id, capabilities_json, quotas_json,
                                    created_at, expires_at, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    g.grant_id.sql(),
                    g.session_id.sql(),
                    g.vm_id.sql(),
                    g.capabilities.to_string(),
                    g.quotas.to_string(),
                    g.created_at,
                    g.expires_at,
                    g.revoked_at
                ],
            )
            .db()?;
        Ok(())
    }

    /// Revokes all grants of a session.
    pub fn revoke_grants(&self, session: &SessionId) -> Result<usize> {
        self.inner
            .execute(
                "UPDATE grants SET revoked_at = ?1 WHERE session_id = ?2 AND revoked_at IS NULL",
                params![now_rfc3339(), session.sql()],
            )
            .db()
    }

    /// Grants of a session.
    pub fn grants_of_session(&self, session: &SessionId) -> Result<Vec<GrantRecord>> {
        let mut st = self
            .inner
            .prepare(
                "SELECT grant_id, session_id, vm_id, capabilities_json, quotas_json, created_at,
                        expires_at, revoked_at FROM grants WHERE session_id = ?1 ORDER BY created_at",
            )
            .db()?;
        let rows = st
            .query_map(params![session.sql()], |r| {
                let caps: String = r.get(3)?;
                let quotas: String = r.get(4)?;
                let conv = |i, e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        i,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                };
                Ok(GrantRecord {
                    grant_id: idc(r, 0)?,
                    session_id: idc(r, 1)?,
                    vm_id: idc(r, 2)?,
                    capabilities: serde_json::from_str(&caps).map_err(|e| conv(3, e))?,
                    quotas: serde_json::from_str(&quotas).map_err(|e| conv(4, e))?,
                    created_at: r.get(5)?,
                    expires_at: r.get(6)?,
                    revoked_at: r.get(7)?,
                })
            })
            .db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    // --------------------------------------------------------------- audit

    /// Appends an audit event and returns its monotonic sequence number.
    pub fn append_audit(&self, e: &AuditEvent) -> Result<i64> {
        let outcome = e
            .outcome
            .ok_or_else(|| StorageError::Constraint("audit event without outcome".into()))?;
        self.inner
            .execute(
                "INSERT INTO audit(at, action, outcome, vm_id, run_id, session_id, operation_id,
                                   error_code)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    now_rfc3339(),
                    e.action,
                    outcome,
                    e.vm_id.sql(),
                    e.run_id.sql(),
                    e.session_id.sql(),
                    e.operation_id.sql(),
                    e.error_code
                ],
            )
            .db()?;
        Ok(self.inner.last_insert_rowid())
    }

    /// Audit events with `seq > after`, in order.
    pub fn audit_since(&self, after: i64, limit: u32) -> Result<Vec<AuditRecord>> {
        let mut st = self
            .inner
            .prepare(
                "SELECT seq, at, action, outcome, vm_id, run_id, session_id, operation_id,
                        error_code FROM audit WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .db()?;
        let rows = st
            .query_map(params![after, limit], |r| {
                Ok(AuditRecord {
                    seq: r.get(0)?,
                    at: r.get(1)?,
                    event: AuditEvent {
                        action: r.get(2)?,
                        outcome: Some(r.get(3)?),
                        vm_id: idc(r, 4)?,
                        run_id: idc(r, 5)?,
                        session_id: idc(r, 6)?,
                        operation_id: idc(r, 7)?,
                        error_code: r.get(8)?,
                    },
                })
            })
            .db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    // ----------------------------------------------------------- artifacts

    /// Records an artifact.
    pub fn insert_artifact(&self, a: &ArtifactRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO artifacts(artifact_id, run_id, kind, managed_path, sha256, size_bytes,
                                       created_at, retain_until)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    a.artifact_id.sql(),
                    a.run_id.sql(),
                    a.kind,
                    a.managed_path.as_str(),
                    a.sha256,
                    to_i64(a.size_bytes)?,
                    a.created_at,
                    a.retain_until
                ],
            )
            .db()?;
        Ok(())
    }

    /// Deletes the row of an artifact.
    pub fn delete_artifact(&self, id: &ArtifactId) -> Result<()> {
        self.inner
            .execute(
                "DELETE FROM artifacts WHERE artifact_id = ?1",
                params![id.sql()],
            )
            .db()?;
        Ok(())
    }

    pub(crate) fn artifact_exists(&self, id: &str) -> Result<bool> {
        self.exists("SELECT 1 FROM artifacts WHERE artifact_id = ?1", id)
    }

    pub(crate) fn exists(&self, sql: &str, id: &str) -> Result<bool> {
        Ok(self
            .inner
            .query_row(sql, params![id], |_| Ok(()))
            .optional()
            .db()?
            .is_some())
    }
}

const DISK_SELECT: &str = "SELECT disk_id, vm_id, role, backing_disk_id, virtual_size_bytes,
        allocated_bytes, managed_path, sha256, status, created_at FROM disks";

fn disk_row(r: &Row<'_>) -> rusqlite::Result<DiskRecord> {
    Ok(DiskRecord {
        disk_id: idc(r, 0)?,
        vm_id: idc(r, 1)?,
        role: r.get(2)?,
        backing_disk_id: idc(r, 3)?,
        virtual_size_bytes: to_u64(r.get(4)?),
        allocated_bytes: to_u64(r.get(5)?),
        managed_path: path_col(r, 6)?,
        sha256: r.get(7)?,
        status: r.get(8)?,
        created_at: r.get(9)?,
    })
}

const FW_SELECT: &str = "SELECT fw_id, vm_id, scope, source_fw_id, managed_path, sha256,
        size_bytes, created_at FROM firmware_state";

fn fw_row(r: &Row<'_>) -> rusqlite::Result<FirmwareRecord> {
    Ok(FirmwareRecord {
        fw_id: idc(r, 0)?,
        vm_id: idc(r, 1)?,
        scope: r.get(2)?,
        source_fw_id: idc(r, 3)?,
        managed_path: path_col(r, 4)?,
        sha256: r.get(5)?,
        size_bytes: to_u64(r.get(6)?),
        created_at: r.get(7)?,
    })
}

const BASELINE_SELECT: &str = "SELECT baseline_id, vm_id, base_disk_id, vars_fw_id, label,
        manifest_sha256, clean_shutdown, protected, created_at FROM baselines";

fn baseline_row(r: &Row<'_>) -> rusqlite::Result<BaselineRecord> {
    Ok(BaselineRecord {
        baseline_id: idc(r, 0)?,
        vm_id: idc(r, 1)?,
        base_disk_id: idc(r, 2)?,
        vars_fw_id: idc(r, 3)?,
        label: r.get(4)?,
        manifest_sha256: r.get(5)?,
        clean_shutdown: r.get(6)?,
        protected: r.get(7)?,
        created_at: r.get(8)?,
    })
}

const RUN_SELECT: &str = "SELECT run_id, vm_id, baseline_id, overlay_disk_id, vars_fw_id, state,
        started_at, ended_at, termination_cause FROM runs";

fn run_row(r: &Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        run_id: idc(r, 0)?,
        vm_id: idc(r, 1)?,
        baseline_id: idc(r, 2)?,
        overlay_disk_id: idc(r, 3)?,
        vars_fw_id: idc(r, 4)?,
        state: r.get(5)?,
        started_at: r.get(6)?,
        ended_at: r.get(7)?,
        termination_cause: r.get(8)?,
    })
}
