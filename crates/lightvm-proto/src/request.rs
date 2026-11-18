//! Typed requests towards the per-VM queue (ROADMAP 6.3, 7.3).
//!
//! These types are the content of the VM control IPC requests; the transport
//! envelope (version, channel authentication, correlation) belongs to
//! `lightvm-service`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Capability, InputBatch, OperationId, OperationKind, OperationStatus, RunId, SessionId,
    SessionToken, VmId,
};

/// Who requests an action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caller {
    /// Local user through the UI (subject to global limits, VM policy and host capacity).
    User,
    /// Agent with a session: also subject to the grant. The token is checked on every
    /// call and right before forwarding.
    Agent {
        /// Declared session.
        session_id: SessionId,
        /// Session secret; missing = `unauthenticated` denial.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<SessionToken>,
    },
}

/// Actor recorded in the audit (without secrets).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    /// Local user.
    User,
    /// Agent session.
    Agent {
        /// Session.
        session_id: SessionId,
    },
    /// Supervisor (expiries, reconciliation, observations).
    System,
    /// Automatic approval under a standing grant (ADR-0039): never a click
    /// of the user, always distinguishable from one in the audit.
    StandingGrant {
        /// Standing grant.
        grant_id: crate::StandingGrantId,
    },
}

impl Caller {
    /// Corresponding actor, without the secret.
    #[must_use]
    pub fn actor(&self) -> Actor {
        match self {
            Caller::User => Actor::User,
            Caller::Agent { session_id, .. } => Actor::Agent {
                session_id: *session_id,
            },
        }
    }
}

/// Action requested on an existing VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VmAction {
    /// Start.
    Start,
    /// Ordinary stop.
    Stop,
    /// Forced stop with OS primitives.
    ForceStop,
    /// Guest reset.
    Reset,
    /// Screenshot.
    Screenshot,
    /// Typed input.
    Input(InputBatch),
}

impl VmAction {
    /// Operation kind.
    #[must_use]
    pub fn kind(&self) -> OperationKind {
        match self {
            VmAction::Start => OperationKind::Start,
            VmAction::Stop => OperationKind::Stop,
            VmAction::ForceStop => OperationKind::ForceStop,
            VmAction::Reset => OperationKind::Reset,
            VmAction::Screenshot => OperationKind::Screenshot,
            VmAction::Input(_) => OperationKind::Input,
        }
    }

    /// Required capability.
    #[must_use]
    pub fn capability(&self) -> Capability {
        match self {
            VmAction::Start => Capability::VmStart,
            VmAction::Stop | VmAction::ForceStop => Capability::VmStop,
            VmAction::Reset => Capability::VmReset,
            VmAction::Screenshot => Capability::VmScreenshot,
            VmAction::Input(_) => Capability::VmInput,
        }
    }
}

/// Request for an action on a VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmRequest {
    /// Idempotency key chosen by the caller: the same request repeated with
    /// the same `operation_id` does not duplicate the effect.
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Expected run: mandatory for reset, screenshot and input; if present
    /// and different from the current run the request is rejected (`state_conflict`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_run_id: Option<RunId>,
    /// Action (object with a `type` discriminant).
    pub action: VmAction,
}

/// Outcome of accepting a request (before forwarding).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Admission {
    /// Operation.
    pub operation_id: OperationId,
    /// Current status of the operation.
    pub status: OperationStatus,
    /// `true` if the request repeats an already known `operation_id` (no new effect).
    pub duplicate: bool,
}
