use std::fmt;

/// RFB client errors. Protocol errors close the connection.
#[derive(Debug)]
pub enum Error {
    /// I/O error on the transport.
    Io(std::io::Error),
    /// TLS error (handshake, certificate verification, configuration).
    Tls(String),
    /// The server sent non-conforming or unexpected data.
    Protocol(String),
    /// The server requires an unsupported version or feature.
    Unsupported(String),
    /// The server refused the connection or authentication, with a reason.
    Refused(String),
    /// The channel is not protected: security `None` offered on cleartext TCP.
    InsecureChannel(String),
    /// Invalid client configuration (paths, certificates).
    Config(String),
    /// Connection closed (by the server or by [`crate::Session::stop`]).
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Tls(e) => write!(f, "TLS: {e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Unsupported(e) => write!(f, "unsupported: {e}"),
            Self::Refused(e) => write!(f, "refused by the server: {e}"),
            Self::InsecureChannel(e) => write!(f, "unprotected channel: {e}"),
            Self::Config(e) => write!(f, "configuration: {e}"),
            Self::Closed => write!(f, "connection closed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::Closed
        } else {
            Self::Io(e)
        }
    }
}

impl From<rustls::Error> for Error {
    fn from(e: rustls::Error) -> Self {
        Self::Tls(e.to_string())
    }
}
