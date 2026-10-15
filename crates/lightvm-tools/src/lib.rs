//! LightVM Tools (ADR-0031, ADR-0034, ADR-0036).
//!
//! Guest input is untrusted. This crate owns the wire limits, run-bound
//! credentials, revocable grants and filesystem confinement. It does not
//! expose host paths or a shell in the guest protocol.

pub mod control;
pub mod files;
pub mod journal;
pub mod policy;
pub mod protocol;
pub mod sandbox;
pub mod server;
pub mod transfer;
