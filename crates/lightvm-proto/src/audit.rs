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
    /// Creation of a baseline with the VM off (F3).
    BaselineCreate,
    /// Restore of a baseline: the previous current state (disk or overlay
    /// and UEFI variables) is discarded (F3).
    BaselineRestore,
    /// Deletion of a baseline (F3).
    BaselineDelete,
    /// Session request of an agent (F4): received, or refused (agent access
    /// off, limits). Never the declared texts.
    SessionRequest,
    /// Session request denied by the user, expired or withdrawn (F4).
    SessionDeny,
    /// Lease renewal of an agent session (F4).
    SessionLease,
    /// Change of the input owner of a VM (F4).
    InputControl,
    /// Agent access switch turned on (F4).
    AgentAccessEnable,
    /// Agent access switch turned off (F4): pending requests denied, live
    /// sessions revoked.
    AgentAccessDisable,
    /// VM list read by an agent session (F4).
    VmList,
    /// VM status read by an agent session (F4).
    VmStatus,
    /// Screenshot bytes read by an agent session (F4, never the bytes).
    ScreenshotRead,
    /// Verdict on a session report: user judgement or automatic verifier (F5).
    SessionVerdict,
    /// Export of a session report to a file chosen by the user (F5, never
    /// the path).
    ReportExport,
    /// Deletion of a session report and its screenshots (F5).
    ReportDelete,
    /// Deletion of an imported medium from the registry (user only): a
    /// managed copy is removed with it, a file used in place is kept.
    MediaDelete,
    /// Automatic reset of a guest whose virtual CPUs stopped on an
    /// unrecoverable error (QEMU `internal-error`, for example a triple fault
    /// under WHPX), as hardware would reset: `succeeded` (reset and resumed),
    /// `failed` (QMP refused), `denied` with `resource_limit` (too many in a
    /// short time: the guest is left stopped for the user). Actor `system`.
    VmAutoReset,
    /// Import of a VMware VM by conversion (user only, ADR-0030): denied
    /// (checks of the `.vmx` and its disks, limits), failed (conversion,
    /// cancellation) or succeeded (VM created with its converted disk).
    /// Never the path of the source files.
    VmImport,
    /// User snapshot taken (ADR-0032, user only): with the VM off, or while
    /// it runs (disk state only, like a power cut at that instant).
    SnapshotTake,
    /// Revert of a VM to a user snapshot with the VM off (ADR-0032): disk,
    /// firmware state and configuration of the snapshot.
    SnapshotRevert,
    /// Deletion of a user snapshot (ADR-0032); its children keep their
    /// state and move to its parent.
    SnapshotDelete,
    /// Rename or new description of a user snapshot (ADR-0032; never the
    /// texts).
    SnapshotUpdate,
    /// Network of a run set up (ADR-0033): `network.mode`, and for the
    /// endpoint allowlist one event per endpoint with the host address it
    /// was resolved to at the start (`failed` if it could not be resolved:
    /// that endpoint refuses every connection).
    NetworkSetup,
    /// A guest connection through the endpoint allowlist ended (ADR-0033):
    /// endpoint, addresses and byte counts, never the content.
    NetworkConnection,
    /// A guest connection attempt refused by the endpoint allowlist
    /// (`denied`; the first ones of each run, later ones only counted).
    NetworkDenied,
    /// The endpoints allowed on a running VM changed (session started or
    /// ended, revocation, expiry, Emergency Stop): connections to endpoints
    /// no longer allowed were reset.
    NetworkPolicy,
}

/// Network details of a `network_*` audit event (ADR-0033). Addresses and
/// counts only: never data carried by the connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkAudit {
    /// Network mode of the run (`offline`, `nat`, `endpoints`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Endpoint name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Destination inside the guest network (`10.0.2.101:1433`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Host address the connection was opened to (or the endpoint
    /// resolved to).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Bytes from the guest to the endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_from_guest: Option<u64>,
    /// Bytes from the endpoint to the guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_to_guest: Option<u64>,
    /// How it ended or why it was refused (`closed`, `revoked`,
    /// `not_allowed`...), or the allowed endpoints for `network_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    /// Network details (`network_*` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkAudit>,
}
