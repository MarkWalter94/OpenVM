//! Synchronous RFB connection: handshake and reading of updates.

use std::io::{BufReader, Read, Write};
use std::time::Duration;

use crate::decode::{self, ZrleState};
use crate::proto::{
    ClientMessage, Encoding, PixelFormat, SUPPORTED_ENCODINGS, ServerInit, read_i32,
    read_server_init, read_string, read_u8, read_u16, read_u32, skip,
};
use crate::transport::{self, ChannelKind, SocketControl, Stream, TlsConfig};
use crate::{Error, Framebuffer, MAX_DIMENSION, Rect, Transport};

const SEC_NONE: u8 = 1;
const SEC_VENCRYPT: u8 = 19;
const VENCRYPT_X509NONE: u32 = 260;
/// Limit on discarded ServerCutText texts (beyond it: protocol error).
const MAX_CUT_TEXT: u32 = 1024 * 1024;
/// Read timeout during the handshake: avoids indefinite waits (for
/// example a VeNCrypt server that does not close after a rejected certificate).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Connection counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionStats {
    /// Processed FramebufferUpdate messages.
    pub updates: u64,
    /// Received rectangles (including pseudo-encodings).
    pub rects: u64,
    /// Rectangles per encoding: Raw, CopyRect, Hextile, ZRLE, DesktopSize.
    pub rects_by_encoding: [u64; 5],
    /// Updated pixels (sum of the areas, excluding DesktopSize).
    pub pixels: u64,
    /// Received resizes.
    pub resizes: u64,
    /// Ignored Bell messages.
    pub bells: u64,
    /// Discarded ServerCutText messages (never delivered to the host).
    pub cut_text_discarded: u64,
    /// Ignored SetColourMapEntries messages.
    pub colour_map_ignored: u64,
}

/// Outcome of a FramebufferUpdate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSummary {
    /// Modified rectangles (after applying them to the local framebuffer).
    pub rects: Vec<Rect>,
    /// The framebuffer was resized in this update.
    pub resized: bool,
    /// Width after the update.
    pub width: u16,
    /// Height after the update.
    pub height: u16,
}

/// Read-only RFB connection.
pub struct Connection {
    io: BufReader<Box<dyn Stream>>,
    control: Option<SocketControl>,
    channel: ChannelKind,
    security: String,
    init: ServerInit,
    fb: Framebuffer,
    zrle: ZrleState,
    pending_full: bool,
    stats: ConnectionStats,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("channel", &self.channel)
            .field("security", &self.security)
            .field("init", &self.init)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

fn hs_write(s: &mut dyn Stream, b: &[u8]) -> Result<(), Error> {
    s.write_all(b)?;
    s.flush()?;
    Ok(())
}

impl Connection {
    /// Opens the transport and performs the full handshake.
    /// Subsequent reads are blocking; to interrupt them from another
    /// thread the socket is closed (see [`crate::Session::stop`]).
    pub fn connect(t: &Transport) -> Result<Self, Error> {
        let opened = transport::open(t)?;
        opened.control.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut c = Self::handshake(opened.stream, opened.kind, opened.tls.as_ref())?;
        opened.control.set_read_timeout(None)?;
        c.control = Some(opened.control);
        Ok(c)
    }

    /// Handshake on an already open stream. `channel` describes the real transport
    /// and is decided by whoever opens the socket, never by the server.
    pub(crate) fn handshake(
        mut s: Box<dyn Stream>,
        channel: ChannelKind,
        tls: Option<&TlsConfig>,
    ) -> Result<Self, Error> {
        let mut ver = [0u8; 12];
        s.read_exact(&mut ver)?;
        let (major, minor) = parse_version(&ver)?;
        if (major, minor) < (3, 8) {
            return Err(Error::Unsupported(format!(
                "RFB version {major}.{minor} (3.8 required)"
            )));
        }
        hs_write(&mut *s, b"RFB 003.008\n")?;

        let n = read_u8(&mut s)?;
        if n == 0 {
            let reason = read_string(&mut s, 4096)?;
            return Err(Error::Refused(reason));
        }
        let mut types = vec![0u8; usize::from(n)];
        s.read_exact(&mut types)?;

        let security = match channel {
            ChannelKind::PrivateUnixSocket => {
                if !types.contains(&SEC_NONE) {
                    return Err(Error::Unsupported(format!(
                        "security types {types:?}: only None is expected on a Unix socket"
                    )));
                }
                hs_write(&mut *s, &[SEC_NONE])?;
                check_security_result(&mut s)?;
                "None on private Unix socket".to_owned()
            }
            ChannelKind::PlainTcp => {
                if !types.contains(&SEC_VENCRYPT) {
                    return Err(Error::InsecureChannel(format!(
                        "the server offers {types:?} on TCP without VeNCrypt: cleartext None refused"
                    )));
                }
                let tls = tls.ok_or_else(|| {
                    Error::Config("VeNCrypt requires the TLS configuration".into())
                })?;
                hs_write(&mut *s, &[SEC_VENCRYPT])?;
                let vmaj = read_u8(&mut s)?;
                let vmin = read_u8(&mut s)?;
                if (vmaj, vmin) < (0, 2) {
                    return Err(Error::Unsupported(format!("VeNCrypt {vmaj}.{vmin}")));
                }
                hs_write(&mut *s, &[0, 2])?;
                if read_u8(&mut s)? != 0 {
                    return Err(Error::Refused("VeNCrypt version 0.2 refused".into()));
                }
                let ns = read_u8(&mut s)?;
                let mut subs = Vec::with_capacity(usize::from(ns));
                for _ in 0..ns {
                    subs.push(read_u32(&mut s)?);
                }
                if !subs.contains(&VENCRYPT_X509NONE) {
                    return Err(Error::Unsupported(format!(
                        "VeNCrypt subtypes {subs:?}: X509None (260) required"
                    )));
                }
                hs_write(&mut *s, &VENCRYPT_X509NONE.to_be_bytes())?;
                if read_u8(&mut s)? != 1 {
                    return Err(Error::Refused("X509None subtype refused".into()));
                }
                let (tls_stream, info) = transport::tls_upgrade(s, tls)?;
                s = tls_stream;
                check_security_result(&mut s)?;
                format!("VeNCrypt X509None, {info}")
            }
        };

        // ClientInit: shared = 1 (does not disconnect other clients).
        hs_write(&mut *s, &[1])?;
        let mut io = BufReader::with_capacity(256 * 1024, s);
        let init = read_server_init(&mut io)?;
        let mut c = Self {
            io,
            control: None,
            channel,
            security,
            fb: Framebuffer::new(init.width, init.height),
            init,
            zrle: ZrleState::new(),
            pending_full: true,
            stats: ConnectionStats::default(),
        };
        c.send(&ClientMessage::SetPixelFormat(PixelFormat::BGRX8888))?;
        c.send(&ClientMessage::SetEncodings(SUPPORTED_ENCODINGS.to_vec()))?;
        Ok(c)
    }

    /// The only write path after the handshake.
    fn send(&mut self, m: &ClientMessage) -> Result<(), Error> {
        let w = self.io.get_mut();
        w.write_all(&m.encode())?;
        w.flush()?;
        Ok(())
    }

    /// ServerInit data.
    pub fn server_init(&self) -> &ServerInit {
        &self.init
    }

    /// Channel on which the handshake took place.
    pub fn channel(&self) -> ChannelKind {
        self.channel
    }

    /// Description of the negotiated security.
    pub fn security(&self) -> &str {
        &self.security
    }

    /// Local framebuffer.
    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    /// Counters.
    pub fn stats(&self) -> &ConnectionStats {
        &self.stats
    }

    pub(crate) fn control(&self) -> Option<SocketControl> {
        self.control.clone()
    }

    /// Sends a FramebufferUpdateRequest: full after connection or
    /// resize, incremental otherwise.
    pub fn request_update(&mut self) -> Result<(), Error> {
        let incremental = !self.pending_full;
        self.pending_full = false;
        let rect = self.fb.full_rect();
        self.send(&ClientMessage::FramebufferUpdateRequest { incremental, rect })
    }

    /// Reads messages from the server up to a FramebufferUpdate and applies it.
    pub fn read_update(&mut self) -> Result<UpdateSummary, Error> {
        loop {
            let t = read_u8(&mut self.io)?;
            match t {
                0 => return self.framebuffer_update(),
                1 => {
                    skip(&mut self.io, 1)?;
                    let _first = read_u16(&mut self.io)?;
                    let n = read_u16(&mut self.io)?;
                    skip(&mut self.io, u64::from(n) * 6)?;
                    self.stats.colour_map_ignored += 1;
                }
                2 => self.stats.bells += 1,
                3 => {
                    skip(&mut self.io, 3)?;
                    let len = read_u32(&mut self.io)?;
                    if len > MAX_CUT_TEXT {
                        return Err(Error::Protocol(format!("ServerCutText of {len} bytes")));
                    }
                    // Discarded: no clipboard (ROADMAP 6.4, 7.2 item 3).
                    skip(&mut self.io, u64::from(len))?;
                    self.stats.cut_text_discarded += 1;
                }
                other => {
                    return Err(Error::Unsupported(format!(
                        "server message of type {other}"
                    )));
                }
            }
        }
    }

    fn framebuffer_update(&mut self) -> Result<UpdateSummary, Error> {
        skip(&mut self.io, 1)?;
        let n = read_u16(&mut self.io)?;
        let mut rects = Vec::with_capacity(usize::from(n));
        let mut resized = false;
        for _ in 0..n {
            let x = read_u16(&mut self.io)?;
            let y = read_u16(&mut self.io)?;
            let w = read_u16(&mut self.io)?;
            let h = read_u16(&mut self.io)?;
            let code = read_i32(&mut self.io)?;
            let rect = Rect::new(x, y, w, h);
            let enc = Encoding::from_code(code)
                .ok_or_else(|| Error::Unsupported(format!("encoding {code} not announced")))?;
            self.stats.rects += 1;
            let idx = match enc {
                Encoding::Raw => 0,
                Encoding::CopyRect => 1,
                Encoding::Hextile => 2,
                Encoding::Zrle => 3,
                Encoding::DesktopSize => 4,
            };
            self.stats.rects_by_encoding[idx] += 1;
            match enc {
                Encoding::Raw => decode::raw(&mut self.io, &mut self.fb, rect)?,
                Encoding::CopyRect => decode::copy_rect(&mut self.io, &mut self.fb, rect)?,
                Encoding::Hextile => decode::hextile(&mut self.io, &mut self.fb, rect)?,
                Encoding::Zrle => decode::zrle(&mut self.io, &mut self.fb, rect, &mut self.zrle)?,
                Encoding::DesktopSize => {
                    if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
                        return Err(Error::Protocol(format!("invalid DesktopSize {w}x{h}")));
                    }
                    self.fb.resize(w, h);
                    self.init.width = w;
                    self.init.height = h;
                    self.pending_full = true;
                    self.stats.resizes += 1;
                    resized = true;
                    rects.clear();
                    continue;
                }
            }
            self.stats.pixels += rect.area();
            if rect.area() > 0 {
                rects.push(rect);
            }
        }
        self.stats.updates += 1;
        if resized {
            rects = vec![self.fb.full_rect()];
        }
        Ok(UpdateSummary {
            rects,
            resized,
            width: self.fb.width(),
            height: self.fb.height(),
        })
    }
}

fn parse_version(v: &[u8; 12]) -> Result<(u32, u32), Error> {
    let bad = || Error::Protocol(format!("invalid version: {:?}", String::from_utf8_lossy(v)));
    if &v[0..4] != b"RFB " || v[7] != b'.' || v[11] != b'\n' {
        return Err(bad());
    }
    let num = |s: &[u8]| -> Result<u32, Error> {
        std::str::from_utf8(s)
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(bad)
    };
    Ok((num(&v[4..7])?, num(&v[8..11])?))
}

fn check_security_result(s: &mut Box<dyn Stream>) -> Result<(), Error> {
    let r = read_u32(s)?;
    if r != 0 {
        let reason = read_string(s, 4096).unwrap_or_else(|_| "no reason".into());
        return Err(Error::Refused(format!("SecurityResult {r}: {reason}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// Simulated stream: reads from a prepared buffer, records what the client writes.
    struct Mock {
        input: io::Cursor<Vec<u8>>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for Mock {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.input.read(b)
        }
    }

    impl Write for Mock {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.written.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn server_init(w: u16, h: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&PixelFormat::BGRX8888.to_bytes());
        v.extend_from_slice(&4u32.to_be_bytes());
        v.extend_from_slice(b"QEMU");
        v
    }

    fn mock(input: Vec<u8>) -> (Box<dyn Stream>, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(Mock {
                input: io::Cursor::new(input),
                written: written.clone(),
            }),
            written,
        )
    }

    /// Server that offers only None on cleartext TCP: the client refuses
    /// without choosing any security type.
    #[test]
    fn none_on_cleartext_tcp_refused() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, SEC_NONE]);
        let (s, written) = mock(input);
        let err = Connection::handshake(s, ChannelKind::PlainTcp, None).unwrap_err();
        assert!(matches!(err, Error::InsecureChannel(_)), "{err:?}");
        // The client wrote only its own version, no security choice.
        assert_eq!(written.lock().unwrap().as_slice(), b"RFB 003.008\n");
    }

    #[test]
    fn version_before_3_8_refused() {
        let (s, _) = mock(b"RFB 003.003\n".to_vec());
        let err = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn vnc_auth_on_unix_socket_unsupported() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, 2]);
        let (s, _) = mock(input);
        let err = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    /// Full simulated session: handshake, Raw update, DesktopSize,
    /// ServerCutText and Bell. All bytes written by the client after ClientInit
    /// are parsed: they must be only messages of type 0, 2 and 3.
    #[test]
    fn simulated_session_writes_only_types_0_2_3() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, SEC_NONE]);
        input.extend_from_slice(&0u32.to_be_bytes());
        input.extend_from_slice(&server_init(4, 2));
        // ServerCutText "ciao" (must be discarded) and Bell.
        input.extend_from_slice(&[3, 0, 0, 0]);
        input.extend_from_slice(&4u32.to_be_bytes());
        input.extend_from_slice(b"ciao");
        input.push(2);
        // FramebufferUpdate: 1 Raw rectangle 2x1 at (1,1).
        input.extend_from_slice(&[0, 0, 0, 1]);
        for n in [1u16, 1, 2, 1] {
            input.extend_from_slice(&n.to_be_bytes());
        }
        input.extend_from_slice(&0i32.to_be_bytes());
        input.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
        // FramebufferUpdate: DesktopSize 8x3.
        input.extend_from_slice(&[0, 0, 0, 1]);
        for n in [0u16, 0, 8, 3] {
            input.extend_from_slice(&n.to_be_bytes());
        }
        input.extend_from_slice(&(-223i32).to_be_bytes());

        let (s, written) = mock(input);
        let mut c = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap();
        assert_eq!(c.server_init().name, "QEMU");
        c.request_update().unwrap();
        let u = c.read_update().unwrap();
        assert_eq!(u.rects, vec![Rect::new(1, 1, 2, 1)]);
        assert_eq!(c.stats().cut_text_discarded, 1);
        assert_eq!(c.stats().bells, 1);
        let fb = c.framebuffer();
        assert_eq!(&fb.data()[(4 + 1) * 4..(4 + 1) * 4 + 3], &[10, 20, 30]);
        c.request_update().unwrap();
        let u = c.read_update().unwrap();
        assert!(u.resized);
        assert_eq!((u.width, u.height), (8, 3));
        c.request_update().unwrap();
        assert!(matches!(c.read_update(), Err(Error::Closed)));

        let w = written.lock().unwrap().clone();
        // Version, security type None, shared ClientInit.
        assert_eq!(&w[..14], b"RFB 003.008\n\x01\x01");
        let mut rest = &w[14..];
        let mut types = Vec::new();
        let mut requests = Vec::new();
        while !rest.is_empty() {
            let t = rest[0];
            types.push(t);
            let len = match t {
                0 => 20,
                2 => 4 + 4 * usize::from(u16::from_be_bytes([rest[2], rest[3]])),
                3 => {
                    requests.push((rest[1], u16::from_be_bytes([rest[6], rest[7]])));
                    10
                }
                other => panic!("the client wrote a message of type {other}"),
            };
            rest = &rest[len..];
        }
        assert!(types.iter().all(|t| CLIENT_TYPES.contains(t)));
        assert_eq!(types, vec![0, 2, 3, 3, 3]);
        // First request full, then incremental, then full after the resize (width 8).
        assert_eq!(requests, vec![(0, 4), (1, 4), (0, 8)]);
    }

    const CLIENT_TYPES: [u8; 3] = crate::CLIENT_MESSAGE_TYPES_SENT;

    #[test]
    fn unannounced_encoding_refused() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, SEC_NONE]);
        input.extend_from_slice(&0u32.to_be_bytes());
        input.extend_from_slice(&server_init(4, 2));
        input.extend_from_slice(&[0, 0, 0, 1]);
        for n in [0u16, 0, 1, 1] {
            input.extend_from_slice(&n.to_be_bytes());
        }
        input.extend_from_slice(&(-239i32).to_be_bytes()); // Cursor: not announced
        let (s, _) = mock(input);
        let mut c = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap();
        assert!(matches!(c.read_update(), Err(Error::Unsupported(_))));
    }
}
