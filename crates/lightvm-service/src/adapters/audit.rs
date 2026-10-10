//! File audit sink (F1-10): one JSON line per event
//! ([`lightvm_proto::AuditEvent`]), append-only, in the user profile.
//!
//! Events arrive from the core already stripped of sensitive payloads (no
//! typed text, tokens, host paths or guest content). Each line is written
//! before returning: if the write fails the core counts the error and, for
//! events that precede an effect, rejects the operation. Every line is also
//! synced to disk (`sync_data`) except the `vm_input` lines (three per
//! console input batch: accepted, forwarded, outcome): they are written to
//! the file like the others and reach the disk with the next synced line
//! (any lifecycle, configuration or denial event), so typing in the console
//! does not cost three disk flushes per key (F2-01 measurements). A crash of
//! the service loses nothing (the lines are in the OS cache); a power loss
//! may lose the last input lines, never a lifecycle line.
//!
//! The `seq` number restarts from 1 at every supervisor start: lines from
//! different starts are told apart by `at` (each line is a full `AuditEvent`).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use lightvm_proto::{AuditAction, AuditEvent, PublicError};
use vm_core::ports::AuditSink;

/// Name of the audit file in the user's state directory.
pub const AUDIT_FILE: &str = "audit.jsonl";

/// Default state directory in the user profile:
/// `%LOCALAPPDATA%\LightVM` on Windows, `$XDG_STATE_HOME/lightvm` (or
/// `~/.local/state/lightvm`) on Unix.
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

/// Audit on an append-only JSON Lines file.
#[derive(Debug)]
pub struct FileAuditSink {
    file: File,
    path: PathBuf,
}

impl FileAuditSink {
    /// Opens (or creates) the file append-only; creates the directory if missing.
    /// On Unix a new file is created with `0600` permissions.
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

    /// Path of the file (local diagnostics; never sent to clients).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AuditSink for FileAuditSink {
    fn append(&mut self, event: &AuditEvent) -> Result<(), PublicError> {
        let line = serde_json::to_string(event)
            .map_err(|_| PublicError::infrastructure("audit: event not serializable"))?;
        writeln!(self.file, "{line}")
            .and_then(|()| {
                if event.action == AuditAction::VmInput {
                    Ok(())
                } else {
                    self.file.sync_data()
                }
            })
            .map_err(|_| PublicError::infrastructure("audit: write failed"))
    }
}
