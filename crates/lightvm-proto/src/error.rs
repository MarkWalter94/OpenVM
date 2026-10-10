//! Stable public errors (ROADMAP 6.3, 9).
//!
//! Two separate families:
//! - [`PublicError`]: **operational** error of a well-formed request (permissions,
//!   state, limits). The code is stable and part of the public contract.
//! - [`ProtocolError`]: the request was not understood (format, version,
//!   size). No effect was attempted.
//!
//! Messages are for humans and contain no secrets or host paths; clients
//! must decide based on `code` and `retryable`, never on the text.
//!
//! # Authentication and authorization
//!
//! Three distinct operational codes, evaluated in this order:
//!
//! | Code | Meaning | Examples |
//! | --- | --- | --- |
//! | [`ErrorCode::Unauthenticated`] | no valid credential: the caller is not identified | IPC credential or session token missing, malformed, unknown, revoked or reused after a supervisor restart; request before the handshake |
//! | [`ErrorCode::SessionExpired`] | credential recognized but expired (grant or lease) | lease not renewed within 10 s, grant past its expiry |
//! | [`ErrorCode::PermissionDenied`] | caller identified but not authorized | VM outside the allowlist, capability missing from the effective policy, session paused for mutations, insufficient IPC role, VM creation by an agent, media different from the approved one |
//!
//! Within `unauthenticated` the reasons are not distinguished towards the client
//! (same code and same message for a missing, wrong or revoked token), so as not
//! to reveal which sessions exist. The protocol code
//! [`ProtocolErrorCode::Unauthenticated`] stays in the contract for compatibility
//! but is not emitted for credentials: a rejected credential is the operational
//! outcome of an understood request, so [`ErrorCode::Unauthenticated`] is used.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable code of an operational error. Serialized names never change;
/// new codes are only ever added.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Caller identified (valid credential) but not authorized for the action:
    /// VM outside the allowlist, missing capability, paused session, insufficient
    /// role. Missing or invalid credentials: [`ErrorCode::Unauthenticated`].
    PermissionDenied,
    /// Session or lease expired: a new authorization in the app is needed.
    SessionExpired,
    /// Another mutation is in progress or queued on the same VM: retryable.
    VmBusy,
    /// Capability not available on this host, build or phase.
    Unsupported,
    /// The action is not allowed in the current state (or refers to a superseded `run_id`).
    StateConflict,
    /// Resource, quota or size limit exceeded.
    ResourceLimit,
    /// Invalid argument (format, range, name, rejected path).
    InvalidArgument,
    /// Object does not exist or is not visible to the caller.
    NotFound,
    /// Infrastructure failure (QEMU, QMP, storage, OS) not attributable to the caller.
    InfrastructureError,
    /// No valid credential: IPC credential or session token missing,
    /// malformed, unknown, revoked or reused after a supervisor restart.
    /// A new authorization in the app is needed. Appended after the first nine
    /// codes, which remain unchanged.
    Unauthenticated,
}

impl ErrorCode {
    /// All codes, in frozen order (new codes are appended at the end).
    pub const ALL: [ErrorCode; 10] = [
        ErrorCode::PermissionDenied,
        ErrorCode::SessionExpired,
        ErrorCode::VmBusy,
        ErrorCode::Unsupported,
        ErrorCode::StateConflict,
        ErrorCode::ResourceLimit,
        ErrorCode::InvalidArgument,
        ErrorCode::NotFound,
        ErrorCode::InfrastructureError,
        ErrorCode::Unauthenticated,
    ];

    /// Stable name (same as the serialization).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::SessionExpired => "session_expired",
            ErrorCode::VmBusy => "vm_busy",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::StateConflict => "state_conflict",
            ErrorCode::ResourceLimit => "resource_limit",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::NotFound => "not_found",
            ErrorCode::InfrastructureError => "infrastructure_error",
            ErrorCode::Unauthenticated => "unauthenticated",
        }
    }

    /// Default value of `retryable`: retrying the *same* request with the
    /// same `operation_id` may succeed. Applies only to idempotent operations:
    /// GUI inputs are never repeated automatically (ROADMAP 6.3).
    #[must_use]
    pub const fn default_retryable(self) -> bool {
        matches!(self, ErrorCode::VmBusy | ErrorCode::InfrastructureError)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Public operational error.
///
/// JSON shape: `{"code": "...", "message": "...", "details": {..}?, "retryable": bool}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicError {
    /// Stable code.
    pub code: ErrorCode,
    /// Message for humans, without secrets or host paths.
    pub message: String,
    /// Optional structured details (textual keys and values, without secrets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, String>>,
    /// `true` if retrying the same idempotent request may succeed.
    pub retryable: bool,
}

impl PublicError {
    /// New error with the default `retryable` for the code.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
            retryable: code.default_retryable(),
        }
    }

    /// Adds a `key = value` detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }

    /// Sets `retryable` explicitly.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Shortcut for `permission_denied`.
    #[must_use]
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }
    /// Shortcut for `session_expired`.
    #[must_use]
    pub fn session_expired(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionExpired, message)
    }
    /// Shortcut for `vm_busy`.
    #[must_use]
    pub fn vm_busy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::VmBusy, message)
    }
    /// Shortcut for `unsupported`.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unsupported, message)
    }
    /// Shortcut for `state_conflict`.
    #[must_use]
    pub fn state_conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::StateConflict, message)
    }
    /// Shortcut for `resource_limit`.
    #[must_use]
    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ResourceLimit, message)
    }
    /// Shortcut for `invalid_argument`.
    #[must_use]
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }
    /// Shortcut for `not_found`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
    /// Shortcut for `infrastructure_error`.
    #[must_use]
    pub fn infrastructure(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InfrastructureError, message)
    }
    /// Shortcut for `unauthenticated`.
    #[must_use]
    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthenticated, message)
    }
}

impl fmt::Display for PublicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PublicError {}

/// Code of a protocol error (request not understood, no effect attempted).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolErrorCode {
    /// Message not decodable or not conforming to the schema.
    MalformedMessage,
    /// Unsupported protocol version.
    UnsupportedVersion,
    /// Unknown method or request type.
    UnknownMethod,
    /// Message exceeds the maximum allowed size.
    MessageTooLarge,
    /// Kept for contract compatibility but **not emitted**: a missing or invalid
    /// credential produces the operational error [`ErrorCode::Unauthenticated`]
    /// (see the module documentation).
    Unauthenticated,
}

impl ProtocolErrorCode {
    /// All codes, in frozen order.
    pub const ALL: [ProtocolErrorCode; 5] = [
        ProtocolErrorCode::MalformedMessage,
        ProtocolErrorCode::UnsupportedVersion,
        ProtocolErrorCode::UnknownMethod,
        ProtocolErrorCode::MessageTooLarge,
        ProtocolErrorCode::Unauthenticated,
    ];

    /// Stable name (same as the serialization).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ProtocolErrorCode::MalformedMessage => "malformed_message",
            ProtocolErrorCode::UnsupportedVersion => "unsupported_version",
            ProtocolErrorCode::UnknownMethod => "unknown_method",
            ProtocolErrorCode::MessageTooLarge => "message_too_large",
            ProtocolErrorCode::Unauthenticated => "unauthenticated",
        }
    }
}

/// Protocol error: distinct from operational errors (ROADMAP 9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolError {
    /// Stable code.
    pub code: ProtocolErrorCode,
    /// Message for humans, without secrets.
    pub message: String,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ProtocolError {}

/// Reply error: either operational or protocol, never mixed up.
///
/// JSON shape: `{"kind": "operational", ...PublicError}` or
/// `{"kind": "protocol", ...ProtocolError}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyError {
    /// Request understood but rejected or failed.
    Operational(PublicError),
    /// Request not understood: no effect attempted.
    Protocol(ProtocolError),
}

impl From<PublicError> for ReplyError {
    fn from(e: PublicError) -> Self {
        Self::Operational(e)
    }
}

impl From<ProtocolError> for ReplyError {
    fn from(e: ProtocolError) -> Self {
        Self::Protocol(e)
    }
}
