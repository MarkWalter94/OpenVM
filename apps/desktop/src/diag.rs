//! Diagnostic log of the app.
//!
//! Release builds use the Windows GUI subsystem: without a console window
//! the diagnostics written to stderr are lost unless the output is
//! redirected. The app therefore also appends them, with a UTC timestamp,
//! to `lightvm-desktop.log`, and points the supervisor's stderr to
//! `lightvm-service.log`, both in the logs folder
//! (`%LOCALAPPDATA%\LightVM\logs` on Windows). Each file is rotated when it
//! exceeds [`MAX_LOG_BYTES`], keeping [`KEPT_LOGS`] older copies
//! (`lightvm-desktop.1.log`, ...).
//!
//! Only diagnostics go there: errors of the supervisor start and of the
//! rendering, never credentials, display tokens, guest contents or typed
//! keys. The logging is enabled only by [`init`] (the executable); the
//! library and its tests write to stderr only.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

/// Size beyond which a log file is rotated.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Rotated copies kept for each log.
pub const KEPT_LOGS: usize = 3;
/// Log of the app.
pub const UI_LOG: &str = "lightvm-desktop.log";
/// Log of the supervisor's stderr.
pub const SERVICE_LOG: &str = "lightvm-service.log";

struct Log {
    dir: PathBuf,
    file: Mutex<Option<(File, u64)>>,
}

static LOG: OnceLock<Log> = OnceLock::new();

/// Default logs folder: `%LOCALAPPDATA%\LightVM\logs` on Windows, the
/// configuration folder of the user elsewhere.
pub fn default_dir() -> Option<PathBuf> {
    crate::settings::profile_dir().map(|d| d.join("logs"))
}

/// Path of the `n`-th rotated copy of `path` (`name.n.log`).
pub fn rotated_name(path: &Path, n: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .map_or_else(|| "log".into(), |s| s.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem}.{n}.log"))
}

/// Rotates `path` if it is larger than `max`: `name.log` becomes
/// `name.1.log`, `name.1.log` becomes `name.2.log`, ... and the copy beyond
/// `keep` is removed.
pub fn rotate(path: &Path, max: u64, keep: usize) -> std::io::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= max {
        return Ok(());
    }
    if keep == 0 {
        return std::fs::remove_file(path);
    }
    let _ = std::fs::remove_file(rotated_name(path, keep));
    for n in (1..keep).rev() {
        let from = rotated_name(path, n);
        if from.exists() {
            std::fs::rename(&from, rotated_name(path, n + 1))?;
        }
    }
    std::fs::rename(path, rotated_name(path, 1))
}

fn open_append(path: &Path) -> std::io::Result<(File, u64)> {
    rotate(path, MAX_LOG_BYTES, KEPT_LOGS)?;
    let f = OpenOptions::new().create(true).append(true).open(path)?;
    let len = f.metadata().map_or(0, |m| m.len());
    Ok((f, len))
}

/// Enables the log files in `dir` (created if missing). Returns the path of
/// the app log. Called once by the executable; later calls do nothing.
pub fn init(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(UI_LOG);
    let opened = open_append(&path)?;
    let _ = LOG.set(Log {
        dir: dir.to_owned(),
        file: Mutex::new(Some(opened)),
    });
    Ok(path)
}

/// Folder of the log files, if enabled.
pub fn dir() -> Option<&'static Path> {
    LOG.get().map(|l| l.dir.as_path())
}

/// Writes a diagnostic line to stderr and, if enabled, to the app log.
/// Never pass secrets (credentials, tokens) or guest contents.
pub fn log(message: &str) {
    eprintln!("{message}");
    let Some(l) = LOG.get() else { return };
    let mut guard = l
        .file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = l.dir.join(UI_LOG);
    if guard.as_ref().is_some_and(|(_, n)| *n > MAX_LOG_BYTES) {
        // Rotation while running: the file is closed, rotated and reopened.
        *guard = None;
        *guard = open_append(&path).ok();
    }
    if let Some((f, n)) = guard.as_mut() {
        let line = format!("{} {message}\n", lightvm_proto::UtcTimestamp::now());
        if f.write_all(line.as_bytes()).is_ok() {
            *n += line.len() as u64;
        }
    }
}

/// stderr for the supervisor: its log file (rotated), if the logs are
/// enabled.
pub fn service_stderr() -> Option<Stdio> {
    let dir = dir()?;
    open_append(&dir.join(SERVICE_LOG))
        .ok()
        .map(|(f, _)| Stdio::from(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_the_newest_copies() {
        let dir = std::env::temp_dir().join(format!(
            "lightvm-diag-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join(UI_LOG);
        // Under the limit: nothing happens.
        std::fs::write(&log, b"small").unwrap();
        rotate(&log, 10, 2).unwrap();
        assert!(log.exists());
        assert!(!rotated_name(&log, 1).exists());
        // Over the limit three times: two copies kept, the oldest dropped.
        for generation in ["first-generation", "second-generation", "third-generation"] {
            std::fs::write(&log, generation).unwrap();
            rotate(&log, 10, 2).unwrap();
            assert!(!log.exists());
        }
        assert_eq!(
            std::fs::read_to_string(rotated_name(&log, 1)).unwrap(),
            "third-generation"
        );
        assert_eq!(
            std::fs::read_to_string(rotated_name(&log, 2)).unwrap(),
            "second-generation"
        );
        assert!(!rotated_name(&log, 3).exists());
        assert_eq!(
            rotated_name(&log, 1).file_name().unwrap(),
            "lightvm-desktop.1.log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
