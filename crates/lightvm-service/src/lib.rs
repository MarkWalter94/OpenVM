//! `lightvm-service`: supervisor and authenticated local IPC.
//!
//! Responsibilities (ROADMAP 6.1, 7.3): a process separate from the UI, sole
//! authority over processes, sessions, policies and VM files; authenticated
//! local IPC used by the UI and the MCP adapter; single instance per catalog;
//! priority path for revocation and Emergency Stop independent of the queue.
//!
//! Composition (F1):
//! - IPC envelope from `lightvm_proto::ipc` (single source with the JSON Schema);
//! - [`engine::CoreEngine`]: `vm_core::Core` with the real ports from
//!   [`adapters`] (SQLite catalog from `vm-storage`, `qemu-backend` backend,
//!   file audit, system clock and identifiers);
//! - [`fakes`]: the same ports in memory and a fake backend for headless tests.
//!
//! Without the `supervisor` feature only the client-side modules remain, used
//! by the UI (ADR-0024: `qemu-backend` does not enter the UI process).
//!
//! Details and evidence: `docs/verification/F1-04-service-ipc-ci.md`.

/// Diagnostics on stderr without panicking: if the parent died and the stderr
/// pipe is closed, the write fails silently instead of interrupting the
/// controlled shutdown (`eprintln!` would panic).
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

#[cfg(feature = "supervisor")]
pub mod adapters;
#[cfg(feature = "supervisor")]
pub mod agents;
#[cfg(feature = "supervisor")]
pub mod display;
#[cfg(feature = "supervisor")]
pub mod engine;
#[cfg(feature = "supervisor")]
pub mod lock;
#[cfg(feature = "supervisor")]
pub mod parent;
#[cfg(feature = "supervisor")]
pub mod ratelimit;
#[cfg(feature = "supervisor")]
pub mod reports;
#[cfg(feature = "supervisor")]
pub mod resume;
#[cfg(feature = "supervisor")]
pub mod server;
#[cfg(feature = "supervisor")]
pub mod start;
#[cfg(feature = "supervisor")]
pub mod supervisor;

#[cfg(feature = "supervisor")]
#[doc(hidden)]
pub mod fakes;
#[cfg(feature = "supervisor")]
#[doc(hidden)]
pub mod test_support;

#[cfg(feature = "supervisor")]
pub use server::{ServiceConfig, ServiceHandle, StartError, start};
