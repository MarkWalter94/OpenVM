//! `lightvm-service`: supervisor e IPC locale autenticato.
//!
//! Responsabilità (ROADMAP 6.1, 7.3): processo distinto dalla UI, unica
//! autorità su processi, sessioni, policy e file VM; IPC locale autenticato
//! usato da UI e adapter MCP; istanza singola per catalogo; percorso
//! prioritario per revoca ed Emergency Stop indipendente dalla coda.
//!
//! Composizione (F1):
//! - involucro IPC da `lightvm_proto::ipc` (fonte unica con JSON Schema);
//! - [`engine::CoreEngine`]: `vm_core::Core` con le porte reali di
//!   [`adapters`] (catalogo SQLite di `vm-storage`, backend `qemu-backend`,
//!   audit su file, orologio e identificativi di sistema);
//! - [`fakes`]: stesse porte in memoria e backend finto per i test headless.
//!
//! Senza la feature `supervisore` restano solo i moduli del lato client, usati
//! dalla UI (ADR-0024: `qemu-backend` non entra nel processo della UI).
//!
//! Dettagli e prove: `docs/verification/F1-04-service-ipc-ci.md`.

/// Diagnostica su stderr senza panico: se il genitore è morto e la pipe di
/// stderr è chiusa, la scrittura fallisce in silenzio invece di interrompere
/// l'arresto controllato (`eprintln!` andrebbe in panico).
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($arg)*);
    }};
}

pub mod client;
pub mod credential;
pub mod frame;
pub mod transport;

#[cfg(feature = "supervisore")]
pub mod adapters;
#[cfg(feature = "supervisore")]
pub mod engine;
#[cfg(feature = "supervisore")]
pub mod lock;
#[cfg(feature = "supervisore")]
pub mod parent;
#[cfg(feature = "supervisore")]
pub mod ratelimit;
#[cfg(feature = "supervisore")]
pub mod server;
#[cfg(feature = "supervisore")]
pub mod supervisor;

#[cfg(feature = "supervisore")]
#[doc(hidden)]
pub mod fakes;
#[cfg(feature = "supervisore")]
#[doc(hidden)]
pub mod test_support;

#[cfg(feature = "supervisore")]
pub use server::{ServiceConfig, ServiceHandle, StartError, start};
