//! `lightvm-service --move-catalog`: move of the whole catalog to another
//! folder, run by the UI as a child process with the supervisor stopped
//! (the storage stays in this binary, ADR-0024).
//!
//! Contract with the parent:
//! - arguments: `--move-catalog --catalog <current root> --to <new root>`
//!   and optionally `--keep-source`;
//! - stdout: JSON lines [`MoveEvent`]: `move_progress` (at most every
//!   [`PROGRESS_INTERVAL`] and at every phase change), then one
//!   `move_done` or `move_failed`;
//! - stdin: a `shutdown` line or EOF (parent gone) cancels the move: the
//!   partial destination is removed and the catalog stays where it was;
//! - exit codes: 0 moved, 3 catalog in use by another instance (supervisor
//!   running), 7 failed, 8 cancelled, 2 wrong usage.
//!
//! The move itself is [`vm_storage::move_catalog`]: copy verified file by
//! file (or rename on the same volume), destination opened as a catalog,
//! only then the source removed. The runtime files of the supervisor
//! ([`RUNTIME_ENTRIES`]) are not part of the catalog and are not copied.

use serde::{Deserialize, Serialize};

/// Minimum interval between two progress lines of the same phase.
pub const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// Exit code: catalog in use by another instance.
pub const EXIT_LOCKED: u8 = 3;
/// Exit code: move failed, catalog unchanged where it was.
pub const EXIT_FAILED: u8 = 7;
/// Exit code: move cancelled, catalog unchanged where it was.
pub const EXIT_CANCELLED: u8 = 8;

/// Top-level entries of a catalog root written by the supervisor at run
/// time and recreated at the next start: service lock, emergency endpoint,
/// private socket directories (`p`, Unix IPC `.run`).
pub const RUNTIME_ENTRIES: [&str; 4] = [".lightvm-service.lock", "emergency.endpoint", "p", ".run"];

/// Line written on stdout by `--move-catalog`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MoveEvent {
    /// Progress of the current phase.
    MoveProgress {
        /// `checking`, `copying`, `verifying`, `validating`,
        /// `removing_source`.
        phase: String,
        /// Bytes done in the phase.
        done_bytes: u64,
        /// Bytes of the phase (0 if not measured in bytes).
        total_bytes: u64,
    },
    /// The catalog is at the new location and validated.
    MoveDone {
        /// `renamed` (same volume) or `copied`.
        method: String,
        /// Files of the catalog.
        files: u64,
        /// Bytes of the catalog.
        bytes: u64,
        /// VMs in the moved catalog.
        vms: u64,
        /// The old folder no longer exists.
        source_removed: bool,
        /// Removal of the old folder failed partway (the move succeeded).
        source_cleanup_error: Option<String>,
    },
    /// Nothing changed: the catalog is still where it was.
    MoveFailed {
        /// Readable cause.
        message: String,
        /// Cancelled on request.
        cancelled: bool,
    },
}

/// Runs the move, writing the events on `out`; returns the exit code.
#[cfg(feature = "supervisor")]
pub fn run(
    from: &std::path::Path,
    to: &std::path::Path,
    keep_source: bool,
    out: &mut dyn std::io::Write,
    cancel: &std::sync::atomic::AtomicBool,
) -> u8 {
    use std::sync::atomic::Ordering;
    let out = std::cell::RefCell::new(out);
    let emit = |e: &MoveEvent| {
        let mut o = out.borrow_mut();
        if let Ok(line) = serde_json::to_string(e) {
            let _ = writeln!(o, "{line}").and_then(|()| o.flush());
        }
    };
    let last = std::cell::Cell::new(None::<(vm_storage::MovePhase, std::time::Instant)>);
    let progress = |p: vm_storage::MoveProgress| {
        let now = std::time::Instant::now();
        let due = match last.get() {
            Some((phase, at)) => {
                phase != p.phase
                    || now.duration_since(at) >= PROGRESS_INTERVAL
                    || p.done_bytes == p.total_bytes
            }
            None => true,
        };
        if due {
            last.set(Some((p.phase, now)));
            emit(&MoveEvent::MoveProgress {
                phase: p.phase.as_str().into(),
                done_bytes: p.done_bytes,
                total_bytes: p.total_bytes,
            });
        }
    };
    let req = vm_storage::MoveRequest {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        keep_source,
        leave_behind: RUNTIME_ENTRIES.iter().map(|s| (*s).to_owned()).collect(),
        fault: None,
    };
    match vm_storage::move_catalog(&req, &free_bytes, &progress, &|| {
        cancel.load(Ordering::SeqCst)
    }) {
        Ok(r) => {
            emit(&MoveEvent::MoveDone {
                method: match r.method {
                    vm_storage::MoveMethod::Renamed => "renamed",
                    vm_storage::MoveMethod::Copied => "copied",
                }
                .into(),
                files: r.files,
                bytes: r.bytes,
                vms: r.vms,
                source_removed: r.source_removed,
                source_cleanup_error: r.source_cleanup_error,
            });
            0
        }
        Err(e) => {
            let cancelled = vm_storage::is_move_cancelled(&e);
            let message = match &e {
                vm_storage::StorageError::CatalogLocked(_) => {
                    "the catalog is in use by a running supervisor: stop it first".to_owned()
                }
                _ => e.to_string(),
            };
            emit(&MoveEvent::MoveFailed { message, cancelled });
            match e {
                _ if cancelled => EXIT_CANCELLED,
                vm_storage::StorageError::CatalogLocked(_) => EXIT_LOCKED,
                _ => EXIT_FAILED,
            }
        }
    }
}

/// Free space on the volume of `dir` (same measurement as the host
/// resources of the backend).
#[cfg(feature = "supervisor")]
fn free_bytes(dir: &std::path::Path) -> std::io::Result<u64> {
    qemu_backend::ManagedPath::verify(dir.to_path_buf())
        .and_then(|p| qemu_backend::prepare::host_resources(&p))
        .map(|r| r.free_disk_bytes)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(all(test, feature = "supervisor"))]
mod tests {
    use super::*;

    fn lines(buf: &[u8]) -> Vec<MoveEvent> {
        String::from_utf8_lossy(buf)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn moves_and_reports_as_json_lines() {
        let base = tempfile_dir();
        let from = base.join("catalog");
        drop(vm_storage::Catalog::open(&from).unwrap());
        std::fs::write(from.join("emergency.endpoint"), b"stale").unwrap();
        let to = base.join("moved");
        let mut out = Vec::new();
        let code = run(&from, &to, true, &mut out, &Default::default());
        let events = lines(&out);
        assert_eq!(code, 0, "{events:?}");
        assert!(matches!(
            events.last(),
            Some(MoveEvent::MoveDone { method, source_removed: false, .. }) if method == "copied"
        ));
        assert!(
            matches!(events.first(), Some(MoveEvent::MoveProgress { phase, .. }) if phase == "checking")
        );
        assert!(!to.join("emergency.endpoint").exists());
        assert!(from.join("emergency.endpoint").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cancel_and_failure_have_their_exit_codes() {
        let base = tempfile_dir();
        let from = base.join("catalog");
        drop(vm_storage::Catalog::open(&from).unwrap());
        let mut out = Vec::new();
        let cancel = std::sync::atomic::AtomicBool::new(true);
        assert_eq!(
            run(&from, &base.join("a"), true, &mut out, &cancel),
            EXIT_CANCELLED
        );
        assert!(matches!(
            lines(&out).last(),
            Some(MoveEvent::MoveFailed {
                cancelled: true,
                ..
            })
        ));
        let mut out = Vec::new();
        assert_eq!(
            run(
                &from,
                &from.join("inside"),
                false,
                &mut out,
                &Default::default()
            ),
            EXIT_FAILED
        );
        let held = vm_storage::Catalog::open(&from).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            run(&from, &base.join("b"), false, &mut out, &Default::default()),
            EXIT_LOCKED
        );
        drop(held);
        assert!(!base.join("a").exists() && !base.join("b").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "lightvm-move-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
