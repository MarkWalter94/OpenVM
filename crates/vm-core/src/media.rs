//! Approved media registry as a model (ROADMAP 6.3; ADR-0008, ADR-0029).
//!
//! A selected path does not authorize whatever file replaces it: before
//! each use the core compares the hash, size and OS identity observed on the
//! open file with those recorded at approval time. The backend accepts only
//! [`VerifiedMedia`], which can be built exclusively by [`verify_media`].
//!
//! Limitation: the check holds for the inspected file. For it to cover use too,
//! the inspection must happen on the same handle handed to QEMU (or on a file
//! in a controlled, read-only directory): a responsibility of storage and backend.

use lightvm_proto::{
    FileIdentity, MediaEntry, MediaKind, MediaLocation, MediaRecord, PublicError, Sha256Digest,
    UtcTimestamp,
};

/// Facts observed on the file of an approved medium, computed on the open
/// file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedMediaFacts {
    /// SHA-256 of the content.
    pub sha256: Sha256Digest,
    /// Size in bytes.
    pub size_bytes: u64,
    /// OS identity of the open file, if available.
    pub file_identity: Option<FileIdentity>,
    /// `true` if the file is open/marked read-only.
    pub read_only: bool,
}

/// Proof that a medium was verified: private fields, built only by [`verify_media`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMedia {
    entry: MediaEntry,
    verified_at: UtcTimestamp,
}

impl VerifiedMedia {
    /// Verified entry.
    #[must_use]
    pub fn entry(&self) -> &MediaEntry {
        &self.entry
    }
    /// Public data.
    #[must_use]
    pub fn record(&self) -> &MediaRecord {
        &self.entry.record
    }
    /// Instant of the verification.
    #[must_use]
    pub fn verified_at(&self) -> UtcTimestamp {
        self.verified_at
    }
}

fn mismatch(entry: &MediaEntry, what: &'static str) -> PublicError {
    let message = match entry.location.file_name() {
        // ADR-0029: the user's own file can be changed between runs; the
        // remedy is to select it again (the name only, never the path).
        Some(name) => format!(
            "the ISO file \u{201c}{name}\u{201d} changed since it was selected: import it again to use it"
        ),
        None => "medium differs from the approved one".to_owned(),
    };
    PublicError::permission_denied(message)
        .with_detail("reason", "media_identity_mismatch")
        .with_detail("field", what)
}

/// Compares the observed facts with the registry. Any difference (hash,
/// size, recorded OS identity) denies use; so does a managed copy that
/// lost its read-only attribute. A file used in place (ADR-0029) belongs to
/// the user and is normally writable: what keeps it unchanged during the
/// run is the guard handle the facts were read through (share mode), so
/// its writability is not a reason to refuse.
pub fn verify_media(
    entry: &MediaEntry,
    observed: &ObservedMediaFacts,
    now: UtcTimestamp,
) -> Result<VerifiedMedia, PublicError> {
    if observed.size_bytes != entry.record.size_bytes {
        return Err(mismatch(entry, "size_bytes"));
    }
    if observed.sha256 != entry.record.sha256 {
        return Err(mismatch(entry, "sha256"));
    }
    if let Some(expected) = entry.file_identity
        && observed.file_identity != Some(expected)
    {
        return Err(mismatch(entry, "file_identity"));
    }
    if matches!(entry.location, MediaLocation::Managed(_)) && !observed.read_only {
        return Err(mismatch(entry, "read_only"));
    }
    Ok(VerifiedMedia {
        entry: entry.clone(),
        verified_at: now,
    })
}

/// Checks that a medium is of the kind required by its role in the configuration.
pub fn check_media_kind(entry: &MediaEntry, expected: MediaKind) -> Result<(), PublicError> {
    if entry.record.kind == expected {
        Ok(())
    } else {
        Err(
            PublicError::invalid_argument("media kind not suitable for the requested role")
                .with_detail("media_id", entry.record.id.to_string()),
        )
    }
}
