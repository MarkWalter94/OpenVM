use std::fmt;

/// Errori del client RFB. Gli errori di protocollo chiudono la connessione.
#[derive(Debug)]
pub enum Error {
    /// Errore di I/O sul trasporto.
    Io(std::io::Error),
    /// Errore TLS (handshake, verifica del certificato, configurazione).
    Tls(String),
    /// Il server ha inviato dati non conformi o non previsti.
    Protocol(String),
    /// Il server richiede una versione o una funzione non supportata.
    Unsupported(String),
    /// Il server ha rifiutato la connessione o l'autenticazione, con motivo.
    Refused(String),
    /// Il canale non è protetto: sicurezza `None` offerta su TCP in chiaro.
    InsecureChannel(String),
    /// Configurazione del client non valida (percorsi, certificati).
    Config(String),
    /// Connessione chiusa (dal server o da [`crate::Session::stop`]).
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Tls(e) => write!(f, "TLS: {e}"),
            Self::Protocol(e) => write!(f, "protocollo: {e}"),
            Self::Unsupported(e) => write!(f, "non supportato: {e}"),
            Self::Refused(e) => write!(f, "rifiutato dal server: {e}"),
            Self::InsecureChannel(e) => write!(f, "canale non protetto: {e}"),
            Self::Config(e) => write!(f, "configurazione: {e}"),
            Self::Closed => write!(f, "connessione chiusa"),
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
