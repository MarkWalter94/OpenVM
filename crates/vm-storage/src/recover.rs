//! Reconciliation after an interruption (ROADMAP 6.3, 7.4 "Crash" row, 8).
//!
//! Run when the catalog is opened (under the lock) and callable through
//! [`Catalog::recover`]. Rules for unfinished intents:
//!
//! | Kind | Situation found | Action |
//! | --- | --- | --- |
//! | creation | DB row missing | removes the temporary and the orphaned final file → `aborted` |
//! | creation | row present, final file present | completes → `committed` |
//! | creation | row present, file only in the temporary | completes the rename → `committed` |
//! | creation | row present, no file | VM `recovering`, overlay `missing`, runs `failed`; baseline untouched → `aborted` |
//! | deletion | file still in place | rolls back, disk `ready` again → `aborted` |
//! | deletion | file in the temporary, row present | completes the row deletion; if a reference prevents it, restores the file → `committed`/`aborted` |
//! | deletion | no file, row present | same as creation's "row present, no file" → `aborted` |
//! | export | any | removes the temporary → `aborted` (the external destination is never touched) |
//! | any | error during reconciliation (file in use, link, I/O) | intent `needs_review`, VM `recovering` and busy; retried at the next reconciliation |
//!
//! It then removes leftover temporaries of already committed deletions and
//! exports, and checks that the registered disk, firmware and media files
//! exist (managed media copies only: files used in place, ADR-0029, are
//! outside the catalog; a path that cannot be verified is reported as
//! [`IntegrityIssue::Unverifiable`]). A missing base file is only reported:
//! reconciliation never modifies a baseline's rows or files.

use rusqlite::params;

use crate::catalog::{
    Catalog, Intent, exists_no_follow, remove_managed_file, remove_temp, target_exists,
};
use crate::enums::{DiskRole, DiskStatus, IntentKind, IntentState, Lifecycle, TargetKind};
use crate::error::{Result, SqlResultExt, StorageError};
use crate::fsx;
use crate::ids::{ArtifactId, BaselineId, DiskId, FirmwareId, MediaId, VmId, parse_id};
use crate::records::Tx;
use crate::time::now_rfc3339;

/// Overall outcome of reconciliation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Unfinished intents found and resolved.
    pub intents: Vec<IntentRecovery>,
    /// Already committed intents whose leftover temporary was removed.
    pub leftover_temps_removed: Vec<i64>,
    /// Inconsistencies between DB and filesystem outside the journal.
    pub integrity: Vec<IntegrityIssue>,
}

/// Outcome for a single intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRecovery {
    /// Intent.
    pub intent_id: i64,
    /// Kind.
    pub kind: IntentKind,
    /// State found.
    pub prior_state: IntentState,
    /// Class of action.
    pub outcome: RecoveryOutcome,
    /// Detail recorded in `journal.resolution`.
    pub detail: String,
}

/// Class of reconciliation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// Aborted, filesystem restored to the previous state.
    RolledBack,
    /// Completed.
    RolledForward,
    /// DB present and file missing: VM in `recovering`, no change to the baseline.
    MarkedRecovering,
    /// Reconciliation failed (I/O error, file in use, path rejected):
    /// intent `needs_review`, VM `recovering` and busy; retried at the next
    /// reconciliation.
    NeedsReview,
}

/// Inconsistency detected by the integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityIssue {
    /// File of a non-baseline disk missing: disk `missing`, VM `recovering`.
    DiskMissing {
        /// Disk.
        disk_id: String,
        /// Managed path.
        path: String,
    },
    /// File of a base missing: only reported, VM `recovering`.
    BaseDiskMissing {
        /// Disk.
        disk_id: String,
        /// Managed path.
        path: String,
    },
    /// File of a firmware state missing: VM `recovering`.
    FirmwareMissing {
        /// Firmware state.
        fw_id: String,
        /// Managed path.
        path: String,
    },
    /// Managed copy of a media missing: media `missing`.
    MediaMissing {
        /// Media.
        media_id: String,
    },
    /// Presence cannot be verified (path behind a link or I/O error):
    /// needs review, VM in `recovering` if the file belongs to a VM.
    Unverifiable {
        /// Managed path.
        path: String,
        /// Observed error.
        detail: String,
    },
}

impl Catalog {
    /// Reconciles unfinished intents and checks the integrity of the
    /// registered files. Idempotent.
    pub fn recover(&mut self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        for id in self.intent_ids_in("'pending','applied','needs_review'")? {
            let prior = self.intent_state(id)?;
            let kind = self.intent_kind(id)?;
            // An error on one intent does not block opening the catalog:
            // the intent stays "needs review" and the VM busy.
            let attempt = self
                .intent_row(id)
                .and_then(|intent| self.recover_one(&intent, prior));
            let (outcome, detail) = match attempt {
                Ok((outcome, detail, state)) => {
                    self.set_intent_state(id, state, Some(&detail))?;
                    (outcome, detail)
                }
                Err(e) => {
                    let detail = format!("needs_review: {e}");
                    self.mark_needs_review(id, &detail)?;
                    (RecoveryOutcome::NeedsReview, detail)
                }
            };
            report.intents.push(IntentRecovery {
                intent_id: id,
                kind,
                prior_state: prior,
                outcome,
                detail,
            });
        }
        report.leftover_temps_removed = self.remove_committed_leftovers()?;
        report.integrity = self.integrity_scan()?;
        Ok(report)
    }

    fn intent_kind(&self, id: i64) -> Result<IntentKind> {
        self.conn
            .query_row(
                "SELECT kind FROM journal WHERE intent_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .db()
    }

    /// Unresolved intent: state `needs_review` and VM in `recovering`.
    fn mark_needs_review(&mut self, id: i64, detail: &str) -> Result<()> {
        self.with_tx(|tx| {
            tx.conn()
                .execute(
                    "UPDATE journal SET state = 'needs_review', resolution = ?1, updated_at = ?2
                     WHERE intent_id = ?3",
                    params![detail, now_rfc3339(), id],
                )
                .db()?;
            tx.conn()
                .execute(
                    "UPDATE vms SET lifecycle = 'recovering', updated_at = ?1
                     WHERE vm_id = (SELECT vm_id FROM journal WHERE intent_id = ?2)",
                    params![now_rfc3339(), id],
                )
                .db()?;
            Ok(())
        })
    }

    fn recover_one(
        &mut self,
        i: &Intent,
        prior: IntentState,
    ) -> Result<(RecoveryOutcome, String, IntentState)> {
        let temp_exists = exists_no_follow(&i.temp_path)?;
        let final_exists = match &i.final_path {
            Some(p) => exists_no_follow(p)?,
            None => false,
        };
        let row_exists =
            self.read(|tx| target_exists(tx, i.target_kind, i.target_id.as_deref()))?;
        use RecoveryOutcome::*;
        match i.kind {
            k if k.is_create() => {
                if !row_exists {
                    let d = self.rollback_fs(i, prior)?;
                    return Ok((RolledBack, d, IntentState::Aborted));
                }
                if final_exists {
                    if temp_exists {
                        remove_temp(&self.layout, &i.temp_rel)?;
                    }
                    restore_readonly(i)?;
                    return Ok((
                        RolledForward,
                        "rolled_forward".into(),
                        IntentState::Committed,
                    ));
                }
                if temp_exists && let (Some(dest), Some(rel)) = (&i.final_path, &i.final_rel) {
                    self.layout.resolve(rel)?;
                    fsx::publish_no_replace(&i.temp_path, dest, rel)?;
                    restore_readonly(i)?;
                    return Ok((
                        RolledForward,
                        "rename_completed".into(),
                        IntentState::Committed,
                    ));
                }
                self.mark_db_without_file(i)?;
                Ok((
                    MarkedRecovering,
                    "db_without_file".into(),
                    IntentState::Aborted,
                ))
            }
            IntentKind::DeleteFile
                if matches!(i.target_kind, TargetKind::Vm | TargetKind::Baseline) =>
            {
                self.recover_vm_file(i, prior, temp_exists, final_exists, row_exists)
            }
            // A restore that did not commit: the file of the previous
            // current state goes back in place (the new overlay is removed
            // by its own creation intent).
            IntentKind::DiscardFile => {
                let d = self.rollback_fs(i, prior)?;
                Ok((RolledBack, d, IntentState::Aborted))
            }
            IntentKind::DeleteFile => {
                if temp_exists && !final_exists {
                    if !row_exists {
                        remove_temp(&self.layout, &i.temp_rel)?;
                        return Ok((
                            RolledForward,
                            "rolled_forward+temp_removed".into(),
                            IntentState::Committed,
                        ));
                    }
                    let tk = i.target_kind;
                    let tid = i.target_id.clone().unwrap_or_default();
                    match self.with_tx(|tx| delete_target_rows(tx, tk, &tid)) {
                        Ok(()) => {
                            remove_temp(&self.layout, &i.temp_rel)?;
                            Ok((
                                RolledForward,
                                "rolled_forward+temp_removed".into(),
                                IntentState::Committed,
                            ))
                        }
                        Err(StorageError::Referenced(_) | StorageError::Constraint(_)) => {
                            let d = self.rollback_fs(i, prior)?;
                            Ok((
                                RolledBack,
                                format!("delete_refused+{d}"),
                                IntentState::Aborted,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                } else if !temp_exists && !final_exists {
                    if row_exists {
                        self.mark_db_without_file(i)?;
                        Ok((
                            MarkedRecovering,
                            "db_without_file".into(),
                            IntentState::Aborted,
                        ))
                    } else {
                        Ok((
                            RolledForward,
                            "already_deleted".into(),
                            IntentState::Committed,
                        ))
                    }
                } else {
                    let d = self.rollback_fs(i, prior)?;
                    Ok((RolledBack, d, IntentState::Aborted))
                }
            }
            _ => {
                let d = self.rollback_fs(i, prior)?;
                Ok((RolledBack, d, IntentState::Aborted))
            }
        }
    }

    /// One file of a group deletion (target `vm`: VM deletion; target
    /// `baseline`: baseline deletion, F3). The group rolls forward as soon as
    /// one file was moved: the first moved file deletes the rows, the
    /// following ones (moved or still in place) are removed.
    fn recover_vm_file(
        &mut self,
        i: &Intent,
        prior: IntentState,
        temp_exists: bool,
        final_exists: bool,
        row_exists: bool,
    ) -> Result<(RecoveryOutcome, String, IntentState)> {
        use RecoveryOutcome::*;
        match (temp_exists, final_exists, row_exists) {
            // Moved, rows still present: complete the deletion.
            (true, false, true) => {
                let tid = i.target_id.clone().unwrap_or_default();
                let tk = i.target_kind;
                match self.with_tx(|tx| delete_target_rows(tx, tk, &tid)) {
                    Ok(()) => {
                        remove_temp(&self.layout, &i.temp_rel)?;
                        Ok((
                            RolledForward,
                            "vm_rows_deleted+temp_removed".into(),
                            IntentState::Committed,
                        ))
                    }
                    Err(StorageError::Referenced(_) | StorageError::Constraint(_)) => {
                        let d = self.rollback_fs(i, prior)?;
                        Ok((
                            RolledBack,
                            format!("delete_refused+{d}"),
                            IntentState::Aborted,
                        ))
                    }
                    Err(e) => Err(e),
                }
            }
            // Moved and rows already gone: only the temporary remains.
            (true, false, false) => {
                remove_temp(&self.layout, &i.temp_rel)?;
                Ok((
                    RolledForward,
                    "rolled_forward+temp_removed".into(),
                    IntentState::Committed,
                ))
            }
            // Not moved, the group already rolled forward: remove the file.
            (temp, true, false) => {
                if temp {
                    remove_temp(&self.layout, &i.temp_rel)?;
                }
                if let Some(rel) = &i.final_rel {
                    remove_managed_file(&self.layout, rel)?;
                    if i.target_kind == TargetKind::Baseline {
                        crate::catalog::remove_empty_parent(&self.layout, rel);
                    }
                }
                Ok((
                    RolledForward,
                    "vm_deleted+file_removed".into(),
                    IntentState::Committed,
                ))
            }
            // Nothing moved and the VM is still there: nothing to undo.
            (false, true, true) => Ok((
                RolledBack,
                "delete_not_started".into(),
                IntentState::Aborted,
            )),
            // File gone in both places.
            (false, false, true) => Ok((RolledBack, "file_missing".into(), IntentState::Aborted)),
            (false, false, false) => Ok((
                RolledForward,
                "already_deleted".into(),
                IntentState::Committed,
            )),
            // Both copies present with the VM still there: restore.
            (true, true, true) => {
                let d = self.rollback_fs(i, prior)?;
                Ok((RolledBack, d, IntentState::Aborted))
            }
        }
    }

    /// DB present and file missing: VM in `recovering`; a non-base disk
    /// becomes `missing` and the open runs using it become `failed`.
    /// No change to baseline rows or files.
    fn mark_db_without_file(&mut self, i: &Intent) -> Result<()> {
        let vm = i.vm_id.clone();
        let tk = i.target_kind;
        let tid = i.target_id.clone();
        self.with_tx(|tx| {
            if let Some(vm) = vm.as_deref().and_then(|v| v.parse::<VmId>().ok())
                && tx.vm(&vm).is_ok()
            {
                tx.set_vm_lifecycle(&vm, Lifecycle::Recovering)?;
            }
            match (tk, tid.as_deref()) {
                (TargetKind::Disk, Some(id)) => {
                    let disk_id: DiskId = parse_id(id)?;
                    if let Some(d) = tx.disk_opt(&disk_id)?
                        && d.role != DiskRole::Base
                    {
                        tx.set_disk_status(&disk_id, DiskStatus::Missing)?;
                        fail_runs_using(tx, "overlay_disk_id", id)?;
                    }
                }
                (TargetKind::Firmware, Some(id)) => fail_runs_using(tx, "vars_fw_id", id)?,
                (TargetKind::Media, Some(id)) => {
                    tx.conn()
                        .execute(
                            "UPDATE media SET status = 'missing' WHERE media_id = ?1",
                            params![id],
                        )
                        .db()?;
                }
                _ => {}
            }
            Ok(())
        })
    }

    fn remove_committed_leftovers(&mut self) -> Result<Vec<i64>> {
        let ids: Vec<i64> = {
            let mut st = self
                .conn
                .prepare(
                    "SELECT intent_id FROM journal
                     WHERE state = 'committed' AND kind IN ('delete_file','export','discard_file')
                       AND resolution IS NULL",
                )
                .db()?;
            let rows = st.query_map([], |r| r.get(0)).db()?;
            rows.collect::<rusqlite::Result<_>>().db()?
        };
        let mut removed = Vec::new();
        for id in ids {
            // A temporary that cannot be removed (in use, behind a link) stays
            // and will be retried: it does not prevent opening the catalog.
            let Ok(i) = self.intent_row(id) else { continue };
            let removed_now = match exists_no_follow(&i.temp_path) {
                Ok(true) => match remove_temp(&self.layout, &i.temp_rel) {
                    Ok(()) => true,
                    Err(_) => continue,
                },
                Ok(false) => false,
                Err(_) => continue,
            };
            if removed_now {
                removed.push(id);
            }
            self.set_resolution(id, "temp_removed")?;
        }
        Ok(removed)
    }

    fn integrity_scan(&mut self) -> Result<Vec<IntegrityIssue>> {
        let layout = self.layout.clone();
        self.with_tx(|tx| {
            let mut issues = Vec::new();
            for d in tx.all_disks()? {
                if d.status != DiskStatus::Ready {
                    continue;
                }
                match presence(&layout, &d.managed_path, &mut issues)? {
                    Some(true) => continue,
                    None => {
                        tx.set_vm_lifecycle(&d.vm_id, Lifecycle::Recovering)?;
                        continue;
                    }
                    Some(false) => {}
                }
                tx.set_vm_lifecycle(&d.vm_id, Lifecycle::Recovering)?;
                if d.role == DiskRole::Base {
                    issues.push(IntegrityIssue::BaseDiskMissing {
                        disk_id: d.disk_id.to_string(),
                        path: d.managed_path.to_string(),
                    });
                } else {
                    tx.set_disk_status(&d.disk_id, DiskStatus::Missing)?;
                    fail_runs_using(tx, "overlay_disk_id", &d.disk_id.to_string())?;
                    issues.push(IntegrityIssue::DiskMissing {
                        disk_id: d.disk_id.to_string(),
                        path: d.managed_path.to_string(),
                    });
                }
            }
            for f in tx.all_firmware()? {
                match presence(&layout, &f.managed_path, &mut issues)? {
                    Some(true) => continue,
                    None => {
                        tx.set_vm_lifecycle(&f.vm_id, Lifecycle::Recovering)?;
                        continue;
                    }
                    Some(false) => {}
                }
                tx.set_vm_lifecycle(&f.vm_id, Lifecycle::Recovering)?;
                fail_runs_using(tx, "vars_fw_id", &f.fw_id.to_string())?;
                issues.push(IntegrityIssue::FirmwareMissing {
                    fw_id: f.fw_id.to_string(),
                    path: f.managed_path.to_string(),
                });
            }
            let media: Vec<(String, String)> = {
                let mut st = tx
                    .conn()
                    // Managed copies only: a file used in place (ADR-0029)
                    // is outside the catalog; its presence is checked when
                    // the registry is listed and before every use.
                    .prepare(
                        "SELECT media_id, managed_path FROM media
                         WHERE status = 'approved' AND storage = 'managed'",
                    )
                    .db()?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).db()?;
                rows.collect::<rusqlite::Result<_>>().db()?
            };
            for (id, path) in media {
                let rel = crate::paths::ManagedPath::parse(&path)?;
                if presence(&layout, &rel, &mut issues)? == Some(false) {
                    tx.conn()
                        .execute(
                            "UPDATE media SET status = 'missing' WHERE media_id = ?1",
                            params![id],
                        )
                        .db()?;
                    issues.push(IntegrityIssue::MediaMissing { media_id: id });
                }
            }
            Ok(issues)
        })
    }
}

/// Presence of a managed file: `None` if the path cannot be verified
/// (link rejected, I/O error), with the inconsistency recorded.
fn presence(
    layout: &crate::layout::CatalogLayout,
    rel: &crate::paths::ManagedPath,
    issues: &mut Vec<IntegrityIssue>,
) -> Result<Option<bool>> {
    match layout.resolve(rel).and_then(|p| exists_no_follow(&p)) {
        Ok(b) => Ok(Some(b)),
        Err(e @ (StorageError::InvalidPath { .. } | StorageError::Io { .. })) => {
            issues.push(IntegrityIssue::Unverifiable {
                path: rel.to_string(),
                detail: e.to_string(),
            });
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn fail_runs_using(tx: &Tx<'_>, column: &str, id: &str) -> Result<()> {
    let col = match column {
        "overlay_disk_id" => "overlay_disk_id",
        _ => "vars_fw_id",
    };
    tx.conn()
        .execute(
            &format!(
                "UPDATE runs SET state = 'failed', ended_at = ?1, termination_cause = 'recovered'
                 WHERE {col} = ?2 AND ended_at IS NULL"
            ),
            params![now_rfc3339(), id],
        )
        .db()?;
    Ok(())
}

fn delete_target_rows(tx: &Tx<'_>, kind: TargetKind, id: &str) -> Result<()> {
    match kind {
        TargetKind::Disk => tx.delete_disk(&parse_id::<DiskId>(id)?),
        TargetKind::Firmware => tx.delete_firmware(&parse_id::<FirmwareId>(id)?),
        TargetKind::Baseline => tx.delete_baseline(&parse_id::<BaselineId>(id)?),
        TargetKind::Artifact => tx.delete_artifact(&parse_id::<ArtifactId>(id)?),
        TargetKind::Media => tx.delete_media(&parse_id::<MediaId>(id)?),
        TargetKind::Vm => crate::vm_delete::delete_vm_rows(tx, &parse_id::<VmId>(id)?),
        TargetKind::None => Ok(()),
    }
}

/// After a roll-forward: files that are created read-only (bases, baseline
/// vars, media copies) get the attribute back, since removing a temporary
/// hard-linked to the same file may have cleared it.
fn restore_readonly(i: &Intent) -> Result<()> {
    let readonly = match i.kind {
        IntentKind::PromoteBaseline | IntentKind::ImportMedia => true,
        IntentKind::CopyVars => i
            .final_rel
            .as_ref()
            .is_some_and(|r| r.components().any(|c| c == "base")),
        _ => false,
    };
    if readonly && let Some(dest) = &i.final_path {
        fsx::ensure_readonly(dest)?;
    }
    Ok(())
}
