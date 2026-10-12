//! Bounded random access to the image and file extent maps shared by the
//! ISO 9660 and UDF readers.

use std::io::{Read, Seek, SeekFrom};

use crate::DetectError;

/// Upper bound on the bytes read from the image by one detection.
pub(crate) const MAX_TOTAL_READ: u64 = 64 * 1024 * 1024;
/// Upper bound on the directory entries visited by one detection.
pub(crate) const MAX_DIR_ENTRIES: u32 = 10_000;
/// Upper bound on the bytes of one directory.
pub(crate) const MAX_DIR_BYTES: u64 = 4 * 1024 * 1024;
/// Upper bound on the extents of one file (UDF allocation descriptors,
/// ISO 9660 multi-extent records).
pub(crate) const MAX_EXTENTS: usize = 4096;

/// Where the bytes of one extent of a file come from.
#[derive(Clone, Debug)]
pub(crate) enum Piece {
    /// Byte offset on the image.
    Disk(u64),
    /// Allocated but not recorded: reads as zeros.
    Zero,
    /// Data embedded in the file entry (UDF ICB flag 3).
    Inline(Vec<u8>),
}

/// One contiguous run of file bytes.
#[derive(Clone, Debug)]
pub(crate) struct Extent {
    pub(crate) len: u64,
    pub(crate) piece: Piece,
}

/// Size and extent map of a file or directory.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileData {
    pub(crate) size: u64,
    pub(crate) extents: Vec<Extent>,
}

impl FileData {
    /// Appends an extent, enforcing [`MAX_EXTENTS`].
    pub(crate) fn push(&mut self, len: u64, piece: Piece) -> Result<(), DetectError> {
        if self.extents.len() >= MAX_EXTENTS {
            return Err(DetectError::LimitExceeded("extents per file"));
        }
        if len > 0 {
            self.extents.push(Extent { len, piece });
        }
        Ok(())
    }

    /// Reads at most `max` bytes starting at file offset `off`. Bytes past
    /// the recorded extents (a malformed map) end the read early.
    pub(crate) fn read<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        off: u64,
        max: u64,
    ) -> Result<Vec<u8>, DetectError> {
        let want = self.size.saturating_sub(off).min(max);
        let want = usize::try_from(want).map_err(|_| DetectError::LimitExceeded("read size"))?;
        let mut out = Vec::with_capacity(want);
        let end = off.saturating_add(want as u64);
        let mut pos = 0u64;
        for ext in &self.extents {
            if out.len() >= want {
                break;
            }
            let ext_end = pos.saturating_add(ext.len);
            let cur = off.saturating_add(out.len() as u64);
            if ext_end > cur {
                let skip = cur.saturating_sub(pos);
                let take = ext_end.min(end) - cur;
                let take_usize =
                    usize::try_from(take).map_err(|_| DetectError::LimitExceeded("read size"))?;
                match &ext.piece {
                    Piece::Disk(base) => {
                        let at = base
                            .checked_add(skip)
                            .ok_or(DetectError::Malformed("extent offset overflow"))?;
                        let start = out.len();
                        out.resize(start + take_usize, 0);
                        disk.read_at(at, &mut out[start..])?;
                    }
                    Piece::Zero => out.resize(out.len() + take_usize, 0),
                    Piece::Inline(bytes) => {
                        let s = usize::try_from(skip).unwrap_or(usize::MAX);
                        let slice = bytes.get(s..).unwrap_or(&[]);
                        let n = slice.len().min(take_usize);
                        out.extend_from_slice(slice.get(..n).unwrap_or(&[]));
                        if n < take_usize {
                            break;
                        }
                    }
                }
            }
            pos = ext_end;
        }
        Ok(out)
    }
}

/// The image with byte and work budgets.
pub(crate) struct Disk<'a, R> {
    r: &'a mut R,
    size: u64,
    read_left: u64,
    entries_left: u32,
}

impl<R> std::fmt::Debug for Disk<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Disk")
            .field("size", &self.size)
            .field("read_left", &self.read_left)
            .field("entries_left", &self.entries_left)
            .finish_non_exhaustive()
    }
}

impl<'a, R: Read + Seek> Disk<'a, R> {
    pub(crate) fn new(r: &'a mut R) -> Result<Self, DetectError> {
        let size = r.seek(SeekFrom::End(0))?;
        Ok(Self {
            r,
            size,
            read_left: MAX_TOTAL_READ,
            entries_left: MAX_DIR_ENTRIES,
        })
    }

    /// Fills `buf` from image offset `off`.
    pub(crate) fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<(), DetectError> {
        let len = buf.len() as u64;
        let end = off
            .checked_add(len)
            .ok_or(DetectError::Malformed("offset overflow"))?;
        if end > self.size {
            return Err(DetectError::Malformed("read beyond the end of the image"));
        }
        if len > self.read_left {
            return Err(DetectError::LimitExceeded("bytes read"));
        }
        self.read_left -= len;
        self.r.seek(SeekFrom::Start(off))?;
        self.r.read_exact(buf)?;
        Ok(())
    }

    /// Reads `len` bytes at `off`, or `None` when the image is too short.
    pub(crate) fn read_vec_opt(
        &mut self,
        off: u64,
        len: usize,
    ) -> Result<Option<Vec<u8>>, DetectError> {
        if off.checked_add(len as u64).is_none_or(|e| e > self.size) {
            return Ok(None);
        }
        let mut v = vec![0u8; len];
        self.read_at(off, &mut v)?;
        Ok(Some(v))
    }

    /// Counts one visited directory entry against [`MAX_DIR_ENTRIES`].
    pub(crate) fn visit_entry(&mut self) -> Result<(), DetectError> {
        self.entries_left = self
            .entries_left
            .checked_sub(1)
            .ok_or(DetectError::LimitExceeded("directory entries"))?;
        Ok(())
    }
}
