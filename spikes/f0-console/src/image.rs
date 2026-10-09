//! PPM (P6) → RGB → PNG in memoria, con validazione delle dimensioni.

/// Immagine RGB a 8 bit per canale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbImage {
    /// Larghezza.
    pub width: u32,
    /// Altezza.
    pub height: u32,
    /// Pixel RGB contigui.
    pub data: Vec<u8>,
}

/// Lato massimo accettato (difesa contro file anomali).
pub const MAX_SIDE: u32 = 8192;

impl RgbImage {
    /// Percentuale di pixel non neri.
    pub fn non_black_percent(&self) -> f64 {
        let n = self
            .data
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|p| p.iter().any(|&c| c > 16))
            .count();
        n as f64 * 100.0 / f64::from((self.width * self.height).max(1))
    }

    /// Numero di pixel diversi tra due immagini delle stesse dimensioni.
    pub fn diff_pixels(&self, other: &Self) -> Option<usize> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        Some(
            self.data
                .as_chunks::<3>()
                .0
                .iter()
                .zip(other.data.as_chunks::<3>().0.iter())
                .filter(|(a, b)| a != b)
                .count(),
        )
    }

    /// Rettangolo che contiene i pixel diversi (x0, y0, x1, y1), se ce ne sono.
    pub fn diff_bbox(&self, other: &Self) -> Option<(u32, u32, u32, u32)> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        let mut bb: Option<(u32, u32, u32, u32)> = None;
        for (i, (a, b)) in self
            .data
            .as_chunks::<3>()
            .0
            .iter()
            .zip(other.data.as_chunks::<3>().0.iter())
            .enumerate()
        {
            if a != b {
                let x = i as u32 % self.width;
                let y = i as u32 / self.width;
                bb = Some(match bb {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        bb
    }
}

/// Analizza un PPM binario (P6, maxval 255).
pub fn parse_ppm(bytes: &[u8]) -> Result<RgbImage, String> {
    let mut pos = 0usize;
    let mut fields = Vec::with_capacity(4);
    while fields.len() < 4 {
        // salta spazi e commenti
        while pos < bytes.len() && (bytes[pos].is_ascii_whitespace() || bytes[pos] == b'#') {
            if bytes[pos] == b'#' {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
            } else {
                pos += 1;
            }
        }
        let start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if start == pos {
            return Err("intestazione PPM troncata".into());
        }
        fields.push(
            std::str::from_utf8(&bytes[start..pos])
                .map_err(|e| e.to_string())?
                .to_owned(),
        );
    }
    pos += 1; // un solo carattere di spazio dopo maxval
    if fields[0] != "P6" {
        return Err(format!("formato {} non supportato (atteso P6)", fields[0]));
    }
    let num = |s: &str| s.parse::<u32>().map_err(|e| format!("{s}: {e}"));
    let (w, h, max) = (num(&fields[1])?, num(&fields[2])?, num(&fields[3])?);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || max != 255 {
        return Err(format!("PPM {w}x{h} maxval {max} non accettato"));
    }
    let need = w as usize * h as usize * 3;
    let data = bytes
        .get(pos..pos + need)
        .ok_or("dati PPM troncati")?
        .to_vec();
    if bytes.len() != pos + need {
        return Err("byte in eccesso dopo i pixel PPM".into());
    }
    Ok(RgbImage {
        width: w,
        height: h,
        data,
    })
}

/// Codifica PNG RGB in memoria (compressione `Fast` del crate png).
pub fn encode_png(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&img.data).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_valido_e_png() {
        let mut ppm = b"P6\n# commento\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[255, 0, 0, 0, 0, 0]);
        let img = parse_ppm(&ppm).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert!((img.non_black_percent() - 50.0).abs() < 1e-9);
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn ppm_non_valido() {
        assert!(parse_ppm(b"P3\n1 1\n255\n000").is_err());
        assert!(parse_ppm(b"P6\n99999 1\n255\n").is_err());
        assert!(parse_ppm(b"P6\n2 2\n255\n\x00").is_err());
    }
}
