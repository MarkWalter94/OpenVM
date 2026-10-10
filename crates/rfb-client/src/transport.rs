//! RFB client transports: private Unix socket (two implementations on
//! Windows) and loopback TCP with VeNCrypt X.509 + client certificate (rustls).
//! There is no cleartext TCP transport with security `None`.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::Error;

/// AF_UNIX socket implementation to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnixImpl {
    /// `socket2` crate (`Domain::UNIX` + `SockAddr::unix`).
    #[default]
    Socket2,
    /// `uds_windows` crate (Windows only).
    UdsWindows,
}

/// Certificates for VeNCrypt X.509 (PEM files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsConfig {
    /// Certificate of the CA that signs the server certificate.
    pub ca_cert: PathBuf,
    /// Client certificate (PEM chain).
    pub client_cert: PathBuf,
    /// Client private key (PEM, PKCS#8 / PKCS#1 / SEC1).
    pub client_key: PathBuf,
    /// Name expected in the server certificate (DNS or IP).
    pub server_name: String,
}

/// Transport to the QEMU VNC server. `Debug` redacts the display ticket.
#[derive(Clone, PartialEq, Eq)]
pub enum Transport {
    /// Unix socket in a private directory (`-vnc unix:<path>`).
    Unix {
        /// Socket path.
        path: PathBuf,
        /// Implementation.
        imp: UnixImpl,
    },
    /// Per-run display socket of the supervisor (ADR-0013): Unix socket in a
    /// private directory; the client first writes the display ticket followed
    /// by `\n`, then speaks RFB. The supervisor forwards to QEMU only after
    /// checking the ticket.
    Ticket {
        /// Socket path from the ticket.
        path: PathBuf,
        /// Implementation.
        imp: UnixImpl,
        /// Display credential (hexadecimal, never logged).
        token: String,
    },
    /// TCP (loopback only) with VeNCrypt X509None and a client certificate.
    VencryptTls {
        /// Server address (must be loopback).
        addr: SocketAddr,
        /// Certificates.
        tls: TlsConfig,
    },
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix { path, imp } => f
                .debug_struct("Unix")
                .field("path", path)
                .field("imp", imp)
                .finish(),
            Self::Ticket { path, imp, .. } => f
                .debug_struct("Ticket")
                .field("path", path)
                .field("imp", imp)
                .field("token", &"<redacted>")
                .finish(),
            Self::VencryptTls { addr, tls } => f
                .debug_struct("VencryptTls")
                .field("addr", addr)
                .field("tls", tls)
                .finish(),
        }
    }
}

/// Nature of the channel on which the security handshake takes place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    /// Unix socket: protection relies on the ACL of the private directory.
    PrivateUnixSocket,
    /// TCP before the TLS upgrade: `None` is never accepted.
    PlainTcp,
}

pub(crate) trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Socket shared between the reading thread and whoever closes the connection.
///
/// On Windows (F0-04) `shutdown` applied to a duplicate obtained with
/// `try_clone` (WSADuplicateSocket) does not unblock an in-progress `recv` on the original
/// socket, and a `recv` timeout (`SO_RCVTIMEO`) on AF_UNIX leaves the
/// stream in a state where subsequent data no longer arrives. That is why
/// reading, writing and closing use the **same** handle, shared with
/// `Arc`, and the read timeout is active only during the handshake (a deadline
/// that, if it fires, closes the connection anyway).
pub(crate) trait RawSock: Send + Sync + 'static {
    fn read_s(&self, b: &mut [u8]) -> io::Result<usize>;
    fn write_s(&self, b: &[u8]) -> io::Result<usize>;
    fn flush_s(&self) -> io::Result<()>;
    fn shutdown_both(&self) -> io::Result<()>;
    fn set_timeout(&self, d: Option<Duration>) -> io::Result<()>;
}

macro_rules! raw_sock {
    ($t:ty) => {
        impl RawSock for $t {
            fn read_s(&self, b: &mut [u8]) -> io::Result<usize> {
                (&*self).read(b)
            }
            fn write_s(&self, b: &[u8]) -> io::Result<usize> {
                (&*self).write(b)
            }
            fn flush_s(&self) -> io::Result<()> {
                (&*self).flush()
            }
            fn shutdown_both(&self) -> io::Result<()> {
                // Half-close: the server receives EOF and closes, and the in-progress
                // `recv` ends. On Windows `Shutdown::Both` does not unblock it (F0-04).
                self.shutdown(Shutdown::Write)
            }
            fn set_timeout(&self, d: Option<Duration>) -> io::Result<()> {
                self.set_read_timeout(d)
            }
        }
    };
}

raw_sock!(socket2::Socket);
raw_sock!(TcpStream);
#[cfg(windows)]
raw_sock!(uds_windows::UnixStream);

/// Read/write stream on the shared socket.
struct Shared(Arc<dyn RawSock>);

impl Read for Shared {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read_s(b)
    }
}

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write_s(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush_s()
    }
}

/// Control of the socket from another thread (close, timeout).
#[derive(Clone)]
pub(crate) struct SocketControl(Arc<dyn RawSock>);

impl std::fmt::Debug for SocketControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SocketControl")
    }
}

impl SocketControl {
    pub(crate) fn shutdown(&self) {
        let _ = self.0.shutdown_both();
    }
    pub(crate) fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.0.set_timeout(d)
    }
    /// Wakes a thread blocked on a read by asking the server for a
    /// non-incremental 1x1 pixel update (type 3 message, the
    /// same as the normal loop). On TLS cleartext bytes are invalid and the
    /// server closes the connection: this unblocks the read too.
    pub(crate) fn wake(&self) {
        let m = crate::ClientMessage::FramebufferUpdateRequest {
            incremental: false,
            rect: crate::Rect::new(0, 0, 1, 1),
        };
        let _ = self.0.write_s(&m.encode());
    }
}

fn share<S: RawSock>(s: S) -> (Box<dyn Stream>, SocketControl) {
    let a: Arc<dyn RawSock> = Arc::new(s);
    (Box::new(Shared(a.clone())), SocketControl(a))
}

pub(crate) struct Opened {
    pub(crate) stream: Box<dyn Stream>,
    pub(crate) kind: ChannelKind,
    pub(crate) control: SocketControl,
    pub(crate) tls: Option<TlsConfig>,
}

/// Opens an AF_UNIX socket with the given implementation.
pub(crate) fn open_unix(
    path: &std::path::Path,
    imp: UnixImpl,
) -> io::Result<(Box<dyn Stream>, SocketControl)> {
    match imp {
        UnixImpl::Socket2 => {
            let s = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
            s.connect(&socket2::SockAddr::unix(path)?)?;
            Ok(share(s))
        }
        #[cfg(windows)]
        UnixImpl::UdsWindows => Ok(share(uds_windows::UnixStream::connect(path)?)),
        #[cfg(not(windows))]
        UnixImpl::UdsWindows => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "uds_windows is available only on Windows",
        )),
    }
}

pub(crate) fn open(t: &Transport) -> Result<Opened, Error> {
    match t {
        Transport::Unix { path, imp } => {
            let (stream, control) = open_unix(path, *imp)?;
            Ok(Opened {
                stream,
                kind: ChannelKind::PrivateUnixSocket,
                control,
                tls: None,
            })
        }
        Transport::Ticket { path, imp, token } => {
            if token.is_empty() || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Config("invalid display ticket".into()));
            }
            let (mut stream, control) = open_unix(path, *imp)?;
            stream.write_all(token.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            Ok(Opened {
                stream,
                kind: ChannelKind::PrivateUnixSocket,
                control,
                tls: None,
            })
        }
        Transport::VencryptTls { addr, tls } => {
            if !addr.ip().is_loopback() {
                return Err(Error::Config(format!(
                    "address {addr} is not loopback: refused"
                )));
            }
            let s = TcpStream::connect_timeout(addr, Duration::from_secs(5))?;
            s.set_nodelay(true)?;
            let (stream, control) = share(s);
            Ok(Opened {
                stream,
                kind: ChannelKind::PlainTcp,
                control,
                tls: Some(tls.clone()),
            })
        }
    }
}

/// Performs the TLS handshake (rustls, ring provider) on the given stream and
/// returns the encrypted stream with a description of the session.
pub(crate) fn tls_upgrade(
    mut stream: Box<dyn Stream>,
    cfg: &TlsConfig,
) -> Result<(Box<dyn Stream>, String), Error> {
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

    let pem_err =
        |what: &str, e: rustls::pki_types::pem::Error| Error::Config(format!("{what}: {e}"));
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(&cfg.ca_cert).map_err(|e| pem_err("CA", e))? {
        roots
            .add(c.map_err(|e| pem_err("CA", e))?)
            .map_err(|e| Error::Config(format!("CA: {e}")))?;
    }
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cfg.client_cert)
        .map_err(|e| pem_err("client certificate", e))?
        .collect::<Result<_, _>>()
        .map_err(|e| pem_err("client certificate", e))?;
    let key =
        PrivateKeyDer::from_pem_file(&cfg.client_key).map_err(|e| pem_err("client key", e))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)?;
    let name = ServerName::try_from(cfg.server_name.clone())
        .map_err(|e| Error::Config(format!("server name: {e}")))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), name)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut stream)
            .map_err(|e| Error::Tls(e.to_string()))?;
    }
    let info = format!(
        "{:?} {:?}",
        conn.protocol_version(),
        conn.negotiated_cipher_suite().map(|c| c.suite())
    );
    Ok((Box::new(rustls::StreamOwned::new(conn, stream)), info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_debug_redacts_the_token() {
        let token = "0123456789abcdef".repeat(4);
        let t = Transport::Ticket {
            path: PathBuf::from("d.sock"),
            imp: UnixImpl::Socket2,
            token: token.clone(),
        };
        let d = format!("{t:?}");
        assert!(!d.contains(&token), "{d}");
        assert!(d.contains("redacted") && d.contains("d.sock"));
    }
}
