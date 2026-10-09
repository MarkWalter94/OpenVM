//! Record del catalogo e operazioni SQL dentro una transazione.
//!
//! Tutte le scritture passano da [`Tx`]: una transazione SQLite aperta da
//! [`crate::Catalog::with_tx`] o, per gli intenti del journal, da
//! [`crate::Catalog::commit`], che registra il passaggio a `committed` nella
//! stessa transazione.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::compat::{
    ArtifactId, BaselineId, DiskId, FirmwareId, GrantId, IdSql, MediaId, OperationId, RunId,
    SessionId, VmId, idc,
};
use crate::enums::{
    ArtifactKind, AuditOutcome, DiskRole, DiskStatus, FirmwareScope, Lifecycle, MediaKind,
    MediaSlot, MediaStatus, OperationState, RunState, SessionState, TerminationCause,
};
use crate::error::{Result, SqlResultExt, StorageError};
use crate::paths::ManagedPath;
use crate::time::now_rfc3339;

fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StorageError::Constraint(format!("valore fuori intervallo: {v}")))
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

/// Dati per creare una VM.
#[derive(Debug, Clone)]
pub struct NewVm {
    /// Nome univoco (senza distinzione maiuscole/minuscole).
    pub name: String,
    /// Sistema operativo guest dichiarato.
    pub guest_os: String,
    /// Versione dello schema della configurazione.
    pub config_version: u32,
    /// Configurazione serializzata in JSON (mai interpretata come opzioni host).
    pub config: serde_json::Value,
}

/// VM nel catalogo.
#[derive(Debug, Clone, PartialEq)]
pub struct VmRecord {
    /// Identificativo.
    pub vm_id: VmId,
    /// Nome.
    pub name: String,
    /// Sistema operativo guest.
    pub guest_os: String,
    /// Ultimo stato persistito.
    pub lifecycle: Lifecycle,
    /// Versione dello schema della configurazione.
    pub config_version: u32,
    /// Configurazione.
    pub config: serde_json::Value,
    /// Revisione (controllo di concorrenza ottimistico).
    pub revision: u64,
    /// SHA-256 del manifest su file.
    pub manifest_sha256: Option<String>,
    /// Creazione (UTC).
    pub created_at: String,
    /// Ultimo aggiornamento (UTC).
    pub updated_at: String,
}

/// Media del registro approvato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRecord {
    /// Identificativo.
    pub media_id: MediaId,
    /// SHA-256 della copia gestita.
    pub sha256: String,
    /// Dimensione in byte.
    pub size_bytes: u64,
    /// Tipo.
    pub kind: MediaKind,
    /// Origine dichiarata dall'utente (testo, mai un percorso da aprire).
    pub declared_origin: String,
    /// Approvazione (UTC).
    pub approved_at: String,
    /// Copia gestita relativa alla radice.
    pub managed_path: ManagedPath,
    /// Stato.
    pub status: MediaStatus,
}

/// Disco gestito.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRecord {
    /// Identificativo.
    pub disk_id: DiskId,
    /// VM proprietaria.
    pub vm_id: VmId,
    /// Ruolo.
    pub role: DiskRole,
    /// Base di un overlay.
    pub backing_disk_id: Option<DiskId>,
    /// Dimensione virtuale.
    pub virtual_size_bytes: u64,
    /// Byte occupati misurati.
    pub allocated_bytes: u64,
    /// Percorso gestito.
    pub managed_path: ManagedPath,
    /// SHA-256 (per le basi).
    pub sha256: Option<String>,
    /// Stato.
    pub status: DiskStatus,
    /// Creazione (UTC).
    pub created_at: String,
}

/// Stato firmware gestito.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareRecord {
    /// Identificativo.
    pub fw_id: FirmwareId,
    /// VM.
    pub vm_id: VmId,
    /// Ambito.
    pub scope: FirmwareScope,
    /// Origine della copia.
    pub source_fw_id: Option<FirmwareId>,
    /// Percorso gestito.
    pub managed_path: ManagedPath,
    /// SHA-256.
    pub sha256: Option<String>,
    /// Dimensione.
    pub size_bytes: u64,
    /// Creazione (UTC).
    pub created_at: String,
}

/// Baseline a VM spenta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineRecord {
    /// Identificativo.
    pub baseline_id: BaselineId,
    /// VM.
    pub vm_id: VmId,
    /// Disco base immutabile.
    pub base_disk_id: DiskId,
    /// Stato firmware della baseline.
    pub vars_fw_id: Option<FirmwareId>,
    /// Etichetta.
    pub label: String,
    /// SHA-256 del manifest.
    pub manifest_sha256: String,
    /// Arresto pulito osservato (un arresto forzato non è mai "verificato").
    pub clean_shutdown: bool,
    /// Baseline iniziale protetta (non cancellabile).
    pub protected: bool,
    /// Creazione (UTC).
    pub created_at: String,
}

/// Esecuzione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    /// Identificativo.
    pub run_id: RunId,
    /// VM.
    pub vm_id: VmId,
    /// Baseline di partenza.
    pub baseline_id: Option<BaselineId>,
    /// Overlay dell'esecuzione.
    pub overlay_disk_id: Option<DiskId>,
    /// Vars dell'esecuzione.
    pub vars_fw_id: Option<FirmwareId>,
    /// Stato.
    pub state: RunState,
    /// Inizio (UTC).
    pub started_at: String,
    /// Fine (UTC).
    pub ended_at: Option<String>,
    /// Causa di terminazione.
    pub termination_cause: Option<TerminationCause>,
}

/// Operazione con revisione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    /// Identificativo.
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Esecuzione.
    pub run_id: Option<RunId>,
    /// Tipo (testo stabile definito dal core).
    pub kind: String,
    /// Stato.
    pub state: OperationState,
    /// Revisione della VM attesa dall'operazione.
    pub revision: u64,
    /// Codice di errore pubblico.
    pub error_code: Option<String>,
    /// Creazione (UTC).
    pub created_at: String,
    /// Aggiornamento (UTC).
    pub updated_at: String,
}

/// Sessione agente (solo persistenza).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Identificativo.
    pub session_id: SessionId,
    /// Obiettivo dichiarato.
    pub objective: String,
    /// Stato.
    pub state: SessionState,
    /// Revisione della policy applicata.
    pub policy_revision: u64,
    /// Creazione (UTC).
    pub created_at: String,
    /// Scadenza (UTC).
    pub expires_at: String,
    /// Fine (UTC).
    pub ended_at: Option<String>,
    /// Causa di terminazione.
    pub termination_cause: Option<String>,
}

/// Grant di sessione su una VM (solo persistenza; nessuna credenziale).
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRecord {
    /// Identificativo.
    pub grant_id: GrantId,
    /// Sessione.
    pub session_id: SessionId,
    /// VM autorizzata.
    pub vm_id: VmId,
    /// Capability approvate.
    pub capabilities: serde_json::Value,
    /// Quote approvate.
    pub quotas: serde_json::Value,
    /// Creazione (UTC).
    pub created_at: String,
    /// Scadenza (UTC).
    pub expires_at: String,
    /// Revoca (UTC).
    pub revoked_at: Option<String>,
}

/// Evento di audit da registrare. Nessun payload: solo tipo, id ed esito.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Tipo di azione (testo stabile, es. `vm.start`).
    pub action: String,
    /// Esito.
    pub outcome: Option<AuditOutcome>,
    /// VM.
    pub vm_id: Option<VmId>,
    /// Esecuzione.
    pub run_id: Option<RunId>,
    /// Sessione.
    pub session_id: Option<SessionId>,
    /// Operazione.
    pub operation_id: Option<OperationId>,
    /// Codice di errore pubblico.
    pub error_code: Option<String>,
}

/// Evento di audit registrato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// Ordine monotono.
    pub seq: i64,
    /// Timestamp (UTC).
    pub at: String,
    /// Evento.
    pub event: AuditEvent,
}

/// Artefatto di un'esecuzione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// Identificativo.
    pub artifact_id: ArtifactId,
    /// Esecuzione.
    pub run_id: RunId,
    /// Tipo.
    pub kind: ArtifactKind,
    /// Percorso gestito.
    pub managed_path: ManagedPath,
    /// SHA-256.
    pub sha256: String,
    /// Dimensione.
    pub size_bytes: u64,
    /// Creazione (UTC).
    pub created_at: String,
    /// Conservazione fino a (UTC).
    pub retain_until: Option<String>,
}

/// Transazione sul catalogo.
#[derive(Debug)]
pub struct Tx<'c> {
    pub(crate) inner: rusqlite::Transaction<'c>,
}

impl Tx<'_> {
    pub(crate) fn conn(&self) -> &rusqlite::Connection {
        &self.inner
    }

    // ---------------------------------------------------------------- VM

    /// Crea una VM. Il nome è univoco senza distinzione di maiuscole.
    pub fn insert_vm(&self, vm: &NewVm) -> Result<VmId> {
        let id = VmId::new_v7();
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

    /// Legge una VM.
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

    /// Elenco degli identificativi delle VM ordinati per nome.
    pub fn vm_ids(&self) -> Result<Vec<VmId>> {
        let mut st = self
            .inner
            .prepare("SELECT vm_id FROM vms ORDER BY name")
            .db()?;
        let rows = st.query_map([], |r| idc(r, 0)).db()?;
        rows.collect::<rusqlite::Result<Vec<VmId>>>().db()
    }

    /// Aggiorna la configurazione se la revisione attesa coincide; restituisce
    /// la nuova revisione. Revisione diversa: [`StorageError::StateConflict`].
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
                "revisione attesa {expected_revision}, corrente {current}"
            )));
        }
        Ok(expected_revision + 1)
    }

    /// Aggiorna l'ultimo stato persistito del ciclo di vita.
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

    /// Cancella una VM senza dischi, baseline, esecuzioni o firmware.
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

    /// Registra un media approvato (la copia gestita va prodotta con un intento
    /// `import_media`).
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

    /// Legge un media.
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

    /// Collega un media approvato a una VM.
    pub fn attach_media(&self, vm: &VmId, slot: MediaSlot, media: &MediaId) -> Result<()> {
        let m = self.media(media)?;
        if m.status != MediaStatus::Approved {
            return Err(StorageError::StateConflict(format!(
                "media {media} non approvato ({})",
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

    /// Scollega il media di uno slot.
    pub fn detach_media(&self, vm: &VmId, slot: MediaSlot) -> Result<()> {
        self.inner
            .execute(
                "DELETE FROM vm_media WHERE vm_id = ?1 AND slot = ?2",
                params![vm.sql(), slot],
            )
            .db()?;
        Ok(())
    }

    /// Media collegati a una VM.
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

    /// Cancella la riga di un media non referenziato.
    pub fn delete_media(&self, id: &MediaId) -> Result<()> {
        self.inner
            .execute("DELETE FROM media WHERE media_id = ?1", params![id.sql()])
            .db()?;
        Ok(())
    }

    // ------------------------------------------------------------- dischi

    /// Registra un disco gestito.
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

    /// Legge un disco.
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

    /// Dischi di una VM.
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

    /// Aggiorna lo stato di un disco.
    pub fn set_disk_status(&self, id: &DiskId, status: DiskStatus) -> Result<()> {
        self.inner
            .execute(
                "UPDATE disks SET status = ?1 WHERE disk_id = ?2",
                params![status, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Aggiorna i byte occupati misurati.
    pub fn set_disk_allocated(&self, id: &DiskId, bytes: u64) -> Result<()> {
        self.inner
            .execute(
                "UPDATE disks SET allocated_bytes = ?1 WHERE disk_id = ?2",
                params![to_i64(bytes)?, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Cancella la riga di un disco non referenziato.
    pub fn delete_disk(&self, id: &DiskId) -> Result<()> {
        self.inner
            .execute("DELETE FROM disks WHERE disk_id = ?1", params![id.sql()])
            .db()?;
        Ok(())
    }

    // ----------------------------------------------------------- firmware

    /// Registra uno stato firmware gestito.
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

    /// Legge uno stato firmware.
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

    /// Cancella la riga di uno stato firmware non referenziato.
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

    /// Registra una baseline. La VM deve risultare spenta (`stopped`): la
    /// baseline si crea soltanto a VM spenta (sezione 8, ADR-0005).
    pub fn insert_baseline(&self, b: &BaselineRecord) -> Result<()> {
        let vm = self.vm(&b.vm_id)?;
        if vm.lifecycle != Lifecycle::Stopped {
            return Err(StorageError::StateConflict(format!(
                "baseline richiesta con VM in stato {}",
                vm.lifecycle
            )));
        }
        let disk = self.disk(&b.base_disk_id)?;
        if disk.role != DiskRole::Base || disk.vm_id != b.vm_id {
            return Err(StorageError::StateConflict(
                "il disco della baseline deve essere una base della stessa VM".into(),
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

    /// Legge una baseline.
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

    /// Baseline di una VM.
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

    /// `true` se la baseline è referenziata da esecuzioni o overlay.
    pub fn baseline_referenced(&self, id: &BaselineId) -> Result<bool> {
        self.inner
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM runs WHERE baseline_id = ?1)
                     OR EXISTS (SELECT 1 FROM disks d JOIN baselines b ON d.backing_disk_id = b.base_disk_id
                                WHERE b.baseline_id = ?1)",
                params![id.sql()],
                |r| r.get(0),
            )
            .db()
    }

    /// Cancella una baseline non referenziata e non protetta, con il suo disco
    /// base e il suo stato firmware. Riferimenti: [`StorageError::Referenced`].
    pub fn delete_baseline(&self, id: &BaselineId) -> Result<()> {
        let b = self.baseline(id)?;
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

    // ---------------------------------------------------------- esecuzioni

    /// Registra l'inizio di un'esecuzione.
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

    /// Legge un'esecuzione.
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

    /// Aggiorna lo stato di un'esecuzione non conclusa.
    pub fn set_run_state(&self, id: &RunId, state: RunState) -> Result<()> {
        self.inner
            .execute(
                "UPDATE runs SET state = ?1 WHERE run_id = ?2",
                params![state, id.sql()],
            )
            .db()?;
        Ok(())
    }

    /// Chiude un'esecuzione con causa di terminazione.
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
                "esecuzione {id} assente o già chiusa"
            )));
        }
        Ok(())
    }

    // ---------------------------------------------------------- operazioni

    /// Registra un'operazione.
    pub fn insert_operation(&self, o: &OperationRecord) -> Result<()> {
        self.inner
            .execute(
                "INSERT INTO operations(operation_id, vm_id, run_id, kind, state, revision,
                                        error_code, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    o.operation_id.sql(),
                    o.vm_id.sql(),
                    o.run_id.sql(),
                    o.kind,
                    o.state,
                    to_i64(o.revision)?,
                    o.error_code,
                    o.created_at,
                    o.updated_at
                ],
            )
            .db()?;
        Ok(())
    }

    /// Aggiorna lo stato di un'operazione.
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

    /// Legge un'operazione.
    pub fn operation(&self, id: &OperationId) -> Result<OperationRecord> {
        self.inner
            .query_row(
                "SELECT operation_id, vm_id, run_id, kind, state, revision, error_code, created_at,
                        updated_at FROM operations WHERE operation_id = ?1",
                params![id.sql()],
                |r| {
                    Ok(OperationRecord {
                        operation_id: idc(r, 0)?,
                        vm_id: idc(r, 1)?,
                        run_id: idc(r, 2)?,
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

    // ------------------------------------------------------ sessioni/grant

    /// Registra una sessione.
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

    /// Aggiorna lo stato di una sessione; gli stati finali registrano la fine.
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

    /// Legge una sessione.
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

    /// Registra un grant.
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

    /// Revoca tutti i grant di una sessione.
    pub fn revoke_grants(&self, session: &SessionId) -> Result<usize> {
        self.inner
            .execute(
                "UPDATE grants SET revoked_at = ?1 WHERE session_id = ?2 AND revoked_at IS NULL",
                params![now_rfc3339(), session.sql()],
            )
            .db()
    }

    /// Grant di una sessione.
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

    /// Aggiunge un evento di audit e ne restituisce l'ordine monotono.
    pub fn append_audit(&self, e: &AuditEvent) -> Result<i64> {
        let outcome = e
            .outcome
            .ok_or_else(|| StorageError::Constraint("audit senza esito".into()))?;
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

    /// Eventi di audit con `seq > after`, in ordine.
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

    // ------------------------------------------------------------ artefatti

    /// Registra un artefatto.
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

    /// Cancella la riga di un artefatto.
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
