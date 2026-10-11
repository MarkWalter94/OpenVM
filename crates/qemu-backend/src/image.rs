//! Disk image operations for baselines (F3; ROADMAP 8, ADR-0005, ADR-0025):
//! standalone copy of a disk (`qemu-img convert`, the backing chain is
//! flattened), content comparison (`qemu-img compare`), consistency check
//! (`qemu-img check`) and creation of an overlay with a fixed relative
//! backing reference (`qemu-img create -u -b <ref> -F qcow2`).
//!
//! Long commands run in their own Job Object with `KILL_ON_JOB_CLOSE` (as
//! QEMU, F0-06): if the supervisor dies the copy dies with it, and
//! [`ImageCommand::run`] terminates it on cancellation (Emergency Stop,
//! revocation, shutdown). Outputs go to files in a private directory and
//! are removed afterwards. Paths are [`ManagedPath`]s; nothing comes from
//! clients. An interrupted `convert` leaves a file that `qemu-img check`
//! accepts (F0-05 §8.2): the caller writes into a journal temporary and
//! publishes only after `compare` and `check`.

use std::ffi::OsString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lightvm_proto::RunId;

use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::probe::stderr_excerpt;
use crate::process::{EMERGENCY_EXIT_CODE, JobLimits, SpawnRequest, minimal_environment};

/// Maximum duration of a `convert` or `compare` (a 2 TiB disk at a slow
/// rate stays well within it; a hung command is terminated).
pub const LONG_IMAGE_TIMEOUT: Duration = Duration::from_secs(6 * 3600);
/// Maximum duration of `create`, `info`, `check`.
pub const SHORT_IMAGE_TIMEOUT: Duration = Duration::from_secs(600);

/// Outcome of an image command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOutput {
    /// Exit code.
    pub code: Option<u32>,
    /// Standard output (truncated to 1 MiB).
    pub stdout: String,
    /// Standard error (truncated to 64 KiB).
    pub stderr: String,
}

/// `qemu-img` command contained in a Job Object, cancellable.
#[derive(Debug)]
pub struct ImageCommand<'a> {
    /// Approved `qemu-img` (verified by the probe).
    pub qemu_img: &'a ManagedPath,
    /// Private directory for the output files.
    pub log_dir: &'a ManagedPath,
    /// Maximum duration.
    pub timeout: Duration,
}

static SEQ: AtomicU32 = AtomicU32::new(0);

fn run_identity() -> RunId {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut rand = [0u8; 10];
    rand[..4].copy_from_slice(&n.to_be_bytes());
    rand[4..8].copy_from_slice(&std::process::id().to_be_bytes());
    RunId::from_unix_millis(ms, rand)
}

fn read_limited(p: &ManagedPath, max: usize) -> String {
    let bytes = std::fs::read(p.as_path()).unwrap_or_default();
    let end = bytes.len().min(max);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl ImageCommand<'_> {
    /// Runs `qemu-img <args>`; `cancel` is polled every 100 ms: when it
    /// returns `true` the Job Object is terminated and the result is
    /// [`BackendError::StateConflict`] ("cancelled").
    pub fn run(&self, args: &[&str], cancel: &dyn Fn() -> bool) -> Result<ImageOutput> {
        let container = crate::process::new_container(&JobLimits::default())?;
        let id = run_identity();
        let stdout = self.log_dir.join_file(&format!("img-{id}.out"))?;
        let stderr = self.log_dir.join_file(&format!("img-{id}.err"))?;
        let req = SpawnRequest {
            run_id: id,
            program: self.qemu_img.clone(),
            args: args.iter().map(OsString::from).collect(),
            env: minimal_environment(),
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        };
        let cleanup = || {
            let _ = std::fs::remove_file(stdout.as_path());
            let _ = std::fs::remove_file(stderr.as_path());
        };
        let child = match container.spawn(&req) {
            Ok(c) => c,
            Err(e) => {
                cleanup();
                return Err(e);
            }
        };
        let deadline = Instant::now() + self.timeout;
        let code = loop {
            match child.wait(Some(Duration::from_millis(100))) {
                Ok(Some(code)) => break Some(code),
                Ok(None) => {}
                Err(e) => {
                    let _ = container.kill_all(EMERGENCY_EXIT_CODE);
                    let _ = child.wait(Some(Duration::from_secs(10)));
                    cleanup();
                    return Err(e);
                }
            }
            let cancelled = cancel();
            if cancelled || Instant::now() >= deadline {
                let _ = container.kill_all(EMERGENCY_EXIT_CODE);
                let _ = child.wait(Some(Duration::from_secs(30)));
                cleanup();
                return Err(if cancelled {
                    BackendError::StateConflict("image operation cancelled".into())
                } else {
                    BackendError::Timeout("qemu-img")
                });
            }
        };
        let out = ImageOutput {
            code,
            stdout: read_limited(&stdout, 1 << 20),
            stderr: read_limited(&stderr, 64 << 10),
        };
        cleanup();
        Ok(out)
    }
}

/// `true` if a `qemu-img` (or QEMU) error text reports a full volume:
/// `strerror(ENOSPC)` of the C runtime or the Win32 message of
/// `ERROR_DISK_FULL`/`ERROR_HANDLE_DISK_FULL`.
pub fn reports_no_space(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    [
        "no space left on device",
        "not enough space on the disk",
        "disk is full",
    ]
    .iter()
    .any(|p| s.contains(p))
}

fn failed(what: &str, o: &ImageOutput) -> BackendError {
    // F6: a full catalog volume is a clear `resource_limit`, not an opaque
    // I/O error (the partial output file is removed by the caller's job).
    if reports_no_space(&o.stderr) {
        return BackendError::ResourceLimit(format!(
            "not enough free space on the catalog volume (qemu-img {what})"
        ));
    }
    BackendError::Io(std::io::Error::other(format!(
        "qemu-img {what} exited with {:?}: {}",
        o.code,
        stderr_excerpt(&o.stderr)
    )))
}

fn refuse_existing(p: &ManagedPath) -> Result<()> {
    if std::fs::symlink_metadata(p.as_path()).is_ok() {
        return Err(BackendError::StateConflict(
            "destination already exists: the backend does not overwrite files".into(),
        ));
    }
    Ok(())
}

/// Standalone QCOW2 copy of `src` (its backing chain, if any, is flattened:
/// the result has no backing file) into the new file `dst`. `src` is read
/// with an explicit format; `dst` must not exist.
pub fn convert_standalone(
    cmd: &ImageCommand<'_>,
    src: &ManagedPath,
    dst: &ManagedPath,
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    refuse_existing(dst)?;
    let o = cmd.run(
        &[
            "convert",
            "-q",
            "-f",
            "qcow2",
            "-O",
            "qcow2",
            src.qemu_value(),
            dst.qemu_value(),
        ],
        cancel,
    )?;
    if o.code != Some(0) {
        return Err(failed("convert", &o));
    }
    Ok(())
}

/// `true` if the two images have the same guest-visible content
/// (`qemu-img compare`: exit 0 identical, 1 different).
pub fn images_identical(
    cmd: &ImageCommand<'_>,
    a: &ManagedPath,
    b: &ManagedPath,
    cancel: &dyn Fn() -> bool,
) -> Result<bool> {
    let o = cmd.run(
        &[
            "compare",
            "-q",
            "-f",
            "qcow2",
            "-F",
            "qcow2",
            a.qemu_value(),
            b.qemu_value(),
        ],
        cancel,
    )?;
    match o.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(failed("compare", &o)),
    }
}

/// `qemu-img check`: `Ok` only for an image without errors or leaks.
pub fn check_image(
    cmd: &ImageCommand<'_>,
    p: &ManagedPath,
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let o = cmd.run(&["check", "-q", "-f", "qcow2", p.qemu_value()], cancel)?;
    if o.code != Some(0) {
        return Err(failed("check", &o));
    }
    Ok(())
}

/// `true` for the only backing reference LightVM writes:
/// `../base/<uuid>/disk.qcow2` (lowercase hexadecimal UUID).
pub fn is_fixed_backing_ref(r: &str) -> bool {
    let Some(rest) = r.strip_prefix("../base/") else {
        return false;
    };
    let Some(id) = rest.strip_suffix("/disk.qcow2") else {
        return false;
    };
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_digit() || ('a'..='f').contains(&c)
            }
        })
}

/// Creates the QCOW2 overlay `dst` of `size_bytes` with the backing
/// reference `backing_ref` (relative to the overlay's **final** directory,
/// fixed shape [`is_fixed_backing_ref`]) and format `qcow2`. The backing
/// file is not opened (`-u`): `dst` is a journal temporary that is moved
/// next to the base afterwards; the reference is verified from the header
/// by the caller, and QEMU receives the base as an explicit node.
pub fn create_overlay_ref(
    cmd: &ImageCommand<'_>,
    backing_ref: &str,
    size_bytes: u64,
    dst: &ManagedPath,
) -> Result<()> {
    if !is_fixed_backing_ref(backing_ref) {
        return Err(BackendError::invalid(
            "backing_ref",
            "not the fixed backing reference shape",
        ));
    }
    if size_bytes == 0 {
        return Err(BackendError::invalid("size", "zero virtual size"));
    }
    refuse_existing(dst)?;
    let size = size_bytes.to_string();
    let o = cmd.run(
        &[
            "create",
            "-q",
            "-f",
            "qcow2",
            "-u",
            "-b",
            backing_ref,
            "-F",
            "qcow2",
            dst.qemu_value(),
            &size,
        ],
        &|| false,
    )?;
    if o.code != Some(0) {
        return Err(failed("create", &o));
    }
    Ok(())
}

/// Virtual size of a QCOW2 image (`qemu-img info --output=json`).
pub fn virtual_size(cmd: &ImageCommand<'_>, p: &ManagedPath) -> Result<u64> {
    let o = cmd.run(
        &["info", "-f", "qcow2", "--output=json", p.qemu_value()],
        &|| false,
    )?;
    if o.code != Some(0) {
        return Err(failed("info", &o));
    }
    let v: serde_json::Value = serde_json::from_str(&o.stdout)
        .map_err(|e| BackendError::Io(std::io::Error::other(format!("qemu-img info: {e}"))))?;
    v.get("virtual-size")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            BackendError::Io(std::io::Error::other("qemu-img info: virtual-size missing"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F6: `qemu-img` failing on a full volume becomes `resource_limit`
    /// with a clear message (injected outputs, the texts QEMU prints).
    #[test]
    fn full_volume_is_a_resource_limit() {
        for stderr in [
            "qemu-img: error while writing at byte 1048576: No space left on device",
            "qemu-img: Could not write: There is not enough space on the disk.",
        ] {
            let o = ImageOutput {
                code: Some(1),
                stdout: String::new(),
                stderr: stderr.into(),
            };
            let e = failed("convert", &o);
            assert!(matches!(e, BackendError::ResourceLimit(_)), "{e}");
            let p = e.to_public();
            assert_eq!(p.code.as_str(), "resource_limit");
            assert!(p.message.contains("free space"), "{}", p.message);
        }
        let o = ImageOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "qemu-img: Image is corrupt".into(),
        };
        assert!(matches!(failed("check", &o), BackendError::Io(_)));
    }

    #[test]
    fn fixed_backing_reference_shape() {
        assert!(is_fixed_backing_ref(
            "../base/01890a5d-ac96-774b-bcce-b302099a8057/disk.qcow2"
        ));
        for bad in [
            "../base/01890a5d-ac96-774b-bcce-b302099a8057/disk.qcow2x",
            "../../base/01890a5d-ac96-774b-bcce-b302099a8057/disk.qcow2",
            "C:/x/disk.qcow2",
            "../base/01890A5D-AC96-774B-BCCE-B302099A8057/disk.qcow2",
            "../base/../x/disk.qcow2",
            "json:{}",
            "",
        ] {
            assert!(!is_fixed_backing_ref(bad), "{bad}");
        }
    }
}
