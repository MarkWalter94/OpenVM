//! Istanza singola per catalogo (ROADMAP 6.1): lock esclusivo su un file nella
//! directory del catalogo. Il lock è del sistema operativo (`LockFileEx` su
//! Windows, `flock` su Unix, tramite `std::fs::File::try_lock`) e si rilascia
//! alla chiusura dell'handle, anche se il processo muore.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Nome del file di lock nella directory del catalogo.
pub const LOCK_FILE_NAME: &str = ".lightvm-service.lock";

/// Errore di acquisizione del lock.
#[derive(Debug)]
pub enum LockError {
    /// Un'altra istanza possiede già il catalogo.
    AlreadyRunning {
        /// File di lock.
        path: PathBuf,
    },
    /// Directory del catalogo non utilizzabile.
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { path } => write!(
                f,
                "un'altra istanza di lightvm-service sta già usando questo catalogo (lock: {})",
                path.display()
            ),
            Self::Io(e) => write!(f, "impossibile creare il lock del catalogo: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// Lock posseduto; rilasciato al drop o alla morte del processo.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Acquisisce il lock senza attendere.
    pub fn acquire(catalog_dir: &Path) -> Result<Self, LockError> {
        let meta = std::fs::symlink_metadata(catalog_dir).map_err(LockError::Io)?;
        if !meta.is_dir() {
            return Err(LockError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "il percorso del catalogo non è una directory",
            )));
        }
        let path = catalog_dir.join(LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(LockError::AlreadyRunning { path }),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }

    /// Percorso del file di lock.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_fails_and_release_on_drop() {
        let dir = crate::test_support::temp_dir("lock");
        let a = InstanceLock::acquire(&dir).unwrap();
        match InstanceLock::acquire(&dir) {
            Err(LockError::AlreadyRunning { .. }) => {}
            other => panic!("atteso AlreadyRunning: {other:?}"),
        }
        drop(a);
        InstanceLock::acquire(&dir).unwrap();
    }

    #[test]
    fn not_a_directory() {
        let dir = crate::test_support::temp_dir("lock-file");
        let f = dir.join("x");
        std::fs::write(&f, b"").unwrap();
        assert!(matches!(InstanceLock::acquire(&f), Err(LockError::Io(_))));
    }
}
