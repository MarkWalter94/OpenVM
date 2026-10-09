//! Trasporti del client RFB: socket Unix privato (due implementazioni su
//! Windows) e TCP loopback con VeNCrypt X.509 + certificato client (rustls).
//! Non esiste un trasporto TCP in chiaro con sicurezza `None`.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::Error;

/// Implementazione del socket AF_UNIX da usare.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnixImpl {
    /// Crate `socket2` (`Domain::UNIX` + `SockAddr::unix`).
    #[default]
    Socket2,
    /// Crate `uds_windows` (solo Windows).
    UdsWindows,
}

/// Certificati per VeNCrypt X.509 (file PEM).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsConfig {
    /// Certificato della CA che firma il certificato del server.
    pub ca_cert: PathBuf,
    /// Certificato del client (catena PEM).
    pub client_cert: PathBuf,
    /// Chiave privata del client (PEM, PKCS#8 / PKCS#1 / SEC1).
    pub client_key: PathBuf,
    /// Nome atteso nel certificato del server (DNS o IP).
    pub server_name: String,
}

/// Trasporto verso il server VNC di QEMU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Socket Unix in una directory privata (`-vnc unix:<percorso>`).
    Unix {
        /// Percorso del socket.
        path: PathBuf,
        /// Implementazione.
        imp: UnixImpl,
    },
    /// TCP (solo loopback) con VeNCrypt X509None e certificato client.
    VencryptTls {
        /// Indirizzo del server (deve essere di loopback).
        addr: SocketAddr,
        /// Certificati.
        tls: TlsConfig,
    },
}

/// Natura del canale su cui si svolge l'handshake di sicurezza.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    /// Socket Unix: protezione affidata all'ACL della directory privata.
    PrivateUnixSocket,
    /// TCP prima dell'upgrade TLS: `None` non è mai accettato.
    PlainTcp,
}

pub(crate) trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Socket condiviso tra il thread che legge e chi chiude la connessione.
///
/// Su Windows (F0-04) `shutdown` applicato a un duplicato ottenuto con
/// `try_clone` (WSADuplicateSocket) non sblocca una `recv` in corso sul socket
/// originale, e un timeout di `recv` (`SO_RCVTIMEO`) su AF_UNIX lascia il
/// flusso in uno stato in cui i dati successivi non arrivano più. Per questo
/// lettura, scrittura e chiusura usano lo **stesso** handle, condiviso con
/// `Arc`, e il timeout di lettura è attivo solo durante l'handshake (scadenza
/// che, se scatta, chiude comunque la connessione).
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
                // Semichiusura: il server riceve EOF e chiude, e la `recv` in
                // corso termina. Su Windows `Shutdown::Both` non la sblocca (F0-04).
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

/// Flusso di lettura/scrittura sul socket condiviso.
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

/// Controllo del socket da un altro thread (chiusura, timeout).
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
    /// Sveglia un thread bloccato in lettura chiedendo al server un
    /// aggiornamento non incrementale di 1x1 pixel (messaggio di tipo 3, lo
    /// stesso del ciclo normale). Su TLS i byte in chiaro non sono validi e il
    /// server chiude la connessione: anche questo sblocca la lettura.
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

/// Apre un socket AF_UNIX con l'implementazione indicata.
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
            "uds_windows è disponibile solo su Windows",
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
        Transport::VencryptTls { addr, tls } => {
            if !addr.ip().is_loopback() {
                return Err(Error::Config(format!(
                    "indirizzo {addr} non di loopback: rifiutato"
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

/// Esegue l'handshake TLS (rustls, provider ring) sul flusso indicato e
/// restituisce il flusso cifrato con una descrizione della sessione.
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
        .map_err(|e| pem_err("certificato client", e))?
        .collect::<Result<_, _>>()
        .map_err(|e| pem_err("certificato client", e))?;
    let key =
        PrivateKeyDer::from_pem_file(&cfg.client_key).map_err(|e| pem_err("chiave client", e))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)?;
    let name = ServerName::try_from(cfg.server_name.clone())
        .map_err(|e| Error::Config(format!("nome server: {e}")))?;
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
