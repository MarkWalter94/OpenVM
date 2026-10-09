//! Credenziali di connessione: 32 byte casuali dal generatore del sistema
//! operativo, confronto a tempo costante, ruoli, scadenza e revoca.
//!
//! Le credenziali vivono solo in memoria: un riavvio del servizio le invalida
//! tutte (ROADMAP 7.4, riga "Sessioni": token riutilizzato dopo riavvio).

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use lightvm_proto::ipc::Role;

/// Lunghezza della credenziale in byte.
pub const CREDENTIAL_LEN: usize = 32;

/// Segreto di 32 byte. `Debug` non mostra il valore.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential([u8; CREDENTIAL_LEN]);

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redatta>)")
    }
}

impl Drop for Credential {
    /// Azzeramento in memoria al rilascio (miglior sforzo, senza `unsafe`).
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}

impl Credential {
    /// Genera una credenziale dal generatore casuale del sistema operativo.
    pub fn generate() -> std::io::Result<Self> {
        let mut b = [0u8; CREDENTIAL_LEN];
        getrandom::fill(&mut b).map_err(std::io::Error::other)?;
        Ok(Self(b))
    }

    /// Forma esadecimale minuscola (64 caratteri).
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Decodifica dalla forma esadecimale; lunghezza e alfabeto verificati.
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

    /// Confronto a tempo costante rispetto al contenuto.
    pub fn ct_eq(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        std::hint::black_box(diff) == 0
    }
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

/// Identificativo interno di una credenziale (non segreto).
pub type CredentialId = u64;

#[derive(Debug)]
struct Entry {
    secret: Credential,
    role: Role,
    expires_at: Option<Instant>,
    revoked: bool,
}

/// Archivio in memoria delle credenziali.
#[derive(Debug, Default)]
pub struct CredentialStore {
    next_id: CredentialId,
    entries: HashMap<CredentialId, Entry>,
}

impl CredentialStore {
    /// Archivio vuoto.
    pub fn new() -> Self {
        Self::default()
    }

    /// Emette una nuova credenziale con ruolo e durata opzionale.
    pub fn issue(
        &mut self,
        role: Role,
        ttl: Option<Duration>,
    ) -> std::io::Result<(CredentialId, Credential)> {
        let secret = Credential::generate()?;
        self.next_id += 1;
        let id = self.next_id;
        self.entries.insert(
            id,
            Entry {
                secret: secret.clone(),
                role,
                expires_at: ttl.map(|t| Instant::now() + t),
                revoked: false,
            },
        );
        Ok((id, secret))
    }

    /// Verifica una credenziale presentata. Confronta con tutte le voci a tempo
    /// costante per voce; restituisce id e ruolo solo se valida, non scaduta e
    /// non revocata. Nessuna distinzione verso il client tra i casi di rifiuto.
    pub fn verify(&self, presented: &Credential) -> Option<(CredentialId, Role)> {
        let now = Instant::now();
        let mut found = None;
        for (id, e) in &self.entries {
            if e.secret.ct_eq(presented) {
                found = Some((*id, e));
            }
        }
        let (id, e) = found?;
        if e.revoked || e.expires_at.is_some_and(|t| now >= t) {
            return None;
        }
        Some((id, e.role))
    }

    /// Revoca una credenziale. Restituisce vero se esisteva.
    pub fn revoke(&mut self, id: CredentialId) -> bool {
        match self.entries.get_mut(&id) {
            Some(e) => {
                e.revoked = true;
                true
            }
            None => false,
        }
    }

    /// Revoca tutte le credenziali con un ruolo diverso da `keep`.
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
        assert_eq!(s.verify(&p_c), None, "scaduta");
        let _ = p;
        let other = Credential::generate().unwrap();
        assert_eq!(s.verify(&other), None);
        assert!(s.revoke(ui));
        assert_eq!(s.verify(&ui_c), None);
    }
}
