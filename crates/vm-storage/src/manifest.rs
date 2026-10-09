//! Manifest JSON versionati per VM e baseline, accanto ai dati.
//!
//! Il manifest è una copia leggibile e autonoma dei metadati essenziali; il suo
//! SHA-256 (dei byte scritti) è registrato nel DB. [`Divergence`] elenca le
//! differenze rilevate tra file e DB. Il manifest non è una prova forense: è
//! modificabile dallo stesso utente (ROADMAP sezione 9).

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::compat::{BaselineId, MediaId, VmId};
use crate::enums::{DiskRole, MediaSlot};
use crate::error::{Result, StorageError};
use crate::layout::CatalogLayout;
use crate::paths::ManagedPath;

/// Versione del formato dei manifest scritti da questo binario.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// File gestito citato da un manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Ruolo (`primary`, `base`, `overlay`, `vars`).
    pub role: String,
    /// Percorso relativo alla radice del catalogo.
    pub path: String,
    /// SHA-256, se noto.
    pub sha256: Option<String>,
}

/// Media collegato citato dal manifest della VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestMedia {
    /// Slot.
    pub slot: MediaSlot,
    /// Media.
    pub media_id: MediaId,
    /// SHA-256 della copia gestita.
    pub sha256: String,
}

/// Manifest di una VM (`vms/<vm_id>/manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmManifest {
    /// Versione del formato.
    pub format_version: u32,
    /// Sempre `"vm"`.
    pub kind: String,
    /// VM.
    pub vm_id: VmId,
    /// Nome.
    pub name: String,
    /// Sistema operativo guest.
    pub guest_os: String,
    /// Versione dello schema di configurazione.
    pub config_version: u32,
    /// Revisione della configurazione.
    pub config_revision: u64,
    /// Configurazione.
    pub config: serde_json::Value,
    /// Dischi non di baseline (primario e overlay), ordinati per percorso.
    pub disks: Vec<ManifestFile>,
    /// Media collegati.
    pub media: Vec<ManifestMedia>,
    /// Baseline della VM, ordinate.
    pub baselines: Vec<BaselineId>,
}

/// Manifest di una baseline (`vms/<vm_id>/base/<baseline_id>/manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineManifest {
    /// Versione del formato.
    pub format_version: u32,
    /// Sempre `"baseline"`.
    pub kind: String,
    /// Baseline.
    pub baseline_id: BaselineId,
    /// VM.
    pub vm_id: VmId,
    /// Etichetta.
    pub label: String,
    /// Revisione della configurazione della VM al momento della baseline.
    pub config_revision: u64,
    /// Configurazione della VM al momento della baseline.
    pub config: serde_json::Value,
    /// Disco base immutabile.
    pub disk: ManifestFile,
    /// Stato firmware della baseline.
    pub vars: Option<ManifestFile>,
    /// Creata a VM spenta (sempre `true`).
    pub created_powered_off: bool,
    /// Arresto pulito osservato.
    pub clean_shutdown: bool,
    /// Creazione (UTC).
    pub created_at: String,
}

/// Divergenza tra manifest su file e catalogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Divergence {
    /// Il DB non registra un hash di manifest.
    HashNotRecorded,
    /// File del manifest assente.
    ManifestMissing,
    /// File presente ma illeggibile o non conforme.
    ManifestUnreadable(String),
    /// SHA-256 del file diverso da quello registrato.
    HashMismatch {
        /// Registrato nel DB.
        recorded: String,
        /// Calcolato sul file.
        actual: String,
    },
    /// Formato più recente di quello supportato.
    UnsupportedFormat(u32),
    /// Campo diverso dal DB.
    FieldMismatch(&'static str),
    /// File di dati citato assente.
    DataFileMissing(String),
    /// SHA-256 di un file di dati diverso (solo verifica profonda).
    DataHashMismatch(String),
}

/// SHA-256 esadecimale di un file, letto a blocchi.
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

/// Serializza in JSON leggibile e scrive in modo atomico: file temporaneo in
/// `tmp/`, `sync_all`, rinomina sulla destinazione. Restituisce lo SHA-256 dei
/// byte scritti.
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

/// Legge un manifest verificandone l'hash rispetto a `recorded`.
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
