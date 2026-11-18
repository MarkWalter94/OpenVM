//! Deletion of a VM with its catalog rows and files (F2, `delete_vm`).
//!
//! Every file goes through the intent journal: one `delete_file` intent per
//! file, all sharing one operation id and targeting the `vms` row
//! ([`TargetKind::Vm`], schema v5). Procedure of
//! [`Catalog::delete_vm_with_files`]:
//!
//! 1. checks: the VM exists, is `stopped` or `failed` (no process), has no
//!    open run and no protected baseline; the directories to empty are under `vms/` or `runs/`;
//! 2. inventory of the regular files under `vms/<vm_id>/`, `runs/<run_id>/`
//!    of the VM's runs and the extra directories given by the caller (the
//!    supervisor keeps some files under the core identifier of the VM); a
//!    link or reparse point anywhere in the tree aborts the deletion before
//!    any change;
//! 3. `begin_intent` for every file (`pending`), then `mark_applied` (the
//!    file is moved into `tmp/`); any error aborts all the intents and
//!    restores the files;
//! 4. one transaction deletes every row of the VM (operations, artifacts,
//!    runs, grants, baselines, disks, firmware, media slots, budget, VM) and
//!    commits all the intents; the temporaries are then removed;
//! 5. the emptied directories are removed (best effort: an empty directory
//!    left behind by a crash is harmless).
//!
//! Reconciliation ([`Catalog::recover`]): once one file of the group has
//! been moved, the group rolls forward (rows deleted, remaining files
//! removed); if none was moved the group rolls back with nothing to undo.

use rusqlite::params;

use crate::catalog::{Catalog, Intent, IntentSpec};
use crate::enums::{IntentKind, Lifecycle, TargetKind};
use crate::error::{PathRejection, Result, SqlResultExt, StorageError};
use crate::ids::{IdSql, OperationId, VmId};
use crate::layout::CatalogLayout;
use crate::paths::{ManagedPath, is_link};
use crate::records::Tx;

/// Outcome of [`Catalog::delete_vm_with_files`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmDeletionReport {
    /// Files removed through the journal.
    pub files_removed: u32,
    /// Bytes of the removed files.
    pub bytes_removed: u64,
    /// Directories removed after the commit.
    pub dirs_removed: u32,
}

/// Directories of a VM tree, deepest first, and its regular files.
#[derive(Default)]
struct Inventory {
    files: Vec<(ManagedPath, u64)>,
    dirs: Vec<ManagedPath>,
}

impl Catalog {
    /// Deletes a VM that has no process with all its rows and files (see the
    /// module documentation). `extra_dirs` are additional directories of the
    /// VM to empty and remove, under `vms/` or `runs/` only (never a top-level
    /// directory itself).
    pub fn delete_vm_with_files(
        &mut self,
        vm: &VmId,
        extra_dirs: &[ManagedPath],
    ) -> Result<VmDeletionReport> {
        for d in extra_dirs {
            let area_ok = (d.is_under("vms") || d.is_under("runs")) && d.components().count() >= 2;
            if !area_ok {
                return Err(StorageError::invalid_path(
                    d.as_str(),
                    PathRejection::OutsideArea,
                ));
            }
        }
        let (runs, registered) = self.read(|tx| {
            check_deletable(tx, vm)?;
            Ok((tx.run_ids_of_vm(vm)?, registered_files(tx, vm)?))
        })?;
        let mut dirs = vec![CatalogLayout::vm_dir(vm)];
        for r in &runs {
            dirs.push(ManagedPath::parse(&format!("runs/{r}"))?);
        }
        dirs.extend(extra_dirs.iter().cloned());
        dirs.sort();
        dirs.dedup();

        let mut inv = Inventory::default();
        for d in &dirs {
            self.inventory(d, &mut inv)?;
        }
        for rel in registered {
            let abs = self.layout.resolve(&rel)?;
            match std::fs::symlink_metadata(&abs) {
                Ok(m) if is_link(&m) || !m.is_file() => {
                    return Err(StorageError::invalid_path(
                        rel.as_str(),
                        PathRejection::Link,
                    ));
                }
                Ok(m) => inv.files.push((rel, m.len())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StorageError::io(&abs, e)),
            }
        }
        inv.files.sort();
        inv.files.dedup_by(|a, b| a.0 == b.0);

        let op = OperationId::new_v7();
        let mut intents: Vec<Intent> = Vec::with_capacity(inv.files.len());
        let outcome = (|| -> Result<()> {
            for (rel, _) in &inv.files {
                intents.push(self.begin_intent(IntentSpec {
                    kind: IntentKind::DeleteFile,
                    vm_id: Some(*vm),
                    operation_id: Some(op),
                    target_kind: TargetKind::Vm,
                    target_id: Some(vm.to_string()),
                    final_path: Some(rel.clone()),
                    read_only: false,
                })?);
            }
            for i in &intents {
                self.mark_applied(i)?;
            }
            if intents.is_empty() {
                self.with_tx(|tx| delete_vm_rows(tx, vm))
            } else {
                self.commit_many(&intents, |tx| delete_vm_rows(tx, vm))
            }
        })();
        if let Err(e) = outcome {
            // In reverse order: a failed abort leaves the intent open and
            // reconciliation resolves it at the next start.
            for i in intents.iter().rev() {
                let _ = self.abort(i);
            }
            return Err(e);
        }
        let mut report = VmDeletionReport {
            files_removed: u32::try_from(inv.files.len()).unwrap_or(u32::MAX),
            bytes_removed: inv.files.iter().map(|(_, n)| *n).sum(),
            dirs_removed: 0,
        };
        // Deepest first: children were pushed after their parents.
        for d in inv.dirs.iter().rev() {
            if let Ok(abs) = self.layout.resolve(d)
                && std::fs::remove_dir(&abs).is_ok()
            {
                report.dirs_removed += 1;
            }
        }
        Ok(report)
    }

    /// Regular files and directories under `dir` (recursive). A missing
    /// directory is empty; a link or reparse point is an error.
    fn inventory(&self, dir: &ManagedPath, inv: &mut Inventory) -> Result<()> {
        let abs = self.layout.resolve(dir)?;
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(StorageError::io(&abs, e)),
        };
        if is_link(&meta) || !meta.is_dir() {
            return Err(StorageError::invalid_path(
                dir.as_str(),
                PathRejection::Link,
            ));
        }
        inv.dirs.push(dir.clone());
        for entry in std::fs::read_dir(&abs).map_err(|e| StorageError::io(&abs, e))? {
            let entry = entry.map_err(|e| StorageError::io(&abs, e))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                StorageError::invalid_path(dir.as_str(), PathRejection::InvalidCharacter)
            })?;
            let rel = dir.join(name)?;
            let p = entry.path();
            let m = std::fs::symlink_metadata(&p).map_err(|e| StorageError::io(&p, e))?;
            if is_link(&m) {
                return Err(StorageError::invalid_path(
                    rel.as_str(),
                    PathRejection::Link,
                ));
            }
            if m.is_dir() {
                self.inventory(&rel, inv)?;
            } else if m.is_file() {
                inv.files.push((rel, m.len()));
            } else {
                return Err(StorageError::invalid_path(
                    rel.as_str(),
                    PathRejection::Link,
                ));
            }
        }
        Ok(())
    }
}

/// The VM exists, has no process and no open run.
pub(crate) fn check_deletable(tx: &Tx<'_>, vm: &VmId) -> Result<()> {
    let rec = tx.vm(vm)?;
    if !matches!(rec.lifecycle, Lifecycle::Stopped | Lifecycle::Failed) {
        return Err(StorageError::StateConflict(format!(
            "VM {vm} in state {}: only a VM without a process can be deleted",
            rec.lifecycle
        )));
    }
    if !tx.open_runs(vm)?.is_empty() {
        return Err(StorageError::StateConflict(format!(
            "VM {vm} has an open run"
        )));
    }
    if tx.baselines_of_vm(vm)?.iter().any(|b| b.protected) {
        return Err(StorageError::Referenced("baseline_protected"));
    }
    Ok(())
}

/// Managed paths of the disks and firmware rows of the VM.
fn registered_files(tx: &Tx<'_>, vm: &VmId) -> Result<Vec<ManagedPath>> {
    let mut out = Vec::new();
    for sql in [
        "SELECT managed_path FROM disks WHERE vm_id = ?1",
        "SELECT managed_path FROM firmware_state WHERE vm_id = ?1",
    ] {
        let mut st = tx.conn().prepare(sql).db()?;
        let rows = st
            .query_map(params![vm.sql()], |r| r.get::<_, String>(0))
            .db()?;
        for p in rows.collect::<rusqlite::Result<Vec<_>>>().db()? {
            out.push(ManagedPath::parse(&p)?);
        }
    }
    Ok(out)
}

/// Deletes every row of the VM, children first. Disks of other VMs backed by
/// a disk of this VM make the deletion fail (`Referenced`).
pub(crate) fn delete_vm_rows(tx: &Tx<'_>, vm: &VmId) -> Result<()> {
    // User snapshots (ADR-0032): their files are under `vms/<vm_id>/`.
    Catalog::snapshot_rows_cleared(tx, vm)?;
    // ADR-0039: its live standing grant is revoked with it.
    tx.revoke_standing_grants_of_vm(vm)?;
    let id = vm.sql();
    let c = tx.conn();
    let steps = [
        "DELETE FROM operations WHERE vm_id = ?1",
        "DELETE FROM artifacts WHERE run_id IN (SELECT run_id FROM runs WHERE vm_id = ?1)",
        "DELETE FROM current_tpm WHERE vm_id = ?1",
        "DELETE FROM vm_current_state WHERE vm_id = ?1",
        "DELETE FROM runs WHERE vm_id = ?1",
        "DELETE FROM disks WHERE vm_id = ?1 AND role = 'overlay'",
        "DELETE FROM grants WHERE vm_id = ?1",
        "DELETE FROM baselines WHERE vm_id = ?1",
        "DELETE FROM disks WHERE vm_id = ?1",
        "DELETE FROM firmware_state WHERE vm_id = ?1",
        "DELETE FROM vms WHERE vm_id = ?1",
    ];
    for sql in steps {
        c.execute(sql, params![id])
            .map_err(|e| match crate::error::map_sqlite(e) {
                StorageError::Constraint(_) => StorageError::Referenced("vm_referenced"),
                other => other,
            })?;
    }
    Ok(())
}
