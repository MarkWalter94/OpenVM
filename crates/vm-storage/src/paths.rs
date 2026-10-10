//! Managed paths, always relative to the catalog root.
//!
//! Rules (ROADMAP 6.3 and 7.4, row "Host"): rejection of absolute paths, `..`,
//! UNC, device paths, Windows reserved names, alternate data streams (`:`)
//! and, at resolution time, of any existing component that is a symbolic link
//! or a reparse point (junctions included).
//!
//! Declared limit: the link check happens before use; a substitution between
//! check and open by a process of the same user remains possible (TOCTOU).
//! The main defense is that the catalog directories are written only by the
//! supervisor.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{PathRejection, Result, StorageError};

const MAX_LEN: usize = 240;
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
];

/// Path relative to the catalog root, syntactically validated.
///
/// Canonical form: components separated by `/`, with no empty components.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ManagedPath(String);

impl ManagedPath {
    /// Validates a proposed string. Accepts `/` as separator; the backslash
    /// is rejected to avoid cross-platform ambiguity.
    pub fn parse(raw: &str) -> Result<Self> {
        let reject = |r| Err(StorageError::invalid_path(raw, r));
        if raw.is_empty() {
            return reject(PathRejection::Empty);
        }
        if raw.len() > MAX_LEN {
            return reject(PathRejection::TooLong);
        }
        if raw.starts_with("\\\\") || raw.starts_with("//") {
            return reject(PathRejection::UncOrDevice);
        }
        if raw.starts_with('/') || raw.starts_with('\\') {
            return reject(PathRejection::Absolute);
        }
        let b = raw.as_bytes();
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            return reject(PathRejection::Absolute);
        }
        for comp in raw.split('/') {
            if comp.is_empty() {
                return reject(PathRejection::InvalidCharacter);
            }
            if comp == ".." || comp == "." {
                return reject(PathRejection::Traversal);
            }
            if comp.chars().any(|c| {
                c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\')
            }) {
                return reject(PathRejection::InvalidCharacter);
            }
            if comp.ends_with('.') || comp.ends_with(' ') {
                return reject(PathRejection::TrailingDotOrSpace);
            }
            let stem = comp.split('.').next().unwrap_or(comp).trim_end();
            if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
                return reject(PathRejection::ReservedName);
            }
        }
        Ok(Self(raw.to_owned()))
    }

    /// Joins additional components, each one validated.
    pub fn join(&self, tail: &str) -> Result<Self> {
        Self::parse(&format!("{}/{}", self.0, tail))
    }

    /// Canonical text form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true` if the path is under `area` (for example `tmp`).
    pub fn is_under(&self, area: &str) -> bool {
        self.0
            .strip_prefix(area)
            .is_some_and(|rest| rest.starts_with('/'))
    }

    /// Components.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for ManagedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `true` if the metadata (without following links) indicate a symbolic link
/// or a reparse point (junction, mount point, etc.).
pub(crate) fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Resolves a managed path under `root`, checking that no existing component
/// (root included) is a link or a reparse point.
/// Components that do not exist yet are allowed (created later).
pub(crate) fn resolve_checked(root: &Path, rel: &ManagedPath) -> Result<PathBuf> {
    check_not_link(root, rel.as_str())?;
    let mut cur = root.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if is_link(&meta) {
                    return Err(StorageError::invalid_path(
                        rel.as_str(),
                        PathRejection::Link,
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(StorageError::io(&cur, e)),
        }
    }
    Ok(root.join(rel.as_str().replace('/', std::path::MAIN_SEPARATOR_STR)))
}

fn check_not_link(path: &Path, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if is_link(&meta) => Err(StorageError::invalid_path(label, PathRejection::Link)),
        Ok(_) => Ok(()),
        Err(e) => Err(StorageError::io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative() {
        for ok in [
            "media/abc.iso",
            "vms/x/base/b/disk.qcow2",
            "tmp/intent-1.part",
            "a",
        ] {
            assert!(ManagedPath::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_hostile() {
        let cases: &[(&str, PathRejection)] = &[
            ("", PathRejection::Empty),
            ("/etc/passwd", PathRejection::Absolute),
            ("\\Windows", PathRejection::Absolute),
            ("C:\\Windows\\x", PathRejection::Absolute),
            ("C:x", PathRejection::Absolute),
            ("c:/x", PathRejection::Absolute),
            ("\\\\server\\share\\x", PathRejection::UncOrDevice),
            ("//server/share/x", PathRejection::UncOrDevice),
            ("\\\\?\\C:\\x", PathRejection::UncOrDevice),
            ("\\\\.\\PhysicalDrive0", PathRejection::UncOrDevice),
            ("../x", PathRejection::Traversal),
            ("vms/../../x", PathRejection::Traversal),
            ("vms/./x", PathRejection::Traversal),
            ("vms\\..\\x", PathRejection::InvalidCharacter),
            ("vms//x", PathRejection::InvalidCharacter),
            ("vms/x/", PathRejection::InvalidCharacter),
            ("media/a.iso:stream", PathRejection::InvalidCharacter),
            ("media/a\u{0}b", PathRejection::InvalidCharacter),
            ("media/a*b", PathRejection::InvalidCharacter),
            ("media/NUL", PathRejection::ReservedName),
            ("media/con.txt", PathRejection::ReservedName),
            ("media/COM1.iso", PathRejection::ReservedName),
            ("media/x.", PathRejection::TrailingDotOrSpace),
            ("media/x ", PathRejection::TrailingDotOrSpace),
        ];
        for (raw, want) in cases {
            match ManagedPath::parse(raw) {
                Err(StorageError::InvalidPath { reason, .. }) => {
                    assert_eq!(reason, *want, "{raw:?}");
                }
                other => panic!("{raw:?} accepted: {other:?}"),
            }
        }
        let long = "a/".repeat(200) + "b";
        assert!(ManagedPath::parse(&long).is_err());
    }

    #[test]
    fn is_under_is_component_aware() {
        let p = ManagedPath::parse("tmp/x").unwrap();
        assert!(p.is_under("tmp"));
        assert!(!ManagedPath::parse("tmpx/y").unwrap().is_under("tmp"));
    }
}
