//! Protected opens of managed files, without `unsafe` (`std` APIs only).
//!
//! - **Share mode** (Windows, `OpenOptionsExt::share_mode`): the baseline and
//!   its vars are opened with `FILE_SHARE_READ` only; the run's overlay and
//!   vars with `FILE_SHARE_READ | FILE_SHARE_WRITE`, without
//!   `FILE_SHARE_DELETE` (ADR-0025, `F0-05` §7).
//! - **Reparse point** (Windows, `FILE_FLAG_OPEN_REPARSE_POINT`): the open
//!   does not follow symbolic links or junctions on the last component; the
//!   type is then checked on the open handle's metadata, so there is no
//!   window between check and use for that component.
//! - **No overwrite**: new files are created with `CREATE_NEW`
//!   (`create_new`) and published to the destination with a hard link
//!   (`hard_link`, which fails if the destination exists) instead of
//!   `rename`, which on Windows replaces an existing destination.
//!
//! On non-Windows systems share modes do not exist: files are opened without
//! exclusion (declared limit, Linux verification deferred to R2a with
//! ADR-0016).

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

/// Sharing granted to other processes by a guard handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Share {
    /// Read only (`FILE_SHARE_READ`): nobody can write, rename or delete.
    ReadOnly,
    /// Read and write, no delete (`FILE_SHARE_READ | FILE_SHARE_WRITE`).
    NoDelete,
}

/// `true` if the error indicates a file already open in an incompatible mode.
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

/// Opens a regular file for reading with the given sharing, without
/// following a reparse point on the last component. `label` is the name used
/// in errors (managed path or description of the source).
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
                "`{label}` is open by another process in an incompatible mode (unexpected writer)"
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

/// Creates a new file for writing (`CREATE_NEW`): fails if anything already
/// exists, links included. No other process can open it while the handle is
/// open (Windows: share mode 0).
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

/// Hex SHA-256 and number of bytes read from the handle, from the start.
pub(crate) fn sha256_handle(f: &File, path: &Path) -> Result<(String, u64)> {
    sha256_handle_with(f, path, &|| false, &|_| {})
}

/// Message of the error returned when a hash is cancelled.
pub const HASH_CANCELLED: &str = "verification cancelled";

/// [`sha256_handle`] polling `cancel` between 1 MiB blocks
/// ([`StorageError::StateConflict`] with [`HASH_CANCELLED`]) and reporting
/// the bytes read with `progress` (delta per block).
pub(crate) fn sha256_handle_with(
    mut f: &File,
    path: &Path,
    cancel: &dyn Fn() -> bool,
    progress: &dyn Fn(u64),
) -> Result<(String, u64)> {
    use std::io::Seek;
    f.seek(io::SeekFrom::Start(0))
        .map_err(|e| StorageError::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        if cancel() {
            return Err(StorageError::StateConflict(HASH_CANCELLED.into()));
        }
        let n = f.read(&mut buf).map_err(|e| StorageError::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total = total.saturating_add(n as u64);
        progress(n as u64);
    }
    Ok((crate::hex(&h.finalize()), total))
}

/// Publishes `temp` to `dest` without ever replacing an existing destination.
///
/// Hard link `dest` → same file as `temp` (fails with `AlreadyExists` if
/// `dest` exists), then removal of the temporary name. If the volume does not
/// support hard links (FAT, exFAT) it falls back to an existence check +
/// `rename`, with the window between check and use declared.
pub(crate) fn publish_no_replace(temp: &Path, dest: &Path, label: &ManagedPath) -> Result<()> {
    match std::fs::hard_link(temp, dest) {
        Ok(()) => std::fs::remove_file(temp).map_err(|e| StorageError::io(temp, e)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(StorageError::StateConflict(
            format!("destination already exists, no overwrite: {label}"),
        )),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => {
            if std::fs::symlink_metadata(dest).is_ok() {
                return Err(StorageError::StateConflict(format!(
                    "destination already exists, no overwrite: {label}"
                )));
            }
            std::fs::rename(temp, dest).map_err(|e| StorageError::io(dest, e))
        }
        Err(e) => Err(StorageError::io(dest, e)),
    }
}

/// Sets (or verifies) the read-only attribute. Returns `true` if the
/// attribute was missing and has been restored.
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
