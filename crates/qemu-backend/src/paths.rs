//! Managed paths: the only source of paths for the QEMU command.
//!
//! The backend does not accept path strings from the caller. Every path
//! arrives as a [`ManagedPath`], which can only be built through
//! [`ManagedPath::verify`], and the paths of a run are returned by an
//! object implementing [`ManagedPaths`] (in production the registry of
//! `vm-storage`/`lightvm-service`, which creates managed directories and copies
//! and protects creation/opening against symlinks, junctions and substitutions: ROADMAP 6.3).

use std::path::{Path, PathBuf};

use crate::error::{BackendError, Result};
use crate::validate::{UNIX_SOCKET_PATH_MAX, check_token, qemu_path_value};
use lightvm_proto::{MediaId, RunId};

/// Verified absolute path that can be represented unambiguously in the
/// QEMU options.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ManagedPath {
    path: PathBuf,
    qemu: String,
}

impl ManagedPath {
    /// Verifies a path:
    /// - UTF-8, absolute, without `.`/`..` components, without UNC,
    ///   `\\?\` or `\\.\` prefixes (see [`crate::validate::qemu_path_value`]);
    /// - no character QEMU would interpret (`,`, `=`, `:` beyond the drive,
    ///   control characters, quotes, metacharacters);
    /// - if it exists, it is neither a symbolic link nor a junction.
    ///
    /// Checks against substitutions between check and use are the
    /// responsibility of the directory owner (storage registry); here only the
    /// form and the last component are verified.
    pub fn verify(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let s = path
            .to_str()
            .ok_or_else(|| BackendError::invalid("path", "path is not UTF-8"))?;
        let qemu = qemu_path_value("path", s)?;
        if let Ok(meta) = std::fs::symlink_metadata(&path)
            && meta.file_type().is_symlink()
        {
            return Err(BackendError::invalid(
                "path",
                "symbolic link or junction not allowed",
            ));
        }
        Ok(Self { path, qemu })
    }

    /// Appends a file name (token) and verifies the result.
    pub fn join_file(&self, name: &str) -> Result<Self> {
        check_token("file_name", name)?;
        Self::verify(self.path.join(name))
    }

    /// Path for operating-system APIs.
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Form used in the QEMU options (`/` separators).
    pub fn qemu_value(&self) -> &str {
        &self.qemu
    }

    /// Verifies that the path can be used as a Unix socket (length of
    /// `sun_path`).
    pub(crate) fn check_socket_len(&self, field: &'static str) -> Result<()> {
        if self.qemu.len() > UNIX_SOCKET_PATH_MAX {
            return Err(BackendError::invalid(
                field,
                format!(
                    "socket path of {} bytes (maximum {UNIX_SOCKET_PATH_MAX})",
                    self.qemu.len()
                ),
            ));
        }
        Ok(())
    }
}

/// Paths of a run, already verified by the storage owner.
///
/// Every method returns absolute paths in directories controlled by the
/// supervisor; the backend does not build paths from caller text, except for
/// fixed file names inside the returned directories.
pub trait ManagedPaths: Send + Sync {
    /// Approved `qemu-system-x86_64` executable.
    fn qemu_system(&self) -> Result<ManagedPath>;

    /// UEFI firmware code (opened read-only as pflash0).
    fn firmware_code(&self) -> Result<ManagedPath>;

    /// Per-run copy of the UEFI variables (pflash1, writable).
    fn firmware_vars(&self, run: &RunId) -> Result<ManagedPath>;

    /// VM disk for this run (overlay), if any.
    fn disk(&self, run: &RunId) -> Result<Option<ManagedPath>>;

    /// Files to keep open read-only with write exclusion for the whole run
    /// (overlay base, baseline variables).
    fn protected_readonly(&self, run: &RunId) -> Result<Vec<ManagedPath>>;

    /// Base disk of the run's overlay (F3), passed to QEMU as an explicit
    /// read-only `qcow2` node referenced by the overlay (`backing=`): the
    /// path written in the overlay header is not used to open it, so no
    /// other chain can be substituted. `None`: the disk has no backing file.
    fn disk_backing(&self, _run: &RunId) -> Result<Option<ManagedPath>> {
        Ok(None)
    }

    /// Approved medium from the media registry (read-only managed copy).
    fn media(&self, media: &MediaId) -> Result<ManagedPath>;

    /// Run directory (stdout/stderr logs).
    fn run_dir(&self, run: &RunId) -> Result<ManagedPath>;

    /// Private run directory (ACL limited to the supervisor user): QMP and
    /// VNC sockets, temporary `screendump` files.
    fn private_dir(&self, run: &RunId) -> Result<ManagedPath>;
}

/// Fixed names of the files created by the backend in the managed directories.
pub(crate) mod names {
    pub(crate) const QMP_SOCKET: &str = "qmp.sock";
    pub(crate) const VNC_SOCKET: &str = "vnc.sock";
    pub(crate) const STDOUT: &str = "qemu.stdout.log";
    pub(crate) const STDERR: &str = "qemu.stderr.log";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn verify_and_join() {
        let p = ManagedPath::verify(r"C:\Windows\Temp\lightvm-test-dir").unwrap();
        assert_eq!(p.qemu_value(), "C:/Windows/Temp/lightvm-test-dir");
        assert!(p.join_file("qmp.sock").is_ok());
        assert!(p.join_file("..").is_err());
        assert!(p.join_file("a/b").is_err());
        assert!(p.join_file("a,b").is_err());
        assert!(ManagedPath::verify(r"relative").is_err());
        let long = format!(r"C:\{}\qmp.sock", "a".repeat(120));
        assert!(
            ManagedPath::verify(&long)
                .unwrap()
                .check_socket_len("qmp")
                .is_err()
        );
    }
}
