//! Read-only UDF (ECMA-167 / OSTA UDF 1.02 to 2.50) reader, enough for the
//! install media written by Microsoft `oscdimg`: anchor at sector 256,
//! volume descriptor sequence, type 1 partition maps and the UDF 2.50
//! metadata partition, file set descriptor, file entries and extended file
//! entries with short, long, extended or embedded allocation descriptors,
//! file identifier descriptors with 8- and 16-bit d-strings.
//!
//! Not handled (clean `Unsupported` error): sparable and virtual partitions
//! (rewritable and incremental media), logical blocks other than 2048 bytes.

use std::io::{Read, Seek};

use crate::DetectError;
use crate::disk::{Disk, FileData, MAX_DIR_BYTES, MAX_EXTENTS, Piece};
use crate::fs::{DirEntry, Fs};

const SECTOR: u64 = 2048;
const AVDP_SECTOR: u64 = 256;
const MAX_VDS_SECTORS: u64 = 64;
const MAX_PARTITIONS: usize = 8;
const MAX_ALLOC_EXTENT_DESCRIPTORS: usize = 256;

const TAG_AVDP: u16 = 2;
const TAG_PD: u16 = 5;
const TAG_LVD: u16 = 6;
const TAG_TERM: u16 = 8;
const TAG_FSD: u16 = 256;
const TAG_FID: u16 = 257;
const TAG_AED: u16 = 258;
const TAG_FE: u16 = 261;
const TAG_EFE: u16 = 266;

/// Address of an ICB: partition reference (index in the partition maps) and
/// logical block number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Icb {
    part: u16,
    lbn: u32,
}

#[derive(Clone, Debug)]
enum Map {
    Physical { number: u16, start: u64, len: u64 },
    Metadata(FileData),
    Unsupported,
}

/// A mounted UDF volume.
#[derive(Clone, Debug)]
pub(crate) struct Udf {
    maps: Vec<Map>,
    root: Icb,
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn malformed(what: &'static str) -> DetectError {
    DetectError::Malformed(what)
}

/// Identifier of a descriptor tag with a valid checksum.
fn tag(b: &[u8]) -> Option<u16> {
    let h = b.get(..16)?;
    let sum = h
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |a, (_, &v)| a.wrapping_add(v));
    (sum == h[4]).then(|| u16::from_le_bytes([h[0], h[1]]))
}

/// Long allocation descriptor at `at`: (length, ICB).
fn long_ad(b: &[u8], at: usize) -> Option<(u32, Icb)> {
    Some((
        le32(b, at)? & 0x3FFF_FFFF,
        Icb {
            lbn: le32(b, at + 4)?,
            part: le16(b, at + 8)?,
        },
    ))
}

/// Decodes an OSTA compressed unicode d-string body.
fn dchars(b: &[u8]) -> Option<String> {
    let (&comp, rest) = b.split_first()?;
    match comp {
        8 | 254 => Some(rest.iter().map(|&c| char::from(c)).collect()),
        16 | 255 => {
            let units = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c));
            Some(
                char::decode_utf16(units)
                    .map(|c| c.unwrap_or('\u{FFFD}'))
                    .collect(),
            )
        }
        _ => None,
    }
}

#[derive(Debug, Default)]
struct Vds {
    partitions: Vec<(u16, u64, u64)>,
    lvd: Option<Vec<u8>>,
}

fn read_vds<R: Read + Seek>(
    disk: &mut Disk<'_, R>,
    len: u32,
    loc: u32,
) -> Result<Vds, DetectError> {
    let mut vds = Vds::default();
    let n = (u64::from(len) / SECTOR).clamp(1, MAX_VDS_SECTORS);
    for i in 0..n {
        let Some(b) = disk.read_vec_opt((u64::from(loc) + i) * SECTOR, SECTOR as usize)? else {
            break;
        };
        match tag(&b) {
            Some(TAG_PD) => {
                if vds.partitions.len() >= MAX_PARTITIONS {
                    return Err(DetectError::LimitExceeded("UDF partitions"));
                }
                let (Some(num), Some(start), Some(plen)) =
                    (le16(&b, 22), le32(&b, 188), le32(&b, 192))
                else {
                    continue;
                };
                vds.partitions
                    .push((num, u64::from(start), u64::from(plen)));
            }
            Some(TAG_LVD) => {
                if vds.lvd.is_none() {
                    vds.lvd = Some(b);
                }
            }
            Some(TAG_TERM) | None => break,
            Some(_) => {}
        }
    }
    Ok(vds)
}

/// Mounts the UDF volume, or `None` when the image has no anchor volume
/// descriptor pointer at sector 256.
pub(crate) fn probe<R: Read + Seek>(disk: &mut Disk<'_, R>) -> Result<Option<Udf>, DetectError> {
    let Some(avdp) = disk.read_vec_opt(AVDP_SECTOR * SECTOR, SECTOR as usize)? else {
        return Ok(None);
    };
    if tag(&avdp) != Some(TAG_AVDP) {
        return Ok(None);
    }
    let main = (le32(&avdp, 16).unwrap_or(0), le32(&avdp, 20).unwrap_or(0));
    let reserve = (le32(&avdp, 24).unwrap_or(0), le32(&avdp, 28).unwrap_or(0));
    let mut vds = read_vds(disk, main.0, main.1)?;
    if vds.lvd.is_none() {
        vds = read_vds(disk, reserve.0, reserve.1)?;
    }
    let lvd = vds
        .lvd
        .take()
        .ok_or_else(|| malformed("UDF logical volume descriptor missing"))?;
    if le32(&lvd, 212) != Some(SECTOR as u32) {
        return Err(DetectError::Unsupported("UDF logical block size"));
    }
    let (_, fsd) = long_ad(&lvd, 248).ok_or_else(|| malformed("UDF file set location"))?;
    let mt_len = le32(&lvd, 264).unwrap_or(0) as usize;
    let n_maps = le32(&lvd, 268).unwrap_or(0) as usize;
    if n_maps > MAX_PARTITIONS {
        return Err(DetectError::LimitExceeded("UDF partition maps"));
    }
    let table = lvd
        .get(440..440usize.saturating_add(mt_len))
        .ok_or_else(|| malformed("UDF partition map table"))?;

    let find_pd = |num: u16| vds.partitions.iter().find(|p| p.0 == num).copied();
    let mut maps = Vec::with_capacity(n_maps);
    let mut metadata = Vec::new();
    let mut p = 0usize;
    for _ in 0..n_maps {
        let ty = *table.get(p).ok_or_else(|| malformed("UDF partition map"))?;
        let l = usize::from(
            *table
                .get(p + 1)
                .ok_or_else(|| malformed("UDF partition map"))?,
        );
        let entry = table
            .get(p..p + l)
            .filter(|_| l >= 6)
            .ok_or_else(|| malformed("UDF partition map"))?;
        p += l;
        match ty {
            1 => {
                let num = le16(entry, 4).unwrap_or(u16::MAX);
                maps.push(match find_pd(num) {
                    Some((number, start, len)) => Map::Physical { number, start, len },
                    None => Map::Unsupported,
                });
            }
            2 if entry
                .get(5..28)
                .is_some_and(|id| id.starts_with(b"*UDF Metadata Partition")) =>
            {
                let (Some(num), Some(file), Some(mirror)) =
                    (le16(entry, 38), le32(entry, 40), le32(entry, 44))
                else {
                    return Err(malformed("UDF metadata partition map"));
                };
                metadata.push((maps.len(), num, file, mirror));
                maps.push(Map::Unsupported);
            }
            _ => maps.push(Map::Unsupported),
        }
    }

    let mut udf = Udf {
        maps,
        root: Icb { part: 0, lbn: 0 },
    };
    for (index, num, file, mirror) in metadata {
        let phys = match udf
            .maps
            .iter()
            .position(|m| matches!(m, Map::Physical { number, .. } if *number == num))
        {
            Some(j) => j,
            None => {
                let (number, start, len) =
                    find_pd(num).ok_or_else(|| malformed("UDF metadata partition"))?;
                udf.maps.push(Map::Physical { number, start, len });
                udf.maps.len() - 1
            }
        };
        let part = u16::try_from(phys).map_err(|_| malformed("UDF partition index"))?;
        let data = match udf.read_icb(disk, Icb { part, lbn: file }) {
            Ok(d) => d,
            Err(_) => udf.read_icb(disk, Icb { part, lbn: mirror })?,
        };
        if let Some(slot) = udf.maps.get_mut(index) {
            *slot = Map::Metadata(data);
        }
    }

    let b = udf.read_block(disk, fsd)?;
    if tag(&b) != Some(TAG_FSD) {
        return Err(malformed("UDF file set descriptor"));
    }
    let (_, root) = long_ad(&b, 400).ok_or_else(|| malformed("UDF root ICB"))?;
    udf.root = root;
    Ok(Some(udf))
}

impl Udf {
    /// Image byte ranges holding `len` bytes from block `lbn` of partition
    /// `part`.
    fn map_extent(&self, part: u16, lbn: u32, len: u64) -> Result<Vec<(u64, u64)>, DetectError> {
        match self.maps.get(usize::from(part)) {
            Some(Map::Physical {
                start, len: plen, ..
            }) => {
                if u64::from(lbn) >= *plen {
                    return Err(malformed("UDF block outside its partition"));
                }
                let at = start
                    .checked_add(u64::from(lbn))
                    .and_then(|s| s.checked_mul(SECTOR))
                    .ok_or_else(|| malformed("UDF block address"))?;
                Ok(vec![(at, len)])
            }
            Some(Map::Metadata(fd)) => {
                let mut want_off = u64::from(lbn) * SECTOR;
                let mut want_len = len;
                let mut out = Vec::new();
                let mut pos = 0u64;
                for ext in &fd.extents {
                    if want_len == 0 {
                        break;
                    }
                    let end = pos.saturating_add(ext.len);
                    if end > want_off {
                        let skip = want_off - pos;
                        let take = (end - want_off).min(want_len);
                        let Piece::Disk(base) = ext.piece else {
                            return Err(malformed("UDF metadata file extent"));
                        };
                        if out.len() >= MAX_EXTENTS {
                            return Err(DetectError::LimitExceeded("extents per file"));
                        }
                        out.push((base.saturating_add(skip), take));
                        want_off += take;
                        want_len -= take;
                    }
                    pos = end;
                }
                if want_len != 0 {
                    return Err(malformed("UDF block outside the metadata file"));
                }
                Ok(out)
            }
            Some(Map::Unsupported) => Err(DetectError::Unsupported("UDF partition type")),
            None => Err(malformed("UDF partition reference")),
        }
    }

    fn read_block<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        icb: Icb,
    ) -> Result<Vec<u8>, DetectError> {
        let mut out = Vec::with_capacity(SECTOR as usize);
        for (at, len) in self.map_extent(icb.part, icb.lbn, SECTOR)? {
            let start = out.len();
            let len = usize::try_from(len).map_err(|_| malformed("UDF extent"))?;
            out.resize(start + len, 0);
            disk.read_at(at, &mut out[start..])?;
        }
        Ok(out)
    }

    /// Reads a file entry or extended file entry and builds its extent map.
    fn read_icb<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        icb: Icb,
    ) -> Result<FileData, DetectError> {
        let b = self.read_block(disk, icb)?;
        let (l_ea_at, base) = match tag(&b) {
            Some(TAG_FE) => (168, 176),
            Some(TAG_EFE) => (208, 216),
            _ => return Err(malformed("UDF file entry")),
        };
        let size = le64(&b, 56).ok_or_else(|| malformed("UDF file entry"))?;
        let flags = le16(&b, 34).ok_or_else(|| malformed("UDF file entry"))?;
        let l_ea = le32(&b, l_ea_at).ok_or_else(|| malformed("UDF file entry"))? as usize;
        let l_ad = le32(&b, l_ea_at + 4).ok_or_else(|| malformed("UDF file entry"))? as usize;
        let ad_start = base + l_ea.min(SECTOR as usize);
        let ads = b
            .get(ad_start..ad_start.saturating_add(l_ad))
            .ok_or_else(|| malformed("UDF allocation descriptors"))?;
        let mut fd = FileData {
            size,
            extents: Vec::new(),
        };
        match flags & 7 {
            3 => {
                let n = usize::try_from(size).unwrap_or(usize::MAX).min(ads.len());
                fd.push(
                    n as u64,
                    Piece::Inline(ads.get(..n).unwrap_or(&[]).to_vec()),
                )?;
            }
            ad_type @ 0..=2 => self.parse_ads(disk, ads.to_vec(), ad_type, icb.part, &mut fd)?,
            _ => return Err(DetectError::Unsupported("UDF allocation descriptor type")),
        }
        Ok(fd)
    }

    fn parse_ads<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        mut cur: Vec<u8>,
        ad_type: u16,
        icb_part: u16,
        fd: &mut FileData,
    ) -> Result<(), DetectError> {
        let size = match ad_type {
            0 => 8,
            1 => 16,
            _ => 20,
        };
        let mut continuations = 0usize;
        loop {
            let mut next = None;
            for ad in cur.chunks_exact(size) {
                let raw = le32(ad, 0).unwrap_or(0);
                let len = raw & 0x3FFF_FFFF;
                if len == 0 {
                    break;
                }
                let loc = match ad_type {
                    0 => le32(ad, 4).map(|lbn| Icb {
                        part: icb_part,
                        lbn,
                    }),
                    1 => long_ad(ad, 0).map(|(_, i)| i),
                    _ => le32(ad, 12)
                        .zip(le16(ad, 16))
                        .map(|(lbn, part)| Icb { part, lbn }),
                }
                .ok_or_else(|| malformed("UDF allocation descriptor"))?;
                match raw >> 30 {
                    0 => {
                        for (at, l) in self.map_extent(loc.part, loc.lbn, u64::from(len))? {
                            fd.push(l, Piece::Disk(at))?;
                        }
                    }
                    3 => {
                        next = Some(loc);
                        break;
                    }
                    _ => fd.push(u64::from(len), Piece::Zero)?,
                }
            }
            let Some(loc) = next else {
                return Ok(());
            };
            continuations += 1;
            if continuations > MAX_ALLOC_EXTENT_DESCRIPTORS {
                return Err(DetectError::LimitExceeded(
                    "UDF allocation extent descriptors",
                ));
            }
            let b = self.read_block(disk, loc)?;
            if tag(&b) != Some(TAG_AED) {
                return Err(malformed("UDF allocation extent descriptor"));
            }
            let l = le32(&b, 20).unwrap_or(0) as usize;
            cur = b
                .get(24..24usize.saturating_add(l))
                .ok_or_else(|| malformed("UDF allocation extent descriptor"))?
                .to_vec();
        }
    }
}

impl Fs for Udf {
    type Node = Icb;

    fn root(&self) -> Icb {
        self.root
    }

    fn list<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        dir: &Icb,
    ) -> Result<Vec<DirEntry<Icb>>, DetectError> {
        let fd = self.read_icb(disk, *dir)?;
        if fd.size > MAX_DIR_BYTES {
            return Err(DetectError::LimitExceeded("directory size"));
        }
        let data = fd.read(disk, 0, MAX_DIR_BYTES)?;
        let mut out = Vec::new();
        let mut pos = 0usize;
        while let Some(b) = data.get(pos..).filter(|b| b.len() >= 38) {
            if tag(b) != Some(TAG_FID) {
                break;
            }
            disk.visit_entry()?;
            let chars = b[18];
            let l_fi = usize::from(b[19]);
            let (_, icb) = long_ad(b, 20).ok_or_else(|| malformed("UDF file identifier"))?;
            let l_iu = usize::from(le16(b, 36).unwrap_or(0));
            let name_at = 38 + l_iu;
            let total = (name_at + l_fi + 3) & !3;
            pos += total;
            if chars & 0x0C != 0 || l_fi == 0 {
                continue; // deleted or parent entry
            }
            let Some(name) = b.get(name_at..name_at + l_fi).and_then(dchars) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            out.push(DirEntry {
                name,
                is_dir: chars & 0x02 != 0,
                node: icb,
            });
        }
        Ok(out)
    }

    fn open<R: Read + Seek>(
        &self,
        disk: &mut Disk<'_, R>,
        node: &Icb,
    ) -> Result<FileData, DetectError> {
        self.read_icb(disk, *node)
    }
}
