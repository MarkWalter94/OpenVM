//! Tipi e codifica dei messaggi RFB usati dal client.
//!
//! Messaggi client-server implementati (RFC 6143, sezione 7.5): soltanto
//! SetPixelFormat (0), SetEncodings (2) e FramebufferUpdateRequest (3).

use std::io::Read;

use crate::{Error, MAX_DIMENSION, Rect};

/// Tipi di messaggio client-server che questo crate può emettere dopo
/// l'handshake. Qualunque altro tipo è assente per costruzione.
pub const CLIENT_MESSAGE_TYPES_SENT: [u8; 3] = [0, 2, 3];

/// Encoding RFB riconosciuti. I valori negativi sono pseudo-encoding.
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
    /// Pseudo-encoding DesktopSize (-223).
    DesktopSize,
}

impl Encoding {
    /// Codice numerico dell'encoding.
    pub const fn code(self) -> i32 {
        match self {
            Self::Raw => 0,
            Self::CopyRect => 1,
            Self::Hextile => 5,
            Self::Zrle => 16,
            Self::DesktopSize => -223,
        }
    }

    /// Encoding dal codice; `None` per i codici non previsti (rifiutati).
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

/// Encoding annunciati al server, in ordine di preferenza.
pub const SUPPORTED_ENCODINGS: [Encoding; 5] = [
    Encoding::Zrle,
    Encoding::Hextile,
    Encoding::CopyRect,
    Encoding::Raw,
    Encoding::DesktopSize,
];

/// Formato pixel RFB (RFC 6143, 7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat {
    /// Bit per pixel.
    pub bits_per_pixel: u8,
    /// Profondità utile.
    pub depth: u8,
    /// Big endian.
    pub big_endian: bool,
    /// True colour.
    pub true_colour: bool,
    /// Valore massimo del rosso.
    pub red_max: u16,
    /// Valore massimo del verde.
    pub green_max: u16,
    /// Valore massimo del blu.
    pub blue_max: u16,
    /// Shift del rosso.
    pub red_shift: u8,
    /// Shift del verde.
    pub green_shift: u8,
    /// Shift del blu.
    pub blue_shift: u8,
}

impl PixelFormat {
    /// 32 bpp, profondità 24, little endian, R<<16 | G<<8 | B: in memoria B, G, R, X.
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

    /// Serializzazione a 16 byte (incluso il padding).
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

    /// Lettura da 16 byte.
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

/// Dati del ServerInit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInit {
    /// Larghezza iniziale.
    pub width: u16,
    /// Altezza iniziale.
    pub height: u16,
    /// Formato pixel proposto dal server (il client poi impone BGRX8888).
    pub pixel_format: PixelFormat,
    /// Nome del desktop (UTF-8 con sostituzione, troncato a 256 caratteri).
    pub name: String,
}

/// Messaggi client-server che il crate sa costruire. È l'**unico** tipo che
/// può essere scritto sul trasporto dopo l'handshake.
///
/// Non esistono varianti per eventi di tastiera, di puntatore o per il testo
/// degli appunti: non sono implementati (ROADMAP 6.4, display in sola lettura).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// SetPixelFormat (tipo 0).
    SetPixelFormat(PixelFormat),
    /// SetEncodings (tipo 2).
    SetEncodings(Vec<Encoding>),
    /// FramebufferUpdateRequest (tipo 3).
    FramebufferUpdateRequest {
        /// Richiesta incrementale.
        incremental: bool,
        /// Area richiesta.
        rect: Rect,
    },
}

impl ClientMessage {
    /// Tipo di messaggio RFB.
    pub const fn message_type(&self) -> u8 {
        match self {
            Self::SetPixelFormat(_) => 0,
            Self::SetEncodings(_) => 2,
            Self::FramebufferUpdateRequest { .. } => 3,
        }
    }

    /// Codifica in byte. Il primo byte è sempre [`Self::message_type`].
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

// ---- lettura ---------------------------------------------------------------

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

/// Scarta `n` byte senza conservarli.
pub(crate) fn skip(r: &mut impl Read, n: u64) -> Result<(), Error> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink())?;
    if copied != n {
        return Err(Error::Closed);
    }
    Ok(())
}

/// Stringa con lunghezza u32, limitata a `max` byte (oltre: errore).
pub(crate) fn read_string(r: &mut impl Read, max: u32) -> Result<String, Error> {
    let len = read_u32(r)?;
    if len > max {
        return Err(Error::Protocol(format!(
            "stringa di {len} byte oltre il limite {max}"
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
            "geometria {width}x{height} oltre il limite"
        )));
    }
    Ok(ServerInit {
        width,
        height,
        pixel_format: PixelFormat::from_bytes(&pf),
        name,
    })
}
