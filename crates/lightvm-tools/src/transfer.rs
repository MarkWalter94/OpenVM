//! Streaming guest-side upload/download helpers. Cancellation cancels an
//! upload on the host; an interrupted download leaves no completed target.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::files::MAX_FILE_BYTES;
use crate::policy::Secret;
use crate::protocol::{self, FileReply, FileRequest, MAX_CHUNK, RelativePath, invalid};
use crate::sandbox::Directory;
use crate::server::FileClient;

/// Expected content of a download, obtained from its approved manifest.
#[derive(Clone, Debug)]
pub struct Manifest {
    pub size: u64,
    pub sha256: String,
}

fn remote(reply: FileReply) -> io::Error {
    let kind = match &reply {
        FileReply::Error { code } if code == "permission_denied" => io::ErrorKind::PermissionDenied,
        FileReply::Error { code } if code == "not_found" => io::ErrorKind::NotFound,
        FileReply::Error { code } if code == "already_exists" => io::ErrorKind::AlreadyExists,
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, "tools operation refused")
}

fn expect_ok(reply: FileReply) -> io::Result<()> {
    match reply {
        FileReply::Ok => Ok(()),
        other => Err(remote(other)),
    }
}

fn cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "transfer cancelled",
        ))
    } else {
        Ok(())
    }
}

/// Sends a regular file without loading it into memory. Hashing and sending
/// use the same held file handle. If it changes during the transfer the
/// host's independent hash/length check prevents publication.
pub fn upload(
    client: &mut FileClient,
    source: &mut File,
    share: &str,
    target: RelativePath,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<()> {
    target.validate()?;
    let metadata = source.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(invalid("regular file within size limit required"));
    }
    let size = metadata.len();
    source.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; MAX_CHUNK];
    let mut hashed = 0u64;
    loop {
        cancelled(cancel)?;
        let n = source.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hashed += n as u64;
        if hashed > size {
            return Err(invalid("source changed"));
        }
        hash.update(&buf[..n]);
    }
    if hashed != size {
        return Err(invalid("source changed"));
    }
    let id = match client.call(&FileRequest::Begin {
        replace: false,
        share: share.into(),
        path: target,
        size,
        sha256: protocol::encode(&hash.finalize()),
    })? {
        FileReply::Transfer { transfer } => transfer,
        other => return Err(remote(other)),
    };
    let result = (|| {
        source.seek(SeekFrom::Start(0))?;
        let mut sent = 0;
        progress(0, size);
        loop {
            cancelled(cancel)?;
            let n = source.read(&mut buf)?;
            if n == 0 {
                break;
            }
            expect_ok(client.call(&FileRequest::Write {
                transfer: id.clone(),
                offset: sent,
                data: protocol::encode(&buf[..n]),
            })?)?;
            sent += n as u64;
            progress(sent, size);
        }
        cancelled(cancel)?;
        expect_ok(client.call(&FileRequest::Commit {
            transfer: id.clone(),
        })?)
    })();
    if result.is_err() {
        let _ = client.call(&FileRequest::Cancel { transfer: id });
    }
    result
}

/// Receives into a create-only destination selected inside the guest. The
/// caller supplies the expected size and digest, obtained from the approved
/// transfer manifest. The final name appears only after verification. The
/// selected destination is absolute; symlinks in its parent are refused.
pub fn download(
    client: &mut FileClient,
    share: &str,
    source: RelativePath,
    destination: &Path,
    manifest: &Manifest,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<()> {
    source.validate()?;
    let size = manifest.size;
    let expected = protocol::decode(&manifest.sha256, 32)?;
    if expected.len() != 32 || size > MAX_FILE_BYTES {
        return Err(invalid("invalid file manifest"));
    }
    let parent = Directory::open(
        destination
            .parent()
            .ok_or_else(|| invalid("destination parent required"))?,
    )?;
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid("invalid destination name"))?;
    RelativePath::new(vec![name.into()])?;
    let staging = format!(".lightvm-transfer-{}", Secret::generate()?.export());
    let mut file = parent.create_file(&staging)?;
    let result = (|| {
        let mut received = 0;
        let mut hash = Sha256::new();
        progress(0, size);
        loop {
            cancelled(cancel)?;
            let (offset, data, eof) = match client.call(&FileRequest::Read {
                share: share.into(),
                path: source.clone(),
                offset: received,
                length: MAX_CHUNK as u32,
            })? {
                FileReply::Data { offset, data, eof } => {
                    (offset, protocol::decode(&data, MAX_CHUNK)?, eof)
                }
                other => return Err(remote(other)),
            };
            if offset != received
                || data.len() as u64 > size.saturating_sub(received)
                || (data.is_empty() && !eof)
            {
                return Err(invalid("invalid file chunk"));
            }
            file.write_all(&data)?;
            hash.update(&data);
            received += data.len() as u64;
            progress(received, size);
            if eof {
                break;
            }
        }
        if received != size || hash.finalize().as_slice() != expected {
            return Err(invalid("file changed or checksum mismatch"));
        }
        cancelled(cancel)?;
        file.sync_all()
    })();
    drop(file);
    let result = result.and_then(|()| parent.publish(&staging, name));
    let _ = parent.remove(&staging);
    result
}
