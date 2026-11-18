//! Standing grant: unattended agent access to one VM (ADR-0039).
//!
//! The user creates a standing grant in the app for **one** VM, with the
//! capabilities, network endpoints, quotas and lease a session may receive,
//! valid for at most [`MAX_STANDING_GRANT_S`]. It is stored in the catalog
//! (policy and the SHA-256 of its pairing credential only, never the
//! credential) and survives a restart of LightVM. A session request
//! presented with its pairing credential is approved by the supervisor
//! without a click when it conforms; any other request is denied, never
//! queued for a manual approval. At most one grant is live at a time.
//!
//! Sessions born from a standing grant end **without** stopping their VM on
//! expiry, lease expiry and client loss (ADR-0039, decision D1).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CapabilitySet, Quotas, ToolGrant, UtcTimestamp, VmId};

pub use crate::ids::StandingGrantId;

/// Longest lifetime of a standing grant, in seconds (7 days).
pub const MAX_STANDING_GRANT_S: u32 = 604_800;
/// Shortest lifetime of a standing grant, in seconds.
pub const MIN_STANDING_GRANT_S: u32 = 60;
/// Longest expected client name of a standing grant.
pub const MAX_STANDING_CLIENT_NAME_CHARS: usize = 64;

/// What the user grants when creating a standing grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StandingGrantSpec {
    /// The single VM of the grant.
    pub vm_id: VmId,
    /// Capabilities a session may receive (not empty).
    pub capabilities: CapabilitySet,
    /// Named Tools resources a session may receive.
    #[serde(default, skip_serializing_if = "ToolGrant::is_empty")]
    pub tools: ToolGrant,
    /// Network endpoints of the VM a session may receive with
    /// `network.endpoints` (ADR-0033).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_endpoints: Vec<String>,
    /// Quotas of each session; `None` = the default agent quotas.
    #[serde(default)]
    pub quotas: Option<Quotas>,
    /// Lease of each session in milliseconds; `None` = 10000.
    #[serde(default)]
    pub lease_ms: Option<u32>,
    /// Hand input control of the VM to each session at once.
    #[serde(default)]
    pub give_input: bool,
    /// Lifetime in seconds
    /// ([`MIN_STANDING_GRANT_S`]..=[`MAX_STANDING_GRANT_S`]).
    pub duration_s: u32,
    /// Client name the requests must declare, if set (plain text, compared
    /// exactly; it is declared by the client, not a proof of identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

/// Why a standing grant was revoked.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StandingRevokeCause {
    /// Revoked by the user in the app.
    User,
    /// Emergency Stop.
    EmergencyStop,
    /// Its VM was deleted.
    VmDeleted,
}

impl StandingRevokeCause {
    /// Stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::EmergencyStop => "emergency_stop",
            Self::VmDeleted => "vm_deleted",
        }
    }

    /// Parses [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "emergency_stop" => Some(Self::EmergencyStop),
            "vm_deleted" => Some(Self::VmDeleted),
            _ => None,
        }
    }
}

/// State of a standing grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StandingGrantState {
    /// Requests presented with its credential are approved when they conform.
    Active,
    /// The configuration of its VM changed after the grant was created or
    /// confirmed: requests are denied until the user confirms it again.
    Suspended,
    /// Revoked: its credential no longer works.
    Revoked {
        /// Cause.
        cause: StandingRevokeCause,
        /// Instant.
        at: UtcTimestamp,
    },
    /// Expired: its credential no longer works.
    Expired {
        /// Instant (its expiry).
        at: UtcTimestamp,
    },
}

impl StandingGrantState {
    /// `true` while active or suspended: its credential works and it is
    /// the single live grant.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Active | Self::Suspended)
    }
}

/// A standing grant (no secret).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StandingGrant {
    /// Identifier.
    pub grant_id: StandingGrantId,
    /// The single VM of the grant.
    pub vm_id: VmId,
    /// Capabilities a session may receive.
    pub capabilities: CapabilitySet,
    /// Named Tools resources a session may receive.
    #[serde(default, skip_serializing_if = "ToolGrant::is_empty")]
    pub tools: ToolGrant,
    /// Network endpoints a session may receive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_endpoints: Vec<String>,
    /// Quotas of each session.
    pub quotas: Quotas,
    /// Lease of each session in milliseconds.
    pub lease_ms: u32,
    /// Input control handed to each session.
    #[serde(default)]
    pub give_input: bool,
    /// Client name the requests must declare, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Expiry (at most `created_at` + [`MAX_STANDING_GRANT_S`]).
    pub expires_at: UtcTimestamp,
    /// Configuration revision of the VM at creation or at the last
    /// confirmation: a different revision suspends the grant.
    pub vm_config_revision: u64,
    /// State.
    pub state: StandingGrantState,
    /// Sessions approved automatically so far.
    #[serde(default)]
    pub sessions_approved: u32,
    /// Last automatic approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_at: Option<UtcTimestamp>,
}

/// What changed in a standing grant ([`crate::ipc::ServiceEvent::StandingGrant`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum StandingGrantChange {
    /// Created by the user.
    Created,
    /// Confirmed again by the user after a suspension.
    Confirmed,
    /// Suspended (the configuration of its VM changed).
    Suspended,
    /// Revoked.
    Revoked,
    /// Expired.
    Expired,
    /// A session was approved automatically.
    SessionApproved {
        /// The new session.
        session_id: crate::SessionId,
    },
    /// A request presented with its credential was denied.
    RequestDenied {
        /// Stable reason (`detail.reason` of the denial).
        reason: String,
    },
}
