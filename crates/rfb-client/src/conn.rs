//! Connessione RFB sincrona: handshake e lettura degli aggiornamenti.

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
/// Limite ai testi ServerCutText scartati (oltre: errore di protocollo).
const MAX_CUT_TEXT: u32 = 1024 * 1024;
/// Timeout di lettura durante l'handshake: evita attese indefinite (per
/// esempio un server VeNCrypt che non chiude dopo un certificato rifiutato).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Contatori della connessione.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionStats {
    /// Messaggi FramebufferUpdate elaborati.
    pub updates: u64,
    /// Rettangoli ricevuti (inclusi pseudo-encoding).
    pub rects: u64,
    /// Rettangoli per encoding: Raw, CopyRect, Hextile, ZRLE, DesktopSize.
    pub rects_by_encoding: [u64; 5],
    /// Pixel aggiornati (somma delle aree, escluso DesktopSize).
    pub pixels: u64,
    /// Ridimensionamenti ricevuti.
    pub resizes: u64,
    /// Messaggi Bell ignorati.
    pub bells: u64,
    /// Messaggi ServerCutText scartati (mai consegnati all'host).
    pub cut_text_discarded: u64,
    /// Messaggi SetColourMapEntries ignorati.
    pub colour_map_ignored: u64,
}

/// Esito di un FramebufferUpdate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSummary {
    /// Rettangoli modificati (dopo l'applicazione al framebuffer locale).
    pub rects: Vec<Rect>,
    /// Il framebuffer è stato ridimensionato in questo aggiornamento.
    pub resized: bool,
    /// Larghezza dopo l'aggiornamento.
    pub width: u16,
    /// Altezza dopo l'aggiornamento.
    pub height: u16,
}

/// Connessione RFB in sola lettura.
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
    /// Apre il trasporto ed esegue l'handshake completo.
    /// Le letture successive sono bloccanti; per interromperle da un altro
    /// thread si chiude il socket (vedi [`crate::Session::stop`]).
    pub fn connect(t: &Transport) -> Result<Self, Error> {
        let opened = transport::open(t)?;
        opened.control.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut c = Self::handshake(opened.stream, opened.kind, opened.tls.as_ref())?;
        opened.control.set_read_timeout(None)?;
        c.control = Some(opened.control);
        Ok(c)
    }

    /// Handshake su un flusso già aperto. `channel` descrive il trasporto reale
    /// ed è deciso da chi apre il socket, mai dal server.
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
                "versione RFB {major}.{minor} (richiesta 3.8)"
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
                        "tipi di sicurezza {types:?}: su socket Unix è previsto solo None"
                    )));
                }
                hs_write(&mut *s, &[SEC_NONE])?;
                check_security_result(&mut s)?;
                "None su socket Unix privato".to_owned()
            }
            ChannelKind::PlainTcp => {
                if !types.contains(&SEC_VENCRYPT) {
                    return Err(Error::InsecureChannel(format!(
                        "il server offre {types:?} su TCP senza VeNCrypt: None in chiaro rifiutato"
                    )));
                }
                let tls = tls.ok_or_else(|| {
                    Error::Config("VeNCrypt richiede la configurazione TLS".into())
                })?;
                hs_write(&mut *s, &[SEC_VENCRYPT])?;
                let vmaj = read_u8(&mut s)?;
                let vmin = read_u8(&mut s)?;
                if (vmaj, vmin) < (0, 2) {
                    return Err(Error::Unsupported(format!("VeNCrypt {vmaj}.{vmin}")));
                }
                hs_write(&mut *s, &[0, 2])?;
                if read_u8(&mut s)? != 0 {
                    return Err(Error::Refused("versione VeNCrypt 0.2 rifiutata".into()));
                }
                let ns = read_u8(&mut s)?;
                let mut subs = Vec::with_capacity(usize::from(ns));
                for _ in 0..ns {
                    subs.push(read_u32(&mut s)?);
                }
                if !subs.contains(&VENCRYPT_X509NONE) {
                    return Err(Error::Unsupported(format!(
                        "sottotipi VeNCrypt {subs:?}: richiesto X509None (260)"
                    )));
                }
                hs_write(&mut *s, &VENCRYPT_X509NONE.to_be_bytes())?;
                if read_u8(&mut s)? != 1 {
                    return Err(Error::Refused("sottotipo X509None rifiutato".into()));
                }
                let (tls_stream, info) = transport::tls_upgrade(s, tls)?;
                s = tls_stream;
                check_security_result(&mut s)?;
                format!("VeNCrypt X509None, {info}")
            }
        };

        // ClientInit: shared = 1 (non disconnette altri client).
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

    /// Unico percorso di scrittura dopo l'handshake.
    fn send(&mut self, m: &ClientMessage) -> Result<(), Error> {
        let w = self.io.get_mut();
        w.write_all(&m.encode())?;
        w.flush()?;
        Ok(())
    }

    /// Dati del ServerInit.
    pub fn server_init(&self) -> &ServerInit {
        &self.init
    }

    /// Canale su cui è avvenuto l'handshake.
    pub fn channel(&self) -> ChannelKind {
        self.channel
    }

    /// Descrizione della sicurezza negoziata.
    pub fn security(&self) -> &str {
        &self.security
    }

    /// Framebuffer locale.
    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    /// Contatori.
    pub fn stats(&self) -> &ConnectionStats {
        &self.stats
    }

    pub(crate) fn control(&self) -> Option<SocketControl> {
        self.control.clone()
    }

    /// Invia un FramebufferUpdateRequest: completo dopo connessione o
    /// ridimensionamento, altrimenti incrementale.
    pub fn request_update(&mut self) -> Result<(), Error> {
        let incremental = !self.pending_full;
        self.pending_full = false;
        let rect = self.fb.full_rect();
        self.send(&ClientMessage::FramebufferUpdateRequest { incremental, rect })
    }

    /// Legge messaggi dal server fino a un FramebufferUpdate e lo applica.
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
                        return Err(Error::Protocol(format!("ServerCutText di {len} byte")));
                    }
                    // Scartato: nessuna clipboard (ROADMAP 6.4, 7.2 punto 3).
                    skip(&mut self.io, u64::from(len))?;
                    self.stats.cut_text_discarded += 1;
                }
                other => {
                    return Err(Error::Unsupported(format!(
                        "messaggio server di tipo {other}"
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
                .ok_or_else(|| Error::Unsupported(format!("encoding {code} non annunciato")))?;
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
                        return Err(Error::Protocol(format!("DesktopSize {w}x{h} non valido")));
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
    let bad = || {
        Error::Protocol(format!(
            "versione non valida: {:?}",
            String::from_utf8_lossy(v)
        ))
    };
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
        let reason = read_string(s, 4096).unwrap_or_else(|_| "nessun motivo".into());
        return Err(Error::Refused(format!("SecurityResult {r}: {reason}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// Flusso simulato: legge da un buffer preparato, registra ciò che il client scrive.
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

    /// Server che offre soltanto None su TCP in chiaro: il client rifiuta
    /// senza scegliere alcun tipo di sicurezza.
    #[test]
    fn none_su_tcp_in_chiaro_rifiutato() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, SEC_NONE]);
        let (s, written) = mock(input);
        let err = Connection::handshake(s, ChannelKind::PlainTcp, None).unwrap_err();
        assert!(matches!(err, Error::InsecureChannel(_)), "{err:?}");
        // Il client ha scritto solo la propria versione, nessuna scelta di sicurezza.
        assert_eq!(written.lock().unwrap().as_slice(), b"RFB 003.008\n");
    }

    #[test]
    fn versione_precedente_alla_3_8_rifiutata() {
        let (s, _) = mock(b"RFB 003.003\n".to_vec());
        let err = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn vnc_auth_su_socket_unix_non_supportata() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, 2]);
        let (s, _) = mock(input);
        let err = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    /// Sessione completa simulata: handshake, aggiornamento Raw, DesktopSize,
    /// ServerCutText e Bell. Tutti i byte scritti dal client dopo ClientInit
    /// sono analizzati: devono essere solo messaggi di tipo 0, 2 e 3.
    #[test]
    fn sessione_simulata_scrive_solo_tipi_0_2_3() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, SEC_NONE]);
        input.extend_from_slice(&0u32.to_be_bytes());
        input.extend_from_slice(&server_init(4, 2));
        // ServerCutText "ciao" (deve essere scartato) e Bell.
        input.extend_from_slice(&[3, 0, 0, 0]);
        input.extend_from_slice(&4u32.to_be_bytes());
        input.extend_from_slice(b"ciao");
        input.push(2);
        // FramebufferUpdate: 1 rettangolo Raw 2x1 in (1,1).
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
        // Versione, tipo di sicurezza None, ClientInit shared.
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
                other => panic!("il client ha scritto un messaggio di tipo {other}"),
            };
            rest = &rest[len..];
        }
        assert!(types.iter().all(|t| CLIENT_TYPES.contains(t)));
        assert_eq!(types, vec![0, 2, 3, 3, 3]);
        // Prima richiesta completa, poi incrementale, poi completa dopo il resize (larghezza 8).
        assert_eq!(requests, vec![(0, 4), (1, 4), (0, 8)]);
    }

    const CLIENT_TYPES: [u8; 3] = crate::CLIENT_MESSAGE_TYPES_SENT;

    #[test]
    fn encoding_non_annunciato_rifiutato() {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, SEC_NONE]);
        input.extend_from_slice(&0u32.to_be_bytes());
        input.extend_from_slice(&server_init(4, 2));
        input.extend_from_slice(&[0, 0, 0, 1]);
        for n in [0u16, 0, 1, 1] {
            input.extend_from_slice(&n.to_be_bytes());
        }
        input.extend_from_slice(&(-239i32).to_be_bytes()); // Cursor: non annunciato
        let (s, _) = mock(input);
        let mut c = Connection::handshake(s, ChannelKind::PrivateUnixSocket, None).unwrap();
        assert!(matches!(c.read_update(), Err(Error::Unsupported(_))));
    }
}
