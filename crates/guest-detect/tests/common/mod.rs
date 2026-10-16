//! Synthetic image builders for the detection tests: ISO 9660 (primary and
//! Joliet hierarchies, subdirectories), UDF 1.02 / 2.50 (file entries or
//! extended file entries, short / long / embedded allocation descriptors,
//! optional metadata partition) and WIM headers with UTF-16LE XML.

#![allow(dead_code, unreachable_pub)]

use std::collections::BTreeMap;

pub const SECTOR: usize = 2048;

// ---------------------------------------------------------------- ISO 9660

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, usize)>,
}

impl Dir {
    fn insert(&mut self, path: &str, index: usize) {
        match path.split_once('/') {
            Some((d, rest)) => self
                .dirs
                .entry(d.to_owned())
                .or_default()
                .insert(rest, index),
            None => self.files.push((path.to_owned(), index)),
        }
    }
}

fn both16(v: u16) -> [u8; 4] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], b[0], b[1]]
}

fn both32(v: u32) -> [u8; 8] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], l[2], l[3], b[0], b[1], b[2], b[3]]
}

fn dir_record(id: &[u8], lba: u32, len: u32, flags: u8) -> Vec<u8> {
    let pad = usize::from(id.len().is_multiple_of(2));
    let total = 33 + id.len() + pad;
    let mut r = vec![0u8; total];
    r[0] = total as u8;
    r[2..10].copy_from_slice(&both32(lba));
    r[10..18].copy_from_slice(&both32(len));
    r[25] = flags;
    r[28..32].copy_from_slice(&both16(1));
    r[32] = id.len() as u8;
    r[33..33 + id.len()].copy_from_slice(id);
    r
}

fn iso_name(name: &str, is_dir: bool, joliet: bool) -> Vec<u8> {
    if joliet {
        let mut s = name.to_owned();
        if !is_dir {
            s.push_str(";1");
        }
        s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    } else {
        let mut s: String = name
            .chars()
            .map(|c| {
                if c == '.' && is_dir {
                    '_'
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        if !is_dir {
            if !s.contains('.') {
                s.push('.');
            }
            s.push_str(";1");
        }
        s.into_bytes()
    }
}

fn sectors(len: usize) -> u32 {
    len.div_ceil(SECTOR).max(1) as u32
}

/// Writes an ISO 9660 image with `files` (paths with `/`). Primary names are
/// upper case (directories starting with `.` become `_`); the Joliet
/// hierarchy, when present, keeps the original names. Directories occupy
/// one sector each. Returns the image; file data starts at sector 19.
pub fn build_iso(files: &[(&str, &[u8])], joliet: bool) -> Vec<u8> {
    let mut tree = Dir::default();
    for (i, (p, _)) in files.iter().enumerate() {
        tree.insert(p, i);
    }
    let mut next = 19u32;
    // Directory sectors for each hierarchy, keyed by path.
    let mut lbas: Vec<BTreeMap<String, u32>> = Vec::new();
    let hierarchies: &[bool] = if joliet { &[false, true] } else { &[false] };
    for _ in hierarchies {
        let mut map = BTreeMap::new();
        fn alloc(d: &Dir, path: String, next: &mut u32, map: &mut BTreeMap<String, u32>) {
            map.insert(path.clone(), *next);
            *next += 1;
            for (n, sub) in &d.dirs {
                alloc(sub, format!("{path}/{n}"), next, map);
            }
        }
        alloc(&tree, String::new(), &mut next, &mut map);
        lbas.push(map);
    }
    let mut file_lba = Vec::new();
    for (_, data) in files {
        file_lba.push(next);
        next += sectors(data.len());
    }
    let mut img = vec![0u8; next as usize * SECTOR];

    for (h, &is_joliet) in hierarchies.iter().enumerate() {
        let map = &lbas[h];
        #[allow(clippy::too_many_arguments)]
        fn write(
            img: &mut [u8],
            d: &Dir,
            path: &str,
            parent: &str,
            map: &BTreeMap<String, u32>,
            file_lba: &[u32],
            files: &[(&str, &[u8])],
            joliet: bool,
        ) {
            let mut rec = Vec::new();
            rec.extend(dir_record(&[0], map[path], SECTOR as u32, 2));
            rec.extend(dir_record(&[1], map[parent], SECTOR as u32, 2));
            for n in d.dirs.keys() {
                let p = format!("{path}/{n}");
                rec.extend(dir_record(
                    &iso_name(n, true, joliet),
                    map[&p],
                    SECTOR as u32,
                    2,
                ));
            }
            for (n, i) in &d.files {
                rec.extend(dir_record(
                    &iso_name(n, false, joliet),
                    file_lba[*i],
                    files[*i].1.len() as u32,
                    0,
                ));
            }
            assert!(rec.len() <= SECTOR, "directory larger than one sector");
            let o = map[path] as usize * SECTOR;
            img[o..o + rec.len()].copy_from_slice(&rec);
            for (n, sub) in &d.dirs {
                write(
                    img,
                    sub,
                    &format!("{path}/{n}"),
                    path,
                    map,
                    file_lba,
                    files,
                    joliet,
                );
            }
        }
        write(&mut img, &tree, "", "", map, &file_lba, files, is_joliet);

        let vd_lba = 16 + h;
        let p = &mut img[vd_lba * SECTOR..(vd_lba + 1) * SECTOR];
        p[0] = if is_joliet { 2 } else { 1 };
        p[1..6].copy_from_slice(b"CD001");
        p[6] = 1;
        p[80..88].copy_from_slice(&both32(next));
        if is_joliet {
            p[88..91].copy_from_slice(b"%/E");
        }
        p[128..132].copy_from_slice(&both16(SECTOR as u16));
        p[156..190].copy_from_slice(&dir_record(&[0], map[""], SECTOR as u32, 2));
    }
    let term = 16 + hierarchies.len();
    img[term * SECTOR] = 255;
    img[term * SECTOR + 1..term * SECTOR + 6].copy_from_slice(b"CD001");
    for (i, (_, data)) in files.iter().enumerate() {
        let o = file_lba[i] as usize * SECTOR;
        img[o..o + data.len()].copy_from_slice(data);
    }
    img
}

// --------------------------------------------------------------------- UDF

/// Allocation descriptor flavour for file data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ad {
    Short,
    Long,
    /// Files up to 1 KiB are embedded in the file entry; larger ones use
    /// short allocation descriptors.
    Embedded,
}

#[derive(Clone, Copy, Debug)]
pub struct UdfOpts {
    /// Extended file entries (tag 266) instead of file entries (tag 261).
    pub efe: bool,
    pub ad: Ad,
    /// File identifiers as 16-bit d-strings (compression id 16).
    pub wide_names: bool,
    /// UDF 2.50 metadata partition holding file entries and directories.
    pub metadata: bool,
}

impl Default for UdfOpts {
    fn default() -> Self {
        Self {
            efe: false,
            ad: Ad::Short,
            wide_names: false,
            metadata: false,
        }
    }
}

const PART_START: usize = 300;

pub fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

pub fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Writes descriptor tag `id` at `loc` with its checksum.
pub fn finish_tag(b: &mut [u8], id: u16, loc: u32) {
    put16(b, 0, id);
    put16(b, 2, 2);
    put32(b, 12, loc);
    let sum = b[..16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |a, (_, &v)| a.wrapping_add(v));
    b[4] = sum;
}

struct UdfImage {
    img: Vec<u8>,
    opts: UdfOpts,
    /// Next free physical block (relative to the partition start).
    next_phys: u32,
    /// Next free metadata block, and the physical block where the metadata
    /// region starts.
    next_meta: u32,
    meta_base: u32,
}

/// Partition reference and block of a file entry or directory block.
#[derive(Clone, Copy)]
struct Loc {
    part: u16,
    lbn: u32,
}

impl UdfImage {
    fn block_mut(&mut self, loc: Loc) -> &mut [u8] {
        let phys = if loc.part == 1 {
            self.meta_base + loc.lbn
        } else {
            loc.lbn
        };
        let o = (PART_START + phys as usize) * SECTOR;
        if self.img.len() < o + SECTOR {
            self.img.resize(o + SECTOR, 0);
        }
        &mut self.img[o..o + SECTOR]
    }

    /// Allocates `n` blocks for file data (physical partition).
    fn alloc_data(&mut self, n: u32) -> Loc {
        let lbn = self.next_phys;
        self.next_phys += n;
        Loc { part: 0, lbn }
    }

    /// Allocates one block for a descriptor (metadata partition if enabled).
    fn alloc_desc(&mut self) -> Loc {
        if self.opts.metadata {
            let lbn = self.next_meta;
            self.next_meta += 1;
            Loc { part: 1, lbn }
        } else {
            self.alloc_data(1)
        }
    }

    fn write_data(&mut self, loc: Loc, data: &[u8]) {
        for (i, chunk) in data.chunks(SECTOR).enumerate() {
            let b = self.block_mut(Loc {
                part: loc.part,
                lbn: loc.lbn + i as u32,
            });
            b[..chunk.len()].copy_from_slice(chunk);
        }
    }

    /// Writes a file entry at `fe` for `data` of `file_type`, storing the
    /// data in the metadata partition when `in_desc_part` (directories).
    fn write_fe(&mut self, fe: Loc, file_type: u8, data: &[u8], in_desc_part: bool) {
        let embedded = self.opts.ad == Ad::Embedded && data.len() <= 1024;
        let mut ads = Vec::new();
        let ad_type: u16;
        if embedded {
            ad_type = 3;
            ads.extend_from_slice(data);
        } else {
            let n = sectors(data.len());
            let loc = if in_desc_part && self.opts.metadata {
                let lbn = self.next_meta;
                self.next_meta += n;
                Loc { part: 1, lbn }
            } else {
                self.alloc_data(n)
            };
            self.write_data(loc, data);
            // Short descriptors refer to the partition of the file entry;
            // data outside it needs a long descriptor.
            let long = self.opts.ad == Ad::Long || loc.part != fe.part;
            // Split into extents of at most 2 blocks to exercise multiple
            // descriptors.
            let mut remaining = data.len();
            let mut lbn = loc.lbn;
            loop {
                let len = remaining.min(2 * SECTOR);
                if long {
                    let mut ad = [0u8; 16];
                    put32(&mut ad, 0, len as u32);
                    put32(&mut ad, 4, lbn);
                    put16(&mut ad, 8, loc.part);
                    ads.extend_from_slice(&ad);
                } else {
                    let mut ad = [0u8; 8];
                    put32(&mut ad, 0, len as u32);
                    put32(&mut ad, 4, lbn);
                    ads.extend_from_slice(&ad);
                }
                remaining -= len;
                lbn += 2;
                if remaining == 0 {
                    break;
                }
            }
            ad_type = if long { 1 } else { 0 };
        }
        let efe = self.opts.efe;
        let b = self.block_mut(fe);
        b[27] = file_type;
        put16(b, 34, ad_type);
        put64(b, 56, data.len() as u64);
        let (l_ea_at, base) = if efe { (208, 216) } else { (168, 176) };
        put32(b, l_ea_at, 0);
        put32(b, l_ea_at + 4, ads.len() as u32);
        b[base..base + ads.len()].copy_from_slice(&ads);
        finish_tag(b, if efe { 266 } else { 261 }, fe.lbn);
    }

    fn fid(&self, name: Option<&str>, is_dir: bool, icb: Loc) -> Vec<u8> {
        let name_bytes: Vec<u8> = match name {
            None => Vec::new(),
            Some(n) if self.opts.wide_names => std::iter::once(16)
                .chain(n.encode_utf16().flat_map(|u| u.to_be_bytes()))
                .collect(),
            Some(n) => std::iter::once(8).chain(n.bytes()).collect(),
        };
        let total = (38 + name_bytes.len() + 3) & !3;
        let mut f = vec![0u8; total];
        put16(&mut f, 16, 1);
        f[18] = if name.is_none() {
            0x0A
        } else if is_dir {
            0x02
        } else {
            0
        };
        f[19] = name_bytes.len() as u8;
        put32(&mut f, 20, SECTOR as u32);
        put32(&mut f, 24, icb.lbn);
        put16(&mut f, 28, icb.part);
        f[38..38 + name_bytes.len()].copy_from_slice(&name_bytes);
        finish_tag(&mut f, 257, 0);
        f
    }

    fn write_dir(&mut self, d: &Dir, fe: Loc, parent: Loc, files: &[(&str, &[u8])]) {
        let mut fids = self.fid(None, true, parent);
        let mut children = Vec::new();
        for (n, sub) in &d.dirs {
            let loc = self.alloc_desc();
            fids.extend(self.fid(Some(n), true, loc));
            children.push((sub, loc));
        }
        for (n, i) in &d.files {
            let loc = self.alloc_desc();
            fids.extend(self.fid(Some(n), false, loc));
            let data = files[*i].1;
            self.write_fe(loc, 5, data, false);
        }
        self.write_fe(fe, 4, &fids, true);
        for (sub, loc) in children {
            self.write_dir(sub, loc, fe, files);
        }
    }
}

/// UDF image (with a small ISO 9660 bridge holding `README.TXT`, as on
/// Microsoft media) containing `files`.
pub fn build_udf(files: &[(&str, &[u8])], opts: UdfOpts) -> Vec<u8> {
    let mut img = build_iso(
        &[("README.TXT", b"This disc contains a UDF file system.")],
        false,
    );
    assert!(img.len() <= 256 * SECTOR);
    img.resize(PART_START * SECTOR, 0);
    let mut u = UdfImage {
        img,
        opts,
        next_phys: 0,
        next_meta: 0,
        meta_base: 0,
    };
    // Metadata region: physical blocks 1..=64; the metadata file entry sits
    // at physical block 0.
    let meta_blocks = 64u32;
    if opts.metadata {
        u.next_phys = 1 + meta_blocks;
        u.meta_base = 1;
    }
    let fsd = u.alloc_desc();
    let root = u.alloc_desc();
    let mut tree = Dir::default();
    for (i, (p, _)) in files.iter().enumerate() {
        tree.insert(p, i);
    }
    u.write_dir(&tree, root, root, files);
    assert!(u.next_meta <= meta_blocks, "metadata region too small");

    if opts.metadata {
        // Metadata file: one short descriptor covering the region.
        let b = u.block_mut(Loc { part: 0, lbn: 0 });
        b[27] = 250;
        put16(b, 34, 0);
        put64(b, 56, u64::from(meta_blocks) * SECTOR as u64);
        put32(b, 208, 0);
        put32(b, 212, 8);
        put32(b, 216, meta_blocks * SECTOR as u32);
        put32(b, 220, 1);
        finish_tag(b, 266, 0);
    }
    // File set descriptor.
    {
        let b = u.block_mut(fsd);
        put32(b, 400, SECTOR as u32);
        put32(b, 404, root.lbn);
        put16(b, 408, root.part);
        finish_tag(b, 256, fsd.lbn);
    }
    let part_len = u.next_phys.max(1);
    let total = PART_START + part_len as usize + 1;
    u.img.resize(total * SECTOR, 0);
    let img = &mut u.img;
    // Anchor.
    {
        let b = &mut img[256 * SECTOR..257 * SECTOR];
        put32(b, 16, 3 * SECTOR as u32);
        put32(b, 20, 257);
        put32(b, 24, 3 * SECTOR as u32);
        put32(b, 28, 257);
        finish_tag(b, 2, 256);
    }
    // Partition descriptor.
    {
        let b = &mut img[257 * SECTOR..258 * SECTOR];
        put16(b, 22, 0);
        put32(b, 188, PART_START as u32);
        put32(b, 192, part_len);
        finish_tag(b, 5, 257);
    }
    // Logical volume descriptor.
    {
        let b = &mut img[258 * SECTOR..259 * SECTOR];
        put32(b, 212, SECTOR as u32);
        put32(b, 248, SECTOR as u32);
        put32(b, 252, fsd.lbn);
        put16(b, 256, fsd.part);
        let mut maps = vec![1u8, 6, 1, 0, 0, 0];
        if opts.metadata {
            let mut m = vec![0u8; 64];
            m[0] = 2;
            m[1] = 64;
            m[5..28].copy_from_slice(b"*UDF Metadata Partition");
            m[36] = 1;
            m[38] = 0;
            m[40..44].copy_from_slice(&0u32.to_le_bytes());
            m[44..48].copy_from_slice(&0u32.to_le_bytes());
            maps.extend(m);
        }
        put32(b, 264, maps.len() as u32);
        put32(b, 268, if opts.metadata { 2 } else { 1 });
        b[440..440 + maps.len()].copy_from_slice(&maps);
        finish_tag(b, 6, 258);
    }
    // Terminating descriptor.
    finish_tag(&mut img[259 * SECTOR..260 * SECTOR], 8, 259);
    u.img
}

/// Byte offset of block `lbn` of the physical partition of a
/// [`build_udf`] image.
pub fn udf_block_at(lbn: u32) -> usize {
    (PART_START + lbn as usize) * SECTOR
}

/// Appends `n` blocks to the physical partition of a [`build_udf`] image
/// (partition descriptor at sector 257); returns the first new block.
pub fn udf_grow(img: &mut Vec<u8>, n: u32) -> u32 {
    let pd = 257 * SECTOR;
    let len = u32::from_le_bytes([img[pd + 192], img[pd + 193], img[pd + 194], img[pd + 195]]);
    put32(&mut img[pd..], 192, len + n);
    img.resize(udf_block_at(len + n) + SECTOR, 0);
    len
}

// --------------------------------------------------------------------- WIM

/// A minimal WIM file: 208-byte header and an uncompressed UTF-16LE XML
/// resource right after it.
pub fn build_wim(xml: &str) -> Vec<u8> {
    let mut x = vec![0xFF, 0xFE];
    x.extend(xml.encode_utf16().flat_map(|u| u.to_le_bytes()));
    let mut w = vec![0u8; 208];
    w[..8].copy_from_slice(b"MSWIM\0\0\0");
    w[8..12].copy_from_slice(&208u32.to_le_bytes());
    w[12..16].copy_from_slice(&0x10d00u32.to_le_bytes());
    w[44..48].copy_from_slice(&1u32.to_le_bytes());
    w[72..79].copy_from_slice(&(x.len() as u64).to_le_bytes()[..7]);
    w[79] = 0;
    w[80..88].copy_from_slice(&208u64.to_le_bytes());
    w[88..96].copy_from_slice(&(x.len() as u64).to_le_bytes());
    w.extend(x);
    w
}

/// WIM XML for one image.
pub fn wim_xml(
    major: u32,
    minor: u32,
    build: u32,
    arch: u32,
    edition: &str,
    display: &str,
) -> String {
    format!(
        "<WIM><TOTALBYTES>123</TOTALBYTES><IMAGE INDEX=\"1\"><DIRCOUNT>10</DIRCOUNT>\
         <WINDOWS><ARCH>{arch}</ARCH><PRODUCTNAME>Microsoft&#174; Windows&#174; Operating System</PRODUCTNAME>\
         <EDITIONID>{edition}</EDITIONID><INSTALLATIONTYPE>Client</INSTALLATIONTYPE>\
         <PRODUCTTYPE>WinNT</PRODUCTTYPE><LANGUAGES><LANGUAGE>en-US</LANGUAGE></LANGUAGES>\
         <VERSION><MAJOR>{major}</MAJOR><MINOR>{minor}</MINOR><BUILD>{build}</BUILD>\
         <SPBUILD>1</SPBUILD><SPLEVEL>1</SPLEVEL></VERSION><SYSTEMROOT>WINDOWS</SYSTEMROOT></WINDOWS>\
         <NAME>{display}</NAME><DESCRIPTION>{display}</DESCRIPTION><FLAGS>{edition}</FLAGS>\
         <DISPLAYNAME>{display}</DISPLAYNAME></IMAGE></WIM>"
    )
}
