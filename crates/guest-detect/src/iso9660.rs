//! Read-only ISO 9660 (ECMA-119) reader: primary volume descriptor (with
//! Rock Ridge `NM` names when present) and Joliet supplementary volume
//! descriptor, directory records, multi-extent files. No interleaving.

use std::io::{Read, Seek};

use crate::DetectError;
use crate::disk::{Disk, FileData, MAX_DIR_BYTES, Piece};
use crate::fs::{DirEntry, Fs};

const SECTOR: u64 = 2048;
const FIRST_VD: u64 = 16;
const MAX_VDS: u64 = 64;

/// One ISO 9660 directory hierarchy (primary or Joliet).
#[derive(Clone, Debug)]
pub(crate) struct IsoFs {
    block: u64,
    joliet: bool,
    root: FileData,
}

/// Reads the volume descriptor set: Joliet hierarchies first, then the
/// primary one. Empty when the image has no `CD001` descriptor.
pub(crate) fn probe<R: Read + Seek>(disk: &mut Disk<'_, R>) -> Result<Vec<IsoFs>, DetectError> {
    let mut joliet = Vec::new();
    let mut primary = Vec::new();
    for i in 0..MAX_VDS {
        let Some(vd) = disk.read_vec_opt((FIRST_VD + i) * SECTOR, SECTOR as usize)? else {
            break;
        };
        if vd.get(1..6) != Some(b"CD001".as_slice()) {
            break;
        }
        let ty = vd.first().copied().unwrap_or(0);
        if ty == 255 {
            break;
        }
        if ty != 1 && ty != 2 {
            continue;
        }
        let is_joliet = ty == 2 && {
            let esc = vd.get(88..120).unwrap_or(&[]);
            [b"%/@", b"%/C", b"%/E"]
                .iter()
                .any(|s| esc.windows(3).any(|w| w == s.as_slice()))
        };
        if ty == 2 && !is_joliet {
            continue;
        }
        let block = u64::from(le16(&vd, 128).unwrap_or(0));
        if !matches!(block, 512 | 1024 | 2048) {
            continue;
        }
        let Some(rec) = vd
            .get(156..190)
            .and_then(|r| parse_record(r, block, is_joliet))
        else {
            continue;
        };
        if !rec.is_dir {
            continue;
        }
        let mut root = FileData {
            size: rec.len,
            extents: Vec::new(),
        };
        root.push(rec.len, Piece::Disk(rec.offset))?;
        let fs = IsoFs {
            block,
            joliet: is_joliet,
            root,
        };
        if is_joliet {
            joliet.push(fs);
        } else {
            primary.push(fs);
        }
    }
    joliet.extend(primary);
    Ok(joliet)
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

#[derive(Debug)]
struct Record {
    name: String,
    is_dir: bool,
    multi: bool,
    special: bool,
    interleaved: bool,
    offset: u64,
    len: u64,
}

fn parse_record(rec: &[u8], block: u64, joliet: bool) -> Option<Record> {
    let lba = u64::from(le32(rec, 2)?);
    let len = u64::from(le32(rec, 10)?);
    let flags = *rec.get(25)?;
    let interleaved = *rec.get(26)? != 0 || *rec.get(27)? != 0;
    let id_len = usize::from(*rec.get(32)?);
    let id = rec.get(33..33 + id_len)?;
    let special = id_len == 1 && matches!(id.first(), Some(0 | 1));
    let name = if special {
        String::new()
    } else if joliet {
        let units = id.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c));
        clean_name(
            &char::decode_utf16(units)
                .map(|c| c.unwrap_or('\u{FFFD}'))
                .collect::<String>(),
        )
    } else {
        let su_start = 33 + id_len + usize::from(id_len.is_multiple_of(2));
        match rec.get(su_start..).and_then(rock_ridge_name) {
            Some(n) => n,
            None => clean_name(&id.iter().map(|&b| char::from(b)).collect::<String>()),
        }
    };
    Some(Record {
        name,
        is_dir: flags & 0x02 != 0,
        multi: flags & 0x80 != 0,
        special,
        interleaved,
        offset: lba.checked_mul(block)?,
        len,
    })
}

/// Strips the `;version` suffix and a trailing dot (`WIN51.;1` -> `WIN51`).
fn clean_name(s: &str) -> String {
    let base = s.split(';').next().unwrap_or("");
    base.strip_suffix('.').unwrap_or(base).to_owned()
}

/// Rock Ridge alternate name (`NM` entries of the system use field).
fn rock_ridge_name(su: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    let mut name: Option<Vec<u8>> = None;
    while let Some(head) = su.get(pos..pos + 4) {
        let len = usize::from(head[2]);
        if len < 4 {
            break;
        }
        let entry = su.get(pos..pos + len)?;
        match &head[..2] {
            b"NM" => {
                let flags = *entry.get(4)?;
                if flags & 0x06 != 0 {
                    return None;
                }
                name.get_or_insert_with(Vec::new)
                    .extend_from_slice(entry.get(5..).unwrap_or(&[]));
                if flags & 0x01 == 0 {
                    break;
                }
            }
            b"ST" => break,
            _ => {}
        }
        pos += len;
    }
    name.filter(|n| !n.is_empty())
        .map(|n| String::from_utf8_lossy(&n).into_owned())
}

impl Fs for IsoFs {
    type Node = FileData;

    fn root(&self) -> FileData {
        self.root.clone()
    }

    fn list<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        dir: &FileData,
    ) -> Result<Vec<DirEntry<FileData>>, DetectError> {
        if dir.size > MAX_DIR_BYTES {
            return Err(DetectError::LimitExceeded("directory size"));
        }
        let data = dir.read(disk, 0, MAX_DIR_BYTES)?;
        let mut out: Vec<DirEntry<FileData>> = Vec::new();
        let mut pending: Option<DirEntry<FileData>> = None;
        let mut pos = 0usize;
        while pos < data.len() {
            let len = usize::from(data[pos]);
            if len == 0 {
                // Records never cross a sector boundary: skip the padding.
                pos = (pos / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            let Some(rec) = data.get(pos..pos + len) else {
                break;
            };
            pos += len;
            disk.visit_entry()?;
            let Some(r) = parse_record(rec, self.block, self.joliet) else {
                continue;
            };
            if r.special || r.interleaved || r.name.is_empty() {
                continue;
            }
            if let Some(p) = pending.as_mut()
                && p.name == r.name
            {
                p.node.size = p.node.size.saturating_add(r.len);
                p.node.push(r.len, Piece::Disk(r.offset))?;
                if !r.multi {
                    out.extend(pending.take());
                }
                continue;
            }
            out.extend(pending.take());
            let mut node = FileData {
                size: r.len,
                extents: Vec::new(),
            };
            node.push(r.len, Piece::Disk(r.offset))?;
            let entry = DirEntry {
                name: r.name,
                is_dir: r.is_dir,
                node,
            };
            if r.multi && !r.is_dir {
                pending = Some(entry);
            } else {
                out.push(entry);
            }
        }
        out.extend(pending);
        Ok(out)
    }

    fn open<R: Read + Seek>(
        &self,
        _disk: &mut Disk<'_, R>,
        node: &FileData,
    ) -> Result<FileData, DetectError> {
        Ok(node.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rock_ridge_names() {
        // SP-like entry, then a name split over two NM entries.
        let su = [
            b"PX\x08\x01\0\0\0\0".as_slice(),
            b"NM\x07\x01\x01.d",
            b"NM\x08\x01\x00isk",
        ]
        .concat();
        assert_eq!(rock_ridge_name(&su).as_deref(), Some(".disk"));
        assert_eq!(rock_ridge_name(b"NM\x05\x01\x02"), None);
        assert_eq!(rock_ridge_name(b"NM\x02"), None);
        assert_eq!(rock_ridge_name(b""), None);
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("WIN51.;1"), "WIN51");
        assert_eq!(clean_name("SETUP.EXE;1"), "SETUP.EXE");
        assert_eq!(clean_name("I386"), "I386");
    }
}
