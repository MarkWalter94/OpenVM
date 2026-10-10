//! Preparation of a run on the host disk (F1-05, F1-08, F1-12):
//! run directory and private socket directory with an explicit
//! user-only ACL, per-run copy of the UEFI variables, creation of the main
//! disk and of the overlay with `qemu-img`, host resources (memory and
//! free space) measured rather than declared.
//!
//! All paths are [`ManagedPath`]s supplied by the storage owner;
//! no function overwrites an existing file. `qemu-img` runs without a
//! shell, with a reduced environment and a timeout ([`crate::probe::run_capture`]); the
//! binary must first be verified with [`crate::probe::verify_approved_binary`]
//! ([`crate::probe::probe_host`] does it).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::cmdline::DiskFormat;
use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::probe::{run_capture, stderr_excerpt};

/// Minimum size of a disk created by the backend.
pub const MIN_DISK_MIB: u64 = 16;
/// Maximum size of a disk created by the backend (2 TiB).
pub const MAX_DISK_MIB: u64 = 2 * 1024 * 1024;

/// Default timeout of `qemu-img create`.
pub const QEMU_IMG_TIMEOUT: Duration = Duration::from_secs(60);

fn refuse_existing(field: &'static str, p: &ManagedPath) -> Result<()> {
    if std::fs::symlink_metadata(p.as_path()).is_ok() {
        return Err(BackendError::StateConflict(format!(
            "`{field}` already exists: the backend does not overwrite files"
        )));
    }
    Ok(())
}

fn qemu_img_create(qemu_img: &ManagedPath, args: &[&str], timeout: Duration) -> Result<()> {
    let o = run_capture(qemu_img.as_path(), args, timeout)?;
    if o.code != Some(0) {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "qemu-img create exited with {:?}: {}",
            o.code,
            stderr_excerpt(&o.stderr)
        ))));
    }
    Ok(())
}

/// Creates the empty main disk (`qemu-img create -f <format>`), of
/// `size_mib` MiB. Rejects existing files and sizes outside
/// [`MIN_DISK_MIB`]..=[`MAX_DISK_MIB`].
pub fn create_disk(
    qemu_img: &ManagedPath,
    disk: &ManagedPath,
    format: DiskFormat,
    size_mib: u64,
    timeout: Duration,
) -> Result<()> {
    if !(MIN_DISK_MIB..=MAX_DISK_MIB).contains(&size_mib) {
        return Err(BackendError::ResourceLimit(format!(
            "disk of {size_mib} MiB outside the range {MIN_DISK_MIB}..={MAX_DISK_MIB}"
        )));
    }
    refuse_existing("disk", disk)?;
    let size = format!("{size_mib}M");
    qemu_img_create(
        qemu_img,
        &[
            "create",
            "-q",
            "-f",
            format.driver(),
            disk.qemu_value(),
            &size,
        ],
        timeout,
    )
}

/// Virtual size in bytes of a disk (`qemu-img info --output=json`,
/// explicit format: no automatic detection).
pub fn disk_virtual_size(
    qemu_img: &ManagedPath,
    disk: &ManagedPath,
    format: DiskFormat,
    timeout: Duration,
) -> Result<u64> {
    let o = run_capture(
        qemu_img.as_path(),
        &[
            "info",
            "-f",
            format.driver(),
            "--output=json",
            disk.qemu_value(),
        ],
        timeout,
    )?;
    if o.code != Some(0) {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "qemu-img info exited with {:?}: {}",
            o.code,
            stderr_excerpt(&o.stderr)
        ))));
    }
    let v: serde_json::Value = serde_json::from_str(&o.stdout)
        .map_err(|e| BackendError::Io(std::io::Error::other(format!("qemu-img info: {e}"))))?;
    v.get("virtual-size")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            BackendError::Io(std::io::Error::other("qemu-img info: virtual-size missing"))
        })
}

/// Brings the disk to `size_mib` MiB (`qemu-img resize`). Growing only:
/// a size smaller than the current one is rejected (data loss in the
/// guest); an equal size does nothing. Returns `true` if the
/// disk was grown.
pub fn grow_disk(
    qemu_img: &ManagedPath,
    disk: &ManagedPath,
    format: DiskFormat,
    size_mib: u64,
    timeout: Duration,
) -> Result<bool> {
    if !(MIN_DISK_MIB..=MAX_DISK_MIB).contains(&size_mib) {
        return Err(BackendError::ResourceLimit(format!(
            "disk of {size_mib} MiB outside the range {MIN_DISK_MIB}..={MAX_DISK_MIB}"
        )));
    }
    let current = disk_virtual_size(qemu_img, disk, format, timeout)?;
    let requested = size_mib << 20;
    if requested < current {
        return Err(BackendError::invalid(
            "disk",
            "shrinking the disk is not supported: it can only grow",
        ));
    }
    if requested == current {
        return Ok(false);
    }
    let size = format!("{size_mib}M");
    let o = run_capture(
        qemu_img.as_path(),
        &[
            "resize",
            "-q",
            "-f",
            format.driver(),
            disk.qemu_value(),
            &size,
        ],
        timeout,
    )?;
    if o.code != Some(0) {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "qemu-img resize exited with {:?}: {}",
            o.code,
            stderr_excerpt(&o.stderr)
        ))));
    }
    Ok(true)
}

/// Creates a qcow2 overlay on `base` with an explicit backing format
/// (`-F`), without automatic detection. The backing reference is the
/// absolute path of `base`.
pub fn create_overlay(
    qemu_img: &ManagedPath,
    base: &ManagedPath,
    base_format: DiskFormat,
    overlay: &ManagedPath,
    timeout: Duration,
) -> Result<()> {
    refuse_existing("overlay", overlay)?;
    if !base.as_path().is_file() {
        return Err(BackendError::invalid(
            "base",
            "the base disk does not exist",
        ));
    }
    qemu_img_create(
        qemu_img,
        &[
            "create",
            "-q",
            "-f",
            "qcow2",
            "-F",
            base_format.driver(),
            "-b",
            base.qemu_value(),
            overlay.qemu_value(),
        ],
        timeout,
    )
}

/// Per-run copy of the UEFI variables from the template (pflash1). Rejects
/// an existing destination and verifies the size of the copy.
pub fn copy_firmware_vars(template: &ManagedPath, dest: &ManagedPath) -> Result<u64> {
    refuse_existing("firmware_vars", dest)?;
    let expected = std::fs::metadata(template.as_path())?.len();
    let copied = std::fs::copy(template.as_path(), dest.as_path())?;
    if copied != expected {
        let _ = std::fs::remove_file(dest.as_path());
        return Err(BackendError::Io(std::io::Error::other(
            "incomplete copy of the UEFI variables",
        )));
    }
    Ok(copied)
}

/// Outcome of the directory preparation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunDirs {
    /// SID of the user the private directory is reserved to.
    pub owner_sid: String,
    /// ACL entries of the private directory after the restriction (expected 1).
    pub private_acl_entries: usize,
}

fn system32(exe: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join(exe)
}

/// SID of the current user (`whoami /user`), form `S-1-...`.
fn current_user_sid() -> Result<String> {
    let o = run_capture(
        &system32("whoami.exe"),
        &["/user", "/fo", "csv", "/nh"],
        Duration::from_secs(10),
    )?;
    o.stdout
        .split(',')
        .map(|f| f.trim().trim_matches('"'))
        .find(|f| f.starts_with("S-1-"))
        .map(str::to_owned)
        .ok_or_else(|| BackendError::Io(std::io::Error::other("user SID not detected")))
}

/// Creates the run directory and the private directory for the QMP/VNC sockets
/// and the screendumps; on the private directory it removes inheritance and
/// grants full control to the current user only (by SID), as
/// verified in F0-02 §4.3, without elevation. Returns the SID and the number
/// of entries of the resulting ACL; with more than one entry the operation fails.
///
/// Uses `icacls.exe` from `%SystemRoot%\System32` (no additional `unsafe`
/// API). Refusal of another user is not tested here (F0-02: no
/// second account available).
pub fn prepare_run_dirs(run_dir: &ManagedPath, private_dir: &ManagedPath) -> Result<RunDirs> {
    std::fs::create_dir_all(run_dir.as_path())?;
    std::fs::create_dir_all(private_dir.as_path())?;
    if !cfg!(windows) {
        return Err(BackendError::Unsupported {
            what: "private directory ACL on this system".into(),
            hint: "on Linux use 0700 permissions (not yet implemented)".into(),
        });
    }
    let sid = current_user_sid()?;
    let grant = format!("*{sid}:(OI)(CI)F");
    let st = Command::new(system32("icacls.exe"))
        .arg(private_dir.as_path())
        .args(["/inheritance:r", "/grant:r", &grant])
        .env_clear()
        .envs(crate::process::minimal_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(BackendError::Spawn)?;
    if !st.success() {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "icacls on the private directory exited with {:?}",
            st.code()
        ))));
    }
    let entries = acl_entries(private_dir.as_path())?;
    if entries != 1 {
        return Err(BackendError::StateConflict(format!(
            "private directory ACL with {entries} entries (expected 1)"
        )));
    }
    Ok(RunDirs {
        owner_sid: sid,
        private_acl_entries: entries,
    })
}

/// Number of ACE entries reported by `icacls <dir>` (lines with `:(`).
fn acl_entries(dir: &Path) -> Result<usize> {
    let dir_s = dir
        .to_str()
        .ok_or_else(|| BackendError::invalid("path", "path is not UTF-8"))?;
    let o = run_capture(&system32("icacls.exe"), &[dir_s], Duration::from_secs(10))?;
    Ok(o.stdout.lines().filter(|l| l.contains(":(")).count())
}

/// Measured host resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostResources {
    /// Total physical memory in bytes.
    pub total_memory_bytes: u64,
    /// Physical memory available in bytes at measurement time.
    pub available_memory_bytes: u64,
    /// Free space available to the user on the volume of the given directory.
    pub free_disk_bytes: u64,
}

/// Measures physical memory and free space on the volume of `storage_dir`.
pub fn host_resources(storage_dir: &ManagedPath) -> Result<HostResources> {
    #[cfg(windows)]
    {
        let (total, avail) = crate::sys::windows::physical_memory()?;
        let free = crate::sys::windows::free_disk_bytes(storage_dir.as_path())?;
        Ok(HostResources {
            total_memory_bytes: total,
            available_memory_bytes: avail,
            free_disk_bytes: free,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = storage_dir;
        Err(BackendError::Unsupported {
            what: "host resource measurement on this system".into(),
            hint: "Linux implementation not yet available".into(),
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> ManagedPath {
        let d = std::env::temp_dir()
            .join("lightvm-qemu-backend-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        ManagedPath::verify(d).unwrap()
    }

    #[test]
    fn private_directory_user_only() {
        let base = tmp_dir("dirs");
        let run = base.join_file("run").unwrap();
        let p = base.join_file("priv").unwrap();
        let r = prepare_run_dirs(&run, &p).unwrap();
        assert!(r.owner_sid.starts_with("S-1-"));
        assert_eq!(r.private_acl_entries, 1);
        assert!(run.as_path().is_dir() && p.as_path().is_dir());
        let _ = std::fs::remove_dir_all(base.as_path());
    }

    #[test]
    fn copy_vars_without_overwriting() {
        let base = tmp_dir("vars");
        std::fs::create_dir_all(base.as_path()).unwrap();
        let t = base.join_file("template.fd").unwrap();
        std::fs::write(t.as_path(), vec![7u8; 4096]).unwrap();
        let d = base.join_file("vars.fd").unwrap();
        assert_eq!(copy_firmware_vars(&t, &d).unwrap(), 4096);
        assert!(matches!(
            copy_firmware_vars(&t, &d),
            Err(BackendError::StateConflict(_))
        ));
        let _ = std::fs::remove_dir_all(base.as_path());
    }

    #[test]
    fn measured_resources() {
        let d = std::env::temp_dir().join("lightvm-qemu-backend-tests");
        std::fs::create_dir_all(&d).unwrap();
        let r = host_resources(&ManagedPath::verify(d).unwrap()).unwrap();
        assert!(r.total_memory_bytes > 0);
        assert!(r.available_memory_bytes <= r.total_memory_bytes);
        assert!(r.free_disk_bytes > 0);
    }

    #[test]
    fn disk_size_validated() {
        let base = tmp_dir("disk");
        let img = base.join_file("qemu-img.exe").unwrap();
        let d = base.join_file("d.qcow2").unwrap();
        for size in [0, MIN_DISK_MIB - 1, MAX_DISK_MIB + 1] {
            assert!(matches!(
                create_disk(&img, &d, DiskFormat::Qcow2, size, QEMU_IMG_TIMEOUT),
                Err(BackendError::ResourceLimit(_))
            ));
        }
    }
}
