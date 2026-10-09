//! `lightvm-service`: supervisor e IPC locale autenticato.
//!
//! Responsabilità (ROADMAP 6.1, 7.3): processo distinto dalla UI, unica
//! autorità su processi, sessioni, policy e file VM; IPC locale autenticato
//! usato da UI e adapter MCP; istanza singola per catalogo; percorso
//! prioritario per revoca ed Emergency Stop indipendente dalla coda.
//!
//! Stato F1: skeleton. `Catalog` e `Backend` sono tratti con implementazioni
//! finte ([`supervisor::fake`]); `vm-storage` e `qemu-backend` li
//! implementeranno. I tipi di protocollo sono in [`compat`] finché
//! `lightvm-proto` non li espone. Dettagli e prove:
//! `docs/verification/F1-04-service-ipc-ci.md`.

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
pub mod compat;
pub mod credential;
pub mod frame;
pub mod lock;
pub mod parent;
pub mod ratelimit;
pub mod server;
pub mod supervisor;
pub mod transport;

#[doc(hidden)]
pub mod test_support;

pub use server::{ServiceConfig, ServiceHandle, StartError, start};
