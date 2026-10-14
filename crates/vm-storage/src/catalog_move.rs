//! Move of a whole catalog to another folder (Settings → Move catalog…).
//!
//! [`move_catalog`] moves the root with everything it holds (database,
//! media, VM disks, baselines, overlays, firmware and TPM state, runs,
//! session reports) and leaves the source untouched until the destination
//! is complete, verified and opened:
//!
//! 1. checks: the source is a catalog root (database present, not a link);
//!    the destination is absolute, missing (with an existing parent) or an
//!    empty folder, neither inside the source nor containing it (compared
//!    after resolving links, without regard to case: over-rejection, never
//!    over-acceptance);
//! 2. the source is opened as at a supervisor start (lock, migrations,
//!    reconciliation): no journal intent may stay open or need review and no
//!    run may lack an end (every VM stopped). The WAL is checkpointed into
//!    the database file and the connection closed; the lock stays held;
//! 3. inventory: every directory and regular file; links, reparse points and
//!    special files are refused as everywhere in the catalog; nothing is
//!    skipped except the top-level entries the caller names (runtime files of
//!    the supervisor), which are reported;
//! 4. fast path, when the source is not kept: `rename` of the root, atomic on
//!    one volume. Across volumes it fails without changing anything and the
//!    copy follows;
//! 5. copy, after checking the free space at the destination: each file
//!    opened without following links and with `FILE_SHARE_READ` only (no
//!    writer during the copy), created with `CREATE_NEW`, hashed while
//!    copied and `fsync`ed; read-only attribute (Unix: mode) carried over;
//! 6. verification: every copied file read back, size and SHA-256 compared
//!    with the source;
//! 7. validation: the destination is opened as a catalog, `PRAGMA
//!    quick_check` passes and it holds the same VMs, media and baselines;
//! 8. only then the source is removed (or kept, on request).
//!
//! Any failure or cancellation before step 8 removes what was created at the
//! destination (an empty destination folder that already existed stays) and
//! leaves the source as it was. A failure while removing the source after a
//! successful move is reported, not undone: the new catalog is complete.
//!
//! Declared limits: free space is read once, before the copy; the read-back
//! of step 6 may be served by the operating system cache.

use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, force_remove_file, remove_dir_no_links};
use crate::error::{PathRejection, Result, SqlResultExt, StorageError};
use crate::fsx::{self, Share};
use crate::layout::{CatalogLock, DB_FILE, LOCK_FILE};
use crate::paths::is_link;

/// Space that must stay free at the destination after the copy.
pub const MOVE_FREE_MARGIN: u64 = 1 << 30;

/// Message of the error returned when the move is cancelled
/// ([`StorageError::StateConflict`]).
pub const MOVE_CANCELLED: &str = "catalog move cancelled";

/// Block size of copy and verification; cancellation is polled between
/// blocks.
const BLOCK: usize = 1 << 20;

/// Step of the move, reported with the progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePhase {
    /// Checks of source and destination, opening of the source.
    Checking,
    /// Copy of the files.
    Copying,
    /// Read-back of the copied files.
    Verifying,
    /// Opening of the destination as a catalog.
    Validating,
    /// Removal of the source.
    RemovingSource,
}

impl MovePhase {
    /// Stable name (`checking`, `copying`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::Copying => "copying",
            Self::Verifying => "verifying",
            Self::Validating => "validating",
            Self::RemovingSource => "removing_source",
        }
    }

    /// Inverse of [`Self::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Checking,
            Self::Copying,
            Self::Verifying,
            Self::Validating,
            Self::RemovingSource,
        ]
        .into_iter()
        .find(|p| p.as_str() == s)
    }
}

/// Progress: bytes done out of the total of the current phase (both zero
/// in the phases that are not measured in bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveProgress {
    /// Current phase.
    pub phase: MovePhase,
    /// Bytes done in this phase.
    pub done_bytes: u64,
    /// Bytes of the phase.
    pub total_bytes: u64,
}

/// Fault injected in a move (tests only; never used by the product).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveFault {
    /// I/O error after this many files have been copied.
    FailAfterFiles(usize),
    /// The copy of the file with this index (0-based, files only) gets one
    /// byte changed before the verification.
    AlterCopy(usize),
    /// No fault, but the rename fast path is not tried (like every other
    /// fault).
    NoRename,
}

/// Move request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveRequest {
    /// Catalog root to move.
    pub from: PathBuf,
    /// New root: missing, or an empty folder.
    pub to: PathBuf,
    /// Keep the source after a successful move (always copies).
    pub keep_source: bool,
    /// Top-level entries of the source that are not part of the catalog
    /// and are not copied (runtime files of the supervisor). They are
    /// reported in [`MoveReport::left_behind`] and removed with the source.
    pub leave_behind: Vec<String>,
    /// Injected fault (tests only).
    #[doc(hidden)]
    pub fault: Option<MoveFault>,
}

/// How the catalog reached the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMethod {
    /// Rename of the root on the same volume.
    Renamed,
    /// Copy, verification and (unless kept) removal of the source.
    Copied,
}

/// Outcome of a successful move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReport {
    /// Rename or copy.
    pub method: MoveMethod,
    /// Regular files of the catalog.
    pub files: u64,
    /// Directories of the catalog (root excluded).
    pub dirs: u64,
    /// Total size of the files.
    pub bytes: u64,
    /// VMs in the moved catalog.
    pub vms: u64,
    /// Media in the moved catalog.
    pub media: u64,
    /// Baselines in the moved catalog.
    pub baselines: u64,
    /// Entries named in [`MoveRequest::leave_behind`] found in the source
    /// and not copied.
    pub left_behind: Vec<String>,
    /// The source no longer exists.
    pub source_removed: bool,
    /// Removal of the source failed partway (the move itself succeeded):
    /// what is left of the source can be deleted by hand.
    pub source_cleanup_error: Option<String>,
}

/// `true` if `e` is the error of a cancelled move.
pub fn is_move_cancelled(e: &StorageError) -> bool {
    matches!(e, StorageError::StateConflict(m) if m == MOVE_CANCELLED)
}

fn cancelled() -> StorageError {
    StorageError::StateConflict(MOVE_CANCELLED.into())
}

/// Content counted before the move and checked on the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    vms: u64,
    media: u64,
    baselines: u64,
}

fn counts(cat: &Catalog) -> Result<Counts> {
    let n = |sql: &str| -> Result<u64> {
        let v: i64 = cat.conn.query_row(sql, [], |r| r.get(0)).db()?;
        Ok(u64::try_from(v).unwrap_or(0))
    };
    Ok(Counts {
        vms: n("SELECT count(*) FROM vms")?,
        media: n("SELECT count(*) FROM media")?,
        baselines: n("SELECT count(*) FROM baselines")?,
    })
}

/// Entry of the inventory, relative to the root.
#[derive(Debug)]
struct Entry {
    rel: PathBuf,
    /// `Some(size)` for a file, `None` for a directory.
    len: Option<u64>,
    perm: std::fs::Permissions,
}

/// Inventory of the source.
#[derive(Debug, Default)]
struct Plan {
    entries: Vec<Entry>,
    left_behind: Vec<String>,
    files: u64,
    dirs: u64,
    bytes: u64,
}

/// Moves the catalog `req.from` to `req.to` (module documentation).
/// `free_bytes` returns the space available on the volume of a folder;
/// `progress` receives the progress; `cancel` is polled between blocks of
/// 1 MiB and between steps.
pub fn move_catalog(
    req: &MoveRequest,
    free_bytes: &dyn Fn(&Path) -> io::Result<u64>,
    progress: &dyn Fn(MoveProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<MoveReport> {
    let (from, to) = (req.from.as_path(), req.to.as_path());
    progress(MoveProgress {
        phase: MovePhase::Checking,
        done_bytes: 0,
        total_bytes: 0,
    });
    check_source(from)?;
    let to_existed = check_destination(from, to)?;

    let cat = Catalog::open(from)?;
    let open = cat.open_intents()?.len();
    if open > 0 {
        return Err(StorageError::StateConflict(format!(
            "{open} catalog operations are unfinished or need review: start LightVM on this catalog and resolve them before moving it"
        )));
    }
    let running: i64 = cat
        .conn
        .query_row(
            "SELECT count(*) FROM runs WHERE ended_at IS NULL",
            [],
            |r| r.get(0),
        )
        .db()?;
    if running > 0 {
        return Err(StorageError::StateConflict(format!(
            "{running} VM runs have not ended: stop every VM (start LightVM on this catalog once if none is running) before moving the catalog"
        )));
    }
    let before = counts(&cat)?;
    let lock = cat.close_keep_lock()?;
    let plan = inventory(from, &req.leave_behind)?;
    if cancel() {
        return Err(cancelled());
    }

    // Every injected fault concerns the copy: none of them renames.
    if !req.keep_source && req.fault.is_none() {
        // The lock file moves with the folder: on Windows a directory with
        // an open file cannot be renamed.
        drop(lock);
        match rename_root(from, to, to_existed) {
            Ok(()) => {
                progress(MoveProgress {
                    phase: MovePhase::Validating,
                    done_bytes: 0,
                    total_bytes: 0,
                });
                if let Err(e) = validate(to, before) {
                    return Err(undo_rename(from, to, to_existed, e));
                }
                return Ok(report(
                    MoveMethod::Renamed,
                    &plan,
                    before,
                    Vec::new(),
                    true,
                    None,
                ));
            }
            // Different volume (or rename not possible): nothing changed,
            // the copy follows under the lock taken again.
            Err(_) => {
                return copy_move(
                    req,
                    plan,
                    before,
                    CatalogLock::acquire(from)?,
                    to_existed,
                    free_bytes,
                    progress,
                    cancel,
                );
            }
        }
    }
    copy_move(
        req, plan, before, lock, to_existed, free_bytes, progress, cancel,
    )
}

#[allow(clippy::too_many_arguments, reason = "internal step of move_catalog")]
fn copy_move(
    req: &MoveRequest,
    plan: Plan,
    before: Counts,
    lock: CatalogLock,
    to_existed: bool,
    free_bytes: &dyn Fn(&Path) -> io::Result<u64>,
    progress: &dyn Fn(MoveProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<MoveReport> {
    let (from, to) = (req.from.as_path(), req.to.as_path());
    let volume = if to_existed {
        to
    } else {
        to.parent().unwrap_or(to)
    };
    let free = free_bytes(volume).map_err(|e| StorageError::io(volume, e))?;
    if free < plan.bytes.saturating_add(MOVE_FREE_MARGIN) {
        return Err(StorageError::ResourceLimit(format!(
            "not enough free space at the destination: the catalog takes {} bytes and {} must stay free, {} are available",
            plan.bytes, MOVE_FREE_MARGIN, free
        )));
    }
    if !to_existed {
        std::fs::create_dir(to).map_err(|e| StorageError::io(to, e))?;
    }
    let copied = copy_verify_validate(from, to, &plan, before, req.fault, progress, cancel);
    if let Err(e) = copied {
        // The source was never written: only the destination is cleaned.
        let _ = clean_destination(to, to_existed);
        return Err(e);
    }
    if req.keep_source {
        drop(lock);
        return Ok(report(
            MoveMethod::Copied,
            &plan,
            before,
            plan.left_behind.clone(),
            false,
            None,
        ));
    }
    progress(MoveProgress {
        phase: MovePhase::RemovingSource,
        done_bytes: 0,
        total_bytes: 0,
    });
    let cleanup = remove_source(from, lock).err().map(|e| e.to_string());
    Ok(report(
        MoveMethod::Copied,
        &plan,
        before,
        plan.left_behind.clone(),
        cleanup.is_none(),
        cleanup,
    ))
}

fn report(
    method: MoveMethod,
    plan: &Plan,
    c: Counts,
    left_behind: Vec<String>,
    source_removed: bool,
    source_cleanup_error: Option<String>,
) -> MoveReport {
    MoveReport {
        method,
        files: plan.files,
        dirs: plan.dirs,
        bytes: plan.bytes,
        vms: c.vms,
        media: c.media,
        baselines: c.baselines,
        left_behind,
        source_removed,
        source_cleanup_error,
    }
}

/// The source is an existing directory, not a link, with a database file.
fn check_source(from: &Path) -> Result<()> {
    let label = from.display().to_string();
    if !from.is_absolute() {
        return Err(StorageError::invalid_path(label, PathRejection::Absolute));
    }
    let meta = std::fs::symlink_metadata(from).map_err(|e| StorageError::io(from, e))?;
    if is_link(&meta) {
        return Err(StorageError::invalid_path(label, PathRejection::Link));
    }
    let db = from.join(DB_FILE);
    match std::fs::symlink_metadata(&db) {
        Ok(m) if meta.is_dir() && m.is_file() && !is_link(&m) => Ok(()),
        _ => Err(StorageError::NotFound {
            kind: "catalog",
            id: label,
        }),
    }
}

/// Checks the destination; returns `true` if it exists (empty folder).
fn check_destination(from: &Path, to: &Path) -> Result<bool> {
    let label = to.display().to_string();
    if !to.is_absolute() {
        return Err(StorageError::invalid_path(label, PathRejection::Absolute));
    }
    if to
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(StorageError::invalid_path(label, PathRejection::Traversal));
    }
    let (a, b) = (normalized(from)?, normalized(to)?);
    if a.starts_with(&b) || b.starts_with(&a) {
        return Err(StorageError::StateConflict(format!(
            "the destination {label} is the catalog folder, inside it or one of its parents: choose a folder outside the catalog"
        )));
    }
    match std::fs::symlink_metadata(to) {
        Ok(m) if is_link(&m) => Err(StorageError::invalid_path(label, PathRejection::Link)),
        Ok(m) if m.is_dir() => {
            let mut it = std::fs::read_dir(to).map_err(|e| StorageError::io(to, e))?;
            if it.next().is_some() {
                return Err(StorageError::StateConflict(format!(
                    "the destination folder {label} is not empty: choose an empty or new folder"
                )));
            }
            Ok(true)
        }
        Ok(_) => Err(StorageError::StateConflict(format!(
            "the destination {label} is a file, not a folder"
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = to.parent().ok_or_else(|| {
                StorageError::invalid_path(label.clone(), PathRejection::Absolute)
            })?;
            match std::fs::symlink_metadata(parent) {
                Ok(m) if m.is_dir() => Ok(false),
                _ => Err(StorageError::NotFound {
                    kind: "destination parent folder",
                    id: parent.display().to_string(),
                }),
            }
        }
        Err(e) => Err(StorageError::io(to, e)),
    }
}

/// Absolute path with links resolved on its existing part, compared without
/// regard to case (the volume may be case-insensitive).
fn normalized(p: &Path) -> Result<PathBuf> {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut c) => {
                for r in rest.iter().rev() {
                    c.push(r);
                }
                return Ok(PathBuf::from(c.to_string_lossy().to_lowercase()));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return Err(StorageError::io(p, e));
                };
                rest.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            Err(e) => return Err(StorageError::io(&existing, e)),
        }
    }
}

/// Inventory of the source, in creation order (a directory before what it
/// contains).
fn inventory(from: &Path, leave_behind: &[String]) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = from.join(&rel);
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| StorageError::io(&dir, e))? {
            names.push(entry.map_err(|e| StorageError::io(&dir, e))?.file_name());
        }
        names.sort();
        // Pushed in reverse so the walk is in name order (stable reports).
        let mut subdirs = Vec::new();
        for name in names {
            let child = rel.join(&name);
            if rel.as_os_str().is_empty() {
                // The lock is recreated by the destination's first opening.
                if name == LOCK_FILE {
                    continue;
                }
                if let Some(n) = name.to_str()
                    && leave_behind.iter().any(|l| l == n)
                {
                    plan.left_behind.push(n.to_owned());
                    continue;
                }
            }
            let path = from.join(&child);
            let label = child.display().to_string();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| StorageError::io(&path, e))?;
            if is_link(&meta) {
                return Err(StorageError::invalid_path(label, PathRejection::Link));
            }
            if meta.is_dir() {
                plan.dirs += 1;
                plan.entries.push(Entry {
                    rel: child.clone(),
                    len: None,
                    perm: meta.permissions(),
                });
                subdirs.push(child);
            } else if meta.is_file() {
                plan.files += 1;
                plan.bytes = plan.bytes.saturating_add(meta.len());
                plan.entries.push(Entry {
                    rel: child,
                    len: Some(meta.len()),
                    perm: meta.permissions(),
                });
            } else {
                return Err(StorageError::StateConflict(format!(
                    "{label} in the catalog is neither a file nor a folder: remove it before moving the catalog"
                )));
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(plan)
}

/// Rename of the root. An empty destination folder is removed first and
/// recreated if the rename fails.
fn rename_root(from: &Path, to: &Path, to_existed: bool) -> io::Result<()> {
    if to_existed {
        std::fs::remove_dir(to)?;
    }
    std::fs::rename(from, to).inspect_err(|_| {
        if to_existed {
            let _ = std::fs::create_dir(to);
        }
    })
}

/// The renamed catalog did not validate: it goes back where it was.
fn undo_rename(from: &Path, to: &Path, to_existed: bool, e: StorageError) -> StorageError {
    match std::fs::rename(to, from) {
        Ok(()) => {
            if to_existed {
                let _ = std::fs::create_dir(to);
            }
            e
        }
        Err(back) => StorageError::StateConflict(format!(
            "the catalog was moved to {} but did not validate ({e}) and could not be moved back ({back}): it is now in {}",
            to.display(),
            to.display()
        )),
    }
}

fn copy_verify_validate(
    from: &Path,
    to: &Path,
    plan: &Plan,
    before: Counts,
    fault: Option<MoveFault>,
    progress: &dyn Fn(MoveProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let total = plan.bytes;
    let done = std::cell::Cell::new(0u64);
    let advance = |phase: MovePhase, n: u64| {
        done.set(done.get().saturating_add(n));
        progress(MoveProgress {
            phase,
            done_bytes: done.get(),
            total_bytes: total,
        });
    };
    advance(MovePhase::Copying, 0);
    let mut hashes = Vec::new();
    for e in &plan.entries {
        if cancel() {
            return Err(cancelled());
        }
        let dst = to.join(&e.rel);
        let Some(len) = e.len else {
            std::fs::create_dir(&dst).map_err(|err| StorageError::io(&dst, err))?;
            continue;
        };
        if let Some(MoveFault::FailAfterFiles(n)) = fault
            && hashes.len() == n
        {
            return Err(StorageError::io(&dst, io::Error::other("injected failure")));
        }
        let alter = fault == Some(MoveFault::AlterCopy(hashes.len()));
        let hash = copy_file(
            &from.join(&e.rel),
            &dst,
            e,
            len,
            alter,
            &|n| {
                advance(MovePhase::Copying, n);
            },
            cancel,
        )?;
        hashes.push(hash);
    }
    sync_dirs(to, plan)?;

    done.set(0);
    advance(MovePhase::Verifying, 0);
    let files = plan.entries.iter().filter_map(|e| e.len.map(|l| (e, l)));
    for ((e, len), expected) in files.zip(&hashes) {
        let dst = to.join(&e.rel);
        let label = e.rel.display().to_string();
        let f = fsx::open_regular(&dst, Share::ReadOnly, &label)?;
        let (hash, n) = fsx::sha256_handle_with(&f, &dst, cancel, &|n| {
            advance(MovePhase::Verifying, n);
        })
        .map_err(|err| match err {
            StorageError::StateConflict(m) if m == fsx::HASH_CANCELLED => cancelled(),
            other => other,
        })?;
        if n != len || &hash != expected {
            return Err(StorageError::IntegrityMismatch {
                path: label,
                detail: format!(
                    "copy differs from the source: {n} bytes, sha256 {hash}; expected {len} bytes, sha256 {expected}"
                ),
            });
        }
    }

    if cancel() {
        return Err(cancelled());
    }
    progress(MoveProgress {
        phase: MovePhase::Validating,
        done_bytes: 0,
        total_bytes: 0,
    });
    validate(to, before)
}

/// Copies one file, returning the SHA-256 of the bytes read from the
/// source.
fn copy_file(
    src: &Path,
    dst: &Path,
    e: &Entry,
    len: u64,
    alter: bool,
    progress: &dyn Fn(u64),
    cancel: &dyn Fn() -> bool,
) -> Result<String> {
    let label = e.rel.display().to_string();
    let mut s = fsx::open_regular(src, Share::ReadOnly, &label)?;
    let mut d = fsx::create_new_exclusive(dst)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BLOCK];
    let mut total = 0u64;
    let mut first = None;
    loop {
        if cancel() {
            return Err(cancelled());
        }
        let n = s.read(&mut buf).map_err(|err| StorageError::io(src, err))?;
        if n == 0 {
            break;
        }
        first = first.or(Some(buf[0]));
        h.update(&buf[..n]);
        d.write_all(&buf[..n])
            .map_err(|err| StorageError::io(dst, err))?;
        total = total.saturating_add(n as u64);
        progress(n as u64);
    }
    if total != len {
        return Err(StorageError::IntegrityMismatch {
            path: label,
            detail: format!("source changed during the copy: {total} bytes read, {len} expected"),
        });
    }
    if alter && let Some(b) = first {
        d.seek(io::SeekFrom::Start(0))
            .and_then(|_| d.write_all(&[!b]))
            .map_err(|err| StorageError::io(dst, err))?;
    }
    d.sync_all().map_err(|err| StorageError::io(dst, err))?;
    drop(d);
    // Read-only attribute of bases and media (Unix: the whole mode).
    std::fs::set_permissions(dst, e.perm.clone()).map_err(|err| StorageError::io(dst, err))?;
    Ok(crate::hex(&h.finalize()))
}

/// Unix: `fsync` of every directory (new names durable) and their modes,
/// deepest first. Windows has no directory `fsync` through `std`; NTFS
/// journals the metadata.
fn sync_dirs(to: &Path, plan: &Plan) -> Result<()> {
    #[cfg(unix)]
    {
        let dirs = plan.entries.iter().filter(|e| e.len.is_none());
        for e in dirs.rev() {
            let p = to.join(&e.rel);
            std::fs::File::open(&p)
                .and_then(|f| f.sync_all())
                .map_err(|err| StorageError::io(&p, err))?;
            std::fs::set_permissions(&p, e.perm.clone())
                .map_err(|err| StorageError::io(&p, err))?;
        }
        std::fs::File::open(to)
            .and_then(|f| f.sync_all())
            .map_err(|err| StorageError::io(to, err))?;
    }
    #[cfg(not(unix))]
    let _ = (to, plan);
    Ok(())
}

/// Opens the destination as a catalog and compares its content.
fn validate(to: &Path, before: Counts) -> Result<()> {
    let cat = Catalog::open(to)?;
    let check: String = cat
        .conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .db()?;
    if check != "ok" {
        return Err(StorageError::IntegrityMismatch {
            path: DB_FILE.into(),
            detail: format!("quick_check of the moved database: {check}"),
        });
    }
    let after = counts(&cat)?;
    if after != before {
        return Err(StorageError::IntegrityMismatch {
            path: DB_FILE.into(),
            detail: format!("moved catalog holds {after:?}, the source held {before:?}"),
        });
    }
    Ok(())
}

/// Removes what the move created at the destination; a folder that existed
/// (empty) before stays.
fn clean_destination(to: &Path, to_existed: bool) -> Result<()> {
    if !to_existed {
        return remove_dir_no_links(to);
    }
    remove_children(to, |_| true)
}

/// Removes the entries of `dir` accepted by `which`, without following
/// links and clearing read-only attributes.
fn remove_children(dir: &Path, which: impl Fn(&std::ffi::OsStr) -> bool) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| StorageError::io(dir, e))? {
        let entry = entry.map_err(|e| StorageError::io(dir, e))?;
        if !which(&entry.file_name()) {
            continue;
        }
        let p = entry.path();
        let m = std::fs::symlink_metadata(&p).map_err(|e| StorageError::io(&p, e))?;
        if is_link(&m) {
            if std::fs::remove_file(&p).is_err() {
                std::fs::remove_dir(&p).map_err(|e| StorageError::io(&p, e))?;
            }
        } else if m.is_dir() {
            remove_dir_no_links(&p)?;
        } else {
            force_remove_file(&p, m)?;
        }
    }
    Ok(())
}

/// Removes the source: everything but the lock file under the lock, then
/// the lock file and the root.
fn remove_source(from: &Path, lock: CatalogLock) -> Result<()> {
    remove_children(from, |n| n != LOCK_FILE)?;
    drop(lock);
    remove_dir_no_links(from)
}
