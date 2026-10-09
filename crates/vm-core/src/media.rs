//! Registro media approvato come modello (ROADMAP 6.3; ADR-0008).
//!
//! Un percorso selezionato non autorizza qualunque file lo sostituisca: prima
//! di ogni uso il core confronta hash, dimensione e identità OS osservati sul
//! file aperto con quelli registrati all'approvazione. Il backend accetta solo
//! [`VerifiedMedia`], costruibile esclusivamente da [`verify_media`].
//!
//! Limite: la verifica vale per il file ispezionato. Perché copra anche l'uso,
//! l'ispezione deve avvenire sullo stesso handle consegnato a QEMU (o su un file
//! in directory controllata e in sola lettura): responsabilità di storage e backend.

use lightvm_proto::{
    FileIdentity, MediaEntry, MediaKind, MediaRecord, PublicError, Sha256Digest, UtcTimestamp,
};

/// Fatti osservati sulla copia gestita, calcolati sul file aperto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedMediaFacts {
    /// SHA-256 del contenuto.
    pub sha256: Sha256Digest,
    /// Dimensione in byte.
    pub size_bytes: u64,
    /// Identità OS del file aperto, se disponibile.
    pub file_identity: Option<FileIdentity>,
    /// `true` se il file è aperto/marcato in sola lettura.
    pub read_only: bool,
}

/// Prova di verifica di un supporto: campi privati, costruibile solo da [`verify_media`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMedia {
    entry: MediaEntry,
    verified_at: UtcTimestamp,
}

impl VerifiedMedia {
    /// Voce verificata.
    #[must_use]
    pub fn entry(&self) -> &MediaEntry {
        &self.entry
    }
    /// Dati pubblici.
    #[must_use]
    pub fn record(&self) -> &MediaRecord {
        &self.entry.record
    }
    /// Istante della verifica.
    #[must_use]
    pub fn verified_at(&self) -> UtcTimestamp {
        self.verified_at
    }
}

fn mismatch(what: &'static str) -> PublicError {
    PublicError::permission_denied("supporto diverso da quello approvato")
        .with_detail("reason", "media_identity_mismatch")
        .with_detail("field", what)
}

/// Confronta i fatti osservati con il registro. Qualunque differenza (hash,
/// dimensione, identità OS registrata, scrivibilità) nega l'uso.
pub fn verify_media(
    entry: &MediaEntry,
    observed: &ObservedMediaFacts,
    now: UtcTimestamp,
) -> Result<VerifiedMedia, PublicError> {
    if observed.size_bytes != entry.record.size_bytes {
        return Err(mismatch("size_bytes"));
    }
    if observed.sha256 != entry.record.sha256 {
        return Err(mismatch("sha256"));
    }
    if let Some(expected) = entry.file_identity
        && observed.file_identity != Some(expected)
    {
        return Err(mismatch("file_identity"));
    }
    if !observed.read_only {
        return Err(mismatch("read_only"));
    }
    Ok(VerifiedMedia {
        entry: entry.clone(),
        verified_at: now,
    })
}

/// Verifica che un supporto sia del tipo richiesto dal ruolo nella configurazione.
pub fn check_media_kind(entry: &MediaEntry, expected: MediaKind) -> Result<(), PublicError> {
    if entry.record.kind == expected {
        Ok(())
    } else {
        Err(
            PublicError::invalid_argument("tipo di supporto non adatto al ruolo richiesto")
                .with_detail("media_id", entry.record.id.to_string()),
        )
    }
}
