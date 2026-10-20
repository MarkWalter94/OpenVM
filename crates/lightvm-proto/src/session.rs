//! Agent sessions, grants, leases, capabilities, test outcomes (ROADMAP 7.3, 9).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{GrantId, OperationId, PublicError, SessionId, UtcTimestamp, VmId};

/// Authorizable action on a VM. Serialized names follow the MCP catalog
/// (ROADMAP 9); `vm.input` covers mouse and keyboard.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum Capability {
    /// List filtered by the grant.
    #[serde(rename = "vm.list")]
    VmList,
    /// Status.
    #[serde(rename = "vm.status")]
    VmStatus,
    /// Start.
    #[serde(rename = "vm.start")]
    VmStart,
    /// Stop (ordinary or forced).
    #[serde(rename = "vm.stop")]
    VmStop,
    /// Reset: requires an explicit capability.
    #[serde(rename = "vm.reset")]
    VmReset,
    /// Screenshot.
    #[serde(rename = "vm.screenshot")]
    VmScreenshot,
    /// Mouse and keyboard.
    #[serde(rename = "vm.input")]
    VmInput,
    /// Checkpoint with the VM off: requires an explicit capability.
    #[serde(rename = "vm.snapshot")]
    VmSnapshot,
    /// Restore with the VM off.
    #[serde(rename = "vm.restore")]
    VmRestore,
    /// Authorized LightVM events and QEMU logs.
    #[serde(rename = "vm.logs")]
    VmLogs,
    /// Network endpoints of the VM's allowlist (ADR-0033, ADR-0034): the
    /// grant lists which ones ([`Grant::network_endpoints`]). Without it
    /// the session's VM runs without network.
    #[serde(rename = "network.endpoints")]
    NetworkEndpoints,
    #[serde(rename = "share.read")]
    ShareRead,
    #[serde(rename = "share.write")]
    ShareWrite,
    #[serde(rename = "clipboard")]
    Clipboard,
    #[serde(rename = "files.transfer")]
    FilesTransfer,
    #[serde(rename = "usb")]
    Usb,
    #[serde(rename = "ui.inspect")]
    UiInspect,
    #[serde(rename = "ui.act")]
    UiAct,
    #[serde(rename = "guest.exec")]
    GuestExec,
}

impl Capability {
    /// VM controls available before optional guest integrations.
    pub const VM_CONTROLS: [Capability; 11] = [
        Self::VmList,
        Self::VmStatus,
        Self::VmStart,
        Self::VmStop,
        Self::VmReset,
        Self::VmScreenshot,
        Self::VmInput,
        Self::VmSnapshot,
        Self::VmRestore,
        Self::VmLogs,
        Self::NetworkEndpoints,
    ];
    /// All capabilities.
    pub const ALL: [Capability; 19] = [
        Capability::ShareRead,
        Capability::ShareWrite,
        Capability::Clipboard,
        Capability::FilesTransfer,
        Capability::Usb,
        Capability::UiInspect,
        Capability::UiAct,
        Capability::GuestExec,
        Capability::VmList,
        Capability::VmStatus,
        Capability::VmStart,
        Capability::VmStop,
        Capability::VmReset,
        Capability::VmScreenshot,
        Capability::VmInput,
        Capability::VmSnapshot,
        Capability::VmRestore,
        Capability::VmLogs,
        Capability::NetworkEndpoints,
    ];

    /// Stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::VmList => "vm.list",
            Capability::VmStatus => "vm.status",
            Capability::VmStart => "vm.start",
            Capability::VmStop => "vm.stop",
            Capability::VmReset => "vm.reset",
            Capability::VmScreenshot => "vm.screenshot",
            Capability::VmInput => "vm.input",
            Capability::VmSnapshot => "vm.snapshot",
            Capability::VmRestore => "vm.restore",
            Capability::VmLogs => "vm.logs",
            Capability::NetworkEndpoints => "network.endpoints",
            Capability::ShareRead => "share.read",
            Capability::ShareWrite => "share.write",
            Capability::Clipboard => "clipboard",
            Capability::FilesTransfer => "files.transfer",
            Capability::Usb => "usb",
            Capability::UiInspect => "ui.inspect",
            Capability::UiAct => "ui.act",
            Capability::GuestExec => "guest.exec",
        }
    }

    /// `true` if the capability authorizes a mutation (blocked by the agent pause).
    #[must_use]
    pub const fn is_mutation(self) -> bool {
        matches!(
            self,
            Capability::VmStart
                | Capability::VmStop
                | Capability::VmReset
                | Capability::VmInput
                | Capability::VmSnapshot
                | Capability::VmRestore
                | Capability::ShareWrite
                | Capability::Clipboard
                | Capability::FilesTransfer
                | Capability::Usb
                | Capability::UiAct
                | Capability::GuestExec
        )
    }

    pub const fn is_integration(self) -> bool {
        matches!(
            self,
            Self::ShareRead
                | Self::ShareWrite
                | Self::Clipboard
                | Self::FilesTransfer
                | Self::Usb
                | Self::UiInspect
                | Self::UiAct
                | Self::GuestExec
        )
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ordered set of capabilities.
pub type CapabilitySet = BTreeSet<Capability>;

/// Quotas of a session or of a policy layer. Each field is an inclusive
/// maximum; the intersection takes the minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Quotas {
    /// Total operations allowed in the session.
    pub max_operations: u32,
    /// Total input events allowed in the session.
    pub max_input_events: u32,
    /// Total screenshots allowed in the session.
    pub max_screenshots: u32,
    /// Maximum size of a single screenshot in bytes.
    pub max_screenshot_bytes: u64,
    /// Operations queued per VM and not yet forwarded.
    pub max_queued_operations: u32,
}

impl Quotas {
    /// Quotas without restrictions of their own (neutral element of the intersection).
    pub const UNBOUNDED: Quotas = Quotas {
        max_operations: u32::MAX,
        max_input_events: u32::MAX,
        max_screenshots: u32::MAX,
        max_screenshot_bytes: u64::MAX,
        max_queued_operations: u32::MAX,
    };

    /// Field-by-field intersection (minimum).
    #[must_use]
    pub fn intersect(&self, other: &Quotas) -> Quotas {
        Quotas {
            max_operations: self.max_operations.min(other.max_operations),
            max_input_events: self.max_input_events.min(other.max_input_events),
            max_screenshots: self.max_screenshots.min(other.max_screenshots),
            max_screenshot_bytes: self.max_screenshot_bytes.min(other.max_screenshot_bytes),
            max_queued_operations: self.max_queued_operations.min(other.max_queued_operations),
        }
    }

    /// `true` if every field of `self` is ≤ the corresponding one of `other`.
    #[must_use]
    pub fn is_within(&self, other: &Quotas) -> bool {
        self.max_operations <= other.max_operations
            && self.max_input_events <= other.max_input_events
            && self.max_screenshots <= other.max_screenshots
            && self.max_screenshot_bytes <= other.max_screenshot_bytes
            && self.max_queued_operations <= other.max_queued_operations
    }
}

/// Monotonic revision of the applied policy (recorded in reports, ROADMAP 9).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct PolicyRevision(pub u64);

/// A policy layer: allowed capabilities and quotas. Used for global limits,
/// VM policy, session grant and host capacity; the effective policy is
/// the intersection of the four layers (ROADMAP 6.3, computed in `vm-core`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyLayer {
    /// Capabilities allowed by this layer.
    pub capabilities: CapabilitySet,
    /// Quotas of this layer.
    pub quotas: Quotas,
}

impl PolicyLayer {
    /// Neutral layer: all capabilities, unlimited quotas.
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            capabilities: Capability::ALL.into_iter().collect(),
            quotas: Quotas::UNBOUNDED,
        }
    }

    /// Layer that allows nothing.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            capabilities: CapabilitySet::new(),
            quotas: Quotas {
                max_operations: 0,
                max_input_events: 0,
                max_screenshots: 0,
                max_screenshot_bytes: 0,
                max_queued_operations: 0,
            },
        }
    }
}

/// Effective policy resulting from the intersection, with the applied revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectivePolicy {
    /// Effective capabilities.
    pub capabilities: CapabilitySet,
    /// Effective quotas.
    pub quotas: Quotas,
    /// Revision of the applied policy.
    pub revision: PolicyRevision,
}

/// Grant issued by the supervisor after the user's approval in the app (ROADMAP 9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Grant {
    #[serde(default, skip_serializing_if = "crate::ToolGrant::is_empty")]
    pub tools: crate::ToolGrant,
    /// Identifier.
    pub id: GrantId,
    /// Session it is bound to.
    pub session_id: SessionId,
    /// Allowed VMs (explicit allowlist; empty = no VM).
    pub vm_allowlist: BTreeSet<VmId>,
    /// Granted capabilities.
    pub capabilities: CapabilitySet,
    /// Absolute UTC expiry.
    pub expires_at: UtcTimestamp,
    /// Granted quotas.
    pub quotas: Quotas,
    /// Revision of the policy the grant was issued with.
    pub policy_revision: PolicyRevision,
    /// Network endpoints of the VM the session may use (names as in the VM
    /// settings, normalized to lower case), with
    /// [`Capability::NetworkEndpoints`]; empty otherwise (ADR-0033).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub network_endpoints: BTreeSet<String>,
}

impl Grant {
    /// Policy layer corresponding to the grant.
    #[must_use]
    pub fn as_layer(&self) -> PolicyLayer {
        PolicyLayer {
            capabilities: self.capabilities.clone(),
            quotas: self.quotas,
        }
    }
}

/// Maximum duration of a lease without a valid renewal (ROADMAP 7.3: ≤ 10 s).
pub const LEASE_MAX_MILLIS: i64 = 10_000;

/// Lease of a session: it must be renewed before it expires, otherwise the
/// session is revoked through the emergency path (ROADMAP 7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Lease {
    /// Session.
    pub session_id: SessionId,
    /// Last valid renewal.
    pub renewed_at: UtcTimestamp,
    /// Expiry: `renewed_at + duration`, with duration in `1..=LEASE_MAX_MILLIS`.
    pub expires_at: UtcTimestamp,
}

impl Lease {
    /// New lease; rejects zero, negative or over-[`LEASE_MAX_MILLIS`] durations.
    pub fn new(
        session_id: SessionId,
        renewed_at: UtcTimestamp,
        duration_millis: i64,
    ) -> Result<Self, PublicError> {
        if !(1..=LEASE_MAX_MILLIS).contains(&duration_millis) {
            return Err(PublicError::invalid_argument(
                "lease duration outside 1..=10000 ms",
            ));
        }
        let expires_at = renewed_at
            .checked_add_millis(duration_millis)
            .ok_or_else(|| PublicError::invalid_argument("lease instant out of range"))?;
        Ok(Self {
            session_id,
            renewed_at,
            expires_at,
        })
    }

    /// `true` if the lease is valid at instant `now` (expiry excluded).
    #[must_use]
    pub fn is_valid_at(&self, now: UtcTimestamp) -> bool {
        now < self.expires_at && self.expires_at.millis_since(self.renewed_at) <= LEASE_MAX_MILLIS
    }
}

/// State of an agent session.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Active.
    Active,
    /// Agent pause: no new mutation forwarded; observation allowed.
    Paused,
    /// Terminated (revocation, expiry, Emergency Stop, client loss...). Final.
    Terminated,
}

/// Termination cause of a session (recorded in the report, ROADMAP 9).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TerminationCause {
    /// Ordinary close from the app.
    Completed,
    /// Revoked by the user.
    Revoked,
    /// Emergency Stop.
    EmergencyStop,
    /// Grant expired.
    GrantExpired,
    /// Lease not renewed within the limit.
    LeaseExpired,
    /// Loss of the client or of the adapter.
    ClientLost,
    /// UI closed or crashed.
    UiClosed,
    /// Supervisor lost or restarted.
    SupervisorLost,
    /// Host resumed from sleep.
    HostResumed,
    /// VM failed or terminated during the session.
    VmTerminated,
    /// Baseline restore: the control session closes (ROADMAP 8).
    Restored,
}

/// Outcome of a test (ROADMAP 9). Completing the conversation does not
/// automatically produce `passed`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    /// Criteria verified.
    Passed,
    /// Criteria not met.
    Failed,
    /// No reliable verifier or insufficient evidence.
    Inconclusive,
    /// Cancelled by the user.
    Cancelled,
    /// Time ran out.
    TimedOut,
    /// Infrastructure failure, distinct from a model failure.
    InfrastructureError,
}

/// Report of a revocation or an Emergency Stop: distinguishes the discarded queue
/// from requests already forwarded (ROADMAP 7.3). It promises no cancellations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RevocationReport {
    /// Queued operations discarded without forwarding (no effect).
    pub discarded: Vec<OperationId>,
    /// Operations already forwarded to the backend: they may have produced effects.
    pub already_forwarded: Vec<OperationId>,
}

/// Session secret (32 bytes) issued by the supervisor. It is not an identifier:
/// it does not appear in logs, reports, process arguments nor in `Debug`.
///
/// In JSON (only on the authenticated IPC channel) it is a string of 64 hexadecimal digits.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken([u8; 32]);

impl SessionToken {
    /// Token from the 32 bytes generated by the supervisor with the OS generator.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Comparison in constant time with respect to the content.
    #[must_use]
    pub fn ct_eq(&self, other: &SessionToken) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }

    /// Bytes of the token (only for the OS credential store and the IPC channel).
    #[must_use]
    pub const fn expose_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

impl Serialize for SessionToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&crate::Sha256Digest::from_bytes(self.0).to_hex())
    }
}

impl<'de> Deserialize<'de> for SessionToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = Cow::<'de, str>::deserialize(deserializer)?;
        crate::Sha256Digest::parse_hex(&s)
            .map(|d| Self(*d.as_bytes()))
            .map_err(|_| serde::de::Error::custom("invalid session token"))
    }
}

impl JsonSchema for SessionToken {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SessionToken")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$",
            "writeOnly": true,
            "description": "Session secret, only on authenticated IPC; never in logs or reports."
        })
    }
}
