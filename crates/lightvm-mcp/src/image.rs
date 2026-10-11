//! Screenshot image for the model: the full PNG produced by the backend,
//! or a half-size copy (2×2 box average) when the client asks for it to
//! save tokens. The session report always keeps the full PNG (the service
//! stores it); verifiers never use the half-size copy.

/// Decodes an 8-bit RGB/RGBA PNG to RGB.
pub(crate) fn decode_rgb(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        _ => return None,
    };
    Some((info.width, info.height, rgb))
}

/// Half-size PNG (2×2 box average): `(width, height, png)`.
pub(crate) fn half(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, rgb) = decode_rgb(bytes)?;
    let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
    let (wu, hwu) = (w as usize, hw as usize);
    let mut out = vec![0u8; hwu * hh as usize * 3];
    for y in 0..hh as usize {
        for x in 0..hwu {
            for c in 0..3 {
                let mut sum = 0u32;
                let mut n = 0u32;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    if sx < wu && sy < h as usize {
                        sum += u32::from(rgb[(sy * wu + sx) * 3 + c]);
                        n += 1;
                    }
                }
                out[(y * hwu + x) * 3 + c] = u8::try_from(sum / n.max(1)).unwrap_or(u8::MAX);
            }
        }
    }
    let mut png_out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png_out, hw, hh);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&out).ok()?;
    }
    Some((hw, hh, png_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_size_round_trip() {
        let (w, h) = (8u32, 6u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&rgb).unwrap();
        }
        let (hw, hh, out) = half(&png_bytes).unwrap();
        assert_eq!((hw, hh), (4, 3));
        let (dw, dh, drgb) = decode_rgb(&out).unwrap();
        assert_eq!((dw, dh, drgb.len()), (4, 3, 36));
    }
}
