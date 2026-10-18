//! Streaming file service over explicitly configured shares. A transfer is
//! bound to the permit under which it began. Publication requires an exact
//! length and SHA-256; partial files are never visible under the final name.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::policy::{Feature, Permit, Secret};
use crate::protocol::{self, FileReply, FileRequest, MAX_CHUNK, RelativePath, invalid};
use crate::sandbox::{Directory, STAGING_PREFIX, reserved_name};

/// Default per-file limit (4 GiB); no whole-file buffers are used.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_TRANSFERS: usize = 8;
pub const TRANSFER_IDLE: Duration = Duration::from_secs(120);
pub const MAX_RECORDS: usize = 1024;

/// Limits apply across reconnects. Cancelled writes still consume the byte
/// budget, so a guest cannot refill the host by repeatedly cancelling.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub total_bytes: u64,
    pub files: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: MAX_FILE_BYTES,
            total_bytes: 8 * 1024 * 1024 * 1024,
            files: 1024,
        }
    }
}

#[derive(Debug)]
pub struct Share {
    path: std::path::PathBuf,
    root: Directory,
    writable: bool,
}

impl Share {
    /// Only called with a host path selected by the user, never guest input.
    pub fn open(path: &Path, writable: bool) -> io::Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            root: Directory::open(path)?,
            writable,
        })
    }
}

#[derive(Clone, Debug)]
pub struct FileRecord {
    pub operation: &'static str,
    pub share: String,
    pub path: RelativePath,
    pub bytes: u64,
    pub outcome: &'static str,
}

#[derive(Debug)]
struct Transfer {
    journal: Option<crate::journal::Cleanup>,
    replace: bool,
    parent: Directory,
    staging: String,
    name: String,
    file: Option<File>,
    share: String,
    path: RelativePath,
    permit: Permit,
    size: u64,
    written: u64,
    expected: Vec<u8>,
    hash: Sha256,
    last: Instant,
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.file.take();
        let removed = self.parent.remove(&self.staging);
        if (removed.is_ok()
            || removed
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::NotFound))
            && let Some(journal) = self.journal.take()
        {
            let _ = journal.finish();
        }
    }
}

#[derive(Debug, Default)]
pub struct FileService {
    journal: Option<crate::journal::Journal>,
    shares: BTreeMap<String, Share>,
    transfers: BTreeMap<String, Transfer>,
    records: VecDeque<FileRecord>,
    limits: Limits,
    bytes: u64,
    begun: u64,
}

impl FileService {
    pub fn with_journal(mut self, journal: crate::journal::Journal) -> io::Result<Self> {
        journal.recover()?;
        self.journal = Some(journal);
        Ok(self)
    }
    pub fn new(shares: BTreeMap<String, Share>) -> io::Result<Self> {
        Self::with_limits(shares, Limits::default())
    }

    pub fn with_limits(shares: BTreeMap<String, Share>, limits: Limits) -> io::Result<Self> {
        if shares.len() > 32 {
            return Err(invalid("too many shares"));
        }
        for name in shares.keys() {
            RelativePath::new(vec![name.clone()])?;
        }
        Ok(Self {
            shares,
            limits,
            ..Self::default()
        })
    }

    /// Only sanitized stable codes leave the service; OS errors and host
    /// paths are never returned to the guest.
    pub fn dispatch(&mut self, permit: &Permit, request: FileRequest) -> FileReply {
        self.reap();
        self.execute(permit, request)
            .unwrap_or_else(|e| FileReply::Error {
                code: match e.kind() {
                    io::ErrorKind::PermissionDenied => "permission_denied",
                    io::ErrorKind::NotFound => "not_found",
                    io::ErrorKind::AlreadyExists => "already_exists",
                    io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => "invalid_request",
                    _ => "file_error",
                }
                .into(),
            })
    }

    pub fn take_records(&mut self) -> Vec<FileRecord> {
        self.records.drain(..).collect()
    }

    /// Called on revoke, disconnect, expiry, session transition and stop.
    pub fn cancel_all(&mut self) {
        let transfers = std::mem::take(&mut self.transfers);
        for (_, t) in transfers {
            self.record(FileRecord {
                operation: "transfer",
                share: t.share.clone(),
                path: t.path.clone(),
                bytes: t.written,
                outcome: "cancelled",
            });
        }
    }

    pub fn reap(&mut self) {
        let expired: Vec<_> = self
            .transfers
            .iter()
            .filter(|(_, t)| {
                t.last.elapsed() >= TRANSFER_IDLE
                    || t.permit
                        .with(Feature::ShareWrite, Some((&t.share, true)), || Ok(()))
                        .is_err()
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(t) = self.transfers.remove(&id) {
                self.record(FileRecord {
                    operation: "transfer",
                    share: t.share.clone(),
                    path: t.path.clone(),
                    bytes: t.written,
                    outcome: "cancelled",
                });
            }
        }
    }

    fn record(&mut self, record: FileRecord) {
        if self.records.len() == MAX_RECORDS {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    fn share(&self, name: &str, write: bool) -> io::Result<&Share> {
        self.shares
            .get(name)
            .filter(|s| !write || s.writable)
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "share unavailable"))
    }

    fn execute(&mut self, permit: &Permit, request: FileRequest) -> io::Result<FileReply> {
        // Staging files belong to the host until publication: removing,
        // replacing or renaming one would publish unverified content or
        // block its cleanup and journal recovery.
        let paths: &[&RelativePath] = match &request {
            FileRequest::List { path, .. }
            | FileRequest::Read { path, .. }
            | FileRequest::Begin { path, .. }
            | FileRequest::Mkdir { path, .. }
            | FileRequest::Remove { path, .. } => &[path],
            FileRequest::Rename { from, to, .. } => &[from, to],
            FileRequest::Write { .. } | FileRequest::Commit { .. } | FileRequest::Cancel { .. } => {
                &[]
            }
        };
        if paths
            .iter()
            .any(|p| p.components().iter().any(|c| reserved_name(c)))
        {
            return Err(invalid("reserved path component"));
        }
        match request {
            FileRequest::List { share, path } => {
                let s = self.share(&share, false)?;
                let mut entries = permit.with(Feature::ShareRead, Some((&share, false)), || {
                    s.root.descend(&path)?.entries()
                })?;
                fit_frame(&mut entries)?;
                Ok(FileReply::Entries { entries })
            }
            FileRequest::Read {
                share,
                path,
                offset,
                length,
            } => {
                if length == 0 || length as usize > MAX_CHUNK {
                    return Err(invalid("read out of bounds"));
                }
                let s = self.share(&share, false)?;
                let remaining = self.limits.total_bytes.saturating_sub(self.bytes);
                let (data, eof) = permit.with(Feature::ShareRead, Some((&share, false)), || {
                    let (parent, name) = s.root.parent(&path)?;
                    let mut f = parent.read_file(&name)?;
                    let size = f.metadata()?.len();
                    if size > self.limits.file_bytes || offset > size {
                        return Err(invalid("file out of bounds"));
                    }
                    if remaining == 0 && offset < size {
                        return Err(invalid("byte budget exhausted"));
                    }
                    f.seek(SeekFrom::Start(offset))?;
                    let mut b = vec![0; (length as u64).min(remaining) as usize];
                    let n = f.read(&mut b)?;
                    b.truncate(n);
                    Ok((protocol::encode(&b), offset + n as u64 >= size))
                })?;
                self.bytes += (data.len() / 2) as u64;
                Ok(FileReply::Data { offset, data, eof })
            }
            FileRequest::Begin {
                replace,
                share,
                path,
                size,
                sha256,
            } => {
                if size > self.limits.file_bytes
                    || size > self.limits.total_bytes.saturating_sub(self.bytes)
                    || self.begun >= self.limits.files
                    || self.transfers.len() >= MAX_TRANSFERS
                {
                    return Err(invalid("transfer limit"));
                }
                let expected = protocol::decode(&sha256, 32)?;
                if expected.len() != 32 {
                    return Err(invalid("SHA-256 required"));
                }
                let s = self.share(&share, true)?;
                let id = Secret::generate()?.export();
                let staging = format!("{STAGING_PREFIX}{id}");
                let (parent, name, file, journal) =
                    permit.with(Feature::ShareWrite, Some((&share, true)), || {
                        let (parent, name) = s.root.parent(&path)?;
                        // Publication is create-only: an existing destination is
                        // never truncated, even when it is a symlink.
                        let journal = self
                            .journal
                            .as_ref()
                            .map(|j| j.prepare(&s.path, &path, &id))
                            .transpose()?;
                        let file = parent.create_file(&staging)?;
                        Ok((parent, name, file, journal))
                    })?;
                self.transfers.insert(
                    id.clone(),
                    Transfer {
                        journal,
                        replace,
                        parent,
                        staging,
                        name,
                        file: Some(file),
                        share,
                        path,
                        permit: permit.clone(),
                        size,
                        written: 0,
                        expected,
                        hash: Sha256::new(),
                        last: Instant::now(),
                    },
                );
                self.begun += 1;
                Ok(FileReply::Transfer { transfer: id })
            }
            FileRequest::Write {
                transfer,
                offset,
                data,
            } => {
                let b = protocol::decode(&data, MAX_CHUNK)?;
                if b.is_empty() {
                    return Err(invalid("empty chunk"));
                }
                if b.len() as u64 > self.limits.total_bytes.saturating_sub(self.bytes) {
                    self.transfers.remove(&transfer);
                    return Err(invalid("byte budget exhausted"));
                }
                let t = self
                    .transfers
                    .get_mut(&transfer)
                    .ok_or_else(|| invalid("unknown transfer"))?;
                // Check the request's permit too; a transfer id is never an
                // authorization token for another guest connection.
                permit.with(Feature::ShareWrite, Some((&t.share, true)), || Ok(()))?;
                let result = t
                    .permit
                    .with(Feature::ShareWrite, Some((&t.share, true)), || {
                        if offset != t.written || b.len() as u64 > t.size.saturating_sub(t.written)
                        {
                            return Err(invalid("non-contiguous chunk"));
                        }
                        t.file
                            .as_mut()
                            .ok_or_else(|| invalid("closed transfer"))?
                            .write_all(&b)
                    });
                if let Err(e) = result {
                    self.transfers.remove(&transfer);
                    return Err(e);
                }
                t.hash.update(&b);
                t.written += b.len() as u64;
                self.bytes += b.len() as u64;
                t.last = Instant::now();
                Ok(FileReply::Ok)
            }
            FileRequest::Commit { transfer } => {
                let mut t = self
                    .transfers
                    .remove(&transfer)
                    .ok_or_else(|| invalid("unknown transfer"))?;
                permit.with(Feature::ShareWrite, Some((&t.share, true)), || Ok(()))?;
                let result = t
                    .permit
                    .with(Feature::ShareWrite, Some((&t.share, true)), || {
                        if t.written != t.size || t.hash.clone().finalize().as_slice() != t.expected
                        {
                            return Err(invalid("transfer length or hash mismatch"));
                        }
                        t.file
                            .as_ref()
                            .ok_or_else(|| invalid("closed transfer"))?
                            .sync_all()?;
                        if t.permit.revoked() {
                            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                        }
                        t.file.take();
                        if t.replace {
                            t.parent.rename_to(&t.staging, &t.parent, &t.name, true)
                        } else {
                            t.parent.publish(&t.staging, &t.name)
                        }
                    });
                self.record(FileRecord {
                    operation: "transfer",
                    share: t.share.clone(),
                    path: t.path.clone(),
                    bytes: t.written,
                    outcome: if result.is_ok() {
                        "completed"
                    } else {
                        "failed"
                    },
                });
                result.map(|()| FileReply::Ok)
            }
            FileRequest::Cancel { transfer } => {
                let t = self
                    .transfers
                    .get(&transfer)
                    .ok_or_else(|| invalid("unknown transfer"))?;
                permit.with(Feature::ShareWrite, Some((&t.share, true)), || Ok(()))?;
                let t = self
                    .transfers
                    .remove(&transfer)
                    .expect("transfer checked above");
                self.record(FileRecord {
                    operation: "transfer",
                    share: t.share.clone(),
                    path: t.path.clone(),
                    bytes: t.written,
                    outcome: "cancelled",
                });
                Ok(FileReply::Ok)
            }
            FileRequest::Mkdir { share, path } => {
                if self.begun >= self.limits.files {
                    return Err(invalid("file count limit"));
                }
                let s = self.share(&share, true)?;
                permit.with(Feature::ShareWrite, Some((&share, true)), || {
                    let (parent, name) = s.root.parent(&path)?;
                    parent.mkdir(&name)
                })?;
                self.begun += 1;
                self.record(FileRecord {
                    operation: "mkdir",
                    share,
                    path,
                    bytes: 0,
                    outcome: "completed",
                });
                Ok(FileReply::Ok)
            }
            FileRequest::Remove { share, path } => {
                let s = self.share(&share, true)?;
                permit.with(Feature::ShareWrite, Some((&share, true)), || {
                    let (parent, name) = s.root.parent(&path)?;
                    parent.remove_entry(&name)
                })?;
                self.record(FileRecord {
                    operation: "remove",
                    share,
                    path,
                    bytes: 0,
                    outcome: "completed",
                });
                Ok(FileReply::Ok)
            }
            FileRequest::Rename {
                share,
                from,
                to,
                replace,
            } => {
                let s = self.share(&share, true)?;
                permit.with(Feature::ShareWrite, Some((&share, true)), || {
                    let (parent, name) = s.root.parent(&from)?;
                    let (destination, target) = s.root.parent(&to)?;
                    parent.rename_to(&name, &destination, &target, replace)
                })?;
                self.record(FileRecord {
                    operation: "rename",
                    share,
                    path: to,
                    bytes: 0,
                    outcome: "completed",
                });
                Ok(FileReply::Ok)
            }
        }
    }
}

/// A listing of many long names can exceed one frame. The sorted tail is
/// omitted so the reply always fits, instead of failing the write and
/// closing the channel (which would cancel every open transfer).
fn fit_frame(entries: &mut Vec<protocol::Entry>) -> io::Result<()> {
    fn size<T: serde::Serialize>(value: &T) -> io::Result<usize> {
        serde_json::to_vec(value)
            .map(|b| b.len())
            .map_err(|_| invalid("listing serialization failed"))
    }
    let mut used = size(&FileReply::Entries {
        entries: Vec::new(),
    })?;
    for (i, entry) in entries.iter().enumerate() {
        // One separating comma per entry after the first.
        used += size(entry)? + usize::from(i > 0);
        if used > protocol::MAX_FRAME {
            entries.truncate(i);
            break;
        }
    }
    Ok(())
}
