//! Socket Unix in directory privata con verifica dell'UID del peer.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

use super::{Accepted, BoxedStream, EndpointOptions, PeerInfo};

/// Limite pratico di `sun_path` (108 byte su Linux, 104 su macOS).
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

/// Crea (o verifica) una directory privata: non un collegamento simbolico,
/// proprietario l'utente effettivo, nessun permesso per gruppo e altri.
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
            "la directory privata non è una directory (collegamento simbolico?)",
        ));
    }
    if meta.uid() != own_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "la directory privata appartiene a un altro utente",
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "la directory privata ha permessi per gruppo o altri (attesi 0700)",
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

impl Listener {
    pub(super) fn bind(opts: &EndpointOptions) -> io::Result<Self> {
        let base = runtime_base(opts);
        ensure_private_dir(&base)?;
        let path = base.join(format!("svc-{}.sock", &super::random_suffix()?[..16]));
        let endpoint = path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "percorso non UTF-8"))?
            .to_owned();
        if endpoint.len() > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "percorso del socket troppo lungo ({} byte, massimo {MAX_SOCKET_PATH}): usare --runtime-dir",
                    endpoint.len()
                ),
            ));
        }
        let inner = UnixListener::bind(&path)?;
        // La directory 0700 protegge già la finestra tra bind e chmod.
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
            Err(e) => return Ok(Accepted::Rejected(format!("credenziali del peer: {e}"))),
        };
        if cred.uid() != self.own_uid {
            return Ok(Accepted::Rejected(format!(
                "UID del peer {} diverso da {}",
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
    fn private_dir_rejects_symlink() {
        let d = crate::test_support::temp_dir("privdir-link");
        let real = d.join("real");
        ensure_private_dir(&real).unwrap();
        let link = d.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
    }
}
