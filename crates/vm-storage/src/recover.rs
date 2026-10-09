//! Riconciliazione dopo un'interruzione (ROADMAP 6.3, 7.4 riga "Crash", 8).
//!
//! Eseguita all'apertura del catalogo (sotto il lock) e invocabile con
//! [`Catalog::recover`]. Regole per gli intenti non conclusi:
//!
//! | Tipo | Situazione trovata | Azione |
//! | --- | --- | --- |
//! | creazione | riga DB assente | rimuove temporaneo e file finale orfano → `aborted` |
//! | creazione | riga presente, file finale presente | completa → `committed` |
//! | creazione | riga presente, file solo nel temporaneo | completa la rinomina → `committed` |
//! | creazione | riga presente, nessun file | VM `recovering`, overlay `missing`, esecuzioni `failed`; baseline intatta → `aborted` |
//! | cancellazione | file ancora al suo posto | annulla, disco di nuovo `ready` → `aborted` |
//! | cancellazione | file nel temporaneo, riga presente | completa la cancellazione della riga; se un riferimento la impedisce ripristina il file → `committed`/`aborted` |
//! | cancellazione | nessun file, riga presente | come "riga presente, nessun file" della creazione → `aborted` |
//! | export | qualunque | rimuove il temporaneo → `aborted` (la destinazione esterna non è mai toccata) |
//!
//! Poi rimuove i temporanei rimasti di cancellazioni ed export già confermati
//! e verifica che i file di dischi, firmware e media registrati esistano.
//! Un file di base mancante viene solo segnalato: la riconciliazione non
//! modifica mai righe o file di una baseline.

use rusqlite::params;

use crate::catalog::{Catalog, Intent, exists_no_follow, remove_temp, target_exists};
use crate::compat::{ArtifactId, BaselineId, DiskId, FirmwareId, MediaId, VmId, parse_id};
use crate::enums::{DiskRole, DiskStatus, IntentKind, IntentState, Lifecycle, TargetKind};
use crate::error::{Result, SqlResultExt, StorageError};
use crate::records::Tx;
use crate::time::now_rfc3339;

/// Esito complessivo della riconciliazione.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Intenti non conclusi trovati e risolti.
    pub intents: Vec<IntentRecovery>,
    /// Intenti già confermati di cui è stato rimosso il temporaneo residuo.
    pub leftover_temps_removed: Vec<i64>,
    /// Incoerenze tra DB e filesystem fuori dal journal.
    pub integrity: Vec<IntegrityIssue>,
}

/// Esito per un intento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRecovery {
    /// Intento.
    pub intent_id: i64,
    /// Tipo.
    pub kind: IntentKind,
    /// Stato trovato.
    pub prior_state: IntentState,
    /// Classe dell'azione.
    pub outcome: RecoveryOutcome,
    /// Dettaglio registrato in `journal.resolution`.
    pub detail: String,
}

/// Classe dell'azione di riconciliazione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// Annullato, filesystem riportato allo stato precedente.
    RolledBack,
    /// Completato.
    RolledForward,
    /// DB presente e file assente: VM in `recovering`, nessuna modifica alla baseline.
    MarkedRecovering,
}

/// Incoerenza rilevata dalla verifica di integrità.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityIssue {
    /// File di un disco non di baseline mancante: disco `missing`, VM `recovering`.
    DiskMissing {
        /// Disco.
        disk_id: String,
        /// Percorso gestito.
        path: String,
    },
    /// File di una base mancante: solo segnalato, VM `recovering`.
    BaseDiskMissing {
        /// Disco.
        disk_id: String,
        /// Percorso gestito.
        path: String,
    },
    /// File di uno stato firmware mancante: VM `recovering`.
    FirmwareMissing {
        /// Stato firmware.
        fw_id: String,
        /// Percorso gestito.
        path: String,
    },
    /// Copia gestita di un media mancante: media `missing`.
    MediaMissing {
        /// Media.
        media_id: String,
    },
}

impl Catalog {
    /// Riconcilia gli intenti non conclusi e verifica l'integrità dei file
    /// registrati. Idempotente.
    pub fn recover(&mut self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        for intent in self.open_intents()? {
            let prior = self.intent_state(intent.id)?;
            let (outcome, detail, state) = self.recover_one(&intent, prior)?;
            self.set_intent_state(intent.id, state, Some(&detail))?;
            report.intents.push(IntentRecovery {
                intent_id: intent.id,
                kind: intent.kind,
                prior_state: prior,
                outcome,
                detail,
            });
        }
        report.leftover_temps_removed = self.remove_committed_leftovers()?;
        report.integrity = self.integrity_scan()?;
        Ok(report)
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
                    return Ok((
                        RolledForward,
                        "rolled_forward".into(),
                        IntentState::Committed,
                    ));
                }
                if temp_exists && let (Some(dest), Some(rel)) = (&i.final_path, &i.final_rel) {
                    self.layout.resolve(rel)?;
                    std::fs::rename(&i.temp_path, dest).map_err(|e| StorageError::io(dest, e))?;
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

    /// DB presente e file assente: VM in `recovering`; un disco non di base
    /// diventa `missing` e le esecuzioni aperte che lo usano `failed`.
    /// Nessuna modifica a righe o file di baseline.
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
                     WHERE state = 'committed' AND kind IN ('delete_file','export')
                       AND resolution IS NULL",
                )
                .db()?;
            let rows = st.query_map([], |r| r.get(0)).db()?;
            rows.collect::<rusqlite::Result<_>>().db()?
        };
        let mut removed = Vec::new();
        for id in ids {
            let i = self.intent_row(id)?;
            if exists_no_follow(&i.temp_path)? {
                remove_temp(&self.layout, &i.temp_rel)?;
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
                if exists_no_follow(&layout.resolve(&d.managed_path)?)? {
                    continue;
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
                if exists_no_follow(&layout.resolve(&f.managed_path)?)? {
                    continue;
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
                    .prepare("SELECT media_id, managed_path FROM media WHERE status = 'approved'")
                    .db()?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).db()?;
                rows.collect::<rusqlite::Result<_>>().db()?
            };
            for (id, path) in media {
                let rel = crate::paths::ManagedPath::parse(&path)?;
                if !exists_no_follow(&layout.resolve(&rel)?)? {
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
        TargetKind::None => Ok(()),
    }
}
