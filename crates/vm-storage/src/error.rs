//! Errori del catalogo.
//!
//! Gli errori sono specifici dello storage; la traduzione negli errori pubblici
//! stabili (`vm_busy`, `state_conflict`, `resource_limit`, ...) avviene nel
//! supervisor tramite [`StorageError::public_code`].

use std::path::PathBuf;

/// Risultato con [`StorageError`].
pub type Result<T> = std::result::Result<T, StorageError>;

/// Motivo per cui un percorso gestito è stato rifiutato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRejection {
    /// Stringa vuota.
    Empty,
    /// Percorso assoluto o con radice (`/x`, `\x`, `C:\x`, `C:x`).
    Absolute,
    /// Percorso UNC (`\\server\share`) o device path (`\\?\`, `\\.\`).
    UncOrDevice,
    /// Componente `..` o `.`.
    Traversal,
    /// Nome riservato di Windows (`CON`, `NUL`, `COM1`, ...).
    ReservedName,
    /// Carattere non ammesso (controllo, `<>:"|?*`, barra rovesciata).
    InvalidCharacter,
    /// Componente che termina con punto o spazio (normalizzato da Windows).
    TrailingDotOrSpace,
    /// Percorso troppo lungo.
    TooLong,
    /// Un componente esistente è un collegamento simbolico o un reparse point
    /// (junction inclusa).
    Link,
    /// Il percorso non si trova sotto la directory attesa (per esempio `tmp/`).
    OutsideArea,
}

/// Errore dello storage.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Errore SQLite.
    #[error("errore SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Errore di I/O su un percorso gestito.
    #[error("errore di I/O su {path}: {source}")]
    Io {
        /// Percorso coinvolto.
        path: PathBuf,
        /// Errore originale.
        source: std::io::Error,
    },
    /// Errore di serializzazione JSON.
    #[error("errore JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Un'altra istanza tiene già il lock del catalogo.
    #[error("catalogo già in uso da un'altra istanza: {0}")]
    CatalogLocked(PathBuf),
    /// Il database ha una versione di schema più recente di quella supportata.
    #[error(
        "schema del catalogo v{found} più recente della versione supportata v{supported}: downgrade non ammesso"
    )]
    SchemaTooNew {
        /// Versione trovata nel database.
        found: u32,
        /// Versione massima nota a questo binario.
        supported: u32,
    },
    /// Una migrazione già applicata non corrisponde a quella incorporata.
    #[error("migrazione v{version} applicata con contenuto diverso da quello incorporato")]
    MigrationMismatch {
        /// Versione divergente.
        version: u32,
    },
    /// Percorso gestito rifiutato.
    #[error("percorso gestito rifiutato ({reason:?}): {path}")]
    InvalidPath {
        /// Percorso proposto (mai letto né aperto).
        path: String,
        /// Motivo.
        reason: PathRejection,
    },
    /// Cancellazione impedita da un riferimento (baseline, disco, media).
    #[error("oggetto referenziato, operazione impedita: {0}")]
    Referenced(&'static str),
    /// Violazione di una regola del catalogo applicata dal database.
    #[error("vincolo del catalogo violato: {0}")]
    Constraint(String),
    /// Oggetto non trovato.
    #[error("{kind} non trovato: {id}")]
    NotFound {
        /// Tipo di oggetto.
        kind: &'static str,
        /// Identificativo.
        id: String,
    },
    /// Un'altra operazione con journal è in corso sulla stessa VM.
    #[error("operazione in corso sulla VM {0}")]
    VmBusy(String),
    /// Transizione non ammessa dallo stato corrente.
    #[error("stato non compatibile: {0}")]
    StateConflict(String),
    /// Budget storage o riserva host superati.
    #[error("limite di storage: {0}")]
    ResourceLimit(String),
    /// Capacità richiesta non disponibile (per esempio quota rigida OS).
    #[error("non supportato: {0}")]
    Unsupported(&'static str),
}

impl StorageError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid_path(path: impl Into<String>, reason: PathRejection) -> Self {
        Self::InvalidPath {
            path: path.into(),
            reason,
        }
    }

    /// Codice pubblico stabile (ROADMAP 6.3) più vicino, se esiste.
    /// Gli errori interni (I/O, SQLite, migrazioni) non hanno un codice pubblico.
    pub fn public_code(&self) -> Option<&'static str> {
        match self {
            Self::VmBusy(_) | Self::CatalogLocked(_) => Some("vm_busy"),
            Self::StateConflict(_) | Self::Referenced(_) | Self::Constraint(_) => {
                Some("state_conflict")
            }
            Self::ResourceLimit(_) => Some("resource_limit"),
            Self::Unsupported(_) => Some("unsupported"),
            Self::InvalidPath { .. } => Some("permission_denied"),
            _ => None,
        }
    }
}

/// Converte gli errori `RAISE(ABORT, 'lightvm:...')` dei trigger in errori nominati.
pub(crate) fn map_sqlite(err: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::SqliteFailure(code, Some(msg)) = &err {
        if let Some(tag) = msg.strip_prefix("lightvm:") {
            return match tag {
                "baseline_referenced" => StorageError::Referenced("baseline_referenced"),
                "disk_referenced" => StorageError::Referenced("disk_referenced"),
                "media_referenced" => StorageError::Referenced("media_referenced"),
                "baseline_protected" => StorageError::Referenced("baseline_protected"),
                other => StorageError::Constraint(other.to_owned()),
            };
        }
        if code.code == rusqlite::ErrorCode::ConstraintViolation {
            return StorageError::Constraint(msg.clone());
        }
    }
    StorageError::Sqlite(err)
}

/// Estensione per mappare i risultati rusqlite con [`map_sqlite`].
pub(crate) trait SqlResultExt<T> {
    fn db(self) -> Result<T>;
}

impl<T> SqlResultExt<T> for std::result::Result<T, rusqlite::Error> {
    fn db(self) -> Result<T> {
        self.map_err(map_sqlite)
    }
}
