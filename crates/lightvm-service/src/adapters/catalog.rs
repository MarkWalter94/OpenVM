//! Adapter `vm_core::ports::CatalogStore` â†’ `vm_storage::Catalog` (SQLite).
//!
//! The core and the storage use the same identifiers from `lightvm-proto` but
//! different records; the adapter converts both ways and covers three gaps in
//! the current `vm-storage` API (reported in the F1-04 verification file):
//!
//! 1. `Tx::insert_vm` generates the `VmId` itself, while the core chooses it
//!    through the `IdGenerator` port: the core's `VmRecord` is kept in
//!    `config_json` (`{"core": VmRecord}`) and the adapter maintains the
//!    mapping core `VmId` â†’ storage `VmId`, rebuilt on open;
//! 2. `Catalog` reads require `&mut self` while the port's reads are `&self`:
//!    the catalog lives in a `RefCell` (a single thread, the supervisor, owns
//!    the adapter);
//! 3. the storage operation does not keep the session and does not update the
//!    `run_id` after insertion; `finish_run` uses the storage's timestamp.
//!
//! No path comes from clients: the managed media paths are validated twice
//! (`ManagedRelPath` of the proto and `ManagedPath` of the storage). The only
//! host path received over IPC is the file the user picked for
//! `import_media` (UI role only), checked by [`validate_import_source`],
//! opened read-only by the storage and, since ADR-0029, used in place: the
//! path recorded is the one the system resolved for the opened handle
//! ([`reference_path`]), refused if QEMU's option syntax could misread it,
//! and the file is re-checked and re-hashed through a guard handle before
//! every start.
//!
//! F2: media import in two steps around the hash computed outside the
//! supervisor thread ([`crate::engine::MediaImportPort`]), VM deletion
//! through the storage journal (`remove_vm`), disk usage of a VM
//! (`vm_disk_usage`); input operations are not stored in the `operations`
//! table (see `upsert_operation`).
//!
//! F3: baselines ([`crate::engine::BaselinePort`]): creation and restore in
//! three steps on the storage journal with the backend's image tool on the
//! worker thread, deletion, current-state files for a start, clean exits of
//! the runs. New VMs are stored under the core's identifier
//! (`insert_vm_with_id`), so all their files live under one `vms/<vm_id>/`;
//! VMs created before keep the two directories (mapping above).
//!
//! ADR-0027: the guest OS of an installation ISO is detected at import on
//! the hashed file, through the handle still held, and stored with the
//! registry row; media imported before are examined once when the registry
//! is listed. Baselines of a legacy BIOS VM carry no UEFI variables.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use std::any::Any;
use std::sync::Arc;

use lightvm_proto::ipc::{
    BaselineSummary, SnapshotSummary, SnapshotTree, VmBaselines, VmSnapshots,
};
use lightvm_proto::{
    BaselineId, DetectedGuest, ErrorCode, Label, ManagedRelPath, MediaEntry, MediaId, MediaKind,
    MediaLocation, MediaRecord, OperationId, OperationKind, OperationRecord, OperationStatus,
    PublicError, RunEndReason, RunId, RunRecord, Sha256Digest, SnapshotId, UtcTimestamp, VmConfig,
    VmId, VmRecord, VmState,
};
use serde::{Deserialize, Serialize};
use vm_core::ports::CatalogStore;
use vm_storage as st;

use crate::engine::{
    BaselineOutcome, BaselinePort, CancelCheck, GuardedMedia, ImageTool, ImportCopied, ImportJob,
    MediaGuardPort, MediaImportPort as _, SnapshotStep, StartDisks, StartGuard, StorageWork,
};

/// Version of the `config_json` format written by this adapter.
const CONFIG_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct StoredConfig {
    /// Full core record (identifier, configuration, revision).
    core: VmRecord,
}

/// Converts a storage error into a public error without host paths.
pub fn storage_error(e: &st::StorageError) -> PublicError {
    if let st::StorageError::NotFound { kind, .. } = e {
        return PublicError::not_found(format!("{kind} not found in the catalog"));
    }
    let code = e
        .public_code()
        .and_then(|c| serde_json::from_value::<ErrorCode>(serde_json::Value::String(c.into())).ok())
        .unwrap_or(ErrorCode::InfrastructureError);
    // The storage text may contain paths: only the code goes to clients.
    PublicError::new(code, "catalog error")
}

fn infra(what: &str) -> PublicError {
    PublicError::infrastructure(format!("catalog: {what}"))
}

fn lifecycle(s: VmState) -> st::Lifecycle {
    match s {
        VmState::Stopped => st::Lifecycle::Stopped,
        VmState::Starting => st::Lifecycle::Starting,
        VmState::Running => st::Lifecycle::Running,
        VmState::Stopping => st::Lifecycle::Stopping,
        VmState::Failed => st::Lifecycle::Failed,
        VmState::Recovering => st::Lifecycle::Recovering,
    }
}

fn vm_state(l: st::Lifecycle) -> VmState {
    match l {
        st::Lifecycle::Stopped => VmState::Stopped,
        st::Lifecycle::Starting => VmState::Starting,
        st::Lifecycle::Running => VmState::Running,
        st::Lifecycle::Stopping => VmState::Stopping,
        st::Lifecycle::Failed => VmState::Failed,
        st::Lifecycle::Recovering => VmState::Recovering,
    }
}

fn op_state(s: OperationStatus) -> st::OperationState {
    match s {
        OperationStatus::Queued => st::OperationState::Queued,
        OperationStatus::Forwarded => st::OperationState::Running,
        OperationStatus::Succeeded => st::OperationState::Succeeded,
        OperationStatus::Failed => st::OperationState::Failed,
        OperationStatus::Discarded => st::OperationState::Cancelled,
    }
}

fn op_status(s: st::OperationState) -> OperationStatus {
    match s {
        st::OperationState::Queued => OperationStatus::Queued,
        st::OperationState::Running => OperationStatus::Forwarded,
        st::OperationState::Succeeded => OperationStatus::Succeeded,
        st::OperationState::Failed => OperationStatus::Failed,
        st::OperationState::Cancelled => OperationStatus::Discarded,
    }
}

fn run_end(reason: RunEndReason) -> (st::RunState, st::TerminationCause) {
    match reason {
        RunEndReason::Stopped => (st::RunState::Stopped, st::TerminationCause::Shutdown),
        RunEndReason::ForceStopped => (st::RunState::Stopped, st::TerminationCause::ForceStop),
        RunEndReason::EmergencyStop => (st::RunState::Stopped, st::TerminationCause::EmergencyStop),
        RunEndReason::StartFailed => (st::RunState::Failed, st::TerminationCause::StartFailed),
        RunEndReason::UnexpectedExit => (st::RunState::Failed, st::TerminationCause::Crash),
        RunEndReason::Reconciled => (st::RunState::Stopped, st::TerminationCause::Recovered),
    }
}

fn snake<T: Serialize>(v: &T) -> Result<String, PublicError> {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        _ => Err(infra("enumeration not serializable")),
    }
}

fn from_snake<T: for<'de> Deserialize<'de>>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

fn ts(s: &str) -> Result<UtcTimestamp, PublicError> {
    s.parse().map_err(|_| infra("invalid timestamp"))
}

/// SQLite catalog of `vm-storage` as seen by the core.
#[derive(Debug)]
pub struct SqliteStore {
    cat: RefCell<st::Catalog>,
    /// Core `VmId` â†’ `VmId` of the row in the storage.
    ids: BTreeMap<VmId, VmId>,
}

impl SqliteStore {
    /// Opens (or creates) the catalog in `root`: catalog lock, migrations,
    /// journal reconciliation (see `vm_storage::Catalog::open`).
    pub fn open(root: &Path) -> Result<Self, st::StorageError> {
        let mut cat = st::Catalog::open(root)?;
        let ids = cat.read(|tx| {
            let mut map = BTreeMap::new();
            for sid in tx.vm_ids()? {
                let row = tx.vm(&sid)?;
                if let Ok(c) = serde_json::from_value::<StoredConfig>(row.config) {
                    map.insert(c.core.id, sid);
                }
            }
            Ok(map)
        })?;
        Ok(Self {
            cat: RefCell::new(cat),
            ids,
        })
    }

    /// Catalog root (for the managed media paths).
    pub fn root(&self) -> PathBuf {
        self.cat.borrow().layout().root().to_path_buf()
    }

    fn sid(&self, id: VmId) -> Result<VmId, PublicError> {
        self.ids
            .get(&id)
            .copied()
            .ok_or_else(|| PublicError::not_found("VM not found in the catalog"))
    }

    fn core_id(&self, sid: VmId) -> Option<VmId> {
        self.ids
            .iter()
            .find_map(|(core, s)| (*s == sid).then_some(*core))
    }

    fn read<T>(&self, f: impl FnOnce(&st::Tx<'_>) -> st::Result<T>) -> Result<T, PublicError> {
        self.cat.borrow_mut().read(f).map_err(|e| storage_error(&e))
    }

    fn write<T>(&self, f: impl FnOnce(&st::Tx<'_>) -> st::Result<T>) -> Result<T, PublicError> {
        self.cat
            .borrow_mut()
            .with_tx(f)
            .map_err(|e| storage_error(&e))
    }

    fn vm_record(row: st::VmRecord) -> Result<VmRecord, PublicError> {
        let stored: StoredConfig =
            serde_json::from_value(row.config).map_err(|_| infra("unreadable VM configuration"))?;
        let mut vm = stored.core;
        vm.persisted_state = vm_state(row.lifecycle);
        Ok(vm)
    }

    /// Registry entry of a row: approved media, and the `missing` ones used
    /// in place (ADR-0029: shown as missing, usable again once the file is
    /// back); a managed copy that went missing or a revoked medium is not
    /// listed.
    fn media_entry(m: st::MediaRecord) -> Result<Option<MediaEntry>, PublicError> {
        let referenced = matches!(m.location, st::MediaLocation::Referenced(_));
        let missing = m.status == st::MediaStatus::Missing;
        if !(m.status == st::MediaStatus::Approved || (missing && referenced)) {
            return Ok(None);
        }
        let (location, shown) = match m.location {
            st::MediaLocation::Managed(rel) => (
                MediaLocation::Managed(
                    ManagedRelPath::parse(rel.as_str())
                        .map_err(|_| infra("invalid managed path"))?,
                ),
                None,
            ),
            st::MediaLocation::Referenced(p) => {
                let shown = p.to_string_lossy().into_owned();
                (MediaLocation::Referenced(p), Some(shown))
            }
        };
        let kind = match m.kind {
            st::MediaKind::Iso => MediaKind::InstallIso,
            st::MediaKind::Data => MediaKind::DataIso,
        };
        Ok(Some(MediaEntry {
            record: MediaRecord {
                id: m.media_id,
                kind,
                sha256: Sha256Digest::parse_hex(&m.sha256).map_err(|_| infra("invalid hash"))?,
                size_bytes: m.size_bytes,
                declared_origin: Label::parse(m.declared_origin)
                    .map_err(|_| infra("invalid origin"))?,
                approved_at: ts(&m.approved_at)?,
                // Installation media only: a data ISO is never a guest hint.
                detected_guest: match kind {
                    MediaKind::InstallIso => m.detected_guest,
                    MediaKind::DataIso => None,
                },
                location: shown,
                missing,
            },
            location,
            // TODO(F1): the storage does not keep the OS identity of the file.
            file_identity: None,
        }))
    }
}

impl CatalogStore for SqliteStore {
    fn insert_vm(&mut self, vm: &VmRecord) -> Result<(), PublicError> {
        let config = serde_json::to_value(StoredConfig { core: vm.clone() })
            .map_err(|_| infra("configuration not serializable"))?;
        let new = st::NewVm {
            name: vm.config.name.as_str().to_owned(),
            guest_os: snake(&vm.config.guest_os)?,
            config_version: CONFIG_VERSION,
            config,
        };
        let state = lifecycle(vm.persisted_state);
        let id = vm.id;
        let sid = self.write(|tx| {
            let sid = tx.insert_vm_with_id(&id, &new)?;
            tx.set_vm_lifecycle(&sid, state)?;
            Ok(sid)
        })?;
        self.ids.insert(vm.id, sid);
        Ok(())
    }

    fn close_open_runs(&mut self, id: VmId, reason: RunEndReason) -> Result<u32, PublicError> {
        let sid = self.sid(id)?;
        let (state, cause) = run_end(reason);
        self.write(|tx| {
            let runs = tx.open_runs(&sid)?;
            for r in &runs {
                tx.end_run(r, state, cause)?;
            }
            Ok(u32::try_from(runs.len()).unwrap_or(u32::MAX))
        })
    }

    fn has_open_intents(&self, id: VmId) -> Result<bool, PublicError> {
        let Some(sid) = self.ids.get(&id).copied() else {
            return Ok(false);
        };
        let sid = sid.to_string();
        let intents = self
            .cat
            .borrow()
            .open_intents()
            .map_err(|e| storage_error(&e))?;
        Ok(intents
            .iter()
            .any(|i| i.vm_id.as_deref() == Some(sid.as_str())))
    }

    fn update_vm(&mut self, vm: &VmRecord, expected_revision: u64) -> Result<(), PublicError> {
        let sid = self.sid(vm.id)?;
        let config = serde_json::to_value(StoredConfig { core: vm.clone() })
            .map_err(|_| infra("configuration not serializable"))?;
        // The catalog revision follows the core's: both start at 1 and grow by
        // 1 at every change.
        self.write(|tx| {
            tx.update_vm_config(&sid, expected_revision, CONFIG_VERSION, &config)
                .map(|_| ())
        })
    }

    fn get_vm(&self, id: VmId) -> Result<Option<VmRecord>, PublicError> {
        let Some(sid) = self.ids.get(&id).copied() else {
            return Ok(None);
        };
        Self::vm_record(self.read(|tx| tx.vm(&sid))?).map(Some)
    }

    fn list_vms(&self) -> Result<Vec<VmRecord>, PublicError> {
        let rows = self.read(|tx| {
            tx.vm_ids()?
                .iter()
                .map(|sid| tx.vm(sid))
                .collect::<st::Result<Vec<_>>>()
        })?;
        // As in `open`: an unreadable row (foreign or older format) is reported
        // and skipped, without preventing the service from starting.
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let id = row.vm_id;
                Self::vm_record(row)
                    .inspect_err(|_| {
                        crate::diag!(
                            "lightvm-service: VM {id} with unreadable configuration: ignored"
                        );
                    })
                    .ok()
            })
            .collect())
    }

    fn set_persisted_state(&mut self, id: VmId, state: VmState) -> Result<(), PublicError> {
        let sid = self.sid(id)?;
        self.write(|tx| tx.set_vm_lifecycle(&sid, lifecycle(state)))
    }

    fn insert_media(&mut self, entry: &MediaEntry) -> Result<(), PublicError> {
        let r = &entry.record;
        let rec = st::MediaRecord {
            media_id: r.id,
            sha256: r.sha256.to_hex(),
            size_bytes: r.size_bytes,
            kind: match r.kind {
                MediaKind::InstallIso => st::MediaKind::Iso,
                MediaKind::DataIso => st::MediaKind::Data,
            },
            declared_origin: r.declared_origin.as_str().to_owned(),
            approved_at: r.approved_at.to_string(),
            location: match &entry.location {
                MediaLocation::Managed(rel) => st::MediaLocation::Managed(
                    st::ManagedPath::parse(rel.as_str()).map_err(|e| storage_error(&e))?,
                ),
                MediaLocation::Referenced(p) => st::MediaLocation::Referenced(p.clone()),
            },
            status: st::MediaStatus::Approved,
            guest_examined: r.detected_guest.is_some(),
            detected_guest: r.detected_guest.clone(),
        };
        self.write(|tx| tx.insert_media(&rec))
    }

    fn get_media(&self, id: MediaId) -> Result<Option<MediaEntry>, PublicError> {
        match self.cat.borrow_mut().read(|tx| tx.media(&id)) {
            Ok(m) => Self::media_entry(m),
            Err(st::StorageError::NotFound { .. }) => Ok(None),
            Err(e) => Err(storage_error(&e)),
        }
    }

    /// Input batches are not persisted as catalog operations: they are never
    /// retried, carry no recovery state, and the core keeps them in memory
    /// for idempotency; each one stays in the audit. Writing three synced
    /// SQLite transactions per keystroke made console typing slow (F2-01).
    fn upsert_operation(&mut self, op: &OperationRecord) -> Result<(), PublicError> {
        if op.kind == OperationKind::Input {
            return Ok(());
        }
        let sid = self.sid(op.vm_id)?;
        let state = op_state(op.status);
        let error = op.error.map(ErrorCode::as_str);
        let rec = st::OperationRecord {
            operation_id: op.id,
            vm_id: sid,
            run_id: op.run_id,
            session_id: op.session_id,
            kind: snake(&op.kind)?,
            state,
            revision: 1,
            error_code: error.map(str::to_owned),
            created_at: op.requested_at.to_string(),
            updated_at: op.updated_at.to_string(),
        };
        let id = op.id;
        self.write(|tx| match tx.operation(&id) {
            Ok(_) => tx.set_operation_state(&id, state, error),
            Err(st::StorageError::NotFound { .. }) => tx.insert_operation(&rec),
            Err(e) => Err(e),
        })
    }

    fn get_operation(&self, id: OperationId) -> Result<Option<OperationRecord>, PublicError> {
        let row = match self.cat.borrow_mut().read(|tx| tx.operation(&id)) {
            Ok(r) => r,
            Err(st::StorageError::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(storage_error(&e)),
        };
        let vm_id = self
            .core_id(row.vm_id)
            .ok_or_else(|| infra("operation of an unknown VM"))?;
        let kind: OperationKind =
            from_snake(&row.kind).ok_or_else(|| infra("unknown operation kind"))?;
        Ok(Some(OperationRecord {
            id: row.operation_id,
            vm_id,
            run_id: row.run_id,
            session_id: row.session_id,
            kind,
            status: op_status(row.state),
            error: row.error_code.as_deref().and_then(from_snake),
            requested_at: ts(&row.created_at)?,
            updated_at: ts(&row.updated_at)?,
        }))
    }

    fn insert_run(&mut self, run: &RunRecord) -> Result<(), PublicError> {
        let sid = self.sid(run.vm_id)?;
        // F3: a run of a VM restored from a baseline references the current
        // overlay, its UEFI variables and the baseline (checked at start).
        let (baseline_id, overlay_disk_id, vars_fw_id) = self
            .cat
            .borrow_mut()
            .current_state_run_refs(&sid)
            .map_err(|e| storage_error(&e))?;
        let rec = st::RunRecord {
            run_id: run.id,
            vm_id: sid,
            baseline_id,
            overlay_disk_id,
            vars_fw_id,
            state: st::RunState::Starting,
            started_at: run.started_at.to_string(),
            ended_at: None,
            termination_cause: None,
        };
        self.write(|tx| tx.insert_run(&rec))
    }

    fn finish_run(
        &mut self,
        id: RunId,
        _ended_at: UtcTimestamp,
        reason: RunEndReason,
    ) -> Result<(), PublicError> {
        let (state, cause) = run_end(reason);
        self.write(|tx| tx.end_run(&id, state, cause))
    }

    /// Installation media approved before the guest detection existed
    /// (`guest_examined = 0`) are examined here, once: the managed copy is
    /// read (bounded, see `guest_detect`) and the outcome persisted, so the
    /// next listings do not read it again.
    ///
    /// ADR-0029: the presence of every file used in place is looked up
    /// (one `stat`, links not followed, nothing read) and the status kept
    /// in step: `missing` when it is gone, `approved` again when it is back
    /// (its hash is verified before the next start anyway).
    fn list_media(&self) -> Result<Vec<MediaRecord>, PublicError> {
        let mut rows = self.read(|tx| {
            tx.media_ids()?
                .iter()
                .map(|id| tx.media(id))
                .collect::<st::Result<Vec<_>>>()
        })?;
        for row in &mut rows {
            let st::MediaLocation::Referenced(p) = &row.location else {
                continue;
            };
            let present = std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
            let status = match (row.status, present) {
                (st::MediaStatus::Approved, false) => st::MediaStatus::Missing,
                (st::MediaStatus::Missing, true) => st::MediaStatus::Approved,
                _ => continue,
            };
            match self.write(|tx| tx.set_media_status(&row.media_id, status)) {
                Ok(()) => row.status = status,
                Err(e) => crate::diag!(
                    "lightvm-service: status of media {} not recorded: {}",
                    row.media_id,
                    e.message
                ),
            }
        }
        let root = self.root();
        for row in rows.iter_mut().filter(|r| {
            !r.guest_examined
                && r.kind == st::MediaKind::Iso
                && r.status == st::MediaStatus::Approved
        }) {
            let copy = match &row.location {
                st::MediaLocation::Managed(rel) => managed_abs(&root, rel.as_str()),
                st::MediaLocation::Referenced(p) => p.clone(),
            };
            let guest = detect_guest(&copy, &format!("media {}", row.media_id));
            match self.write(|tx| tx.set_media_detected_guest(&row.media_id, guest.as_ref())) {
                Ok(()) => {
                    row.detected_guest = guest;
                    row.guest_examined = true;
                }
                Err(e) => crate::diag!(
                    "lightvm-service: guest detection of media {} not recorded: {}",
                    row.media_id,
                    e.message
                ),
            }
        }
        let mut out = Vec::new();
        for row in rows {
            if let Some(e) = Self::media_entry(row)? {
                out.push(e.record);
            }
        }
        Ok(out)
    }

    fn import_media(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &Label,
    ) -> Result<MediaEntry, PublicError> {
        let job = self.begin_import(source, kind, declared_origin)?;
        let copied = job.run();
        self.finish_import(copied)
    }

    /// Deletion of the registry row; a read-only managed copy is removed
    /// with it through the storage journal, a file used in place is never
    /// touched (ADR-0029). The storage refusals keep a readable message
    /// without paths.
    fn remove_media(&mut self, id: MediaId) -> Result<(), PublicError> {
        self.cat.borrow_mut().delete_media(&id).map_err(|e| {
            crate::diag!("lightvm-service: media deletion: {e}");
            match e {
                st::StorageError::VmBusy(_) => {
                    PublicError::vm_busy("the medium is open by a running VM or another operation")
                }
                st::StorageError::StateConflict(m) if m.starts_with("the medium is used by") => {
                    PublicError::state_conflict(m).with_detail("reason", "media_in_use")
                }
                e => storage_error(&e),
            }
        })?;
        crate::diag!("lightvm-service: media {id} deleted");
        Ok(())
    }

    /// Deletion through the storage journal: rows of the VM, `vms/<id>`
    /// under both identifiers (storage and core) and `runs/<run_id>` of
    /// every run of the VM.
    fn remove_vm(&mut self, id: VmId) -> Result<(), PublicError> {
        let sid = self.sid(id)?;
        let mut extra = Vec::new();
        if sid != id {
            extra.push(st::CatalogLayout::vm_dir(&id));
        }
        let report = self
            .cat
            .borrow_mut()
            .delete_vm_with_files(&sid, &extra)
            .map_err(|e| {
                crate::diag!("lightvm-service: VM deletion: {e}");
                storage_error(&e)
            })?;
        crate::diag!(
            "lightvm-service: VM deleted: {} files ({} MiB), {} directories",
            report.files_removed,
            report.bytes_removed >> 20,
            report.dirs_removed
        );
        self.ids.remove(&id);
        Ok(())
    }

    /// Bytes of `vms/<id>` (both identifiers) and of the run directories.
    fn vm_disk_usage(&self, id: VmId) -> Option<u64> {
        let sid = self.ids.get(&id).copied()?;
        let runs = self.read(|tx| tx.run_ids_of_vm(&sid)).ok()?;
        let root = self.root();
        let rel = |m: st::ManagedPath| managed_abs(&root, m.as_str());
        let mut total = tree_bytes(&rel(st::CatalogLayout::vm_dir(&id)));
        if sid != id {
            total += tree_bytes(&rel(st::CatalogLayout::vm_dir(&sid)));
        }
        for r in runs {
            total += tree_bytes(&root.join("runs").join(r.to_string()));
        }
        Some(total)
    }
}

/// Free space that must remain on the catalog volume after a baseline (the
/// reserve for manifests and the minimal outcome, ROADMAP 8).
const BASELINE_FREE_MARGIN: u64 = 1 << 30;

/// Baselines of a TPM guest (ADR-0028, required evidence 3): refused until
/// the swtpm state is part of the baseline.
fn tpm_baseline_unsupported() -> PublicError {
    PublicError::unsupported(
        "baselines of VMs with a TPM (Windows 11) are not supported yet: the TPM state is not part of the baseline",
    )
    .with_detail("reason", "tpm_state")
}

/// Public error of a baseline operation, without paths.
fn baseline_error(e: &st::StorageError) -> PublicError {
    match e {
        st::StorageError::Unsupported(m) if *m == st::TPM_BASELINE_UNSUPPORTED => {
            tpm_baseline_unsupported()
        }
        st::StorageError::Referenced(r @ ("baseline_referenced" | "baseline_protected")) => {
            PublicError::state_conflict(if *r == "baseline_referenced" {
                "the baseline is in use by the VM's current state"
            } else {
                "the baseline is protected"
            })
            .with_detail("reason", *r)
        }
        st::StorageError::IntegrityMismatch { .. } => {
            crate::diag!("lightvm-service: baseline integrity: {e}");
            PublicError::state_conflict("baseline files altered or missing: verification failed")
                .with_detail("reason", "integrity")
        }
        st::StorageError::InvalidPath { .. } => {
            crate::diag!("lightvm-service: baseline integrity: {e}");
            PublicError::state_conflict("baseline files replaced by a link")
                .with_detail("reason", "integrity")
        }
        st::StorageError::VmBusy(_) => {
            crate::diag!("lightvm-service: baseline: {e}");
            PublicError::vm_busy(
                "a disk of the VM is open by another program, or another storage operation is in progress",
            )
        }
        st::StorageError::NotFound { kind, .. } => {
            PublicError::not_found(format!("{kind} not found"))
        }
        st::StorageError::StateConflict(m) if m == st::RESTORE_WITH_SNAPSHOTS => {
            PublicError::state_conflict(
                "the VM has snapshots: restoring a baseline would discard the disk that holds them; delete the snapshots first",
            )
            .with_detail("reason", "snapshots_exist")
        }
        st::StorageError::StateConflict(m) if m.contains("no working disk") => {
            PublicError::state_conflict(
                "the VM has no disk yet: start it once before taking a baseline",
            )
        }
        st::StorageError::Io { source, .. } if source.kind() == std::io::ErrorKind::StorageFull => {
            PublicError::resource_limit("not enough free space in the catalog")
        }
        other => {
            crate::diag!("lightvm-service: baseline: {other}");
            storage_error(other)
        }
    }
}

/// Error of the image tool kept beside the storage error of a job.
type ToolError = Arc<std::sync::Mutex<Option<PublicError>>>;

fn stash(slot: &ToolError, e: PublicError) -> st::StorageError {
    *slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
    st::StorageError::Unsupported("image tool failed")
}

/// Result of the worker step of a baseline job on the SQLite catalog.
enum SqliteJobDone {
    Create(st::BaselineJobDone, ToolError),
    Restore(st::RestoreJobDone, ToolError),
    /// User snapshot operation (ADR-0032) and the result of its disk
    /// operation; settled from the disk either way.
    Snapshot(Box<SnapshotJob>, Result<(), PublicError>),
}

/// Kind of user snapshot operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapKind {
    Take,
    Revert,
    Delete,
}

/// A user snapshot operation recorded in the catalog.
struct SnapshotJob {
    op: st::SnapshotOp,
    kind: SnapKind,
    /// Core identifier of the VM.
    vm: VmId,
    /// New record of the VM after a revert.
    record: Option<VmRecord>,
}

fn tool_error(slot: &ToolError) -> Option<PublicError> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn parse_sha(s: &str) -> Result<Sha256Digest, PublicError> {
    Sha256Digest::parse_hex(s).map_err(|_| infra("invalid hash"))
}

impl SqliteStore {
    /// Standalone working files of a VM (F2 layout, core identifier): they
    /// are produced by the backend. A legacy BIOS VM has no UEFI variables
    /// (ADR-0027): its baselines and restored states carry none.
    fn standalone(vm: &VmRecord) -> st::CurrentStateFiles {
        st::CurrentStateFiles {
            disk: st::CatalogLayout::primary_disk(&vm.id),
            vars: vm
                .config
                .firmware
                .has_nvram()
                .then(|| st::CatalogLayout::primary_vars(&vm.id)),
        }
    }

    fn summary_of(vm: VmId, i: st::BaselineInfo) -> Result<BaselineSummary, PublicError> {
        let label = Label::parse(i.record.label.clone())
            .or_else(|_| Label::parse("baseline"))
            .map_err(|_| infra("invalid label"))?;
        Ok(BaselineSummary {
            baseline_id: i.record.baseline_id,
            vm_id: vm,
            label,
            created_at: ts(&i.record.created_at)?,
            clean_shutdown: i.record.clean_shutdown,
            protected: i.record.protected,
            derived_from: i.derived_from,
            config_revision: i.config_revision.unwrap_or(0),
            virtual_size_bytes: i.disk.virtual_size_bytes,
            size_bytes: i
                .disk
                .allocated_bytes
                .saturating_add(i.vars.as_ref().map_or(0, |v| v.size_bytes)),
            disk_sha256: parse_sha(i.disk.sha256.as_deref().unwrap_or_default())?,
            firmware_sha256: i
                .vars
                .as_ref()
                .and_then(|v| v.sha256.as_deref())
                .map(parse_sha)
                .transpose()?,
            current: i.current,
            in_use: i.in_use,
        })
    }

    /// Bytes the VM uses on the host besides its standalone disk (overlays,
    /// baselines, UEFI variables): what `limits.storage_budget_gib` covers.
    fn budget_usage(&self, vm: VmId) -> u64 {
        let total = self.vm_disk_usage(vm).unwrap_or(0);
        let primary = managed_abs(&self.root(), st::CatalogLayout::primary_disk(&vm).as_str());
        let primary = std::fs::metadata(primary).map_or(0, |m| m.len());
        total.saturating_sub(primary)
    }

    /// Budget and free-space check before a baseline of `estimate` bytes.
    fn check_baseline_space(&self, vm: &VmRecord, estimate: u64) -> Result<(), PublicError> {
        if let Some(gib) = vm.config.limits.storage_budget_gib {
            let need = self.budget_usage(vm.id).saturating_add(estimate);
            if need > gib << 30 {
                return Err(PublicError::resource_limit(
                    "VM storage budget not enough for the baseline",
                )
                .with_detail("needed_bytes", need.to_string())
                .with_detail("budget_gib", gib.to_string()));
            }
        }
        let free = qemu_backend::ManagedPath::verify(self.root())
            .and_then(|r| qemu_backend::prepare::host_resources(&r))
            .map(|r| r.free_disk_bytes);
        if let Ok(free) = free
            && free < estimate.saturating_add(BASELINE_FREE_MARGIN)
        {
            return Err(PublicError::resource_limit(
                "not enough free space in the catalog for the baseline",
            )
            .with_detail("needed_bytes", estimate.to_string())
            .with_detail("free_bytes", free.to_string()));
        }
        Ok(())
    }
}

impl BaselinePort for SqliteStore {
    fn list_baselines(&self, vm: VmId) -> Result<Vec<BaselineSummary>, PublicError> {
        let sid = self.sid(vm)?;
        let infos = self
            .cat
            .borrow_mut()
            .baseline_infos(&sid)
            .map_err(|e| baseline_error(&e))?;
        infos.into_iter().map(|i| Self::summary_of(vm, i)).collect()
    }

    fn baseline_facts(&self, vm: VmId) -> VmBaselines {
        let Ok(sid) = self.sid(vm) else {
            return VmBaselines::default();
        };
        match self.cat.borrow_mut().baseline_overview(&sid) {
            Ok(o) => VmBaselines {
                current: o.current,
                count: o.count,
                bytes: o.bytes,
                overlay_bytes: o.overlay_bytes,
                operation: None,
            },
            Err(e) => {
                crate::diag!("lightvm-service: baseline facts: {e}");
                VmBaselines::default()
            }
        }
    }

    fn begin_create_baseline(
        &mut self,
        vm: &VmRecord,
        label: &Label,
        allow_unclean: bool,
        tool: Arc<dyn ImageTool>,
        cancel: CancelCheck,
    ) -> Result<StorageWork, PublicError> {
        if vm.config.guest_os.needs_tpm() {
            return Err(tpm_baseline_unsupported());
        }
        let sid = self.sid(vm.id)?;
        let clean = self
            .cat
            .borrow_mut()
            .last_exit_clean(&sid)
            .map_err(|e| baseline_error(&e))?;
        if !clean && !allow_unclean {
            return Err(PublicError::state_conflict(
                "the last stop of the VM was not an observed clean shutdown: the baseline would not be verified",
            )
            .with_detail("reason", "unclean_shutdown"));
        }
        // A UEFI VM that never started has no UEFI variables yet: they are
        // created from the firmware template (as the first start would), so
        // that every baseline of a UEFI VM carries its firmware state. A
        // legacy BIOS VM has none (ADR-0027).
        let has_overlay = self
            .cat
            .borrow_mut()
            .baseline_overview(&sid)
            .map(|o| o.current.is_some())
            .unwrap_or(false);
        if !has_overlay
            && vm.config.firmware.has_nvram()
            && let Some(template) = tool.firmware_vars_template()
        {
            let vars = managed_abs(
                &self.root(),
                st::CatalogLayout::primary_vars(&vm.id).as_str(),
            );
            let disk = managed_abs(
                &self.root(),
                st::CatalogLayout::primary_disk(&vm.id).as_str(),
            );
            if std::fs::symlink_metadata(&disk).is_ok() {
                crate::adapters::backend::ensure_vm_vars(&template, &vars).map_err(|e| {
                    crate::diag!("lightvm-service: UEFI variables for the baseline: {e}");
                    PublicError::infrastructure("UEFI variables of the VM not created")
                })?;
            }
        }
        let job = self
            .cat
            .borrow_mut()
            .begin_create_baseline(st::CreateBaseline {
                vm_id: sid,
                label: label.as_str().to_owned(),
                clean_shutdown: clean,
                protected: false,
                standalone: Self::standalone(vm),
            })
            .map_err(|e| baseline_error(&e))?;
        // Upper bound of the image: the source (and its base for an overlay).
        let current = self
            .cat
            .borrow_mut()
            .baseline_overview(&sid)
            .ok()
            .and_then(|o| o.current);
        let base = current
            .and_then(|b| {
                self.list_baselines(vm.id)
                    .ok()?
                    .into_iter()
                    .find(|s| s.baseline_id == b)
            })
            .map_or(0, |s| s.size_bytes);
        let estimate = job.source_bytes().saturating_add(base);
        if let Err(e) = self.check_baseline_space(vm, estimate) {
            let done = job.run(|_, _| Err(st::StorageError::ResourceLimit("refused".into())));
            let _ = self.cat.borrow_mut().abort_baseline_job(done);
            return Err(e);
        }
        let slot: ToolError = Arc::default();
        Ok(Box::new(move || {
            let s2 = slot.clone();
            let done = job.run(move |src, dst| {
                let c = || cancel();
                tool.convert(src, dst, &c).map_err(|e| stash(&s2, e))?;
                let same = tool.compare(src, dst, &c).map_err(|e| stash(&s2, e))?;
                if !same {
                    return Err(stash(
                        &s2,
                        PublicError::infrastructure("the baseline image differs from the source"),
                    ));
                }
                tool.check(dst, &c).map_err(|e| stash(&s2, e))?;
                tool.virtual_size(dst).map_err(|e| stash(&s2, e))
            });
            Box::new(SqliteJobDone::Create(done, slot)) as Box<dyn Any + Send>
        }))
    }

    fn begin_restore_baseline(
        &mut self,
        vm: &VmRecord,
        baseline: BaselineId,
        tool: Arc<dyn ImageTool>,
        cancel: CancelCheck,
    ) -> Result<StorageWork, PublicError> {
        if vm.config.guest_os.needs_tpm() {
            return Err(tpm_baseline_unsupported());
        }
        let sid = self.sid(vm.id)?;
        let job = self
            .cat
            .borrow_mut()
            .begin_restore(st::RestoreBaseline {
                vm_id: sid,
                baseline_id: baseline,
                standalone: Self::standalone(vm),
            })
            .map_err(|e| baseline_error(&e))?;
        let slot: ToolError = Arc::default();
        Ok(Box::new(move || {
            let s2 = slot.clone();
            let done = job.run(move |backing, size, dst| {
                if cancel() {
                    return Err(stash(
                        &s2,
                        PublicError::state_conflict("operation cancelled"),
                    ));
                }
                tool.create_overlay(backing, size, dst)
                    .map_err(|e| stash(&s2, e))
            });
            Box::new(SqliteJobDone::Restore(done, slot)) as Box<dyn Any + Send>
        }))
    }

    fn finish_storage_job(
        &mut self,
        done: Box<dyn Any + Send>,
    ) -> Result<BaselineOutcome, PublicError> {
        let done = done
            .downcast::<SqliteJobDone>()
            .map_err(|_| infra("baseline job of another catalog"))?;
        match *done {
            SqliteJobDone::Create(done, slot) => {
                let rec = self
                    .cat
                    .borrow_mut()
                    .finish_create_baseline(done)
                    .map_err(|e| tool_error(&slot).unwrap_or_else(|| baseline_error(&e)))?;
                let core_vm = self
                    .core_id(rec.vm_id)
                    .ok_or_else(|| infra("baseline of an unknown VM"))?;
                let summary = self
                    .list_baselines(core_vm)?
                    .into_iter()
                    .find(|s| s.baseline_id == rec.baseline_id)
                    .ok_or_else(|| infra("new baseline not listed"))?;
                Ok(BaselineOutcome::Created(summary))
            }
            SqliteJobDone::Restore(done, slot) => {
                let state = self
                    .cat
                    .borrow_mut()
                    .finish_restore(done)
                    .map_err(|e| tool_error(&slot).unwrap_or_else(|| baseline_error(&e)))?;
                let core_vm = self
                    .core_id(state.vm_id)
                    .ok_or_else(|| infra("restore of an unknown VM"))?;
                Ok(BaselineOutcome::Restored {
                    vm_id: core_vm,
                    baseline_id: state.baseline_id,
                })
            }
            SqliteJobDone::Snapshot(job, tool) => {
                let kind = job.kind;
                match self.settle_snapshot_job(*job)? {
                    Some(outcome) => Ok(outcome),
                    // The disk operation did not take effect: its own error,
                    // or a generic one if it reported success.
                    None => Err(tool.err().unwrap_or_else(|| {
                        PublicError::state_conflict(match kind {
                            SnapKind::Take => "the snapshot was not created in the VM's disk",
                            SnapKind::Revert => "the VM's disk was not reverted",
                            SnapKind::Delete => "the snapshot is still in the VM's disk",
                        })
                    })),
                }
            }
        }
    }

    fn abort_storage_job(
        &mut self,
        done: Box<dyn Any + Send>,
    ) -> Result<Option<BaselineOutcome>, PublicError> {
        let done = done
            .downcast::<SqliteJobDone>()
            .map_err(|_| infra("baseline job of another catalog"))?;
        let r = match *done {
            SqliteJobDone::Create(d, _) => self.cat.borrow_mut().abort_baseline_job(d),
            SqliteJobDone::Restore(d, _) => self.cat.borrow_mut().abort_restore_job(d),
            // A snapshot operation cannot be "aborted": the disk says
            // whether it happened.
            SqliteJobDone::Snapshot(job, _) => return self.settle_snapshot_job(*job),
        };
        r.map(|()| None).map_err(|e| storage_error(&e))
    }

    fn delete_baseline(&mut self, vm: VmId, baseline: BaselineId) -> Result<(), PublicError> {
        let sid = self.sid(vm)?;
        self.cat
            .borrow_mut()
            .delete_baseline_files(&sid, &baseline)
            .map_err(|e| baseline_error(&e))
    }

    /// Guard opened here (supervisor thread), baseline hash verified later
    /// through the same handles ([`RunStartGuard`], start worker).
    fn start_disks(&self, vm: VmId, run: RunId) -> Result<Option<StartDisks>, PublicError> {
        let sid = self.sid(vm)?;
        let files = self
            .cat
            .borrow_mut()
            .open_current_state(&sid, &run)
            .map_err(|e| baseline_error(&e))?;
        Ok(files.map(|f| StartDisks {
            overlay: f.overlay,
            vars: f.vars,
            base: f.base,
            base_vars: f.base_vars,
            guard: Box::new(RunStartGuard(f.guard)),
        }))
    }

    fn record_exit(&self, run: RunId, clean: bool) {
        if let Err(e) = self.cat.borrow_mut().record_run_exit(&run, clean) {
            crate::diag!("lightvm-service: end of run not recorded: {e}");
        }
    }

    // ----- user snapshots (ADR-0032) ---------------------------------------

    fn snapshot_tree(&self, vm: &VmRecord) -> Result<SnapshotTree, PublicError> {
        let sid = self.sid(vm.id)?;
        let list = self
            .cat
            .borrow_mut()
            .snapshot_list(&sid)
            .map_err(|e| snapshot_error(&e))?;
        let snapshots = list
            .snapshots
            .into_iter()
            .map(|s| Self::snapshot_summary(vm.id, s))
            .collect::<Result<Vec<_>, _>>()?;
        let reason = self.snapshot_unavailable(vm);
        Ok(SnapshotTree {
            snapshots,
            current: list.current,
            available: reason.is_none(),
            unavailable_reason: reason.map(str::to_owned),
        })
    }

    fn snapshot_facts(&self, vm: VmId) -> VmSnapshots {
        let Ok(sid) = self.sid(vm) else {
            return VmSnapshots::default();
        };
        match self.read(|tx| Ok((tx.snapshot_count(&sid)?, tx.snapshot_head(&sid)?))) {
            Ok((count, current)) => VmSnapshots { count, current },
            Err(e) => {
                crate::diag!("lightvm-service: snapshot facts: {}", e.message);
                VmSnapshots::default()
            }
        }
    }

    fn snapshot_config(
        &self,
        vm: VmId,
        snapshot: SnapshotId,
    ) -> Result<(VmConfig, bool), PublicError> {
        let s = self.snapshot_of(vm, snapshot)?;
        let config: VmConfig = serde_json::from_value(s.config)
            .map_err(|_| infra("unreadable configuration of the snapshot"))?;
        Ok((config, s.taken_while_running))
    }

    fn begin_take_snapshot(
        &mut self,
        vm: &VmRecord,
        name: &Label,
        description: &str,
        running: bool,
        thumbnail: Option<Vec<u8>>,
        reserve: u64,
    ) -> Result<SnapshotStep, PublicError> {
        if let Some(r) = self.snapshot_unavailable(vm) {
            return Err(
                PublicError::state_conflict(r).with_detail("reason", "snapshots_unavailable")
            );
        }
        let sid = self.sid(vm.id)?;
        let files = Self::standalone(vm);
        // While the VM runs QEMU writes the VM's variables file itself
        // (`vms/<vm_id>/firmware/primary.vars.fd`): that is "the run's
        // variables" copied with the disk state.
        let clean = !running
            && self
                .cat
                .borrow_mut()
                .last_exit_clean(&sid)
                .map_err(|e| snapshot_error(&e))?;
        let thumbnail = thumbnail.filter(|t| t.len() <= st::MAX_THUMBNAIL_BYTES);
        self.check_snapshot_space(vm, thumbnail.as_ref().map_or(0, Vec::len), reserve)?;
        let config = serde_json::to_value(&vm.config)
            .map_err(|_| infra("configuration not serializable"))?;
        let op = self
            .cat
            .borrow_mut()
            .begin_take_snapshot(st::TakeSnapshot {
                vm_id: sid,
                name: name.as_str().to_owned(),
                description: description.to_owned(),
                running,
                clean_shutdown: clean,
                config,
                config_revision: vm.config_revision,
                disk: files.disk,
                vars: files.vars,
                thumbnail,
            })
            .map_err(|e| snapshot_error(&e))?;
        Ok(Self::snapshot_step(op, SnapKind::Take, vm.id, None))
    }

    fn begin_revert_snapshot(
        &mut self,
        vm: &VmRecord,
        snapshot: SnapshotId,
        record: VmRecord,
    ) -> Result<SnapshotStep, PublicError> {
        if let Some(r) = self.snapshot_unavailable(vm) {
            return Err(
                PublicError::state_conflict(r).with_detail("reason", "snapshots_unavailable")
            );
        }
        let sid = self.sid(vm.id)?;
        let files = Self::standalone(vm);
        let config = serde_json::to_value(StoredConfig {
            core: VmRecord {
                persisted_state: VmState::Stopped,
                ..record.clone()
            },
        })
        .map_err(|_| infra("configuration not serializable"))?;
        let op = self
            .cat
            .borrow_mut()
            .begin_revert_snapshot(st::RevertSnapshot {
                vm_id: sid,
                snapshot_id: snapshot,
                disk: files.disk,
                vars: files.vars,
                config,
                config_version: CONFIG_VERSION,
                expected_revision: vm.config_revision,
            })
            .map_err(|e| snapshot_error(&e))?;
        Ok(Self::snapshot_step(
            op,
            SnapKind::Revert,
            vm.id,
            Some(record),
        ))
    }

    fn begin_delete_snapshot(
        &mut self,
        vm: &VmRecord,
        snapshot: SnapshotId,
    ) -> Result<SnapshotStep, PublicError> {
        let sid = self.sid(vm.id)?;
        let files = Self::standalone(vm);
        let op = self
            .cat
            .borrow_mut()
            .begin_delete_snapshot(&sid, &snapshot, &files.disk)
            .map_err(|e| snapshot_error(&e))?;
        Ok(Self::snapshot_step(op, SnapKind::Delete, vm.id, None))
    }

    fn update_snapshot(
        &mut self,
        vm: VmId,
        snapshot: SnapshotId,
        name: &Label,
        description: &str,
    ) -> Result<SnapshotSummary, PublicError> {
        let sid = self.sid(vm)?;
        let s = self
            .cat
            .borrow_mut()
            .update_snapshot_text(&sid, &snapshot, name.as_str(), description)
            .map_err(|e| snapshot_error(&e))?;
        Self::snapshot_summary(vm, s)
    }

    fn snapshot_thumbnail(
        &self,
        vm: VmId,
        snapshot: SnapshotId,
    ) -> Result<Option<Vec<u8>>, PublicError> {
        let sid = self.sid(vm)?;
        let path = self
            .cat
            .borrow_mut()
            .snapshot_thumbnail(&sid, &snapshot)
            .map_err(|e| snapshot_error(&e))?;
        let Some(path) = path else { return Ok(None) };
        let bytes = std::fs::read(&path).map_err(|e| io_infra("snapshot thumbnail", &e))?;
        Ok((bytes.len() <= st::MAX_THUMBNAIL_BYTES).then_some(bytes))
    }

    fn begin_promote_snapshot(
        &mut self,
        vm: &VmRecord,
        snapshot: SnapshotId,
        label: &Label,
        tool: Arc<dyn ImageTool>,
        cancel: CancelCheck,
    ) -> Result<StorageWork, PublicError> {
        if vm.config.guest_os.needs_tpm() {
            return Err(tpm_baseline_unsupported());
        }
        let sid = self.sid(vm.id)?;
        let s = self.snapshot_of(vm.id, snapshot)?;
        let job = self
            .cat
            .borrow_mut()
            .begin_promote_snapshot(
                st::CreateBaseline {
                    vm_id: sid,
                    label: label.as_str().to_owned(),
                    clean_shutdown: s.clean_shutdown,
                    protected: false,
                    standalone: Self::standalone(vm),
                },
                &snapshot,
            )
            .map_err(|e| snapshot_error(&e))?;
        // Upper bound of the image: the whole disk file.
        let estimate = job.source_bytes();
        if let Err(e) = self.check_baseline_space(vm, estimate) {
            let done = job.run(|_, _| Err(st::StorageError::ResourceLimit("refused".into())));
            let _ = self.cat.borrow_mut().abort_baseline_job(done);
            return Err(e);
        }
        let slot: ToolError = Arc::default();
        Ok(Box::new(move || {
            let s2 = slot.clone();
            let tag = job.snapshot_tag().map(str::to_owned).unwrap_or_default();
            let done = job.run(move |src, dst| {
                let c = || cancel();
                tool.convert_snapshot(src, &tag, dst, &c)
                    .map_err(|e| stash(&s2, e))?;
                tool.check(dst, &c).map_err(|e| stash(&s2, e))?;
                tool.virtual_size(dst).map_err(|e| stash(&s2, e))
            });
            Box::new(SqliteJobDone::Create(done, slot)) as Box<dyn Any + Send>
        }))
    }
}

/// Public error of a user snapshot operation, without paths.
fn snapshot_error(e: &st::StorageError) -> PublicError {
    match e {
        st::StorageError::StateConflict(m)
            if m == st::SNAPSHOT_ON_OVERLAY
                || m == st::SNAPSHOT_NO_DISK
                || m == st::PROMOTE_RUNNING_SNAPSHOT =>
        {
            PublicError::state_conflict(m.clone()).with_detail("reason", "snapshots_unavailable")
        }
        st::StorageError::IntegrityMismatch { .. } => {
            crate::diag!("lightvm-service: snapshot integrity: {e}");
            PublicError::state_conflict(
                "the snapshot's files or the VM's disk were altered: verification failed",
            )
            .with_detail("reason", "integrity")
        }
        st::StorageError::ResourceLimit(_) => {
            PublicError::resource_limit("snapshot refused by a storage limit")
        }
        other => baseline_error(other),
    }
}

fn io_infra(what: &str, e: &std::io::Error) -> PublicError {
    crate::diag!("lightvm-service: {what}: {e}");
    PublicError::infrastructure(format!("{what} not readable"))
}

/// Free space that must remain on the catalog volume after a snapshot is
/// taken, on top of the host reserve (manifests, minimal outcome).
const SNAPSHOT_FREE_MARGIN: u64 = 256 << 20;

impl SqliteStore {
    /// Why snapshots of this VM cannot be taken or reverted now, from the
    /// catalog: TPM guest, VM on an overlay of a baseline, no disk yet.
    fn snapshot_unavailable(&self, vm: &VmRecord) -> Option<&'static str> {
        if vm.config.guest_os.needs_tpm() {
            return Some(
                "snapshots of VMs with a TPM (Windows 11) are not supported yet: the TPM state is not part of the snapshot",
            );
        }
        let sid = self.sid(vm.id).ok()?;
        let overlay = self
            .cat
            .borrow_mut()
            .baseline_overview(&sid)
            .map(|o| o.current.is_some())
            .unwrap_or(false);
        if overlay {
            return Some(st::SNAPSHOT_ON_OVERLAY);
        }
        let disk = managed_abs(
            &self.root(),
            st::CatalogLayout::primary_disk(&vm.id).as_str(),
        );
        if std::fs::symlink_metadata(disk).is_err() {
            return Some(st::SNAPSHOT_NO_DISK);
        }
        None
    }

    /// A snapshot of the VM (core identifier).
    fn snapshot_of(
        &self,
        vm: VmId,
        snapshot: SnapshotId,
    ) -> Result<st::SnapshotRecord, PublicError> {
        let sid = self.sid(vm)?;
        let s = self
            .read(|tx| tx.snapshot(&snapshot))
            .map_err(|e| match e.code {
                ErrorCode::NotFound => PublicError::not_found("snapshot not found"),
                _ => e,
            })?;
        if s.vm_id != sid {
            return Err(PublicError::not_found("snapshot not found"));
        }
        Ok(s)
    }

    fn snapshot_summary(vm: VmId, s: st::SnapshotRecord) -> Result<SnapshotSummary, PublicError> {
        let name = Label::parse(s.name.clone())
            .or_else(|_| Label::parse("snapshot"))
            .map_err(|_| infra("invalid snapshot name"))?;
        let disk_gib = s
            .config
            .get("disk_gib")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        Ok(SnapshotSummary {
            snapshot_id: s.snapshot_id,
            vm_id: vm,
            parent: s.parent_id,
            name,
            description: s.description,
            created_at: ts(&s.created_at)?,
            taken_while_running: s.taken_while_running,
            clean_shutdown: s.clean_shutdown,
            firmware_saved: s.vars_sha256.is_some(),
            has_thumbnail: s.thumbnail_bytes > 0,
            disk_gib,
            config_revision: s.config_revision,
            files_bytes: s.vars_bytes.saturating_add(s.thumbnail_bytes),
        })
    }

    fn snapshot_step(
        op: st::SnapshotOp,
        kind: SnapKind,
        vm: VmId,
        record: Option<VmRecord>,
    ) -> SnapshotStep {
        let disk = op.disk().to_path_buf();
        let tag = op.tag().to_owned();
        let job = SnapshotJob {
            op,
            kind,
            vm,
            record,
        };
        SnapshotStep {
            disk,
            tag,
            finish: Box::new(move |r| {
                Box::new(SqliteJobDone::Snapshot(Box::new(job), r)) as Box<dyn Any + Send>
            }),
        }
    }

    /// Settles a snapshot operation from the disk; `Some` if it took effect.
    fn settle_snapshot_job(
        &mut self,
        job: SnapshotJob,
    ) -> Result<Option<BaselineOutcome>, PublicError> {
        let settled = self
            .cat
            .borrow_mut()
            .settle_snapshot(&job.op)
            .map_err(|e| snapshot_error(&e))?;
        if settled == st::Settled::NotDone {
            return Ok(None);
        }
        let id = job.op.snapshot_id();
        Ok(Some(match job.kind {
            SnapKind::Take => {
                let s = self.snapshot_of(job.vm, id)?;
                BaselineOutcome::SnapshotTaken(Self::snapshot_summary(job.vm, s)?)
            }
            SnapKind::Revert => BaselineOutcome::SnapshotReverted {
                vm_id: job.vm,
                snapshot_id: id,
                record: Box::new(job.record.ok_or_else(|| infra("reverted record missing"))?),
            },
            SnapKind::Delete => BaselineOutcome::SnapshotDeleted {
                vm_id: job.vm,
                snapshot_id: id,
            },
        }))
    }

    /// Storage budget and free space before a snapshot (ADR-0017,
    /// ADR-0032). Conservative: after the snapshot every cluster now in the
    /// disk may be rewritten by the guest and kept twice, so the free space
    /// minus the host reserve must hold the disk's current size again, plus
    /// the snapshot's files. The VM budget (`storage_budget_gib`, which
    /// excludes the VM's own disk) must hold the snapshot's files.
    fn check_snapshot_space(
        &self,
        vm: &VmRecord,
        thumbnail: usize,
        reserve: u64,
    ) -> Result<(), PublicError> {
        let root = self.root();
        let disk = managed_abs(&root, st::CatalogLayout::primary_disk(&vm.id).as_str());
        let vars = managed_abs(&root, st::CatalogLayout::primary_vars(&vm.id).as_str());
        let disk_bytes = std::fs::metadata(&disk).map_or(0, |m| m.len());
        let files = std::fs::metadata(&vars)
            .map_or(0, |m| m.len())
            .saturating_add(thumbnail as u64);
        if let Some(gib) = vm.config.limits.storage_budget_gib {
            let need = self.budget_usage(vm.id).saturating_add(files);
            if need > gib << 30 {
                return Err(PublicError::resource_limit(
                    "VM storage budget not enough for the snapshot",
                )
                .with_detail("needed_bytes", need.to_string())
                .with_detail("budget_gib", gib.to_string()));
            }
        }
        let estimate = disk_bytes.saturating_add(files);
        let free = qemu_backend::ManagedPath::verify(&root)
            .and_then(|r| qemu_backend::prepare::host_resources(&r))
            .map(|r| r.free_disk_bytes);
        if let Ok(free) = free {
            let reserve = reserve.saturating_add(SNAPSHOT_FREE_MARGIN);
            if free.saturating_sub(reserve) < estimate {
                return Err(PublicError::resource_limit(
                    "not enough free space in the catalog for a snapshot: the guest may rewrite the whole disk after it",
                )
                .with_detail("needed_bytes", estimate.to_string())
                .with_detail("free_bytes", free.to_string())
                .with_detail("reserve_bytes", reserve.to_string()));
            }
        }
        Ok(())
    }
}

/// Absolute path of a managed relative path (`/` separator).
fn managed_abs(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Guest OS of the installation image at `path` (ADR-0027): a hint for the
/// wizard. Unreadable or unrecognized media give `None`; the reason goes to
/// the service log only (no paths in public errors).
fn detect_guest(path: &Path, what: &dyn std::fmt::Display) -> Option<DetectedGuest> {
    detected(guest_detect::detect_path(path), what)
}

/// [`detect_guest`] reading through an open handle (the import's, which
/// keeps the file unchanged since it was hashed).
fn detect_guest_in(
    mut file: &std::fs::File,
    what: &dyn std::fmt::Display,
) -> Option<DetectedGuest> {
    detected(guest_detect::detect(&mut file), what)
}

fn detected(
    outcome: Result<Option<DetectedGuest>, guest_detect::DetectError>,
    what: &dyn std::fmt::Display,
) -> Option<DetectedGuest> {
    match outcome {
        Ok(Some(g)) => {
            crate::diag!(
                "lightvm-service: {what}: detected guest {:?} ({:?}, build {:?})",
                g.os,
                g.arch,
                g.build
            );
            Some(g)
        }
        Ok(None) => {
            crate::diag!("lightvm-service: {what}: guest not recognized or not supported");
            None
        }
        Err(e) => {
            crate::diag!("lightvm-service: {what}: guest detection failed: {e}");
            None
        }
    }
}

/// Maximum length of the path of a medium chosen by the user.
const MAX_SOURCE_PATH: usize = 4096;

/// Windows device names: a file with one of these stems opens a device.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

/// `true` for a symbolic link or, on Windows, any reparse point (junctions,
/// mount points, cloud placeholders).
pub(crate) fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Checks of the file chosen by the user with the native picker (ADR-0008;
/// ROADMAP 7.4, "Host" row): [`validate_import_path`] on the text, then
/// [`check_import_handle`] on the handle the hash reads from. The path is
/// opened only for reading by the catalog; QEMU later receives the path the
/// system resolved for it ([`reference_path`]), after the checks at every
/// start (ADR-0029).
///
/// The service runs the two halves on different threads: the textual one on
/// the supervisor thread, the file one on the import worker thread
/// ([`crate::engine::MediaImportPort::begin_import`]), so a slow or hanging
/// source never blocks the supervisor. This function runs both in sequence
/// (tests, tools).
pub fn validate_import_source(raw: &str, kind: MediaKind) -> Result<PathBuf, PublicError> {
    let path = validate_import_path(raw)?;
    precheck_source(&path)?;
    let file = open_source(&path)?;
    check_import_handle(&file, &path, kind)?;
    Ok(path)
}

/// Textual checks of the media path (no file system access, never
/// panics: only byte-level and `Path` operations). Rejected:
/// - empty, longer than 4096 bytes, control characters â†’ `invalid_argument`;
/// - UNC paths (`\\server\share`) and device paths (`\\?\`, `\\.\`, `\??\`),
///   alternate data streams (`:` after the drive), Windows device names
///   (`CON`, `NUL`, `COM1`, ...) â†’ `permission_denied`;
/// - relative paths, `.`/`..` components, names ending in `.` or space â†’
///   `invalid_argument`;
/// - an extension other than `.iso` â†’ `invalid_argument`.
pub fn validate_import_path(raw: &str) -> Result<PathBuf, PublicError> {
    use std::path::{Component, Path};
    let invalid = PublicError::invalid_argument;
    let denied = PublicError::permission_denied;
    if raw.is_empty() || raw.len() > MAX_SOURCE_PATH || raw.chars().any(char::is_control) {
        return Err(invalid("invalid media path"));
    }
    if cfg!(windows) {
        let b = raw.as_bytes();
        let sep = |c: u8| c == b'\\' || c == b'/';
        if b.len() >= 2 && sep(b[0]) && (sep(b[1]) || b[1] == b'?') {
            return Err(denied("UNC and device paths are not allowed"));
        }
        // Byte-level: `raw[2..]` could split a multi-byte character.
        if b.len() > 2 && b[2..].contains(&b':') {
            return Err(denied("alternate data streams are not allowed"));
        }
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(invalid("an absolute path is required"));
    }
    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                if !matches!(p.kind(), std::path::Prefix::Disk(_)) {
                    return Err(denied("UNC and device paths are not allowed"));
                }
            }
            Component::RootDir => {}
            Component::CurDir | Component::ParentDir => {
                return Err(invalid("`.` and `..` are not allowed in the media path"));
            }
            Component::Normal(n) => {
                let n = n.to_str().ok_or_else(|| invalid("invalid media path"))?;
                let stem = n.split('.').next().unwrap_or(n).trim_end();
                if cfg!(windows) && RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
                    return Err(denied("device names are not allowed"));
                }
                if cfg!(windows) && (n.ends_with('.') || n.ends_with(' ')) {
                    return Err(invalid("invalid media path"));
                }
            }
        }
    }
    let is_iso = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("iso"));
    if !is_iso {
        return Err(invalid("only `.iso` files can be imported"));
    }
    Ok(path.to_path_buf())
}

/// Opens the source like the catalog does (read, share read only, without
/// following a reparse point on the last component).
fn open_source(path: &Path) -> Result<std::fs::File, PublicError> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ, FILE_FLAG_OPEN_REPARSE_POINT.
        o.share_mode(0x1);
        o.custom_flags(0x0020_0000);
    }
    o.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PublicError::not_found("media file not found"),
        _ => PublicError::permission_denied("media file not accessible"),
    })
}

/// File checks on the handle the hash reads from (import worker thread):
/// - a link, junction or other reparse point on the file â†’ `permission_denied`;
///   anything but a regular file â†’ `invalid_argument`;
/// - an empty file or one over the limit of the kind (16 GiB installation,
///   8 GiB data) â†’ `invalid_argument` / `resource_limit`;
/// - a file without the ISO 9660 / UDF signature â†’ `invalid_argument`;
/// - Windows, location resolved by the system on the handle
///   (`GetFinalPathNameByHandleW`, then `GetDriveTypeW` on the volume
///   root): a file on a network share or on a volume that is not a local
///   fixed, removable or optical drive (mapped network drives included) â†’
///   `permission_denied`;
/// - Windows, **reparse traversal rule**: the final path of the handle must
///   be the requested path itself (compared without case, `/` as `\`). Any
///   difference means the system resolved something on the way: a symbolic
///   link, junction or mount point in an intermediate directory, a `subst`
///   or mapped drive, or an 8.3 short name; all are refused
///   (`permission_denied`, "choose the file in its real location"), so the
///   file hashed and recorded is always the one at the path the user saw.
///
/// The handle position is left at the start of the file.
pub fn check_import_handle(
    file: &std::fs::File,
    requested: &Path,
    kind: MediaKind,
) -> Result<(), PublicError> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let invalid = PublicError::invalid_argument;
    let denied = PublicError::permission_denied;
    let meta = file
        .metadata()
        .map_err(|_| denied("media file not accessible"))?;
    if is_link(&meta) {
        return Err(denied(
            "links, junctions and reparse points are not allowed",
        ));
    }
    if !meta.is_file() {
        return Err(invalid("the media path is not a regular file"));
    }
    if meta.len() == 0 {
        return Err(invalid("empty media file"));
    }
    if meta.len() > kind.max_bytes() {
        return Err(
            PublicError::resource_limit("medium exceeds the maximum size")
                .with_detail("size_bytes", meta.len().to_string())
                .with_detail("max_bytes", kind.max_bytes().to_string()),
        );
    }
    #[cfg(windows)]
    {
        let loc = qemu_backend::prepare::file_location(file)
            .map_err(|_| denied("location of the media file not verifiable"))?;
        if !loc.volume.is_local() {
            return Err(denied(
                "media on network shares or non-local volumes are not allowed",
            ));
        }
        if !same_windows_path(&loc.final_path, requested) {
            return Err(denied(
                "the media path goes through a link, junction, mount point or substituted drive: choose the file in its real location",
            ));
        }
    }
    #[cfg(not(windows))]
    let _ = requested;
    let mut f = file;
    let mut id = [0u8; 5];
    let signed = f.seek(SeekFrom::Start(VOLUME_DESCRIPTOR_ID_OFFSET)).is_ok()
        && f.read_exact(&mut id).is_ok()
        && (&id == b"CD001" || &id == b"BEA01");
    f.seek(SeekFrom::Start(0))
        .map_err(|_| denied("media file not accessible"))?;
    if !signed {
        return Err(invalid(st::NOT_OPTICAL_IMAGE));
    }
    Ok(())
}

/// Same Windows path, without case and with `/` as `\`.
#[cfg_attr(not(windows), allow(dead_code))]
fn same_windows_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}
/// Public error of a failed import, without paths.
fn import_error(e: &st::StorageError) -> PublicError {
    match e {
        st::StorageError::Unsupported(m) if *m == st::NOT_OPTICAL_IMAGE => {
            PublicError::invalid_argument(st::NOT_OPTICAL_IMAGE)
        }
        st::StorageError::ResourceLimit(_) => {
            PublicError::resource_limit("medium exceeds the maximum size")
        }
        st::StorageError::VmBusy(_) => {
            PublicError::state_conflict("the media file is in use by another program")
        }
        st::StorageError::InvalidPath { .. } => {
            PublicError::permission_denied("links, junctions and reparse points are not allowed")
        }
        st::StorageError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            PublicError::not_found("media file not found")
        }
        st::StorageError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            PublicError::permission_denied("media file not accessible")
        }
        st::StorageError::Io { source, .. } if source.kind() == std::io::ErrorKind::StorageFull => {
            PublicError::resource_limit("not enough free space in the catalog for the medium")
        }
        other => {
            crate::diag!("lightvm-service: media import: {other}");
            storage_error(other)
        }
    }
}

fn storage_kind(kind: MediaKind) -> st::MediaKind {
    match kind {
        MediaKind::InstallIso => st::MediaKind::Iso,
        MediaKind::DataIso => st::MediaKind::Data,
    }
}

/// Bytes of the regular files under `dir` (recursive, without following
/// links); 0 if missing.
fn tree_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(Result::ok)
        .map(|e| match std::fs::symlink_metadata(e.path()) {
            Ok(m) if is_link(&m) => 0,
            Ok(m) if m.is_dir() => tree_bytes(&e.path()),
            Ok(m) if m.is_file() => m.len(),
            _ => 0,
        })
        .sum()
}

/// Result of the worker step of an import on the SQLite catalog.
struct SqliteHashed {
    hashed: st::HashedMedia,
    /// Public reason when the service's own checks refused the source.
    rejected: Option<PublicError>,
}

/// Placeholder storage error for a source refused by the service's checks
/// (the public error travels in [`SqliteHashed::rejected`]).
const SOURCE_REFUSED: &str = "media source refused by the service checks";

/// ADR-0030: journaled import of a VMware VM (`import_vm` intent): the
/// converted disk is written into the intent's temporary and published as
/// `vms/<vm_id>/primary.qcow2` together with the VM row (stored under the
/// core's identifier, like new VMs, see `insert_vm`).
impl crate::engine::VmImportPort for SqliteStore {
    fn catalog_root(&self) -> Option<PathBuf> {
        Some(self.root())
    }

    fn begin_vm_import(&mut self, vm: VmId) -> Result<crate::engine::VmImportSlot, PublicError> {
        let imp = self
            .cat
            .borrow_mut()
            .begin_vm_import(&vm, None)
            .map_err(|e| storage_error(&e))?;
        Ok(crate::engine::VmImportSlot {
            vm_id: vm,
            temp_path: imp.temp_path().to_path_buf(),
            inner: Box::new(imp),
        })
    }

    fn finish_vm_import(
        &mut self,
        slot: crate::engine::VmImportSlot,
        vm: &VmRecord,
    ) -> Result<(), PublicError> {
        let imp = slot
            .inner
            .downcast::<st::VmImport>()
            .map_err(|_| infra("import of another catalog"))?;
        let config = serde_json::to_value(StoredConfig { core: vm.clone() })
            .map_err(|_| infra("configuration not serializable"))?;
        let new = st::NewVm {
            name: vm.config.name.as_str().to_owned(),
            guest_os: snake(&vm.config.guest_os)?,
            config_version: CONFIG_VERSION,
            config,
        };
        self.cat
            .borrow_mut()
            .finish_vm_import(*imp, &new, lifecycle(vm.persisted_state))
            .map_err(|e| storage_error(&e))?;
        self.ids.insert(vm.id, vm.id);
        Ok(())
    }

    fn abort_vm_import(&mut self, slot: crate::engine::VmImportSlot) {
        if let Ok(imp) = slot.inner.downcast::<st::VmImport>()
            && let Err(e) = self.cat.borrow_mut().abort_vm_import(*imp)
        {
            // Reconciliation finishes it at the next open.
            crate::diag!("lightvm-service: VMware import not rolled back: {e}");
        }
    }
}

impl crate::engine::MediaImportPort for SqliteStore {
    /// Reserves the import only: the source is opened, checked
    /// ([`check_import_handle`], [`reference_path`]) and hashed by the
    /// returned job, on the import worker thread. Nothing is copied
    /// (ADR-0029): the file is used where it is.
    fn begin_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &Label,
    ) -> Result<ImportJob, PublicError> {
        let limits = st::MediaLimits {
            max_bytes: kind.max_bytes(),
            optical_signature: true,
        };
        let reservation = self
            .cat
            .borrow_mut()
            .reserve_media_import(source, storage_kind(kind), declared_origin.as_str(), limits)
            .map_err(|e| import_error(&e))?;
        let root = self.root();
        let requested = source.to_path_buf();
        Ok(ImportJob::new(move || {
            // Readable error for the common refusals (a folder, a link, a
            // missing file); the checks that count are made on the handle.
            if let Err(e) = precheck_source(&requested) {
                let hashed = reservation.refuse(st::StorageError::Unsupported(SOURCE_REFUSED));
                return ImportCopied(Box::new(SqliteHashed {
                    hashed,
                    rejected: Some(e),
                }));
            }
            let mut rejected = None;
            let mut hashed = reservation.open_and_hash(|file, requested| {
                let check = check_import_handle(file, requested, kind)
                    .and_then(|()| reference_path(file, requested, &root));
                check.map_err(|e| {
                    rejected = Some(e);
                    st::StorageError::Unsupported(SOURCE_REFUSED)
                })
            });
            // ADR-0027: the guest OS is read from the hashed file through
            // the handle still held, so it cannot change in between; a
            // failure never blocks the import, the wizard then asks the user.
            if kind == MediaKind::InstallIso
                && let Some(file) = hashed.source_file()
            {
                let guest = detect_guest_in(file, &"the imported medium");
                hashed.set_detected_guest(guest);
            }
            ImportCopied(Box::new(SqliteHashed { hashed, rejected }))
        }))
    }

    fn finish_import(&mut self, copied: ImportCopied) -> Result<MediaEntry, PublicError> {
        let SqliteHashed { hashed, rejected } = *copied
            .0
            .downcast::<SqliteHashed>()
            .map_err(|_| infra("import started on another catalog"))?;
        let rec = self
            .cat
            .borrow_mut()
            .finish_media_import(hashed)
            .map_err(|e| rejected.unwrap_or_else(|| import_error(&e)))?;
        Self::media_entry(rec)?.ok_or_else(|| infra("imported medium not approved"))
    }

    fn abort_import(&mut self, copied: ImportCopied) -> Result<(), PublicError> {
        let SqliteHashed { hashed, .. } = *copied
            .0
            .downcast::<SqliteHashed>()
            .map_err(|_| infra("import started on another catalog"))?;
        self.cat
            .borrow_mut()
            .abort_media_import(hashed)
            .map_err(|e| storage_error(&e))
    }

    /// A start found the file of a medium used in place missing: the
    /// registry shows it as such (best effort: the next listing looks
    /// again anyway).
    fn mark_media_missing(&self, id: MediaId) {
        let referenced = self
            .read(|tx| tx.media(&id))
            .is_ok_and(|m| matches!(m.location, st::MediaLocation::Referenced(_)));
        if referenced
            && let Err(e) = self.write(|tx| tx.set_media_status(&id, st::MediaStatus::Missing))
        {
            crate::diag!(
                "lightvm-service: status of media {id} not recorded: {}",
                e.message
            );
        }
    }
}

/// Classification of the source by path, on the worker thread, before the
/// open: missing → `not_found`; link, junction or reparse point →
/// `permission_denied`; not a regular file → `invalid_argument`.
fn precheck_source(path: &Path) -> Result<(), PublicError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(PublicError::not_found("media file not found"))
        }
        Err(_) => Err(PublicError::permission_denied("media file not accessible")),
        Ok(m) if is_link(&m) => Err(PublicError::permission_denied(
            "links, junctions and reparse points are not allowed",
        )),
        Ok(m) if !m.is_file() => Err(PublicError::invalid_argument(
            "the media path is not a regular file",
        )),
        Ok(_) => Ok(()),
    }
}

/// Path recorded for a medium used in place (ADR-0029), after
/// [`check_import_handle`] on the same handle:
/// - the path the system resolved for the open file (Windows: final path of
///   the handle, equal to the requested one by the reparse traversal rule;
///   elsewhere `canonicalize`), so every start opens exactly that file;
/// - refused (`permission_denied`) inside the catalog folder: the catalog
///   owns and moves its own files, a reference into it would break;
/// - refused (`invalid_argument`) if QEMU could not receive it unambiguously
///   (a `,`, `=`, `:`, quote or other character its option syntax
///   interprets, see `qemu_backend::validate::qemu_path_value`): the user
///   renames the file or its folder.
pub fn reference_path(
    file: &std::fs::File,
    requested: &Path,
    root: &Path,
) -> Result<PathBuf, PublicError> {
    let denied = PublicError::permission_denied;
    #[cfg(windows)]
    let path = {
        let _ = requested;
        qemu_backend::prepare::file_location(file)
            .map_err(|_| denied("location of the media file not verifiable"))?
            .final_path
    };
    #[cfg(not(windows))]
    let path = {
        let _ = file;
        std::fs::canonicalize(requested).map_err(|_| denied("media file not accessible"))?
    };
    // The catalog root as the system resolves it (`\\?\` removed from a
    // drive path, like the final path of the handle).
    let root = std::fs::canonicalize(root).map_or_else(
        |_| root.to_path_buf(),
        |r| {
            let s = r.to_string_lossy();
            match s.strip_prefix(r"\\?\") {
                Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
                _ => r.clone(),
            }
        },
    );
    if is_inside(&path, &root) {
        return Err(denied(
            "the ISO is inside the LightVM catalog folder: choose a file outside it",
        ));
    }
    let text = path
        .to_str()
        .ok_or_else(|| PublicError::invalid_argument("invalid media path"))?;
    if qemu_backend::validate::qemu_path_value("media", text).is_err() {
        return Err(PublicError::invalid_argument(
            "the location of the ISO contains a character QEMU cannot accept (for example a comma, an equals sign or a quote): rename the file or its folder and import it again",
        ));
    }
    Ok(path)
}

/// `true` if `path` is `dir` or below it (Windows: without case, `/` as `\`).
fn is_inside(path: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let (p, d) = (norm(path), norm(dir));
    p == d || p.strip_prefix(&d).is_some_and(|rest| rest.starts_with('/'))
}

/// Offset of the first volume descriptor (sector 16 of 2048 bytes) plus the
/// type byte: there ISO 9660 writes `CD001`, UDF `BEA01` (ADR-0008).
const VOLUME_DESCRIPTOR_ID_OFFSET: u64 = 16 * 2048 + 1;

/// `true` if the file has the signature of an ISO 9660 (also hybrid with UDF)
/// or UDF optical image: the only format allowed for media (read-only CD-ROM
/// in the guest).
pub fn is_optical_image(path: &Path) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut id = [0u8; 5];
    f.seek(SeekFrom::Start(VOLUME_DESCRIPTOR_ID_OFFSET)).is_ok()
        && f.read_exact(&mut id).is_ok()
        && (&id == b"CD001" || &id == b"BEA01")
}

/// Catalog guard of a start (F3): the baseline's hash is verified through
/// the guard handles opened by [`SqliteStore::start_disks`].
struct RunStartGuard(st::RunGuard);

impl StartGuard for RunStartGuard {
    fn verify_bytes(&self) -> u64 {
        self.0.baseline_bytes()
    }

    fn verify(
        &mut self,
        cancel: &dyn Fn() -> bool,
        progress: &dyn Fn(u64),
    ) -> Result<(), PublicError> {
        self.0
            .verify_baseline(cancel, progress)
            .map_err(|e| baseline_error(&e))
    }
}

/// Inspector of the approved media: the file (a managed copy, or the user's
/// file used in place, ADR-0029) is opened by [`vm_storage::MediaGuard`]
/// (`FILE_SHARE_READ` only, no reparse point followed, regular file; for a
/// file used in place every directory of its path checked against links
/// too, and on Windows the final path of the handle must still be the
/// recorded one on a local volume, as at import) and its SHA-256 and size
/// are read **through that handle**; the comparison with the registry is
/// done by the core. For a start ([`MediaGuardPort`]) the handle is opened
/// on the supervisor thread, hashed on the start worker and kept by the
/// backend until the run is released, so the file QEMU opens is the file
/// verified (see `crate::start`).
///
/// The OS identity of the file is not observed (`file_identity: None`): it
/// cannot be read with stable `std` without `unsafe` (see `vm_storage`
/// media), and the registry does not record it either.
#[derive(Debug, Clone)]
pub struct ManagedMediaInspector {
    root: PathBuf,
}

impl ManagedMediaInspector {
    /// Inspector for the catalog root.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn open(&self, entry: &MediaEntry) -> Result<ManagedGuardedMedia, PublicError> {
        let name = entry.location.file_name();
        let guard = match &entry.location {
            MediaLocation::Managed(rel) => {
                let rel = st::ManagedPath::parse(rel.as_str())
                    .map_err(|_| infra("invalid managed path"))?;
                let guard = st::MediaGuard::open(&self.root, &rel)
                    .map_err(|e| media_open_error(&e, None))?;
                if !is_optical_handle(guard.file()) {
                    return Err(PublicError::invalid_argument(
                        "the medium is not an ISO 9660 or UDF image",
                    ));
                }
                guard
            }
            MediaLocation::Referenced(path) => {
                let guard = st::MediaGuard::open_external(path)
                    .map_err(|e| media_open_error(&e, name.as_deref()))?;
                check_import_handle(guard.file(), path, entry.record.kind).map_err(|e| {
                    let file = name.as_deref().unwrap_or("ISO");
                    PublicError::new(
                        e.code,
                        format!(
                            "the ISO file \u{201c}{file}\u{201d} cannot be used: {}",
                            e.message
                        ),
                    )
                })?;
                guard
            }
        };
        Ok(ManagedGuardedMedia { guard, name })
    }
}

/// Public error of a medium that cannot be opened or read for a start;
/// `name` is the file name of a medium used in place (never its path).
fn media_open_error(e: &st::StorageError, name: Option<&str>) -> PublicError {
    match (e, name) {
        (st::StorageError::Io { source, .. }, Some(name))
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            PublicError::not_found(format!(
                "the ISO file \u{201c}{name}\u{201d} is no longer where it was selected (moved, renamed, deleted, or its drive is not connected): put it back or import it again"
            ))
            .with_detail("reason", "media_file_missing")
        }
        (st::StorageError::Io { source, .. }, None)
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            PublicError::not_found("managed copy of the medium missing")
        }
        (st::StorageError::VmBusy(_), Some(name)) => {
            crate::diag!("lightvm-service: medium: {e}");
            PublicError::vm_busy(format!(
                "the ISO file \u{201c}{name}\u{201d} is open for writing by another program"
            ))
        }
        (st::StorageError::VmBusy(_), None) => {
            crate::diag!("lightvm-service: medium: {e}");
            PublicError::vm_busy("the managed copy of the medium is open by another program")
        }
        (st::StorageError::InvalidPath { .. }, Some(name)) => PublicError::permission_denied(
            format!(
                "the path of the ISO file \u{201c}{name}\u{201d} now goes through a link or junction, or it is not a regular file: import it again from its real location"
            ),
        ),
        (st::StorageError::InvalidPath { .. }, None) => {
            PublicError::permission_denied("managed copy of the medium is not a regular file")
        }
        (st::StorageError::StateConflict(m), _) if m == st::HASH_CANCELLED => {
            PublicError::state_conflict(st::HASH_CANCELLED)
        }
        (other, _) => {
            crate::diag!("lightvm-service: medium: {other}");
            infra("reading the medium failed")
        }
    }
}

/// `true` if the ISO 9660 / UDF signature is read through the handle.
fn is_optical_handle(mut f: &std::fs::File) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut id = [0u8; 5];
    f.seek(SeekFrom::Start(VOLUME_DESCRIPTOR_ID_OFFSET)).is_ok()
        && f.read_exact(&mut id).is_ok()
        && (&id == b"CD001" || &id == b"BEA01")
}

/// File of a medium held open by a [`vm_storage::MediaGuard`] for a start.
#[derive(Debug)]
struct ManagedGuardedMedia {
    guard: st::MediaGuard,
    /// File name of a medium used in place, for messages.
    name: Option<String>,
}

impl GuardedMedia for ManagedGuardedMedia {
    fn size_bytes(&self) -> u64 {
        self.guard.size_bytes()
    }

    fn observe(
        &mut self,
        cancel: &dyn Fn() -> bool,
        progress: &dyn Fn(u64),
    ) -> Result<vm_core::media::ObservedMediaFacts, PublicError> {
        let (hex, size) = self
            .guard
            .sha256_with(cancel, progress)
            .map_err(|e| media_open_error(&e, self.name.as_deref()))?;
        Ok(vm_core::media::ObservedMediaFacts {
            sha256: Sha256Digest::parse_hex(&hex).map_err(|_| infra("invalid hash"))?,
            size_bytes: size,
            file_identity: None,
            read_only: self
                .guard
                .read_only()
                .map_err(|e| media_open_error(&e, self.name.as_deref()))?,
        })
    }
}

impl MediaGuardPort for ManagedMediaInspector {
    fn open_for_start(&mut self, entry: &MediaEntry) -> Result<Box<dyn GuardedMedia>, PublicError> {
        Ok(Box::new(self.open(entry)?))
    }
}

impl vm_core::ports::MediaInspector for ManagedMediaInspector {
    fn inspect(
        &mut self,
        entry: &MediaEntry,
    ) -> Result<vm_core::media::ObservedMediaFacts, PublicError> {
        self.open(entry)?.observe(&|| false, &|_| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F6: the OS "disk full" error during a media copy or a baseline file
    /// operation reaches the user as `resource_limit` with a clear message,
    /// not as an opaque infrastructure error (injected OS error).
    #[test]
    fn disk_full_is_a_clear_resource_limit() {
        let full = || st::StorageError::Io {
            path: PathBuf::from("media/tmp/copy"),
            source: if cfg!(windows) {
                std::io::Error::from_raw_os_error(112) // ERROR_DISK_FULL
            } else {
                std::io::Error::from_raw_os_error(28) // ENOSPC
            },
        };
        let e = import_error(&full());
        assert_eq!(e.code, ErrorCode::ResourceLimit);
        assert!(e.message.contains("free space"), "{}", e.message);
        assert!(!e.message.contains("media/tmp"), "no path in the message");
        let e = baseline_error(&full());
        assert_eq!(e.code, ErrorCode::ResourceLimit);
        assert!(e.message.contains("free space"), "{}", e.message);
    }

    #[test]
    fn iso_9660_or_udf_signature() {
        let d = std::env::temp_dir().join(format!(
            "lightvm-iso-signature-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&d).unwrap();
        let file = |name: &str, id: &[u8]| {
            let mut b = vec![0u8; 40 * 1024];
            let o = usize::try_from(VOLUME_DESCRIPTOR_ID_OFFSET).unwrap();
            b[o..o + id.len()].copy_from_slice(id);
            let p = d.join(name);
            std::fs::write(&p, b).unwrap();
            p
        };
        assert!(is_optical_image(&file("iso9660.iso", b"CD001")));
        assert!(is_optical_image(&file("udf.iso", b"BEA01")));
        assert!(!is_optical_image(&file("zip.iso", b"PK\x03\x04\x00")));
        let short = d.join("short.iso");
        std::fs::write(&short, b"CD001").unwrap();
        assert!(!is_optical_image(&short));
        assert!(!is_optical_image(&d.join("missing.iso")));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Never panics, whatever the text: the checks run on the supervisor
    /// thread (a panic there used to kill every VM).
    #[test]
    fn import_path_validation_never_panics() {
        let fixed = [
            "â‚¬:\\a.iso",
            "Câ‚¬:\\a.iso",
            "â‚¬",
            "â‚¬â‚¬",
            "\u{1F600}:\\x.iso",
            "C:\\Ã©:stream.iso",
            "C:Ã©",
            "C:\\dir\\â‚¬.iso",
            "\\\\?\\C:\\a.iso",
            "\\\\.\\PhysicalDrive0",
            "\\??\\C:\\a.iso",
            "//server/share/a.iso",
            "C:\\CON.iso",
            "C:\\nul .iso",
            "C:\\a.iso:ads",
            "C:\\a\\..\\b.iso",
            "C:a.iso",
            "a.iso",
            "",
            ":",
            "::",
            "\0",
            "C:\\a.iso.",
            "C:\\\u{202E}osi.exe",
            "/",
            "\\",
            "C:\\",
        ];
        for s in fixed {
            let _ = validate_import_path(s);
            let _ = validate_import_source(s, MediaKind::InstallIso);
        }
        assert_eq!(
            validate_import_path("â‚¬:\\a.iso").unwrap_err().code,
            if cfg!(windows) {
                ErrorCode::PermissionDenied
            } else {
                ErrorCode::InvalidArgument
            }
        );
        // Pseudo-random strings over an alphabet of separators, colons,
        // dots and multi-byte characters.
        let alphabet: Vec<char> = "C:\\/.?aâ‚¬Ã©\u{1F600}\u{0301}NUL iso ".chars().collect();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..20_000 {
            let mut s = String::new();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = (x % 24) as usize;
            for _ in 0..len {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push(alphabet[(x % alphabet.len() as u64) as usize]);
            }
            if s.ends_with(".iso") || x.is_multiple_of(3) {
                let _ = validate_import_source(&s, MediaKind::DataIso);
            }
            let _ = validate_import_path(&s);
        }
    }

    /// Start checks on a real managed copy: the handle opened for the hash
    /// keeps the file unchangeable until it is dropped (no write, rename or
    /// delete, no rename of a directory above it), so the file QEMU opens is
    /// the file verified; a writer already present refuses the start; a
    /// copy changed before the start is detected by the hash.
    #[test]
    fn start_media_guard_pins_the_verified_file() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let root = crate::test_support::temp_dir("media-guard");
        let dir = root.join("media");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.iso");
        let mut bytes = vec![0u8; 3 << 20];
        let o = usize::try_from(VOLUME_DESCRIPTOR_ID_OFFSET).unwrap();
        bytes[o..o + 5].copy_from_slice(b"CD001");
        std::fs::write(&file, &bytes).unwrap();
        let set_ro = |ro: bool| {
            let mut p = std::fs::metadata(&file).unwrap().permissions();
            p.set_readonly(ro);
            std::fs::set_permissions(&file, p).unwrap();
        };
        set_ro(true);
        let hex = qemu_backend::probe::sha256_file(&file).unwrap();
        let entry = MediaEntry {
            record: MediaRecord {
                id: MediaId::new_v7(),
                kind: MediaKind::InstallIso,
                sha256: Sha256Digest::parse_hex(&hex).unwrap(),
                size_bytes: bytes.len() as u64,
                declared_origin: Label::parse("test").unwrap(),
                approved_at: UtcTimestamp::now(),
                detected_guest: None,
                location: None,
                missing: false,
            },
            location: MediaLocation::Managed(ManagedRelPath::parse("media/m.iso").unwrap()),
            file_identity: None,
        };
        let mut insp = ManagedMediaInspector::new(root.clone());
        let mut g = insp.open_for_start(&entry).unwrap();
        assert_eq!(g.size_bytes(), bytes.len() as u64);
        #[cfg(windows)]
        {
            // Read-only attribute removed by the same user: writes, rename
            // and deletion still refused by the guard's share mode.
            set_ro(false);
            assert!(std::fs::OpenOptions::new().write(true).open(&file).is_err());
            assert!(std::fs::rename(&file, dir.join("other.iso")).is_err());
            assert!(std::fs::remove_file(&file).is_err());
            assert!(std::fs::rename(&dir, root.join("media2")).is_err());
            set_ro(true);
        }
        let read = AtomicU64::new(0);
        let facts = g
            .observe(&|| false, &|n| {
                read.fetch_add(n, Ordering::Relaxed);
            })
            .unwrap();
        assert_eq!(read.load(Ordering::Relaxed), bytes.len() as u64);
        vm_core::media::verify_media(&entry, &facts, UtcTimestamp::now()).unwrap();
        // Cancellation between blocks.
        let e = g.observe(&|| true, &|_| {}).unwrap_err();
        assert_eq!(e.code, ErrorCode::StateConflict);
        drop(g);

        // A process already writing the copy: the start is refused.
        set_ro(false);
        #[cfg(windows)]
        {
            let w = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
            let e = insp.open_for_start(&entry).unwrap_err();
            assert_eq!(e.code, ErrorCode::VmBusy, "{e:?}");
            drop(w);
        }
        // Changed before the start: the hash read through the handle differs.
        bytes[1 << 20] ^= 0xff;
        std::fs::write(&file, &bytes).unwrap();
        set_ro(true);
        let mut g = insp.open_for_start(&entry).unwrap();
        let facts = g.observe(&|| false, &|_| {}).unwrap();
        let e = vm_core::media::verify_media(&entry, &facts, UtcTimestamp::now()).unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        drop(g);
        // Not an optical image, missing copy.
        bytes[o] = b'X';
        set_ro(false);
        std::fs::write(&file, &bytes).unwrap();
        assert_eq!(
            insp.open_for_start(&entry).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            insp.open_for_start(&entry).unwrap_err().code,
            ErrorCode::NotFound
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_checks_run_on_the_handle() {
        let d = crate::test_support::temp_dir("import-handle");
        let iso = |p: &Path, sig: &[u8]| {
            let mut b = vec![0u8; 40 * 1024];
            let o = usize::try_from(VOLUME_DESCRIPTOR_ID_OFFSET).unwrap();
            b[o..o + sig.len()].copy_from_slice(sig);
            std::fs::write(p, b).unwrap();
        };
        let real = d.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let good = real.join("good.iso");
        iso(&good, b"CD001");
        let path = good.to_str().unwrap();
        assert_eq!(
            validate_import_source(path, MediaKind::InstallIso).unwrap(),
            good
        );
        // The handle is left at the start for the hash.
        let f = open_source(&good).unwrap();
        check_import_handle(&f, &good, MediaKind::InstallIso).unwrap();
        let mut first = [1u8; 4];
        std::io::Read::read_exact(&mut &f, &mut first).unwrap();
        assert_eq!(first, [0; 4]);
        let bad = real.join("zip.iso");
        iso(&bad, b"PK\x03\x04\x00");
        assert_eq!(
            validate_import_source(bad.to_str().unwrap(), MediaKind::InstallIso)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        let empty = real.join("empty.iso");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            validate_import_source(empty.to_str().unwrap(), MediaKind::InstallIso)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            validate_import_source(
                real.join("missing.iso").to_str().unwrap(),
                MediaKind::DataIso
            )
            .unwrap_err()
            .code,
            ErrorCode::NotFound
        );
        // Intermediate junction (Windows): the final path differs from the
        // requested one, refused.
        #[cfg(windows)]
        {
            let j = d.join("j");
            let ok = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&j)
                .arg(&real)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                let through = j.join("good.iso");
                let e = validate_import_source(through.to_str().unwrap(), MediaKind::InstallIso)
                    .unwrap_err();
                assert_eq!(e.code, ErrorCode::PermissionDenied, "{e:?}");
                assert!(e.message.contains("real location"), "{e:?}");
                let _ = std::fs::remove_dir(&j);
            }
            // Different case is the same file: accepted.
            let upper = good.to_str().unwrap().to_uppercase();
            assert!(validate_import_source(&upper, MediaKind::InstallIso).is_ok());
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// ISO image of `len` bytes with the ISO 9660 signature.
    fn iso_bytes(len: usize, fill: u8) -> Vec<u8> {
        let mut b = vec![fill; len];
        let o = usize::try_from(VOLUME_DESCRIPTOR_ID_OFFSET).unwrap();
        b[o..o + 5].copy_from_slice(b"CD001");
        b
    }

    fn import(
        store: &mut SqliteStore,
        path: &Path,
        kind: MediaKind,
    ) -> Result<MediaEntry, PublicError> {
        use crate::engine::MediaImportPort as _;
        let job = store.begin_import(path, kind, &Label::parse("test").unwrap())?;
        let done = job.run();
        store.finish_import(done)
    }

    /// ADR-0029: an import records the user's file in place (no copy under
    /// `media/`, the absolute path the system resolved), the start opens and
    /// hashes that same file, a change between runs is refused naming the
    /// file, and a missing file is reported by name and marked `missing`,
    /// then usable again once back.
    #[test]
    fn imported_iso_is_used_in_place_and_verified_at_every_start() {
        use crate::engine::MediaImportPort as _;
        let root = crate::test_support::temp_dir("in-place-catalog");
        let isos = crate::test_support::temp_dir("in-place-isos");
        let iso = isos.join("ubuntu.iso");
        let bytes = iso_bytes(3 << 20, 0);
        std::fs::write(&iso, &bytes).unwrap();
        let mut store = SqliteStore::open(&root).unwrap();
        let entry = import(&mut store, &iso, MediaKind::InstallIso).unwrap();
        let MediaLocation::Referenced(recorded) = &entry.location else {
            panic!("{entry:?}")
        };
        assert!(recorded.is_absolute());
        assert!(
            same_windows_path(recorded, &iso) || recorded == &std::fs::canonicalize(&iso).unwrap()
        );
        assert_eq!(entry.record.location.as_deref(), recorded.to_str());
        assert!(!entry.record.missing);
        assert_eq!(
            std::fs::read_dir(root.join("media")).unwrap().count(),
            0,
            "nothing copied into the catalog"
        );
        assert_eq!(
            std::fs::read(&iso).unwrap(),
            bytes,
            "the user's file untouched"
        );
        let listed = store.list_media().unwrap();
        assert_eq!(listed, vec![entry.record.clone()]);

        // Start: the guard opens the same file and the hash matches.
        let mut insp = ManagedMediaInspector::new(root.clone());
        let mut g = insp.open_for_start(&entry).unwrap();
        #[cfg(windows)]
        {
            assert!(std::fs::OpenOptions::new().write(true).open(&iso).is_err());
            assert!(std::fs::remove_file(&iso).is_err());
        }
        let facts = g.observe(&|| false, &|_| {}).unwrap();
        vm_core::media::verify_media(&entry, &facts, UtcTimestamp::now()).unwrap();
        drop(g);

        // Changed between runs: the start is refused, naming the file only.
        let mut changed = bytes.clone();
        changed[2 << 20] ^= 0xff;
        std::fs::write(&iso, &changed).unwrap();
        let mut g = insp.open_for_start(&entry).unwrap();
        let facts = g.observe(&|| false, &|_| {}).unwrap();
        let e = vm_core::media::verify_media(&entry, &facts, UtcTimestamp::now()).unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied);
        assert!(
            e.message
                .contains("\u{201c}ubuntu.iso\u{201d} changed since it was selected"),
            "{e:?}"
        );
        assert!(
            !e.message.contains(isos.to_str().unwrap()),
            "no path: {e:?}"
        );
        drop(g);
        // Selected again: same medium, new hash, usable.
        let again = import(&mut store, &iso, MediaKind::InstallIso).unwrap();
        assert_eq!(again.record.id, entry.record.id);
        let mut g = insp.open_for_start(&again).unwrap();
        let facts = g.observe(&|| false, &|_| {}).unwrap();
        vm_core::media::verify_media(&again, &facts, UtcTimestamp::now()).unwrap();
        drop(g);

        // Moved away: not found, by name, marked missing; listed as such.
        let moved = isos.join("moved.iso");
        std::fs::rename(&iso, &moved).unwrap();
        let e = insp.open_for_start(&again).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        assert!(
            e.message
                .contains("\u{201c}ubuntu.iso\u{201d} is no longer where it was selected"),
            "{e:?}"
        );
        assert!(
            !e.message.contains(isos.to_str().unwrap()),
            "no path: {e:?}"
        );
        store.mark_media_missing(again.record.id);
        let listed = store.list_media().unwrap();
        assert!(listed[0].missing, "{listed:?}");
        assert!(
            store
                .get_media(again.record.id)
                .unwrap()
                .unwrap()
                .record
                .missing
        );
        // Back in place: listed as available again.
        std::fs::rename(&moved, &iso).unwrap();
        assert!(!store.list_media().unwrap()[0].missing);

        // Deleting the medium keeps the user's file.
        store.remove_media(again.record.id).unwrap();
        assert!(store.list_media().unwrap().is_empty());
        assert_eq!(std::fs::read(&iso).unwrap(), changed);
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&isos);
    }

    /// Files that cannot be used in place are refused at import with a
    /// clear reason: inside the catalog folder, or at a path QEMU's option
    /// syntax would misread.
    #[test]
    fn import_refuses_paths_that_cannot_be_used_in_place() {
        let root = crate::test_support::temp_dir("in-place-refusals");
        let mut store = SqliteStore::open(&root).unwrap();
        let inside = root.join("vms").join("inside.iso");
        std::fs::write(&inside, iso_bytes(40 * 1024, 0)).unwrap();
        let e = import(&mut store, &inside, MediaKind::DataIso).unwrap_err();
        assert_eq!(e.code, ErrorCode::PermissionDenied, "{e:?}");
        assert!(e.message.contains("catalog folder"), "{e:?}");
        let isos = crate::test_support::temp_dir("in-place-comma");
        let comma = isos.join("windows,version.iso");
        std::fs::write(&comma, iso_bytes(40 * 1024, 0)).unwrap();
        let e = import(&mut store, &comma, MediaKind::InstallIso).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{e:?}");
        assert!(e.message.contains("rename the file"), "{e:?}");
        assert!(store.list_media().unwrap().is_empty());
        drop(store);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&isos);
    }
}
