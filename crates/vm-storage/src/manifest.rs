//! Versioned JSON manifests for VMs and baselines, stored next to the data.
//!
//! The manifest is a readable, self-contained copy of the essential metadata;
//! its SHA-256 (of the bytes written) is recorded in the DB. [`Divergence`]
//! lists the differences detected between file and DB. The manifest is not
//! forensic evidence: the same user can modify it (ROADMAP section 9).

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::enums::{DiskRole, MediaSlot};
use crate::error::{Result, StorageError};
use crate::ids::{BaselineId, MediaId, VmId};
use crate::layout::CatalogLayout;
use crate::paths::ManagedPath;

/// Format version of the manifests written by this binary.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// Managed file referenced by a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Role (`primary`, `base`, `overlay`, `vars`).
    pub role: String,
    /// Path relative to the catalog root.
    pub path: String,
    /// SHA-256, if known.
    pub sha256: Option<String>,
}

/// Attached media referenced by the VM manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestMedia {
    /// Slot.
    pub slot: MediaSlot,
    /// Media.
    pub media_id: MediaId,
    /// SHA-256 of the managed copy.
    pub sha256: String,
}

/// Manifest of a VM (`vms/<vm_id>/manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmManifest {
    /// Versione del formato.
    pub format_version: u32,
    /// Always `"vm"`.
    pub kind: String,
    /// VM.
    pub vm_id: VmId,
    /// Name.
    pub name: String,
    /// Guest operating system.
    pub guest_os: String,
    /// Configuration schema version.
    pub config_version: u32,
    /// Configuration revision.
    pub config_revision: u64,
    /// Configuration.
    pub config: serde_json::Value,
    /// Non-baseline disks (primary and overlays), sorted by path.
    pub disks: Vec<ManifestFile>,
    /// Attached media.
    pub media: Vec<ManifestMedia>,
    /// Baselines of the VM, sorted.
    pub baselines: Vec<BaselineId>,
}

/// Manifest of a baseline (`vms/<vm_id>/base/<baseline_id>/manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineManifest {
    /// Versione del formato.
    pub format_version: u32,
    /// Always `"baseline"`.
    pub kind: String,
    /// Baseline.
    pub baseline_id: BaselineId,
    /// VM.
    pub vm_id: VmId,
    /// Label.
    pub label: String,
    /// VM configuration revision at the time of the baseline.
    pub config_revision: u64,
    /// VM configuration at the time of the baseline.
    pub config: serde_json::Value,
    /// Immutable base disk.
    pub disk: ManifestFile,
    /// Firmware state of the baseline.
    pub vars: Option<ManifestFile>,
    /// Created with the VM powered off (always `true`).
    pub created_powered_off: bool,
    /// Clean shutdown observed.
    pub clean_shutdown: bool,
    /// Creation time (UTC).
    pub created_at: String,
}

/// Divergence between the on-disk manifest and the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// The DB does not record a manifest hash.
    HashNotRecorded,
    /// Manifest file missing.
    ManifestMissing,
    /// File present but unreadable or non-conforming.
    ManifestUnreadable(String),
    /// File SHA-256 differs from the recorded one.
    HashMismatch {
        /// Recorded in the DB.
        recorded: String,
        /// Computed on the file.
        actual: String,
    },
    /// Format newer than the supported one.
    UnsupportedFormat(u32),
    /// Field differs from the DB.
    FieldMismatch(&'static str),
    /// Referenced data file missing.
    DataFileMissing(String),
    /// SHA-256 of a data file differs (deep verification only).
    DataHashMismatch(String),
}

/// Hex SHA-256 of a file, read in chunks.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).map_err(|e| StorageError::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| StorageError::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::hex(&h.finalize()))
}

/// Serializes to human-readable JSON and writes atomically: temporary file in
/// `tmp/`, `sync_all`, rename onto the destination. Returns the SHA-256 of the
/// bytes written.
pub(crate) fn write_atomic<T: Serialize>(
    layout: &CatalogLayout,
    rel: &ManagedPath,
    value: &T,
) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let sha = crate::hex(&Sha256::digest(&bytes));
    let tmp_rel = ManagedPath::parse(&format!("tmp/manifest-{}.json", uuid::Uuid::now_v7()))?;
    let tmp = layout.resolve(&tmp_rel)?;
    let dest = layout.resolve(rel)?;
    if let Some((parent, _)) = rel.as_str().rsplit_once('/') {
        layout.ensure_dir(&ManagedPath::parse(parent)?)?;
    }
    {
        let mut f = std::fs::File::create_new(&tmp).map_err(|e| StorageError::io(&tmp, e))?;
        f.write_all(&bytes).map_err(|e| StorageError::io(&tmp, e))?;
        f.sync_all().map_err(|e| StorageError::io(&tmp, e))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(StorageError::io(&dest, e));
    }
    Ok(sha)
}

/// Reads a manifest, checking its hash against `recorded`.
pub(crate) fn read_checked<T: for<'de> Deserialize<'de>>(
    layout: &CatalogLayout,
    rel: &ManagedPath,
    recorded: Option<&str>,
    out: &mut Vec<Divergence>,
) -> Result<Option<T>> {
    let path = layout.resolve(rel)?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push(Divergence::ManifestMissing);
            return Ok(None);
        }
        Err(e) => return Err(StorageError::io(&path, e)),
    };
    let actual = crate::hex(&Sha256::digest(&bytes));
    match recorded {
        None => out.push(Divergence::HashNotRecorded),
        Some(r) if r != actual => out.push(Divergence::HashMismatch {
            recorded: r.to_owned(),
            actual,
        }),
        Some(_) => {}
    }
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            out.push(Divergence::ManifestUnreadable(e.to_string()));
            Ok(None)
        }
    }
}

pub(crate) fn role_name(role: DiskRole) -> String {
    role.as_str().to_owned()
}
