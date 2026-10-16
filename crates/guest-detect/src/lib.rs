//! Guest OS detection from installation ISO images (ADR-0027).
//!
//! [`detect`] reads an approved ISO (untrusted bytes) through `Read + Seek`
//! and identifies Windows XP, Vista, 7, 8.1, 10 and 11 ARM64 and Ubuntu / other Linux
//! media:
//!
//! - Windows Vista and later: `sources/install.wim` or `sources/install.esd`,
//!   found through UDF (modern media) or ISO 9660 / Joliet; version,
//!   architecture and edition come from the WIM XML metadata.
//! - Windows XP: `I386/` or `AMD64/` and a `WIN51*` / `WIN52*` marker in the
//!   root of the ISO 9660 / Joliet hierarchy.
//! - Linux: `.disk/info`.
//!
//! Windows 11 (build 22000 and later) supports x64 and ARM64 media
//! (ADR-0035/0028). Windows 8 (6.2), Windows 11 x86, Windows 10 on ARM and
//! server editions are recognized and reported as `Ok(None)`: they are not
//! supported guests. The result is a
//! hint for the wizard, never a guarantee.
//!
//! The work is bounded: bytes read, directory entries visited, directory
//! sizes, extents per file and XML bytes are capped, and malformed input
//! yields a [`DetectError`] or `Ok(None)`, never a panic.

use std::fmt;
use std::io::{Read, Seek};
use std::path::Path;

use lightvm_proto::{DetectedGuest, GuestArch, GuestOs, Label};

mod disk;
mod fs;
mod iso9660;
mod udf;
mod wim;

/// Synthetic image builders (ISO 9660, UDF, WIM) shared with the tests of
/// other crates; not part of the API.
#[cfg(feature = "test-images")]
#[doc(hidden)]
#[path = "../tests/common/mod.rs"]
pub mod testing;

use disk::Disk;
use fs::{Fs, find, name_eq};

/// Longest edition label produced (characters).
pub const MAX_EDITION_CHARS: usize = 128;
/// Upper bound on the bytes of `.disk/info` read.
const MAX_DISK_INFO_BYTES: u64 = 4096;
/// First Windows 11 build.
const WINDOWS_11_FIRST_BUILD: u32 = 22000;

/// Why an image could not be examined.
#[derive(Debug)]
#[non_exhaustive]
pub enum DetectError {
    /// Reading the image failed.
    Io(std::io::Error),
    /// Neither an ISO 9660 nor a UDF volume.
    NotAnImage,
    /// A structure on the image is inconsistent.
    Malformed(&'static str),
    /// A valid structure this reader does not handle.
    Unsupported(&'static str),
    /// A work or size bound was reached.
    LimitExceeded(&'static str),
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DetectError::Io(e) => write!(f, "I/O error reading the image: {e}"),
            DetectError::NotAnImage => f.write_str("not an ISO 9660 or UDF image"),
            DetectError::Malformed(what) => write!(f, "malformed image: {what}"),
            DetectError::Unsupported(what) => write!(f, "unsupported image feature: {what}"),
            DetectError::LimitExceeded(what) => write!(f, "detection limit reached: {what}"),
        }
    }
}

impl std::error::Error for DetectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DetectError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for DetectError {
    fn from(e: std::io::Error) -> Self {
        DetectError::Io(e)
    }
}

/// Outcome of examining one file system.
#[derive(Debug)]
enum Decision {
    /// A supported guest.
    Found(DetectedGuest),
    /// A recognized but unsupported system (Windows 8, Windows 11 x86, ...).
    Unsupported,
    /// Nothing recognized: try the next file system.
    Unknown,
}

/// Detects the guest OS of the installation image read from `r`.
///
/// `Ok(None)` means "not recognized or not supported". UDF is examined
/// first, then the Joliet and primary ISO 9660 hierarchies; the first
/// answer wins. An error is returned when no file system answers and one
/// of them failed (malformed, unsupported or over a bound), or when the
/// image holds neither file system ([`DetectError::NotAnImage`]). The
/// reader is only seeked and read; its position afterwards is unspecified.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<Option<DetectedGuest>, DetectError> {
    let mut disk = Disk::new(r)?;
    let mut first_err = None;
    let mut examined = false;

    match udf::probe(&mut disk) {
        Ok(Some(fs)) => {
            examined = true;
            match detect_in(&fs, &mut disk, false) {
                Ok(Decision::Found(g)) => return Ok(Some(g)),
                Ok(Decision::Unsupported) => return Ok(None),
                Ok(Decision::Unknown) => {}
                Err(e) => first_err = Some(e),
            }
        }
        Ok(None) => {}
        Err(e) => first_err = Some(e),
    }
    match iso9660::probe(&mut disk) {
        Ok(volumes) => {
            for fs in &volumes {
                match detect_in(fs, &mut disk, true) {
                    Ok(Decision::Found(g)) => return Ok(Some(g)),
                    Ok(Decision::Unsupported) => return Ok(None),
                    Ok(Decision::Unknown) => examined = true,
                    Err(e) => {
                        first_err.get_or_insert(e);
                    }
                }
            }
        }
        Err(e) => {
            first_err.get_or_insert(e);
        }
    }
    match (examined, first_err) {
        // A failure on one file system outweighs "nothing found" on another:
        // the media may hold a guest this reader could not reach.
        (_, Some(e)) => Err(e),
        (true, None) => Ok(None),
        (false, None) => Err(DetectError::NotAnImage),
    }
}

/// [`detect`] on the file at `path`.
pub fn detect_path(path: &Path) -> Result<Option<DetectedGuest>, DetectError> {
    let mut f = std::fs::File::open(path)?;
    detect(&mut f)
}

fn detect_in<F: Fs, R: Read + Seek>(
    fs: &F,
    disk: &mut Disk<'_, R>,
    iso9660: bool,
) -> Result<Decision, DetectError> {
    let root = fs.list(disk, &fs.root())?;

    // Windows Vista and later.
    if let Some(sources) = root
        .iter()
        .find(|e| e.is_dir && name_eq(&e.name, "sources"))
    {
        let entries = fs.list(disk, &sources.node)?;
        let wim = ["install.wim", "install.esd"]
            .iter()
            .find_map(|n| entries.iter().find(|e| !e.is_dir && name_eq(&e.name, n)));
        if let Some(wim) = wim {
            let data = fs.open(disk, &wim.node)?;
            return Ok(match wim::read_image_info(disk, &data)? {
                Some(info) => windows_decision(&info),
                None => Decision::Unknown,
            });
        }
    }

    // Windows XP (ISO 9660 / Joliet media only).
    if iso9660 && let Some(d) = xp_decision(root.iter().map(|e| (e.name.as_str(), e.is_dir))) {
        return Ok(d);
    }

    // Linux.
    if let Some(info) = find(fs, disk, &[".disk", "info"], false)? {
        let data = fs.open(disk, &info)?;
        let bytes = data.read(disk, 0, MAX_DISK_INFO_BYTES)?;
        return Ok(linux_decision(&String::from_utf8_lossy(&bytes)));
    }
    Ok(Decision::Unknown)
}

/// A short single-line label from untrusted text, within the [`Label`]
/// rules.
fn label(s: &str) -> Option<Label> {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let short: String = cleaned.chars().take(MAX_EDITION_CHARS).collect();
    Label::parse(short.trim()).ok()
}

fn windows_decision(info: &wim::ImageInfo) -> Decision {
    let (Some(major), Some(minor)) = (info.major, info.minor) else {
        return Decision::Unknown;
    };
    let server = [&info.installation_type, &info.edition_id].iter().any(|v| {
        v.as_deref()
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("server"))
    });
    if server {
        return Decision::Unsupported;
    }
    let (os, arch) = match (major, minor) {
        (6, 0) => (GuestOs::WindowsVista, x86_arch(info.arch)),
        (6, 1) => (GuestOs::Windows7, x86_arch(info.arch)),
        (6, 3) => (GuestOs::Windows81, x86_arch(info.arch)),
        (10, 0) => match info.build {
            Some(b) if b < WINDOWS_11_FIRST_BUILD => (GuestOs::Windows10, x86_arch(info.arch)),
            // Windows 11 requires a known 64-bit architecture (ADR-0028/0035).
            Some(_) if info.arch == Some(WIM_ARCH_ARM64) => {
                (GuestOs::Windows11Arm64, Ok(Some(GuestArch::Aarch64)))
            }
            Some(_) if info.arch == Some(9) => (GuestOs::Windows11X64, Ok(Some(GuestArch::X86_64))),
            _ => return Decision::Unsupported,
        },
        // 6.2 is Windows 8; anything else is not a supported desktop guest.
        _ => return Decision::Unsupported,
    };
    let Ok(arch) = arch else {
        return Decision::Unsupported;
    };
    let edition = info
        .display_name
        .as_deref()
        .or(info.name.as_deref())
        .and_then(label)
        .or_else(|| {
            info.edition_id
                .as_deref()
                .and_then(|e| label(&format!("Windows {e}")))
        });
    Decision::Found(DetectedGuest {
        os,
        arch,
        build: info.build,
        edition,
    })
}

/// WIM `<ARCH>` value of ARM64 media (`PROCESSOR_ARCHITECTURE_ARM64`).
const WIM_ARCH_ARM64: u32 = 12;

/// Architecture of an x86 Windows release from the WIM `<ARCH>` value;
/// `Err` for ARM, ARM64 and Itanium (Windows 10 on ARM and older ARM
/// releases are not supported guests).
fn x86_arch(arch: Option<u32>) -> Result<Option<GuestArch>, ()> {
    match arch {
        Some(0) => Ok(Some(GuestArch::X86)),
        Some(9) => Ok(Some(GuestArch::X86_64)),
        None => Ok(None),
        Some(_) => Err(()),
    }
}

/// Windows XP markers: `I386/` with `WIN51*` (x86), `AMD64/` with
/// `WIN52*` (XP x64, NT 5.2).
fn xp_decision<'a>(root: impl Iterator<Item = (&'a str, bool)>) -> Option<Decision> {
    let mut i386 = false;
    let mut amd64 = false;
    let mut win51: Option<String> = None;
    let mut win52 = false;
    for (name, is_dir) in root {
        let upper = name.to_ascii_uppercase();
        if is_dir {
            i386 |= upper == "I386";
            amd64 |= upper == "AMD64";
        } else if upper.starts_with("WIN51") {
            // Prefer the most specific marker (WIN51IP over WIN51).
            if win51.as_ref().is_none_or(|w| w.len() < upper.len()) {
                win51 = Some(upper);
            }
        } else if upper.starts_with("WIN52") {
            win52 = true;
        }
    }
    if amd64 && win52 {
        return Some(Decision::Found(DetectedGuest {
            os: GuestOs::WindowsXp,
            arch: Some(GuestArch::X86_64),
            build: Some(3790),
            edition: label("Windows XP Professional x64 Edition"),
        }));
    }
    if i386 && let Some(marker) = win51 {
        let edition = if marker.starts_with("WIN51IP") {
            "Windows XP Professional"
        } else if marker.starts_with("WIN51IC") {
            "Windows XP Home Edition"
        } else {
            "Windows XP"
        };
        return Some(Decision::Found(DetectedGuest {
            os: GuestOs::WindowsXp,
            arch: Some(GuestArch::X86),
            build: Some(2600),
            edition: label(edition),
        }));
    }
    None
}

/// `.disk/info`: Ubuntu LTS amd64, another x86_64 Linux, or unknown.
fn linux_decision(info: &str) -> Decision {
    let line = info.lines().next().unwrap_or("").trim();
    let x86_64 = line.contains("amd64") || line.contains("x86_64");
    if !x86_64 {
        return Decision::Unknown;
    }
    let os = if line.starts_with("Ubuntu") && line.contains("LTS") {
        GuestOs::UbuntuLtsX86_64
    } else {
        GuestOs::OtherLinuxX86_64
    };
    Decision::Found(DetectedGuest {
        os,
        arch: Some(GuestArch::X86_64),
        build: None,
        edition: label(line),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(xml: &str) -> wim::ImageInfo {
        wim::first_install_image(xml).expect("image")
    }

    fn found(d: Decision) -> DetectedGuest {
        match d {
            Decision::Found(g) => g,
            other => panic!("expected a guest, got {other:?}"),
        }
    }

    #[test]
    fn windows_versions_map_to_guests() {
        let xml = |maj, min, build, arch| {
            format!(
                "<WIM><IMAGE INDEX=\"1\"><WINDOWS><ARCH>{arch}</ARCH><EDITIONID>Professional</EDITIONID>\
                 <VERSION><MAJOR>{maj}</MAJOR><MINOR>{min}</MINOR><BUILD>{build}</BUILD></VERSION>\
                 </WINDOWS><NAME>Windows X Pro</NAME></IMAGE></WIM>"
            )
        };
        let g = found(windows_decision(&image(&xml(6, 1, 7601, 9))));
        assert_eq!(g.os, GuestOs::Windows7);
        assert_eq!(g.arch, Some(GuestArch::X86_64));
        assert_eq!(g.build, Some(7601));
        assert_eq!(g.edition.unwrap().as_str(), "Windows X Pro");
        assert_eq!(
            found(windows_decision(&image(&xml(6, 0, 6002, 0)))).os,
            GuestOs::WindowsVista
        );
        assert_eq!(
            found(windows_decision(&image(&xml(6, 3, 9600, 9)))).os,
            GuestOs::Windows81
        );
        assert_eq!(
            found(windows_decision(&image(&xml(10, 0, 19045, 9)))).os,
            GuestOs::Windows10
        );
        assert_eq!(
            found(windows_decision(&image(&xml(10, 0, 22631, 9)))).os,
            GuestOs::Windows11X64
        );
        assert!(matches!(
            windows_decision(&image(&xml(6, 2, 9200, 9))),
            Decision::Unsupported
        ));
        assert!(matches!(
            windows_decision(&image(&xml(10, 0, 19045, 12))),
            Decision::Unsupported
        ));
        let g = found(windows_decision(&image(&xml(10, 0, 26200, 12))));
        assert_eq!(g.os, GuestOs::Windows11Arm64);
        assert_eq!(g.arch, Some(GuestArch::Aarch64));
        assert_eq!(g.build, Some(26200));
        assert_eq!(g.edition.unwrap().as_str(), "Windows X Pro");
        assert_eq!(
            found(windows_decision(&image(&xml(10, 0, 22000, 12)))).os,
            GuestOs::Windows11Arm64
        );
        // Windows 11 x86 and Windows 11 with unknown architecture.
        assert!(matches!(
            windows_decision(&image(&xml(10, 0, 22000, 0))),
            Decision::Unsupported
        ));
        let no_arch = "<WIM><IMAGE INDEX=\"1\"><WINDOWS><EDITIONID>Professional</EDITIONID>\
                       <VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26200</BUILD></VERSION>\
                       </WINDOWS></IMAGE></WIM>";
        assert!(matches!(
            windows_decision(&image(no_arch)),
            Decision::Unsupported
        ));
        // ARM (32-bit) Windows 11 does not exist as a guest either.
        assert!(matches!(
            windows_decision(&image(&xml(10, 0, 26200, 5))),
            Decision::Unsupported
        ));
        // Windows 8 on ARM64 stays Windows 8.
        assert!(matches!(
            windows_decision(&image(&xml(6, 2, 9200, 12))),
            Decision::Unsupported
        ));
    }

    #[test]
    fn server_editions_are_unsupported_on_arm64() {
        let xml = "<WIM><IMAGE INDEX=\"1\"><WINDOWS><ARCH>12</ARCH><EDITIONID>ServerDatacenter</EDITIONID>\
                   <INSTALLATIONTYPE>Server</INSTALLATIONTYPE>\
                   <VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION>\
                   </WINDOWS></IMAGE></WIM>";
        assert!(matches!(
            windows_decision(&image(xml)),
            Decision::Unsupported
        ));
    }

    #[test]
    fn windows_pe_images_are_skipped() {
        let xml = "<WIM><IMAGE INDEX=\"1\"><WINDOWS><EDITIONID>WindowsPE</EDITIONID><VERSION><MAJOR>10</MAJOR>\
                   <MINOR>0</MINOR><BUILD>19041</BUILD></VERSION></WINDOWS></IMAGE>\
                   <IMAGE INDEX=\"2\"><WINDOWS><EDITIONID>Core</EDITIONID><VERSION><MAJOR>10</MAJOR>\
                   <MINOR>0</MINOR><BUILD>19041</BUILD></VERSION></WINDOWS><DISPLAYNAME>Windows 10 Home</DISPLAYNAME></IMAGE></WIM>";
        let info = image(xml);
        assert_eq!(info.edition_id.as_deref(), Some("Core"));
        assert_eq!(info.display_name.as_deref(), Some("Windows 10 Home"));
    }

    #[test]
    fn labels_are_sanitized_and_bounded() {
        assert_eq!(label("  a\tb\u{0}c  ").unwrap().as_str(), "a b c");
        assert_eq!(
            label(&"x".repeat(1000)).unwrap().as_str().len(),
            MAX_EDITION_CHARS
        );
        assert!(label("\u{1}\u{2}").is_none());
    }

    #[test]
    fn disk_info_rules() {
        let g = found(linux_decision(
            "Ubuntu 24.04.1 LTS \"Noble Numbat\" - Release amd64 (20240827.1)\n",
        ));
        assert_eq!(g.os, GuestOs::UbuntuLtsX86_64);
        let g = found(linux_decision(
            "Ubuntu 25.10 \"Questing Quokka\" - Release amd64",
        ));
        assert_eq!(g.os, GuestOs::OtherLinuxX86_64);
        let g = found(linux_decision(
            "Debian GNU/Linux 12.5.0 \"Bookworm\" - Official amd64 DVD Binary-1",
        ));
        assert_eq!(g.os, GuestOs::OtherLinuxX86_64);
        assert!(matches!(
            linux_decision("Ubuntu 24.04 LTS - Release arm64"),
            Decision::Unknown
        ));
    }

    #[test]
    fn xp_markers() {
        let g = found(
            xp_decision([("I386", true), ("WIN51", false), ("WIN51IP", false)].into_iter())
                .unwrap(),
        );
        assert_eq!(g.os, GuestOs::WindowsXp);
        assert_eq!(g.arch, Some(GuestArch::X86));
        assert_eq!(g.edition.unwrap().as_str(), "Windows XP Professional");
        let g = found(
            xp_decision([("AMD64", true), ("I386", true), ("WIN52", false)].into_iter()).unwrap(),
        );
        assert_eq!(g.arch, Some(GuestArch::X86_64));
        assert!(xp_decision([("I386", true)].into_iter()).is_none());
        assert!(xp_decision([("WIN51", false)].into_iter()).is_none());
    }
}
