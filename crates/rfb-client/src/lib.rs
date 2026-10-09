//! `rfb-client`: client RFB 3.8 minimo, **in sola lettura**, per la console di
//! LightVM (ROADMAP 6.4, ADR-0003, ADR-0013). Stato: spike di F0, passo 3
//! (verifica `docs/verification/F0-04-console-rfb-input.md`).
//!
//! Che cosa fa:
//! - handshake di versione 3.8;
//! - sicurezza `None` accettata **solo** su socket Unix privato; su TCP accetta
//!   soltanto VeNCrypt con sottotipo X509None (TLS con verifica del server e
//!   certificato client), e rifiuta `None` in chiaro;
//! - ClientInit condiviso, ServerInit (geometria, formato pixel, nome);
//! - formato pixel 32 bpp BGRX (little endian, R<<16 | G<<8 | B);
//! - encoding Raw, CopyRect, Hextile, ZRLE e pseudo-encoding DesktopSize;
//! - ciclo di FramebufferUpdateRequest incrementali con limite di frequenza.
//!
//! Che cosa **non** fa, per costruzione: il crate non contiene codice che
//! costruisca i messaggi client-server di tipo 4, 5 e 6 (eventi di tastiera, di
//! puntatore e testo degli appunti). L'unico percorso di scrittura dopo
//! l'handshake è [`ClientMessage`], che ha tre sole varianti (tipi 0, 2, 3);
//! la proprietà è verificata da test automatici (`tests/sola_lettura.rs`).
//! I messaggi ServerCutText ricevuti sono scartati senza essere consegnati.
//! L'input dell'utente passa da QMP (ROADMAP 6.4), fuori da questo crate.

#![forbid(unsafe_code)]

mod conn;
mod decode;
mod error;
mod framebuffer;
mod proto;
mod session;
mod transport;

pub use conn::{Connection, ConnectionStats, UpdateSummary};
pub use error::Error;
pub use framebuffer::{Framebuffer, Rect};
pub use proto::{
    CLIENT_MESSAGE_TYPES_SENT, ClientMessage, Encoding, PixelFormat, SUPPORTED_ENCODINGS,
    ServerInit,
};
pub use session::{Session, SessionConfig, SessionEvent, UpdateInfo};
pub use transport::{ChannelKind, TlsConfig, Transport, UnixImpl};

/// Dimensione massima accettata per lato del framebuffer (difesa da server ostili).
pub const MAX_DIMENSION: u16 = 8192;
