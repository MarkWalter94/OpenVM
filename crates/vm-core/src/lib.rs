//! `vm-core`: casi d'uso, macchina a stati con guardie, coda di mutazioni per
//! VM, policy effettiva, sessioni, Emergency Stop e audit minimo.
//!
//! Riferimenti: ROADMAP 6.1, 6.3, 7.3, 7.4, 8, 9; `docs/ARCHITECTURE.md` 4–6.
//!
//! Nessuna I/O, nessun QEMU, nessun SQLite: le dipendenze esterne sono porte
//! ([`ports`]). La stessa logica vale per UI e MCP; la UI non è un livello
//! fidato aggiuntivo. I tipi condivisi stanno in `lightvm-proto`.

mod core;
pub mod media;
pub mod policy;
pub mod ports;
pub mod resources;
pub mod session;

pub use crate::core::{
    Core, CoreConfig, Dispatch, DispatchOutcome, EmergencyStopPlan, IDEMPOTENCY_WINDOW,
    MAX_QUEUE_LEN, MediaRegistration, SessionTermination, StartPlan, VmStatus, action_allowed_in,
};
pub use lightvm_proto as proto;
