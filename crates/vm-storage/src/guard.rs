//! Protezione dei dischi durante un'esecuzione (ADR-0025, backlog F1-09).
//!
//! Il lock delle immagini di QEMU non protegge su Windows (`F0-02` §8.3,
//! `F0-05` §7.1). Il catalogo tiene quindi aperti, per tutta la durata
//! dell'esecuzione, handle di guardia indipendenti da QEMU:
//!
//! - **baseline** (disco base e vars della baseline): lettura con il solo
//!   `FILE_SHARE_READ`. Finché l'handle è aperto nessun altro processo può
//!   scriverli, rinominarli o cancellarli; QEMU li legge. Se l'apertura fallisce
//!   per violazione di condivisione, qualcuno li tiene già aperti in scrittura
//!   (**rilevatore di scrittori**): errore [`StorageError::VmBusy`], l'avvio va
//!   rifiutato. Dopo l'apertura lo SHA-256 è ricalcolato **leggendo attraverso
//!   lo stesso handle** e confrontato con il catalogo: tra verifica e avvio di
//!   QEMU il contenuto non può cambiare. L'attributo di sola lettura mancante
//!   viene ripristinato e riportato.
//! - **overlay e vars dell'esecuzione**: lettura con
//!   `FILE_SHARE_READ | FILE_SHARE_WRITE`, senza `FILE_SHARE_DELETE`: impedisce
//!   rinomina e cancellazione durante l'uso, lascia scrivere QEMU.
//!
//! **Limite dichiarato (decisione aperta, ADR-0025 punto 4):** la share mode è
//! una proprietà dell'apertura, non del processo; QEMU apre l'overlay con
//! `FILE_SHARE_WRITE`, quindi un altro processo dello stesso utente può
//! scrivere l'overlay in uso. La protezione aggiuntiva (directory per
//! esecuzione con ACL esplicita, identità di processo distinta per QEMU, oppure
//! limite dichiarato in via definitiva) è una scelta dell'utente non ancora
//! presa; questo modulo non la implementa.
//!
//! Altri limiti: la protezione cessa alla chiusura degli handle (fine
//! esecuzione o morte del supervisor); non protegge da un amministratore, da
//! accessi a livello di volume o da mappature in memoria; su sistemi non
//! Windows i file sono aperti senza esclusione.

use std::fs::File;

use crate::catalog::Catalog;
use crate::error::{Result, StorageError};
use crate::fsx::{self, Share};
use crate::ids::{BaselineId, RunId};
use crate::paths::ManagedPath;

/// File tenuto aperto da un handle di guardia.
#[derive(Debug)]
struct GuardedFile {
    path: ManagedPath,
    _file: File,
}

/// Esito delle verifiche eseguite all'acquisizione di una guardia.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardReport {
    /// File il cui SHA-256 è stato ricalcolato attraverso l'handle di guardia
    /// e trovato uguale al catalogo.
    pub hash_verified: Vec<ManagedPath>,
    /// File della baseline senza hash registrato (verifica non possibile).
    pub hash_not_recorded: Vec<ManagedPath>,
    /// File della baseline trovati senza attributo di sola lettura e corretti.
    pub readonly_restored: Vec<ManagedPath>,
}

/// Guardia sulla baseline: disco base e vars aperti con il solo
/// `FILE_SHARE_READ`. La protezione dura finché il valore è vivo.
#[derive(Debug)]
pub struct BaselineGuard {
    baseline_id: BaselineId,
    files: Vec<GuardedFile>,
    report: GuardReport,
}

impl BaselineGuard {
    /// Baseline protetta.
    pub fn baseline_id(&self) -> BaselineId {
        self.baseline_id
    }

    /// File protetti in sola lettura.
    pub fn paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.files.iter().map(|f| &f.path)
    }

    /// Verifiche eseguite all'acquisizione.
    pub fn report(&self) -> &GuardReport {
        &self.report
    }
}

/// Guardia di un'esecuzione: baseline in sola lettura più overlay e vars
/// dell'esecuzione protetti da rinomina e cancellazione.
#[derive(Debug)]
pub struct RunGuard {
    run_id: RunId,
    baseline: Option<BaselineGuard>,
    no_delete: Vec<GuardedFile>,
}

impl RunGuard {
    /// Esecuzione protetta.
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    /// Guardia della baseline di partenza, se l'esecuzione ne ha una.
    pub fn baseline(&self) -> Option<&BaselineGuard> {
        self.baseline.as_ref()
    }

    /// File protetti soltanto da rinomina e cancellazione (overlay, vars
    /// dell'esecuzione). Restano scrivibili da altri processi dello stesso
    /// utente (limite dichiarato).
    pub fn no_delete_paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.no_delete.iter().map(|f| &f.path)
    }
}

/// Esito del rilevatore di scrittori su una baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterProbe {
    /// Nessun handle incompatibile: l'apertura esclusiva è riuscita.
    Clear,
    /// Almeno un file della baseline è aperto da un altro processo in modo
    /// incompatibile con la sola lettura condivisa (tipicamente in scrittura).
    Busy(Vec<ManagedPath>),
}

impl Catalog {
    /// Apre disco base e vars della baseline con il solo `FILE_SHARE_READ`,
    /// ricalcola lo SHA-256 attraverso gli handle e lo confronta con il
    /// catalogo, ripristina l'attributo di sola lettura se manca.
    ///
    /// Errori: [`StorageError::VmBusy`] se un altro processo tiene un file
    /// aperto in modo incompatibile (rilevatore di scrittori);
    /// [`StorageError::IntegrityMismatch`] se il contenuto non corrisponde;
    /// [`StorageError::InvalidPath`] se un file è stato sostituito da un
    /// collegamento.
    pub fn guard_baseline(&mut self, id: &BaselineId) -> Result<BaselineGuard> {
        self.baseline_guard(id, true)
    }

    /// Rilevatore di scrittori: prova l'apertura esclusiva della baseline e la
    /// rilascia subito. Non verifica gli hash.
    pub fn probe_baseline_writers(&mut self, id: &BaselineId) -> Result<WriterProbe> {
        let files = self.baseline_files(id)?;
        let mut busy = Vec::new();
        for (rel, _) in files {
            let abs = self.layout.resolve(&rel)?;
            match fsx::open_regular(&abs, Share::ReadOnly, rel.as_str()) {
                Ok(_) => {}
                Err(StorageError::VmBusy(_)) => busy.push(rel),
                Err(e) => return Err(e),
            }
        }
        Ok(if busy.is_empty() {
            WriterProbe::Clear
        } else {
            WriterProbe::Busy(busy)
        })
    }

    /// Guardia per l'avvio di un'esecuzione già preparata: da chiamare prima di
    /// avviare QEMU e da tenere viva fino alla fine del processo. Verifica
    /// l'hash della baseline attraverso gli handle (ogni avvio) e apre overlay
    /// e vars dell'esecuzione senza `FILE_SHARE_DELETE`.
    pub fn guard_run(&mut self, run_id: &RunId) -> Result<RunGuard> {
        let (run, overlay, vars) = self.read(|tx| {
            let run = tx.run(run_id)?;
            let overlay = run
                .overlay_disk_id
                .as_ref()
                .map(|d| tx.disk(d))
                .transpose()?;
            let vars = run
                .vars_fw_id
                .as_ref()
                .map(|f| tx.firmware(f))
                .transpose()?;
            Ok((run, overlay, vars))
        })?;
        if run.ended_at.is_some() {
            return Err(StorageError::StateConflict(format!(
                "esecuzione {run_id} già conclusa"
            )));
        }
        let baseline = match &run.baseline_id {
            Some(b) => Some(self.baseline_guard(b, true)?),
            None => None,
        };
        let mut no_delete = Vec::new();
        let paths = overlay
            .map(|d| d.managed_path)
            .into_iter()
            .chain(vars.map(|f| f.managed_path));
        for rel in paths {
            let abs = self.layout.resolve(&rel)?;
            let file = fsx::open_regular(&abs, Share::NoDelete, rel.as_str())?;
            no_delete.push(GuardedFile {
                path: rel,
                _file: file,
            });
        }
        Ok(RunGuard {
            run_id: *run_id,
            baseline,
            no_delete,
        })
    }

    /// Guardia della baseline con verifica facoltativa degli hash (la
    /// preparazione di un'esecuzione usa solo il rilevatore di scrittori; la
    /// verifica completa avviene in [`Self::guard_run`], prima dell'avvio).
    pub(crate) fn baseline_guard(
        &mut self,
        id: &BaselineId,
        verify: bool,
    ) -> Result<BaselineGuard> {
        let files = self.baseline_files(id)?;
        let mut guard = BaselineGuard {
            baseline_id: *id,
            files: Vec::new(),
            report: GuardReport::default(),
        };
        for (rel, sha) in files {
            let abs = self.layout.resolve(&rel)?;
            let file = fsx::open_regular(&abs, Share::ReadOnly, rel.as_str())?;
            if verify {
                match sha {
                    Some(expected) => {
                        let (actual, _) = fsx::sha256_handle(&file, &abs)?;
                        if actual != expected {
                            return Err(StorageError::IntegrityMismatch {
                                path: rel.to_string(),
                                detail: format!("SHA-256 atteso {expected}, trovato {actual}"),
                            });
                        }
                        guard.report.hash_verified.push(rel.clone());
                    }
                    None => guard.report.hash_not_recorded.push(rel.clone()),
                }
            }
            if fsx::ensure_readonly(&abs)? {
                guard.report.readonly_restored.push(rel.clone());
            }
            guard.files.push(GuardedFile {
                path: rel,
                _file: file,
            });
        }
        Ok(guard)
    }

    /// Disco base e vars di una baseline con l'hash registrato.
    fn baseline_files(&mut self, id: &BaselineId) -> Result<Vec<(ManagedPath, Option<String>)>> {
        self.read(|tx| {
            let b = tx.baseline(id)?;
            let disk = tx.disk(&b.base_disk_id)?;
            let mut out = vec![(disk.managed_path, disk.sha256)];
            if let Some(f) = &b.vars_fw_id {
                let fw = tx.firmware(f)?;
                out.push((fw.managed_path, fw.sha256));
            }
            Ok(out)
        })
    }
}
