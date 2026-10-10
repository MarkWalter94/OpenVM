//! `lightvm-proto`: tipi condivisi IPC/MCP e JSON Schema generati da fonte unica.
//!
//! Riferimenti: ROADMAP 6.3 (regole di implementazione), 7.2–7.3 (profilo e
//! autorizzazione), 8 (baseline), 9 (sessioni e report); `docs/ARCHITECTURE.md`.
//!
//! Contenuto:
//! - identificativi tipizzati su UUID v7 ([`VmId`], [`RunId`], [`OperationId`],
//!   [`SessionId`], [`BaselineId`], [`MediaId`], [`GrantId`]);
//! - macchina a stati [`VmState`] con tabella delle transizioni pura e stato
//!   osservato [`ObservedState`] distinto da quello persistito;
//! - errori pubblici stabili [`PublicError`] / [`ErrorCode`], separati dagli
//!   errori di protocollo [`ProtocolError`];
//! - modelli del catalogo, input tipizzato, screenshot, sessioni, grant, lease,
//!   capability, quote, esiti dei test, audit;
//! - [`schema::public_schemas`] per i JSON Schema committati in `schema/`.
//!
//! Nessun campo ricevuto dai client è interpretato come percorso o opzione host.
//! Il crate non esegue I/O, salvo la generazione di identificativi e istanti
//! dall'orologio di sistema dietro la feature `system-clock`.

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
    ImageFormat, MAX_MEMORY_MARGIN_MIB, MAX_STORAGE_BUDGET_GIB, MIN_CPU_MAX_PERCENT,
    MIN_MEMORY_MARGIN_MIB, MediaEntry, MediaKind, MediaRecord, NetworkProfile, OperationKind,
    OperationRecord, OperationStatus, ResourceLimits, RunEndReason, RunRecord, Screenshot,
    VmConfig, VmLimits, VmRecord,
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

/// Versione del protocollo IPC/MCP interno descritto da questi tipi. Cambia solo
/// con modifiche incompatibili degli schemi.
pub const PROTOCOL_VERSION: u32 = 1;
