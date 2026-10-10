//! Single instance per catalog (ROADMAP 6.1): exclusive lock on a file in the
//! catalog directory. The lock belongs to the operating system (`LockFileEx` on
//! Windows, `flock` on Unix, through `std::fs::File::try_lock`) and is released
//! when the handle is closed, even if the process dies.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Name of the lock file in the catalog directory.
pub const LOCK_FILE_NAME: &str = ".lightvm-service.lock";

/// Lock acquisition error.
#[derive(Debug)]
pub enum LockError {
    /// Another instance already owns the catalog.
    AlreadyRunning {
        /// Lock file.
        path: PathBuf,
    },
    /// Catalog directory not usable.
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { path } => write!(
                f,
                "another instance of lightvm-service is already using this catalog (lock: {})",
                path.display()
            ),
            Self::Io(e) => write!(f, "cannot create the catalog lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// Held lock; released on drop or when the process dies.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Acquires the lock without waiting.
    pub fn acquire(catalog_dir: &Path) -> Result<Self, LockError> {
        let meta = std::fs::symlink_metadata(catalog_dir).map_err(LockError::Io)?;
        if !meta.is_dir() {
            return Err(LockError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the catalog path is not a directory",
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

    /// Path of the lock file.
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
            other => panic!("expected AlreadyRunning: {other:?}"),
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
