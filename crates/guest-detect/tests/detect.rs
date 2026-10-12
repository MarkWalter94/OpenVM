//! Detection on synthetic ISO 9660 / Joliet / UDF images, robustness on
//! mutated images, and (ignored) detection on real media.

mod common;

use std::io::Cursor;

use common::{Ad, SECTOR, UdfOpts, build_iso, build_udf, build_wim, wim_xml};
use guest_detect::{DetectError, detect, detect_path};
use lightvm_proto::{DetectedGuest, GuestArch, GuestOs};
use proptest::prelude::*;

fn run(img: &[u8]) -> Result<Option<DetectedGuest>, DetectError> {
    detect(&mut Cursor::new(img))
}

fn windows_udf(major: u32, minor: u32, build: u32, arch: u32, opts: UdfOpts) -> Vec<u8> {
    let wim = build_wim(&wim_xml(
        major,
        minor,
        build,
        arch,
        "Professional",
        "Windows Pro",
    ));
    build_udf(
        &[
            ("setup.exe", b"MZ"),
            ("boot/bcd", b"bcd"),
            ("sources/boot.wim", b"MSWIM\0\0\0"),
            ("sources/install.wim", &wim),
        ],
        opts,
    )
}

fn expect(img: &[u8], os: GuestOs, arch: GuestArch, build: Option<u32>) -> DetectedGuest {
    let g = run(img).expect("detect").expect("recognized");
    assert_eq!(g.os, os);
    assert_eq!(g.arch, Some(arch));
    assert_eq!(g.build, build);
    g
}

#[test]
fn windows_versions_through_udf() {
    let o = UdfOpts::default();
    let g = expect(
        &windows_udf(6, 0, 6002, 0, o),
        GuestOs::WindowsVista,
        GuestArch::X86,
        Some(6002),
    );
    assert_eq!(g.edition.unwrap().as_str(), "Windows Pro");
    expect(
        &windows_udf(6, 1, 7601, 9, o),
        GuestOs::Windows7,
        GuestArch::X86_64,
        Some(7601),
    );
    expect(
        &windows_udf(6, 3, 9600, 9, o),
        GuestOs::Windows81,
        GuestArch::X86_64,
        Some(9600),
    );
    expect(
        &windows_udf(10, 0, 19045, 9, o),
        GuestOs::Windows10,
        GuestArch::X86_64,
        Some(19045),
    );
    assert!(run(&windows_udf(10, 0, 22631, 9, o)).unwrap().is_none());
    assert!(run(&windows_udf(6, 2, 9200, 9, o)).unwrap().is_none());
}

#[test]
fn udf_variants() {
    let variants = [
        UdfOpts {
            efe: true,
            ..UdfOpts::default()
        },
        UdfOpts {
            ad: Ad::Long,
            ..UdfOpts::default()
        },
        UdfOpts {
            ad: Ad::Embedded,
            wide_names: true,
            ..UdfOpts::default()
        },
        UdfOpts {
            efe: true,
            metadata: true,
            ..UdfOpts::default()
        },
        UdfOpts {
            efe: true,
            metadata: true,
            ad: Ad::Embedded,
            wide_names: true,
        },
    ];
    for o in variants {
        expect(
            &windows_udf(6, 1, 7601, 9, o),
            GuestOs::Windows7,
            GuestArch::X86_64,
            Some(7601),
        );
    }
}

#[test]
fn udf_lookup_is_case_insensitive_and_accepts_esd() {
    let wim = build_wim(&wim_xml(10, 0, 19041, 0, "Core", "Windows 10 Home"));
    let img = build_udf(&[("SOURCES/Install.ESD", &wim)], UdfOpts::default());
    let g = expect(&img, GuestOs::Windows10, GuestArch::X86, Some(19041));
    assert_eq!(g.edition.unwrap().as_str(), "Windows 10 Home");
}

#[test]
fn install_wim_through_joliet() {
    let wim = build_wim(&wim_xml(6, 1, 7600, 0, "Ultimate", "Windows 7 Ultimate"));
    let img = build_iso(&[("sources/install.wim", &wim)], true);
    expect(&img, GuestOs::Windows7, GuestArch::X86, Some(7600));
}

#[test]
fn windows_xp_markers() {
    let img = build_iso(
        &[
            ("I386/TXTSETUP.SIF", b"[Version]"),
            ("WIN51", b"Windows"),
            ("WIN51IP", b"Windows"),
            ("WIN51IP.SP3", b"Windows"),
            ("SETUP.EXE", b"MZ"),
        ],
        true,
    );
    let g = expect(&img, GuestOs::WindowsXp, GuestArch::X86, Some(2600));
    assert_eq!(g.edition.unwrap().as_str(), "Windows XP Professional");

    // Primary hierarchy only (no Joliet).
    let img = build_iso(&[("I386/TXTSETUP.SIF", b"x"), ("WIN51IC", b"x")], false);
    let g = expect(&img, GuestOs::WindowsXp, GuestArch::X86, Some(2600));
    assert_eq!(g.edition.unwrap().as_str(), "Windows XP Home Edition");

    let img = build_iso(
        &[
            ("AMD64/TXTSETUP.SIF", b"x"),
            ("I386/X", b"x"),
            ("WIN52", b"x"),
        ],
        true,
    );
    expect(&img, GuestOs::WindowsXp, GuestArch::X86_64, Some(3790));

    // A marker without the setup directory is not enough.
    let img = build_iso(&[("WIN51", b"x"), ("README.TXT", b"x")], true);
    assert!(run(&img).unwrap().is_none());
}

#[test]
fn ubuntu_and_other_linux() {
    let info = b"Ubuntu 26.04.1 LTS \"Resolute Raccoon\" - Release amd64 (20260801)";
    let img = build_iso(
        &[
            (".disk/info", info),
            ("casper/vmlinuz", b"kernel"),
            ("boot/grub/grub.cfg", b"menu"),
        ],
        true,
    );
    let g = expect(&img, GuestOs::UbuntuLtsX86_64, GuestArch::X86_64, None);
    assert!(
        g.edition
            .unwrap()
            .as_str()
            .starts_with("Ubuntu 26.04.1 LTS")
    );

    let img = build_iso(
        &[(
            ".disk/info",
            b"Debian GNU/Linux 12.5.0 \"Bookworm\" - Official amd64 DVD Binary-1",
        )],
        true,
    );
    expect(&img, GuestOs::OtherLinuxX86_64, GuestArch::X86_64, None);

    // Without Joliet the primary names are mangled (`_DISK`): not found.
    let img = build_iso(&[(".disk/info", info)], false);
    assert!(run(&img).unwrap().is_none());

    let img = build_iso(&[(".disk/info", b"Ubuntu 24.04 LTS - Release arm64")], true);
    assert!(run(&img).unwrap().is_none());
}

#[test]
fn not_an_image_and_empty_media() {
    assert!(matches!(run(&[]), Err(DetectError::NotAnImage)));
    assert!(matches!(
        run(&vec![0u8; 300 * SECTOR]),
        Err(DetectError::NotAnImage)
    ));
    let img = build_iso(&[("README.TXT", b"hello")], true);
    assert!(run(&img).unwrap().is_none());
}

#[test]
fn malformed_wim_is_reported() {
    let img = build_udf(
        &[("sources/install.wim", b"NOTAWIM-at-all")],
        UdfOpts::default(),
    );
    assert!(matches!(run(&img), Err(DetectError::Malformed(_))));
    // Compressed XML resource.
    let mut wim = build_wim(&wim_xml(6, 1, 7601, 9, "Professional", "x"));
    wim[79] = 0x04;
    let img = build_udf(&[("sources/install.wim", &wim)], UdfOpts::default());
    assert!(matches!(run(&img), Err(DetectError::Unsupported(_))));
}

#[test]
fn truncated_images_do_not_panic() {
    let full = windows_udf(10, 0, 19045, 9, UdfOpts::default());
    for cut in (0..full.len()).step_by(SECTOR / 2) {
        let _ = run(&full[..cut]);
    }
}

fn mutate(mut img: Vec<u8>, edits: &[(usize, u8)]) -> Vec<u8> {
    // Edits land in the volume descriptors, the UDF descriptors and the
    // file system blocks, where they matter.
    let hot: Vec<std::ops::Range<usize>> = vec![
        16 * SECTOR..20 * SECTOR,
        256 * SECTOR..260 * SECTOR,
        300 * SECTOR..img.len(),
    ];
    for (i, &(pos, val)) in edits.iter().enumerate() {
        let r = &hot[i % hot.len()];
        if r.is_empty() || r.end > img.len() {
            continue;
        }
        let at = r.start + pos % (r.end - r.start);
        img[at] = val;
    }
    img
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn mutated_udf_images_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..48),
        meta in any::<bool>(),
        efe in any::<bool>(),
    ) {
        let o = UdfOpts { efe, metadata: meta, ..UdfOpts::default() };
        let img = mutate(windows_udf(6, 1, 7601, 9, o), &edits);
        let _ = run(&img);
    }

    #[test]
    fn mutated_iso_images_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..48),
    ) {
        let img = build_iso(
            &[("I386/TXTSETUP.SIF", b"x"), ("WIN51IP", b"x"), (".disk/info", b"Ubuntu 24.04 LTS amd64")],
            true,
        );
        let mut img = img;
        let len = img.len();
        for (pos, val) in edits {
            img[16 * SECTOR + pos % (len - 16 * SECTOR)] = val;
        }
        let _ = run(&img);
    }

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut img = vec![0u8; 16 * SECTOR];
        img.extend_from_slice(b"\x01CD001\x01");
        img.extend(bytes);
        let _ = run(&img);
    }
}

/// Runs [`detect_path`] on every `*.iso` in `LIGHTVM_DETECT_ISO_DIR` and
/// prints the result. Real media are large: the reader only seeks.
#[test]
#[ignore = "needs real ISO images in LIGHTVM_DETECT_ISO_DIR"]
fn real_isos() {
    let dir = std::env::var_os("LIGHTVM_DETECT_ISO_DIR").expect("LIGHTVM_DETECT_ISO_DIR not set");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso")))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no *.iso found");
    for p in paths {
        let start = std::time::Instant::now();
        let r = detect_path(&p);
        println!(
            "{}: {:?} ({} ms)",
            p.display(),
            r,
            start.elapsed().as_millis()
        );
    }
}
