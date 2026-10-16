//! Guest file-channel data types. No filesystem I/O is performed here.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io;

/// Guest path: a sequence of names, not a host-native path. Empty means
/// the share root (only for listing).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolRelativePath(Vec<String>);

impl ToolRelativePath {
    /// Checks on deserialization are repeated at the filesystem boundary.
    pub fn new(parts: Vec<String>) -> io::Result<Self> {
        let p = Self(parts);
        p.validate()?;
        Ok(p)
    }

    pub fn components(&self) -> &[String] {
        &self.0
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.0.len() > 64 || self.0.iter().map(String::len).sum::<usize>() > 4096 {
            return Err(invalid("path too long"));
        }
        for s in &self.0 {
            let stem = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
            let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'));
            if s.is_empty()
                || s.len() > 255
                || s == "."
                || s == ".."
                || s.ends_with(['.', ' '])
                || s.chars()
                    .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
                || reserved
            {
                return Err(invalid("invalid path component"));
            }
        }
        Ok(())
    }
}

/// Host share operations, authorized by the supervisor before dispatch.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolFileRequest {
    List {
        share: String,
        path: ToolRelativePath,
    },
    Read {
        share: String,
        path: ToolRelativePath,
        offset: u64,
        length: u32,
    },
    Begin {
        #[serde(default)]
        replace: bool,
        share: String,
        path: ToolRelativePath,
        size: u64,
        sha256: String,
    },
    Write {
        transfer: String,
        offset: u64,
        data: String,
    },
    Commit {
        transfer: String,
    },
    Cancel {
        transfer: String,
    },
    Mkdir {
        share: String,
        path: ToolRelativePath,
    },
    Remove {
        share: String,
        path: ToolRelativePath,
    },
    Rename {
        share: String,
        from: ToolRelativePath,
        to: ToolRelativePath,
        replace: bool,
    },
}

impl std::fmt::Debug for ToolFileRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::List { share, path } => f
                .debug_struct("List")
                .field("share", share)
                .field("path", path)
                .finish(),
            Self::Read {
                share,
                path,
                offset,
                length,
            } => f
                .debug_struct("Read")
                .field("share", share)
                .field("path", path)
                .field("offset", offset)
                .field("length", length)
                .finish(),
            Self::Begin {
                share, path, size, ..
            } => f
                .debug_struct("Begin")
                .field("share", share)
                .field("path", path)
                .field("size", size)
                .finish(),
            Self::Write {
                transfer,
                offset,
                data,
            } => f
                .debug_struct("Write")
                .field("transfer", transfer)
                .field("offset", offset)
                .field("bytes", &(data.len() / 2))
                .finish(),
            Self::Commit { transfer } => f.debug_tuple("Commit").field(transfer).finish(),
            Self::Cancel { transfer } => f.debug_tuple("Cancel").field(transfer).finish(),
            Self::Mkdir { share, path } => f.debug_tuple("Mkdir").field(share).field(path).finish(),
            Self::Remove { share, path } => {
                f.debug_tuple("Remove").field(share).field(path).finish()
            }
            Self::Rename {
                share,
                from,
                to,
                replace,
            } => f
                .debug_struct("Rename")
                .field("share", share)
                .field("from", from)
                .field("to", to)
                .field("replace", replace)
                .finish(),
        }
    }
}

/// Directory entry: names and metadata, never host paths.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolFileEntry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolFileReply {
    Entries {
        entries: Vec<ToolFileEntry>,
    },
    Data {
        offset: u64,
        data: String,
        eof: bool,
    },
    Transfer {
        transfer: String,
    },
    Ok,
    Error {
        code: String,
    },
}

impl std::fmt::Debug for ToolFileReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Entries { entries } => f.debug_tuple("Entries").field(entries).finish(),
            Self::Data { offset, data, eof } => f
                .debug_struct("Data")
                .field("offset", offset)
                .field("bytes", &(data.len() / 2))
                .field("eof", eof)
                .finish(),
            Self::Transfer { transfer } => f.debug_tuple("Transfer").field(transfer).finish(),
            Self::Ok => f.write_str("Ok"),
            Self::Error { code } => f.debug_tuple("Error").field(code).finish(),
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl ToolFileRequest {
    /// Clipboard images reuse the bounded chunk envelope, never host/guest
    /// file paths. Read tokens identify an immutable per-connection capture.
    pub fn validate_clipboard_image(&self) -> io::Result<()> {
        const MAX: u64 = 4 * 1024 * 1024 * 4 + 12;
        let image = |path: &ToolRelativePath| path.components() == ["image"];
        let mut normalized = self.clone();
        match &mut normalized {
            Self::Begin {
                share,
                path,
                size,
                sha256,
                ..
            } => {
                if !image(path)
                    || *size > MAX
                    || !matches!(share.as_str(), "clipboard-read" | "clipboard-write")
                    || (share == "clipboard-read" && (*size != 0 || sha256 != &"0".repeat(64)))
                    || (share == "clipboard-write" && *size < 12)
                {
                    return Err(invalid("invalid clipboard image request"));
                }
                *share = "inbox".into();
            }
            Self::Read {
                share,
                path,
                offset,
                ..
            } => {
                if !image(path)
                    || *offset > MAX
                    || share.is_empty()
                    || share.len() > 64
                    || !share.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(invalid("invalid clipboard capture token"));
                }
                *share = "outbox".into();
            }
            Self::Write { offset, data, .. } => {
                if *offset > MAX || data.len() as u64 / 2 > MAX - *offset {
                    return Err(invalid("clipboard image write exceeds limit"));
                }
            }
            Self::Commit { .. } | Self::Cancel { .. } => (),
            _ => return Err(invalid("not a clipboard image operation")),
        }
        normalized.validate_guest()
    }

    /// Inbox/outbox RPC is narrower than the guest's named host shares.
    pub fn validate_guest(&self) -> io::Result<()> {
        let root = |share: &str, path: &ToolRelativePath, write: bool| -> io::Result<()> {
            path.validate()?;
            if (write && share != "inbox") || (!write && share != "outbox") {
                return Err(invalid("guest file root denied"));
            }
            Ok(())
        };
        let token = |id: &str| -> io::Result<()> {
            if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("invalid transfer id"));
            }
            Ok(())
        };
        match self {
            Self::List { share, path } => root(share, path, false),
            Self::Read {
                share,
                path,
                length,
                ..
            } => {
                root(share, path, false)?;
                if *length == 0 || *length > 16384 || path.components().is_empty() {
                    return Err(invalid("invalid read bounds"));
                }
                Ok(())
            }
            Self::Begin {
                share,
                path,
                size,
                sha256,
                replace,
            } => {
                root(share, path, true)?;
                if *replace
                    || path.components().is_empty()
                    || *size > 4 * 1024 * 1024 * 1024
                    || sha256.len() != 64
                    || !sha256.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(invalid("invalid guest transfer"));
                }
                Ok(())
            }
            Self::Write { transfer, data, .. } => {
                token(transfer)?;
                if data.len() > 32768
                    || data.len() % 2 != 0
                    || !data.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(invalid("invalid chunk"));
                }
                Ok(())
            }
            Self::Commit { transfer } | Self::Cancel { transfer } => token(transfer),
            _ => Err(invalid("guest operation unavailable")),
        }
    }
}
impl ToolFileReply {
    pub fn validate(&self) -> io::Result<()> {
        match self {
            Self::Entries { entries } => {
                if entries.len() > 1024 {
                    return Err(invalid("too many file entries"));
                }
                let mut names = std::collections::BTreeSet::new();
                for entry in entries {
                    ToolRelativePath::new(vec![entry.name.clone()])?;
                    if !names.insert(&entry.name) {
                        return Err(invalid("duplicate entry"));
                    }
                }
            }
            Self::Data { data, .. }
                if data.len() > 32768
                    || data.len() % 2 != 0
                    || !data.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                return Err(invalid("invalid reply chunk"));
            }
            Self::Transfer { transfer }
                if transfer.is_empty()
                    || transfer.len() > 64
                    || !transfer.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                return Err(invalid("invalid transfer id"));
            }
            Self::Error { code }
                if code.is_empty()
                    || code.len() > 64
                    || !code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                return Err(invalid("invalid error code"));
            }
            _ => (),
        }
        Ok(())
    }
}
