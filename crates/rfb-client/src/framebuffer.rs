use crate::Error;

/// Rettangolo in coordinate del framebuffer (pixel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Ascissa del vertice in alto a sinistra.
    pub x: u16,
    /// Ordinata del vertice in alto a sinistra.
    pub y: u16,
    /// Larghezza.
    pub w: u16,
    /// Altezza.
    pub h: u16,
}

impl Rect {
    /// Costruisce un rettangolo.
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// Area in pixel.
    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    /// Rettangolo minimo che contiene `self` e `other`.
    pub fn union(&self, other: &Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 =
            (u32::from(self.x) + u32::from(self.w)).max(u32::from(other.x) + u32::from(other.w));
        let y1 =
            (u32::from(self.y) + u32::from(self.h)).max(u32::from(other.y) + u32::from(other.h));
        Self::new(
            x0,
            y0,
            (x1 - u32::from(x0)) as u16,
            (y1 - u32::from(y0)) as u16,
        )
    }
}

/// Framebuffer locale in formato BGRX 32 bpp (byte in memoria: B, G, R, X),
/// righe contigue senza padding (`stride = width * 4`).
#[derive(Clone, PartialEq, Eq)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    data: Vec<u8>,
}

impl std::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framebuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Framebuffer {
    /// Framebuffer nero delle dimensioni indicate.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            data: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    /// Larghezza in pixel.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Altezza in pixel.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Byte BGRX dell'intero framebuffer.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Rettangolo che copre l'intero framebuffer.
    pub fn full_rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Ridimensiona e azzera (nero), come dopo un DesktopSize.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.data.clear();
        self.data
            .resize(usize::from(width) * usize::from(height) * 4, 0);
    }

    /// Verifica che il rettangolo sia interamente contenuto nel framebuffer.
    pub(crate) fn check(&self, r: Rect) -> Result<(), Error> {
        if u32::from(r.x) + u32::from(r.w) > u32::from(self.width)
            || u32::from(r.y) + u32::from(r.h) > u32::from(self.height)
        {
            return Err(Error::Protocol(format!(
                "rettangolo {r:?} fuori dal framebuffer {}x{}",
                self.width, self.height
            )));
        }
        Ok(())
    }

    fn offset(&self, x: u16, y: u16) -> usize {
        (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4
    }

    /// Copia pixel BGRX contigui (`r.w * r.h * 4` byte) nel rettangolo.
    pub(crate) fn put(&mut self, r: Rect, src: &[u8]) -> Result<(), Error> {
        self.check(r)?;
        let row = usize::from(r.w) * 4;
        if src.len() != row * usize::from(r.h) {
            return Err(Error::Protocol("dimensione dei pixel non coerente".into()));
        }
        for (i, chunk) in src
            .chunks_exact(row.max(1))
            .enumerate()
            .take(usize::from(r.h))
        {
            let off = self.offset(r.x, r.y + i as u16);
            self.data[off..off + row].copy_from_slice(chunk);
        }
        Ok(())
    }

    /// Riempie il rettangolo con un pixel BGRX.
    pub(crate) fn fill(&mut self, r: Rect, px: [u8; 4]) -> Result<(), Error> {
        self.check(r)?;
        for y in r.y..r.y + r.h {
            let off = self.offset(r.x, y);
            for p in self.data[off..off + usize::from(r.w) * 4]
                .as_chunks_mut::<4>()
                .0
            {
                p.copy_from_slice(&px);
            }
        }
        Ok(())
    }

    /// CopyRect: copia l'area di origine (sx, sy) nel rettangolo di destinazione.
    /// Gestisce la sovrapposizione passando per un buffer temporaneo.
    pub(crate) fn copy_within(&mut self, dst: Rect, sx: u16, sy: u16) -> Result<(), Error> {
        self.check(dst)?;
        self.check(Rect::new(sx, sy, dst.w, dst.h))?;
        let tmp = self.region(Rect::new(sx, sy, dst.w, dst.h));
        self.put(dst, &tmp)
    }

    /// Copia i byte BGRX di un rettangolo (contigui, `r.w * 4` per riga).
    /// Il rettangolo viene ritagliato ai bordi del framebuffer.
    pub fn region(&self, r: Rect) -> Vec<u8> {
        let r = self.clip(r);
        let row = usize::from(r.w) * 4;
        let mut out = Vec::with_capacity(row * usize::from(r.h));
        for y in r.y..r.y + r.h {
            let off = self.offset(r.x, y);
            out.extend_from_slice(&self.data[off..off + row]);
        }
        out
    }

    /// Copia un rettangolo da un altro framebuffer delle stesse dimensioni.
    pub fn copy_from(&mut self, other: &Self, r: Rect) {
        if self.width != other.width || self.height != other.height {
            *self = other.clone();
            return;
        }
        let r = self.clip(r);
        let row = usize::from(r.w) * 4;
        for y in r.y..r.y + r.h {
            let off = self.offset(r.x, y);
            self.data[off..off + row].copy_from_slice(&other.data[off..off + row]);
        }
    }

    /// Ritaglia un rettangolo ai bordi del framebuffer.
    pub fn clip(&self, r: Rect) -> Rect {
        let x = r.x.min(self.width);
        let y = r.y.min(self.height);
        let w = r.w.min(self.width - x);
        let h = r.h.min(self.height - y);
        Rect::new(x, y, w, h)
    }

    /// Converte un rettangolo in RGBA (alpha 255), utile per una texture.
    pub fn region_rgba(&self, r: Rect) -> Vec<u8> {
        let mut v = self.region(r);
        for p in v.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            p[3] = 255;
        }
        v
    }

    /// Converte l'intero framebuffer in RGB (3 byte per pixel), per PNG/PPM.
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() / 4 * 3);
        for p in self.data.as_chunks::<4>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        out
    }
}
