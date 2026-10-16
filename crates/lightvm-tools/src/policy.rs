//! The supervisor replaces a grant on every session transition. Old handles
//! cannot regain access after a grant is widened or a new session begins.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use subtle::ConstantTimeEq;

use crate::protocol::{self, Hello};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    ShareRead,
    ShareWrite,
    Clipboard,
    FilesTransfer,
    Usb,
    UiInspect,
    UiAct,
}

/// No implicit permissions. A write share does not grant clipboard or USB.
#[derive(Clone, Debug, Default)]
pub struct Grant {
    pub features: BTreeSet<Feature>,
    /// Share names and whether each share may be written.
    pub shares: BTreeMap<String, bool>,
    pub usb: BTreeSet<String>,
    pub expires: Option<Instant>,
}

struct State {
    generation: u64,
    grant: Grant,
}

/// One instance per VM run. Never persist or reuse it after a restore.
#[derive(Clone)]
pub struct Gate {
    state: Arc<Mutex<State>>,
    revoked: Arc<AtomicBool>,
}

impl fmt::Debug for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Gate(<permissions>)")
    }
}

#[derive(Clone, Debug)]
pub struct Permit {
    gate: Gate,
    generation: u64,
}

impl Gate {
    pub fn new(grant: Grant) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                generation: 0,
                grant,
            })),
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }
    /// Invalidates every existing permit, including active transfers.
    pub fn replace(&self, grant: Grant) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.generation = s
            .generation
            .checked_add(1)
            .expect("permission generation exhausted");
        s.grant = grant;
        self.revoked.store(false, Ordering::SeqCst);
    }
    pub fn revoke(&self) {
        // Never wait for file I/O on the priority path. Already admitted
        // writes may finish in a staging file; publication checks this flag
        // again after flushing. A syscall already forwarded is not undone.
        self.revoked.store(true, Ordering::SeqCst);
    }
    pub fn revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }
    /// A lease renewal must not invalidate in-flight transfer permits.
    pub fn renew(&self, expires: Option<Instant>) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .grant
            .expires = expires;
    }
    pub fn permit(&self) -> Permit {
        Permit {
            gate: self.clone(),
            generation: self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation,
        }
    }
}

impl Permit {
    /// Supervisor-only maintenance of the current run. This admits no public
    /// capability; it still rejects stale generations, revoked or expired runs.
    pub fn with_run<T>(&self, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.checked(None, None, operation)
    }
    /// Serializes admission against grant replacement. Revocation rejects
    /// new admissions without waiting for a syscall already in progress.
    pub fn with<T>(
        &self,
        feature: Feature,
        share: Option<(&str, bool)>,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        self.checked(Some(feature), share, operation)
    }

    fn checked<T>(
        &self,
        feature: Option<Feature>,
        share: Option<(&str, bool)>,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let s = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.revoked()
            || s.generation != self.generation
            || s.grant.expires.is_some_and(|t| Instant::now() >= t)
            || feature.is_some_and(|feature| !s.grant.features.contains(&feature))
            || share.is_some_and(|(name, write)| {
                !s.grant.shares.get(name).is_some_and(|w| !write || *w)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "tools permission denied",
            ));
        }
        operation()
    }

    pub fn revoked(&self) -> bool {
        self.gate.revoked.load(Ordering::SeqCst)
    }
}

/// Per-run secret: only an explicit export can reveal its hexadecimal form.
pub struct Secret([u8; 32]);
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}
impl Secret {
    pub fn generate() -> io::Result<Self> {
        let mut b = [0; 32];
        getrandom::fill(&mut b).map_err(io::Error::other)?;
        Ok(Self(b))
    }
    pub fn export(&self) -> String {
        protocol::encode(&self.0)
    }
    pub fn authenticate(&self, hello: &Hello, vm: &str, run: &str) -> bool {
        let mut b = protocol::decode(&hello.credential, 32).unwrap_or_default();
        let accepted = hello.version == protocol::VERSION
            && hello.vm == vm
            && hello.run == run
            && bool::from(self.0.as_slice().ct_eq(&b));
        b.fill(0);
        std::hint::black_box(&b);
        accepted
    }
}
