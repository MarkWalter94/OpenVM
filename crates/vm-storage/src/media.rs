//! Registry of approved media (ROADMAP 6.3, 7.4 row "File"; ADR-0008).
//!
//! A path chosen by the user does not authorize whatever file later replaces
//! it: on approval the catalog produces a **read-only managed copy** in
//! `media/<media_id>.iso` and records its SHA-256 and size. QEMU only ever
//! receives the managed copy.
//!
//! Procedure of [`Catalog::approve_media`]:
//!
//! 1. the source is opened once for reading without following a reparse
//!    point on the last component (Windows: `FILE_FLAG_OPEN_REPARSE_POINT`)
//!    and with `FILE_SHARE_READ` only, so nobody can modify it during the
//!    copy; the type (regular file, not a link) is checked on the handle's
//!    metadata;
//! 2. an `import_media` journal intent reserves the temporary file in `tmp/`,
//!    created with `CREATE_NEW` and an exclusive share mode;
//! 3. copy and SHA-256 are computed in a single read from the same handle:
//!    the recorded hash is that of the bytes copied, not of a reopened file;
//! 4. publication without overwrite (hard link) and read-only attribute;
//!    the `media` row and the intent confirmation in the same transaction.
//!
//! Limits: the file's OS identity (volume serial number and file index,
//! `FileIdentity` from `lightvm-proto`) cannot be read with the stable `std`
//! APIs on Windows without `unsafe` code: it is not recorded. The read-only
//! attribute can be removed by the same user; [`Catalog::verify_media`]
//! recomputes the hash through a handle with `FILE_SHARE_READ` only before
//! use.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, Intent, IntentSpec};
use crate::enums::{IntentKind, MediaKind, MediaStatus, TargetKind};
use crate::error::{Result, StorageError};
use crate::fsx::{self, Share};
use crate::ids::MediaId;
use crate::layout::CatalogLayout;
use crate::paths::ManagedPath;
use crate::records::MediaRecord;
use crate::time::now_rfc3339;

/// Maximum length of the declared origin (free text, never a path to open).
pub const MAX_DECLARED_ORIGIN: usize = 512;

/// Result of [`Catalog::verify_media`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCheck {
    /// Verified registry entry.
    pub record: MediaRecord,
    /// The read-only attribute was missing and has been restored.
    pub readonly_restored: bool,
}

/// Checks applied by [`Catalog::approve_media_checked`] on the opened source
/// handle, so that the bytes checked are the bytes copied.
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

#[derive(Debug)]
struct ImportMeta {
    intent: Intent,
    media_id: MediaId,
    final_rel: ManagedPath,
    kind: MediaKind,
    declared_origin: String,
}

/// Import started by [`Catalog::begin_media_import`]: open source handle and
/// reserved temporary file. [`Self::copy`] needs no catalog and can run on
/// another thread while the catalog serves other requests.
#[derive(Debug)]
pub struct MediaImportJob {
    src: File,
    source: PathBuf,
    expected_len: u64,
    limits: MediaLimits,
    meta: ImportMeta,
}

/// Outcome of [`MediaImportJob::copy`], to give to
/// [`Catalog::finish_media_import`].
#[derive(Debug)]
pub struct CopiedMedia {
    meta: ImportMeta,
    /// SHA-256 and size of the copied bytes.
    outcome: Result<(String, u64)>,
}

impl MediaImportJob {
    /// Size of the source measured on the handle.
    pub fn size_bytes(&self) -> u64 {
        self.expected_len
    }

    /// Copies the source into the temporary file (`CREATE_NEW`, exclusive)
    /// and computes the SHA-256 of the copied bytes in the same read; checks
    /// the optical signature on those bytes if required. Synced before
    /// returning. Never touches the catalog database.
    pub fn copy(self) -> CopiedMedia {
        let Self {
            mut src,
            source,
            expected_len,
            limits,
            meta,
        } = self;
        let outcome = copy_and_hash(&mut src, &source, &meta.intent.temp_path, limits).and_then(
            |(sha, size)| {
                if size == expected_len {
                    Ok((sha, size))
                } else {
                    Err(StorageError::IntegrityMismatch {
                        path: SOURCE_LABEL.into(),
                        detail: format!("read {size} bytes, expected {expected_len}"),
                    })
                }
            },
        );
        drop(src);
        CopiedMedia { meta, outcome }
    }
}

fn copy_and_hash(
    src: &mut File,
    source: &Path,
    temp: &Path,
    limits: MediaLimits,
) -> Result<(String, u64)> {
    let mut dst = fsx::create_new_exclusive(temp)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
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
        dst.write_all(&buf[..n])
            .map_err(|e| StorageError::io(temp, e))?;
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
    dst.sync_all().map_err(|e| StorageError::io(temp, e))?;
    Ok((crate::hex(&h.finalize()), total))
}

/// Import reserved by [`Catalog::reserve_media_import`]: the `import_media`
/// intent is recorded but the source is **not** opened yet. Every access to
/// the user's file (open, checks, copy) happens in
/// [`Self::open_and_copy`], which needs no catalog and runs on a worker
/// thread: a slow or hanging source (network, removable media) never blocks
/// the thread that owns the catalog.
#[derive(Debug)]
pub struct MediaImportReservation {
    source: PathBuf,
    limits: MediaLimits,
    meta: ImportMeta,
}

impl MediaImportReservation {
    /// Ends the reservation without touching the source (refused by the
    /// caller's own checks): [`Catalog::finish_media_import`] returns
    /// `reason` and aborts the intent.
    pub fn refuse(self, reason: StorageError) -> CopiedMedia {
        CopiedMedia {
            meta: self.meta,
            outcome: Err(reason),
        }
    }

    /// Opens the source once (shared for reading only, without following a
    /// reparse point on the last component), checks type and size on the
    /// handle, runs `check` on the same handle (for example the volume and
    /// final-path checks of the caller), then copies and hashes from that
    /// handle as [`MediaImportJob::copy`] does. Any failure is in the
    /// outcome: [`Catalog::finish_media_import`] then aborts the intent.
    pub fn open_and_copy(self, check: impl FnOnce(&File) -> Result<()>) -> CopiedMedia {
        let Self {
            source,
            limits,
            meta,
        } = self;
        let outcome = (|| {
            let (mut src, expected_len) = open_source(&source, limits)?;
            check(&src)?;
            {
                use std::io::Seek as _;
                src.seek(std::io::SeekFrom::Start(0))
                    .map_err(|e| StorageError::io(&source, e))?;
            }
            let (sha, size) = copy_and_hash(&mut src, &source, &meta.intent.temp_path, limits)?;
            if size == expected_len {
                Ok((sha, size))
            } else {
                Err(StorageError::IntegrityMismatch {
                    path: SOURCE_LABEL.into(),
                    detail: format!("read {size} bytes, expected {expected_len}"),
                })
            }
        })();
        CopiedMedia { meta, outcome }
    }
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

fn check_origin(declared_origin: &str) -> Result<()> {
    if declared_origin.len() > MAX_DECLARED_ORIGIN || declared_origin.chars().any(char::is_control)
    {
        return Err(StorageError::Constraint(
            "declared origin too long or contains control characters".into(),
        ));
    }
    Ok(())
}

impl Catalog {
    /// Approves a medium: read-only managed copy, hash and registry row.
    /// `source` is the file chosen by the user with the native picker (never
    /// a path received from a client). No overwrite: the copy is created with
    /// `CREATE_NEW` and published with a hard link that fails if the
    /// destination exists.
    pub fn approve_media(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
    ) -> Result<MediaRecord> {
        self.approve_media_checked(source, kind, declared_origin, MediaLimits::NONE)
    }

    /// [`Self::approve_media`] with size and format checks on the opened
    /// handle: the size is checked before copying, the signature on the
    /// bytes actually copied (the copy is discarded if it is missing).
    /// The copy reads the whole file once (sequential, 64 KiB blocks): for a
    /// multi-GiB ISO it takes as long as the disk needs to copy it. The same
    /// procedure in three steps, with the copy outside the catalog, is
    /// [`Self::begin_media_import`] → [`MediaImportJob::copy`] →
    /// [`Self::finish_media_import`].
    pub fn approve_media_checked(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
        limits: MediaLimits,
    ) -> Result<MediaRecord> {
        let job = self.begin_media_import(source, kind, declared_origin, limits)?;
        self.finish_media_import(job.copy())
    }

    /// First step of an import: checks the declared origin, opens the source
    /// once (shared for reading only, without following a reparse point),
    /// checks type and size on the handle and records the `import_media`
    /// intent. The returned job holds the handle: nobody can modify the
    /// source until it is dropped. A job never finished leaves a `pending`
    /// intent that reconciliation rolls back at the next start.
    pub fn begin_media_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
        limits: MediaLimits,
    ) -> Result<MediaImportJob> {
        check_origin(declared_origin)?;
        let (src, expected_len) = open_source(source, limits)?;
        let meta = self.import_intent(kind, declared_origin)?;
        Ok(MediaImportJob {
            src,
            source: source.to_path_buf(),
            expected_len,
            limits,
            meta,
        })
    }

    /// First step of an import that never touches the source: checks the
    /// declared origin and records the `import_media` intent. The source is
    /// opened, checked and copied later by
    /// [`MediaImportReservation::open_and_copy`] (on any thread), and the
    /// import ends with [`Self::finish_media_import`] or
    /// [`Self::abort_media_import`]. A reservation never finished leaves a
    /// `pending` intent that reconciliation rolls back at the next start.
    pub fn reserve_media_import(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
        limits: MediaLimits,
    ) -> Result<MediaImportReservation> {
        check_origin(declared_origin)?;
        let meta = self.import_intent(kind, declared_origin)?;
        Ok(MediaImportReservation {
            source: source.to_path_buf(),
            limits,
            meta,
        })
    }

    /// Discards a copy without publishing it (import cancelled): the intent
    /// is aborted and the temporary file removed.
    pub fn abort_media_import(&mut self, copied: CopiedMedia) -> Result<()> {
        self.abort(&copied.meta.intent)
    }

    fn import_intent(&mut self, kind: MediaKind, declared_origin: &str) -> Result<ImportMeta> {
        let media_id = MediaId::new_v7();
        let final_rel = CatalogLayout::media_file(&media_id, "iso")?;
        let intent = self.begin_intent(IntentSpec {
            kind: IntentKind::ImportMedia,
            vm_id: None,
            operation_id: None,
            target_kind: TargetKind::Media,
            target_id: Some(media_id.to_string()),
            final_path: Some(final_rel.clone()),
            read_only: true,
        })?;
        Ok(ImportMeta {
            intent,
            media_id,
            final_rel,
            kind,
            declared_origin: declared_origin.to_owned(),
        })
    }

    /// Last step of an import: publishes the copy (read-only, without
    /// overwriting) and records the registry row with the intent commit. If
    /// the copy failed, or publishing fails, the intent is aborted and the
    /// temporary removed.
    pub fn finish_media_import(&mut self, copied: CopiedMedia) -> Result<MediaRecord> {
        let CopiedMedia { meta, outcome } = copied;
        let result = outcome.and_then(|(sha256, size)| {
            self.mark_applied(&meta.intent)?;
            let record = MediaRecord {
                media_id: meta.media_id,
                sha256,
                size_bytes: size,
                kind: meta.kind,
                declared_origin: meta.declared_origin.clone(),
                approved_at: now_rfc3339(),
                managed_path: meta.final_rel.clone(),
                status: MediaStatus::Approved,
            };
            self.commit(&meta.intent, |tx| tx.insert_media(&record))?;
            Ok(record)
        });
        if result.is_err() {
            // A failed abort leaves the intent open: reconciliation resolves
            // it at the next startup.
            let _ = self.abort(&meta.intent);
        }
        result
    }

    /// Verifies a managed copy before use: opened with `FILE_SHARE_READ` only
    /// without following links, SHA-256 and size recomputed through the
    /// handle, read-only attribute restored if missing. Missing copy: media
    /// marked `missing` and [`StorageError::NotFound`].
    pub fn verify_media(&mut self, id: &MediaId) -> Result<MediaCheck> {
        let record = self.read(|tx| tx.media(id))?;
        if record.status != MediaStatus::Approved {
            return Err(StorageError::StateConflict(format!(
                "media {id} not approved ({})",
                record.status
            )));
        }
        let abs = self.layout.resolve(&record.managed_path)?;
        if !crate::catalog::exists_no_follow(&abs)? {
            let id_s = id.to_string();
            self.with_tx(|tx| {
                tx.conn()
                    .execute(
                        "UPDATE media SET status = 'missing' WHERE media_id = ?1",
                        rusqlite::params![id_s],
                    )
                    .map_err(crate::error::map_sqlite)?;
                Ok(())
            })?;
            return Err(StorageError::NotFound {
                kind: "managed media copy",
                id: record.managed_path.to_string(),
            });
        }
        let file = fsx::open_regular(&abs, Share::ReadOnly, record.managed_path.as_str())?;
        let (sha, size) = fsx::sha256_handle(&file, &abs)?;
        if sha != record.sha256 || size != record.size_bytes {
            return Err(StorageError::IntegrityMismatch {
                path: record.managed_path.to_string(),
                detail: format!(
                    "expected {} ({} bytes), found {sha} ({size} bytes)",
                    record.sha256, record.size_bytes
                ),
            });
        }
        let readonly_restored = fsx::ensure_readonly(&abs)?;
        drop(file);
        Ok(MediaCheck {
            record,
            readonly_restored,
        })
    }
}

/// Managed media copy held open for a VM start (ADR-0008, ADR-0025).
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
        let file = fsx::open_regular(&abs, Share::ReadOnly, rel.as_str())?;
        let size = file
            .metadata()
            .map_err(|e| StorageError::io(&abs, e))?
            .len();
        Ok(Self { file, abs, size })
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
