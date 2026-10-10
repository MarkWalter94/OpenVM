//! Types and encoding of the RFB messages used by the client.
//!
//! Implemented client-to-server messages (RFC 6143, section 7.5): only
//! SetPixelFormat (0), SetEncodings (2) and FramebufferUpdateRequest (3).

use std::io::Read;

use crate::{Error, MAX_DIMENSION, Rect};

/// Client-to-server message types this crate can emit after the
/// handshake. Any other type is absent by construction.
pub const CLIENT_MESSAGE_TYPES_SENT: [u8; 3] = [0, 2, 3];

/// Recognized RFB encodings. Negative values are pseudo-encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Raw (0).
    Raw,
    /// CopyRect (1).
    CopyRect,
    /// Hextile (5).
    Hextile,
    /// ZRLE (16).
    Zrle,
    /// DesktopSize pseudo-encoding (-223).
    DesktopSize,
}

impl Encoding {
    /// Numeric code of the encoding.
    pub const fn code(self) -> i32 {
        match self {
            Self::Raw => 0,
            Self::CopyRect => 1,
            Self::Hextile => 5,
            Self::Zrle => 16,
            Self::DesktopSize => -223,
        }
    }

    /// Encoding from its code; `None` for unexpected codes (rejected).
    pub const fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Raw,
            1 => Self::CopyRect,
            5 => Self::Hextile,
            16 => Self::Zrle,
            -223 => Self::DesktopSize,
            _ => return None,
        })
    }
}

/// Encodings announced to the server, in order of preference.
pub const SUPPORTED_ENCODINGS: [Encoding; 5] = [
    Encoding::Zrle,
    Encoding::Hextile,
    Encoding::CopyRect,
    Encoding::Raw,
    Encoding::DesktopSize,
];

/// RFB pixel format (RFC 6143, 7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat {
    /// Bits per pixel.
    pub bits_per_pixel: u8,
    /// Useful depth.
    pub depth: u8,
    /// Big endian.
    pub big_endian: bool,
    /// True colour.
    pub true_colour: bool,
    /// Maximum red value.
    pub red_max: u16,
    /// Maximum green value.
    pub green_max: u16,
    /// Maximum blue value.
    pub blue_max: u16,
    /// Red shift.
    pub red_shift: u8,
    /// Green shift.
    pub green_shift: u8,
    /// Blue shift.
    pub blue_shift: u8,
}

impl PixelFormat {
    /// 32 bpp, depth 24, little endian, R<<16 | G<<8 | B: in memory B, G, R, X.
    pub const BGRX8888: Self = Self {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    /// 16-byte serialization (padding included).
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = u8::from(self.big_endian);
        b[3] = u8::from(self.true_colour);
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }

    /// Reads from 16 bytes.
    pub fn from_bytes(b: &[u8; 16]) -> Self {
        Self {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
        }
    }
}

/// ServerInit data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInit {
    /// Initial width.
    pub width: u16,
    /// Initial height.
    pub height: u16,
    /// Pixel format proposed by the server (the client then imposes BGRX8888).
    pub pixel_format: PixelFormat,
    /// Desktop name (UTF-8 with replacement, truncated to 256 characters).
    pub name: String,
}

/// Client-to-server messages the crate can build. It is the **only** type that
/// can be written to the transport after the handshake.
///
/// There are no variants for keyboard events, pointer events or clipboard
/// text: they are not implemented (ROADMAP 6.4, read-only display).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// SetPixelFormat (type 0).
    SetPixelFormat(PixelFormat),
    /// SetEncodings (type 2).
    SetEncodings(Vec<Encoding>),
    /// FramebufferUpdateRequest (type 3).
    FramebufferUpdateRequest {
        /// Incremental request.
        incremental: bool,
        /// Requested area.
        rect: Rect,
    },
}

impl ClientMessage {
    /// RFB message type.
    pub const fn message_type(&self) -> u8 {
        match self {
            Self::SetPixelFormat(_) => 0,
            Self::SetEncodings(_) => 2,
            Self::FramebufferUpdateRequest { .. } => 3,
        }
    }

    /// Encodes to bytes. The first byte is always [`Self::message_type`].
    pub fn encode(&self) -> Vec<u8> {
        let mut v = vec![self.message_type()];
        match self {
            Self::SetPixelFormat(pf) => {
                v.extend_from_slice(&[0, 0, 0]);
                v.extend_from_slice(&pf.to_bytes());
            }
            Self::SetEncodings(encs) => {
                v.push(0);
                v.extend_from_slice(&(encs.len() as u16).to_be_bytes());
                for e in encs {
                    v.extend_from_slice(&e.code().to_be_bytes());
                }
            }
            Self::FramebufferUpdateRequest { incremental, rect } => {
                v.push(u8::from(*incremental));
                for n in [rect.x, rect.y, rect.w, rect.h] {
                    v.extend_from_slice(&n.to_be_bytes());
                }
            }
        }
        v
    }
}

// ---- reading ---------------------------------------------------------------

pub(crate) fn read_u8(r: &mut impl Read) -> Result<u8, Error> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

pub(crate) fn read_u16(r: &mut impl Read) -> Result<u16, Error> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

pub(crate) fn read_u32(r: &mut impl Read) -> Result<u32, Error> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

pub(crate) fn read_i32(r: &mut impl Read) -> Result<i32, Error> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_be_bytes(b))
}

pub(crate) fn read_vec(r: &mut impl Read, n: usize) -> Result<Vec<u8>, Error> {
    let mut v = vec![0u8; n];
    r.read_exact(&mut v)?;
    Ok(v)
}

/// Discards `n` bytes without keeping them.
pub(crate) fn skip(r: &mut impl Read, n: u64) -> Result<(), Error> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink())?;
    if copied != n {
        return Err(Error::Closed);
    }
    Ok(())
}

/// String with a u32 length, limited to `max` bytes (beyond: error).
pub(crate) fn read_string(r: &mut impl Read, max: u32) -> Result<String, Error> {
    let len = read_u32(r)?;
    if len > max {
        return Err(Error::Protocol(format!(
            "string of {len} bytes over the {max} limit"
        )));
    }
    let v = read_vec(r, len as usize)?;
    Ok(String::from_utf8_lossy(&v).chars().take(256).collect())
}

pub(crate) fn read_server_init(r: &mut impl Read) -> Result<ServerInit, Error> {
    let width = read_u16(r)?;
    let height = read_u16(r)?;
    let mut pf = [0u8; 16];
    r.read_exact(&mut pf)?;
    let name = read_string(r, 4096)?;
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Error::Protocol(format!(
            "geometry {width}x{height} over the limit"
        )));
    }
    Ok(ServerInit {
        width,
        height,
        pixel_format: PixelFormat::from_bytes(&pf),
        name,
    })
}
