//! Screenshot tramite QMP `screendump` (ROADMAP 6.4).
//!
//! Il file temporaneo è scelto soltanto dal backend nella directory privata
//! dell'esecuzione; il PPM viene letto con un limite di dimensione, validato,
//! convertito in PNG in memoria e cancellato in ogni caso.
//!
//! [`CapturedScreenshot`] è locale perché porta i byte dell'immagine;
//! [`CapturedScreenshot::metadata`] produce i metadati di `lightvm-proto`
//! ([`lightvm_proto::Screenshot`]) che viaggiano su IPC/MCP.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lightvm_proto::{DisplayGeometry, ImageFormat, RunId, Sha256Digest, UtcTimestamp, VmId};
use sha2::{Digest, Sha256};

use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::qmp::QmpClient;

/// Lato massimo accettato in pixel (uguale a [`DisplayGeometry::MAX_SIDE`]).
pub const MAX_SIDE: u32 = DisplayGeometry::MAX_SIDE;

/// Screenshot catturato dal backend: PNG in memoria più metadati.
#[derive(Clone, Debug)]
pub struct CapturedScreenshot {
    /// Esecuzione a cui appartiene.
    pub run_id: RunId,
    /// Geometria del display al momento della cattura.
    pub geometry: DisplayGeometry,
    /// Immagine PNG RGB a 8 bit.
    pub png: Vec<u8>,
    /// SHA-256 del PNG.
    pub sha256: Sha256Digest,
    /// Istante della cattura.
    pub captured_at: UtcTimestamp,
}

impl CapturedScreenshot {
    /// Metadati pubblici (senza byte né percorsi).
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

/// Dimensione massima del file PPM accettata (lato massimo al quadrato × 3
/// più intestazione).
pub const MAX_PPM_BYTES: u64 = (MAX_SIDE as u64) * (MAX_SIDE as u64) * 3 + 64;

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

impl RgbImage {
    /// Percentuale di pixel non neri (soglia 16 su ogni canale).
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

/// Analizza un PPM binario (P6, maxval 255) con lato massimo `max_side`.
pub fn parse_ppm(bytes: &[u8], max_side: u32) -> Result<RgbImage> {
    let bad = |m: String| BackendError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, m));
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
            return Err(bad("intestazione PPM troncata".into()));
        }
        fields.push(std::str::from_utf8(&bytes[start..pos]).map_err(|e| bad(format!("PPM: {e}")))?);
    }
    if pos >= bytes.len() || !bytes[pos].is_ascii_whitespace() {
        return Err(bad("intestazione PPM non terminata".into()));
    }
    pos += 1;
    if fields[0] != "P6" {
        return Err(bad(format!(
            "formato {} non supportato (atteso P6)",
            fields[0]
        )));
    }
    let num = |s: &str| s.parse::<u32>().map_err(|e| bad(format!("PPM {s}: {e}")));
    let (w, h, max) = (num(fields[1])?, num(fields[2])?, num(fields[3])?);
    if w == 0 || h == 0 || max != 255 {
        return Err(bad(format!("PPM {w}x{h} maxval {max} non accettato")));
    }
    if w > max_side || h > max_side {
        return Err(BackendError::ResourceLimit(format!(
            "schermata {w}x{h} oltre il lato massimo {max_side}"
        )));
    }
    let need = w as usize * h as usize * 3;
    if bytes.len() != pos + need {
        return Err(bad(format!(
            "dati PPM di {} byte, attesi {need}",
            bytes.len().saturating_sub(pos)
        )));
    }
    Ok(RgbImage {
        width: w,
        height: h,
        data: bytes[pos..].to_vec(),
    })
}

/// Codifica PNG RGB in memoria (compressione `Fast`).
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

/// Rimuove il file temporaneo all'uscita dallo scope, anche in caso di errore.
struct TempFile<'a>(&'a ManagedPath);

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.as_path());
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Esegue `screendump` nella directory privata e restituisce PNG e immagine.
/// Il PNG oltre `max_png_bytes` è rifiutato con `resource_limit`.
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
                "file screendump di {len} byte"
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
            "screenshot PNG di {} byte (massimo {max_png_bytes})",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_valido_e_png() {
        let mut ppm = b"P6\n# commento\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[255, 0, 0, 0, 0, 0]);
        let img = parse_ppm(&ppm, MAX_SIDE).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert!((img.non_black_percent() - 50.0).abs() < 1e-9);
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn metadati_pubblici() {
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
    fn ppm_non_valido() {
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
