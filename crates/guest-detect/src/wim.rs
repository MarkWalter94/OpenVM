//! WIM / ESD header and XML metadata: version, architecture and edition of
//! the first installable image. The XML resource is read uncompressed (as
//! written by every Microsoft tool, also in `install.esd`) and scanned with
//! a tolerant tag finder, not a full XML parser.

use std::io::{Read, Seek};

use crate::DetectError;
use crate::disk::{Disk, FileData};

/// WIM header magic.
pub(crate) const MAGIC: &[u8; 8] = b"MSWIM\0\0\0";
/// Size of the WIM header read (`WIMHEADER_V1_PACKED`).
const HEADER_LEN: u64 = 208;
/// Offset of the XML data `RESHDR_DISK_SHORT` in the header.
const XML_RESHDR: usize = 72;
/// Resource flag: compressed.
const RESHDR_FLAG_COMPRESSED: u8 = 0x04;
/// Upper bound on the XML bytes read.
pub(crate) const MAX_XML_BYTES: u64 = 1024 * 1024;
/// Upper bound on the `<IMAGE>` elements examined.
const MAX_IMAGES: usize = 64;

/// Fields of one `<IMAGE>` element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImageInfo {
    pub(crate) major: Option<u32>,
    pub(crate) minor: Option<u32>,
    pub(crate) build: Option<u32>,
    pub(crate) arch: Option<u32>,
    pub(crate) edition_id: Option<String>,
    pub(crate) installation_type: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) display_name: Option<String>,
}

/// Reads the header and the XML metadata of a WIM/ESD file and returns the
/// first image with a version that is not a Windows PE / setup image.
pub(crate) fn read_image_info<R: Read + Seek>(
    disk: &mut Disk<'_, R>,
    file: &FileData,
) -> Result<Option<ImageInfo>, DetectError> {
    let header = file.read(disk, 0, HEADER_LEN)?;
    if header.len() < HEADER_LEN as usize || header.get(..8) != Some(MAGIC.as_slice()) {
        return Err(DetectError::Malformed("WIM header"));
    }
    let rh = header
        .get(XML_RESHDR..XML_RESHDR + 24)
        .ok_or(DetectError::Malformed("WIM header"))?;
    let mut size7 = [0u8; 8];
    size7[..7].copy_from_slice(&rh[..7]);
    let size = u64::from_le_bytes(size7);
    let flags = rh[7];
    let mut off8 = [0u8; 8];
    off8.copy_from_slice(&rh[8..16]);
    let offset = u64::from_le_bytes(off8);
    if flags & RESHDR_FLAG_COMPRESSED != 0 {
        return Err(DetectError::Unsupported("compressed WIM XML resource"));
    }
    if size == 0 || offset < HEADER_LEN {
        return Err(DetectError::Malformed("WIM XML resource"));
    }
    let xml = file.read(disk, offset, size.min(MAX_XML_BYTES))?;
    Ok(first_install_image(&decode_xml(&xml)))
}

/// Decodes the XML bytes: UTF-16LE (with or without BOM) or UTF-8.
pub(crate) fn decode_xml(b: &[u8]) -> String {
    let utf16 = b.starts_with(&[0xFF, 0xFE]) || (b.len() >= 2 && b[0] == b'<' && b[1] == 0);
    if utf16 {
        let body = b.strip_prefix(&[0xFF, 0xFE]).unwrap_or(b);
        let units = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect()
    } else {
        String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)).into_owned()
    }
}

/// Text content of the first `<tag>…</tag>` in `s` (tags without
/// attributes, as written in WIM XML).
fn tag_text<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let rest = s.get(start..)?;
    let end = rest.find(&close)?;
    rest.get(..end)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

fn number(s: &str, tag: &str) -> Option<u32> {
    tag_text(s, tag)?.trim().parse().ok()
}

fn text(s: &str, tag: &str) -> Option<String> {
    tag_text(s, tag).map(unescape).filter(|t| !t.is_empty())
}

/// Parses one `<IMAGE>` element body.
pub(crate) fn parse_image(s: &str) -> ImageInfo {
    ImageInfo {
        major: number(s, "MAJOR"),
        minor: number(s, "MINOR"),
        build: number(s, "BUILD"),
        arch: number(s, "ARCH"),
        edition_id: text(s, "EDITIONID"),
        installation_type: text(s, "INSTALLATIONTYPE"),
        name: text(s, "NAME"),
        display_name: text(s, "DISPLAYNAME"),
    }
}

/// The first image with a version that is not Windows PE.
pub(crate) fn first_install_image(xml: &str) -> Option<ImageInfo> {
    let mut rest = xml;
    for _ in 0..MAX_IMAGES {
        let at = rest.find("<IMAGE")?;
        let after = rest.get(at + 6..)?;
        if !after.starts_with([' ', '>', '\t', '\r', '\n']) {
            rest = after;
            continue;
        }
        let end = after.find("</IMAGE>").unwrap_or(after.len());
        let body = after.get(..end)?;
        rest = after.get(end..).unwrap_or("");
        let info = parse_image(body);
        let is_pe = [&info.edition_id, &info.installation_type].iter().any(|v| {
            v.as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("WindowsPE"))
        });
        if info.major.is_some() && !is_pe {
            return Some(info);
        }
        if rest.is_empty() {
            return None;
        }
    }
    None
}
