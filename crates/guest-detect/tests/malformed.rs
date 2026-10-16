//! Crafted malformed media (ADR-0027 "Required evidence"): looping and
//! oversized structures end in a bounded, specific outcome, never a panic,
//! a hang or an allocation sized from the image.

mod common;

use std::io::Cursor;
use std::sync::mpsc;
use std::time::Duration;

use common::{
    SECTOR, UdfOpts, build_iso, build_udf, build_wim, finish_tag, put16, put32, udf_block_at,
    udf_grow, wim_xml,
};
use guest_detect::{DetectError, detect};
use lightvm_proto::{DetectedGuest, GuestOs};

/// [`detect`] on a worker thread; a loop that does not end fails the test.
fn run(img: Vec<u8>) -> Result<Option<DetectedGuest>, DetectError> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(detect(&mut Cursor::new(img)));
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("detection did not finish (hang or panic)")
}

fn le32(img: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(img[at..at + 4].try_into().unwrap())
}

fn windows_wim() -> Vec<u8> {
    build_wim(&wim_xml(6, 1, 7601, 9, "Professional", "Windows 7 Pro"))
}

fn is_windows7(r: &Result<Option<DetectedGuest>, DetectError>) -> bool {
    matches!(r, Ok(Some(g)) if g.os == GuestOs::Windows7)
}

// ---------------------------------------------------------------- ISO 9660

/// Root directory record of the primary volume descriptor.
const PVD_ROOT: usize = 16 * SECTOR + 156;

/// Both-endian 32-bit field.
fn set_both32(img: &mut [u8], at: usize, v: u32) {
    img[at..at + 4].copy_from_slice(&v.to_le_bytes());
    img[at + 4..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// Offset of the only directory record with identifier `id`.
fn iso_record(img: &[u8], id: &[u8]) -> usize {
    let pat: Vec<u8> = std::iter::once(id.len() as u8).chain(id.to_vec()).collect();
    let hits: Vec<usize> = img
        .windows(pat.len())
        .enumerate()
        .filter(|(_, w)| *w == pat.as_slice())
        .map(|(i, _)| i - 32)
        .collect();
    assert_eq!(hits.len(), 1, "record {id:?}");
    hits[0]
}

#[test]
fn iso_directory_pointing_to_the_root_is_followed_once() {
    let mut img = build_iso(&[("sources/readme.txt", b"x")], false);
    let root = le32(&img, PVD_ROOT + 2);
    let rec = iso_record(&img, b"SOURCES");
    set_both32(&mut img, rec + 2, root);
    // `sources` lists the root again: no install.wim, no recursion.
    assert!(matches!(run(img), Ok(None)));
}

#[test]
fn iso_oversized_directories_are_refused_before_reading() {
    let mut img = build_iso(&[("README.TXT", b"x")], false);
    set_both32(&mut img, PVD_ROOT + 10, u32::MAX);
    assert!(matches!(
        run(img),
        Err(DetectError::LimitExceeded("directory size"))
    ));

    let mut img = build_iso(&[("sources/readme.txt", b"x")], false);
    let rec = iso_record(&img, b"SOURCES");
    set_both32(&mut img, rec + 10, u32::MAX);
    assert!(matches!(
        run(img),
        Err(DetectError::LimitExceeded("directory size"))
    ));
}

/// Primary-only image with an empty root, whose volume descriptor set is
/// `n` copies of the primary descriptor without terminator, the root
/// directory (`root_len` bytes) right after it.
fn repeated_pvds(n: usize, root_len: u32) -> Vec<u8> {
    let base = build_iso(&[], false);
    let root_at = 16 + n;
    let mut img = vec![0u8; root_at * SECTOR + (root_len as usize).max(SECTOR)];
    let old_root = le32(&base, PVD_ROOT + 2) as usize;
    img[root_at * SECTOR..(root_at + 1) * SECTOR]
        .copy_from_slice(&base[old_root * SECTOR..(old_root + 1) * SECTOR]);
    let mut pvd = base[16 * SECTOR..17 * SECTOR].to_vec();
    set_both32(&mut pvd, 156 + 2, root_at as u32);
    set_both32(&mut pvd, 156 + 10, root_len);
    for i in 0..n {
        img[(16 + i) * SECTOR..(17 + i) * SECTOR].copy_from_slice(&pvd);
    }
    img
}

#[test]
fn iso_volume_descriptors_without_terminator_are_capped() {
    // 200 primary descriptors: only the first 64 become volumes.
    assert!(matches!(run(repeated_pvds(200, SECTOR as u32)), Ok(None)));
    // Volumes sharing a 4 MiB root (the directory cap) exhaust the 64 MiB
    // read budget instead of being read 64 times.
    assert!(matches!(
        run(repeated_pvds(200, 4 * 1024 * 1024)),
        Err(DetectError::LimitExceeded("bytes read"))
    ));
}

#[test]
fn iso_descriptor_past_the_cap_is_not_read() {
    // `n` boot records (type 0), then the primary descriptor at 16 + n.
    let with_boot_records = |n: usize| {
        let mut img = repeated_pvds(n + 1, SECTOR as u32);
        for i in 0..n {
            let o = (16 + i) * SECTOR;
            img[o..o + SECTOR].fill(0);
            img[o + 1..o + 6].copy_from_slice(b"CD001");
        }
        img
    };
    assert!(matches!(run(with_boot_records(63)), Ok(None)));
    assert!(matches!(
        run(with_boot_records(64)),
        Err(DetectError::NotAnImage)
    ));
}

#[test]
fn iso_directory_entry_budget_is_enforced() {
    // A root of `n` minimal records (34 bytes, 60 per sector). The budget
    // is per detection: the root is listed twice (sources, `.disk/info`).
    let image = |n: usize| {
        let mut img = build_iso(&[], false);
        let root_at = img.len() / SECTOR;
        let sectors = n.div_ceil(60);
        img.resize((root_at + sectors) * SECTOR, 0);
        for i in 0..n {
            let o = (root_at + i / 60) * SECTOR + (i % 60) * 34;
            img[o] = 34;
            img[o + 32] = 1;
            img[o + 33] = b'A';
        }
        set_both32(&mut img, PVD_ROOT + 2, root_at as u32);
        set_both32(&mut img, PVD_ROOT + 10, (sectors * SECTOR) as u32);
        img
    };
    assert!(matches!(run(image(4_000)), Ok(None)));
    assert!(matches!(
        run(image(10_001)),
        Err(DetectError::LimitExceeded("directory entries"))
    ));
}

// --------------------------------------------------------------------- WIM

/// Primary-only ISO 9660 image with `sources/install.wim`.
fn iso_with_wim(wim: &[u8]) -> Vec<u8> {
    build_iso(&[("sources/install.wim", wim)], false)
}

#[test]
fn wim_xml_size_near_max_is_capped() {
    // Declared XML size 2^56 - 1: the read stops at the end of the file.
    let mut wim = windows_wim();
    wim[72..79].fill(0xFF);
    assert!(is_windows7(&run(iso_with_wim(&wim))));

    // The file also claims 4 GiB: at most MAX_XML_BYTES is requested, past
    // the end of the image.
    let mut img = iso_with_wim(&wim);
    let rec = iso_record(&img, b"INSTALL.WIM;1");
    set_both32(&mut img, rec + 10, u32::MAX);
    assert!(matches!(
        run(img),
        Err(DetectError::Malformed("read beyond the end of the image"))
    ));
}

#[test]
fn wim_xml_offset_past_the_file_reads_nothing() {
    let mut wim = windows_wim();
    wim[80..88].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(run(iso_with_wim(&wim)), Ok(None)));
}

#[test]
fn wim_xml_beyond_the_cap_is_not_read() {
    // A comment pushes the <IMAGE> element past 1 MiB of UTF-16 XML.
    let padded = |chars: usize| {
        let xml = wim_xml(6, 1, 7601, 9, "Professional", "Windows 7 Pro");
        build_wim(&xml.replacen("<WIM>", &format!("<WIM><!--{}-->", "x".repeat(chars)), 1))
    };
    assert!(is_windows7(&run(iso_with_wim(&padded(400_000)))));
    assert!(matches!(run(iso_with_wim(&padded(600_000))), Ok(None)));
}

#[test]
fn wim_images_past_the_cap_are_not_examined() {
    let pe = "<IMAGE INDEX=\"1\"><WINDOWS><EDITIONID>WindowsPE</EDITIONID><VERSION>\
              <MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>19041</BUILD></VERSION></WINDOWS></IMAGE>";
    let xml = |n: usize| {
        let client = wim_xml(6, 1, 7601, 9, "Professional", "Windows 7 Pro");
        format!("<WIM>{}{}", pe.repeat(n), &client["<WIM>".len()..])
    };
    assert!(is_windows7(&run(iso_with_wim(&build_wim(&xml(63))))));
    assert!(matches!(run(iso_with_wim(&build_wim(&xml(64)))), Ok(None)));
}

// --------------------------------------------------------------------- UDF

fn windows_udf() -> Vec<u8> {
    build_udf(
        &[("sources/install.wim", &windows_wim())],
        UdfOpts::default(),
    )
}

/// Byte offset of the root file entry (from the file set descriptor at
/// block 0 of the partition).
fn udf_root_fe(img: &[u8]) -> usize {
    udf_block_at(le32(img, udf_block_at(0) + 404))
}

/// Short allocation descriptor: extent type (0 recorded, 3 continuation),
/// length, block.
fn short_ad(ty: u32, len: u32, lbn: u32) -> [u8; 8] {
    let mut ad = [0u8; 8];
    put32(&mut ad, 0, ty << 30 | len);
    put32(&mut ad, 4, lbn);
    ad
}

/// Replaces the root file entry's allocation descriptors with `ads`.
fn set_root_ads(img: &mut [u8], ads: &[[u8; 8]]) {
    let fe = udf_root_fe(img);
    let b = &mut img[fe..fe + SECTOR];
    put16(b, 34, 0);
    put32(b, 172, (ads.len() * 8) as u32);
    b[176..176 + ads.len() * 8].copy_from_slice(ads.as_flattened());
}

/// Writes an allocation extent descriptor holding `ads` at block `lbn`.
fn write_aed(img: &mut [u8], lbn: u32, ads: &[[u8; 8]]) {
    let o = udf_block_at(lbn);
    let b = &mut img[o..o + SECTOR];
    b.fill(0);
    put32(b, 20, (ads.len() * 8) as u32);
    b[24..24 + ads.len() * 8].copy_from_slice(ads.as_flattened());
    finish_tag(b, 258, lbn);
}

#[test]
fn udf_allocation_extent_descriptor_loops_are_capped() {
    let cont = |lbn| short_ad(3, SECTOR as u32, lbn);

    // An extent descriptor continuing into itself.
    let mut img = build_udf(&[], UdfOpts::default());
    let a = udf_grow(&mut img, 1);
    write_aed(&mut img, a, &[cont(a)]);
    set_root_ads(&mut img, &[cont(a)]);
    assert!(matches!(
        run(img),
        Err(DetectError::LimitExceeded(
            "UDF allocation extent descriptors"
        ))
    ));

    // Two descriptors continuing into each other.
    let mut img = build_udf(&[], UdfOpts::default());
    let a = udf_grow(&mut img, 2);
    write_aed(&mut img, a, &[cont(a + 1)]);
    write_aed(&mut img, a + 1, &[cont(a)]);
    set_root_ads(&mut img, &[cont(a)]);
    assert!(matches!(
        run(img),
        Err(DetectError::LimitExceeded(
            "UDF allocation extent descriptors"
        ))
    ));

    // A self loop adding 252 extents per turn hits the extent cap first.
    let mut img = build_udf(&[], UdfOpts::default());
    let a = udf_grow(&mut img, 1);
    let mut ads = vec![short_ad(0, SECTOR as u32, a); 252];
    ads.push(cont(a));
    write_aed(&mut img, a, &ads);
    set_root_ads(&mut img, &[cont(a)]);
    assert!(matches!(
        run(img),
        Err(DetectError::LimitExceeded("extents per file"))
    ));
}

#[test]
fn udf_directory_pointing_to_the_root_is_followed_once() {
    let mut img = build_udf(&[("sources/readme.txt", b"x")], UdfOpts::default());
    let root = le32(&img, udf_block_at(0) + 404);
    // File identifier of `sources` (8-bit d-string at offset 38).
    let at = img
        .windows(8)
        .position(|w| w == b"\x08sources")
        .expect("sources FID")
        - 38;
    put32(&mut img, at + 24, root);
    put16(&mut img, at + 28, 0);
    assert!(matches!(run(img), Ok(None)));
}

#[test]
fn udf_oversized_directory_is_refused_before_reading() {
    let mut img = build_udf(&[], UdfOpts::default());
    let fe = udf_root_fe(&img);
    img[fe + 56..fe + 64].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        run(img),
        Err(DetectError::LimitExceeded("directory size"))
    ));
}

#[test]
fn udf_partition_descriptors_are_capped() {
    // Main sequence moved to the free sectors 260..: `n` copies of the
    // partition descriptor, the logical volume descriptor, a terminator.
    let image = |n: usize| {
        let mut img = windows_udf();
        let (pd, lvd) = (257 * SECTOR, 258 * SECTOR);
        for i in 0..n {
            img.copy_within(pd..pd + SECTOR, (260 + i) * SECTOR);
        }
        img.copy_within(lvd..lvd + SECTOR, (260 + n) * SECTOR);
        finish_tag(&mut img[(261 + n) * SECTOR..(262 + n) * SECTOR], 8, 0);
        let avdp = 256 * SECTOR;
        put32(&mut img, avdp + 16, ((n + 2) * SECTOR) as u32);
        put32(&mut img, avdp + 20, 260);
        img
    };
    assert!(is_windows7(&run(image(8))));
    assert!(matches!(
        run(image(9)),
        Err(DetectError::LimitExceeded("UDF partitions"))
    ));
}

#[test]
fn udf_partition_maps_are_capped() {
    let lvd = 258 * SECTOR;
    for n in [9, u32::MAX] {
        let mut img = windows_udf();
        put32(&mut img, lvd + 268, n);
        assert!(matches!(
            run(img),
            Err(DetectError::LimitExceeded("UDF partition maps"))
        ));
    }
    let mut img = windows_udf();
    put32(&mut img, lvd + 264, u32::MAX);
    assert!(matches!(
        run(img),
        Err(DetectError::Malformed("UDF partition map table"))
    ));
}

#[test]
fn udf_volume_descriptor_sequence_without_terminator_is_capped() {
    // Sequence of maximal declared length at the end of the image: the
    // partition descriptor, fillers (implementation use, tag 4), the
    // logical volume descriptor at index `lvd_at`, no terminator.
    let image = |lvd_at: usize| {
        let mut img = windows_udf();
        let loc = img.len() / SECTOR;
        img.resize((loc + lvd_at + 1) * SECTOR, 0);
        img.copy_within(257 * SECTOR..258 * SECTOR, loc * SECTOR);
        for i in 1..lvd_at {
            let o = (loc + i) * SECTOR;
            finish_tag(&mut img[o..o + SECTOR], 4, (loc + i) as u32);
        }
        img.copy_within(258 * SECTOR..259 * SECTOR, (loc + lvd_at) * SECTOR);
        let avdp = 256 * SECTOR;
        for at in [16, 24] {
            put32(&mut img, avdp + at, u32::MAX);
            put32(&mut img, avdp + at + 4, loc as u32);
        }
        img
    };
    assert!(is_windows7(&run(image(63))));
    assert!(matches!(
        run(image(64)),
        Err(DetectError::Malformed(
            "UDF logical volume descriptor missing"
        ))
    ));
}
