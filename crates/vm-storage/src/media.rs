//! Registry of approved media (ROADMAP 6.3, 7.4 row "File"; ADR-0008,
//! ADR-0029).
//!
//! A path chosen by the user does not authorize whatever file later replaces
//! it: on approval the catalog records the file's SHA-256 and size, and
//! every use re-verifies them through a handle that keeps the file
//! unchangeable until the run ends ([`MediaGuard`]).
//!
//! **Media used in place (ADR-0029, owner decision).** New imports never
//! copy the ISO: the registry row records the absolute path of the user's
//! file as resolved by the system ([`MediaLocation::Referenced`]). Procedure
//! of an import ([`Catalog::reserve_media_import`] →
//! [`MediaImportReservation::open_and_hash`] → [`Catalog::finish_media_import`]):
//!
//! 1. the reservation chooses the identifier and touches nothing (no journal
//!    intent, no file: an interrupted import leaves nothing behind);
//! 2. on a worker thread the source is opened once for reading without
//!    following a reparse point on the last component (Windows:
//!    `FILE_FLAG_OPEN_REPARSE_POINT`) and with `FILE_SHARE_READ` only, so
//!    nobody can modify, rename or delete it while it is examined; type
//!    (regular file, not a link) and size are checked on the handle, the
//!    caller's own checks run on it and give the path to record, then
//!    SHA-256 and the ISO 9660 / UDF signature are read in a single pass
//!    through the same handle, which stays open (the guest detection of the
//!    caller reads through it too);
//! 3. the registry row is written in one transaction; the same file selected
//!    again (same path and kind) updates its row instead, keeping the
//!    identifier the VMs refer to.
//!
//! The file is the user's: the catalog never writes, moves or deletes it.
//! Between runs it can change or disappear; the next start then refuses it
//! (hash mismatch, or not found → status `missing`).
//!
//! **Managed copies.** Media approved before schema 8 keep their read-only
//! copy in `media/<media_id>.iso` ([`MediaLocation::Managed`]) and keep
//! working. [`Catalog::delete_media`] removes such a copy through one
//! `delete_file` intent (target [`TargetKind::Media`]): the copy is moved
//! into `tmp/`, the row is deleted with the intent commit, then the
//! temporary is removed (read-only attribute cleared first). Reconciliation
//! rolls a moved copy forward (row deleted) or back (file restored, if a VM
//! was attached meanwhile) like any other `delete_file` intent; unfinished
//! `import_media` intents of an older version still reconcile as before.
//!
//! Limits: the file's OS identity (volume serial number and file index,
//! `FileIdentity` from `lightvm-proto`) cannot be read with the stable `std`
//! APIs on Windows without `unsafe` code: it is not recorded. The read-only
//! attribute of a managed copy can be removed by the same user;
//! [`Catalog::verify_media`] recomputes the hash through a handle with
//! `FILE_SHARE_READ` only before use. Without share modes (non-Windows
//! hosts) a file used in place can be written by another process during a
//! run (declared limit, R2a).

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, IntentSpec};
use crate::enums::{IntentKind, MediaKind, MediaStatus, TargetKind};
use crate::error::{PathRejection, Result, StorageError};
use crate::fsx::{self, Share};
use crate::ids::MediaId;
use crate::paths::{ManagedPath, is_link};
use crate::records::{MediaLocation, MediaRecord};
use crate::time::now_rfc3339;
use lightvm_proto::DetectedGuest;

/// Maximum length of the declared origin (free text, never a path to open).
pub const MAX_DECLARED_ORIGIN: usize = 512;

/// Result of [`Catalog::verify_media`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCheck {
    /// Verified registry entry.
    pub record: MediaRecord,
    /// The read-only attribute of a managed copy was missing and has been
    /// restored (always `false` for a file used in place).
    pub readonly_restored: bool,
}

/// Checks applied on the opened source handle, on the bytes hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaLimits {
    /// Maximum size in bytes ([`StorageError::ResourceLimit`] above it).
    pub max_bytes: u64,
    /// Requires the ISO 9660 (`CD001`) or UDF (`BEA01`) signature of the
    /// first volume descriptor ([`StorageError::Unsupported`] with
    /// [`NOT_OPTICAL_IMAGE`] otherwise).
    pub optical_signature: bool,
}

impl MediaLimits {
    /// No limit (behaviour of [`Catalog::approve_media`]).
    pub const NONE: Self = Self {
        max_bytes: u64::MAX,
        optical_signature: false,
    };
}

/// Message of the [`StorageError::Unsupported`] returned for a source without
/// an optical image signature.
pub const NOT_OPTICAL_IMAGE: &str = "the medium is not an ISO 9660 or UDF image";

/// Offset of the identifier of the first volume descriptor (sector 16 of
/// 2048 bytes, after the type byte): `CD001` (ISO 9660) or `BEA01` (UDF).
const VOLUME_DESCRIPTOR_ID_OFFSET: u64 = 16 * 2048 + 1;

/// Name of the source in errors (never the user's path).
const SOURCE_LABEL: &str = "media source";

/// Import reserved by [`Catalog::reserve_media_import`]: identifier chosen,
/// nothing recorded and the source **not** opened yet. Every access to the
/// user's file (open, checks, hash) happens in [`Self::open_and_hash`],
/// which needs no catalog and runs on a worker thread: a slow or hanging
/// source never blocks the thread that owns the catalog.
#[derive(Debug)]
pub struct MediaImportReservation {
    media_id: MediaId,
    source: PathBuf,
    kind: MediaKind,
    declared_origin: String,
    limits: MediaLimits,
}

/// Outcome of [`MediaImportReservation::open_and_hash`], to give to
/// [`Catalog::finish_media_import`] (or [`Catalog::abort_media_import`]).
/// Holds the source handle (`FILE_SHARE_READ` only) until then.
#[derive(Debug)]
pub struct HashedMedia {
    media_id: MediaId,
    kind: MediaKind,
    declared_origin: String,
    /// Open source, kept so the file cannot change until the row is written.
    file: Option<File>,
    /// Path to record, SHA-256 and size of the bytes read.
    outcome: Result<(PathBuf, String, u64)>,
    /// Outcome of the guest OS detection (`None`: not examined).
    detected: Option<Option<DetectedGuest>>,
}

impl HashedMedia {
    /// The source handle when the checks and the hash succeeded: the guest
    /// OS detection (ADR-0027) reads the hashed file through it, so nobody
    /// can change it in between.
    pub fn source_file(&self) -> Option<&File> {
        self.outcome.as_ref().ok().and(self.file.as_ref())
    }

    /// Records the guest OS detected on [`Self::source_file`] (`None`:
    /// nothing supported recognized): stored with the registry row.
    pub fn set_detected_guest(&mut self, guest: Option<DetectedGuest>) {
        self.detected = Some(guest);
    }
}

impl MediaImportReservation {
    /// Ends the reservation without touching the source (refused by the
    /// caller's own checks): [`Catalog::finish_media_import`] returns
    /// `reason`.
    pub fn refuse(self, reason: StorageError) -> HashedMedia {
        HashedMedia {
            media_id: self.media_id,
            kind: self.kind,
            declared_origin: self.declared_origin,
            file: None,
            outcome: Err(reason),
            detected: None,
        }
    }

    /// Opens the source once (shared for reading only, without following a
    /// reparse point on the last component), checks type and size on the
    /// handle, runs `check` on the same handle (the caller's volume and
    /// final-path checks), which returns the absolute path to record, then
    /// reads SHA-256 and signature from that handle in one pass. The handle
    /// stays open in the result. Any failure is in the outcome:
    /// [`Catalog::finish_media_import`] then returns it.
    pub fn open_and_hash(self, check: impl FnOnce(&File, &Path) -> Result<PathBuf>) -> HashedMedia {
        let Self {
            media_id,
            source,
            kind,
            declared_origin,
            limits,
        } = self;
        let mut file = None;
        let outcome = (|| {
            let (src, expected_len) = open_source(&source, limits)?;
            let src = file.insert(src);
            let path = check(src, &source)?;
            if !path.is_absolute() {
                return Err(StorageError::invalid_path(
                    SOURCE_LABEL,
                    PathRejection::Absolute,
                ));
            }
            let (sha, size) = hash_source(src, &source, limits)?;
            if size != expected_len {
                return Err(StorageError::IntegrityMismatch {
                    path: SOURCE_LABEL.into(),
                    detail: format!("read {size} bytes, expected {expected_len}"),
                });
            }
            Ok((path, sha, size))
        })();
        HashedMedia {
            media_id,
            kind,
            declared_origin,
            file,
            outcome,
            detected: None,
        }
    }
}

/// SHA-256 of the whole source read from the start of the handle, with the
/// optical signature taken from the same bytes if required.
fn hash_source(mut src: &File, source: &Path, limits: MediaLimits) -> Result<(String, u64)> {
    use std::io::Seek as _;
    src.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| StorageError::io(source, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    let mut sig = [0u8; 5];
    let sig_end = VOLUME_DESCRIPTOR_ID_OFFSET + sig.len() as u64;
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| StorageError::io(source, e))?;
        if n == 0 {
            break;
        }
        // Bytes of the signature that fall in this block.
        let end = total.saturating_add(n as u64);
        let lo = VOLUME_DESCRIPTOR_ID_OFFSET.max(total);
        let hi = sig_end.min(end);
        for off in lo..hi {
            let (Ok(si), Ok(bi)) = (
                usize::try_from(off - VOLUME_DESCRIPTOR_ID_OFFSET),
                usize::try_from(off - total),
            ) else {
                break;
            };
            sig[si] = buf[bi];
        }
        h.update(&buf[..n]);
        total = end;
        if total > limits.max_bytes {
            return Err(StorageError::ResourceLimit(format!(
                "medium over the limit of {} bytes",
                limits.max_bytes
            )));
        }
    }
    if limits.optical_signature && (total < sig_end || (&sig != b"CD001" && &sig != b"BEA01")) {
        return Err(StorageError::Unsupported(NOT_OPTICAL_IMAGE));
    }
    Ok((crate::hex(&h.finalize()), total))
}

/// Opens the source of an import and checks type and size on the handle.
fn open_source(source: &Path, limits: MediaLimits) -> Result<(File, u64)> {
    let src = fsx::open_regular(source, Share::ReadOnly, SOURCE_LABEL)?;
    let len = src
        .metadata()
        .map_err(|e| StorageError::io(source, e))?
        .len();
    if len > limits.max_bytes {
        return Err(StorageError::ResourceLimit(format!(
            "medium of {len} bytes over the limit of {}",
            limits.max_bytes
        )));
    }
    Ok((src, len))
}

/// Path to record for a source opened by [`Catalog::approve_media`]: the
/// absolute path the system resolves for it (links in the directories
/// resolved, Windows verbatim prefix `\\?\` removed from a drive path).
/// The supervisor records the final path of the handle instead, after its
/// own checks.
pub fn canonical_source(_file: &File, requested: &Path) -> Result<PathBuf> {
    let p = std::fs::canonicalize(requested).map_err(|e| StorageError::io(requested, e))?;
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && rest.as_bytes().get(1) == Some(&b':')
        {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(p)
}

fn check_origin(declared_origin: &str) -> Result<()> {
    if declared_origin.len() > MAX_DECLARED_ORIGIN || declared_origin.chars().any(char::is_control)
    {
        return Err(StorageError::Constraint(
            "declared origin too long or contains control characters".into(),
        ));
    }
    Ok(())
}

/// Label of a media file in errors: the managed path, or only the file name
/// of a file used in place (never the user's full path).
fn location_label(l: &MediaLocation) -> String {
    match l {
        MediaLocation::Managed(p) => p.to_string(),
        MediaLocation::Referenced(p) => p.file_name().map_or_else(
            || SOURCE_LABEL.to_owned(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

impl Catalog {
    /// Approves a medium in one call: checks, hash and registry row, the
    /// file used in place ([`canonical_source`] is the path recorded).
    /// `source` is the file chosen by the user with the native picker (never
    /// a path received from a client).
    pub fn approve_media(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
    ) -> Result<MediaRecord> {
        self.approve_media_checked(source, kind, declared_origin, MediaLimits::NONE)
    }

    /// [`Self::approve_media`] with size and format checks on the opened
    /// handle: the size is checked before reading, the signature on the
    /// bytes hashed. Hashing reads the whole file once (sequential, 1 MiB
    /// blocks). The same procedure in three steps, with the hash outside the
    /// catalog, is [`Self::reserve_media_import`] →
    /// [`MediaImportReservation::open_and_hash`] → [`Self::finish_media_import`].
    pub fn approve_media_checked(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
        limits: MediaLimits,
    ) -> Result<MediaRecord> {
        let reservation = self.reserve_media_import(source, kind, declared_origin, limits)?;
        self.finish_media_import(reservation.open_and_hash(canonical_source))
    }

    /// First step of an import, which never touches the source: checks the
    /// declared origin and chooses the identifier. Nothing is recorded: an
    /// import abandoned at any point leaves nothing to reconcile.
    pub fn reserve_media_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
        limits: MediaLimits,
    ) -> Result<MediaImportReservation> {
        check_origin(declared_origin)?;
        Ok(MediaImportReservation {
            media_id: MediaId::new_v7(),
            source: source.to_path_buf(),
            kind,
            declared_origin: declared_origin.to_owned(),
            limits,
        })
    }

    /// Abandons an import (cancelled): only the source handle is closed.
    pub fn abort_media_import(&mut self, hashed: HashedMedia) -> Result<()> {
        drop(hashed);
        Ok(())
    }

    /// Last step of an import: records the registry row in one transaction,
    /// while the source handle is still open. A file already in the
    /// registry with the same path and kind is approved again in its row
    /// (new hash and size, status `approved`): the identifier the VMs use
    /// does not change. Returns the error of a failed check or hash.
    pub fn finish_media_import(&mut self, hashed: HashedMedia) -> Result<MediaRecord> {
        let HashedMedia {
            media_id,
            kind,
            declared_origin,
            file,
            outcome,
            detected,
        } = hashed;
        let (path, sha256, size) = outcome?;
        let record = self.with_tx(|tx| {
            let existing = tx.referenced_media(&path, kind)?;
            let mut record = MediaRecord {
                media_id: existing.unwrap_or(media_id),
                sha256,
                size_bytes: size,
                kind,
                declared_origin,
                approved_at: now_rfc3339(),
                location: MediaLocation::Referenced(path),
                status: MediaStatus::Approved,
                guest_examined: detected.is_some(),
                detected_guest: detected.flatten(),
            };
            if existing.is_some() {
                tx.reapprove_media(&record)?;
                record = tx.media(&record.media_id)?;
            } else {
                tx.insert_media(&record)?;
            }
            Ok(record)
        })?;
        drop(file);
        Ok(record)
    }

    /// Verifies a medium before use, through a handle opened with
    /// `FILE_SHARE_READ` only, without following links ([`MediaGuard`]):
    /// SHA-256 and size recomputed through the handle; the read-only
    /// attribute of a managed copy restored if missing. File not found:
    /// media marked `missing` and [`StorageError::NotFound`].
    pub fn verify_media(&mut self, id: &MediaId) -> Result<MediaCheck> {
        let record = self.read(|tx| tx.media(id))?;
        if !matches!(record.status, MediaStatus::Approved | MediaStatus::Missing) {
            return Err(StorageError::StateConflict(format!(
                "media {id} not approved ({})",
                record.status
            )));
        }
        let opened = match &record.location {
            MediaLocation::Managed(rel) => MediaGuard::open(self.layout.root(), rel),
            MediaLocation::Referenced(p) => MediaGuard::open_external(p),
        };
        let guard = match opened {
            Ok(g) => g,
            Err(StorageError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                self.with_tx(|tx| tx.set_media_status(id, MediaStatus::Missing))?;
                return Err(StorageError::NotFound {
                    kind: "media file",
                    id: location_label(&record.location),
                });
            }
            Err(e) => return Err(e),
        };
        if record.status == MediaStatus::Missing {
            // A managed copy that went missing stays missing; a file used
            // in place may be back (the hash below decides).
            if record.location.managed().is_some() {
                return Err(StorageError::StateConflict(format!(
                    "media {id} not approved ({})",
                    record.status
                )));
            }
            self.with_tx(|tx| tx.set_media_status(id, MediaStatus::Approved))?;
        }
        let (sha, size) = guard.sha256_with(&|| false, &|_| {})?;
        if sha != record.sha256 || size != record.size_bytes {
            return Err(StorageError::IntegrityMismatch {
                path: location_label(&record.location),
                detail: format!(
                    "expected {} ({} bytes), found {sha} ({size} bytes)",
                    record.sha256, record.size_bytes
                ),
            });
        }
        let readonly_restored = match &record.location {
            MediaLocation::Managed(_) => fsx::ensure_readonly(&guard.abs)?,
            MediaLocation::Referenced(_) => false,
        };
        drop(guard);
        Ok(MediaCheck {
            record: MediaRecord {
                status: MediaStatus::Approved,
                ..record
            },
            readonly_restored,
        })
    }

    /// Deletes a medium of the registry (approved or `missing`).
    ///
    /// A file used in place belongs to the user: only the row is removed,
    /// the file is never opened, moved or deleted. A managed copy is
    /// removed with the row through the journal (see the module
    /// documentation).
    ///
    /// Refused with [`StorageError::StateConflict`] (naming the VMs) while
    /// a VM has it attached, and, for a managed copy, with
    /// [`StorageError::VmBusy`] while another intent on it is unfinished or
    /// a handle holds the copy open without sharing deletion (the
    /// [`MediaGuard`] of a run: on Windows the move into `tmp/` then fails
    /// with a sharing violation). The read-only attribute of the copy is
    /// cleared just before the file is removed. A copy already missing only
    /// removes the row.
    pub fn delete_media(&mut self, id: &MediaId) -> Result<()> {
        let id_s = id.to_string();
        let (record, users, busy) = self.read(|tx| {
            let busy: bool = tx
                .conn()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM journal WHERE target_kind = ?1
                       AND target_id = ?2 AND state IN ('pending','applied','needs_review'))",
                    rusqlite::params![TargetKind::Media, id_s],
                    |r| r.get(0),
                )
                .map_err(crate::error::map_sqlite)?;
            Ok((tx.media(id)?, tx.media_users(id)?, busy))
        })?;
        if !users.is_empty() {
            return Err(StorageError::StateConflict(in_use_message(&users)));
        }
        if busy {
            return Err(StorageError::VmBusy(format!(
                "another operation on media {id} is unfinished"
            )));
        }
        let managed = match &record.location {
            MediaLocation::Managed(rel) => rel.clone(),
            MediaLocation::Referenced(_) => return self.with_tx(|tx| tx.delete_media(id)),
        };
        let abs = self.layout.resolve(&managed)?;
        if !crate::catalog::exists_no_follow(&abs)? {
            return self.with_tx(|tx| tx.delete_media(id));
        }
        let intent = self.begin_intent(IntentSpec {
            kind: IntentKind::DeleteFile,
            vm_id: None,
            operation_id: None,
            target_kind: TargetKind::Media,
            target_id: Some(id_s),
            final_path: Some(managed),
            read_only: false,
        })?;
        let result = (|| -> Result<()> {
            self.mark_applied(&intent).map_err(|e| match e {
                StorageError::Io { source, .. } if fsx::is_sharing_violation(&source) => {
                    StorageError::VmBusy(format!("media {id} is held open by a running VM"))
                }
                e => e,
            })?;
            self.fault_at(crate::FaultPoint::DeleteBeforeCommit)?;
            // `commit` removes the moved copy from `tmp/`, clearing its
            // read-only attribute first.
            self.commit(&intent, |tx| tx.delete_media(id))
        })();
        match result {
            Ok(()) => Ok(()),
            Err(e @ StorageError::InjectedCrash(_)) => Err(e),
            Err(e) => {
                // A failed abort leaves the intent open: reconciliation
                // resolves it at the next startup.
                let _ = self.abort(&intent);
                Err(e)
            }
        }
    }
}

/// Refusal of a deletion while VMs use a medium: their number and names (at
/// most five, then how many more).
pub(crate) fn in_use_message(users: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut names: Vec<String> = users
        .iter()
        .take(SHOWN)
        .map(|n| format!("\u{201c}{n}\u{201d}"))
        .collect();
    if users.len() > SHOWN {
        names.push(format!("{} more", users.len() - SHOWN));
    }
    let n = users.len();
    format!(
        "the medium is used by {n} VM{}: {}",
        if n == 1 { "" } else { "s" },
        names.join(", ")
    )
}

/// Every existing component of an absolute path, from the first directory
/// below the root to the file itself, checked against links and reparse
/// points (as [`crate::paths`] does under the catalog root). A component
/// that does not exist is an I/O error of kind `NotFound`.
fn check_external_components(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute() {
        return Err(StorageError::invalid_path(label, PathRejection::Absolute));
    }
    let mut chain: Vec<&Path> = path.ancestors().collect();
    // The root itself (`C:\`, `/`) is not a component that can be a link.
    chain.pop();
    for p in chain.into_iter().rev() {
        let meta = std::fs::symlink_metadata(p).map_err(|e| StorageError::io(p, e))?;
        if is_link(&meta) {
            return Err(StorageError::invalid_path(label, PathRejection::Link));
        }
    }
    Ok(())
}

/// File of an approved medium held open for a VM start (ADR-0008,
/// ADR-0025, ADR-0029): a managed copy, or the user's file used in place.
///
/// Opened by the supervisor thread before the hash and kept until the run is
/// released: with `FILE_SHARE_READ` only (Windows) nobody can write, rename
/// or delete the file, nor rename a directory above it, while the guard is
/// alive; a process already holding it open for writing makes the open fail
/// ([`StorageError::VmBusy`]). The hash ([`Self::sha256_with`]) is read
/// **through this handle** (on any thread), so the bytes verified are the
/// bytes QEMU reads from the same, unchangeable file.
#[derive(Debug)]
pub struct MediaGuard {
    file: File,
    abs: PathBuf,
    size: u64,
}

impl MediaGuard {
    /// Opens `rel` under the catalog `root`: every existing component
    /// checked against links and reparse points, the file opened without
    /// following a reparse point and checked to be a regular file on the
    /// handle. A missing copy is an I/O error of kind `NotFound`.
    pub fn open(root: &Path, rel: &ManagedPath) -> Result<Self> {
        let abs = crate::paths::resolve_checked(root, rel)?;
        Self::open_abs(abs, rel.as_str())
    }

    /// Opens the user's file of a medium used in place (ADR-0029), given by
    /// its absolute path: every component (each directory and the file) is
    /// checked not to be a link or reparse point, then the file is opened
    /// as in [`Self::open`]. A missing file (or directory) is an I/O error
    /// of kind `NotFound`. Errors name the file, never the full path.
    pub fn open_external(path: &Path) -> Result<Self> {
        let label = path.file_name().map_or_else(
            || SOURCE_LABEL.to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        check_external_components(path, &label)?;
        Self::open_abs(path.to_path_buf(), &label)
    }

    fn open_abs(abs: PathBuf, label: &str) -> Result<Self> {
        let file = fsx::open_regular(&abs, Share::ReadOnly, label)?;
        let size = file
            .metadata()
            .map_err(|e| StorageError::io(&abs, e))?
            .len();
        Ok(Self { file, abs, size })
    }

    /// Absolute path of the guarded file (the one QEMU receives).
    pub fn path(&self) -> &Path {
        &self.abs
    }

    /// Size read on the handle at open.
    pub fn size_bytes(&self) -> u64 {
        self.size
    }

    /// `true` if the file has the read-only attribute (read on the handle).
    pub fn read_only(&self) -> Result<bool> {
        Ok(self
            .file
            .metadata()
            .map_err(|e| StorageError::io(&self.abs, e))?
            .permissions()
            .readonly())
    }

    /// The guard handle (reads only: for example the volume descriptor).
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Hex SHA-256 and bytes read through the handle; `cancel` is polled
    /// between 1 MiB blocks ([`StorageError::StateConflict`] with
    /// [`crate::HASH_CANCELLED`]), `progress` receives the bytes read.
    pub fn sha256_with(
        &self,
        cancel: &dyn Fn() -> bool,
        progress: &dyn Fn(u64),
    ) -> Result<(String, u64)> {
        fsx::sha256_handle_with(&self.file, &self.abs, cancel, progress)
    }
}
