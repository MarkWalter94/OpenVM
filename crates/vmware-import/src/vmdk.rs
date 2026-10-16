//! VMDK descriptors: text files, or embedded in a hosted sparse extent.
//!
//! A VMDK disk is described by a text descriptor: either a small file of
//! its own (monolithicFlat, twoGbMaxExtentSparse/Flat) or a region inside
//! the header of a hosted sparse extent (`KDMV` magic: monolithicSparse,
//! streamOptimized and the delta disks VMware writes for snapshots). The
//! descriptor names the extents (data files) and, for a snapshot delta,
//! the parent (`parentFileNameHint`).
//!
//! Accepted (QEMU reads them as plain disk data): create types
//! [`ALLOWED_CREATE_TYPES`], extents `FLAT`, `SPARSE`, `ZERO`, `VMFS`,
//! `VMFSSPARSE` with access `RW`, `RDONLY` or `NOACCESS`. Refused: raw
//! device mappings (`VMFSRAW`, `VMFSRDM`, `fullDevice`,
//! `partitionedDevice`), `SESPARSE`, ESX `COWD` sparse files, custom
//! types. Limits: [`crate::MAX_DESCRIPTOR_BYTES`], [`crate::MAX_EXTENTS`],
//! [`MAX_LINES`].

use std::io::{Read, Seek, SeekFrom};

use crate::ImportError;

/// Most lines of a descriptor.
pub const MAX_LINES: usize = 10_000;
/// Longest file name in a descriptor.
pub const MAX_NAME_BYTES: usize = 1024;

/// Create types accepted (lower case).
pub const ALLOWED_CREATE_TYPES: &[&str] = &[
    "monolithicsparse",
    "monolithicflat",
    "twogbmaxextentsparse",
    "twogbmaxextentflat",
    "streamoptimized",
    "vmfs",
    "vmfssparse",
];

/// Access of an extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// `RW`.
    ReadWrite,
    /// `RDONLY`.
    ReadOnly,
    /// `NOACCESS`.
    NoAccess,
}

/// Kind of extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentKind {
    /// `FLAT` / `VMFS`: raw data in a file.
    Flat,
    /// `SPARSE` / `VMFSSPARSE`: hosted or ESX sparse file.
    Sparse,
    /// `ZERO`: no file, reads as zeros.
    Zero,
}

/// One extent line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    /// Access.
    pub access: Access,
    /// Size in 512-byte sectors.
    pub sectors: u64,
    /// Kind.
    pub kind: ExtentKind,
    /// File name as written (relative to the descriptor's folder, or
    /// absolute), `None` for `ZERO`.
    pub file: Option<String>,
}

/// Parsed descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// `createType` (as written).
    pub create_type: String,
    /// `parentFileNameHint` (as written), for a snapshot delta.
    pub parent_hint: Option<String>,
    /// `CID`.
    pub cid: Option<u32>,
    /// `parentCID` (`ffffffff` = no parent).
    pub parent_cid: Option<u32>,
    /// Extents in order.
    pub extents: Vec<Extent>,
}

impl Descriptor {
    /// Total size in sectors.
    #[must_use]
    pub fn sectors(&self) -> u64 {
        self.extents
            .iter()
            .fold(0u64, |a, e| a.saturating_add(e.sectors))
    }
}

/// Magic of a hosted sparse extent (`KDMV` little-endian).
pub const SPARSE_MAGIC: [u8; 4] = *b"KDMV";
/// Magic of an ESX sparse extent (`COWD`), not supported.
pub const COWD_MAGIC: [u8; 4] = *b"COWD";

/// Where a file's descriptor comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorSource {
    /// A text descriptor file.
    Text,
    /// Embedded in a hosted sparse extent.
    Embedded,
}

/// Reads the descriptor of `f` (`name` for messages): embedded in a
/// `KDMV` header, or the whole file if it is a small text descriptor. A
/// sparse extent without an embedded descriptor (the `-s001` parts of a
/// split disk) or a large file without a header (a flat extent) is not a
/// descriptor.
pub fn read_descriptor(
    f: &mut std::fs::File,
    name: &str,
) -> Result<(Descriptor, DescriptorSource), ImportError> {
    let not_desc = |why| ImportError::InvalidDescriptor(name.to_owned(), why);
    let io = |_| ImportError::Io(name.to_owned());
    let len = f.metadata().map_err(io)?.len();
    f.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut head = [0u8; 512];
    let n = read_up_to(f, &mut head).map_err(io)?;
    if n >= 4 && head[..4] == COWD_MAGIC {
        return Err(ImportError::UnsupportedDisk(
            name.to_owned(),
            "ESX sparse (COWD) disks are not supported",
        ));
    }
    if n >= 4 && head[..4] == SPARSE_MAGIC {
        if n < 512 {
            return Err(not_desc("truncated sparse header"));
        }
        let le64 = |o: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&head[o..o + 8]);
            u64::from_le_bytes(b)
        };
        let desc_offset = le64(28);
        let desc_sectors = le64(36);
        if desc_offset == 0 || desc_sectors == 0 {
            return Err(not_desc(
                "a sparse extent without an embedded descriptor (choose the main .vmdk)",
            ));
        }
        let bytes = desc_sectors
            .checked_mul(512)
            .filter(|b| *b <= crate::MAX_DESCRIPTOR_BYTES)
            .ok_or_else(|| ImportError::TooLarge(format!("the descriptor of {name}")))?;
        let start = desc_offset
            .checked_mul(512)
            .filter(|s| s.checked_add(bytes).is_some_and(|end| end <= len))
            .ok_or_else(|| not_desc("descriptor outside the file"))?;
        f.seek(SeekFrom::Start(start)).map_err(io)?;
        let mut buf = vec![0u8; usize::try_from(bytes).map_err(|_| not_desc("size"))?];
        f.read_exact(&mut buf).map_err(io)?;
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        let d = parse_descriptor(&buf[..end], name)?;
        return Ok((d, DescriptorSource::Embedded));
    }
    if len > crate::MAX_DESCRIPTOR_BYTES {
        return Err(not_desc(
            "not a VMDK descriptor (a data extent? choose the main .vmdk)",
        ));
    }
    f.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    f.take(crate::MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(io)?;
    if buf.len() as u64 > crate::MAX_DESCRIPTOR_BYTES {
        return Err(ImportError::TooLarge(format!("the descriptor {name}")));
    }
    let d = parse_descriptor(&buf, name)?;
    Ok((d, DescriptorSource::Text))
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Parses descriptor text.
pub fn parse_descriptor(bytes: &[u8], name: &str) -> Result<Descriptor, ImportError> {
    let bad = |why| ImportError::InvalidDescriptor(name.to_owned(), why);
    if bytes.len() as u64 > crate::MAX_DESCRIPTOR_BYTES {
        return Err(ImportError::TooLarge(format!("the descriptor of {name}")));
    }
    if bytes.contains(&0) {
        return Err(bad("binary content"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| bad("not UTF-8 text"))?;
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r').trim());
    let first = lines.next().unwrap_or("");
    if !first.eq_ignore_ascii_case("# Disk DescriptorFile") {
        return Err(bad("not a VMDK descriptor"));
    }
    let mut create_type = None;
    let mut parent_hint = None;
    let mut cid = None;
    let mut parent_cid = None;
    let mut extents = Vec::new();
    for (n, line) in lines.enumerate() {
        if n >= MAX_LINES {
            return Err(bad("too many lines"));
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(ext) = parse_extent(line, name)? {
            if extents.len() >= crate::MAX_EXTENTS {
                return Err(bad("too many extents"));
            }
            extents.push(ext);
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        match k.as_str() {
            "createtype" => create_type = Some(quoted(v).ok_or_else(|| bad("createType"))?),
            "parentfilenamehint" => {
                let h = quoted(v).ok_or_else(|| bad("parentFileNameHint"))?;
                check_name(&h, name)?;
                parent_hint = Some(h);
            }
            "cid" => cid = Some(hex32(v).ok_or_else(|| bad("CID"))?),
            "parentcid" => parent_cid = Some(hex32(v).ok_or_else(|| bad("parentCID"))?),
            _ => {}
        }
    }
    let create_type = create_type.ok_or_else(|| bad("createType missing"))?;
    let ct = create_type.to_ascii_lowercase();
    if !ALLOWED_CREATE_TYPES.contains(&ct.as_str()) {
        return Err(ImportError::UnsupportedDisk(
            name.to_owned(),
            "this VMDK type (raw device, ESX-only or custom) is not supported",
        ));
    }
    if extents.is_empty() {
        return Err(bad("no extents"));
    }
    if extents.iter().all(|e| e.kind == ExtentKind::Zero) {
        return Err(bad("no data extents"));
    }
    let d = Descriptor {
        create_type,
        parent_hint,
        cid,
        parent_cid,
        extents,
    };
    if d.sectors() == 0 || d.sectors() == u64::MAX {
        return Err(bad("disk size"));
    }
    // A parent CID other than "none" needs a parent, and the other way round.
    let has_parent_cid = d.parent_cid.is_some_and(|p| p != u32::MAX);
    if d.parent_hint.is_some() && !has_parent_cid {
        return Err(bad("parentFileNameHint without parentCID"));
    }
    if has_parent_cid && d.parent_hint.is_none() {
        return Err(bad("parentCID without parentFileNameHint"));
    }
    Ok(d)
}

/// Extent line: `ACCESS SECTORS TYPE ["FILE" [OFFSET]]`; `Ok(None)` if the
/// line is not an extent line.
fn parse_extent(line: &str, name: &str) -> Result<Option<Extent>, ImportError> {
    let bad = |why| ImportError::InvalidDescriptor(name.to_owned(), why);
    let (access_s, rest) = token(line);
    let access = match access_s {
        "RW" => Access::ReadWrite,
        "RDONLY" => Access::ReadOnly,
        "NOACCESS" => Access::NoAccess,
        _ => return Ok(None),
    };
    let (sectors_s, rest) = token(rest);
    let sectors = Some(sectors_s)
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| bad("extent size"))?;
    let (kind_s, tail) = token(rest);
    let kind = match kind_s {
        "FLAT" | "VMFS" => ExtentKind::Flat,
        "SPARSE" | "VMFSSPARSE" => ExtentKind::Sparse,
        "ZERO" => ExtentKind::Zero,
        "VMFSRAW" | "VMFSRDM" | "SESPARSE" => {
            return Err(ImportError::UnsupportedDisk(
                name.to_owned(),
                "raw device mappings and SE sparse extents are not supported",
            ));
        }
        _ => return Err(bad("unknown extent type")),
    };
    let tail = tail.trim();
    let file = if kind == ExtentKind::Zero {
        if !tail.is_empty() {
            return Err(bad("ZERO extent with a file"));
        }
        None
    } else {
        let inner = tail.strip_prefix('"').ok_or_else(|| bad("extent file"))?;
        let end = inner.find('"').ok_or_else(|| bad("extent file"))?;
        let file = &inner[..end];
        let after = inner[end + 1..].trim();
        if !(after.is_empty() || (after.len() <= 20 && after.bytes().all(|b| b.is_ascii_digit()))) {
            return Err(bad("extent offset"));
        }
        check_name(file, name)?;
        Some(file.to_owned())
    };
    if sectors == 0 {
        return Err(bad("empty extent"));
    }
    Ok(Some(Extent {
        access,
        sectors,
        kind,
        file,
    }))
}

/// First blank-separated token of `s` and the rest.
fn token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// File name from a descriptor: non-empty, bounded, no control characters.
fn check_name(f: &str, name: &str) -> Result<(), ImportError> {
    if f.is_empty() || f.len() > MAX_NAME_BYTES || f.chars().any(char::is_control) {
        return Err(ImportError::InvalidDescriptor(
            name.to_owned(),
            "file name in the descriptor",
        ));
    }
    Ok(())
}

fn quoted(v: &str) -> Option<String> {
    let inner = v.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('"')).then(|| inner.to_owned())
}

fn hex32(v: &str) -> Option<u32> {
    let v = v.trim();
    (!v.is_empty() && v.len() <= 8)
        .then(|| u32::from_str_radix(v, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAT: &str = "# Disk DescriptorFile\nversion=1\nCID=fd32bc87\nparentCID=ffffffff\n\
        createType=\"monolithicFlat\"\n\n# Extent description\nRW 6291456 FLAT \"d-flat.vmdk\" 0\n\n\
        ddb.adapterType = \"ide\"\n";

    #[test]
    fn flat_descriptor() {
        let d = parse_descriptor(FLAT.as_bytes(), "d.vmdk").unwrap();
        assert_eq!(d.create_type, "monolithicFlat");
        assert_eq!(d.extents.len(), 1);
        assert_eq!(d.extents[0].file.as_deref(), Some("d-flat.vmdk"));
        assert_eq!(d.sectors(), 6_291_456);
        assert_eq!(d.parent_hint, None);
    }

    #[test]
    fn refusals() {
        let with = |from: &str, to: &str| FLAT.replace(from, to);
        for (bad, what) in [
            (with("monolithicFlat", "fullDevice"), "full device"),
            (with("monolithicFlat", "partitionedDevice"), "partitioned"),
            (with("monolithicFlat", "custom"), "custom"),
            (with("FLAT \"d-flat.vmdk\" 0", "VMFSRDM \"x.vmdk\""), "rdm"),
            (with("FLAT \"d-flat.vmdk\" 0", "VMFSRAW \"x.vmdk\""), "raw"),
            (with("FLAT \"d-flat.vmdk\" 0", "FLAT \"\" 0"), "empty name"),
            (with("FLAT \"d-flat.vmdk\" 0", "FLAT \"a\" x"), "offset"),
            (with("RW 6291456", "RW 0"), "empty extent"),
            (with("RW 6291456", "RW 99999999999999999999999"), "size"),
            (with("# Disk DescriptorFile", "# Something"), "header"),
            (
                with("parentCID=ffffffff", "parentCID=12345678"),
                "parent cid",
            ),
            (
                with("createType", "parentFileNameHint=\"p.vmdk\"\ncreateType"),
                "hint without parent cid",
            ),
        ] {
            assert!(parse_descriptor(bad.as_bytes(), "d").is_err(), "{what}");
        }
        assert!(parse_descriptor(b"# Disk DescriptorFile\0", "d").is_err());
        let many = format!(
            "{FLAT}{}",
            "RW 1 FLAT \"x.vmdk\" 0\n".repeat(crate::MAX_EXTENTS)
        );
        assert!(parse_descriptor(many.as_bytes(), "d").is_err());
    }

    #[test]
    fn delta_with_parent() {
        let t = "# Disk DescriptorFile\nversion=1\nCID=53c96037\nparentCID=a2cda9cd\n\
                 createType=\"monolithicSparse\"\nparentFileNameHint=\"base.vmdk\"\n\
                 RW 6291456 SPARSE \"child.vmdk\"\n";
        let d = parse_descriptor(t.as_bytes(), "child.vmdk").unwrap();
        assert_eq!(d.parent_hint.as_deref(), Some("base.vmdk"));
        assert_eq!(d.parent_cid, Some(0xa2cd_a9cd));
    }
}
