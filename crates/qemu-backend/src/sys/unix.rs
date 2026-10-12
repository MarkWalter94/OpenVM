//! Unix primitives (macOS, Linux) without `unsafe`: `rustix` for the file
//! system calls, the system tools for what has no safe wrapper (memory on
//! macOS).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{BackendError, Result};

/// Effective user ID of this process.
pub(crate) fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Free space available to the user on the volume of `dir`, in bytes.
pub(crate) fn free_disk_bytes(dir: &Path) -> Result<u64> {
    let st = rustix::fs::statvfs(dir).map_err(|e| BackendError::Io(e.into()))?;
    Ok(st.f_bavail.saturating_mul(st.f_frsize))
}

/// Total and available physical memory in bytes.
#[cfg(target_os = "macos")]
pub(crate) fn physical_memory() -> Result<(u64, u64)> {
    let timeout = Duration::from_secs(5);
    let total = crate::probe::run_capture(
        Path::new("/usr/sbin/sysctl"),
        &["-n", "hw.memsize"],
        timeout,
    )?;
    let total: u64 = total
        .stdout
        .trim()
        .parse()
        .map_err(|_| BackendError::Io(std::io::Error::other("hw.memsize not readable")))?;
    let vm = crate::probe::run_capture(Path::new("/usr/bin/vm_stat"), &[], timeout)?;
    Ok((total, vm_stat_available(&vm.stdout).unwrap_or(0).min(total)))
}

/// Available memory from `vm_stat`: free, inactive and speculative pages
/// (the pages macOS can hand out without paging).
#[cfg(any(target_os = "macos", test))]
fn vm_stat_available(text: &str) -> Option<u64> {
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> u64 {
        text.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.rsplit(':').next())
            .and_then(|v| v.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let n = pages("Pages free") + pages("Pages inactive") + pages("Pages speculative");
    Some(n.saturating_mul(page))
}

/// Total and available physical memory in bytes (`/proc/meminfo`).
#[cfg(not(target_os = "macos"))]
pub(crate) fn physical_memory() -> Result<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo")?;
    let kib = |name: &str| -> Option<u64> {
        text.lines()
            .find(|l| l.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let total = kib("MemTotal:")
        .ok_or_else(|| BackendError::Io(std::io::Error::other("MemTotal not readable")))?;
    let avail = kib("MemAvailable:").unwrap_or(0);
    Ok((total * 1024, avail * 1024))
}

/// Final path of an open file as the kernel resolves it.
pub(crate) fn final_path(file: &File) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let c = rustix::fs::getpath(file).map_err(|e| BackendError::Io(e.into()))?;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(c.into_bytes())))
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::fd::AsRawFd as _;
        Ok(std::fs::read_link(format!(
            "/proc/self/fd/{}",
            file.as_raw_fd()
        ))?)
    }
}

/// Whether the file lives on a network file system.
pub(crate) fn is_remote(file: &File) -> Result<bool> {
    let st = rustix::fs::fstatfs(file).map_err(|e| BackendError::Io(e.into()))?;
    #[cfg(target_os = "macos")]
    {
        let name: Vec<u8> = st
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        Ok(matches!(
            name.as_slice(),
            b"nfs" | b"smbfs" | b"afpfs" | b"webdav" | b"cifs" | b"ftp" | b"macfuse"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        // NFS, SMB, CIFS, SMB2, FUSE (sshfs and similar): magic numbers of
        // `statfs(2)`.
        #[allow(clippy::unnecessary_cast)]
        let t = st.f_type as i64;
        Ok(matches!(
            t,
            0x6969 | 0x517B | 0xFF53_4D42 | 0xFE53_4D42 | 0x6573_5546
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_stat_is_parsed() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                    Pages free:                               10.\n\
                    Pages active:                            500.\n\
                    Pages inactive:                           20.\n\
                    Pages speculative:                         2.\n";
        assert_eq!(vm_stat_available(text), Some(32 * 16384));
        assert_eq!(vm_stat_available("garbage"), None);
    }

    #[test]
    fn measures_this_host() {
        let dir = std::env::temp_dir();
        assert!(free_disk_bytes(&dir).unwrap() > 0);
        let (total, avail) = physical_memory().unwrap();
        assert!(total > 0 && avail <= total);
        let f = File::open(&dir).unwrap();
        assert!(final_path(&f).unwrap().is_absolute());
        assert!(!is_remote(&f).unwrap());
    }
}
