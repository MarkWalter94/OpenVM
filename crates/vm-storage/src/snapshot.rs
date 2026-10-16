//! User snapshots in manual use (ADR-0032), journaled.
//!
//! A snapshot is a QCOW2 **internal snapshot** of the VM's own disk (its
//! name, the "tag", is the snapshot identifier), plus a directory
//! `vms/<vm_id>/snapshots/<snapshot_id>/` with a read-only copy of the UEFI
//! variables (`vars.fd`, UEFI VMs) and an optional thumbnail (`thumb.png`),
//! plus a catalog row with the name, the description, the VM configuration
//! at that moment and the parent in the tree. `snapshot_heads` records the
//! snapshot the current state descends from ("you are here").
//!
//! The disk operation itself (`qemu-img snapshot -c/-a/-d` with the VM off,
//! QMP `blockdev-snapshot-internal-sync` / `-delete-internal-sync` while it
//! runs) is done by the caller between `begin_*` and `settle_*`: the
//! storage does not start processes. Every operation is a journal intent
//! (`take_snapshot`, `revert_snapshot`, `delete_snapshot`, schema v10) with
//! a JSON payload, and is **settled from the disk** with the same rules
//! whether it ends normally, is cancelled, or is found open after a crash
//! (`Catalog::recover`):
//!
//! | Intent | Disk found | Action |
//! | --- | --- | --- |
//! | `take_snapshot` | tag present | files published (temporary → final), row inserted, head = new snapshot → `committed` |
//! | `take_snapshot` | tag absent (or disk missing) | temporary and final directory removed → `aborted` |
//! | `revert_snapshot` | active L1 table equal to the snapshot's | UEFI variables replaced by the snapshot's copy, configuration of the payload written, head = snapshot → `committed` |
//! | `revert_snapshot` | otherwise | nothing changed (the variables are touched only after the disk is reverted) → `aborted` |
//! | `delete_snapshot` | tag absent | files moved away, children re-parented to the deleted one's parent, head moved to the parent, row deleted → `committed` |
//! | `delete_snapshot` | tag present | nothing changed → `aborted` |
//! | any | disk unreadable (I/O error, not a QCOW2 image) | intent `needs_review`, VM busy (generic rule of `recover.rs`) |
//!
//! The order of the steps makes the rules sound: a take writes its files
//! before the tag exists; a revert changes the variables and the
//! configuration only after the disk was reverted; a delete moves the files
//! only after the tag is gone. The database effect and the intent's
//! `committed` state are written in one transaction. The caller decides who
//! may do what (user only, VM off or running, no agent session: `vm-core`);
//! the storage refuses a VM whose current state is an overlay of a baseline
//! (sessions run there: snapshots act only on the VM's own disk).

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::catalog::{
    Catalog, Intent, exists_no_follow, remove_dir_no_links, remove_temp, set_state_tx,
    target_exists,
};
use crate::enums::{IntentKind, IntentState, TargetKind};
use crate::error::{PathRejection, Result, SqlResultExt, StorageError};
use crate::fsx;
use crate::ids::{IdSql, SnapshotId, VmId, idc};
use crate::layout::CatalogLayout;
use crate::paths::{ManagedPath, is_link};
use crate::qcow2;
use crate::records::Tx;
use crate::time::now_rfc3339;

/// Message of the refusal on a VM whose current state is an overlay.
pub const SNAPSHOT_ON_OVERLAY: &str =
    "snapshots act on the VM's own disk: this VM runs on an overlay of a baseline";
/// Message of the refusal of a baseline restore while snapshots exist.
pub const RESTORE_WITH_SNAPSHOTS: &str =
    "the VM has snapshots: restoring a baseline would discard the disk that holds them";
/// Message of the refusal on a VM without a disk yet.
pub const SNAPSHOT_NO_DISK: &str = "the VM has no disk yet: start it once before taking a snapshot";
/// Largest thumbnail kept (bytes of PNG).
pub const MAX_THUMBNAIL_BYTES: usize = 40 * 1024;

const VARS_FILE: &str = "vars.fd";
const THUMB_FILE: &str = "thumb.png";
const NEW_VARS: &str = "new.vars";
const MOVED_DIR: &str = "files";

/// A user snapshot as recorded in the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    /// Snapshot.
    pub snapshot_id: SnapshotId,
    /// VM.
    pub vm_id: VmId,
    /// Parent in the tree.
    pub parent_id: Option<SnapshotId>,
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Name of the internal snapshot in the disk.
    pub disk_tag: String,
    /// Taken while the VM ran (crash-consistent disk state).
    pub taken_while_running: bool,
    /// The VM was off after an observed clean shutdown (or never ran).
    pub clean_shutdown: bool,
    /// Configuration of the VM at that moment (opaque JSON of the caller).
    pub config: serde_json::Value,
    /// Configuration revision at that moment.
    pub config_revision: u64,
    /// SHA-256 of the copy of the UEFI variables, if any.
    pub vars_sha256: Option<String>,
    /// Bytes of the copy of the UEFI variables (0 without).
    pub vars_bytes: u64,
    /// Bytes of the thumbnail (0 without).
    pub thumbnail_bytes: u64,
    /// Creation instant (RFC 3339).
    pub created_at: String,
}

/// Snapshots of a VM with the current position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotList {
    /// Snapshots, oldest first (parents before their children).
    pub snapshots: Vec<SnapshotRecord>,
    /// Snapshot the current state descends from.
    pub current: Option<SnapshotId>,
}

/// Request to take a snapshot.
#[derive(Debug, Clone)]
pub struct TakeSnapshot {
    /// VM.
    pub vm_id: VmId,
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// The VM is running (disk-only, crash-consistent snapshot).
    pub running: bool,
    /// The VM is off after an observed clean shutdown (or never ran).
    pub clean_shutdown: bool,
    /// Configuration of the VM now (opaque JSON, restored by a revert).
    pub config: serde_json::Value,
    /// Its revision.
    pub config_revision: u64,
    /// The VM's own disk (`vms/<vm>/primary.qcow2`).
    pub disk: ManagedPath,
    /// The VM's UEFI variables now (the file the run uses while running).
    pub vars: Option<ManagedPath>,
    /// Thumbnail PNG (running only), at most [`MAX_THUMBNAIL_BYTES`].
    pub thumbnail: Option<Vec<u8>>,
}

/// Request to revert to a snapshot (VM off).
#[derive(Debug, Clone)]
pub struct RevertSnapshot {
    /// VM.
    pub vm_id: VmId,
    /// Snapshot.
    pub snapshot_id: SnapshotId,
    /// The VM's own disk.
    pub disk: ManagedPath,
    /// The VM's UEFI variables (replaced by the snapshot's copy, removed if
    /// the snapshot has none); `None` for a VM without UEFI variables.
    pub vars: Option<ManagedPath>,
    /// New configuration of the VM row after the revert (opaque JSON).
    pub config: serde_json::Value,
    /// Its format version.
    pub config_version: u32,
    /// Revision the row must have now (it becomes `+ 1`).
    pub expected_revision: u64,
}

/// A snapshot operation in progress (journal intent recorded).
#[derive(Debug, Clone)]
pub struct SnapshotOp {
    intent: Intent,
    snapshot_id: SnapshotId,
    tag: String,
    disk: PathBuf,
}

impl SnapshotOp {
    /// Snapshot involved (the new one for a take).
    #[must_use]
    pub fn snapshot_id(&self) -> SnapshotId {
        self.snapshot_id
    }

    /// Name of the internal snapshot in the disk.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The VM's disk (absolute).
    #[must_use]
    pub fn disk(&self) -> &Path {
        &self.disk
    }

    /// Journal intent identifier.
    #[must_use]
    pub fn intent_id(&self) -> i64 {
        self.intent.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TakePayload {
    snapshot_id: SnapshotId,
    vm_id: VmId,
    parent: Option<SnapshotId>,
    name: String,
    description: String,
    running: bool,
    clean_shutdown: bool,
    config: serde_json::Value,
    config_revision: u64,
    disk: String,
    tag: String,
    has_vars: bool,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RevertPayload {
    snapshot_id: SnapshotId,
    vm_id: VmId,
    disk: String,
    tag: String,
    vars: Option<String>,
    snapshot_vars: Option<String>,
    snapshot_vars_sha256: Option<String>,
    config: serde_json::Value,
    config_version: u32,
    expected_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeletePayload {
    snapshot_id: SnapshotId,
    vm_id: VmId,
    disk: String,
    tag: String,
}

/// Outcome of settling a snapshot intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// The operation took effect (committed).
    Done,
    /// The operation did not take effect (aborted, nothing changed).
    NotDone,
}

fn row_to_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotRecord> {
    let cfg: String = r.get(8)?;
    Ok(SnapshotRecord {
        snapshot_id: idc(r, 0)?,
        vm_id: idc(r, 1)?,
        parent_id: idc(r, 2)?,
        name: r.get(3)?,
        description: r.get(4)?,
        disk_tag: r.get(5)?,
        taken_while_running: r.get::<_, i64>(6)? != 0,
        clean_shutdown: r.get::<_, i64>(7)? != 0,
        config: serde_json::from_str(&cfg).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
        })?,
        config_revision: u64::try_from(r.get::<_, i64>(9)?).unwrap_or(0),
        vars_sha256: r.get(10)?,
        vars_bytes: u64::try_from(r.get::<_, i64>(11)?).unwrap_or(0),
        thumbnail_bytes: u64::try_from(r.get::<_, i64>(12)?).unwrap_or(0),
        created_at: r.get(13)?,
    })
}

const COLUMNS: &str = "snapshot_id, vm_id, parent_id, name, description, disk_tag,
     taken_while_running, clean_shutdown, config_json, config_revision, vars_sha256,
     vars_bytes, thumbnail_bytes, created_at";

fn i64_of(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StorageError::Constraint("value out of range".into()))
}

impl Tx<'_> {
    /// A snapshot.
    pub fn snapshot(&self, id: &SnapshotId) -> Result<SnapshotRecord> {
        self.conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM snapshots WHERE snapshot_id = ?1"),
                params![id.sql()],
                row_to_record,
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "snapshot",
                id: id.to_string(),
            })
    }

    /// Snapshots of a VM, oldest first.
    pub fn snapshots_of_vm(&self, vm: &VmId) -> Result<Vec<SnapshotRecord>> {
        let mut st = self
            .conn()
            .prepare(&format!(
                "SELECT {COLUMNS} FROM snapshots WHERE vm_id = ?1 ORDER BY created_at, snapshot_id"
            ))
            .db()?;
        let rows = st.query_map(params![vm.sql()], row_to_record).db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()
    }

    /// Snapshot the VM's current state descends from.
    pub fn snapshot_head(&self, vm: &VmId) -> Result<Option<SnapshotId>> {
        self.conn()
            .query_row(
                "SELECT snapshot_id FROM snapshot_heads WHERE vm_id = ?1",
                params![vm.sql()],
                |r| idc(r, 0),
            )
            .optional()
            .db()
    }

    /// Number of snapshots of a VM.
    pub fn snapshot_count(&self, vm: &VmId) -> Result<u32> {
        let n: i64 = self
            .conn()
            .query_row(
                "SELECT count(*) FROM snapshots WHERE vm_id = ?1",
                params![vm.sql()],
                |r| r.get(0),
            )
            .db()?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    fn set_snapshot_head(&self, vm: &VmId, head: Option<&SnapshotId>) -> Result<()> {
        match head {
            Some(h) => self
                .conn()
                .execute(
                    "INSERT INTO snapshot_heads(vm_id, snapshot_id) VALUES (?1, ?2)
                     ON CONFLICT(vm_id) DO UPDATE SET snapshot_id = excluded.snapshot_id",
                    params![vm.sql(), h.sql()],
                )
                .db()?,
            None => self
                .conn()
                .execute(
                    "DELETE FROM snapshot_heads WHERE vm_id = ?1",
                    params![vm.sql()],
                )
                .db()?,
        };
        Ok(())
    }

    fn insert_snapshot(&self, s: &SnapshotRecord) -> Result<()> {
        self.conn()
            .execute(
                &format!(
                    "INSERT INTO snapshots({COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
                ),
                params![
                    s.snapshot_id.sql(),
                    s.vm_id.sql(),
                    s.parent_id.sql(),
                    s.name,
                    s.description,
                    s.disk_tag,
                    i64::from(s.taken_while_running),
                    i64::from(s.clean_shutdown),
                    s.config.to_string(),
                    i64_of(s.config_revision)?,
                    s.vars_sha256,
                    i64_of(s.vars_bytes)?,
                    i64_of(s.thumbnail_bytes)?,
                    s.created_at,
                ],
            )
            .db()?;
        Ok(())
    }

    /// Removes a snapshot row: its children move to its parent, the head
    /// moves to its parent if it was the head.
    fn remove_snapshot_row(&self, s: &SnapshotRecord) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE snapshots SET parent_id = ?1 WHERE parent_id = ?2",
                params![s.parent_id.sql(), s.snapshot_id.sql()],
            )
            .db()?;
        if self.snapshot_head(&s.vm_id)? == Some(s.snapshot_id) {
            self.set_snapshot_head(&s.vm_id, s.parent_id.as_ref())?;
        }
        self.conn()
            .execute(
                "DELETE FROM snapshots WHERE snapshot_id = ?1",
                params![s.snapshot_id.sql()],
            )
            .db()?;
        Ok(())
    }
}

/// Opens a regular managed file for reading while another process may
/// write it (the run's UEFI variables): no link on the last component.
fn open_for_copy(path: &Path, label: &str) -> Result<std::fs::File> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| StorageError::io(path, e))?;
    if is_link(&meta) || !meta.is_file() {
        return Err(StorageError::invalid_path(label, PathRejection::Link));
    }
    fsx::open_regular(path, fsx::Share::NoDelete, label).or_else(|e| match e {
        // QEMU holds the run's variables open for writing with a narrower
        // share mode on some hosts: a plain read open still works.
        StorageError::VmBusy(_) => std::fs::File::open(path).map_err(|e| StorageError::io(path, e)),
        other => Err(other),
    })
}

/// Copies `src` into the new file `dst` (no overwrite), synced, through a
/// `.part` name renamed at the end: `dst` is complete whenever it exists.
fn copy_complete(src: &Path, label: &str, dst: &Path) -> Result<(String, u64)> {
    let f = open_for_copy(src, label)?;
    let mut part = dst.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    if exists_no_follow(&part)? {
        std::fs::remove_file(&part).map_err(|e| StorageError::io(&part, e))?;
    }
    let out = crate::baseline::copy_from_handle(&f, src, &part)?;
    std::fs::rename(&part, dst).map_err(|e| StorageError::io(dst, e))?;
    Ok(out)
}

/// Disk tag state: `Some(true)` present, `Some(false)` absent (or the disk
/// file is gone), error if the disk cannot be read.
fn tag_present(disk: &Path, tag: &str) -> Result<bool> {
    match qcow2::read_info(disk) {
        Ok(info) => Ok(info.find(tag).is_some()),
        Err(StorageError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

impl Catalog {
    /// Snapshots of a VM with the current position.
    pub fn snapshot_list(&mut self, vm: &VmId) -> Result<SnapshotList> {
        self.read(|tx| {
            Ok(SnapshotList {
                snapshots: tx.snapshots_of_vm(vm)?,
                current: tx.snapshot_head(vm)?,
            })
        })
    }

    /// Absolute path of a snapshot's thumbnail, if it has one.
    pub fn snapshot_thumbnail(&mut self, vm: &VmId, id: &SnapshotId) -> Result<Option<PathBuf>> {
        let s = self.read(|tx| tx.snapshot(id))?;
        if s.vm_id != *vm {
            return Err(StorageError::NotFound {
                kind: "snapshot",
                id: id.to_string(),
            });
        }
        if s.thumbnail_bytes == 0 {
            return Ok(None);
        }
        let p = self
            .layout
            .resolve(&CatalogLayout::snapshot_thumbnail(vm, id))?;
        Ok(exists_no_follow(&p)?.then_some(p))
    }

    /// Absolute path of a snapshot's copy of the UEFI variables, if any.
    pub fn snapshot_vars(&mut self, vm: &VmId, id: &SnapshotId) -> Result<Option<PathBuf>> {
        let s = self.read(|tx| tx.snapshot(id))?;
        if s.vm_id != *vm {
            return Err(StorageError::NotFound {
                kind: "snapshot",
                id: id.to_string(),
            });
        }
        if s.vars_sha256.is_none() {
            return Ok(None);
        }
        Ok(Some(
            self.layout.resolve(&CatalogLayout::snapshot_vars(vm, id))?,
        ))
    }

    /// New name and description of a snapshot of `vm`.
    pub fn update_snapshot_text(
        &mut self,
        vm: &VmId,
        id: &SnapshotId,
        name: &str,
        description: &str,
    ) -> Result<SnapshotRecord> {
        self.with_tx(|tx| {
            let s = tx.snapshot(id)?;
            if s.vm_id != *vm {
                return Err(StorageError::NotFound {
                    kind: "snapshot",
                    id: id.to_string(),
                });
            }
            tx.conn()
                .execute(
                    "UPDATE snapshots SET name = ?1, description = ?2 WHERE snapshot_id = ?3",
                    params![name, description, id.sql()],
                )
                .db()?;
            tx.snapshot(id)
        })
    }

    /// Common checks: the VM exists, its current state is not an overlay of
    /// a baseline, its disk is a regular file.
    fn snapshot_target(&mut self, vm: &VmId, disk: &ManagedPath) -> Result<PathBuf> {
        let overlay = self.read(|tx| {
            tx.vm(vm)?;
            Ok(tx.current_state(vm)?.is_some())
        })?;
        if overlay {
            return Err(StorageError::StateConflict(SNAPSHOT_ON_OVERLAY.into()));
        }
        let abs = self.layout.resolve(disk)?;
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if is_link(&m) || !m.is_file() => Err(StorageError::invalid_path(
                disk.as_str(),
                PathRejection::Link,
            )),
            Ok(_) => Ok(abs),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::StateConflict(SNAPSHOT_NO_DISK.into()))
            }
            Err(e) => Err(StorageError::io(&abs, e)),
        }
    }

    /// First step of a take: intent `take_snapshot`, copy of the UEFI
    /// variables and thumbnail into the intent's temporary directory. The
    /// caller then creates the internal snapshot [`SnapshotOp::tag`] in
    /// [`SnapshotOp::disk`] and calls [`Self::settle_snapshot`].
    pub fn begin_take_snapshot(&mut self, req: TakeSnapshot) -> Result<SnapshotOp> {
        if req
            .thumbnail
            .as_ref()
            .is_some_and(|t| t.len() > MAX_THUMBNAIL_BYTES)
        {
            return Err(StorageError::ResourceLimit("thumbnail too large".into()));
        }
        let disk = self.snapshot_target(&req.vm_id, &req.disk)?;
        let vars_src = match &req.vars {
            Some(rel) => {
                let abs = self.layout.resolve(rel)?;
                exists_no_follow(&abs)?.then_some((abs, rel.clone()))
            }
            None => None,
        };
        let parent = self.read(|tx| tx.snapshot_head(&req.vm_id))?;
        let snapshot_id = SnapshotId::new_v7();
        let tag = snapshot_id.to_string();
        let payload = TakePayload {
            snapshot_id,
            vm_id: req.vm_id,
            parent,
            name: req.name.clone(),
            description: req.description.clone(),
            running: req.running,
            clean_shutdown: req.clean_shutdown && !req.running,
            config: req.config.clone(),
            config_revision: req.config_revision,
            disk: req.disk.to_string(),
            tag: tag.clone(),
            has_vars: vars_src.is_some(),
            created_at: now_rfc3339(),
        };
        let intent = self.begin_snapshot_intent(
            IntentKind::TakeSnapshot,
            &req.vm_id,
            &snapshot_id.to_string(),
            Some(CatalogLayout::snapshot_dir(&req.vm_id, &snapshot_id)),
            &serde_json::to_value(&payload)?,
        )?;
        let files = (|| -> Result<()> {
            if let Some((abs, rel)) = &vars_src {
                copy_complete(abs, rel.as_str(), &intent.temp_path.join(VARS_FILE))?;
            }
            if let Some(png) = &req.thumbnail {
                let p = intent.temp_path.join(THUMB_FILE);
                let mut f = fsx::create_new_exclusive(&p)?;
                std::io::Write::write_all(&mut f, png).map_err(|e| StorageError::io(&p, e))?;
                f.sync_all().map_err(|e| StorageError::io(&p, e))?;
            }
            Ok(())
        })();
        if let Err(e) = files {
            let _ = remove_temp(&self.layout, &intent.temp_rel);
            let _ =
                self.set_intent_state(intent.id, IntentState::Aborted, Some("files_not_copied"));
            return Err(e);
        }
        Ok(SnapshotOp {
            intent,
            snapshot_id,
            tag,
            disk,
        })
    }

    /// First step of a revert (VM `stopped`, no open run): intent
    /// `revert_snapshot`, copy of the snapshot's UEFI variables (hash
    /// verified) into the temporary directory. The caller then applies the
    /// internal snapshot (`qemu-img snapshot -a`) and calls
    /// [`Self::settle_snapshot`].
    pub fn begin_revert_snapshot(&mut self, req: RevertSnapshot) -> Result<SnapshotOp> {
        let (rec, open) = self.read(|tx| Ok((tx.vm(&req.vm_id)?, tx.open_runs(&req.vm_id)?)))?;
        if rec.lifecycle != crate::Lifecycle::Stopped || !open.is_empty() {
            return Err(StorageError::StateConflict(
                "snapshot revert requested with the VM not stopped".into(),
            ));
        }
        let disk = self.snapshot_target(&req.vm_id, &req.disk)?;
        let s = self.read(|tx| tx.snapshot(&req.snapshot_id))?;
        if s.vm_id != req.vm_id {
            return Err(StorageError::NotFound {
                kind: "snapshot",
                id: req.snapshot_id.to_string(),
            });
        }
        if !tag_present(&disk, &s.disk_tag)? {
            return Err(StorageError::IntegrityMismatch {
                path: req.disk.to_string(),
                detail: "the snapshot is missing from the VM's disk".into(),
            });
        }
        let snapshot_vars = s
            .vars_sha256
            .as_ref()
            .map(|_| CatalogLayout::snapshot_vars(&req.vm_id, &req.snapshot_id));
        let payload = RevertPayload {
            snapshot_id: req.snapshot_id,
            vm_id: req.vm_id,
            disk: req.disk.to_string(),
            tag: s.disk_tag.clone(),
            vars: req.vars.as_ref().map(ToString::to_string),
            snapshot_vars: snapshot_vars.as_ref().map(ToString::to_string),
            snapshot_vars_sha256: s.vars_sha256.clone(),
            config: req.config.clone(),
            config_version: req.config_version,
            expected_revision: req.expected_revision,
        };
        let intent = self.begin_snapshot_intent(
            IntentKind::RevertSnapshot,
            &req.vm_id,
            &req.snapshot_id.to_string(),
            req.vars.clone(),
            &serde_json::to_value(&payload)?,
        )?;
        if let (Some(rel), Some(_)) = (&snapshot_vars, &req.vars) {
            let copied = (|| -> Result<()> {
                let src = self.layout.resolve(rel)?;
                let (sha, _) = copy_complete(&src, rel.as_str(), &intent.temp_path.join(NEW_VARS))?;
                if Some(&sha) != s.vars_sha256.as_ref() {
                    return Err(StorageError::IntegrityMismatch {
                        path: rel.to_string(),
                        detail: "the snapshot's UEFI variables were altered".into(),
                    });
                }
                Ok(())
            })();
            if let Err(e) = copied {
                let _ = remove_temp(&self.layout, &intent.temp_rel);
                let _ =
                    self.set_intent_state(intent.id, IntentState::Aborted, Some("vars_not_copied"));
                return Err(e);
            }
        }
        Ok(SnapshotOp {
            snapshot_id: req.snapshot_id,
            tag: s.disk_tag,
            disk,
            intent,
        })
    }

    /// First step of a deletion (VM off or running): intent
    /// `delete_snapshot`. The caller then deletes the internal snapshot
    /// (`qemu-img snapshot -d` or QMP) and calls [`Self::settle_snapshot`].
    pub fn begin_delete_snapshot(
        &mut self,
        vm: &VmId,
        id: &SnapshotId,
        disk: &ManagedPath,
    ) -> Result<SnapshotOp> {
        let disk_abs = self.snapshot_target(vm, disk)?;
        let s = self.read(|tx| tx.snapshot(id))?;
        if s.vm_id != *vm {
            return Err(StorageError::NotFound {
                kind: "snapshot",
                id: id.to_string(),
            });
        }
        let payload = DeletePayload {
            snapshot_id: *id,
            vm_id: *vm,
            disk: disk.to_string(),
            tag: s.disk_tag.clone(),
        };
        let intent = self.begin_snapshot_intent(
            IntentKind::DeleteSnapshot,
            vm,
            &id.to_string(),
            Some(CatalogLayout::snapshot_dir(vm, id)),
            &serde_json::to_value(&payload)?,
        )?;
        Ok(SnapshotOp {
            intent,
            snapshot_id: *id,
            tag: s.disk_tag,
            disk: disk_abs,
        })
    }

    /// Settles a snapshot operation from the disk (see the module table):
    /// called when the caller's disk operation ended, failed or was
    /// cancelled. [`Settled::Done`] = the operation took effect.
    pub fn settle_snapshot(&mut self, op: &SnapshotOp) -> Result<Settled> {
        self.fault_at(crate::FaultPoint::SnapshotBeforeSettle)?;
        let (settled, detail) = self.settle_snapshot_intent(&op.intent)?;
        let _ = self.set_resolution(op.intent.id, &detail);
        Ok(settled)
    }

    /// Settles an open snapshot intent; returns the outcome and the detail
    /// for the journal. Idempotent: reconciliation calls it again after a
    /// crash at any point.
    pub(crate) fn settle_snapshot_intent(&mut self, i: &Intent) -> Result<(Settled, String)> {
        let payload = self
            .intent_payload(i.id)?
            .ok_or_else(|| StorageError::Constraint("snapshot intent without payload".into()))?;
        match i.kind {
            IntentKind::TakeSnapshot => {
                let p: TakePayload = serde_json::from_str(&payload)?;
                self.settle_take(i, &p)
            }
            IntentKind::RevertSnapshot => {
                let p: RevertPayload = serde_json::from_str(&payload)?;
                self.settle_revert(i, &p)
            }
            IntentKind::DeleteSnapshot => {
                let p: DeletePayload = serde_json::from_str(&payload)?;
                self.settle_delete(i, &p)
            }
            _ => Err(StorageError::StateConflict("not a snapshot intent".into())),
        }
    }

    fn final_dir(&self, i: &Intent) -> Result<PathBuf> {
        i.final_path
            .clone()
            .ok_or_else(|| StorageError::Constraint("snapshot intent without final path".into()))
    }

    fn abort_snapshot_intent(&mut self, i: &Intent, detail: &str) -> Result<(Settled, String)> {
        remove_temp(&self.layout, &i.temp_rel)?;
        self.set_intent_state(i.id, IntentState::Aborted, Some(detail))?;
        Ok((Settled::NotDone, detail.to_owned()))
    }

    fn settle_take(&mut self, i: &Intent, p: &TakePayload) -> Result<(Settled, String)> {
        let final_dir = self.final_dir(i)?;
        let row =
            self.read(|tx| target_exists(tx, TargetKind::Snapshot, i.target_id.as_deref()))?;
        let disk = self.layout.resolve(&ManagedPath::parse(&p.disk)?)?;
        if !row && !tag_present(&disk, &p.tag)? {
            if exists_no_follow(&final_dir)? {
                remove_dir_no_links(&final_dir)?;
            }
            return self.abort_snapshot_intent(i, "tag_absent+files_removed");
        }
        // The tag is in the disk (or the row already exists): publish.
        if !exists_no_follow(&final_dir)? {
            if !exists_no_follow(&i.temp_path)? {
                return Err(StorageError::IntegrityMismatch {
                    path: i.temp_rel.to_string(),
                    detail: "files of the snapshot missing".into(),
                });
            }
            if let Some(rel) = &i.final_rel
                && let Some((parent, _)) = rel.as_str().rsplit_once('/')
            {
                self.layout.ensure_dir(&ManagedPath::parse(parent)?)?;
            }
            if let Some(rel) = &i.final_rel {
                self.layout.resolve(rel)?;
            }
            std::fs::rename(&i.temp_path, &final_dir)
                .map_err(|e| StorageError::io(&final_dir, e))?;
        } else if exists_no_follow(&i.temp_path)? {
            remove_temp(&self.layout, &i.temp_rel)?;
        }
        self.fault_at(crate::FaultPoint::SnapshotAfterPublish)?;
        let vars = final_dir.join(VARS_FILE);
        let thumb = final_dir.join(THUMB_FILE);
        let vars_info = if p.has_vars {
            if !exists_no_follow(&vars)? {
                return Err(StorageError::IntegrityMismatch {
                    path: format!("{}/{VARS_FILE}", p.snapshot_id),
                    detail: "UEFI variables of the snapshot missing".into(),
                });
            }
            fsx::ensure_readonly(&vars)?;
            let f = fsx::open_regular(&vars, fsx::Share::ReadOnly, VARS_FILE)?;
            Some(fsx::sha256_handle(&f, &vars)?)
        } else {
            None
        };
        let thumb_bytes = if exists_no_follow(&thumb)? {
            fsx::ensure_readonly(&thumb)?;
            std::fs::metadata(&thumb).map_or(0, |m| m.len())
        } else {
            0
        };
        let rec = SnapshotRecord {
            snapshot_id: p.snapshot_id,
            vm_id: p.vm_id,
            parent_id: p.parent,
            name: p.name.clone(),
            description: p.description.clone(),
            disk_tag: p.tag.clone(),
            taken_while_running: p.running,
            clean_shutdown: p.clean_shutdown,
            config: p.config.clone(),
            config_revision: p.config_revision,
            vars_sha256: vars_info.as_ref().map(|(s, _)| s.clone()),
            vars_bytes: vars_info.as_ref().map_or(0, |(_, n)| *n),
            thumbnail_bytes: thumb_bytes,
            created_at: p.created_at.clone(),
        };
        let id = i.id;
        self.with_tx(|tx| {
            if !target_exists(tx, TargetKind::Snapshot, i.target_id.as_deref())? {
                // The parent may have been deleted meanwhile only through
                // another intent of this VM, which the journal excludes;
                // still, never insert a dangling parent.
                let parent = match &rec.parent_id {
                    Some(pid)
                        if target_exists(tx, TargetKind::Snapshot, Some(&pid.to_string()))? =>
                    {
                        Some(*pid)
                    }
                    _ => None,
                };
                tx.insert_snapshot(&SnapshotRecord {
                    parent_id: parent,
                    ..rec.clone()
                })?;
                tx.set_snapshot_head(&rec.vm_id, Some(&rec.snapshot_id))?;
            }
            set_state_tx(tx, id, IntentState::Committed, Some("taken"))
        })?;
        Ok((Settled::Done, "taken".into()))
    }

    fn settle_revert(&mut self, i: &Intent, p: &RevertPayload) -> Result<(Settled, String)> {
        let disk = self.layout.resolve(&ManagedPath::parse(&p.disk)?)?;
        let reverted = match qcow2::active_matches(&disk, &p.tag) {
            Ok(m) => m == Some(true),
            Err(StorageError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                false
            }
            Err(e) => return Err(e),
        };
        if !reverted {
            return self.abort_snapshot_intent(i, "disk_not_reverted");
        }
        // The disk is at the snapshot: the variables follow it.
        if let Some(vars_rel) = &p.vars {
            let rel = ManagedPath::parse(vars_rel)?;
            let dest = self.layout.resolve(&rel)?;
            let new = i.temp_path.join(NEW_VARS);
            if let Some(src_rel) = &p.snapshot_vars {
                if !exists_no_follow(&i.temp_path)? {
                    std::fs::create_dir(&i.temp_path)
                        .map_err(|e| StorageError::io(&i.temp_path, e))?;
                }
                if !exists_no_follow(&new)? {
                    let src_rel = ManagedPath::parse(src_rel)?;
                    let src = self.layout.resolve(&src_rel)?;
                    let (sha, _) = copy_complete(&src, src_rel.as_str(), &new)?;
                    if Some(&sha) != p.snapshot_vars_sha256.as_ref() {
                        return Err(StorageError::IntegrityMismatch {
                            path: src_rel.to_string(),
                            detail: "the snapshot's UEFI variables were altered".into(),
                        });
                    }
                }
                if exists_no_follow(&dest)? {
                    crate::catalog::remove_managed_file(&self.layout, &rel)?;
                }
                self.fault_at(crate::FaultPoint::SnapshotRevertVarsRemoved)?;
                if let Some((parent, _)) = rel.as_str().rsplit_once('/') {
                    self.layout.ensure_dir(&ManagedPath::parse(parent)?)?;
                }
                std::fs::rename(&new, &dest).map_err(|e| StorageError::io(&dest, e))?;
            } else if exists_no_follow(&dest)? {
                // The snapshot was taken before the first start: no
                // variables (the next start creates them from the template).
                crate::catalog::remove_managed_file(&self.layout, &rel)?;
            }
        }
        self.fault_at(crate::FaultPoint::SnapshotAfterPublish)?;
        let id = i.id;
        self.with_tx(|tx| {
            tx.update_vm_config(&p.vm_id, p.expected_revision, p.config_version, &p.config)?;
            tx.set_snapshot_head(&p.vm_id, Some(&p.snapshot_id))?;
            // One transaction with the intent's state: once committed the
            // intent is no longer open, so this never runs twice.
            set_state_tx(tx, id, IntentState::Committed, Some("reverted"))
        })?;
        let _ = remove_temp(&self.layout, &i.temp_rel);
        Ok((Settled::Done, "reverted".into()))
    }

    fn settle_delete(&mut self, i: &Intent, p: &DeletePayload) -> Result<(Settled, String)> {
        let final_dir = self.final_dir(i)?;
        let row = self.read(|tx| {
            if target_exists(tx, TargetKind::Snapshot, i.target_id.as_deref())? {
                Ok(Some(tx.snapshot(&p.snapshot_id)?))
            } else {
                Ok(None)
            }
        })?;
        let disk = self.layout.resolve(&ManagedPath::parse(&p.disk)?)?;
        if row.is_some() && tag_present(&disk, &p.tag)? {
            // Not deleted: put back whatever an earlier attempt moved.
            let moved = i.temp_path.join(MOVED_DIR);
            if exists_no_follow(&moved)? && !exists_no_follow(&final_dir)? {
                std::fs::rename(&moved, &final_dir).map_err(|e| StorageError::io(&final_dir, e))?;
            }
            return self.abort_snapshot_intent(i, "tag_present");
        }
        if exists_no_follow(&final_dir)? {
            if !exists_no_follow(&i.temp_path)? {
                std::fs::create_dir(&i.temp_path).map_err(|e| StorageError::io(&i.temp_path, e))?;
            }
            std::fs::rename(&final_dir, i.temp_path.join(MOVED_DIR))
                .map_err(|e| StorageError::io(&final_dir, e))?;
        }
        self.fault_at(crate::FaultPoint::SnapshotAfterPublish)?;
        let id = i.id;
        self.with_tx(|tx| {
            if let Some(s) = &row {
                tx.remove_snapshot_row(s)?;
            }
            set_state_tx(tx, id, IntentState::Committed, Some("deleted"))
        })?;
        let _ = remove_temp(&self.layout, &i.temp_rel);
        Ok((Settled::Done, "deleted".into()))
    }

    /// Removes the rows of every snapshot of a VM (VM deletion).
    pub(crate) fn snapshot_rows_cleared(tx: &Tx<'_>, vm: &VmId) -> Result<()> {
        tx.conn()
            .execute(
                "UPDATE snapshots SET parent_id = NULL WHERE vm_id = ?1",
                params![vm.sql()],
            )
            .db()?;
        tx.conn()
            .execute(
                "DELETE FROM snapshot_heads WHERE vm_id = ?1",
                params![vm.sql()],
            )
            .db()?;
        tx.conn()
            .execute("DELETE FROM snapshots WHERE vm_id = ?1", params![vm.sql()])
            .db()?;
        Ok(())
    }
}
