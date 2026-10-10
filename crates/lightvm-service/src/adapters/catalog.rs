//! Adapter `vm_core::ports::CatalogStore` → `vm_storage::Catalog` (SQLite).
//!
//! The core and the storage use the same identifiers from `lightvm-proto` but
//! different records; the adapter converts both ways and covers three gaps in
//! the current `vm-storage` API (reported in the F1-04 verification file):
//!
//! 1. `Tx::insert_vm` generates the `VmId` itself, while the core chooses it
//!    through the `IdGenerator` port: the core's `VmRecord` is kept in
//!    `config_json` (`{"core": VmRecord}`) and the adapter maintains the
//!    mapping core `VmId` → storage `VmId`, rebuilt on open;
//! 2. `Catalog` reads require `&mut self` while the port's reads are `&self`:
//!    the catalog lives in a `RefCell` (a single thread, the supervisor, owns
//!    the adapter);
//! 3. the storage operation does not keep the session and does not update the
//!    `run_id` after insertion; `finish_run` uses the storage's timestamp.
//!
//! No path comes from clients: the managed media paths are validated twice
//! (`ManagedRelPath` of the proto and `ManagedPath` of the storage).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use lightvm_proto::{
    ErrorCode, Label, ManagedRelPath, MediaEntry, MediaId, MediaKind, MediaRecord, OperationId,
    OperationKind, OperationRecord, OperationStatus, PublicError, RunEndReason, RunId, RunRecord,
    Sha256Digest, UtcTimestamp, VmId, VmRecord, VmState,
};
use serde::{Deserialize, Serialize};
use vm_core::ports::CatalogStore;
use vm_storage as st;

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
    /// Core `VmId` → `VmId` of the row in the storage.
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
    pub fn root(&self) -> std::path::PathBuf {
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

    fn media_entry(m: st::MediaRecord) -> Result<Option<MediaEntry>, PublicError> {
        if m.status != st::MediaStatus::Approved {
            return Ok(None);
        }
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
            },
            managed_path: ManagedRelPath::parse(m.managed_path.as_str())
                .map_err(|_| infra("invalid managed path"))?,
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
        let sid = self.write(|tx| {
            let sid = tx.insert_vm(&new)?;
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
            managed_path: st::ManagedPath::parse(entry.managed_path.as_str())
                .map_err(|e| storage_error(&e))?,
            status: st::MediaStatus::Approved,
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

    fn upsert_operation(&mut self, op: &OperationRecord) -> Result<(), PublicError> {
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
        let rec = st::RunRecord {
            run_id: run.id,
            vm_id: sid,
            // Per-run baseline, overlay and firmware arrive with F3.
            baseline_id: None,
            overlay_disk_id: None,
            vars_fw_id: None,
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

/// Inspector of the managed media copies: SHA-256 hash and size read from the
/// file in the catalog; the comparison with the registry is done by the core.
///
/// TODO(F1-04): the OS identity of the file and the open protected against
/// reparse points belong to the storage (no API yet); the inspection does not
/// happen on the same handle handed to QEMU.
#[derive(Debug, Clone)]
pub struct ManagedMediaInspector {
    root: std::path::PathBuf,
}

impl ManagedMediaInspector {
    /// Inspector for the catalog root.
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl vm_core::ports::MediaInspector for ManagedMediaInspector {
    fn inspect(
        &mut self,
        entry: &MediaEntry,
    ) -> Result<vm_core::media::ObservedMediaFacts, PublicError> {
        let path = entry
            .managed_path
            .as_str()
            .split('/')
            .fold(self.root.clone(), |p, c| p.join(c));
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|_| PublicError::not_found("managed copy of the medium missing"))?;
        if !meta.is_file() {
            return Err(PublicError::permission_denied(
                "managed copy of the medium is not a regular file",
            ));
        }
        if !is_optical_image(&path) {
            return Err(PublicError::invalid_argument(
                "the medium is not an ISO 9660 or UDF image",
            ));
        }
        let hex = qemu_backend::probe::sha256_file(&path)
            .map_err(|_| infra("reading the managed copy failed"))?;
        Ok(vm_core::media::ObservedMediaFacts {
            sha256: Sha256Digest::parse_hex(&hex).map_err(|_| infra("invalid hash"))?,
            size_bytes: meta.len(),
            file_identity: None,
            read_only: meta.permissions().readonly(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
