//! Screenshot via QMP `screendump` (ROADMAP 6.4).
//!
//! The temporary file is chosen only by the backend in the run's private
//! directory; the PPM is read with a size limit, validated,
//! converted to PNG in memory and deleted in every case.
//!
//! [`CapturedScreenshot`] is local because it carries the image bytes;
//! [`CapturedScreenshot::metadata`] produces the `lightvm-proto` metadata
//! ([`lightvm_proto::Screenshot`]) that travels over IPC/MCP.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lightvm_proto::{DisplayGeometry, ImageFormat, RunId, Sha256Digest, UtcTimestamp, VmId};
use sha2::{Digest, Sha256};

use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::qmp::QmpClient;

/// Maximum accepted side in pixels (equal to [`DisplayGeometry::MAX_SIDE`]).
pub const MAX_SIDE: u32 = DisplayGeometry::MAX_SIDE;

/// Screenshot captured by the backend: in-memory PNG plus metadata.
#[derive(Clone, Debug)]
pub struct CapturedScreenshot {
    /// Run it belongs to.
    pub run_id: RunId,
    /// Display geometry at capture time.
    pub geometry: DisplayGeometry,
    /// 8-bit RGB PNG image.
    pub png: Vec<u8>,
    /// SHA-256 of the PNG.
    pub sha256: Sha256Digest,
    /// Capture instant.
    pub captured_at: UtcTimestamp,
}

impl CapturedScreenshot {
    /// Public metadata (no bytes or paths).
    pub fn metadata(&self, vm_id: VmId) -> lightvm_proto::Screenshot {
        lightvm_proto::Screenshot {
            vm_id,
            run_id: self.run_id,
            geometry: self.geometry,
            format: ImageFormat::Png,
            sha256: self.sha256,
            byte_len: self.png.len() as u64,
            captured_at: self.captured_at,
        }
    }
}

/// Maximum accepted PPM file size (maximum side squared × 3
/// plus header).
pub const MAX_PPM_BYTES: u64 = (MAX_SIDE as u64) * (MAX_SIDE as u64) * 3 + 64;

/// RGB image with 8 bits per channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbImage {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Contiguous RGB pixels.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Percentage of non-black pixels (threshold 16 on each channel).
    pub fn non_black_percent(&self) -> f64 {
        let n = self
            .data
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|p| p.iter().any(|&c| c > 16))
            .count();
        n as f64 * 100.0 / (f64::from(self.width) * f64::from(self.height)).max(1.0)
    }
}

/// Parses a binary PPM (P6, maxval 255) with maximum side `max_side`.
pub fn parse_ppm(bytes: &[u8], max_side: u32) -> Result<RgbImage> {
    let (w, h, pos) = parse_ppm_header(bytes, max_side)?;
    let need = w as usize * h as usize * 3;
    if bytes.len() != pos + need {
        return Err(invalid_data(format!(
            "PPM data of {} bytes, expected {need}",
            bytes.len().saturating_sub(pos)
        )));
    }
    Ok(RgbImage {
        width: w,
        height: h,
        data: bytes[pos..].to_vec(),
    })
}

fn invalid_data(m: String) -> BackendError {
    BackendError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, m))
}

/// Header of a binary PPM (P6, maxval 255): width, height and offset of the
/// pixel data. Sides above `max_side` give `resource_limit`.
pub fn parse_ppm_header(bytes: &[u8], max_side: u32) -> Result<(u32, u32, usize)> {
    let bad = invalid_data;
    let mut pos = 0usize;
    let mut fields: Vec<&str> = Vec::with_capacity(4);
    while fields.len() < 4 {
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
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && pos - start < 16 {
            pos += 1;
        }
        if start == pos {
            return Err(bad("truncated PPM header".into()));
        }
        fields.push(std::str::from_utf8(&bytes[start..pos]).map_err(|e| bad(format!("PPM: {e}")))?);
    }
    if pos >= bytes.len() || !bytes[pos].is_ascii_whitespace() {
        return Err(bad("unterminated PPM header".into()));
    }
    pos += 1;
    if fields[0] != "P6" {
        return Err(bad(format!(
            "format {} not supported (expected P6)",
            fields[0]
        )));
    }
    let num = |s: &str| s.parse::<u32>().map_err(|e| bad(format!("PPM {s}: {e}")));
    let (w, h, max) = (num(fields[1])?, num(fields[2])?, num(fields[3])?);
    if w == 0 || h == 0 || max != 255 {
        return Err(bad(format!("PPM {w}x{h} maxval {max} not accepted")));
    }
    if w > max_side || h > max_side {
        return Err(BackendError::ResourceLimit(format!(
            "screen {w}x{h} exceeds the maximum side {max_side}"
        )));
    }
    Ok((w, h, pos))
}

/// Encodes an RGB PNG in memory (`Fast` compression).
pub fn encode_png(img: &RgbImage) -> Result<Vec<u8>> {
    let err = |e: png::EncodingError| BackendError::Io(std::io::Error::other(e.to_string()));
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut w = enc.write_header().map_err(err)?;
        w.write_image_data(&img.data).map_err(err)?;
        w.finish().map_err(err)?;
    }
    Ok(out)
}

/// Removes the temporary file when leaving the scope, even on error.
struct TempFile<'a>(&'a ManagedPath);

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.as_path());
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Runs `screendump` in the private directory and returns PNG and image.
/// A PNG larger than `max_png_bytes` is rejected with `resource_limit`.
pub(crate) async fn capture(
    qmp: &QmpClient,
    private_dir: &ManagedPath,
    run_id: RunId,
    timeout: Duration,
    max_png_bytes: u64,
) -> Result<(CapturedScreenshot, RgbImage)> {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = private_dir.join_file(&format!("screendump-{n}.ppm"))?;
    let _ = std::fs::remove_file(file.as_path());
    let guard = TempFile(&file);
    qmp.screendump_ppm(&file, timeout).await?;
    let path = file.as_path().to_owned();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let f = std::fs::File::open(&path)?;
        let len = f.metadata()?.len();
        if len > MAX_PPM_BYTES {
            return Err(BackendError::ResourceLimit(format!(
                "screendump file of {len} bytes"
            )));
        }
        let mut buf = Vec::with_capacity(len as usize);
        f.take(MAX_PPM_BYTES + 1).read_to_end(&mut buf)?;
        Ok(buf)
    })
    .await
    .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))??;
    drop(guard);
    let img = tokio::task::spawn_blocking(move || parse_ppm(&bytes, MAX_SIDE))
        .await
        .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))??;
    let geometry = DisplayGeometry::new(img.width, img.height)?;
    let png = encode_png(&img)?;
    if png.len() as u64 > max_png_bytes {
        return Err(BackendError::ResourceLimit(format!(
            "PNG screenshot of {} bytes (maximum {max_png_bytes})",
            png.len()
        )));
    }
    let sha256 = Sha256Digest::from_bytes(Sha256::digest(&png).into());
    Ok((
        CapturedScreenshot {
            run_id,
            geometry,
            png,
            sha256,
            captured_at: UtcTimestamp::now(),
        },
        img,
    ))
}

/// Current display geometry from a `screendump` of which only the PPM
/// header is read (no decoding, no PNG): cheaper than [`capture`]. The file
/// size must match the header. The temporary file is removed in every case.
pub(crate) async fn capture_geometry(
    qmp: &QmpClient,
    private_dir: &ManagedPath,
    timeout: Duration,
) -> Result<DisplayGeometry> {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = private_dir.join_file(&format!("screendump-{n}.ppm"))?;
    let _ = std::fs::remove_file(file.as_path());
    let guard = TempFile(&file);
    qmp.screendump_ppm(&file, timeout).await?;
    let path = file.as_path().to_owned();
    let (w, h) = tokio::task::spawn_blocking(move || -> Result<(u32, u32)> {
        let f = std::fs::File::open(&path)?;
        let len = f.metadata()?.len();
        let mut head = Vec::with_capacity(64);
        f.take(64).read_to_end(&mut head)?;
        let (w, h, pos) = parse_ppm_header(&head, MAX_SIDE)?;
        let need = u64::from(w) * u64::from(h) * 3 + pos as u64;
        if len != need {
            return Err(invalid_data(format!(
                "screendump of {len} bytes, expected {need}"
            )));
        }
        Ok((w, h))
    })
    .await
    .map_err(|e| BackendError::Io(std::io::Error::other(e.to_string())))??;
    drop(guard);
    Ok(DisplayGeometry::new(w, h)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_header_only() {
        let (w, h, pos) = parse_ppm_header(b"P6\n# c\n640 480\n255\n", MAX_SIDE).unwrap();
        assert_eq!((w, h), (640, 480));
        assert_eq!(pos, 19);
        assert!(parse_ppm_header(b"P5\n1 1\n255\n", MAX_SIDE).is_err());
        assert!(matches!(
            parse_ppm_header(b"P6\n1 99999\n255\n", MAX_SIDE),
            Err(BackendError::ResourceLimit(_))
        ));
    }

    #[test]
    fn valid_ppm_and_png() {
        let mut ppm = b"P6\n# comment\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[255, 0, 0, 0, 0, 0]);
        let img = parse_ppm(&ppm, MAX_SIDE).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert!((img.non_black_percent() - 50.0).abs() < 1e-9);
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn public_metadata() {
        let png = vec![1u8, 2, 3];
        let s = CapturedScreenshot {
            run_id: RunId::from_unix_millis(1_700_000_000_000, [1; 10]),
            geometry: DisplayGeometry::new(2, 1).unwrap(),
            sha256: Sha256Digest::from_bytes(Sha256::digest(&png).into()),
            png,
            captured_at: UtcTimestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        };
        let vm = VmId::from_unix_millis(1_700_000_000_000, [2; 10]);
        let m = s.metadata(vm);
        assert_eq!(m.vm_id, vm);
        assert_eq!(m.run_id, s.run_id);
        assert_eq!(m.byte_len, 3);
        assert_eq!(m.format, ImageFormat::Png);
        assert_eq!(m.sha256, s.sha256);
    }

    #[test]
    fn invalid_ppm() {
        assert!(parse_ppm(b"P3\n1 1\n255\n000", MAX_SIDE).is_err());
        assert!(matches!(
            parse_ppm(b"P6\n99999 1\n255\n", MAX_SIDE),
            Err(BackendError::ResourceLimit(_))
        ));
        assert!(parse_ppm(b"P6\n2 2\n255\n\x00", MAX_SIDE).is_err());
        assert!(parse_ppm(b"P6\n1 1\n65535\n\x00\x00\x00", MAX_SIDE).is_err());
        assert!(parse_ppm(b"P6\n1 1\n255\n\x00\x00\x00\x00", MAX_SIDE).is_err());
        assert!(parse_ppm(b"P6", MAX_SIDE).is_err());
        assert!(parse_ppm(b"", MAX_SIDE).is_err());
    }
}
