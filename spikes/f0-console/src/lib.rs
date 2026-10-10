//! Spike F0-04 (not product code): minimal QMP client for input and
//! `screendump`, PPM→PNG conversion, US typing table and statistics.
//!
//! The QMP client accepts only a Unix socket path (private channel,
//! ADR-0012) and does not expose a generic `execute` to the UI: it offers
//! typed methods for input, capture and status. The product QMP lives in
//! `crates/qemu-backend` starting from F1.

#![forbid(unsafe_code)]

pub mod image;
pub mod qmp;
pub mod stats;
pub mod typing;
