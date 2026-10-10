//! `lightvm-proto`: shared IPC/MCP types and JSON Schemas generated from a single source.
//!
//! References: ROADMAP 6.3 (implementation rules), 7.2–7.3 (profile and
//! authorization), 8 (baselines), 9 (sessions and reports); `docs/ARCHITECTURE.md`.
//!
//! Contents:
//! - typed identifiers over UUID v7 ([`VmId`], [`RunId`], [`OperationId`],
//!   [`SessionId`], [`BaselineId`], [`MediaId`], [`GrantId`]);
//! - state machine [`VmState`] with a pure transition table and an observed
//!   state [`ObservedState`] distinct from the persisted one;
//! - stable public errors [`PublicError`] / [`ErrorCode`], separate from
//!   protocol errors [`ProtocolError`];
//! - catalog models, typed input, screenshots, sessions, grants, leases,
//!   capabilities, quotas, test outcomes, audit;
//! - [`schema::public_schemas`] for the JSON Schemas committed in `schema/`.
//!
//! No field received from clients is interpreted as a host path or option.
//! The crate performs no I/O, except generating identifiers and instants
//! from the system clock behind the `system-clock` feature.

mod audit;
mod error;
mod ids;
mod input;
pub mod ipc;
mod model;
mod request;
pub mod schema;
mod session;
mod state;
mod text;
mod time;

pub use audit::{AuditAction, AuditEvent, AuditOutcome};
pub use error::{ErrorCode, ProtocolError, ProtocolErrorCode, PublicError, ReplyError};
pub use ids::{BaselineId, GrantId, IdParseError, MediaId, OperationId, RunId, SessionId, VmId};
pub use input::{
    ABS_AXIS_MAX, InputBatch, InputEvent, KeyAction, MAX_INPUT_EVENTS_PER_REQUEST, MouseButton,
    PointerButton, PointerMove,
};
pub use model::{
    BaselineOrigin, BaselineRecord, DisplayGeometry, FileIdentity, Firmware, GuestOs, HostCapacity,
    ImageFormat, MAX_DATA_MEDIA_BYTES, MAX_INSTALL_MEDIA_BYTES, MAX_MEMORY_MARGIN_MIB,
    MAX_STORAGE_BUDGET_GIB, MIN_CPU_MAX_PERCENT, MIN_MEMORY_MARGIN_MIB, MediaEntry, MediaKind,
    MediaRecord, NetworkProfile, OperationKind, OperationRecord, OperationStatus, ResourceLimits,
    RunEndReason, RunRecord, Screenshot, VmConfig, VmLimits, VmRecord,
};
pub use request::{Actor, Admission, Caller, VmAction, VmRequest};
pub use session::{
    Capability, CapabilitySet, EffectivePolicy, Grant, LEASE_MAX_MILLIS, Lease, PolicyLayer,
    PolicyRevision, Quotas, RevocationReport, SessionState, SessionToken, TerminationCause,
    TestOutcome,
};
pub use state::{BackendObservation, ObservationSource, ObservedState, VmState};
pub use text::{Label, ManagedRelPath, QCode, Sha256Digest, TextError, VmName};
pub use time::{TimestampError, UtcTimestamp};

/// Version of the internal IPC/MCP protocol described by these types. It changes only
/// with incompatible schema changes.
pub const PROTOCOL_VERSION: u32 = 1;
