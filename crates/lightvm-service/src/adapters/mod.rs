//! Real adapters for the `vm-core` ports (composition root, ROADMAP 6.1):
//!
//! - [`catalog::SqliteStore`]: `CatalogStore` over `vm_storage::Catalog`;
//! - [`backend::QemuHost`] / [`backend::QemuVmBackend`]: `VmBackend` and the
//!   Emergency Stop priority path over `qemu-backend`;
//! - [`audit::FileAuditSink`]: `AuditSink` on a JSON Lines file in the user profile;
//! - [`SystemClock`] and [`SystemIds`]: real clock and UUID v7 identifiers.
//!
//! The adapters are as thin as possible: where another crate's API does not
//! match, the gap is noted in the module comment and in the verification
//! file `docs/verification/F1-04-service-ipc-ci.md`.

pub mod audit;
pub mod backend;
pub mod catalog;
pub mod network;

use lightvm_proto::UtcTimestamp;
use vm_core::ports::{Clock, IdGenerator};

/// System UTC clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::now()
    }
}

/// UUID v7 identifiers: Unix milliseconds and 10 bytes from the operating
/// system random generator. Within the same millisecond the order is not
/// guaranteed (not needed: identifiers are opaque).
#[derive(Debug, Clone, Default)]
pub struct SystemIds {
    counter: u64,
}

impl IdGenerator for SystemIds {
    fn next_parts(&mut self) -> (u64, [u8; 10]) {
        let millis = u64::try_from(UtcTimestamp::now().as_unix_millis()).unwrap_or(0);
        let mut r = [0u8; 10];
        self.counter = self.counter.wrapping_add(1);
        if getrandom::fill(&mut r).is_err() {
            // Fallback without randomness: identifiers distinct within the process.
            r[2..].copy_from_slice(&self.counter.to_be_bytes());
        }
        (millis, r)
    }
}
