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

#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
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

#[cfg(windows)]
fn system32(exe: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join(exe)
}

#[cfg(windows)]
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
    restrict_to_current_user(private_dir)
}

/// Creates `dir` if missing, removes inheritance and grants full control to
/// the current user only (by SID), then checks that the ACL has exactly one
/// entry (same procedure as [`prepare_run_dirs`], used also for the
/// supervisor's per-run display sockets).
pub fn restrict_to_current_user(private_dir: &ManagedPath) -> Result<RunDirs> {
    std::fs::create_dir_all(private_dir.as_path())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        // Unix: mode 0700, owned by the effective user, not a link.
        std::fs::set_permissions(
            private_dir.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let meta = std::fs::symlink_metadata(private_dir.as_path())?;
        let uid = crate::sys::unix::euid();
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(BackendError::StateConflict(
                "private directory not owned by the current user or not 0700".into(),
            ));
        }
        Ok(RunDirs {
            owner_sid: format!("uid={uid}"),
            private_acl_entries: 1,
        })
    }
    #[cfg(windows)]
    {
        restrict_windows(private_dir)
    }
}

#[cfg(windows)]
fn restrict_windows(private_dir: &ManagedPath) -> Result<RunDirs> {
    let sid = current_user_sid()?;
    let grant = format!("*{sid}:(OI)(CI)F");
    let mut icacls = Command::new(system32("icacls.exe"));
    icacls
        .arg(private_dir.as_path())
        .args(["/inheritance:r", "/grant:r", &grant])
        .env_clear()
        .envs(crate::process::minimal_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Not while `sys::windows::spawn` holds inheritable handles; the wait
    // happens outside the lock.
    let spawned = {
        let _spawn = crate::process::spawn_lock();
        icacls.spawn()
    };
    let st = spawned
        .and_then(|mut c| c.wait())
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

#[cfg(windows)]
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

/// Total physical memory of the host in bytes.
pub fn total_memory_bytes() -> Result<u64> {
    #[cfg(windows)]
    {
        Ok(crate::sys::windows::physical_memory()?.0)
    }
    #[cfg(unix)]
    {
        Ok(crate::sys::unix::physical_memory()?.0)
    }
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
    #[cfg(unix)]
    {
        let (total, avail) = crate::sys::unix::physical_memory()?;
        let free = crate::sys::unix::free_disk_bytes(storage_dir.as_path())?;
        Ok(HostResources {
            total_memory_bytes: total,
            available_memory_bytes: avail,
            free_disk_bytes: free,
        })
    }
}

/// Kind of volume holding an open file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    /// Local fixed disk.
    Fixed,
    /// Removable local disk (USB stick, card reader).
    Removable,
    /// Local optical drive.
    CdRom,
    /// Network share, including mapped network drives.
    Remote,
    /// RAM disk, unknown type or no root directory.
    Other,
}

impl VolumeKind {
    /// Local volumes from which a user-chosen file may be read.
    pub fn is_local(self) -> bool {
        matches!(self, Self::Fixed | Self::Removable | Self::CdRom)
    }
}

/// Where an open file really is, as resolved by the operating system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLocation {
    /// Final path of the handle with the `\\?\` prefix removed
    /// (`\\?\UNC\server\share\x` becomes `\\server\share\x`): links,
    /// junctions, mount points, `subst` and mapped drives resolved.
    pub final_path: std::path::PathBuf,
    /// Kind of the volume holding the file.
    pub volume: VolumeKind,
}

/// Final path and volume kind of an open file (Windows:
/// `GetFinalPathNameByHandleW`, `GetVolumePathNameW`, `GetDriveTypeW`).
pub fn file_location(file: &std::fs::File) -> Result<FileLocation> {
    #[cfg(windows)]
    {
        use std::path::PathBuf;
        let raw = crate::sys::windows::final_path_by_handle(file)?;
        let s = raw.to_string_lossy().into_owned();
        let (final_path, unc) = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            (PathBuf::from(format!(r"\\{rest}")), true)
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            (PathBuf::from(rest), false)
        } else {
            (PathBuf::from(&s), s.starts_with(r"\\"))
        };
        if unc {
            return Ok(FileLocation {
                final_path,
                volume: VolumeKind::Remote,
            });
        }
        let root = crate::sys::windows::volume_path_name(&final_path)?;
        // `DRIVE_*` values of `GetDriveTypeW`.
        let volume = match crate::sys::windows::drive_type(Path::new(&root)) {
            2 => VolumeKind::Removable,
            3 => VolumeKind::Fixed,
            4 => VolumeKind::Remote,
            5 => VolumeKind::CdRom,
            _ => VolumeKind::Other,
        };
        Ok(FileLocation { final_path, volume })
    }
    #[cfg(unix)]
    {
        let final_path = crate::sys::unix::final_path(file)?;
        let volume = if crate::sys::unix::is_remote(file)? {
            VolumeKind::Remote
        } else {
            VolumeKind::Fixed
        };
        Ok(FileLocation { final_path, volume })
    }
}

/// `false` only if no running process has this PID; access denied or any
/// error count as running (conservative: used to decide whether a
/// leftover directory of a previous service can be removed).
pub fn pid_may_be_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        crate::sys::windows::pid_may_be_running(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn location_of_a_local_file_and_pid_liveness() {
        let d = tmp_dir("location");
        std::fs::create_dir_all(d.as_path()).unwrap();
        let p = d.as_path().join("f.bin");
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let loc = file_location(&f).unwrap();
        assert_eq!(loc.volume, VolumeKind::Fixed);
        assert!(loc.volume.is_local());
        // Same file, possibly with different case or long names of the
        // temporary directory.
        let canon = std::fs::canonicalize(&p).unwrap();
        let canon = canon.to_string_lossy();
        let canon = canon.strip_prefix(r"\\?\").unwrap_or(&canon);
        assert!(
            loc.final_path.to_string_lossy().eq_ignore_ascii_case(canon),
            "{loc:?} vs {canon}"
        );
        assert!(pid_may_be_running(std::process::id()));
        assert!(!pid_may_be_running(0xFFFF_FFF0));
    }

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
