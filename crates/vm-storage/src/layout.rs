//! Layout su disco della radice del catalogo e lock di istanza singola.
//!
//! ```text
//! <radice>/
//!   catalog.sqlite3            database (WAL: anche -wal e -shm)
//!   catalog.lock               lock esclusivo dell'istanza (sezione 6.1)
//!   catalog.sqlite3.backup-v<N>-<UTC>   copie prima delle migrazioni
//!   media/<media_id>.iso       copie gestite del registro media
//!   vms/<vm_id>/manifest.json  manifest della VM
//!   vms/<vm_id>/primary.qcow2  disco di lavoro
//!   vms/<vm_id>/base/<baseline_id>/{disk.qcow2,vars.fd,manifest.json}
//!   vms/<vm_id>/overlays/<run_id>.qcow2
//!   vms/<vm_id>/firmware/{primary.vars.fd,<run_id>.vars.fd}
//!   runs/<run_id>/{artifacts,logs}/
//!   tmp/                       temporanei del journal
//! ```

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::{PathRejection, Result, StorageError};
use crate::ids::{BaselineId, MediaId, RunId, VmId};
use crate::paths::{ManagedPath, is_link, resolve_checked};
use crate::time::UtcMillis;

/// Nome del file di database.
pub const DB_FILE: &str = "catalog.sqlite3";
/// Nome del file di lock.
pub const LOCK_FILE: &str = "catalog.lock";
/// Directory dei temporanei.
pub const TMP_DIR: &str = "tmp";

const TOP_DIRS: &[&str] = &["media", "vms", "runs", TMP_DIR];

/// Radice del catalogo con i percorsi canonici.
#[derive(Debug, Clone)]
pub struct CatalogLayout {
    root: PathBuf,
}

impl CatalogLayout {
    /// Prepara la radice (creandola se manca) e le directory di primo livello.
    /// Rifiuta una radice o directory di primo livello che siano collegamenti.
    pub fn prepare(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root).map_err(|e| StorageError::io(root, e))?;
        let meta = std::fs::symlink_metadata(root).map_err(|e| StorageError::io(root, e))?;
        if is_link(&meta) {
            return Err(StorageError::invalid_path(
                root.display().to_string(),
                PathRejection::Link,
            ));
        }
        let layout = Self {
            root: root.to_path_buf(),
        };
        for d in TOP_DIRS {
            layout.ensure_dir(&ManagedPath::parse(d)?)?;
        }
        Ok(layout)
    }

    /// Radice assoluta.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Percorso del database.
    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILE)
    }

    /// Risolve un percorso gestito verificando l'assenza di collegamenti.
    pub fn resolve(&self, rel: &ManagedPath) -> Result<PathBuf> {
        resolve_checked(&self.root, rel)
    }

    /// Converte un percorso assoluto (per esempio restituito da un altro
    /// componente) nel percorso gestito corrispondente, rifiutando:
    /// percorsi non assoluti o fuori dalla radice
    /// ([`PathRejection::OutsideArea`]), componenti `..`/`.` o nomi non validi
    /// (regole di [`ManagedPath::parse`]) e componenti esistenti che siano
    /// collegamenti o reparse point ([`PathRejection::Link`]).
    ///
    /// Il confronto con la radice è per componenti e sensibile alle maiuscole:
    /// una grafia diversa della radice è rifiutata (rifiuto in eccesso, mai
    /// un'accettazione in eccesso).
    pub fn managed_from_absolute(&self, abs: &Path) -> Result<ManagedPath> {
        let label = abs.display().to_string();
        let outside = || StorageError::invalid_path(label.clone(), PathRejection::OutsideArea);
        if !abs.is_absolute() {
            return Err(outside());
        }
        let rest = abs.strip_prefix(&self.root).map_err(|_| outside())?;
        let mut parts = Vec::new();
        for c in rest.components() {
            match c {
                std::path::Component::Normal(s) => {
                    let s = s.to_str().ok_or_else(|| {
                        StorageError::invalid_path(label.clone(), PathRejection::InvalidCharacter)
                    })?;
                    parts.push(s);
                }
                std::path::Component::ParentDir | std::path::Component::CurDir => {
                    return Err(StorageError::invalid_path(
                        label.clone(),
                        PathRejection::Traversal,
                    ));
                }
                _ => return Err(outside()),
            }
        }
        if parts.is_empty() {
            return Err(StorageError::invalid_path(label, PathRejection::Empty));
        }
        let rel = ManagedPath::parse(&parts.join("/"))?;
        self.resolve(&rel)?;
        Ok(rel)
    }

    /// Crea (se manca) una directory gestita, componente per componente,
    /// rifiutando collegamenti esistenti.
    pub fn ensure_dir(&self, rel: &ManagedPath) -> Result<PathBuf> {
        let mut cur = self.root.clone();
        let mut acc = String::new();
        for comp in rel.components() {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(comp);
            cur.push(comp);
            match std::fs::symlink_metadata(&cur) {
                Ok(meta) if is_link(&meta) => {
                    return Err(StorageError::invalid_path(acc, PathRejection::Link));
                }
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(StorageError::io(
                        &cur,
                        std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "non è una directory",
                        ),
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&cur).map_err(|e| StorageError::io(&cur, e))?;
                }
                Err(e) => return Err(StorageError::io(&cur, e)),
            }
        }
        Ok(cur)
    }

    /// `vms/<vm_id>`.
    pub fn vm_dir(vm: &VmId) -> ManagedPath {
        mp(format!("vms/{vm}"))
    }
    /// `vms/<vm_id>/manifest.json`.
    pub fn vm_manifest(vm: &VmId) -> ManagedPath {
        mp(format!("vms/{vm}/manifest.json"))
    }
    /// `vms/<vm_id>/primary.qcow2`.
    pub fn primary_disk(vm: &VmId) -> ManagedPath {
        mp(format!("vms/{vm}/primary.qcow2"))
    }
    /// `vms/<vm_id>/base/<baseline_id>`.
    pub fn baseline_dir(vm: &VmId, b: &BaselineId) -> ManagedPath {
        mp(format!("vms/{vm}/base/{b}"))
    }
    /// `vms/<vm_id>/base/<baseline_id>/disk.qcow2`.
    pub fn baseline_disk(vm: &VmId, b: &BaselineId) -> ManagedPath {
        mp(format!("vms/{vm}/base/{b}/disk.qcow2"))
    }
    /// `vms/<vm_id>/base/<baseline_id>/vars.fd`.
    pub fn baseline_vars(vm: &VmId, b: &BaselineId) -> ManagedPath {
        mp(format!("vms/{vm}/base/{b}/vars.fd"))
    }
    /// `vms/<vm_id>/base/<baseline_id>/manifest.json`.
    pub fn baseline_manifest(vm: &VmId, b: &BaselineId) -> ManagedPath {
        mp(format!("vms/{vm}/base/{b}/manifest.json"))
    }
    /// `vms/<vm_id>/overlays/<run_id>.qcow2`.
    pub fn overlay(vm: &VmId, run: &RunId) -> ManagedPath {
        mp(format!("vms/{vm}/overlays/{run}.qcow2"))
    }
    /// `vms/<vm_id>/firmware/<run_id>.vars.fd`.
    pub fn run_vars(vm: &VmId, run: &RunId) -> ManagedPath {
        mp(format!("vms/{vm}/firmware/{run}.vars.fd"))
    }
    /// `vms/<vm_id>/firmware/primary.vars.fd`.
    pub fn primary_vars(vm: &VmId) -> ManagedPath {
        mp(format!("vms/{vm}/firmware/primary.vars.fd"))
    }
    /// `runs/<run_id>/artifacts`.
    pub fn run_artifacts(run: &RunId) -> ManagedPath {
        mp(format!("runs/{run}/artifacts"))
    }
    /// `runs/<run_id>/logs`.
    pub fn run_logs(run: &RunId) -> ManagedPath {
        mp(format!("runs/{run}/logs"))
    }
    /// `media/<media_id>.<ext>`.
    pub fn media_file(media: &MediaId, ext: &str) -> Result<ManagedPath> {
        ManagedPath::parse(&format!("media/{media}.{ext}"))
    }

    /// Pulizia prudente di `tmp/`: rimuove soltanto file regolari direttamente
    /// dentro `tmp/` con data di modifica anteriore a `started_at` e non in
    /// `keep`. Non segue collegamenti, non scende nelle sottodirectory e non
    /// tocca nulla fuori da `tmp/`; gli elementi saltati sono riportati.
    pub fn clean_tmp(&self, started_at: UtcMillis, keep: &[ManagedPath]) -> Result<TmpCleanup> {
        let tmp = self.resolve(&ManagedPath::parse(TMP_DIR)?)?;
        let mut report = TmpCleanup::default();
        let entries = std::fs::read_dir(&tmp).map_err(|e| StorageError::io(&tmp, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| StorageError::io(&tmp, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = match ManagedPath::parse(&format!("{TMP_DIR}/{name}")) {
                Ok(p) => p,
                Err(_) => {
                    report.skipped.push(name);
                    continue;
                }
            };
            let meta = match std::fs::symlink_metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => {
                    report.skipped.push(name);
                    continue;
                }
            };
            if is_link(&meta) || !meta.is_file() || keep.contains(&rel) {
                report.skipped.push(name);
                continue;
            }
            let mtime = meta
                .modified()
                .map(UtcMillis::from_system_time)
                .unwrap_or(started_at);
            if mtime >= started_at {
                report.kept_recent.push(name);
                continue;
            }
            std::fs::remove_file(entry.path()).map_err(|e| StorageError::io(entry.path(), e))?;
            report.removed.push(name);
        }
        Ok(report)
    }
}

fn mp(s: String) -> ManagedPath {
    // Composti solo da costanti e UUID: sempre validi.
    ManagedPath::parse(&s).unwrap_or_else(|e| unreachable!("percorso interno non valido: {e}"))
}

/// Esito della pulizia di `tmp/`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TmpCleanup {
    /// File rimossi.
    pub removed: Vec<String>,
    /// File recenti (non anteriori all'avvio) conservati.
    pub kept_recent: Vec<String>,
    /// Elementi saltati: collegamenti, directory, nomi non validi, file del journal aperto.
    pub skipped: Vec<String>,
}

/// Lock esclusivo del catalogo (istanza singola, sezione 6.1).
///
/// Implementato con `File::try_lock` della libreria standard (`LockFileEx` su
/// Windows, `flock` su Unix) su `catalog.lock`. Il lock è consultivo per gli
/// altri processi che non lo usano e non protegge da un amministratore o da un
/// processo dello stesso utente che ignori il protocollo.
#[derive(Debug)]
pub struct CatalogLock {
    _file: File,
    path: PathBuf,
}

impl CatalogLock {
    /// Acquisisce il lock senza attendere.
    pub fn acquire(root: &Path) -> Result<Self> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| StorageError::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(std::fs::TryLockError::WouldBlock) => Err(StorageError::CatalogLocked(path)),
            Err(std::fs::TryLockError::Error(e)) => Err(StorageError::io(&path, e)),
        }
    }

    /// Percorso del file di lock.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
