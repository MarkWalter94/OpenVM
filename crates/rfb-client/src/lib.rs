//! `rfb-client`: minimal, **read-only** RFB 3.8 client for the LightVM
//! console (ROADMAP 6.4, ADR-0003, ADR-0013). Status: F0 spike, step 3
//! (verification `docs/verification/F0-04-console-rfb-input.md`).
//!
//! What it does:
//! - version 3.8 handshake;
//! - security `None` accepted **only** on a private Unix socket; on TCP it accepts
//!   only VeNCrypt with the X509None subtype (TLS with server verification and a
//!   client certificate), and rejects cleartext `None`;
//! - shared ClientInit, ServerInit (geometry, pixel format, name);
//! - 32 bpp BGRX pixel format (little endian, R<<16 | G<<8 | B);
//! - Raw, CopyRect, Hextile, ZRLE encodings and the DesktopSize pseudo-encoding;
//! - loop of incremental FramebufferUpdateRequests with a rate limit.
//!
//! What it does **not** do, by construction: the crate contains no code that
//! builds client-to-server messages of type 4, 5 and 6 (keyboard events,
//! pointer events and clipboard text). The only write path after the
//! handshake is [`ClientMessage`], which has only three variants (types 0, 2, 3);
//! the property is checked by automated tests (`tests/read_only.rs`).
//! Received ServerCutText messages are discarded without being delivered.
//! User input goes through QMP (ROADMAP 6.4), outside this crate.

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

/// Maximum accepted size per framebuffer side (defense against hostile servers).
pub const MAX_DIMENSION: u16 = 8192;
