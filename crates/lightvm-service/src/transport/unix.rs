//! Unix socket in a private directory with peer UID check.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

use super::{Accepted, BoxedStream, EndpointOptions, PeerInfo};

/// Practical limit of `sun_path` (108 bytes on Linux, 104 on macOS).
const MAX_SOCKET_PATH: usize = 100;

#[derive(Debug)]
pub(super) struct Listener {
    inner: UnixListener,
    path: PathBuf,
    endpoint: String,
    own_uid: u32,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn own_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Creates (or checks) a private directory: not a symbolic link, owned by the
/// effective user, no permissions for group and others.
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the private directory is not a directory (symbolic link?)",
        ));
    }
    if meta.uid() != own_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the private directory belongs to another user",
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the private directory has group or other permissions (expected 0700)",
        ));
    }
    Ok(())
}

fn runtime_base(opts: &EndpointOptions) -> PathBuf {
    if let Some(d) = &opts.runtime_dir {
        return d.clone();
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(x) if Path::new(&x).is_absolute() => Path::new(&x).join("lightvm"),
        _ => opts.catalog_dir.join(".run"),
    }
}

/// Length of the socket file name `svc-<16 hex>.sock` plus the separator.
const SOCKET_NAME_LEN: usize = 26;

impl Listener {
    pub(super) fn bind(opts: &EndpointOptions) -> io::Result<Self> {
        let mut base = runtime_base(opts);
        // macOS has no XDG_RUNTIME_DIR and long catalog or temporary paths:
        // a base that cannot hold the socket falls back to a short private
        // directory instead of refusing the start.
        if base.as_os_str().len() + SOCKET_NAME_LEN > MAX_SOCKET_PATH {
            let short = crate::display::fallback_root();
            crate::diag!(
                "lightvm-service: path {} too long for the IPC socket, private directory {}",
                base.display(),
                short.display()
            );
            base = short;
        }
        ensure_private_dir(&base)?;
        let path = base.join(format!("svc-{}.sock", &super::random_suffix()?[..16]));
        let endpoint = path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 path"))?
            .to_owned();
        if endpoint.len() > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "socket path too long ({} bytes, maximum {MAX_SOCKET_PATH}): use --runtime-dir",
                    endpoint.len()
                ),
            ));
        }
        let inner = UnixListener::bind(&path)?;
        // The 0700 directory already protects the window between bind and chmod.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            inner,
            path,
            endpoint,
            own_uid: own_uid(),
        })
    }

    pub(super) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(super) async fn accept(&self) -> io::Result<Accepted> {
        let (stream, _) = self.inner.accept().await?;
        let cred = match stream.peer_cred() {
            Ok(c) => c,
            Err(e) => return Ok(Accepted::Rejected(format!("peer credentials: {e}"))),
        };
        if cred.uid() != self.own_uid {
            return Ok(Accepted::Rejected(format!(
                "peer UID {} differs from {}",
                cred.uid(),
                self.own_uid
            )));
        }
        let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
        Ok(Accepted::Ok(
            Box::new(stream),
            PeerInfo {
                pid,
                os_identity: format!("uid={}", cred.uid()),
            },
        ))
    }
}

pub(super) async fn connect(endpoint: &str) -> io::Result<BoxedStream> {
    let s = UnixStream::connect(endpoint).await?;
    Ok(Box::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_rejects_open_permissions() {
        let d = crate::test_support::temp_dir("privdir");
        let p = d.join("x");
        ensure_private_dir(&p).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_private_dir(&p).is_err());
    }

    #[test]
    fn long_runtime_dir_falls_back_to_a_short_private_dir() {
        let d = crate::test_support::temp_dir("long-runtime");
        let long = d.join("x".repeat(MAX_SOCKET_PATH));
        let opts = EndpointOptions {
            runtime_dir: Some(long.clone()),
            catalog_dir: long,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let l = rt.block_on(async { Listener::bind(&opts) }).unwrap();
        assert!(l.endpoint().len() <= MAX_SOCKET_PATH, "{}", l.endpoint());
        let root = crate::display::fallback_root();
        assert!(Path::new(l.endpoint()).starts_with(&root));
        drop(l);
        // Only the socket is removed by the listener; the directory is left
        // to the stale-root sweep of a later service, removed here instead.
        let _ = fs::remove_dir(&root);
    }

    #[test]
    fn private_dir_rejects_symlink() {
        let d = crate::test_support::temp_dir("privdir-link");
        let real = d.join("real");
        ensure_private_dir(&real).unwrap();
        let link = d.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
    }
}
