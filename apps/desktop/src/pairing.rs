//! Pairing file for agent clients (F4, ADR-0006).
//!
//! An agent client (the LightVM MCP adapter, F5) connects to the supervisor
//! with a **pairing credential** issued on the user's request
//! (`create_pairing_credential`). The credential grants no VM permission:
//! it only lets the client ask for a session, which the user then approves
//! in the app with a verification code.
//!
//! The credential is never shown, never copied to the clipboard (the app
//! has no clipboard features) and never logged. It goes to a file only the
//! current user can read, where the adapter reads it:
//!
//! - Windows: `%LOCALAPPDATA%\LightVM\pairing\pairing.json`; the `pairing`
//!   folder has its inheritance removed and a single entry granting full
//!   control to the current user's SID (`icacls.exe` from `System32`, the
//!   procedure of the supervisor's private run folders, F0-02 §4.3), checked
//!   after the change; the file inherits it.
//! - Unix: `$XDG_RUNTIME_DIR/lightvm-pairing/pairing.json` (folder `0700`,
//!   file `0600`), or `~/.local/state/lightvm/pairing/` without a runtime
//!   folder.
//!
//! Content (JSON, `format` = [`FORMAT`]): `endpoint` (the supervisor's IPC
//! endpoint, not a secret), `credential` (64 hexadecimal digits) and
//! `expires_at` (UTC). The credential expires after 10 minutes, does not
//! survive a supervisor restart and the file is removed when agent access is
//! turned off and when the app stops the supervisor. Same-user processes
//! can read it, as they can already reach the endpoint: the threat model
//! (ROADMAP 7.1) leaves them out of the guarantees.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use lightvm_proto::UtcTimestamp;

/// Name of the pairing file.
pub const FILE_NAME: &str = "pairing.json";
/// Value of the `format` field.
pub const FORMAT: &str = "lightvm-pairing-v1";

/// Pairing file written: where, and until when the credential is valid.
/// It does not contain the credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingFile {
    /// Full path of the file.
    pub path: PathBuf,
    /// Expiry of the credential.
    pub expires_at: UtcTimestamp,
}

/// Default folder of the pairing file for the current user.
pub fn default_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("LightVM").join("pairing"))
    }
    #[cfg(not(windows))]
    {
        if let Some(r) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(r).join("lightvm-pairing"));
        }
        std::env::var_os("HOME")
            .filter(|d| !d.is_empty())
            .map(|h| PathBuf::from(h).join(".local/state/lightvm/pairing"))
    }
}

/// Writes the pairing file in `dir` (created and restricted to the current
/// user first) and returns its path. `credential` is consumed and its
/// memory overwritten once written.
pub fn write(
    dir: &Path,
    endpoint: &str,
    credential: String,
    expires_at: UtcTimestamp,
) -> Result<PathBuf, String> {
    let mut secret = credential.into_bytes();
    let outcome = write_inner(dir, endpoint, &secret, expires_at);
    secret.fill(0);
    std::hint::black_box(&secret);
    outcome
}

fn write_inner(
    dir: &Path,
    endpoint: &str,
    credential: &[u8],
    expires_at: UtcTimestamp,
) -> Result<PathBuf, String> {
    if credential.len() != 64 || !credential.iter().all(u8::is_ascii_hexdigit) {
        return Err("the supervisor returned a malformed pairing credential".into());
    }
    let credential = std::str::from_utf8(credential).map_err(|_| "malformed credential")?;
    private_dir(dir)?;
    let body = serde_json::json!({
        "format": FORMAT,
        "endpoint": endpoint,
        "credential": credential,
        "expires_at": expires_at.to_string(),
    });
    let mut text = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    let written = write_private_file(&tmp, &text);
    text.fill(0);
    written.map_err(|e| format!("pairing file not written: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("pairing file not written: {e}"));
    }
    Ok(path)
}

/// Removes the pairing file of `dir`, if present (credential revoked,
/// expired or no longer useful).
pub fn remove(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(FILE_NAME));
    let _ = std::fs::remove_file(dir.join(format!("{FILE_NAME}.tmp")));
}

fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

/// Creates `dir` if missing and restricts it to the current user.
fn private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("pairing folder not created: {e}"))?;
    restrict(dir)
}

#[cfg(unix)]
fn restrict(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("pairing folder permissions not set: {e}"))
}

#[cfg(windows)]
fn restrict(dir: &Path) -> Result<(), String> {
    let sid = current_user_sid()?;
    let grant = format!("*{sid}:(OI)(CI)F");
    let out = system_tool("icacls.exe")
        .arg(dir)
        .args(["/inheritance:r", "/grant:r", &grant])
        .output()
        .map_err(|e| format!("icacls not started: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "icacls on the pairing folder exited with {:?}",
            out.status.code()
        ));
    }
    let entries = acl_entries(dir)?;
    if entries != 1 {
        return Err(format!(
            "pairing folder ACL with {entries} entries (expected 1): file not written"
        ));
    }
    Ok(())
}

/// Tool of `%SystemRoot%\System32`, without a console window.
#[cfg(windows)]
fn system_tool(exe: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let mut c = std::process::Command::new(Path::new(&root).join("System32").join(exe));
    c.stdin(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    c
}

/// SID of the current user (`whoami /user`), form `S-1-...`.
#[cfg(windows)]
fn current_user_sid() -> Result<String, String> {
    let out = system_tool("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .map_err(|e| format!("whoami not started: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split(',')
        .map(|f| f.trim().trim_matches('"'))
        .find(|f| f.starts_with("S-1-") && f[4..].chars().all(|c| c.is_ascii_digit() || c == '-'))
        .map(str::to_owned)
        .ok_or_else(|| "user SID not detected".into())
}

/// Entries of the ACL of `dir` reported by `icacls <dir>` (lines with `:(`).
#[cfg(windows)]
fn acl_entries(dir: &Path) -> Result<usize, String> {
    let out = system_tool("icacls.exe")
        .arg(dir)
        .output()
        .map_err(|e| format!("icacls not started: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(":("))
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "lightvm-pairing-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn writes_a_private_file_and_removes_it() {
        let dir = temp_dir("write");
        let cred = "ab".repeat(32);
        let at = UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap();
        let path = write(&dir, r"\\.\pipe\lightvm-test", cred.clone(), at).unwrap();
        assert_eq!(path, dir.join(FILE_NAME));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["format"], FORMAT);
        assert_eq!(v["endpoint"], r"\\.\pipe\lightvm-test");
        assert_eq!(v["credential"], cred.as_str());
        assert_eq!(v["expires_at"], at.to_string());
        assert!(!dir.join(format!("{FILE_NAME}.tmp")).exists());
        #[cfg(windows)]
        assert_eq!(acl_entries(&dir).unwrap(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let m = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o600);
        }
        // A second pairing replaces the first.
        let cred2 = "cd".repeat(32);
        write(&dir, "e", cred2.clone(), at).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["credential"], cred2.as_str());
        remove(&dir);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_credential_is_refused() {
        let dir = temp_dir("malformed");
        let at = UtcTimestamp::from_unix_millis(0).unwrap();
        assert!(write(&dir, "e", "zz".repeat(32), at).is_err());
        assert!(write(&dir, "e", "ab".into(), at).is_err());
        assert!(!dir.join(FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
