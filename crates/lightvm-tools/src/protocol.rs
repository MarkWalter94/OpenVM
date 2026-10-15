//! Versioned, length-prefixed JSON frames. Files use bounded hexadecimal
//! chunks, not a JSON array of integers or a whole-file allocation.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Wire protocol version. Unsupported versions are refused at handshake.
pub const VERSION: u32 = 1;
/// Maximum binary chunk (64 KiB).
pub const MAX_CHUNK: usize = 64 * 1024;
/// Maximum JSON payload, checked before allocation.
pub const MAX_FRAME: usize = 256 * 1024;
/// Maximum UTF-8 clipboard text (1 MiB), transferred in chunks if needed.
pub const MAX_CLIPBOARD: usize = 1024 * 1024;
/// Maximum entries returned by one directory listing.
pub const MAX_ENTRIES: usize = 1024;

/// First message on a new connection. Debug output redacts the credential.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub version: u32,
    pub vm: String,
    pub run: String,
    pub credential: String,
}

impl std::fmt::Debug for Hello {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hello")
            .field("version", &self.version)
            .field("vm", &self.vm)
            .field("run", &self.run)
            .field("credential", &"<redacted>")
            .finish()
    }
}

impl Drop for Hello {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.credential).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

pub use lightvm_proto::{
    ToolFileEntry as Entry, ToolFileReply as FileReply, ToolFileRequest as FileRequest,
    ToolRelativePath as RelativePath,
};

/// Reads one frame, returning `None` only for clean EOF between frames.
pub fn read<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len = [0; 4];
    loop {
        match r.read(&mut len[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    r.read_exact(&mut len[1..])?;
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(invalid("frame out of bounds"));
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b)?;
    serde_json::from_slice(&b)
        .map(Some)
        .map_err(|_| invalid("malformed frame"))
}

pub fn write<W: Write, T: Serialize>(w: &mut W, value: &T) -> io::Result<()> {
    // The bounded writer also prevents an oversized locally constructed
    // message from allocating an unbounded temporary serialization buffer.
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if b.len() > MAX_FRAME.saturating_sub(self.0.len()) {
                return Err(invalid("frame out of bounds"));
            }
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, value).map_err(|_| invalid("frame out of bounds"))?;
    w.write_all(&(out.0.len() as u32).to_be_bytes())?;
    w.write_all(&out.0)?;
    w.flush()
}

pub fn encode(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

pub fn decode(s: &str, max: usize) -> io::Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || s.len() / 2 > max {
        return Err(invalid("hex out of bounds"));
    }
    fn digit(b: u8) -> io::Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(invalid("invalid hex")),
        }
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Ok(digit(p[0])? * 16 + digit(p[1])?))
        .collect()
}

pub(crate) fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
