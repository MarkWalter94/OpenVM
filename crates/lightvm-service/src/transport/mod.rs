//! Local IPC transport (ROADMAP 6.1, 9).
//!
//! - Windows: named pipe with a security descriptor that admits only the SID
//!   of the current user, medium integrity label with `NW/NR/NX` (a
//!   low-integrity process cannot open the pipe), remote clients rejected,
//!   first-instance flag. After acceptance: same logon session and same SID
//!   of the peer process.
//! - Unix: socket in a private `0700` directory (by default
//!   `$XDG_RUNTIME_DIR/lightvm`), socket `0600`, peer UID check with
//!   `SO_PEERCRED`/`getpeereid` (through `tokio::net::UnixStream::peer_cred`).
//!
//! The OS identity is a necessary condition, not a sufficient one: the
//! credential is always verified in the handshake (`server`).

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

/// Prefix of the fallback private roots in the temporary directory
/// (`lightvm-p-<pid>`, used when the catalog path is too long for the
/// AF_UNIX sockets).
pub const FALLBACK_ROOT_PREFIX: &str = "lightvm-p-";

/// Fallback private root of this process in the user's temporary folder
/// (`<temp>/lightvm-p-<pid>`), shared by the QEMU sockets and the IPC
/// endpoint when the configured paths are too long for AF_UNIX.
pub fn fallback_root() -> PathBuf {
    std::env::temp_dir().join(format!("{FALLBACK_ROOT_PREFIX}{}", std::process::id()))
}

/// Bidirectional stream of an IPC connection.
pub trait IpcStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> IpcStream for T {}

/// Boxed stream, platform independent.
pub type BoxedStream = Box<dyn IpcStream>;

/// Peer identity accepted by the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// PID reported by the operating system (diagnostic only, never used on its
    /// own as an identity).
    pub pid: Option<u32>,
    /// Verified OS identity (SID on Windows, UID on Unix).
    pub os_identity: String,
}

/// Endpoint creation options.
#[derive(Debug, Clone, Default)]
pub struct EndpointOptions {
    /// Private directory for the socket (Unix only). If absent,
    /// `$XDG_RUNTIME_DIR/lightvm` is used, then `<catalog>/.run`.
    pub runtime_dir: Option<PathBuf>,
    /// Catalog directory (fallback for the private directory on Unix).
    pub catalog_dir: PathBuf,
}

/// Listener of the local endpoint.
#[derive(Debug)]
pub struct Listener {
    inner: imp::Listener,
}

/// Outcome of an accept: connection with a verified peer, or a peer rejected
/// by the OS identity check (the stream is closed).
#[derive(Debug)]
pub enum Accepted {
    /// Verified peer.
    Ok(BoxedStream, PeerInfo),
    /// Rejected peer, with a diagnostic reason.
    Rejected(String),
}

impl std::fmt::Debug for dyn IpcStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IpcStream")
    }
}

impl Listener {
    /// Creates the endpoint with restrictive permissions.
    pub fn bind(opts: &EndpointOptions) -> io::Result<Self> {
        Ok(Self {
            inner: imp::Listener::bind(opts)?,
        })
    }

    /// Endpoint name to hand to clients (pipe or socket path). It is not a
    /// secret.
    pub fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }

    /// Waits for the next connection and verifies the peer's OS identity.
    pub async fn accept(&self) -> io::Result<Accepted> {
        self.inner.accept().await
    }
}

/// Connects a client to the endpoint.
pub async fn connect(endpoint: &str) -> io::Result<BoxedStream> {
    imp::connect(endpoint).await
}

/// Random hexadecimal name for the endpoint.
pub(crate) fn random_suffix() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    Ok(crate::credential::hex_encode(&b))
}
