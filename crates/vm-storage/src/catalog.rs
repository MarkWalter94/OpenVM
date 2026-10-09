//! Catalogo: radice, lock, database, journal a intenti e manifest.

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

/// Esito registrato quando la pubblicazione trova la destinazione già occupata
/// da un file che l'intento non ha creato: quel file non va mai rimosso.
const DESTINATION_CONFLICT: &str = "destination_conflict";

/// Esito dell'apertura del catalogo.
#[derive(Debug, Clone)]
pub struct OpenReport {
    /// Migrazioni applicate.
    pub migration: MigrationReport,
    /// Avvio precedente registrato (UTC), se esiste.
    pub previous_start: Option<String>,
    /// Riconciliazione degli intenti non conclusi.
    pub recovery: RecoveryReport,
    /// Pulizia prudente di `tmp/`.
    pub tmp_cleanup: TmpCleanup,
}

/// Catalogo aperto: tiene il lock di istanza singola per tutta la sua vita.
///
/// Le mutazioni richiedono `&mut self`: insieme al lock del catalogo questo
/// serializza le operazioni distruttive dell'istanza.
#[derive(Debug)]
pub struct Catalog {
    // Ordine dei campi = ordine di rilascio: la connessione prima del lock.
    pub(crate) conn: Connection,
    pub(crate) layout: CatalogLayout,
    pub(crate) started_at: UtcMillis,
    report: Option<OpenReport>,
    _lock: CatalogLock,
}

/// Richiesta di un nuovo intento del journal.
#[derive(Debug, Clone)]
pub struct IntentSpec {
    /// Tipo.
    pub kind: IntentKind,
    /// VM coinvolta (serializza gli intenti per VM).
    pub vm_id: Option<VmId>,
    /// Operazione: intenti della stessa operazione possono coesistere sulla VM.
    pub operation_id: Option<OperationId>,
    /// Tipo di riga coinvolta.
    pub target_kind: TargetKind,
    /// Identificativo della riga coinvolta (che sarà creata o cancellata).
    pub target_id: Option<String>,
    /// Destinazione (creazione) o sorgente (cancellazione) gestita. `None` per export.
    pub final_path: Option<ManagedPath>,
    /// Imposta l'attributo di sola lettura prima dello spostamento (basi, media).
    pub read_only: bool,
}

/// Intento registrato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Identificativo progressivo.
    pub id: i64,
    /// Tipo.
    pub kind: IntentKind,
    /// VM.
    pub vm_id: Option<String>,
    /// Operazione.
    pub operation_id: Option<String>,
    /// Tipo di riga coinvolta.
    pub target_kind: TargetKind,
    /// Riga coinvolta.
    pub target_id: Option<String>,
    /// Percorso temporaneo gestito (`tmp/...`).
    pub temp_rel: ManagedPath,
    /// Percorso finale gestito.
    pub final_rel: Option<ManagedPath>,
    /// Percorso temporaneo assoluto: il chiamante vi scrive il file da creare.
    pub temp_path: PathBuf,
    /// Percorso finale assoluto.
    pub final_path: Option<PathBuf>,
    read_only: bool,
}

impl Catalog {
    /// Apre (o crea) il catalogo in `root`:
    /// lock esclusivo → migrazioni (con backup) → riconciliazione del journal →
    /// pulizia prudente di `tmp/`.
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

    /// Esito dell'apertura.
    pub fn open_report(&self) -> Option<&OpenReport> {
        self.report.as_ref()
    }

    /// Layout della radice.
    pub fn layout(&self) -> &CatalogLayout {
        &self.layout
    }

    /// Istante di apertura di questa istanza.
    pub fn started_at(&self) -> UtcMillis {
        self.started_at
    }

    /// Versione di schema corrente.
    pub fn schema_version(&self) -> Result<u32> {
        migrate::current_version(&self.conn)
    }

    /// Esegue `f` in una transazione `IMMEDIATE`, confermata se `f` riesce.
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

    /// Lettura in una transazione (annullata al termine).
    pub fn read<T>(&mut self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let tx = Tx {
            inner: self.conn.transaction().db()?,
        };
        f(&tx)
    }

    // ------------------------------------------------------------ journal

    /// Registra un intento `pending` prima di qualunque modifica al filesystem.
    ///
    /// Controlli: percorsi validi e senza collegamenti; per la creazione la
    /// destinazione non deve esistere e la riga non deve esistere; per la
    /// cancellazione la sorgente deve esistere e la riga non deve essere
    /// referenziata; un solo gruppo di intenti aperti per VM
    /// ([`StorageError::VmBusy`]).
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
                            "intento di creazione senza destinazione".into(),
                        ));
                    };
                    if spec.target_kind == TargetKind::None || spec.target_id.is_none() {
                        return Err(StorageError::StateConflict(
                            "intento di creazione senza riga di destinazione".into(),
                        ));
                    }
                    if target_exists(tx, spec.target_kind, spec.target_id.as_deref())? {
                        return Err(StorageError::StateConflict(
                            "riga di destinazione già presente".into(),
                        ));
                    }
                    if exists_no_follow(fp)? {
                        return Err(StorageError::StateConflict(format!(
                            "destinazione già esistente: {rel}"
                        )));
                    }
                    if let Some((parent, _)) = rel.as_str().rsplit_once('/') {
                        layout.ensure_dir(&ManagedPath::parse(parent)?)?;
                    }
                }
                IntentKind::DeleteFile => {
                    let (Some(fp), Some(rel)) = (&final_path, &spec.final_path) else {
                        return Err(StorageError::StateConflict(
                            "cancellazione senza sorgente".into(),
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

    /// Completa la parte su filesystem e passa a `applied`.
    ///
    /// - creazione: il file temporaneo (regolare, non collegamento) viene
    ///   sincronizzato, eventualmente reso di sola lettura e rinominato sulla
    ///   destinazione, che non deve esistere;
    /// - cancellazione: la sorgente viene rinominata nel temporaneo;
    /// - export: il temporaneo deve esistere.
    pub fn mark_applied(&mut self, intent: &Intent) -> Result<()> {
        self.expect_state(intent.id, &[IntentState::Pending])?;
        match intent.kind {
            k if k.is_create() => {
                let dest = intent
                    .final_path
                    .as_ref()
                    .ok_or_else(|| StorageError::StateConflict("destinazione assente".into()))?;
                let meta = std::fs::symlink_metadata(&intent.temp_path)
                    .map_err(|e| StorageError::io(&intent.temp_path, e))?;
                if is_link(&meta) || !meta.is_file() {
                    return Err(StorageError::invalid_path(
                        intent.temp_rel.as_str(),
                        PathRejection::Link,
                    ));
                }
                sync_file(&intent.temp_path)?;
                // Ricontrollo dei collegamenti sul percorso di destinazione.
                let rel = intent
                    .final_rel
                    .as_ref()
                    .ok_or_else(|| StorageError::StateConflict("destinazione assente".into()))?;
                self.layout.resolve(rel)?;
                // Pubblicazione senza sovrascrittura (collegamento fisico): una
                // destinazione comparsa nel frattempo non viene mai sostituita.
                if let Err(e) = fsx::publish_no_replace(&intent.temp_path, dest, rel) {
                    if matches!(e, StorageError::StateConflict(_)) {
                        // La destinazione non è nostra: l'annullamento e la
                        // riconciliazione non devono rimuoverla.
                        self.set_resolution(intent.id, DESTINATION_CONFLICT)?;
                    }
                    return Err(e);
                }
                if intent.read_only {
                    fsx::ensure_readonly(dest)?;
                }
            }
            IntentKind::DeleteFile => {
                let src = intent
                    .final_path
                    .as_ref()
                    .ok_or_else(|| StorageError::StateConflict("sorgente assente".into()))?;
                if let Some(rel) = &intent.final_rel {
                    self.layout.resolve(rel)?;
                }
                std::fs::rename(src, &intent.temp_path).map_err(|e| StorageError::io(src, e))?;
            }
            _ => {
                if !exists_no_follow(&intent.temp_path)? {
                    return Err(StorageError::NotFound {
                        kind: "file temporaneo",
                        id: intent.temp_rel.to_string(),
                    });
                }
            }
        }
        self.set_intent_state(intent.id, IntentState::Applied, None)
    }

    /// Applica l'effetto sul DB e conferma l'intento nella stessa transazione.
    /// Vedi [`Self::commit_many`].
    pub fn commit(&mut self, intent: &Intent, f: impl FnOnce(&Tx<'_>) -> Result<()>) -> Result<()> {
        self.commit_many(std::slice::from_ref(intent), f)
    }

    /// Applica l'effetto sul DB di più intenti `applied` e li conferma tutti
    /// nella stessa transazione SQL. Dopo `f` verifica che le righe di
    /// destinazione esistano (creazione) o non esistano più (cancellazione);
    /// altrimenti annulla la transazione e restituisce errore lasciando gli
    /// intenti `applied` (da annullare o riconciliare). Dopo la conferma
    /// rimuove i temporanei delle cancellazioni e degli export.
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
                    IntentKind::DeleteFile => !present || i.target_kind == TargetKind::None,
                    _ => true,
                };
                if !ok {
                    return Err(StorageError::StateConflict(format!(
                        "intento {}: effetto sul DB non coerente con {}",
                        i.id, i.kind
                    )));
                }
                set_state_tx(tx, i.id, IntentState::Committed, None)?;
            }
            Ok(())
        })?;
        for i in intents {
            if matches!(i.kind, IntentKind::DeleteFile | IntentKind::Export)
                && remove_temp(&self.layout, &i.temp_rel).is_ok()
            {
                self.set_resolution(i.id, "temp_removed")?;
            }
        }
        Ok(())
    }

    /// Annulla un intento `pending` o `applied`, ripristinando il filesystem.
    pub fn abort(&mut self, intent: &Intent) -> Result<()> {
        let state = self.expect_state(intent.id, &[IntentState::Pending, IntentState::Applied])?;
        let row = self.intent_row(intent.id)?;
        let resolution = self.rollback_fs(&row, state)?;
        self.set_intent_state(intent.id, IntentState::Aborted, Some(&resolution))
    }

    /// Stato corrente di un intento.
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

    /// Esito registrato dalla riconciliazione o dall'annullamento.
    pub fn intent_resolution(&self, id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT resolution FROM journal WHERE intent_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .db()
    }

    /// Intenti non conclusi (`pending`, `applied` o `needs_review`).
    pub fn open_intents(&self) -> Result<Vec<Intent>> {
        self.intent_ids_in("'pending','applied','needs_review'")?
            .into_iter()
            .map(|id| self.intent_row(id))
            .collect()
    }

    /// Intenti che la riconciliazione non è riuscita a risolvere ("da
    /// verificare"): la VM coinvolta resta occupata. Il dettaglio dell'ultimo
    /// tentativo è in [`Self::intent_resolution`].
    pub fn intents_needing_review(&self) -> Result<Vec<i64>> {
        self.intent_ids_in("'needs_review'")
    }

    /// Identificativi degli intenti negli stati indicati (letterali SQL fissi).
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

    /// Temporanei degli intenti non conclusi, da non toccare nella pulizia di
    /// `tmp/`. Letti come testo: un intento con percorsi non risolvibili (per
    /// esempio dietro un collegamento) non impedisce l'apertura del catalogo.
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
                "intento {id} in stato {s}"
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

    /// Annullamento sul filesystem (usato da `abort` e dalla riconciliazione).
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
                    // Interruzione prima della pubblicazione: la directory
                    // della nuova baseline (creata da `begin_intent`) e un
                    // eventuale manifest vanno rimossi.
                    remove_baseline_leftovers(&self.layout, rel)?;
                }
                if parts.is_empty() {
                    parts.push("nothing_to_undo");
                }
                Ok(parts.join("+"))
            }
            IntentKind::DeleteFile => {
                let res = if temp_exists && !final_exists {
                    let (Some(src), Some(rel)) = (&i.final_path, &i.final_rel) else {
                        return Err(StorageError::StateConflict("sorgente assente".into()));
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

    /// Riscrive il manifest della VM dal DB e ne registra l'hash.
    pub fn write_vm_manifest(&mut self, vm: &VmId) -> Result<String> {
        let m = self.read(|tx| build_vm_manifest(tx, vm))?;
        let sha = manifest::write_atomic(&self.layout, &CatalogLayout::vm_manifest(vm), &m)?;
        self.with_tx(|tx| tx.set_vm_manifest_hash(vm, &sha))?;
        Ok(sha)
    }

    /// Confronta il manifest della VM con il DB.
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

    /// Confronta il manifest di una baseline con il DB. Con `deep` ricalcola
    /// anche lo SHA-256 del disco base e delle vars (lettura completa).
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

fn set_state_tx(tx: &Tx<'_>, id: i64, state: IntentState, resolution: Option<&str>) -> Result<()> {
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
        TargetKind::None => return Ok(false),
    };
    tx.exists(sql, id)
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
                .map_err(|_| StorageError::Constraint("baseline_id non valido".into()))?;
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
        TargetKind::Artifact | TargetKind::None => {}
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

/// Rimuove un file o una directory sotto `tmp/`, senza seguire collegamenti.
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

/// Rimuove un file gestito regolare (mai una directory né un collegamento).
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

/// Dopo la rimozione di una base orfana: rimuove il manifest accanto e la
/// directory della baseline se vuota. Mai altro.
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
fn force_remove_file(p: &Path, meta: std::fs::Metadata) -> Result<()> {
    // Le basi e i media sono resi di sola lettura (mitigazione F0-02 §8.4):
    // l'attributo va tolto prima della rimozione di un file gestito orfano.
    let mut perm = meta.permissions();
    if perm.readonly() {
        perm.set_readonly(false);
        std::fs::set_permissions(p, perm).map_err(|e| StorageError::io(p, e))?;
    }
    std::fs::remove_file(p).map_err(|e| StorageError::io(p, e))
}

fn remove_dir_no_links(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| StorageError::io(dir, e))? {
        let entry = entry.map_err(|e| StorageError::io(dir, e))?;
        let p = entry.path();
        let m = std::fs::symlink_metadata(&p).map_err(|e| StorageError::io(&p, e))?;
        if is_link(&m) {
            // Rimuove il collegamento stesso, mai la destinazione
            // (`remove_dir` per junction e collegamenti a directory).
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
