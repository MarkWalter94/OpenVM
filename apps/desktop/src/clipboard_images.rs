//! Bounded clipboard images. LIMG + little-endian width/height + RGBA8;
//! data is streamed in 16 KiB chunks over the clipboard capability.
use lightvm_proto::{ToolFileReply as Reply, ToolFileRequest as Request, ToolRelativePath};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_PIXELS: usize = 4 * 1024 * 1024;
const CHUNK: usize = 16384;
pub type Result<T> = std::result::Result<T, String>;

pub fn dimensions(data: &[u8]) -> Result<(usize, usize)> {
    if data.len() < 12 || &data[..4] != b"LIMG" {
        return Err("Invalid clipboard image header".into());
    }
    let width = u32::from_le_bytes(data[4..8].try_into().expect("width")) as usize;
    let height = u32::from_le_bytes(data[8..12].try_into().expect("height")) as usize;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || width.checked_mul(height).is_none_or(|n| n > MAX_PIXELS)
    {
        return Err("Clipboard image exceeds 4 million pixels or 8192 pixels per side".into());
    }
    Ok((width, height))
}

pub fn pack(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut data = Vec::from(b"LIMG".as_slice());
    data.extend_from_slice(
        &u32::try_from(width)
            .map_err(|_| "Image width")?
            .to_le_bytes(),
    );
    data.extend_from_slice(
        &u32::try_from(height)
            .map_err(|_| "Image height")?
            .to_le_bytes(),
    );
    let (width, height) = dimensions(&data)?;
    if rgba.len() != width * height * 4 {
        return Err("Invalid clipboard image length".into());
    }
    data.extend_from_slice(rgba);
    Ok(data)
}

fn hex(data: &[u8]) -> String {
    data.iter()
        .flat_map(|b| {
            [
                char::from(b"0123456789abcdef"[(b >> 4) as usize]),
                char::from(b"0123456789abcdef"[(b & 15) as usize]),
            ]
        })
        .collect()
}
fn decode(data: &str) -> Result<Vec<u8>> {
    if data.len() > CHUNK * 2 || !data.len().is_multiple_of(2) {
        return Err("Invalid image chunk".into());
    }
    data.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            Ok(
                ((p[0] as char).to_digit(16).ok_or("Invalid image hex")? * 16
                    + (p[1] as char).to_digit(16).ok_or("Invalid image hex")?)
                    as u8,
            )
        })
        .collect()
}
fn path() -> ToolRelativePath {
    ToolRelativePath::new(vec!["image".into()]).expect("image name")
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("Clipboard image transfer cancelled".into())
    } else {
        Ok(())
    }
}
fn ok(reply: Reply) -> Result<()> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error { code } => Err(format!("Guest image clipboard: {code}")),
        _ => Err("Invalid image clipboard reply".into()),
    }
}

pub fn send(
    data: &[u8],
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
    call: &mut impl FnMut(Request) -> Result<Reply>,
) -> Result<()> {
    let (width, height) = dimensions(data)?;
    if data.len() != 12 + width * height * 4 {
        return Err("Invalid image length".into());
    }
    check(cancel)?;
    let transfer = match call(Request::Begin {
        replace: false,
        share: "clipboard-write".into(),
        path: path(),
        size: data.len() as u64,
        sha256: hex(&Sha256::digest(data)),
    })? {
        Reply::Transfer { transfer } => transfer,
        other => return Err(format!("Guest did not start image upload: {other:?}")),
    };
    let result = (|| {
        for (i, chunk) in data.chunks(CHUNK).enumerate() {
            check(cancel)?;
            ok(call(Request::Write {
                transfer: transfer.clone(),
                offset: (i * CHUNK) as u64,
                data: hex(chunk),
            })?)?;
            progress((i * CHUNK + chunk.len()) as u64, data.len() as u64);
        }
        check(cancel)?;
        ok(call(Request::Commit {
            transfer: transfer.clone(),
        })?)
    })();
    if result.is_err() {
        let _ = call(Request::Cancel { transfer });
    }
    result
}

pub fn receive(
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
    call: &mut impl FnMut(Request) -> Result<Reply>,
) -> Result<Vec<u8>> {
    check(cancel)?;
    let token = match call(Request::Begin {
        replace: false,
        share: "clipboard-read".into(),
        path: path(),
        size: 0,
        sha256: "0".repeat(64),
    })? {
        Reply::Transfer { transfer } => transfer,
        other => return Err(format!("Guest did not capture an image: {other:?}")),
    };
    let result = (|| {
        let mut data = Vec::new();
        let mut size = None;
        loop {
            check(cancel)?;
            let (offset, chunk, eof) = match call(Request::Read {
                share: token.clone(),
                path: path(),
                offset: data.len() as u64,
                length: CHUNK as u32,
            })? {
                Reply::Data { offset, data, eof } => (offset, decode(&data)?, eof),
                _ => return Err("Invalid image capture reply".into()),
            };
            if offset != data.len() as u64 || chunk.is_empty() {
                return Err("Invalid image capture offset".into());
            }
            if data.is_empty() {
                let (width, height) = dimensions(&chunk)?;
                size = Some(12 + width * height * 4);
            }
            let size = size.ok_or("Missing image header")?;
            if chunk.len() > size.saturating_sub(data.len()) {
                return Err("Image length mismatch".into());
            }
            data.extend_from_slice(&chunk);
            progress(data.len() as u64, size as u64);
            if eof {
                if data.len() != size {
                    return Err("Truncated image".into());
                }
                break;
            }
        }
        check(cancel)?;
        Ok(data)
    })();
    let _ = call(Request::Cancel { transfer: token });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_stream_round_trip_uses_tokens_chunks_and_digest() {
        let pixels = vec![0x73; 201 * 101 * 4];
        let image = pack(201, 101, &pixels).unwrap();
        let mut uploaded = Vec::new();
        let mut committed = false;
        let token = "ab".repeat(32);
        send(&image, &AtomicBool::new(false), |_, _| {}, &mut |request| {
            request.validate_clipboard_image().unwrap();
            match request {
                Request::Begin { sha256, share, .. } => {
                    assert_eq!(share, "clipboard-write");
                    assert_eq!(sha256, hex(&Sha256::digest(&image)));
                    Ok(Reply::Transfer {
                        transfer: token.clone(),
                    })
                }
                Request::Write { offset, data, .. } => {
                    assert_eq!(offset, uploaded.len() as u64);
                    uploaded.extend(decode(&data)?);
                    Ok(Reply::Ok)
                }
                Request::Commit { .. } => {
                    committed = true;
                    Ok(Reply::Ok)
                }
                _ => panic!("unexpected request"),
            }
        })
        .unwrap();
        assert!(committed);
        assert_eq!(uploaded, image);
        let result = receive(&AtomicBool::new(false), |_, _| {}, &mut |request| {
            request.validate_clipboard_image().unwrap();
            match request {
                Request::Begin { share, .. } => {
                    assert_eq!(share, "clipboard-read");
                    Ok(Reply::Transfer {
                        transfer: token.clone(),
                    })
                }
                Request::Read {
                    share,
                    offset,
                    length,
                    ..
                } => {
                    assert_eq!(share, token);
                    let end = (offset as usize + length as usize).min(image.len());
                    Ok(Reply::Data {
                        offset,
                        data: hex(&image[offset as usize..end]),
                        eof: end == image.len(),
                    })
                }
                Request::Cancel { transfer } => {
                    assert_eq!(transfer, token);
                    Ok(Reply::Ok)
                }
                _ => panic!("unexpected request"),
            }
        })
        .unwrap();
        assert_eq!(result, image);
    }

    #[test]
    fn image_cancel_never_commits() {
        let image = pack(100, 100, &vec![0; 100 * 100 * 4]).unwrap();
        let cancel = AtomicBool::new(false);
        let mut cancelled = false;
        assert!(
            send(&image, &cancel, |_, _| {}, &mut |request| match request {
                Request::Begin { .. } => Ok(Reply::Transfer {
                    transfer: "ab".repeat(32)
                }),
                Request::Write { .. } => {
                    cancel.store(true, Ordering::Relaxed);
                    Ok(Reply::Ok)
                }
                Request::Cancel { .. } => {
                    cancelled = true;
                    Ok(Reply::Ok)
                }
                _ => panic!("must not commit after cancellation"),
            })
            .is_err()
        );
        assert!(cancelled);
    }

    #[test]
    fn malformed_images_are_bounded_before_allocation() {
        assert!(pack(0, 1, &[]).is_err());
        assert!(pack(8193, 1, &[]).is_err());
        assert!(pack(4096, 4096, &[]).is_err());
        assert!(pack(1, 1, &[1, 2, 3]).is_err());
        for bytes in [
            b"bad".to_vec(),
            b"LIMG\xff\xff\xff\xff\xff\xff\xff\xff".to_vec(),
            b"LIMG\x01\0\0\0\x01\0\0\0".to_vec(),
        ] {
            let mut cancelled = false;
            assert!(
                receive(
                    &AtomicBool::new(false),
                    |_, _| {},
                    &mut |request| match request {
                        Request::Begin { .. } => Ok(Reply::Transfer {
                            transfer: "ab".repeat(32)
                        }),
                        Request::Read { offset, .. } => Ok(Reply::Data {
                            offset,
                            data: hex(&bytes),
                            eof: true
                        }),
                        Request::Cancel { .. } => {
                            cancelled = true;
                            Ok(Reply::Ok)
                        }
                        _ => panic!("unexpected request"),
                    }
                )
                .is_err()
            );
            assert!(cancelled);
        }
    }
}
