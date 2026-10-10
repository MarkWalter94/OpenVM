//! Minimal audit events (ROADMAP 7.2 item 9, 9; F1: creation, start,
//! stop and denials). Only action type, identifiers, outcome and order: no
//! sensitive payload (typed text, tokens, host paths, guest content).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Actor, Capability, ErrorCode, OperationId, RunId, SessionId, TerminationCause, UtcTimestamp,
    VmId,
};

/// Type of recorded action.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// VM creation.
    VmCreate,
    /// VM start (request forwarded or outcome).
    VmStart,
    /// VM stop (ordinary or forced).
    VmStop,
    /// VM reset.
    VmReset,
    /// Screenshot.
    VmScreenshot,
    /// Input (implicit count only, never the content).
    VmInput,
    /// Observed state change (process/QMP).
    VmStateObserved,
    /// Emergency Stop.
    EmergencyStop,
    /// Session creation.
    SessionCreate,
    /// Session pause.
    SessionPause,
    /// Session resume.
    SessionResume,
    /// Session termination (revocation, expiry...).
    SessionTerminate,
    /// Operation discarded before forwarding.
    OperationDiscard,
    /// Change to a VM's configuration (resources and limits).
    VmConfigure,
    /// Deletion of a stopped VM with its files.
    VmDelete,
    /// Import of a medium into the approved registry (user only).
    MediaImport,
    /// Issue of a per-run console display ticket (never the token).
    ConsoleOpen,
    /// Revocation of a console display ticket (run ended, UI connection
    /// closed or revoked, Emergency Stop, new ticket, shutdown); the cause is
    /// in `termination`.
    ConsoleRevoke,
}

/// Recorded outcome.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Accepted (queued).
    Accepted,
    /// Forwarded to the backend.
    Forwarded,
    /// Completed.
    Succeeded,
    /// Failed.
    Failed,
    /// Denied: no effect.
    Denied,
    /// Discarded before forwarding: no effect.
    Discarded,
}

/// Audit event. `seq` is the total order assigned by the core, strictly increasing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEvent {
    /// Order (from 1, strictly increasing).
    pub seq: u64,
    /// UTC instant.
    pub at: UtcTimestamp,
    /// Actor.
    pub actor: Actor,
    /// Action.
    pub action: AuditAction,
    /// Outcome.
    pub outcome: AuditOutcome,
    /// VM involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_id: Option<VmId>,
    /// Run involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Operation involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
    /// Session involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Capability evaluated (for denials).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    /// Public error code (for denials and failures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
    /// Termination cause (for `session_terminate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationCause>,
}
