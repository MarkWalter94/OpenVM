//! Adattatore `vm_core::ports::CatalogStore` → `vm_storage::Catalog` (SQLite).
//!
//! Il core e lo storage usano gli stessi identificativi di `lightvm-proto` ma
//! record diversi; l'adattatore converte nei due sensi e copre tre lacune
//! dell'API attuale di `vm-storage` (riportate nel file di verifica F1-04):
//!
//! 1. `Tx::insert_vm` genera da sé il `VmId`, mentre il core lo sceglie con la
//!    porta `IdGenerator`: il `VmRecord` del core viene conservato in
//!    `config_json` (`{"core": VmRecord}`) e l'adattatore mantiene la
//!    corrispondenza `VmId` del core → `VmId` dello storage, ricostruita
//!    all'apertura;
//! 2. le letture di `Catalog` richiedono `&mut self` mentre le letture della
//!    porta sono `&self`: il catalogo sta in un `RefCell` (un solo thread, il
//!    supervisor, possiede l'adattatore);
//! 3. l'operazione dello storage non conserva la sessione e non aggiorna il
//!    `run_id` dopo l'inserimento; `finish_run` usa l'istante dello storage.
//!
//! Nessun percorso arriva dai client: i percorsi gestiti dei media sono
//! validati due volte (`ManagedRelPath` del proto e `ManagedPath` dello storage).

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

/// Versione del formato `config_json` scritto da questo adattatore.
const CONFIG_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct StoredConfig {
    /// Record completo del core (identificativo, configurazione, revisione).
    core: VmRecord,
}

/// Converte un errore dello storage in errore pubblico senza percorsi host.
pub fn storage_error(e: &st::StorageError) -> PublicError {
    if let st::StorageError::NotFound { kind, .. } = e {
        return PublicError::not_found(format!("{kind} inesistente nel catalogo"));
    }
    let code = e
        .public_code()
        .and_then(|c| serde_json::from_value::<ErrorCode>(serde_json::Value::String(c.into())).ok())
        .unwrap_or(ErrorCode::InfrastructureError);
    // Il testo dello storage può contenere percorsi: verso i client va solo il codice.
    PublicError::new(code, "errore del catalogo")
}

fn infra(what: &str) -> PublicError {
    PublicError::infrastructure(format!("catalogo: {what}"))
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
        _ => Err(infra("enumerazione non serializzabile")),
    }
}

fn from_snake<T: for<'de> Deserialize<'de>>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

fn ts(s: &str) -> Result<UtcTimestamp, PublicError> {
    s.parse().map_err(|_| infra("istante non valido"))
}

/// Catalogo SQLite di `vm-storage` visto dal core.
#[derive(Debug)]
pub struct SqliteStore {
    cat: RefCell<st::Catalog>,
    /// `VmId` del core → `VmId` della riga nello storage.
    ids: BTreeMap<VmId, VmId>,
}

impl SqliteStore {
    /// Apre (o crea) il catalogo in `root`: lock del catalogo, migrazioni,
    /// riconciliazione del journal (vedi `vm_storage::Catalog::open`).
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

    /// Radice del catalogo (per i percorsi gestiti dei media).
    pub fn root(&self) -> std::path::PathBuf {
        self.cat.borrow().layout().root().to_path_buf()
    }

    fn sid(&self, id: VmId) -> Result<VmId, PublicError> {
        self.ids
            .get(&id)
            .copied()
            .ok_or_else(|| PublicError::not_found("VM inesistente nel catalogo"))
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
        let stored: StoredConfig = serde_json::from_value(row.config)
            .map_err(|_| infra("configurazione VM illeggibile"))?;
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
                sha256: Sha256Digest::parse_hex(&m.sha256).map_err(|_| infra("hash non valido"))?,
                size_bytes: m.size_bytes,
                declared_origin: Label::parse(m.declared_origin)
                    .map_err(|_| infra("origine non valida"))?,
                approved_at: ts(&m.approved_at)?,
            },
            managed_path: ManagedRelPath::parse(m.managed_path.as_str())
                .map_err(|_| infra("percorso gestito non valido"))?,
            // TODO(F1): lo storage non conserva l'identità OS del file.
            file_identity: None,
        }))
    }
}

impl CatalogStore for SqliteStore {
    fn insert_vm(&mut self, vm: &VmRecord) -> Result<(), PublicError> {
        let config = serde_json::to_value(StoredConfig { core: vm.clone() })
            .map_err(|_| infra("configurazione non serializzabile"))?;
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
        rows.into_iter().map(Self::vm_record).collect()
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
            .ok_or_else(|| infra("operazione di una VM sconosciuta"))?;
        let kind: OperationKind =
            from_snake(&row.kind).ok_or_else(|| infra("tipo di operazione sconosciuto"))?;
        Ok(Some(OperationRecord {
            id: row.operation_id,
            vm_id,
            run_id: row.run_id,
            // TODO(F1): lo storage non conserva la sessione dell'operazione.
            session_id: None,
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
            // Baseline, overlay e firmware per esecuzione arrivano con F3.
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

/// Ispettore delle copie gestite dei media: hash SHA-256 e dimensione letti
/// dal file nel catalogo; il confronto con il registro è del core.
///
/// TODO(F1-04): l'identità OS del file e l'apertura protetta contro reparse
/// point spettano allo storage (nessuna API ancora); l'ispezione non avviene
/// sullo stesso handle consegnato a QEMU.
#[derive(Debug, Clone)]
pub struct ManagedMediaInspector {
    root: std::path::PathBuf,
}

impl ManagedMediaInspector {
    /// Ispettore per la radice del catalogo.
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
            .map_err(|_| PublicError::not_found("copia gestita del supporto assente"))?;
        if !meta.is_file() {
            return Err(PublicError::permission_denied(
                "copia gestita del supporto non è un file regolare",
            ));
        }
        let hex = qemu_backend::probe::sha256_file(&path)
            .map_err(|_| infra("lettura della copia gestita non riuscita"))?;
        Ok(vm_core::media::ObservedMediaFacts {
            sha256: Sha256Digest::parse_hex(&hex).map_err(|_| infra("hash non valido"))?,
            size_bytes: meta.len(),
            file_identity: None,
            read_only: meta.permissions().readonly(),
        })
    }
}
