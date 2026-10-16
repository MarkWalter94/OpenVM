//! Read-only view of the internal snapshot table of a QCOW2 image
//! (ADR-0032), without `qemu-img`: the storage does not start processes,
//! yet reconciliation must decide from the disk whether a snapshot
//! operation happened.
//!
//! Read: the header (version 2 or 3), the snapshot table (identifier, name,
//! L1 table of each snapshot) and the active L1 table. Every offset and
//! size is bounded before it is used; the file is never written. Format:
//! QEMU `docs/interop/qcow2.txt`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{Result, StorageError};

/// `QFI\xfb`.
const MAGIC: [u8; 4] = [0x51, 0x46, 0x49, 0xfb];
/// QEMU's own limit on the number of internal snapshots.
const MAX_SNAPSHOTS: u32 = 65_536;
/// QEMU's own limit on an L1 table (`QCOW_MAX_L1_SIZE`, 32 MiB).
const MAX_L1_BYTES: u64 = 32 << 20;
/// Longest snapshot table read (QEMU limits it to 64 MiB).
const MAX_TABLE_BYTES: u64 = 64 << 20;
/// Bits 9..55 of an L1 entry: offset of the L2 table (the other bits are
/// flags, such as `COPIED`, that change with the reference count).
const L1_OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;

/// Internal snapshot found in the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qcow2Snapshot {
    /// Numeric identifier assigned by QEMU.
    pub id: String,
    /// Name (LightVM writes the snapshot identifier here).
    pub name: String,
    /// Offset of the snapshot's L1 table.
    pub l1_offset: u64,
    /// Entries of the snapshot's L1 table.
    pub l1_size: u32,
    /// Virtual disk size recorded with the snapshot (extra data), if any.
    pub disk_size: Option<u64>,
}

/// Snapshot table and active L1 table of an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qcow2Info {
    /// Virtual size.
    pub size: u64,
    /// Offset of the active L1 table.
    pub l1_offset: u64,
    /// Entries of the active L1 table.
    pub l1_size: u32,
    /// Internal snapshots, in table order.
    pub snapshots: Vec<Qcow2Snapshot>,
}

impl Qcow2Info {
    /// Snapshot whose name is `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Qcow2Snapshot> {
        self.snapshots.iter().find(|s| s.name == name)
    }
}

fn bad(path: &Path, detail: impl Into<String>) -> StorageError {
    StorageError::IntegrityMismatch {
        path: path.display().to_string(),
        detail: detail.into(),
    }
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_be_bytes(a)
    })
}

fn read_at(f: &mut File, path: &Path, offset: u64, len: u64, file_len: u64) -> Result<Vec<u8>> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| bad(path, "offset out of range"))?;
    if end > file_len {
        return Err(bad(path, "table beyond the end of the file"));
    }
    let len = usize::try_from(len).map_err(|_| bad(path, "table too large"))?;
    let mut buf = vec![0u8; len];
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| StorageError::io(path, e))?;
    f.read_exact(&mut buf)
        .map_err(|e| StorageError::io(path, e))?;
    Ok(buf)
}

/// Reads the header and the snapshot table of the QCOW2 image at `path`.
/// Not a QCOW2 image, an encrypted one, or a table out of bounds:
/// [`StorageError::IntegrityMismatch`].
pub fn read_info(path: &Path) -> Result<Qcow2Info> {
    let mut f = File::open(path).map_err(|e| StorageError::io(path, e))?;
    let file_len = f.metadata().map_err(|e| StorageError::io(path, e))?.len();
    let head = read_at(&mut f, path, 0, 104.min(file_len), file_len)?;
    if head.get(0..4) != Some(&MAGIC[..]) {
        return Err(bad(path, "not a QCOW2 image"));
    }
    let field32 = |o| be32(&head, o).ok_or_else(|| bad(path, "truncated header"));
    let field64 = |o| be64(&head, o).ok_or_else(|| bad(path, "truncated header"));
    let version = field32(4)?;
    if !(2..=3).contains(&version) {
        return Err(bad(path, format!("unsupported QCOW2 version {version}")));
    }
    let size = field64(24)?;
    let l1_size = field32(36)?;
    let l1_offset = field64(40)?;
    let nb = field32(60)?;
    let table = field64(64)?;
    if u64::from(l1_size) * 8 > MAX_L1_BYTES {
        return Err(bad(path, "L1 table too large"));
    }
    if nb > MAX_SNAPSHOTS {
        return Err(bad(path, "too many snapshots"));
    }
    let mut snapshots = Vec::new();
    let mut pos = table;
    for _ in 0..nb {
        if pos.saturating_sub(table) > MAX_TABLE_BYTES {
            return Err(bad(path, "snapshot table too large"));
        }
        let fixed = read_at(&mut f, path, pos, 40, file_len)?;
        let e32 = |o: usize| be32(&fixed, o).ok_or_else(|| bad(path, "truncated table"));
        let e16 = |o: usize| be16(&fixed, o).ok_or_else(|| bad(path, "truncated table"));
        let sl1_offset = be64(&fixed, 0).ok_or_else(|| bad(path, "truncated table"))?;
        let sl1_size = e32(8)?;
        let id_len = u64::from(e16(12)?);
        let name_len = u64::from(e16(14)?);
        let extra_len = u64::from(e32(36)?);
        if extra_len > 1024 {
            return Err(bad(path, "extra data too large"));
        }
        let rest_len = extra_len + id_len + name_len;
        let rest = read_at(&mut f, path, pos + 40, rest_len, file_len)?;
        let extra = usize::try_from(extra_len).map_err(|_| bad(path, "extra data too large"))?;
        let id_end = extra + usize::from(e16(12)?);
        let disk_size = if extra >= 16 { be64(&rest, 8) } else { None };
        let text = |a: usize, b: usize| {
            rest.get(a..b)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .ok_or_else(|| bad(path, "truncated table"))
        };
        snapshots.push(Qcow2Snapshot {
            id: text(extra, id_end)?,
            name: text(id_end, rest.len())?,
            l1_offset: sl1_offset,
            l1_size: sl1_size,
            disk_size,
        });
        pos = (pos + 40 + rest_len).div_ceil(8) * 8;
    }
    Ok(Qcow2Info {
        size,
        l1_offset,
        l1_size,
        snapshots,
    })
}

fn l1_entries(
    f: &mut File,
    path: &Path,
    offset: u64,
    size: u32,
    file_len: u64,
) -> Result<Vec<u64>> {
    let bytes = u64::from(size) * 8;
    if bytes > MAX_L1_BYTES {
        return Err(bad(path, "L1 table too large"));
    }
    if size == 0 {
        return Ok(Vec::new());
    }
    let buf = read_at(f, path, offset, bytes, file_len)?;
    Ok(buf
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| be64(c, 0).unwrap_or(0) & L1_OFFSET_MASK)
        .collect())
}

/// `true` if the active L1 table of the image points to exactly the same
/// L2 tables as snapshot `name` (missing entries count as unallocated):
/// the disk's current content is the snapshot's. This is the state right
/// after `qemu-img snapshot -a` and until the guest writes; QEMU writes the
/// active L1 table in one write, so an interrupted revert leaves either
/// the previous table or the snapshot's. `None` if the snapshot is not in
/// the table.
pub fn active_matches(path: &Path, name: &str) -> Result<Option<bool>> {
    let info = read_info(path)?;
    let Some(s) = info.find(name) else {
        return Ok(None);
    };
    let mut f = File::open(path).map_err(|e| StorageError::io(path, e))?;
    let file_len = f.metadata().map_err(|e| StorageError::io(path, e))?.len();
    if let Some(sz) = s.disk_size
        && sz != info.size
    {
        return Ok(Some(false));
    }
    let active = l1_entries(&mut f, path, info.l1_offset, info.l1_size, file_len)?;
    let snap = l1_entries(&mut f, path, s.l1_offset, s.l1_size, file_len)?;
    let n = active.len().max(snap.len());
    let same =
        (0..n).all(|i| active.get(i).copied().unwrap_or(0) == snap.get(i).copied().unwrap_or(0));
    Ok(Some(same))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal QCOW2 v3 image: header, active L1 table at 0x10000, snapshot
    /// table at 0x20000 with the given snapshots (name, L1 entries).
    pub(crate) fn image(active: &[u64], snaps: &[(&str, &[u64])], size: u64) -> Vec<u8> {
        let mut img = vec![0u8; 0x40000];
        img[0..4].copy_from_slice(&MAGIC);
        img[4..8].copy_from_slice(&3u32.to_be_bytes());
        img[20..24].copy_from_slice(&16u32.to_be_bytes());
        img[24..32].copy_from_slice(&size.to_be_bytes());
        img[36..40].copy_from_slice(&(active.len() as u32).to_be_bytes());
        img[40..48].copy_from_slice(&0x10000u64.to_be_bytes());
        img[60..64].copy_from_slice(&(snaps.len() as u32).to_be_bytes());
        img[64..72].copy_from_slice(&0x20000u64.to_be_bytes());
        img[100..104].copy_from_slice(&104u32.to_be_bytes());
        for (i, e) in active.iter().enumerate() {
            let o = 0x10000 + i * 8;
            img[o..o + 8].copy_from_slice(&e.to_be_bytes());
        }
        let mut pos = 0x20000usize;
        for (k, (name, l1)) in snaps.iter().enumerate() {
            let l1_off = 0x30000 + k * 0x1000;
            for (i, e) in l1.iter().enumerate() {
                let o = l1_off + i * 8;
                img[o..o + 8].copy_from_slice(&e.to_be_bytes());
            }
            let id = format!("{}", k + 1);
            img[pos..pos + 8].copy_from_slice(&(l1_off as u64).to_be_bytes());
            img[pos + 8..pos + 12].copy_from_slice(&(l1.len() as u32).to_be_bytes());
            img[pos + 12..pos + 14].copy_from_slice(&(id.len() as u16).to_be_bytes());
            img[pos + 14..pos + 16].copy_from_slice(&(name.len() as u16).to_be_bytes());
            img[pos + 36..pos + 40].copy_from_slice(&16u32.to_be_bytes());
            img[pos + 48..pos + 56].copy_from_slice(&size.to_be_bytes());
            let id_start = pos + 40 + 16;
            img[id_start..id_start + id.len()].copy_from_slice(id.as_bytes());
            let name_start = id_start + id.len();
            img[name_start..name_start + name.len()].copy_from_slice(name.as_bytes());
            pos = (name_start + name.len()).div_ceil(8) * 8;
        }
        img
    }

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lightvm-q2-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("d.qcow2")
    }

    #[test]
    fn snapshot_table_and_active_l1_are_read() {
        let p = tmp();
        let copied = 1u64 << 63;
        std::fs::write(
            &p,
            image(
                &[0x50000 | copied, 0],
                &[("a", &[0x50000, 0]), ("b", &[0x60000])],
                1 << 30,
            ),
        )
        .unwrap();
        let info = read_info(&p).unwrap();
        assert_eq!(info.snapshots.len(), 2);
        assert_eq!(info.find("b").unwrap().id, "2");
        assert_eq!(info.find("a").unwrap().disk_size, Some(1 << 30));
        // The COPIED flag does not count; a missing entry is unallocated.
        assert_eq!(active_matches(&p, "a").unwrap(), Some(true));
        assert_eq!(active_matches(&p, "b").unwrap(), Some(false));
        assert_eq!(active_matches(&p, "c").unwrap(), None);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn hostile_tables_are_rejected_without_panic() {
        let p = tmp();
        let mut img = image(&[0], &[("a", &[0])], 1 << 20);
        // Snapshot count beyond the limit.
        img[60..64].copy_from_slice(&(MAX_SNAPSHOTS + 1).to_be_bytes());
        std::fs::write(&p, &img).unwrap();
        assert!(read_info(&p).is_err());
        // Table offset beyond the file.
        let mut img = image(&[0], &[("a", &[0])], 1 << 20);
        img[64..72].copy_from_slice(&u64::MAX.to_be_bytes());
        std::fs::write(&p, &img).unwrap();
        assert!(read_info(&p).is_err());
        // Name running past the end of the table.
        let mut img = image(&[0], &[("a", &[0])], 1 << 20);
        img[0x20000 + 14..0x20000 + 16].copy_from_slice(&u16::MAX.to_be_bytes());
        let len = img.len();
        img.truncate(len.min(0x20000 + 200));
        std::fs::write(&p, &img).unwrap();
        assert!(read_info(&p).is_err());
        std::fs::write(&p, b"not an image").unwrap();
        assert!(read_info(&p).is_err());
        // Random bytes never panic.
        let mut seed = 0x1234_5678_u64;
        for _ in 0..200 {
            let mut img = image(&[0x50000], &[("x", &[0x50000])], 1 << 20);
            for _ in 0..16 {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let at = (seed >> 33) as usize % 0x20100;
                img[at] = (seed >> 13) as u8;
            }
            std::fs::write(&p, &img).unwrap();
            let _ = read_info(&p);
            let _ = active_matches(&p, "x");
        }
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
