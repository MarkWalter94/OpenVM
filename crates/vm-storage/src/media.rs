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

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, IntentSpec};
use crate::enums::{IntentKind, MediaKind, MediaStatus, TargetKind};
use crate::error::{Result, StorageError};
use crate::fsx::{self, Share};
use crate::ids::MediaId;
use crate::layout::CatalogLayout;
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
        if declared_origin.len() > MAX_DECLARED_ORIGIN
            || declared_origin.chars().any(char::is_control)
        {
            return Err(StorageError::Constraint(
                "declared origin too long or contains control characters".into(),
            ));
        }
        let label = "media source";
        let mut src = fsx::open_regular(source, Share::ReadOnly, label)?;
        let expected_len = src
            .metadata()
            .map_err(|e| StorageError::io(source, e))?
            .len();
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
        let result = (|| -> Result<MediaRecord> {
            let (sha256, size) = {
                let mut dst = fsx::create_new_exclusive(&intent.temp_path)?;
                let mut h = Sha256::new();
                let mut buf = vec![0u8; 1 << 16];
                let mut total = 0u64;
                loop {
                    let n = src
                        .read(&mut buf)
                        .map_err(|e| StorageError::io(source, e))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                    dst.write_all(&buf[..n])
                        .map_err(|e| StorageError::io(&intent.temp_path, e))?;
                    total = total.saturating_add(n as u64);
                }
                dst.sync_all()
                    .map_err(|e| StorageError::io(&intent.temp_path, e))?;
                (crate::hex(&h.finalize()), total)
            };
            if size != expected_len {
                return Err(StorageError::IntegrityMismatch {
                    path: label.into(),
                    detail: format!("read {size} bytes, expected {expected_len}"),
                });
            }
            drop(src);
            self.mark_applied(&intent)?;
            let record = MediaRecord {
                media_id,
                sha256,
                size_bytes: size,
                kind,
                declared_origin: declared_origin.to_owned(),
                approved_at: now_rfc3339(),
                managed_path: final_rel.clone(),
                status: MediaStatus::Approved,
            };
            self.commit(&intent, |tx| tx.insert_media(&record))?;
            Ok(record)
        })();
        if result.is_err() {
            // A failed abort leaves the intent open: reconciliation resolves
            // it at the next startup.
            let _ = self.abort(&intent);
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
