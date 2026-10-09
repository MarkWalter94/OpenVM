//! Decodifica dei rettangoli: Raw, CopyRect, Hextile, ZRLE.
//! Tutti i decoder assumono il formato pixel BGRX8888 imposto dal client.

use std::io::Read;

use flate2::{Decompress, FlushDecompress, Status};

use crate::proto::{read_u8, read_u16, read_u32, read_vec};
use crate::{Error, Framebuffer, Rect};

/// Limite ai dati compressi di un singolo rettangolo ZRLE.
const MAX_ZRLE_COMPRESSED: u32 = 64 * 1024 * 1024;

pub(crate) fn raw(r: &mut impl Read, fb: &mut Framebuffer, rect: Rect) -> Result<(), Error> {
    fb.check(rect)?;
    let data = read_vec(r, rect.area() as usize * 4)?;
    fb.put(rect, &data)
}

pub(crate) fn copy_rect(r: &mut impl Read, fb: &mut Framebuffer, rect: Rect) -> Result<(), Error> {
    let sx = read_u16(r)?;
    let sy = read_u16(r)?;
    fb.copy_within(rect, sx, sy)
}

fn pixel(r: &mut impl Read) -> Result<[u8; 4], Error> {
    let mut p = [0u8; 4];
    r.read_exact(&mut p)?;
    Ok(p)
}

pub(crate) fn hextile(r: &mut impl Read, fb: &mut Framebuffer, rect: Rect) -> Result<(), Error> {
    fb.check(rect)?;
    let mut bg = [0u8; 4];
    let mut fg = [0u8; 4];
    let mut ty = 0u16;
    while ty < rect.h {
        let th = 16.min(rect.h - ty);
        let mut tx = 0u16;
        while tx < rect.w {
            let tw = 16.min(rect.w - tx);
            let tile = Rect::new(rect.x + tx, rect.y + ty, tw, th);
            let sub = read_u8(r)?;
            if sub & 1 != 0 {
                let data = read_vec(r, tile.area() as usize * 4)?;
                fb.put(tile, &data)?;
            } else {
                if sub & 2 != 0 {
                    bg = pixel(r)?;
                }
                fb.fill(tile, bg)?;
                if sub & 4 != 0 {
                    fg = pixel(r)?;
                }
                if sub & 8 != 0 {
                    let n = read_u8(r)?;
                    for _ in 0..n {
                        let color = if sub & 16 != 0 { pixel(r)? } else { fg };
                        let xy = read_u8(r)?;
                        let wh = read_u8(r)?;
                        let sx = u16::from(xy >> 4);
                        let sy = u16::from(xy & 15);
                        let sw = u16::from(wh >> 4) + 1;
                        let sh = u16::from(wh & 15) + 1;
                        if sx + sw > tw || sy + sh > th {
                            return Err(Error::Protocol("subrect Hextile fuori dal tile".into()));
                        }
                        fb.fill(Rect::new(tile.x + sx, tile.y + sy, sw, sh), color)?;
                    }
                }
            }
            tx += tw;
        }
        ty += th;
    }
    Ok(())
}

/// Stato ZRLE: un solo flusso zlib per tutta la connessione (RFC 6143, 7.7.6).
pub(crate) struct ZrleState {
    inflater: Decompress,
    out: Vec<u8>,
}

impl ZrleState {
    pub(crate) fn new() -> Self {
        Self {
            inflater: Decompress::new(true),
            out: Vec::new(),
        }
    }

    fn inflate(&mut self, input: &[u8], limit: usize) -> Result<(), Error> {
        self.out.clear();
        let mut pos = 0usize;
        loop {
            if self.out.len() == self.out.capacity() {
                if self.out.len() >= limit {
                    return Err(Error::Protocol(
                        "dati ZRLE decompressi oltre il limite".into(),
                    ));
                }
                self.out.reserve(self.out.len().max(64 * 1024));
            }
            let before_in = self.inflater.total_in();
            let before_out = self.inflater.total_out();
            let status = self
                .inflater
                .decompress_vec(&input[pos..], &mut self.out, FlushDecompress::Sync)
                .map_err(|e| Error::Protocol(format!("zlib: {e}")))?;
            let consumed = (self.inflater.total_in() - before_in) as usize;
            let produced = self.inflater.total_out() - before_out;
            pos += consumed;
            if status == Status::StreamEnd {
                return Err(Error::Protocol("fine inattesa del flusso zlib ZRLE".into()));
            }
            let spare = self.out.len() < self.out.capacity();
            if pos >= input.len() && spare {
                return Ok(());
            }
            if consumed == 0 && produced == 0 && spare {
                return Err(Error::Protocol("flusso zlib ZRLE bloccato".into()));
            }
        }
    }
}

/// Lettore su slice con errore di protocollo a fine dati.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        if self.pos + n > self.buf.len() {
            return Err(Error::Protocol("dati ZRLE troncati".into()));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    /// CPIXEL a 3 byte (B, G, R) per BGRX8888 con profondità 24.
    fn cpixel(&mut self) -> Result<[u8; 4], Error> {
        let s = self.take(3)?;
        Ok([s[0], s[1], s[2], 0])
    }

    fn run_length(&mut self) -> Result<usize, Error> {
        let mut len = 1usize;
        loop {
            let b = self.u8()?;
            len += usize::from(b);
            if b != 255 {
                return Ok(len);
            }
        }
    }
}

pub(crate) fn zrle(
    r: &mut impl Read,
    fb: &mut Framebuffer,
    rect: Rect,
    st: &mut ZrleState,
) -> Result<(), Error> {
    fb.check(rect)?;
    let len = read_u32(r)?;
    if len > MAX_ZRLE_COMPRESSED {
        return Err(Error::Protocol(format!(
            "rettangolo ZRLE di {len} byte oltre il limite"
        )));
    }
    let compressed = read_vec(r, len as usize)?;
    let tiles = rect.w.div_ceil(64) as usize * rect.h.div_ceil(64) as usize;
    // Caso peggiore ragionevole: ogni tile raw (1 + 64*64*3) più margine per RLE.
    let limit = tiles * (1 + 64 * 64 * 4) + 1024 * 1024;
    st.inflate(&compressed, limit)?;
    let mut c = Cursor {
        buf: &st.out,
        pos: 0,
    };
    let mut tile_px: Vec<[u8; 4]> = Vec::with_capacity(64 * 64);
    let mut ty = 0u16;
    while ty < rect.h {
        let th = 64.min(rect.h - ty);
        let mut tx = 0u16;
        while tx < rect.w {
            let tw = 64.min(rect.w - tx);
            let tile = Rect::new(rect.x + tx, rect.y + ty, tw, th);
            let n = usize::from(tw) * usize::from(th);
            let sub = c.u8()?;
            match sub {
                0 => {
                    tile_px.clear();
                    for _ in 0..n {
                        tile_px.push(c.cpixel()?);
                    }
                    put_tile(fb, tile, &tile_px)?;
                }
                1 => {
                    let p = c.cpixel()?;
                    fb.fill(tile, p)?;
                }
                2..=16 => {
                    let mut pal = [[0u8; 4]; 16];
                    for p in pal.iter_mut().take(usize::from(sub)) {
                        *p = c.cpixel()?;
                    }
                    let bits = match sub {
                        2 => 1,
                        3 | 4 => 2,
                        _ => 4,
                    };
                    let row_bytes = (usize::from(tw) * bits).div_ceil(8);
                    tile_px.clear();
                    for _ in 0..th {
                        let row = c.take(row_bytes)?;
                        for x in 0..usize::from(tw) {
                            let bit = x * bits;
                            let byte = row[bit / 8];
                            let shift = 8 - bits - (bit % 8);
                            let idx = usize::from((byte >> shift) & ((1u8 << bits) - 1));
                            if idx >= usize::from(sub) {
                                return Err(Error::Protocol(
                                    "indice palette ZRLE non valido".into(),
                                ));
                            }
                            tile_px.push(pal[idx]);
                        }
                    }
                    put_tile(fb, tile, &tile_px)?;
                }
                128 => {
                    tile_px.clear();
                    while tile_px.len() < n {
                        let p = c.cpixel()?;
                        let run = c.run_length()?;
                        if tile_px.len() + run > n {
                            return Err(Error::Protocol("run ZRLE oltre il tile".into()));
                        }
                        tile_px.extend(std::iter::repeat_n(p, run));
                    }
                    put_tile(fb, tile, &tile_px)?;
                }
                130..=255 => {
                    let size = usize::from(sub - 128);
                    let mut pal = Vec::with_capacity(size);
                    for _ in 0..size {
                        pal.push(c.cpixel()?);
                    }
                    tile_px.clear();
                    while tile_px.len() < n {
                        let b = c.u8()?;
                        let idx = usize::from(b & 127);
                        let p = *pal.get(idx).ok_or_else(|| {
                            Error::Protocol("indice palette RLE non valido".into())
                        })?;
                        let run = if b & 128 != 0 { c.run_length()? } else { 1 };
                        if tile_px.len() + run > n {
                            return Err(Error::Protocol("run ZRLE oltre il tile".into()));
                        }
                        tile_px.extend(std::iter::repeat_n(p, run));
                    }
                    put_tile(fb, tile, &tile_px)?;
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "subencoding ZRLE {other} non valido"
                    )));
                }
            }
            tx += tw;
        }
        ty += th;
    }
    if c.pos != c.buf.len() {
        return Err(Error::Protocol(format!(
            "{} byte ZRLE in eccesso dopo i tile",
            c.buf.len() - c.pos
        )));
    }
    Ok(())
}

fn put_tile(fb: &mut Framebuffer, tile: Rect, px: &[[u8; 4]]) -> Result<(), Error> {
    let flat: Vec<u8> = px.iter().flatten().copied().collect();
    fb.put(tile, &flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    fn compress_sync(c: &mut Compress, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 1024);
        c.compress_vec(data, &mut out, FlushCompress::Sync).unwrap();
        out
    }

    #[test]
    fn hextile_sfondo_e_subrect() {
        let mut fb = Framebuffer::new(20, 16);
        // Tile 1 (16x16): sfondo rosso + 1 subrect blu 2x3 in (1,2). Tile 2 (4x16): sfondo ereditato.
        let mut s = vec![2 | 8 | 16];
        s.extend_from_slice(&[0, 0, 255, 0]); // bg BGRX rosso
        s.push(1);
        s.extend_from_slice(&[255, 0, 0, 0]); // blu
        s.push((1 << 4) | 2);
        s.push((1 << 4) | 2);
        s.push(0); // tile 2: nessun bit, sfondo precedente
        hextile(&mut s.as_slice(), &mut fb, Rect::new(0, 0, 20, 16)).unwrap();
        let px = |x: usize, y: usize| &fb.data()[(y * 20 + x) * 4..(y * 20 + x) * 4 + 3];
        assert_eq!(px(0, 0), &[0, 0, 255]);
        assert_eq!(px(1, 2), &[255, 0, 0]);
        assert_eq!(px(2, 4), &[255, 0, 0]);
        assert_eq!(px(3, 2), &[0, 0, 255]);
        assert_eq!(px(19, 15), &[0, 0, 255]);
    }

    #[test]
    fn zrle_solido_palette_e_rle_su_due_rettangoli() {
        let mut c = Compress::new(Compression::default(), true);
        let mut st = ZrleState::new();
        let mut fb = Framebuffer::new(70, 2);
        // Rettangolo 1: 70x2 -> due tile (64x2 solido verde, 6x2 palette 2 colori).
        let mut d = vec![1, 0, 255, 0];
        d.extend_from_slice(&[2, 1, 1, 1, 9, 9, 9]);
        d.extend_from_slice(&[0b1010_1000, 0b0101_0100]);
        let z = compress_sync(&mut c, &d);
        let mut msg = (z.len() as u32).to_be_bytes().to_vec();
        msg.extend_from_slice(&z);
        zrle(
            &mut msg.as_slice(),
            &mut fb,
            Rect::new(0, 0, 70, 2),
            &mut st,
        )
        .unwrap();
        assert_eq!(&fb.data()[0..3], &[0, 255, 0]);
        fn at(fb: &Framebuffer, x: usize, y: usize) -> u8 {
            fb.data()[(y * 70 + x) * 4]
        }
        assert_eq!(at(&fb, 64, 0), 9);
        assert_eq!(at(&fb, 65, 0), 1);
        assert_eq!(at(&fb, 64, 1), 1);
        assert_eq!(at(&fb, 65, 1), 9);
        // Rettangolo 2 sullo stesso flusso zlib: RLE semplice 4x1 (3 + 1 pixel).
        let d2 = vec![128, 7, 7, 7, 2, 8, 8, 8, 0];
        let z2 = compress_sync(&mut c, &d2);
        let mut msg2 = (z2.len() as u32).to_be_bytes().to_vec();
        msg2.extend_from_slice(&z2);
        zrle(
            &mut msg2.as_slice(),
            &mut fb,
            Rect::new(0, 0, 4, 1),
            &mut st,
        )
        .unwrap();
        assert_eq!(at(&fb, 2, 0), 7);
        assert_eq!(at(&fb, 3, 0), 8);
    }

    #[test]
    fn zrle_rifiuta_run_oltre_il_tile() {
        let mut c = Compress::new(Compression::default(), true);
        let mut st = ZrleState::new();
        let mut fb = Framebuffer::new(2, 1);
        let z = compress_sync(&mut c, &[128, 1, 2, 3, 5]);
        let mut msg = (z.len() as u32).to_be_bytes().to_vec();
        msg.extend_from_slice(&z);
        assert!(zrle(&mut msg.as_slice(), &mut fb, Rect::new(0, 0, 2, 1), &mut st).is_err());
    }

    #[test]
    fn rettangolo_fuori_dal_framebuffer_rifiutato() {
        let mut fb = Framebuffer::new(4, 4);
        let data = vec![0u8; 4 * 4 * 4];
        assert!(raw(&mut data.as_slice(), &mut fb, Rect::new(2, 2, 4, 4)).is_err());
        let src = [0u8, 3, 0, 3];
        assert!(copy_rect(&mut src.as_slice(), &mut fb, Rect::new(0, 0, 2, 2)).is_err());
    }

    #[test]
    fn copy_rect_con_sovrapposizione() {
        let mut fb = Framebuffer::new(4, 1);
        let data: Vec<u8> = (0..16).collect();
        fb.put(Rect::new(0, 0, 4, 1), &data).unwrap();
        let src = [0u8, 0, 0, 0];
        copy_rect(&mut src.as_slice(), &mut fb, Rect::new(1, 0, 3, 1)).unwrap();
        assert_eq!(&fb.data()[4..16], &data[0..12]);
    }
}
