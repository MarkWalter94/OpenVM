//! Adattatori reali delle porte di `vm-core` (composition root, ROADMAP 6.1):
//!
//! - [`catalog::SqliteStore`]: `CatalogStore` su `vm_storage::Catalog`;
//! - [`backend::QemuHost`] / [`backend::QemuVmBackend`]: `VmBackend` e
//!   percorso prioritario dell'Emergency Stop su `qemu-backend`;
//! - [`audit::FileAuditSink`]: `AuditSink` su file JSON Lines nel profilo utente;
//! - [`SystemClock`] e [`SystemIds`]: orologio e identificativi UUID v7 reali.
//!
//! Gli adattatori sono il più sottili possibile: dove l'API di un altro crate
//! non combacia, la lacuna è indicata nel commento del modulo e nel file di
//! verifica `docs/verification/F1-04-service-ipc-ci.md`.

pub mod audit;
pub mod backend;
pub mod catalog;

use lightvm_proto::UtcTimestamp;
use vm_core::ports::{Clock, IdGenerator};

/// Orologio UTC di sistema.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::now()
    }
}

/// Identificativi UUID v7: millisecondi Unix e 10 byte dal generatore casuale
/// del sistema operativo. Nello stesso millisecondo l'ordine non è garantito
/// (non serve: gli identificativi sono opachi).
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
            // Ripiego senza casualità: identificativi distinti nel processo.
            r[2..].copy_from_slice(&self.counter.to_be_bytes());
        }
        (millis, r)
    }
}
