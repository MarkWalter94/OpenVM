//! Destinazione dell'audit su file (F1-10): una riga JSON per evento
//! ([`lightvm_proto::AuditEvent`]), in sola aggiunta, nel profilo utente.
//!
//! Gli eventi arrivano dal core già privi di payload sensibili (nessun testo
//! digitato, token, percorso host o contenuto guest). Ogni riga viene scritta e
//! sincronizzata prima di restituire: se la scrittura fallisce il core conta
//! l'errore e, per gli eventi che precedono un effetto, rifiuta l'operazione.
//!
//! Il numero `seq` riparte da 1 a ogni avvio del supervisor: le righe di avvii
//! diversi si distinguono per `at` (ogni riga è un `AuditEvent` completo).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use lightvm_proto::{AuditEvent, PublicError};
use vm_core::ports::AuditSink;

/// Nome del file di audit nella directory di stato dell'utente.
pub const AUDIT_FILE: &str = "audit.jsonl";

/// Directory di stato predefinita nel profilo utente:
/// `%LOCALAPPDATA%\LightVM` su Windows, `$XDG_STATE_HOME/lightvm` (o
/// `~/.local/state/lightvm`) su Unix.
pub fn default_state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("LightVM"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .map(|d| d.join("lightvm"))
    }
}

/// Audit su file JSON Lines in sola aggiunta.
#[derive(Debug)]
pub struct FileAuditSink {
    file: File,
    path: PathBuf,
}

impl FileAuditSink {
    /// Apre (o crea) il file in sola aggiunta; crea la directory se manca.
    /// Su Unix il file nuovo nasce con permessi `0600`.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut opts = OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Percorso del file (diagnostica locale; mai inviato ai client).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AuditSink for FileAuditSink {
    fn append(&mut self, event: &AuditEvent) -> Result<(), PublicError> {
        let line = serde_json::to_string(event)
            .map_err(|_| PublicError::infrastructure("audit: evento non serializzabile"))?;
        writeln!(self.file, "{line}")
            .and_then(|()| self.file.sync_data())
            .map_err(|_| PublicError::infrastructure("audit: scrittura non riuscita"))
    }
}
