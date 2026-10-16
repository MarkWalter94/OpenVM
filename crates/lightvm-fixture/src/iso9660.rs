//! Minimal ISO 9660 (ECMA-119) writer: one volume, files in the root
//! directory only, level-1 names (`NAME.EXT`, upper case), no Rock Ridge or
//! Joliet. Enough for a read-only data medium mounted by Linux (`iso9660`
//! driver, names shown in lower case) and accepted by the LightVM import
//! (`CD001` signature at offset 32769, ADR-0008).
//!
//! Reproducible: every timestamp is [`FIXED_TIME`], files are written in the
//! given order, every unused byte is zero (or space where the standard asks
//! for it). Layout (2048-byte sectors): 0–15 system area, 16 primary volume
//! descriptor, 17 terminator, 18 type-L path table, 19 type-M path table,
//! 20 root directory, 21… file data.

/// Logical block size.
pub const SECTOR: usize = 2048;

/// Recording time of every entry: 2026-01-01 00:00:00 UTC.
pub const FIXED_TIME: (u16, u8, u8, u8, u8, u8) = (2026, 1, 1, 0, 0, 0);

const PVD_LBA: usize = 16;
const PATH_L_LBA: u32 = 18;
const PATH_M_LBA: u32 = 19;
const ROOT_LBA: u32 = 20;
const FIRST_FILE_LBA: u32 = 21;

fn both16(v: u16) -> [u8; 4] {
    let l = v.to_le_bytes();
    let b = v.to_be_bytes();
    [l[0], l[1], b[0], b[1]]
}

fn both32(v: u32) -> [u8; 8] {
    let l = v.to_le_bytes();
    let b = v.to_be_bytes();
    [l[0], l[1], l[2], l[3], b[0], b[1], b[2], b[3]]
}

fn padded(s: &str, len: usize) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.resize(len, b' ');
    v
}

fn dir_date() -> [u8; 7] {
    let (y, mo, d, h, mi, s) = FIXED_TIME;
    [(y - 1900) as u8, mo, d, h, mi, s, 0]
}

fn volume_date() -> [u8; 17] {
    let (y, mo, d, h, mi, s) = FIXED_TIME;
    let text = format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}00");
    let mut out = [0u8; 17];
    out[..16].copy_from_slice(text.as_bytes());
    out
}

fn dir_record(id: &[u8], lba: u32, len: u32, dir: bool) -> Vec<u8> {
    let pad = usize::from(id.len().is_multiple_of(2));
    let total = 33 + id.len() + pad;
    let mut r = vec![0u8; total];
    r[0] = total as u8;
    r[2..10].copy_from_slice(&both32(lba));
    r[10..18].copy_from_slice(&both32(len));
    r[18..25].copy_from_slice(&dir_date());
    r[25] = if dir { 0x02 } else { 0x00 };
    r[28..32].copy_from_slice(&both16(1));
    r[32] = id.len() as u8;
    r[33..33 + id.len()].copy_from_slice(id);
    r
}

fn sectors(len: usize) -> usize {
    len.div_ceil(SECTOR).max(1)
}

/// ISO image with `files` (`NAME.EXT` level-1 names, at most 8+3 upper-case
/// characters) in the root directory and volume label `label` (d-characters,
/// at most 32).
///
/// # Panics
/// On names or a label outside the level-1 rules, or a root directory larger
/// than one sector: the inputs are constants of the fixture.
pub fn write_iso(label: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    write_iso_names(label, files, false)
}

/// Root-only ISO with ECMA-119 level-2 file identifiers (up to 31 characters,
/// including the version suffix). Allows Windows' AUTOUNATTEND.XML in lab
/// media. Short names produce exactly the same bytes as [`write_iso`].
pub fn write_iso_level2(label: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    write_iso_names(label, files, true)
}

fn write_iso_names(label: &str, files: &[(&str, &[u8])], level2: bool) -> Vec<u8> {
    assert!(
        label.len() <= 32
            && label
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'),
        "invalid volume label"
    );
    let mut lba = FIRST_FILE_LBA;
    let mut entries = Vec::new();
    for (name, data) in files {
        let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
        assert!(
            (1..=if level2 { 29 } else { 8 }).contains(&stem.len())
                && if level2 {
                    name.len() <= 29
                } else {
                    ext.len() <= 3
                }
                && !ext.contains('.')
                && name.bytes().all(|c| c.is_ascii_uppercase()
                    || c.is_ascii_digit()
                    || c == b'_'
                    || c == b'.'),
            "invalid ISO name {name}"
        );
        let id = format!("{name};1");
        let len = u32::try_from(data.len()).expect("file too large");
        entries.push((id, lba, len, *data));
        lba += sectors(data.len()) as u32;
    }
    let total_sectors = lba;

    let mut root = Vec::new();
    root.extend(dir_record(&[0], ROOT_LBA, SECTOR as u32, true));
    root.extend(dir_record(&[1], ROOT_LBA, SECTOR as u32, true));
    for (id, lba, len, _) in &entries {
        root.extend(dir_record(id.as_bytes(), *lba, *len, false));
    }
    assert!(
        root.len() <= SECTOR,
        "root directory larger than one sector"
    );

    let mut img = vec![0u8; total_sectors as usize * SECTOR];
    // Primary volume descriptor.
    {
        let p = &mut img[PVD_LBA * SECTOR..(PVD_LBA + 1) * SECTOR];
        p[0] = 1;
        p[1..6].copy_from_slice(b"CD001");
        p[6] = 1;
        p[8..40].copy_from_slice(&padded("", 32));
        p[40..72].copy_from_slice(&padded(label, 32));
        p[80..88].copy_from_slice(&both32(total_sectors));
        p[120..124].copy_from_slice(&both16(1));
        p[124..128].copy_from_slice(&both16(1));
        p[128..132].copy_from_slice(&both16(SECTOR as u16));
        p[132..140].copy_from_slice(&both32(10));
        p[140..144].copy_from_slice(&PATH_L_LBA.to_le_bytes());
        p[148..152].copy_from_slice(&PATH_M_LBA.to_be_bytes());
        p[156..190].copy_from_slice(&dir_record(&[0], ROOT_LBA, SECTOR as u32, true));
        p[190..318].copy_from_slice(&padded("", 128));
        p[318..446].copy_from_slice(&padded("LIGHTVM", 128));
        p[446..574].copy_from_slice(&padded("LIGHTVM-FIXTURE", 128));
        p[574..702].copy_from_slice(&padded("LIGHTVM-FIXTURE", 128));
        p[702..739].copy_from_slice(&padded("", 37));
        p[739..776].copy_from_slice(&padded("", 37));
        p[776..813].copy_from_slice(&padded("", 37));
        p[813..830].copy_from_slice(&volume_date());
        p[830..847].copy_from_slice(&volume_date());
        let mut never = [b'0'; 17];
        never[16] = 0;
        p[847..864].copy_from_slice(&never);
        p[864..881].copy_from_slice(&volume_date());
        p[881] = 1;
    }
    // Terminator.
    {
        let t = &mut img[(PVD_LBA + 1) * SECTOR..(PVD_LBA + 2) * SECTOR];
        t[0] = 255;
        t[1..6].copy_from_slice(b"CD001");
        t[6] = 1;
    }
    // Path tables: only the root (identifier 0x00, padded to even length).
    for (lba, be) in [(PATH_L_LBA, false), (PATH_M_LBA, true)] {
        let o = lba as usize * SECTOR;
        img[o] = 1;
        let extent = if be {
            ROOT_LBA.to_be_bytes()
        } else {
            ROOT_LBA.to_le_bytes()
        };
        img[o + 2..o + 6].copy_from_slice(&extent);
        let parent = if be {
            1u16.to_be_bytes()
        } else {
            1u16.to_le_bytes()
        };
        img[o + 6..o + 8].copy_from_slice(&parent);
    }
    let r = ROOT_LBA as usize * SECTOR;
    img[r..r + root.len()].copy_from_slice(&root);
    for (_, lba, _, data) in &entries {
        let o = *lba as usize * SECTOR;
        img[o..o + data.len()].copy_from_slice(data);
    }
    img
}

/// Volume label of an image written by [`write_iso`].
pub fn volume_label(img: &[u8]) -> Option<String> {
    let p = img.get(PVD_LBA * SECTOR..(PVD_LBA + 1) * SECTOR)?;
    (p[1..6] == *b"CD001").then(|| String::from_utf8_lossy(&p[40..72]).trim_end().to_owned())
}

/// Files of the root directory (name without `;1`, content): used to check
/// the images this module writes.
pub fn read_root(img: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let r = ROOT_LBA as usize * SECTOR;
    let dir = img.get(r..r + SECTOR)?;
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < dir.len() && dir[pos] != 0 {
        let len = usize::from(dir[pos]);
        let rec = dir.get(pos..pos + len)?;
        let lba = u32::from_le_bytes(rec[2..6].try_into().ok()?) as usize;
        let size = u32::from_le_bytes(rec[10..14].try_into().ok()?) as usize;
        let id_len = usize::from(rec[32]);
        let id = &rec[33..33 + id_len];
        if rec[25] & 0x02 == 0 {
            let name = String::from_utf8_lossy(id);
            let name = name.trim_end_matches(";1").to_owned();
            let data = img.get(lba * SECTOR..lba * SECTOR + size)?.to_vec();
            out.push((name, data));
        }
        pos += len;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn level2_preserves_short_images_and_windows_answer_file_name() {
        let files = [("TOOLS.EXE", b"fixture".as_slice())];
        assert_eq!(
            write_iso("TOOLS", &files),
            write_iso_level2("TOOLS", &files)
        );
        let image = write_iso_level2("TOOLS", &[("AUTOUNATTEND.XML", b"answer")]);
        assert_eq!(
            read_root(&image).unwrap(),
            vec![("AUTOUNATTEND.XML".into(), b"answer".to_vec())]
        );
    }
}
