//! Catalog: root, lock, database, intent journal and manifests.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::enums::{DiskRole, DiskStatus, IntentKind, IntentState, TargetKind};
use crate::error::{PathRejection, Result, SqlResultExt, StorageError};
use crate::fsx;
use crate::ids::{BaselineId, IdSql, OperationId, VmId};
use crate::layout::{CatalogLayout, CatalogLock, TMP_DIR, TmpCleanup};
use crate::manifest::{
    self, BaselineManifest, Divergence, MANIFEST_FORMAT_VERSION, ManifestFile, ManifestMedia,
    VmManifest,
};
use crate::migrate::{self, MigrationReport};
use crate::paths::{ManagedPath, is_link};
use crate::records::Tx;
use crate::recover::RecoveryReport;
use crate::time::{UtcMillis, now_rfc3339};

/// Resolution recorded when publishing finds the destination already taken
/// by a file the intent did not create: that file must never be removed.
const DESTINATION_CONFLICT: &str = "destination_conflict";

/// Outcome of opening the catalog.
#[derive(Debug, Clone)]
pub struct OpenReport {
    /// Migrations applied.
    pub migration: MigrationReport,
    /// Previously recorded start (UTC), if any.
    pub previous_start: Option<String>,
    /// Reconciliation of unfinished intents.
    pub recovery: RecoveryReport,
    /// Conservative cleanup of `tmp/`.
    pub tmp_cleanup: TmpCleanup,
}

/// Open catalog: holds the single-instance lock for its whole lifetime.
///
/// Mutations require `&mut self`: together with the catalog lock this
/// serializes the instance's destructive operations.
#[derive(Debug)]
pub struct Catalog {
    // Field order = drop order: the connection before the lock.
    pub(crate) conn: Connection,
    pub(crate) layout: CatalogLayout,
    pub(crate) started_at: UtcMillis,
    report: Option<OpenReport>,
    /// Fault injection for crash tests (`None` in the product).
    pub(crate) fault: Option<(crate::FaultPoint, crate::FaultAction)>,
    _lock: CatalogLock,
}

/// Request for a new journal intent.
#[derive(Debug, Clone)]
pub struct IntentSpec {
    /// Kind.
    pub kind: IntentKind,
    /// VM involved (serializes intents per VM).
    pub vm_id: Option<VmId>,
    /// Operation: intents of the same operation may coexist on the VM.
    pub operation_id: Option<OperationId>,
    /// Kind of row involved.
    pub target_kind: TargetKind,
    /// Identifier of the row involved (to be created or deleted).
    pub target_id: Option<String>,
    /// Managed destination (creation) or source (deletion). `None` for export.
    pub final_path: Option<ManagedPath>,
    /// Sets the read-only attribute before the move (bases, media).
    pub read_only: bool,
}

/// Recorded intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Sequential identifier.
    pub id: i64,
    /// Kind.
    pub kind: IntentKind,
    /// VM.
    pub vm_id: Option<String>,
    /// Operation.
    pub operation_id: Option<String>,
    /// Kind of row involved.
    pub target_kind: TargetKind,
    /// Row involved.
    pub target_id: Option<String>,
    /// Managed temporary path (`tmp/...`).
    pub temp_rel: ManagedPath,
    /// Managed final path.
    pub final_rel: Option<ManagedPath>,
    /// Absolute temporary path: the caller writes the file to be created there.
    pub temp_path: PathBuf,
    /// Absolute final path.
    pub final_path: Option<PathBuf>,
    read_only: bool,
}

impl Catalog {
    /// Opens (or creates) the catalog in `root`:
    /// exclusive lock → migrations (with backup) → journal reconciliation →
    /// conservative cleanup of `tmp/`.
    pub fn open(root: &Path) -> Result<Self> {
        let layout = CatalogLayout::prepare(root)?;
        let lock = CatalogLock::acquire(layout.root())?;
        let db_path = layout.db_path();
        let mut conn = migrate::open_connection(&db_path)?;
        let migration = migrate::migrate(&mut conn, &db_path)?;
        let started_at = UtcMillis::now();
        let previous_start: Option<String> = conn
            .query_row(
                "SELECT value FROM catalog_meta WHERE key = 'last_start'",
                [],
                |r| r.get(0),
            )
            .optional()
            .db()?;
        conn.execute(
            "INSERT INTO catalog_meta(key, value) VALUES ('last_start', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![started_at.rfc3339()],
        )
        .db()?;
        let mut cat = Self {
            conn,
            layout,
            started_at,
            report: None,
            fault: None,
            _lock: lock,
        };
        let recovery = cat.recover()?;
        let keep = cat.open_intent_temp_paths()?;
        let tmp_cleanup = cat.layout.clean_tmp(started_at, &keep)?;
        cat.report = Some(OpenReport {
            migration,
            previous_start,
            recovery,
            tmp_cleanup,
        });
        Ok(cat)
    }

    /// Writes the whole WAL into the database file and closes the
    /// connection, keeping the single-instance lock: the database file is
    /// then complete on its own and nobody else can open the catalog
    /// (catalog move, [`crate::move_catalog`]).
    pub(crate) fn close_keep_lock(self) -> Result<CatalogLock> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                r.get::<_, i64>(0)
            })
            .db()?;
        let Self { conn, _lock, .. } = self;
        conn.close().map_err(|(_, e)| StorageError::Sqlite(e))?;
        Ok(_lock)
    }

    /// Outcome of opening.
    pub fn open_report(&self) -> Option<&OpenReport> {
        self.report.as_ref()
    }

    /// Root layout.
    pub fn layout(&self) -> &CatalogLayout {
        &self.layout
    }

    /// Instant this instance was opened.
    pub fn started_at(&self) -> UtcMillis {
        self.started_at
    }

    /// Current schema version.
    pub fn schema_version(&self) -> Result<u32> {
        migrate::current_version(&self.conn)
    }

    /// Runs `f` in an `IMMEDIATE` transaction, committed if `f` succeeds.
    pub fn with_tx<T>(&mut self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let tx = Tx {
            inner: self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db()?,
        };
        let out = f(&tx)?;
        tx.inner.commit().db()?;
        Ok(out)
    }

    /// Read inside a transaction (rolled back at the end).
    pub fn read<T>(&mut self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let tx = Tx {
            inner: self.conn.transaction().db()?,
        };
        f(&tx)
    }

    // ------------------------------------------------------------ journal

    /// Records a `pending` intent before any filesystem change.
    ///
    /// Checks: valid paths without links; for creation the destination must
    /// not exist and the row must not exist; for deletion the source must
    /// exist and the row must not be referenced; only one group of open
    /// intents per VM ([`StorageError::VmBusy`]).
    pub fn begin_intent(&mut self, spec: IntentSpec) -> Result<Intent> {
        let temp_rel = ManagedPath::parse(&format!(
            "{TMP_DIR}/{}-{}.part",
            spec.kind.as_str(),
            uuid::Uuid::now_v7().simple()
        ))?;
        let temp_path = self.layout.resolve(&temp_rel)?;
        let final_path = match &spec.final_path {
            Some(p) => {
                if p.is_under(TMP_DIR) || p.as_str() == TMP_DIR {
                    return Err(StorageError::invalid_path(
                        p.as_str(),
                        PathRejection::OutsideArea,
                    ));
                }
                Some(self.layout.resolve(p)?)
            }
            None => None,
        };
        let layout = self.layout.clone();
        self.with_tx(|tx| {
            check_vm_free(tx, spec.vm_id.as_ref(), spec.operation_id.as_ref())?;
            match spec.kind {
                k if k.is_create() => {
                    let (Some(fp), Some(rel)) = (&final_path, &spec.final_path) else {
                        return Err(StorageError::StateConflict(
                            "creation intent without destination".into(),
                        ));
                    };
                    if spec.target_kind == TargetKind::None || spec.target_id.is_none() {
                        return Err(StorageError::StateConflict(
                            "creation intent without target row".into(),
                        ));
                    }
                    if target_exists(tx, spec.target_kind, spec.target_id.as_deref())? {
                        return Err(StorageError::StateConflict(
                            "target row already present".into(),
                        ));
                    }
                    if exists_no_follow(fp)? {
                        return Err(StorageError::StateConflict(format!(
                            "destination already exists: {rel}"
                        )));
                    }
                    if let Some((parent, _)) = rel.as_str().rsplit_once('/') {
                        layout.ensure_dir(&ManagedPath::parse(parent)?)?;
                    }
                }
                IntentKind::DiscardFile => {
                    let (Some(fp), Some(rel)) = (&final_path, &spec.final_path) else {
                        return Err(StorageError::StateConflict("discard without source".into()));
                    };
                    if !exists_no_follow(fp)? {
                        return Err(StorageError::NotFound {
                            kind: "file",
                            id: rel.to_string(),
                        });
                    }
                    discard_precheck(tx, spec.target_kind, spec.target_id.as_deref())?;
                }
                IntentKind::DeleteFile => {
                    let (Some(fp), Some(rel)) = (&final_path, &spec.final_path) else {
                        return Err(StorageError::StateConflict(
                            "deletion without source".into(),
                        ));
                    };
                    if !exists_no_follow(fp)? {
                        return Err(StorageError::NotFound {
                            kind: "file",
                            id: rel.to_string(),
                        });
                    }
                    deletion_precheck(tx, spec.target_kind, spec.target_id.as_deref())?;
                    if spec.target_kind == TargetKind::Disk
                        && let Some(id) = &spec.target_id
                    {
                        tx.conn()
                            .execute(
                                "UPDATE disks SET status = 'deleting' WHERE disk_id = ?1",
                                params![id],
                            )
                            .db()?;
                    }
                }
                _ => {}
            }
            let now = now_rfc3339();
            tx.conn()
                .execute(
                    "INSERT INTO journal(kind, state, vm_id, operation_id, target_kind, target_id,
                                         temp_path, final_path, created_at, updated_at)
                     VALUES (?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![
                        spec.kind,
                        spec.vm_id.sql(),
                        spec.operation_id.sql(),
                        spec.target_kind,
                        spec.target_id,
                        temp_rel.as_str(),
                        spec.final_path.as_ref().map(ManagedPath::as_str),
                        now
                    ],
                )
                .db()?;
            Ok(Intent {
                id: tx.conn().last_insert_rowid(),
                kind: spec.kind,
                vm_id: spec.vm_id.as_ref().map(VmId::to_string),
                operation_id: spec.operation_id.as_ref().map(OperationId::to_string),
                target_kind: spec.target_kind,
                target_id: spec.target_id.clone(),
                temp_rel: temp_rel.clone(),
                final_rel: spec.final_path.clone(),
                temp_path: temp_path.clone(),
                final_path: final_path.clone(),
                read_only: spec.read_only,
            })
        })
    }

    /// Records a `pending` user snapshot intent (ADR-0032) with its JSON
    /// payload: only one group of open intents per VM
    /// ([`StorageError::VmBusy`]). The temporary is a directory created
    /// here (`tmp/<kind>-<uuid>.part/`); the file rules of
    /// [`Self::begin_intent`] do not apply: these intents are settled by
    /// `snapshot.rs` from the disk's QCOW2 snapshot table.
    pub(crate) fn begin_snapshot_intent(
        &mut self,
        kind: IntentKind,
        vm: &VmId,
        target_id: &str,
        final_path: Option<ManagedPath>,
        payload: &serde_json::Value,
    ) -> Result<Intent> {
        if !kind.is_snapshot() {
            return Err(StorageError::StateConflict(
                "not a snapshot intent kind".into(),
            ));
        }
        let temp_rel = ManagedPath::parse(&format!(
            "{TMP_DIR}/{}-{}.part",
            kind.as_str(),
            uuid::Uuid::now_v7().simple()
        ))?;
        let temp_path = self.layout.resolve(&temp_rel)?;
        let final_abs = final_path
            .as_ref()
            .map(|p| self.layout.resolve(p))
            .transpose()?;
        let payload = payload.to_string();
        let intent = self.with_tx(|tx| {
            check_vm_free(tx, Some(vm), None)?;
            let now = now_rfc3339();
            tx.conn()
                .execute(
                    "INSERT INTO journal(kind, state, vm_id, operation_id, target_kind, target_id,
                                         temp_path, final_path, payload, created_at, updated_at)
                     VALUES (?1, 'pending', ?2, NULL, 'snapshot', ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![
                        kind,
                        vm.sql(),
                        target_id,
                        temp_rel.as_str(),
                        final_path.as_ref().map(ManagedPath::as_str),
                        payload,
                        now
                    ],
                )
                .db()?;
            Ok(Intent {
                id: tx.conn().last_insert_rowid(),
                kind,
                vm_id: Some(vm.to_string()),
                operation_id: None,
                target_kind: TargetKind::Snapshot,
                target_id: Some(target_id.to_owned()),
                temp_rel: temp_rel.clone(),
                final_rel: final_path.clone(),
                temp_path: temp_path.clone(),
                final_path: final_abs.clone(),
                read_only: false,
            })
        })?;
        if let Err(e) = std::fs::create_dir(&intent.temp_path) {
            let err = StorageError::io(&intent.temp_path, e);
            let _ =
                self.set_intent_state(intent.id, IntentState::Aborted, Some("temp_not_created"));
            return Err(err);
        }
        Ok(intent)
    }

    /// JSON payload of an intent (user snapshot intents, schema v10).
    pub(crate) fn intent_payload(&self, id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT payload FROM journal WHERE intent_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .db()
    }

    /// Completes the filesystem part and moves to `applied`.
    ///
    /// - creation: the temporary file (regular, not a link) is synced,
    ///   optionally made read-only and renamed onto the destination, which
    ///   must not exist;
    /// - deletion: the source is renamed into the temporary;
    /// - export: the temporary must exist.
    pub fn mark_applied(&mut self, intent: &Intent) -> Result<()> {
        self.expect_state(intent.id, &[IntentState::Pending])?;
        match intent.kind {
            k if k.is_create() => {
                let dest = intent
                    .final_path
                    .as_ref()
                    .ok_or_else(|| StorageError::StateConflict("missing destination".into()))?;
                let meta = std::fs::symlink_metadata(&intent.temp_path)
                    .map_err(|e| StorageError::io(&intent.temp_path, e))?;
                if is_link(&meta) || !meta.is_file() {
                    return Err(StorageError::invalid_path(
                        intent.temp_rel.as_str(),
                        PathRejection::Link,
                    ));
                }
                sync_file(&intent.temp_path)?;
                // Re-check for links on the destination path.
                let rel = intent
                    .final_rel
                    .as_ref()
                    .ok_or_else(|| StorageError::StateConflict("missing destination".into()))?;
                self.layout.resolve(rel)?;
                // Publish without overwriting (hard link): a destination that
                // appeared in the meantime is never replaced.
                if let Err(e) = fsx::publish_no_replace(&intent.temp_path, dest, rel) {
                    if matches!(e, StorageError::StateConflict(_)) {
                        // The destination is not ours: abort and
                        // reconciliation must not remove it.
                        self.set_resolution(intent.id, DESTINATION_CONFLICT)?;
                    }
                    return Err(e);
                }
                if intent.read_only {
                    fsx::ensure_readonly(dest)?;
                }
            }
            IntentKind::DeleteFile | IntentKind::DiscardFile => {
                let src = intent
                    .final_path
                    .as_ref()
                    .ok_or_else(|| StorageError::StateConflict("missing source".into()))?;
                if let Some(rel) = &intent.final_rel {
                    self.layout.resolve(rel)?;
                }
                std::fs::rename(src, &intent.temp_path).map_err(|e| StorageError::io(src, e))?;
            }
            _ => {
                if !exists_no_follow(&intent.temp_path)? {
                    return Err(StorageError::NotFound {
                        kind: "temporary file",
                        id: intent.temp_rel.to_string(),
                    });
                }
            }
        }
        self.set_intent_state(intent.id, IntentState::Applied, None)
    }

    /// Applies the DB effect and commits the intent in the same transaction.
    /// See [`Self::commit_many`].
    pub fn commit(&mut self, intent: &Intent, f: impl FnOnce(&Tx<'_>) -> Result<()>) -> Result<()> {
        self.commit_many(std::slice::from_ref(intent), f)
    }

    /// Applies the DB effect of several `applied` intents and commits them all
    /// in the same SQL transaction. After `f` it checks that the target rows
    /// exist (creation) or no longer exist (deletion); otherwise it rolls back
    /// the transaction and returns an error, leaving the intents `applied`
    /// (to be aborted or reconciled). After the commit it removes the
    /// temporaries of deletions and exports.
    pub fn commit_many(
        &mut self,
        intents: &[Intent],
        f: impl FnOnce(&Tx<'_>) -> Result<()>,
    ) -> Result<()> {
        for i in intents {
            self.expect_state(i.id, &[IntentState::Applied])?;
        }
        self.with_tx(|tx| {
            f(tx)?;
            for i in intents {
                let present = target_exists(tx, i.target_kind, i.target_id.as_deref())?;
                let ok = match i.kind {
                    k if k.is_create() => present,
                    IntentKind::DeleteFile | IntentKind::DiscardFile => {
                        !present || i.target_kind == TargetKind::None
                    }
                    _ => true,
                };
                if !ok {
                    return Err(StorageError::StateConflict(format!(
                        "intent {}: DB effect inconsistent with {}",
                        i.id, i.kind
                    )));
                }
                set_state_tx(tx, i.id, IntentState::Committed, None)?;
            }
            Ok(())
        })?;
        self.fault_at(crate::FaultPoint::AfterCommit)?;
        for i in intents {
            if matches!(
                i.kind,
                IntentKind::DeleteFile | IntentKind::DiscardFile | IntentKind::Export
            ) && remove_temp(&self.layout, &i.temp_rel).is_ok()
            {
                self.set_resolution(i.id, "temp_removed")?;
            }
        }
        Ok(())
    }

    /// Aborts a `pending` or `applied` intent, restoring the filesystem.
    pub fn abort(&mut self, intent: &Intent) -> Result<()> {
        let state = self.expect_state(intent.id, &[IntentState::Pending, IntentState::Applied])?;
        let row = self.intent_row(intent.id)?;
        let resolution = self.rollback_fs(&row, state)?;
        self.set_intent_state(intent.id, IntentState::Aborted, Some(&resolution))
    }

    /// Current state of an intent.
    pub fn intent_state(&self, id: i64) -> Result<IntentState> {
        self.conn
            .query_row(
                "SELECT state FROM journal WHERE intent_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .db()?
            .ok_or_else(|| StorageError::NotFound {
                kind: "intent",
                id: id.to_string(),
            })
    }

    /// Resolution recorded by reconciliation or by abort.
    pub fn intent_resolution(&self, id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT resolution FROM journal WHERE intent_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .db()
    }

    /// Unfinished intents (`pending`, `applied` or `needs_review`).
    pub fn open_intents(&self) -> Result<Vec<Intent>> {
        self.intent_ids_in("'pending','applied','needs_review'")?
            .into_iter()
            .map(|id| self.intent_row(id))
            .collect()
    }

    /// Intents that reconciliation could not resolve ("needs review"): the
    /// VM involved stays busy. Details of the last attempt are in
    /// [`Self::intent_resolution`].
    pub fn intents_needing_review(&self) -> Result<Vec<i64>> {
        self.intent_ids_in("'needs_review'")
    }

    /// Identifiers of intents in the given states (fixed SQL literals).
    pub(crate) fn intent_ids_in(&self, states: &'static str) -> Result<Vec<i64>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT intent_id FROM journal WHERE state IN ({states}) ORDER BY intent_id"
            ))
            .db()?;
        let rows = st.query_map([], |r| r.get(0)).db()?;
        rows.collect::<rusqlite::Result<_>>().db()
    }

    pub(crate) fn intent_row(&self, id: i64) -> Result<Intent> {
        type JournalRow = (
            IntentKind,
            Option<String>,
            Option<String>,
            TargetKind,
            Option<String>,
            String,
            Option<String>,
        );
        let (kind, vm, op, tk, tid, temp, fin): JournalRow = self
            .conn
            .query_row(
                "SELECT kind, vm_id, operation_id, target_kind, target_id, temp_path, final_path
                 FROM journal WHERE intent_id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .db()?;
        let temp_rel = ManagedPath::parse(&temp)?;
        if !temp_rel.is_under(TMP_DIR) {
            return Err(StorageError::invalid_path(temp, PathRejection::OutsideArea));
        }
        let final_rel = fin.as_deref().map(ManagedPath::parse).transpose()?;
        Ok(Intent {
            id,
            kind,
            vm_id: vm,
            operation_id: op,
            target_kind: tk,
            target_id: tid,
            temp_path: self.layout.resolve(&temp_rel)?,
            final_path: final_rel
                .as_ref()
                .map(|r| self.layout.resolve(r))
                .transpose()?,
            temp_rel,
            final_rel,
            read_only: false,
        })
    }

    /// Temporaries of unfinished intents, not to be touched by the `tmp/`
    /// cleanup. Read as text: an intent with unresolvable paths (for
    /// example behind a link) does not prevent the catalog from opening.
    fn open_intent_temp_paths(&self) -> Result<Vec<ManagedPath>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT temp_path FROM journal
                 WHERE state IN ('pending','applied','needs_review')",
            )
            .db()?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).db()?;
        let texts = rows.collect::<rusqlite::Result<Vec<_>>>().db()?;
        Ok(texts
            .iter()
            .filter_map(|t| ManagedPath::parse(t).ok())
            .collect())
    }

    fn expect_state(&self, id: i64, allowed: &[IntentState]) -> Result<IntentState> {
        let s = self.intent_state(id)?;
        if allowed.contains(&s) {
            Ok(s)
        } else {
            Err(StorageError::StateConflict(format!(
                "intent {id} in state {s}"
            )))
        }
    }

    pub(crate) fn set_intent_state(
        &mut self,
        id: i64,
        state: IntentState,
        resolution: Option<&str>,
    ) -> Result<()> {
        self.with_tx(|tx| set_state_tx(tx, id, state, resolution))
    }

    pub(crate) fn set_resolution(&mut self, id: i64, resolution: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE journal SET resolution = ?1, updated_at = ?2 WHERE intent_id = ?3",
                params![resolution, now_rfc3339(), id],
            )
            .db()?;
        Ok(())
    }

    /// Filesystem rollback (used by `abort` and by reconciliation).
    pub(crate) fn rollback_fs(&mut self, i: &Intent, _state: IntentState) -> Result<String> {
        let temp_exists = exists_no_follow(&i.temp_path)?;
        let final_exists = match &i.final_path {
            Some(p) => exists_no_follow(p)?,
            None => false,
        };
        let row_exists =
            self.read(|tx| target_exists(tx, i.target_kind, i.target_id.as_deref()))?;
        match i.kind {
            k if k.is_create() => {
                let mut parts = Vec::new();
                if temp_exists {
                    remove_temp(&self.layout, &i.temp_rel)?;
                    parts.push("temp_removed");
                }
                let foreign_final =
                    self.intent_resolution(i.id)?.as_deref() == Some(DESTINATION_CONFLICT);
                if final_exists && !row_exists && foreign_final {
                    parts.push("foreign_final_kept");
                } else if final_exists && !row_exists {
                    if let Some(rel) = &i.final_rel {
                        remove_managed_file(&self.layout, rel)?;
                        if i.kind == IntentKind::PromoteBaseline {
                            remove_baseline_leftovers(&self.layout, rel)?;
                        }
                    }
                    parts.push("orphan_final_removed");
                } else if !final_exists
                    && !row_exists
                    && i.kind == IntentKind::PromoteBaseline
                    && let Some(rel) = &i.final_rel
                {
                    // Interrupted before publishing: the new baseline's
                    // directory (created by `begin_intent`) and any
                    // manifest must be removed.
                    remove_baseline_leftovers(&self.layout, rel)?;
                }
                // An import that did not commit leaves no VM directory
                // (created by `begin_intent`) behind.
                if i.kind == IntentKind::ImportVm
                    && !row_exists
                    && let Some(rel) = &i.final_rel
                {
                    remove_empty_parent(&self.layout, rel);
                }
                if parts.is_empty() {
                    parts.push("nothing_to_undo");
                }
                Ok(parts.join("+"))
            }
            IntentKind::DeleteFile | IntentKind::DiscardFile => {
                let res = if temp_exists && !final_exists {
                    let (Some(src), Some(rel)) = (&i.final_path, &i.final_rel) else {
                        return Err(StorageError::StateConflict("missing source".into()));
                    };
                    self.layout.resolve(rel)?;
                    std::fs::rename(&i.temp_path, src).map_err(|e| StorageError::io(src, e))?;
                    "file_restored"
                } else if temp_exists {
                    remove_temp(&self.layout, &i.temp_rel)?;
                    "stray_temp_removed"
                } else {
                    "delete_not_started"
                };
                if i.target_kind == TargetKind::Disk
                    && let Some(id) = &i.target_id
                {
                    let id = id.clone();
                    self.with_tx(|tx| {
                        tx.conn()
                            .execute(
                                "UPDATE disks SET status = 'ready'
                                     WHERE disk_id = ?1 AND status = 'deleting'",
                                params![id],
                            )
                            .db()?;
                        Ok(())
                    })?;
                }
                Ok(res.to_owned())
            }
            _ => {
                if temp_exists {
                    remove_temp(&self.layout, &i.temp_rel)?;
                    Ok("temp_removed".into())
                } else {
                    Ok("nothing_to_undo".into())
                }
            }
        }
    }

    // ----------------------------------------------------------- manifest

    /// Rewrites the VM manifest from the DB and records its hash.
    pub fn write_vm_manifest(&mut self, vm: &VmId) -> Result<String> {
        let m = self.read(|tx| build_vm_manifest(tx, vm))?;
        let sha = manifest::write_atomic(&self.layout, &CatalogLayout::vm_manifest(vm), &m)?;
        self.with_tx(|tx| tx.set_vm_manifest_hash(vm, &sha))?;
        Ok(sha)
    }

    /// Compares the VM manifest with the DB.
    pub fn verify_vm_manifest(&mut self, vm: &VmId) -> Result<Vec<Divergence>> {
        let layout = self.layout.clone();
        self.read(|tx| {
            let rec = tx.vm(vm)?;
            let mut out = Vec::new();
            let file: Option<VmManifest> = manifest::read_checked(
                &layout,
                &CatalogLayout::vm_manifest(vm),
                rec.manifest_sha256.as_deref(),
                &mut out,
            )?;
            if let Some(file) = file {
                let expected = build_vm_manifest(tx, vm)?;
                if file.format_version > MANIFEST_FORMAT_VERSION {
                    out.push(Divergence::UnsupportedFormat(file.format_version));
                }
                compare_vm(&file, &expected, &mut out);
            }
            Ok(out)
        })
    }

    /// Compares a baseline manifest with the DB. With `deep` it also
    /// recomputes the SHA-256 of the base disk and of the vars (full read).
    pub fn verify_baseline_manifest(
        &mut self,
        id: &BaselineId,
        deep: bool,
    ) -> Result<Vec<Divergence>> {
        let layout = self.layout.clone();
        self.read(|tx| {
            let b = tx.baseline(id)?;
            let mut out = Vec::new();
            let rel = CatalogLayout::baseline_manifest(&b.vm_id, id);
            let file: Option<BaselineManifest> =
                manifest::read_checked(&layout, &rel, Some(&b.manifest_sha256), &mut out)?;
            let disk = tx.disk(&b.base_disk_id)?;
            let vars = b.vars_fw_id.as_ref().map(|f| tx.firmware(f)).transpose()?;
            if let Some(file) = &file {
                if file.format_version > MANIFEST_FORMAT_VERSION {
                    out.push(Divergence::UnsupportedFormat(file.format_version));
                }
                if file.baseline_id != b.baseline_id {
                    out.push(Divergence::FieldMismatch("baseline_id"));
                }
                if file.vm_id != b.vm_id {
                    out.push(Divergence::FieldMismatch("vm_id"));
                }
                if file.label != b.label {
                    out.push(Divergence::FieldMismatch("label"));
                }
                if file.clean_shutdown != b.clean_shutdown || !file.created_powered_off {
                    out.push(Divergence::FieldMismatch("shutdown"));
                }
                if file.disk.path != disk.managed_path.as_str() || file.disk.sha256 != disk.sha256 {
                    out.push(Divergence::FieldMismatch("disk"));
                }
                let vars_db = vars
                    .as_ref()
                    .map(|v| (v.managed_path.as_str().to_owned(), v.sha256.clone()));
                let vars_file = file
                    .vars
                    .as_ref()
                    .map(|v| (v.path.clone(), v.sha256.clone()));
                if vars_db != vars_file {
                    out.push(Divergence::FieldMismatch("vars"));
                }
            }
            let mut files = vec![(disk.managed_path.clone(), disk.sha256.clone())];
            if let Some(v) = &vars {
                files.push((v.managed_path.clone(), v.sha256.clone()));
            }
            for (rel, sha) in files {
                let p = layout.resolve(&rel)?;
                if !exists_no_follow(&p)? {
                    out.push(Divergence::DataFileMissing(rel.to_string()));
                } else if deep && sha.as_deref() != Some(manifest::sha256_file(&p)?.as_str()) {
                    out.push(Divergence::DataHashMismatch(rel.to_string()));
                }
            }
            Ok(out)
        })
    }
}

fn compare_vm(file: &VmManifest, expected: &VmManifest, out: &mut Vec<Divergence>) {
    let checks: [(&'static str, bool); 9] = [
        ("vm_id", file.vm_id == expected.vm_id),
        ("name", file.name == expected.name),
        ("guest_os", file.guest_os == expected.guest_os),
        (
            "config_version",
            file.config_version == expected.config_version,
        ),
        (
            "config_revision",
            file.config_revision == expected.config_revision,
        ),
        ("config", file.config == expected.config),
        ("disks", file.disks == expected.disks),
        ("media", file.media == expected.media),
        ("baselines", file.baselines == expected.baselines),
    ];
    for (name, ok) in checks {
        if !ok {
            out.push(Divergence::FieldMismatch(name));
        }
    }
}

pub(crate) fn build_vm_manifest(tx: &Tx<'_>, vm: &VmId) -> Result<VmManifest> {
    let rec = tx.vm(vm)?;
    let mut disks: Vec<ManifestFile> = tx
        .disks_of_vm(vm)?
        .into_iter()
        .filter(|d| d.role != DiskRole::Base && d.status != DiskStatus::Deleting)
        .map(|d| ManifestFile {
            role: manifest::role_name(d.role),
            path: d.managed_path.to_string(),
            sha256: d.sha256,
        })
        .collect();
    disks.sort_by(|a, b| a.path.cmp(&b.path));
    let mut media = Vec::new();
    for (slot, id) in tx.vm_media(vm)? {
        let m = tx.media(&id)?;
        media.push(ManifestMedia {
            slot,
            media_id: id,
            sha256: m.sha256,
        });
    }
    let mut baselines: Vec<BaselineId> = tx
        .baselines_of_vm(vm)?
        .into_iter()
        .map(|b| b.baseline_id)
        .collect();
    baselines.sort();
    Ok(VmManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        kind: "vm".into(),
        vm_id: rec.vm_id,
        name: rec.name,
        guest_os: rec.guest_os,
        config_version: rec.config_version,
        config_revision: rec.revision,
        config: rec.config,
        disks,
        media,
        baselines,
    })
}

pub(crate) fn set_state_tx(
    tx: &Tx<'_>,
    id: i64,
    state: IntentState,
    resolution: Option<&str>,
) -> Result<()> {
    tx.conn()
        .execute(
            "UPDATE journal SET state = ?1, resolution = coalesce(?2, resolution), updated_at = ?3
             WHERE intent_id = ?4",
            params![state, resolution, now_rfc3339(), id],
        )
        .db()?;
    Ok(())
}

fn check_vm_free(tx: &Tx<'_>, vm: Option<&VmId>, op: Option<&OperationId>) -> Result<()> {
    let Some(vm) = vm else { return Ok(()) };
    let mut st = tx
        .conn()
        .prepare(
            "SELECT operation_id FROM journal
             WHERE vm_id = ?1 AND state IN ('pending','applied','needs_review')",
        )
        .db()?;
    let ops = st
        .query_map(params![vm.sql()], |r| r.get::<_, Option<String>>(0))
        .db()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .db()?;
    let mine = op.map(OperationId::to_string);
    if ops.iter().any(|o| o.is_none() || *o != mine) {
        return Err(StorageError::VmBusy(vm.to_string()));
    }
    Ok(())
}

pub(crate) fn target_exists(tx: &Tx<'_>, kind: TargetKind, id: Option<&str>) -> Result<bool> {
    let Some(id) = id else { return Ok(false) };
    let sql = match kind {
        TargetKind::Disk => "SELECT 1 FROM disks WHERE disk_id = ?1",
        TargetKind::Firmware => "SELECT 1 FROM firmware_state WHERE fw_id = ?1",
        TargetKind::Baseline => "SELECT 1 FROM baselines WHERE baseline_id = ?1",
        TargetKind::Artifact => return tx.artifact_exists(id),
        TargetKind::Media => "SELECT 1 FROM media WHERE media_id = ?1",
        TargetKind::Vm => "SELECT 1 FROM vms WHERE vm_id = ?1",
        TargetKind::Snapshot => "SELECT 1 FROM snapshots WHERE snapshot_id = ?1",
        TargetKind::None => return Ok(false),
    };
    tx.exists(sql, id)
}

/// A file of the VM's previous current state can be discarded only if no
/// open run uses it (the restore requires a stopped VM).
fn discard_precheck(tx: &Tx<'_>, kind: TargetKind, id: Option<&str>) -> Result<()> {
    let Some(id) = id else { return Ok(()) };
    let sql = match kind {
        TargetKind::Disk => {
            "SELECT EXISTS (SELECT 1 FROM runs WHERE overlay_disk_id = ?1 AND ended_at IS NULL)"
        }
        TargetKind::Firmware => {
            "SELECT EXISTS (SELECT 1 FROM runs WHERE vars_fw_id = ?1 AND ended_at IS NULL)"
        }
        TargetKind::None => return Ok(()),
        _ => {
            return Err(StorageError::StateConflict(
                "discard of an unsupported target".into(),
            ));
        }
    };
    let used: bool = tx.conn().query_row(sql, params![id], |r| r.get(0)).db()?;
    if used {
        return Err(StorageError::Referenced("disk_referenced"));
    }
    Ok(())
}

fn deletion_precheck(tx: &Tx<'_>, kind: TargetKind, id: Option<&str>) -> Result<()> {
    let Some(id) = id else { return Ok(()) };
    let referenced = |sql: &str| -> Result<bool> {
        tx.conn()
            .query_row(sql, params![id], |r| r.get::<_, bool>(0))
            .db()
    };
    match kind {
        TargetKind::Disk => {
            if referenced(
                "SELECT EXISTS (SELECT 1 FROM baselines WHERE base_disk_id = ?1)
                     OR EXISTS (SELECT 1 FROM disks WHERE backing_disk_id = ?1)
                     OR EXISTS (SELECT 1 FROM runs WHERE overlay_disk_id = ?1)",
            )? {
                return Err(StorageError::Referenced("disk_referenced"));
            }
        }
        TargetKind::Baseline => {
            if referenced("SELECT protected FROM baselines WHERE baseline_id = ?1")? {
                return Err(StorageError::Referenced("baseline_protected"));
            }
            let bid: BaselineId = id
                .parse()
                .map_err(|_| StorageError::Constraint("invalid baseline_id".into()))?;
            if tx.baseline_referenced(&bid)? {
                return Err(StorageError::Referenced("baseline_referenced"));
            }
        }
        TargetKind::Firmware => {
            if referenced(
                "SELECT EXISTS (SELECT 1 FROM baselines WHERE vars_fw_id = ?1)
                     OR EXISTS (SELECT 1 FROM runs WHERE vars_fw_id = ?1)",
            )? {
                return Err(StorageError::Referenced("firmware_referenced"));
            }
        }
        TargetKind::Media => {
            if referenced("SELECT EXISTS (SELECT 1 FROM vm_media WHERE media_id = ?1)")? {
                return Err(StorageError::Referenced("media_referenced"));
            }
        }
        TargetKind::Vm => {
            let vm: VmId = id
                .parse()
                .map_err(|_| StorageError::Constraint("invalid vm_id".into()))?;
            crate::vm_delete::check_deletable(tx, &vm)?;
        }
        TargetKind::Artifact | TargetKind::Snapshot | TargetKind::None => {}
    }
    Ok(())
}

pub(crate) fn exists_no_follow(p: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(p) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(StorageError::io(p, e)),
    }
}

fn sync_file(p: &Path) -> Result<()> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(p)
        .map_err(|e| StorageError::io(p, e))?;
    f.sync_all().map_err(|e| StorageError::io(p, e))
}

/// Removes a file or directory under `tmp/`, without following links.
pub(crate) fn remove_temp(layout: &CatalogLayout, rel: &ManagedPath) -> Result<()> {
    if !rel.is_under(TMP_DIR) {
        return Err(StorageError::invalid_path(
            rel.as_str(),
            PathRejection::OutsideArea,
        ));
    }
    let p = layout.resolve(rel)?;
    match std::fs::symlink_metadata(&p) {
        Ok(m) if is_link(&m) => Err(StorageError::invalid_path(
            rel.as_str(),
            PathRejection::Link,
        )),
        Ok(m) if m.is_dir() => remove_dir_no_links(&p),
        Ok(m) => force_remove_file(&p, m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StorageError::io(&p, e)),
    }
}

/// Removes a regular managed file (never a directory or a link).
pub(crate) fn remove_managed_file(layout: &CatalogLayout, rel: &ManagedPath) -> Result<()> {
    let p = layout.resolve(rel)?;
    match std::fs::symlink_metadata(&p) {
        Ok(m) if is_link(&m) || !m.is_file() => Err(StorageError::invalid_path(
            rel.as_str(),
            PathRejection::Link,
        )),
        Ok(m) => force_remove_file(&p, m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StorageError::io(&p, e)),
    }
}

/// Removes the parent directory of a managed file if it is empty (best
/// effort: an empty directory left behind is harmless).
pub(crate) fn remove_empty_parent(layout: &CatalogLayout, rel: &ManagedPath) {
    if let Some((dir, _)) = rel.as_str().rsplit_once('/')
        && let Ok(dir_rel) = ManagedPath::parse(dir)
        && let Ok(p) = layout.resolve(&dir_rel)
        && let Ok(mut it) = std::fs::read_dir(&p)
        && it.next().is_none()
    {
        let _ = std::fs::remove_dir(&p);
    }
}

/// After removing an orphaned base: removes the manifest next to it and the
/// baseline directory if empty. Nothing else.
fn remove_baseline_leftovers(layout: &CatalogLayout, disk_rel: &ManagedPath) -> Result<()> {
    let Some((dir, _)) = disk_rel.as_str().rsplit_once('/') else {
        return Ok(());
    };
    let dir_rel = ManagedPath::parse(dir)?;
    let manifest_rel = dir_rel.join("manifest.json")?;
    remove_managed_file(layout, &manifest_rel)?;
    let dir_path = layout.resolve(&dir_rel)?;
    if let Ok(mut it) = std::fs::read_dir(&dir_path)
        && it.next().is_none()
    {
        let _ = std::fs::remove_dir(&dir_path);
    }
    Ok(())
}

#[allow(clippy::permissions_set_readonly_false)]
pub(crate) fn force_remove_file(p: &Path, meta: std::fs::Metadata) -> Result<()> {
    // Bases and media are made read-only (mitigation F0-02 §8.4): the
    // attribute must be cleared before removing an orphaned managed file.
    let mut perm = meta.permissions();
    if perm.readonly() {
        perm.set_readonly(false);
        std::fs::set_permissions(p, perm).map_err(|e| StorageError::io(p, e))?;
    }
    std::fs::remove_file(p).map_err(|e| StorageError::io(p, e))
}

pub(crate) fn remove_dir_no_links(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| StorageError::io(dir, e))? {
        let entry = entry.map_err(|e| StorageError::io(dir, e))?;
        let p = entry.path();
        let m = std::fs::symlink_metadata(&p).map_err(|e| StorageError::io(&p, e))?;
        if is_link(&m) {
            // Removes the link itself, never its target
            // (`remove_dir` for junctions and directory links).
            if std::fs::remove_file(&p).is_err() {
                std::fs::remove_dir(&p).map_err(|e| StorageError::io(&p, e))?;
            }
        } else if m.is_dir() {
            remove_dir_no_links(&p)?;
        } else {
            force_remove_file(&p, m)?;
        }
    }
    std::fs::remove_dir(dir).map_err(|e| StorageError::io(dir, e))
}
