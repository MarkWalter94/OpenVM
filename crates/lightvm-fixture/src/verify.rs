//! Deterministic verifier of ADR-0019 on a 24-bit RGB framebuffer (row-major,
//! 3 bytes per pixel): exact colours, connected components, full bounding
//! boxes. No OCR, no approximate comparison, no model judgement.
//!
//! Checks implemented (ADR-0019, "Expected result and verification"):
//! - V3: for each of B1…B4 a connected set of pixels of that exact colour
//!   whose bounding box is full (pixel count = area), at least 12×3 cells of
//!   the minimum cell size; the four boxes at the same height, in order
//!   B1, B2, B3, B4 from left to right; B5 "marker absent";
//! - V4/V5: as V3 with B5 "marker present"; V6: as V3 with B5 absent;
//! - calibration guard: none of the six fixture colours may appear in a
//!   reference screenshot taken before the fixture runs (otherwise the
//!   outcome is `inconclusive`).
//!
//! The mouse checks V1/V2 (language list of the live session's welcome
//! window) are not implemented here: see `docs/verification/F3-01`.

/// B1 = RGB (1, 254, 3).
pub const B1: [u8; 3] = [1, 254, 3];
/// B2 = RGB (251, 2, 249).
pub const B2: [u8; 3] = [251, 2, 249];
/// B3 = RGB (4, 5, 250).
pub const B3: [u8; 3] = [4, 5, 250];
/// B4 = (6·7, 200+13, 3⁴) = RGB (42, 213, 81).
pub const B4: [u8; 3] = [42, 213, 81];
/// B5 "marker present" = RGB (250, 250, 6).
pub const B5_PRESENT: [u8; 3] = [250, 250, 6];
/// B5 "marker absent" = RGB (6, 250, 250).
pub const B5_ABSENT: [u8; 3] = [6, 250, 250];

/// The six fixture colours.
pub const ALL: [[u8; 3]; 6] = [B1, B2, B3, B4, B5_PRESENT, B5_ABSENT];

/// RGB framebuffer.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height * 3` bytes.
    pub rgb: &'a [u8],
}

/// Bounding box of a full connected box of one colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxPx {
    /// Left.
    pub x: usize,
    /// Top.
    pub y: usize,
    /// Width.
    pub w: usize,
    /// Height.
    pub h: usize,
}

/// Minimum terminal cell size in pixels, from the calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibration {
    /// Minimum cell width.
    pub cell_w: usize,
    /// Minimum cell height.
    pub cell_h: usize,
}

/// Default calibration for GNOME Terminal at 1280×800 with the default font
/// (cells observed around 8–10 × 17–19 px): conservative minimums.
pub const DEFAULT_CALIBRATION: Calibration = Calibration {
    cell_w: 6,
    cell_h: 12,
};

/// State of the disk marker read from B5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `LIGHTVM-FX1` present at the start of `/dev/vda`.
    Present,
    /// Absent.
    Absent,
}

/// Outcome of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Band B1…B4 found and B5 in the given state.
    Ok(Marker),
    /// Fixture output not (fully) on screen, with the reason.
    NotFound(String),
}

impl Frame<'_> {
    fn px(&self, x: usize, y: usize) -> [u8; 3] {
        let o = (y * self.width + x) * 3;
        [self.rgb[o], self.rgb[o + 1], self.rgb[o + 2]]
    }

    /// `true` if any pixel has one of the six fixture colours (calibration
    /// guard on a reference screenshot).
    pub fn has_fixture_colour(&self) -> bool {
        self.rgb.as_chunks::<3>().0.iter().any(|p| ALL.contains(p))
    }

    /// Largest 4-connected component of exactly `colour`, if its bounding
    /// box is full.
    pub fn full_box(&self, colour: [u8; 3]) -> Option<BoxPx> {
        let (w, h) = (self.width, self.height);
        if self.rgb.len() < w * h * 3 {
            return None;
        }
        let mut seen = vec![false; w * h];
        let mut best: Option<(usize, BoxPx)> = None;
        let mut stack = Vec::new();
        for start in 0..w * h {
            if seen[start] || self.px(start % w, start / w) != colour {
                continue;
            }
            seen[start] = true;
            stack.push(start);
            let (mut x0, mut y0, mut x1, mut y1, mut n) = (w, h, 0, 0, 0usize);
            while let Some(i) = stack.pop() {
                let (x, y) = (i % w, i / w);
                n += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                let mut push = |j: usize| {
                    if !seen[j] && self.px(j % w, j / w) == colour {
                        seen[j] = true;
                        stack.push(j);
                    }
                };
                if x > 0 {
                    push(i - 1);
                }
                if x + 1 < w {
                    push(i + 1);
                }
                if y > 0 {
                    push(i - w);
                }
                if y + 1 < h {
                    push(i + w);
                }
            }
            let b = BoxPx {
                x: x0,
                y: y0,
                w: x1 - x0 + 1,
                h: y1 - y0 + 1,
            };
            if best.is_none_or(|(m, _)| n > m) {
                best = Some((n, b));
            }
        }
        best.and_then(|(n, b)| (n == b.w * b.h).then_some(b))
    }

    /// V3/V4/V5/V6: band B1…B4 and state of B5.
    pub fn check(&self, cal: Calibration) -> Check {
        let mut band = Vec::new();
        for (name, c) in [("B1", B1), ("B2", B2), ("B3", B3), ("B4", B4)] {
            match self.full_box(c) {
                Some(b) if b.w >= 12 * cal.cell_w && b.h >= 3 * cal.cell_h => band.push(b),
                Some(b) => return Check::NotFound(format!("{name} too small: {b:?}")),
                None => return Check::NotFound(format!("{name} not found as a full box")),
            }
        }
        if band
            .windows(2)
            .any(|p| p[0].y != p[1].y || p[0].x >= p[1].x)
        {
            return Check::NotFound(format!("band out of order or misaligned: {band:?}"));
        }
        let b5 = |c| {
            self.full_box(c)
                .filter(|b| b.w >= 36 * cal.cell_w && b.h >= 3 * cal.cell_h && b.y > band[0].y)
        };
        match (b5(B5_PRESENT), b5(B5_ABSENT)) {
            (Some(_), None) => Check::Ok(Marker::Present),
            (None, Some(_)) => Check::Ok(Marker::Absent),
            (Some(_), Some(_)) => Check::NotFound("B5 in both states".into()),
            (None, None) => Check::NotFound("B5 not found".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic terminal: 10×20 px cells, background grey.
    fn frame(marker: Option<Marker>, b4: [u8; 3]) -> Vec<u8> {
        let (w, h) = (640, 300);
        let mut rgb = vec![40u8; w * h * 3];
        let mut fill = |x0: usize, y0: usize, cw: usize, ch: usize, c: [u8; 3]| {
            for y in y0..y0 + ch * 20 {
                for x in x0..x0 + cw * 10 {
                    let o = (y * w + x) * 3;
                    rgb[o..o + 3].copy_from_slice(&c);
                }
            }
        };
        for (i, c) in [B1, B2, B3, b4].into_iter().enumerate() {
            fill(10 + i * 120, 20, 12, 3, c);
        }
        match marker {
            Some(Marker::Present) => fill(10, 80, 36, 3, B5_PRESENT),
            Some(Marker::Absent) => fill(10, 80, 36, 3, B5_ABSENT),
            None => {}
        }
        rgb
    }

    fn check(rgb: &[u8]) -> Check {
        Frame {
            width: 640,
            height: 300,
            rgb,
        }
        .check(DEFAULT_CALIBRATION)
    }

    #[test]
    fn band_and_marker_states() {
        assert_eq!(
            check(&frame(Some(Marker::Absent), B4)),
            Check::Ok(Marker::Absent)
        );
        assert_eq!(
            check(&frame(Some(Marker::Present), B4)),
            Check::Ok(Marker::Present)
        );
        assert!(matches!(check(&frame(None, B4)), Check::NotFound(_)));
        // Wrong computed colour (B4) → not found.
        assert!(matches!(
            check(&frame(Some(Marker::Absent), [42, 213, 80])),
            Check::NotFound(_)
        ));
        // A pixel of another colour inside a block: the box is not full.
        let mut rgb = frame(Some(Marker::Absent), B4);
        let o = (30 * 640 + 20) * 3;
        rgb[o..o + 3].copy_from_slice(&[0, 0, 0]);
        assert!(matches!(check(&rgb), Check::NotFound(_)));
        let plain = vec![40u8; 640 * 300 * 3];
        assert!(
            !Frame {
                width: 640,
                height: 300,
                rgb: &plain
            }
            .has_fixture_colour()
        );
        assert!(
            Frame {
                width: 640,
                height: 300,
                rgb: &frame(None, B4)
            }
            .has_fixture_colour()
        );
    }
}
