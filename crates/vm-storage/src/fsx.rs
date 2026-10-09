//! Aperture protette dei file gestiti, senza `unsafe` (solo API di `std`).
//!
//! - **Share mode** (Windows, `OpenOptionsExt::share_mode`): la baseline e il
//!   suo vars si aprono con il solo `FILE_SHARE_READ`; overlay e vars
//!   dell'esecuzione con `FILE_SHARE_READ | FILE_SHARE_WRITE`, senza
//!   `FILE_SHARE_DELETE` (ADR-0025, `F0-05` §7).
//! - **Reparse point** (Windows, `FILE_FLAG_OPEN_REPARSE_POINT`): l'apertura
//!   non segue collegamenti simbolici o junction sull'ultimo componente; il
//!   tipo è poi verificato sui metadati dell'handle aperto, quindi senza
//!   finestra tra controllo e uso per quel componente.
//! - **Nessuna sovrascrittura**: i file nuovi nascono con `CREATE_NEW`
//!   (`create_new`) e sono pubblicati sulla destinazione con un collegamento
//!   fisico (`hard_link`, che fallisce se la destinazione esiste) invece di
//!   `rename`, che su Windows sostituisce una destinazione esistente.
//!
//! Su sistemi non Windows le share mode non esistono: i file sono aperti senza
//! esclusione (limite dichiarato, verifica Linux rinviata a R2a con ADR-0016).

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{PathRejection, Result, StorageError};
use crate::paths::{ManagedPath, is_link};

/// `FILE_SHARE_READ`.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
/// `FILE_SHARE_WRITE`.
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// `FILE_FLAG_OPEN_REPARSE_POINT`.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `ERROR_SHARING_VIOLATION`.
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;
/// `ERROR_LOCK_VIOLATION`.
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Condivisione concessa agli altri processi da un handle di guardia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Share {
    /// Solo lettura (`FILE_SHARE_READ`): nessuno può scrivere, rinominare o cancellare.
    ReadOnly,
    /// Lettura e scrittura, senza cancellazione (`FILE_SHARE_READ | FILE_SHARE_WRITE`).
    NoDelete,
}

/// `true` se l'errore indica un file già aperto in modo incompatibile.
pub(crate) fn is_sharing_violation(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(
            e.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        )
    }
    #[cfg(not(windows))]
    {
        let _ = e;
        false
    }
}

/// Apre in lettura un file regolare con la condivisione indicata, senza
/// seguire un reparse point sull'ultimo componente. `label` è il nome usato
/// negli errori (percorso gestito o descrizione della sorgente).
pub(crate) fn open_regular(path: &Path, share: Share, label: &str) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(match share {
            Share::ReadOnly => FILE_SHARE_READ,
            Share::NoDelete => FILE_SHARE_READ | FILE_SHARE_WRITE,
        });
        o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(windows))]
    let _ = share;
    let f = o.open(path).map_err(|e| {
        if is_sharing_violation(&e) {
            StorageError::VmBusy(format!(
                "`{label}` è aperto da un altro processo in modo incompatibile (scrittore inatteso)"
            ))
        } else {
            StorageError::io(path, e)
        }
    })?;
    let meta = f.metadata().map_err(|e| StorageError::io(path, e))?;
    if is_link(&meta) || !meta.is_file() {
        return Err(StorageError::invalid_path(label, PathRejection::Link));
    }
    Ok(f)
}

/// Crea un file nuovo per la scrittura (`CREATE_NEW`): fallisce se esiste già
/// qualunque cosa, collegamenti compresi. Nessun altro processo può aprirlo
/// finché l'handle è aperto (Windows: share mode 0).
pub(crate) fn create_new_exclusive(path: &Path) -> Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(path).map_err(|e| StorageError::io(path, e))
}

/// SHA-256 esadecimale e numero di byte letti dall'handle, dall'inizio.
pub(crate) fn sha256_handle(mut f: &File, path: &Path) -> Result<(String, u64)> {
    use std::io::Seek;
    f.seek(io::SeekFrom::Start(0))
        .map_err(|e| StorageError::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| StorageError::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total = total.saturating_add(n as u64);
    }
    Ok((crate::hex(&h.finalize()), total))
}

/// Pubblica `temp` su `dest` senza mai sostituire una destinazione esistente.
///
/// Collegamento fisico `dest` → stesso file di `temp` (fallisce con
/// `AlreadyExists` se `dest` esiste), poi rimozione del nome temporaneo. Se il
/// volume non supporta i collegamenti fisici (FAT, exFAT) ripiega su controllo
/// di esistenza + `rename`, con la finestra tra controllo e uso dichiarata.
pub(crate) fn publish_no_replace(temp: &Path, dest: &Path, label: &ManagedPath) -> Result<()> {
    match std::fs::hard_link(temp, dest) {
        Ok(()) => std::fs::remove_file(temp).map_err(|e| StorageError::io(temp, e)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(StorageError::StateConflict(
            format!("destinazione già esistente, nessuna sovrascrittura: {label}"),
        )),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            if std::fs::symlink_metadata(dest).is_ok() {
                return Err(StorageError::StateConflict(format!(
                    "destinazione già esistente, nessuna sovrascrittura: {label}"
                )));
            }
            std::fs::rename(temp, dest).map_err(|e| StorageError::io(dest, e))
        }
        Err(e) => Err(StorageError::io(dest, e)),
    }
}

/// Imposta (o verifica) l'attributo di sola lettura. Restituisce `true` se
/// l'attributo mancava ed è stato ripristinato.
pub(crate) fn ensure_readonly(path: &Path) -> Result<bool> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| StorageError::io(path, e))?;
    let mut p = meta.permissions();
    if p.readonly() {
        return Ok(false);
    }
    p.set_readonly(true);
    std::fs::set_permissions(path, p).map_err(|e| StorageError::io(path, e))?;
    Ok(true)
}
