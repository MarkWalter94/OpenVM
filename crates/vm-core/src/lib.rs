//! `vm-core`: use cases, state machine with guards, per-VM mutation queue,
//! effective policy, sessions, Emergency Stop and minimal audit.
//!
//! References: ROADMAP 6.1, 6.3, 7.3, 7.4, 8, 9; `docs/ARCHITECTURE.md` 4–6.
//!
//! No I/O, no QEMU, no SQLite: external dependencies are ports
//! ([`ports`]). The same logic applies to UI and MCP; the UI is not an
//! additional trusted layer. Shared types live in `lightvm-proto`.

mod core;
pub mod media;
pub mod policy;
pub mod ports;
pub mod resources;
pub mod session;

pub use crate::core::{
    Core, CoreConfig, Dispatch, DispatchOutcome, EmergencyStopPlan, IDEMPOTENCY_WINDOW,
    MAX_QUEUE_LEN, MediaRegistration, SessionTermination, StartPlan, VmStatus, action_allowed_in,
    input_allowed_for,
};
pub use lightvm_proto as proto;
