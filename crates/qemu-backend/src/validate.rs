//! Validation of the QEMU option grammar (ROADMAP 6.3).
//!
//! Spawning without a shell is not enough: QEMU itself interprets the values of
//! `key=value,key=value` options (QemuOpts/keyval) and prefixes such as
//! `unix:`. Rules applied:
//!
//! - text values that go into an option (machine name, CPU model,
//!   identifiers) are **tokens** of a closed alphabet `[A-Za-z0-9._-]`, non
//!   empty, at most [`TOKEN_MAX_LEN`] long and never starting with `-`;
//! - any value that goes into an option separated by commas or `=` is
//!   rejected if it contains `,`, `=`, `:` or control characters
//!   ([`check_option_value`]);
//! - paths follow [`qemu_path_value`]: absolute, `/` separators, the only
//!   `:` allowed is the drive letter one on Windows.

use crate::error::{BackendError, Result};

/// Maximum length of a token.
pub const TOKEN_MAX_LEN: usize = 64;

/// Verifies that `s` is a token: `[A-Za-z0-9._-]{1,64}`, first character not `-`.
pub fn check_token(field: &'static str, s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(BackendError::invalid(field, "empty value"));
    }
    if s.len() > TOKEN_MAX_LEN {
        return Err(BackendError::invalid(
            field,
            format!("more than {TOKEN_MAX_LEN} characters"),
        ));
    }
    if s.starts_with('-') {
        return Err(BackendError::invalid(field, "starts with `-`"));
    }
    if let Some(c) = s
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(BackendError::invalid(
            field,
            format!("character {c:?} not allowed (alphabet [A-Za-z0-9._-])"),
        ));
    }
    Ok(())
}

/// Rejects a value meant for an option separated by commas or `=` if it
/// contains `,`, `=`, `:` or control characters.
pub fn check_option_value(field: &'static str, s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(BackendError::invalid(field, "empty value"));
    }
    if let Some(c) = s
        .chars()
        .find(|&c| matches!(c, ',' | '=' | ':') || c.is_control())
    {
        return Err(BackendError::invalid(
            field,
            format!("character {c:?} not allowed in QEMU options"),
        ));
    }
    Ok(())
}

/// Block protocols allowed in the QEMU command (ADR-0004, ADR-0010): local
/// files only.
pub const ALLOWED_BLOCK_PROTOCOLS: &[&str] = &["file"];

/// Remote block drivers present in the QEMU 11.1.0 build for Windows
/// (F0-02 §3) and always rejected.
pub const REMOTE_BLOCK_PROTOCOLS: &[&str] = &["http", "https", "ftp", "ftps", "nfs", "ssh", "nbd"];

/// Verifies that `name` is an allowed block protocol and returns the
/// canonical name. Remote drivers get a dedicated message.
pub fn check_block_protocol(name: &str) -> Result<&'static str> {
    if let Some(p) = ALLOWED_BLOCK_PROTOCOLS.iter().find(|p| **p == name) {
        return Ok(p);
    }
    if REMOTE_BLOCK_PROTOCOLS.contains(&name) {
        return Err(BackendError::invalid(
            "block_protocol",
            format!("remote protocol `{name}` excluded by the offline profile"),
        ));
    }
    Err(BackendError::invalid(
        "block_protocol",
        "block protocol not in the allowlist (only `file` is allowed)",
    ))
}

/// Maximum length of a Unix socket path (`sun_path`, 108 bytes including
/// the terminator on Windows and Linux).
pub const UNIX_SOCKET_PATH_MAX: usize = 107;

/// Characters forbidden in a path passed to QEMU, besides control
/// characters: option separators, protocol prefixes, metacharacters and
/// characters not valid in Windows file names.
const PATH_FORBIDDEN: &[char] = &[',', '=', ':', '"', '\'', '?', '*', '<', '>', '|', '\\'];

/// Converts an absolute path to the form used in the QEMU options and
/// validates it. On Windows: `X:/a/b` (drive letter, `/` separators); elsewhere
/// `/a/b`. Rejects empty components, `.` and `..`, UNC or device prefixes
/// and every character of [`PATH_FORBIDDEN`] outside the drive letter.
pub fn qemu_path_value(field: &'static str, raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        return Err(BackendError::invalid(
            field,
            "control character in the path",
        ));
    }
    let unified = raw.replace('\\', "/");
    let (prefix, rest) = split_root(field, &unified)?;
    if rest.is_empty() {
        return Err(BackendError::invalid(field, "the path is a root"));
    }
    for comp in rest.split('/') {
        match comp {
            "" => {
                return Err(BackendError::invalid(
                    field,
                    "empty component (double or trailing separators)",
                ));
            }
            "." | ".." => {
                return Err(BackendError::invalid(
                    field,
                    "`.` or `..` components not allowed",
                ));
            }
            _ => {}
        }
        if let Some(c) = comp.chars().find(|c| PATH_FORBIDDEN.contains(c)) {
            return Err(BackendError::invalid(
                field,
                format!("character {c:?} not allowed in the path"),
            ));
        }
        if comp.ends_with(' ') || comp.ends_with('.') {
            return Err(BackendError::invalid(
                field,
                "component ending with a space or a dot",
            ));
        }
    }
    Ok(format!("{prefix}{rest}"))
}

#[cfg(windows)]
fn split_root<'a>(field: &'static str, p: &'a str) -> Result<(String, &'a str)> {
    let b = p.as_bytes();
    if p.starts_with("//") {
        return Err(BackendError::invalid(
            field,
            "UNC or device paths not allowed",
        ));
    }
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
        let letter = (b[0] as char).to_ascii_uppercase();
        return Ok((format!("{letter}:/"), &p[3..]));
    }
    Err(BackendError::invalid(
        field,
        "expected an absolute path with a drive letter (X:\\...)",
    ))
}

#[cfg(not(windows))]
fn split_root<'a>(field: &'static str, p: &'a str) -> Result<(String, &'a str)> {
    if p.starts_with("//") {
        return Err(BackendError::invalid(field, "double leading separator"));
    }
    match p.strip_prefix('/') {
        Some(rest) => Ok(("/".to_owned(), rest)),
        None => Err(BackendError::invalid(field, "expected an absolute path")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token() {
        for ok in ["q35", "pc-q35-11.1", "qemu64", "Skylake-Client-v4", "a_b.c"] {
            assert!(check_token("t", ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-S",
            "a,b",
            "a=b",
            "a:b",
            "a b",
            "a\nb",
            "è",
            &"x".repeat(65),
        ] {
            assert!(check_token("t", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn block_protocols() {
        assert_eq!(check_block_protocol("file").unwrap(), "file");
        for p in REMOTE_BLOCK_PROTOCOLS {
            let e = check_block_protocol(p).unwrap_err().to_string();
            assert!(e.contains("remote"), "{p}: {e}");
        }
        for p in ["vvfat", "host_device", "FILE", "file,x", "", "iscsi"] {
            assert!(check_block_protocol(p).is_err(), "{p:?}");
        }
    }

    #[test]
    fn option_value() {
        assert!(check_option_value("v", "abc").is_ok());
        for bad in ["a,b", "a=b", "a:b", "a\u{0}b", "a\tb", ""] {
            assert!(check_option_value("v", bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths() {
        assert_eq!(
            qemu_path_value("p", r"e:\OpenVM\lab\vms\a b\x.qcow2").unwrap(),
            "E:/OpenVM/lab/vms/a b/x.qcow2"
        );
        for bad in [
            r"\\server\share\x",
            r"\\?\E:\x",
            r"\\.\PhysicalDrive0",
            r"relative\x",
            r"E:x",
            r"E:\a\..\b",
            r"E:\a\.\b",
            r"E:\a,b",
            r"E:\a=b",
            r"E:\a:b",
            r"E:\a\",
            r"E:\a\\b",
            "E:\\a\nb",
            r"E:\",
            r"E:\a.",
            r"E:\a\b ",
            r"E:\a?b",
        ] {
            assert!(qemu_path_value("p", bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_paths() {
        assert_eq!(qemu_path_value("p", "/srv/a b/x").unwrap(), "/srv/a b/x");
        for bad in ["rel/x", "//x", "/a/../b", "/a,b", "/a:b", "/"] {
            assert!(qemu_path_value("p", bad).is_err(), "{bad:?}");
        }
    }
}
