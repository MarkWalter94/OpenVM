//! Trasporto IPC locale (ROADMAP 6.1, 9).
//!
//! - Windows: named pipe con descrittore di sicurezza che ammette solo il SID
//!   dell'utente corrente, etichetta di integrità media con `NW/NR/NX` (un
//!   processo a integrità bassa non apre la pipe), client remoti rifiutati,
//!   flag di prima istanza. Dopo l'accettazione: stessa sessione di logon e
//!   stesso SID del processo peer.
//! - Unix: socket in una directory privata `0700` (per difetto
//!   `$XDG_RUNTIME_DIR/lightvm`), socket `0600`, verifica dell'UID del peer con
//!   `SO_PEERCRED`/`getpeereid` (tramite `tokio::net::UnixStream::peer_cred`).
//!
//! L'identità OS è una condizione necessaria, non sufficiente: la credenziale
//! viene verificata sempre nell'handshake (`server`).

use std::io;
use std::path::PathBuf;

use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

/// Flusso bidirezionale di una connessione IPC.
pub trait IpcStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> IpcStream for T {}

/// Flusso in scatola, indipendente dalla piattaforma.
pub type BoxedStream = Box<dyn IpcStream>;

/// Identità del peer accettata dal trasporto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// PID riportato dal sistema operativo (solo diagnostico, mai usato da solo
    /// come identità).
    pub pid: Option<u32>,
    /// Identità OS verificata (SID su Windows, UID su Unix).
    pub os_identity: String,
}

/// Opzioni di creazione dell'endpoint.
#[derive(Debug, Clone, Default)]
pub struct EndpointOptions {
    /// Directory privata per il socket (solo Unix). Se assente si usa
    /// `$XDG_RUNTIME_DIR/lightvm`, poi `<catalogo>/.run`.
    pub runtime_dir: Option<PathBuf>,
    /// Directory del catalogo (ripiego per la directory privata su Unix).
    pub catalog_dir: PathBuf,
}

/// Listener dell'endpoint locale.
#[derive(Debug)]
pub struct Listener {
    inner: imp::Listener,
}

/// Esito di un'accettazione: connessione con peer verificato, oppure peer
/// rifiutato dal controllo di identità OS (il flusso viene chiuso).
#[derive(Debug)]
pub enum Accepted {
    /// Peer verificato.
    Ok(BoxedStream, PeerInfo),
    /// Peer rifiutato, con motivo diagnostico.
    Rejected(String),
}

impl std::fmt::Debug for dyn IpcStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IpcStream")
    }
}

impl Listener {
    /// Crea l'endpoint con permessi restrittivi.
    pub fn bind(opts: &EndpointOptions) -> io::Result<Self> {
        Ok(Self {
            inner: imp::Listener::bind(opts)?,
        })
    }

    /// Nome dell'endpoint da consegnare ai client (percorso della pipe o del
    /// socket). Non è un segreto.
    pub fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }

    /// Attende la prossima connessione e verifica l'identità OS del peer.
    pub async fn accept(&self) -> io::Result<Accepted> {
        self.inner.accept().await
    }
}

/// Connessione di un client all'endpoint.
pub async fn connect(endpoint: &str) -> io::Result<BoxedStream> {
    imp::connect(endpoint).await
}

/// Nome casuale esadecimale per l'endpoint.
pub(crate) fn random_suffix() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    Ok(crate::credential::hex_encode(&b))
}
