//! Connection credentials: 32 random bytes from the operating system
//! generator, constant-time comparison, roles, expiry and revocation.
//!
//! Credentials live only in memory: a service restart invalidates them all
//! (ROADMAP 7.4, "Sessions" row: token reused after restart). The one
//! exception is the pairing credential of a standing grant (ADR-0039): the
//! catalog keeps its SHA-256 only, and the service registers that hash again
//! at start ([`CredentialStore::register_hashed`]); the secret itself never
//! reaches the service again.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use lightvm_proto::ipc::Role;

/// Credential length in bytes.
pub const CREDENTIAL_LEN: usize = 32;

/// 32-byte secret. `Debug` does not show the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential([u8; CREDENTIAL_LEN]);

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

impl Drop for Credential {
    /// Zeroes the memory on drop (best effort, without `unsafe`).
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}

impl Credential {
    /// Generates a credential from the operating system random generator.
    pub fn generate() -> std::io::Result<Self> {
        let mut b = [0u8; CREDENTIAL_LEN];
        getrandom::fill(&mut b).map_err(std::io::Error::other)?;
        Ok(Self(b))
    }

    /// Lowercase hexadecimal form (64 characters).
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Decodes from the hexadecimal form; length and alphabet are checked.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.as_bytes();
        if s.len() != CREDENTIAL_LEN * 2 {
            return None;
        }
        let mut out = [0u8; CREDENTIAL_LEN];
        for (i, pair) in s.as_chunks::<2>().0.iter().enumerate() {
            out[i] = (hex_val(pair[0])? << 4) | hex_val(pair[1])?;
        }
        Some(Self(out))
    }

    /// Comparison in constant time with respect to the content.
    pub fn ct_eq(&self, other: &Self) -> bool {
        ct_eq32(&self.0, &other.0)
    }

    /// SHA-256 of the secret (what the catalog keeps of a standing grant's
    /// credential, ADR-0039).
    pub fn sha256(&self) -> [u8; 32] {
        use sha2::Digest as _;
        sha2::Sha256::digest(self.0).into()
    }
}

fn ct_eq32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// What the store keeps of a credential.
#[derive(Debug)]
enum Secret {
    /// The secret (issued by this service).
    Plain(Credential),
    /// Its SHA-256 only (standing grant registered again at start).
    Hash([u8; 32]),
}

pub(crate) fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(HEX[(x >> 4) as usize] as char);
        s.push(HEX[(x & 0xf) as usize] as char);
    }
    s
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Internal identifier of a credential (not secret).
pub type CredentialId = u64;

#[derive(Debug)]
struct Entry {
    secret: Secret,
    role: Role,
    expires_at: Option<Instant>,
    revoked: bool,
}

/// In-memory credential store.
#[derive(Debug, Default)]
pub struct CredentialStore {
    next_id: CredentialId,
    entries: HashMap<CredentialId, Entry>,
}

impl CredentialStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Issues a new credential with a role and an optional lifetime.
    pub fn issue(
        &mut self,
        role: Role,
        ttl: Option<Duration>,
    ) -> std::io::Result<(CredentialId, Credential)> {
        let secret = Credential::generate()?;
        // Expired or revoked entries are no longer needed: they do not pile up
        // with repeated issuing (verification scans them all).
        let now = Instant::now();
        self.entries
            .retain(|_, e| !e.revoked && e.expires_at.is_none_or(|t| now < t));
        self.next_id += 1;
        let id = self.next_id;
        self.entries.insert(
            id,
            Entry {
                secret: Secret::Plain(secret.clone()),
                role,
                expires_at: ttl.map(|t| Instant::now() + t),
                revoked: false,
            },
        );
        Ok((id, secret))
    }

    /// Verifies a presented credential. Compares against all entries in
    /// constant time per entry; returns id and role only if valid, not expired
    /// and not revoked. No distinction towards the client between rejection cases.
    pub fn verify(&self, presented: &Credential) -> Option<(CredentialId, Role)> {
        let now = Instant::now();
        let presented_sha = presented.sha256();
        let mut found = None;
        for (id, e) in &self.entries {
            let matches = match &e.secret {
                Secret::Plain(s) => s.ct_eq(presented),
                Secret::Hash(h) => ct_eq32(h, &presented_sha),
            };
            if matches {
                found = Some((*id, e));
            }
        }
        let (id, e) = found?;
        if e.revoked || e.expires_at.is_some_and(|t| now >= t) {
            return None;
        }
        Some((id, e.role))
    }

    /// Registers a credential known only by its SHA-256 (the pairing
    /// credential of a standing grant, loaded from the catalog at start,
    /// ADR-0039), with the lifetime left.
    pub fn register_hashed(
        &mut self,
        role: Role,
        sha256: [u8; 32],
        ttl: Option<Duration>,
    ) -> CredentialId {
        self.next_id += 1;
        let id = self.next_id;
        self.entries.insert(
            id,
            Entry {
                secret: Secret::Hash(sha256),
                role,
                expires_at: ttl.map(|t| Instant::now() + t),
                revoked: false,
            },
        );
        id
    }

    /// `true` if the credential exists, is not revoked and not expired.
    pub fn is_valid(&self, id: CredentialId) -> bool {
        let now = Instant::now();
        self.entries
            .get(&id)
            .is_some_and(|e| !e.revoked && e.expires_at.is_none_or(|t| now < t))
    }

    /// Revokes a credential. Returns true if it existed.
    pub fn revoke(&mut self, id: CredentialId) -> bool {
        match self.entries.get_mut(&id) {
            Some(e) => {
                e.revoked = true;
                true
            }
            None => false,
        }
    }

    /// Revokes all credentials with a role other than `keep`.
    pub fn revoke_all_except(&mut self, keep: Role) -> u32 {
        let mut n = 0;
        for e in self.entries.values_mut() {
            if e.role != keep && !e.revoked {
                e.revoked = true;
                n += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_validation() {
        let c = Credential::generate().unwrap();
        let h = c.to_hex();
        assert_eq!(h.len(), 64);
        assert_eq!(Credential::from_hex(&h), Some(c));
        assert!(Credential::from_hex(&h[..63]).is_none());
        assert!(Credential::from_hex(&"g".repeat(64)).is_none());
        assert!(Credential::from_hex("").is_none());
    }

    #[test]
    fn two_credentials_differ() {
        assert_ne!(
            Credential::generate().unwrap(),
            Credential::generate().unwrap()
        );
    }

    #[test]
    fn debug_redacts() {
        let c = Credential::generate().unwrap();
        assert!(!format!("{c:?}").contains(&c.to_hex()));
    }

    #[test]
    fn store_verify_revoke_expire() {
        let mut s = CredentialStore::new();
        let (ui, ui_c) = s.issue(Role::Ui, None).unwrap();
        let (p, p_c) = s.issue(Role::Pairing, Some(Duration::ZERO)).unwrap();
        assert_eq!(s.verify(&ui_c), Some((ui, Role::Ui)));
        assert_eq!(s.verify(&p_c), None, "expired");
        let _ = p;
        let other = Credential::generate().unwrap();
        assert_eq!(s.verify(&other), None);
        assert!(s.revoke(ui));
        assert_eq!(s.verify(&ui_c), None);
    }

    /// ADR-0039: a credential known only by its hash verifies like the
    /// original, expires and is revoked like any other.
    #[test]
    fn hashed_credentials_verify_expire_and_revoke() {
        let mut s = CredentialStore::new();
        let c = Credential::generate().unwrap();
        let id = s.register_hashed(Role::Pairing, c.sha256(), Some(Duration::from_secs(60)));
        assert_eq!(s.verify(&c), Some((id, Role::Pairing)));
        assert!(s.is_valid(id));
        assert_eq!(s.verify(&Credential::generate().unwrap()), None);
        // Issuing others does not purge it.
        let _ = s.issue(Role::Pairing, None).unwrap();
        assert_eq!(s.verify(&c), Some((id, Role::Pairing)));
        assert_eq!(s.revoke_all_except(Role::Ui), 2);
        assert_eq!(s.verify(&c), None);
        assert!(!s.is_valid(id));
        let gone = s.register_hashed(Role::Pairing, c.sha256(), Some(Duration::ZERO));
        assert_eq!(s.verify(&c), None, "expired");
        assert!(!s.is_valid(gone));
    }
}
