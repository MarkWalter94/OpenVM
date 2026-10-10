use crate::Error;

/// Rectangle in framebuffer coordinates (pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// X coordinate of the top-left corner.
    pub x: u16,
    /// Y coordinate of the top-left corner.
    pub y: u16,
    /// Width.
    pub w: u16,
    /// Height.
    pub h: u16,
}

impl Rect {
    /// Builds a rectangle.
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// Area in pixels.
    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    /// Smallest rectangle containing `self` and `other`.
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

/// Local framebuffer in 32 bpp BGRX format (bytes in memory: B, G, R, X),
/// contiguous rows without padding (`stride = width * 4`).
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
    /// Black framebuffer of the given size.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            data: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// BGRX bytes of the whole framebuffer.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Rectangle covering the whole framebuffer.
    pub fn full_rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Resizes and clears (black), as after a DesktopSize.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.data.clear();
        self.data
            .resize(usize::from(width) * usize::from(height) * 4, 0);
    }

    /// Checks that the rectangle is entirely contained in the framebuffer.
    pub(crate) fn check(&self, r: Rect) -> Result<(), Error> {
        if u32::from(r.x) + u32::from(r.w) > u32::from(self.width)
            || u32::from(r.y) + u32::from(r.h) > u32::from(self.height)
        {
            return Err(Error::Protocol(format!(
                "rectangle {r:?} outside the {}x{} framebuffer",
                self.width, self.height
            )));
        }
        Ok(())
    }

    fn offset(&self, x: u16, y: u16) -> usize {
        (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4
    }

    /// Copies contiguous BGRX pixels (`r.w * r.h * 4` bytes) into the rectangle.
    pub(crate) fn put(&mut self, r: Rect, src: &[u8]) -> Result<(), Error> {
        self.check(r)?;
        let row = usize::from(r.w) * 4;
        if src.len() != row * usize::from(r.h) {
            return Err(Error::Protocol("inconsistent pixel data size".into()));
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

    /// Fills the rectangle with a BGRX pixel.
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

    /// CopyRect: copies the source area (sx, sy) into the destination rectangle.
    /// Handles overlap by going through a temporary buffer.
    pub(crate) fn copy_within(&mut self, dst: Rect, sx: u16, sy: u16) -> Result<(), Error> {
        self.check(dst)?;
        self.check(Rect::new(sx, sy, dst.w, dst.h))?;
        let tmp = self.region(Rect::new(sx, sy, dst.w, dst.h));
        self.put(dst, &tmp)
    }

    /// Copies the BGRX bytes of a rectangle (contiguous, `r.w * 4` per row).
    /// The rectangle is clipped to the framebuffer edges.
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

    /// Copies a rectangle from another framebuffer of the same size.
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

    /// Clips a rectangle to the framebuffer edges.
    pub fn clip(&self, r: Rect) -> Rect {
        let x = r.x.min(self.width);
        let y = r.y.min(self.height);
        let w = r.w.min(self.width - x);
        let h = r.h.min(self.height - y);
        Rect::new(x, y, w, h)
    }

    /// Converts a rectangle to RGBA (alpha 255), useful for a texture.
    pub fn region_rgba(&self, r: Rect) -> Vec<u8> {
        let mut v = self.region(r);
        for p in v.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            p[3] = 255;
        }
        v
    }

    /// Converts the whole framebuffer to RGB (3 bytes per pixel), for PNG/PPM.
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() / 4 * 3);
        for p in self.data.as_chunks::<4>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        out
    }
}
