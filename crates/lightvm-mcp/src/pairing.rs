//! Pairing file written by the LightVM app ("Pair a client"): the endpoint
//! of the supervisor's IPC and a pairing credential (ADR-0006).
//!
//! The adapter reads the app's file at its default location
//! (`%LOCALAPPDATA%\LightVM\pairing\pairing.json` on Windows), or another
//! file of the same format named by `LIGHTVM_PAIRING_FILE` (a *path*; never
//! a command-line argument, never the credential itself). Before reading,
//! the folder must be private to the current user: on Windows its ACL has
//! exactly one entry, for the current user (the app removes inheritance and
//! grants only the user's SID); on Unix the folder and the file have no
//! group/other permission bits. The content is never printed, logged nor
//! returned to the MCP client. The credential alone grants no VM
//! permission: it only lets the adapter ask for a session that the user
//! approves in the app.

use std::path::{Path, PathBuf};

use lightvm_proto::UtcTimestamp;
use lightvm_service::credential::Credential;

/// Value of the `format` field written by the app.
pub const FORMAT: &str = "lightvm-pairing-v1";
/// Name of the pairing file.
pub const FILE_NAME: &str = "pairing.json";
/// Environment variable with the *path* of the pairing file.
pub const ENV_PAIRING_FILE: &str = "LIGHTVM_PAIRING_FILE";
/// Largest pairing file accepted.
const MAX_FILE_BYTES: u64 = 4096;

/// Content of a valid pairing file. `Debug` does not show the credential.
pub struct Pairing {
    /// IPC endpoint of the supervisor (not a secret).
    pub endpoint: String,
    /// Pairing credential.
    pub credential: Credential,
    /// Expiry of the credential (for new connections).
    pub expires_at: UtcTimestamp,
}

impl std::fmt::Debug for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pairing")
            .field("endpoint", &self.endpoint)
            .field("credential", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Why a pairing file was not usable (never the content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    /// No path configured and no default location.
    NoLocation,
    /// File missing or not readable.
    Missing,
    /// Not a regular file (link, junction, folder) or too large.
    NotRegular,
    /// Malformed content.
    Malformed(&'static str),
    /// The credential expired: pair again in the app.
    Expired,
    /// The folder (or file) is not private to the current user.
    NotPrivate,
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLocation => f.write_str("no pairing file location"),
            Self::Missing => f.write_str("pairing file not found"),
            Self::NotRegular => f.write_str("pairing file is not a small regular file"),
            Self::Malformed(why) => write!(f, "pairing file not valid ({why})"),
            Self::Expired => f.write_str("pairing credential expired"),
            Self::NotPrivate => {
                f.write_str("pairing folder not private to the current user (file not read)")
            }
        }
    }
}

/// Default location used by the app (`apps/desktop/src/pairing.rs`).
pub fn default_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|d| !d.is_empty())
            .map(|d| {
                PathBuf::from(d)
                    .join("LightVM")
                    .join("pairing")
                    .join(FILE_NAME)
            })
    }
    #[cfg(not(windows))]
    {
        if let Some(r) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(r).join("lightvm-pairing").join(FILE_NAME));
        }
        std::env::var_os("HOME").filter(|d| !d.is_empty()).map(|h| {
            PathBuf::from(h)
                .join(".local/state/lightvm/pairing")
                .join(FILE_NAME)
        })
    }
}

/// Path of the pairing file: `LIGHTVM_PAIRING_FILE`, then the default.
pub fn resolve_path() -> Option<PathBuf> {
    std::env::var_os(ENV_PAIRING_FILE)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(default_path)
}

/// Reads and checks the pairing file (folder privacy first).
pub fn read(path: &Path) -> Result<Pairing, PairingError> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| PairingError::Missing)?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(PairingError::NotRegular);
    }
    let dir = path.parent().ok_or(PairingError::NoLocation)?;
    if !private_folder(dir, path) {
        return Err(PairingError::NotPrivate);
    }
    let mut bytes = std::fs::read(path).map_err(|_| PairingError::Missing)?;
    let parsed = parse(&bytes);
    bytes.fill(0);
    parsed
}

/// Unix: folder and file without group/other permission bits.
#[cfg(unix)]
pub fn private_folder(dir: &Path, file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o077);
    matches!((mode(dir), mode(file)), (Ok(0), Ok(0)))
}

/// Windows: the folder is not a link and its ACL (as `icacls` reports it)
/// has exactly one entry, for the current user.
#[cfg(windows)]
pub fn private_folder(dir: &Path, _file: &Path) -> bool {
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    let Some(user) = current_user() else {
        return false;
    };
    let Ok(out) = system_tool("icacls.exe").arg(dir).output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let entries: Vec<&str> = text.lines().filter(|l| l.contains(":(")).collect();
    let me = format!("{}:(", user.to_lowercase());
    entries.len() == 1 && entries[0].contains(&me)
}

/// `DOMAIN\user` of the current user (`whoami`).
#[cfg(windows)]
fn current_user() -> Option<String> {
    let out = system_tool("whoami.exe").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty() && s.contains('\\')).then_some(s)
}

/// Tool of `%SystemRoot%\System32`, without a console window.
#[cfg(windows)]
fn system_tool(exe: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let mut c = std::process::Command::new(Path::new(&root).join("System32").join(exe));
    c.stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    c
}

/// Restricts a folder to the current user as the app does (tests and the
/// real-client harness; in production the app owns the folder).
pub fn restrict_folder(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        let out = system_tool("whoami.exe")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let sid = text
            .split(',')
            .map(|f| f.trim().trim_matches('"'))
            .find(|f| f.starts_with("S-1-"))
            .ok_or_else(|| std::io::Error::other("user SID not detected"))?
            .to_owned();
        let st = system_tool("icacls.exe")
            .arg(dir)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
            .stdout(std::process::Stdio::null())
            .status()?;
        if st.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("icacls failed"))
        }
    }
}

fn parse(bytes: &[u8]) -> Result<Pairing, PairingError> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| PairingError::Malformed("not JSON"))?;
    if v.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err(PairingError::Malformed("format"));
    }
    let endpoint = v
        .get("endpoint")
        .and_then(|e| e.as_str())
        .filter(|e| !e.is_empty() && e.len() <= 512 && !e.chars().any(char::is_control))
        .ok_or(PairingError::Malformed("endpoint"))?
        .to_owned();
    let credential = v
        .get("credential")
        .and_then(|c| c.as_str())
        .and_then(Credential::from_hex)
        .ok_or(PairingError::Malformed("credential"))?;
    let expires_at: UtcTimestamp = v
        .get("expires_at")
        .and_then(|e| serde_json::from_value(e.clone()).ok())
        .ok_or(PairingError::Malformed("expires_at"))?;
    if UtcTimestamp::now() >= expires_at {
        return Err(PairingError::Expired);
    }
    Ok(Pairing {
        endpoint,
        credential,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later() -> String {
        UtcTimestamp::now()
            .checked_add_millis(600_000)
            .unwrap()
            .to_string()
    }

    #[test]
    fn folder_must_be_private() {
        let dir = std::env::temp_dir().join(format!("lightvm-mcp-pairing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(FILE_NAME);
        std::fs::write(&f, b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Inherited ACL / group-readable folder: refused before reading.
        assert_eq!(read(&f).unwrap_err(), PairingError::NotPrivate);
        restrict_folder(&dir).unwrap();
        assert!(private_folder(&dir, &f));
        assert_eq!(read(&f).unwrap_err(), PairingError::Malformed("format"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_file_and_debug_redacts() {
        let cred = "ab".repeat(32);
        let body = serde_json::json!({
            "format": FORMAT, "endpoint": r"\\.\pipe\x", "credential": cred, "expires_at": later()
        });
        let p = parse(body.to_string().as_bytes()).unwrap();
        assert_eq!(p.endpoint, r"\\.\pipe\x");
        assert!(!format!("{p:?}").contains(&cred));
    }

    #[test]
    fn invalid_files_are_refused_without_echo() {
        let cred = "ab".repeat(32);
        for (body, want) in [
            (serde_json::json!({"format": "x"}), "format"),
            (
                serde_json::json!({"format": FORMAT, "endpoint": "e\n", "credential": cred, "expires_at": later()}),
                "endpoint",
            ),
            (
                serde_json::json!({"format": FORMAT, "endpoint": "e", "credential": "zz", "expires_at": later()}),
                "credential",
            ),
        ] {
            let e = parse(body.to_string().as_bytes()).unwrap_err();
            assert_eq!(e, PairingError::Malformed(want));
            assert!(!e.to_string().contains(&cred));
        }
        let expired = serde_json::json!({
            "format": FORMAT, "endpoint": "e", "credential": cred, "expires_at": "2020-01-01T00:00:00.000Z"
        });
        assert_eq!(
            parse(expired.to_string().as_bytes()).unwrap_err(),
            PairingError::Expired
        );
    }
}
