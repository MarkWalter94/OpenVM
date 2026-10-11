//! `lightvm-fixture`: the deterministic test fixture of ADR-0019, by F3.
//!
//! - [`fixture_iso`]: the approved data medium with volume label
//!   [`VOLUME_LABEL`] containing `fx.sh` (`fixtures/lightvm-fx/fx.sh`),
//!   produced byte for byte reproducibly by a minimal ISO 9660 writer
//!   ([`iso9660`]): fixed timestamps, fixed order, no tool outside the
//!   repository. Built from the command line by `lightvm-fx-iso`
//!   (`fixtures/lightvm-fx/build.ps1`), imported as a data medium with
//!   `import_media` (ADR-0008).
//! - [`verify`]: the deterministic verifier of ADR-0019 on 24-bit RGB
//!   framebuffers (exact colours, connected boxes, no OCR, no approximate
//!   comparison).

pub mod iso9660;
pub mod verify;

/// Volume label of the fixture medium (`/media/ubuntu/LIGHTVM_FX` in the
/// live session, `mount -L LIGHTVM_FX`).
pub const VOLUME_LABEL: &str = "LIGHTVM_FX";

/// The fixture script, LF line endings enforced (a CRLF checkout would break
/// `sh`).
pub fn fx_sh() -> Vec<u8> {
    include_str!("../../../fixtures/lightvm-fx/fx.sh")
        .replace("\r\n", "\n")
        .into_bytes()
}

/// The fixture ISO: `FX.SH` (seen as `fx.sh` by Linux) in the root of an
/// ISO 9660 volume labelled [`VOLUME_LABEL`]. Same bytes on every call.
pub fn fixture_iso() -> Vec<u8> {
    iso9660::write_iso(VOLUME_LABEL, &[("FX.SH", &fx_sh())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_iso_is_reproducible_and_carries_the_script() {
        let a = fixture_iso();
        let b = fixture_iso();
        assert_eq!(a, b, "byte for byte reproducible");
        assert_eq!(&a[32769..32774], b"CD001");
        assert_eq!(a.len() % 2048, 0);
        let script = fx_sh();
        assert!(script.is_ascii());
        assert!(!script.contains(&b'\r'));
        let pos = a
            .windows(script.len())
            .position(|w| w == script.as_slice())
            .expect("script in the image");
        assert_eq!(pos % 2048, 0, "file data at a sector boundary");
        let files = iso9660::read_root(&a).unwrap();
        assert_eq!(files, vec![("FX.SH".to_owned(), script)]);
        assert_eq!(iso9660::volume_label(&a).unwrap(), VOLUME_LABEL);
    }
}
