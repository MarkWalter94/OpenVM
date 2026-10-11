//! On-disk layout of the catalog root and single-instance lock.
//!
//! ```text
//! <root>/
//!   catalog.sqlite3            database (WAL: also -wal and -shm)
//!   catalog.lock               exclusive instance lock (section 6.1)
//!   catalog.sqlite3.backup-v<N>-<UTC>   copies taken before migrations
//!   media/<media_id>.iso       managed copies of the media registry
//!   vms/<vm_id>/manifest.json  VM manifest
//!   vms/<vm_id>/primary.qcow2  working disk
//!   vms/<vm_id>/base/<baseline_id>/{disk.qcow2,vars.fd,manifest.json}
//!   vms/<vm_id>/overlays/<run_id>.qcow2      per-run overlay (prepare_run_from_baseline)
//!   vms/<vm_id>/overlays/<disk_id>.qcow2     overlay of the current state (F3 restore)
//!   vms/<vm_id>/firmware/{primary.vars.fd,<run_id>.vars.fd,<fw_id>.vars.fd}
//!   runs/<run_id>/{artifacts,logs}/
//!   tmp/                       journal temporary files
//! ```

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::{PathRejection, Result, StorageError};
use crate::ids::{BaselineId, DiskId, FirmwareId, MediaId, RunId, VmId};
use crate::paths::{ManagedPath, is_link, resolve_checked};
use crate::time::UtcMillis;

/// Name of the database file.
pub const DB_FILE: &str = "catalog.sqlite3";
/// Name of the lock file.
pub const LOCK_FILE: &str = "catalog.lock";
/// Directory for temporary files.
pub const TMP_DIR: &str = "tmp";

const TOP_DIRS: &[&str] = &["media", "vms", "runs", TMP_DIR];

/// Catalog root with the canonical paths.
#[derive(Debug, Clone)]
pub struct CatalogLayout {
    root: PathBuf,
}

impl CatalogLayout {
    /// Prepares the root (creating it if missing) and the top-level
    /// directories. Rejects a root or top-level directory that is a link.
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

    /// Absolute root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the database.
    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILE)
    }

    /// Resolves a managed path, checking that there are no links.
    pub fn resolve(&self, rel: &ManagedPath) -> Result<PathBuf> {
        resolve_checked(&self.root, rel)
    }

    /// Converts an absolute path (for example one returned by another
    /// component) into the corresponding managed path, rejecting: paths that
    /// are not absolute or lie outside the root
    /// ([`PathRejection::OutsideArea`]), `..`/`.` components or invalid names
    /// (rules of [`ManagedPath::parse`]) and existing components that are
    /// links or reparse points ([`PathRejection::Link`]).
    ///
    /// The comparison with the root is per component and case-sensitive: a
    /// different spelling of the root is rejected (over-rejection, never
    /// over-acceptance).
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

    /// Creates (if missing) a managed directory, component by component,
    /// rejecting existing links.
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
                        std::io::Error::new(std::io::ErrorKind::AlreadyExists, "not a directory"),
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
    /// `vms/<vm_id>/overlays/<disk_id>.qcow2`: overlay of the VM's current
    /// state after a restore (F3).
    pub fn state_overlay(vm: &VmId, disk: &DiskId) -> ManagedPath {
        mp(format!("vms/{vm}/overlays/{disk}.qcow2"))
    }
    /// `vms/<vm_id>/firmware/<fw_id>.vars.fd`: UEFI variables of the VM's
    /// current state after a restore (F3).
    pub fn state_vars(vm: &VmId, fw: &FirmwareId) -> ManagedPath {
        mp(format!("vms/{vm}/firmware/{fw}.vars.fd"))
    }
    /// Backing reference written in the header of a current-state overlay
    /// ([`Self::state_overlay`]): the base disk of the baseline, relative to
    /// the overlay's directory (`../base/<baseline_id>/disk.qcow2`). The only
    /// backing reference LightVM ever writes: one level, fixed shape.
    pub fn overlay_backing_ref(b: &BaselineId) -> String {
        format!("../base/{b}/disk.qcow2")
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

    /// Conservative cleanup of `tmp/`: removes only regular files directly
    /// inside `tmp/` with a modification time earlier than `started_at` and
    /// not in `keep`. Does not follow links, does not descend into
    /// subdirectories and touches nothing outside `tmp/`; skipped entries are
    /// reported.
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
    // Built only from constants and UUIDs: always valid.
    ManagedPath::parse(&s).unwrap_or_else(|e| unreachable!("invalid internal path: {e}"))
}

/// Result of the `tmp/` cleanup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TmpCleanup {
    /// Files removed.
    pub removed: Vec<String>,
    /// Recent files (not older than startup) kept.
    pub kept_recent: Vec<String>,
    /// Skipped entries: links, directories, invalid names, files of the open journal.
    pub skipped: Vec<String>,
}

/// Exclusive catalog lock (single instance, section 6.1).
///
/// Implemented with the standard library's `File::try_lock` (`LockFileEx` on
/// Windows, `flock` on Unix) on `catalog.lock`. The lock is advisory for other
/// processes that do not use it and does not protect against an administrator
/// or a process of the same user that ignores the protocol.
#[derive(Debug)]
pub struct CatalogLock {
    _file: File,
    path: PathBuf,
}

impl CatalogLock {
    /// Acquires the lock without waiting.
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

    /// Path of the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
