//! Paths read from VMware files: they must name regular files inside the
//! VM folder.
//!
//! [`check_vmx_path`] checks the text of the `.vmx` path the user chose
//! (absolute, local drive letter, no UNC or device path, no alternate data
//! stream, extension `.vmx`); [`resolve_in_folder`] resolves a file name
//! from a `.vmx` or a descriptor (relative to the folder of the file that
//! names it, or absolute) and refuses:
//!
//! - empty, overlong or control-character names, UNC (`\\server\share`),
//!   device (`\\.\`, `\\?\`) and drive-relative (`C:x`) paths, alternate
//!   data streams, Windows device names (`CON`, `NUL`, `COM1`...);
//! - `.` and `..` components (no traversal, even one that would stay
//!   inside the folder);
//! - absolute paths that are not inside the VM folder;
//! - a link, junction or other reparse point on **any** component between
//!   the VM folder and the file, and anything but a regular file at the
//!   end.
//!
//! Comparisons with the folder are per component, without case on Windows.

use std::path::{Component, Path, PathBuf};

use crate::ImportError;

/// Longest path accepted.
pub const MAX_PATH_BYTES: usize = 4096;

const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$", "CLOCK$",
];

/// `true` for a symbolic link or, on Windows, any reparse point (junctions,
/// mount points, cloud placeholders, deduplicated files).
#[must_use]
pub fn is_link(meta: &std::fs::Metadata) -> bool {
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

/// Textual checks of the `.vmx` path chosen by the user (no file system
/// access).
pub fn check_vmx_path(raw: &str) -> Result<PathBuf, ImportError> {
    if raw.is_empty() || raw.len() > MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(ImportError::InvalidPath(
            "empty, too long or control characters",
        ));
    }
    let p = check_text(raw).map_err(ImportError::InvalidPath)?;
    if !p.is_absolute() {
        return Err(ImportError::InvalidPath("an absolute path is required"));
    }
    let vmx = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("vmx"));
    if !vmx {
        return Err(ImportError::InvalidPath("only .vmx files can be imported"));
    }
    Ok(p)
}

/// Common textual rules of a path: UNC/device/stream/reserved names,
/// `.`/`..`, trailing dot or space.
fn check_text(raw: &str) -> Result<PathBuf, &'static str> {
    let b = raw.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    if b.len() >= 2 && sep(b[0]) && sep(b[1]) {
        return Err("UNC, network and device paths are not allowed");
    }
    if cfg!(windows) {
        if b.first() == Some(&b'\\') && b.get(1) == Some(&b'?') {
            return Err("device paths are not allowed");
        }
        // A `:` only as the drive letter of an absolute path.
        let colon_ok =
            |i: usize| i == 1 && b[0].is_ascii_alphabetic() && b.get(2).is_some_and(|c| sep(*c));
        if b.iter()
            .enumerate()
            .any(|(i, c)| *c == b':' && !colon_ok(i))
        {
            return Err("alternate data streams and drive-relative paths are not allowed");
        }
    }
    let path = PathBuf::from(if cfg!(windows) {
        raw.replace('/', "\\")
    } else {
        raw.to_owned()
    });
    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                if !matches!(p.kind(), std::path::Prefix::Disk(_)) {
                    return Err("UNC, network and device paths are not allowed");
                }
            }
            Component::RootDir => {}
            Component::CurDir | Component::ParentDir => {
                return Err("`.` and `..` are not allowed");
            }
            Component::Normal(n) => {
                let n = n.to_str().ok_or("not valid text")?;
                if cfg!(windows) {
                    let stem = n.split('.').next().unwrap_or(n).trim_end();
                    if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
                        return Err("device names are not allowed");
                    }
                    if n.ends_with('.') || n.ends_with(' ') {
                        return Err("names ending with a dot or a space are not allowed");
                    }
                }
            }
        }
    }
    Ok(path)
}

/// Same path, per component (without case on Windows).
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| {
                let s = c.as_os_str().to_string_lossy().into_owned();
                if cfg!(windows) { s.to_lowercase() } else { s }
            })
            .collect()
    };
    norm(a) == norm(b)
}

/// Relative part of `p` below `dir` (per component, without case on
/// Windows), `None` if `p` is not strictly below `dir`.
#[must_use]
pub fn below(p: &Path, dir: &Path) -> Option<Vec<std::ffi::OsString>> {
    let eq = |a: &std::ffi::OsStr, b: &std::ffi::OsStr| {
        if cfg!(windows) {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        } else {
            a == b
        }
    };
    let mut pc = p.components();
    for dc in dir.components() {
        let c = pc.next()?;
        if !eq(c.as_os_str(), dc.as_os_str()) {
            return None;
        }
    }
    let rest: Vec<_> = pc.map(|c| c.as_os_str().to_owned()).collect();
    (!rest.is_empty()).then_some(rest)
}

/// Resolves `raw` (from a `.vmx` or a descriptor in `base_dir`) to a regular
/// file inside `folder` (see the module rules). `base_dir` must itself be
/// `folder` or below it.
pub fn resolve_in_folder(
    folder: &Path,
    base_dir: &Path,
    raw: &str,
) -> Result<PathBuf, ImportError> {
    // Messages name the file only, never the folders the text names.
    let last = raw
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("(no name)");
    let shown = crate::shown(last);
    let outside = || ImportError::OutsideFolder(shown.clone());
    if raw.is_empty() || raw.len() > MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(outside());
    }
    let text = check_text(raw).map_err(|_| outside())?;
    let candidate = if text.is_absolute() {
        text
    } else {
        if text.has_root() || matches!(text.components().next(), Some(Component::Prefix(_))) {
            // `\x` (root of the current drive) or `C:x`.
            return Err(outside());
        }
        base_dir.join(text)
    };
    let rest = below(&candidate, folder).ok_or_else(outside)?;
    // Walk from the folder down, never following a link.
    let mut cur = folder.to_path_buf();
    let last = rest.len() - 1;
    for (i, part) in rest.iter().enumerate() {
        cur.push(part);
        let meta = match std::fs::symlink_metadata(&cur) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ImportError::NotFound(crate::file_name(&cur)));
            }
            Err(_) => return Err(ImportError::Io(crate::file_name(&cur))),
        };
        if is_link(&meta) {
            return Err(ImportError::Link(crate::file_name(&cur)));
        }
        if i < last && !meta.is_dir() {
            return Err(ImportError::NotAFile(crate::file_name(&cur)));
        }
        if i == last && !meta.is_file() {
            return Err(ImportError::NotAFile(crate::file_name(&cur)));
        }
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmx_path_text() {
        if cfg!(windows) {
            assert!(check_vmx_path(r"D:\VMs\Win\Win.vmx").is_ok());
            assert!(check_vmx_path("D:/VMs/Win/Win.VMX").is_ok());
            for bad in [
                r"\\server\share\Win.vmx",
                r"\\?\D:\VMs\Win.vmx",
                r"\\.\PhysicalDrive0",
                r"D:\VMs\Win.vmx:stream",
                r"D:Win.vmx",
                r"VMs\Win.vmx",
                r"D:\VMs\..\Win.vmx",
                r"D:\VMs\CON\Win.vmx",
                r"D:\VMs\Win.vmdk",
                r"D:\VMs\Win \a.vmx",
                r"D:\VMs\Win.\a.vmx",
                "",
            ] {
                assert!(check_vmx_path(bad).is_err(), "{bad}");
            }
        } else {
            assert!(check_vmx_path("/vms/win/win.vmx").is_ok());
            assert!(check_vmx_path("//server/x.vmx").is_err());
            assert!(check_vmx_path("/vms/../x.vmx").is_err());
        }
    }

    #[test]
    fn below_folder() {
        let f = Path::new(if cfg!(windows) {
            r"D:\VMs\Win"
        } else {
            "/vms/win"
        });
        let inside = f.join("sub").join("a.vmdk");
        assert_eq!(below(&inside, f).map(|v| v.len()), Some(2));
        assert!(below(f, f).is_none());
        let sibling = f.parent().unwrap().join("Win2").join("a.vmdk");
        assert!(below(&sibling, f).is_none());
        if cfg!(windows) {
            assert!(below(Path::new(r"d:\vms\WIN\a.vmdk"), f).is_some());
        }
    }
}
