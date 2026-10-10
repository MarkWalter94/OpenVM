//! Exclusion of the VM files independent of the QEMU lock.
//!
//! On Windows the image lock of QEMU 11.1.0 does not protect: QEMU opens
//! base and overlay with `FILE_SHARE_WRITE` (F0-02 8.3, F0-05 7.1). The backend
//! therefore keeps open for the whole run:
//!
//! - the files to protect read-only (overlay base, baseline UEFI
//!   variables) with read access and share mode `FILE_SHARE_READ`:
//!   no other process can open them for writing, rename them or
//!   delete them; if another process already has them open for writing,
//!   the open fails (error 32) and the launch is refused with `vm_busy`
//!   (F0-05 7.2, 7.4);
//! - the overlay and the run's variables with share
//!   `FILE_SHARE_READ | FILE_SHARE_WRITE` and without `FILE_SHARE_DELETE`: this prevents
//!   rename and deletion during use. It does not prevent writes by a
//!   third process of the same user (F0-05 7.3, declared limit).
//!
//! Limits: protection against mistakes and concurrent tools, not a security
//! boundary; it ends when the handles are closed (end of run or death of the
//! supervisor, in which case the Job Object also terminates QEMU).
//! On non-Windows systems the files are opened without exclusion (TODO(F1/R2a):
//! advisory lock and permissions, to be verified on Linux).

use std::fs::{File, OpenOptions};

use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;

/// Handles kept open for the duration of a run.
#[derive(Debug, Default)]
pub struct StorageGuards {
    readonly: Vec<(ManagedPath, File)>,
    no_delete: Vec<(ManagedPath, File)>,
}

impl StorageGuards {
    /// Opens the files to protect. On error no handle stays open.
    pub fn acquire(readonly: &[ManagedPath], no_delete: &[ManagedPath]) -> Result<Self> {
        let mut g = Self::default();
        for p in readonly {
            g.readonly.push((p.clone(), open_readonly_exclusive(p)?));
        }
        for p in no_delete {
            g.no_delete.push((p.clone(), open_no_delete(p)?));
        }
        Ok(g)
    }

    /// Files protected read-only.
    pub fn readonly_paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.readonly.iter().map(|(p, _)| p)
    }

    /// Files protected from rename/deletion.
    pub fn no_delete_paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.no_delete.iter().map(|(p, _)| p)
    }
}

fn map_open_error(p: &ManagedPath, e: std::io::Error) -> BackendError {
    #[cfg(windows)]
    if e.raw_os_error() == Some(crate::sys::windows::ERROR_SHARING_VIOLATION) {
        return BackendError::VmBusy(format!(
            "`{}` is open by another process in an incompatible mode",
            p.as_path().display()
        ));
    }
    BackendError::Io(std::io::Error::new(
        e.kind(),
        format!("opening `{}`: {e}", p.as_path().display()),
    ))
}

/// Opens for reading with share mode `FILE_SHARE_READ`.
pub fn open_readonly_exclusive(p: &ManagedPath) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(crate::sys::windows::FILE_SHARE_READ);
    }
    o.open(p.as_path()).map_err(|e| map_open_error(p, e))
}

/// Opens for reading with share mode `FILE_SHARE_READ | FILE_SHARE_WRITE`
/// (without `FILE_SHARE_DELETE`).
pub fn open_no_delete(p: &ManagedPath) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(crate::sys::windows::FILE_SHARE_READ | crate::sys::windows::FILE_SHARE_WRITE);
    }
    o.open(p.as_path()).map_err(|e| map_open_error(p, e))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn tmp(name: &str) -> ManagedPath {
        let dir = std::env::temp_dir().join("lightvm-qemu-backend-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = ManagedPath::verify(dir.join(name)).unwrap();
        std::fs::write(p.as_path(), b"base").unwrap();
        p
    }

    #[test]
    fn base_protected_from_write_and_delete() {
        let p = tmp("guard-base.bin");
        let g = StorageGuards::acquire(std::slice::from_ref(&p), &[]).unwrap();
        // reading allowed
        assert_eq!(std::fs::read(p.as_path()).unwrap(), b"base");
        // write, delete and rename refused
        assert!(OpenOptions::new().write(true).open(p.as_path()).is_err());
        assert!(std::fs::remove_file(p.as_path()).is_err());
        assert!(std::fs::rename(p.as_path(), p.as_path().with_extension("x")).is_err());
        drop(g);
        std::fs::remove_file(p.as_path()).unwrap();
    }

    #[test]
    fn pre_existing_writer_detected() {
        let p = tmp("guard-writer.bin");
        let w = OpenOptions::new().write(true).open(p.as_path()).unwrap();
        let r = StorageGuards::acquire(std::slice::from_ref(&p), &[]);
        assert!(matches!(r, Err(BackendError::VmBusy(_))), "{r:?}");
        drop(w);
        std::fs::remove_file(p.as_path()).unwrap();
    }

    #[test]
    fn overlay_not_deletable_but_writable() {
        let p = tmp("guard-overlay.bin");
        let g = StorageGuards::acquire(&[], std::slice::from_ref(&p)).unwrap();
        assert!(OpenOptions::new().write(true).open(p.as_path()).is_ok());
        assert!(std::fs::remove_file(p.as_path()).is_err());
        drop(g);
        std::fs::remove_file(p.as_path()).unwrap();
    }
}
