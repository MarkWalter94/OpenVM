//! Envelope of the local IPC protocol between clients (UI, MCP adapter) and
//! `lightvm-service` (ROADMAP 6.1, 6.3, 7.3, 9; ADR-0002).
//!
//! The transport (named pipe with ACL on Windows, Unix socket on Linux), the
//! framing (4-byte big-endian length plus JSON) and the channel authentication
//! belong to the service; only the messages live here, with a JSON schema
//! generated from the same source ([`schemas`], files `schema/ipc_*.json`).
//!
//! Rules:
//! - the client's first message is [`ClientMessage::Hello`] with the offered
//!   versions and the channel credential; the service picks the first
//!   supported version in order of preference ([`SUPPORTED_PROTOCOL_VERSIONS`]);
//! - every request carries a [`RequestId`] chosen by the client and returned in the
//!   response; unknown operations cannot be deserialized
//!   (`malformed_message`, no effect);
//! - no field is interpreted as a host path or option: VMs are
//!   referenced by [`VmId`], media by registry [`crate::MediaId`]. Three
//!   exceptions, all reserved to the `ui` role and never to agent sessions:
//!   [`Request::ImportMedia`] carries the file the user chose with the native
//!   picker of the UI (opened read-only as a regular file, checked and hashed
//!   by the service, then used in place: ADR-0029),
//!   [`Request::PreviewVmwareImport`] / [`Request::ImportVmware`] carry the
//!   `.vmx` file chosen with the picker (read as untrusted input, its disks
//!   converted into the catalog and never modified: ADR-0030), and
//!   [`ConsoleTicket`] returns the path of the per-run display socket
//!   (ADR-0013);
//! - errors are [`ReplyError`]: operational ([`crate::PublicError`]) or
//!   protocol ([`crate::ProtocolError`]), never mixed up. Missing, malformed,
//!   unknown or revoked credential → `unauthenticated`; insufficient role
//!   → `permission_denied` (see [`crate::ErrorCode`]).

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};

use crate::report::{
    AgentBaselineView, LogEntry, ReportEvent, ReportFormat, SessionReport, SessionReportSummary,
    SessionVerifier,
};
use crate::{
    Admission, BaselineId, CapabilitySet, DisplayGeometry, Grant, HostCapacity, Label, Lease,
    MediaKind, MediaRecord, OperationId, OperationRecord, PROTOCOL_VERSION, Quotas, ReplyError,
    RevocationReport, RunId, Screenshot, SessionId, SessionState, SessionToken, Sha256Digest,
    SnapshotId, TerminationCause, TestOutcome, UtcTimestamp, VmConfig, VmId, VmName, VmRecord,
    VmRequest, VmState,
};

/// IPC protocol versions supported by the service, in order of preference.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[PROTOCOL_VERSION];

/// Identifier of a request chosen by the client and returned in the response.
pub type RequestId = u64;

/// Identifier of a connection assigned by the service at handshake.
pub type ConnectionId = u64;

/// Role associated with the channel credential.
///
/// `ui` corresponds to [`crate::Caller::User`] towards the core; `pairing` does not
/// grant permissions on VMs (ADR-0006): the agent session grant arrives
/// in F4 ([`crate::Grant`], [`crate::SessionToken`]).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Desktop interface that started the service.
    Ui,
    /// Client waiting for pairing (MCP adapter): no VM permissions.
    Pairing,
}

/// Messages from the client to the service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Mandatory first message.
    Hello {
        /// Request identifier.
        request_id: RequestId,
        /// Protocol versions offered by the client.
        protocol_versions: Vec<u32>,
        /// Channel credential: 32 bytes as 64 lowercase hexadecimal digits.
        /// It stays free text in the schema so that a malformed credential
        /// gives the same outcome (`unauthenticated`) as a missing or wrong one,
        /// without distinguishing the reasons. Secret: never in logs or reports.
        #[serde(default)]
        #[schemars(extend("writeOnly" = true))]
        credential: Option<String>,
        /// Client name, diagnostic only (truncated to 64 characters).
        #[serde(default)]
        client_name: Option<String>,
    },
    /// Typed request after the handshake.
    Request {
        /// Request identifier.
        request_id: RequestId,
        /// Requested operation.
        body: Request,
    },
}

/// Typed operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// User-only guest integration call; no host paths or commands.
    GuestTools {
        vm_id: VmId,
        run_id: RunId,
        call: crate::ToolCall,
    },
    /// Liveness check.
    Ping,
    /// Capabilities of the service and the backend.
    GetCapabilities,
    /// List of the catalog VMs (UI only).
    ListVms,
    /// Status of a VM (UI only).
    VmStatus {
        /// VM.
        vm_id: VmId,
    },
    /// Creation of a VM in the catalog (UI only; never from agent sessions).
    CreateVm {
        /// Requested configuration.
        config: VmConfig,
    },
    /// Change to the configuration of a stopped VM: resources and limits
    /// (UI only; the disk can only grow).
    UpdateVm {
        /// VM.
        vm_id: VmId,
        /// Expected revision: a concurrent change is rejected.
        expected_revision: u64,
        /// New full configuration.
        config: VmConfig,
    },
    /// Action on an existing VM through the core's queue (UI only in F1).
    /// The response arrives after the backend outcome, except for `stop`
    /// with a backend that stops in the background (QEMU): the response
    /// arrives when the graceful stop has started (operation `forwarded`, VM
    /// `stopping`) and the end is published as a `vm_state` event.
    SubmitVm {
        /// Request with idempotency key.
        request: VmRequest,
    },
    /// Subscription to state events (UI only).
    Subscribe,
    /// List of active connections (UI only).
    ListConnections,
    /// Revocation of a connection and its credential (UI only).
    RevokeConnection {
        /// Connection to revoke.
        connection_id: ConnectionId,
    },
    /// Issuing of a pairing credential to be delivered out of band (UI only).
    CreatePairingCredential,
    /// Emergency Stop: priority path, does not go through the queue (UI only).
    EmergencyStop,
    /// Fate of the VMs if the UI ends without a controlled shutdown (UI only).
    /// Takes effect immediately; the UI sends it again at every service start.
    SetParentLossPolicy {
        /// New policy.
        policy: ParentLossPolicy,
    },
    /// Controlled shutdown of the service (UI only).
    Shutdown,
    /// Approved media of the registry (UI only).
    ListMedia,
    /// Import of an ISO chosen by the user with the native picker of the UI
    /// (UI only, never from agent sessions): SHA-256 and registry row, the
    /// file is used where it is and never copied (ADR-0008, ADR-0029). The
    /// path is opened as a regular file without following links and
    /// checked by the service; QEMU later receives the path the system
    /// resolved for that file, re-verified (hash) before every start.
    ImportMedia {
        /// File chosen by the user.
        source_path: String,
        /// Kind of media.
        media_kind: MediaKind,
        /// Origin declared by the user (free text, never interpreted).
        declared_origin: String,
    },
    /// Deletion of an imported medium (UI only, never from agent sessions;
    /// the UI asks for confirmation): registry row removed; a read-only
    /// managed copy (media imported before ADR-0029) is removed with it
    /// through the catalog journal, a file used in place is never touched.
    /// Refused (`state_conflict`) while the configuration of a VM uses it as
    /// installation or data media, and (`vm_busy`) while a run holds a
    /// managed copy open.
    DeleteMedia {
        /// Medium of the registry.
        media_id: crate::MediaId,
    },
    /// Deletion of a stopped VM with its disk and run files (UI only).
    DeleteVm {
        /// VM.
        vm_id: VmId,
        /// Expected revision: a concurrent change is rejected.
        expected_revision: u64,
    },
    /// Console of a running VM (UI only): per-run display ticket (ADR-0013).
    /// The ticket is revoked when the run ends, on Emergency Stop and when
    /// the connection is revoked.
    OpenConsole {
        /// VM.
        vm_id: VmId,
    },
    /// Baselines of a VM (UI only, F3).
    ListBaselines {
        /// VM.
        vm_id: VmId,
    },
    /// Creation of a baseline with the VM off (UI only, F3; ROADMAP 8,
    /// ADR-0005): the VM's current disk and UEFI variables are materialized
    /// into a standalone, read-only, hashed baseline. Long operation (a
    /// `qemu-img convert` of the whole disk): it runs outside the supervisor
    /// thread, the VM is busy meanwhile (`vm_busy` for other mutations,
    /// [`VmBaselines::operation`] set) and the reply arrives when it ends.
    /// Cancelled by the Emergency Stop, by the revocation or closing of the
    /// connection and by the service shutdown (nothing is published).
    CreateBaseline {
        /// VM (must be `stopped`).
        vm_id: VmId,
        /// Label shown to the user.
        label: Label,
        /// The last run did not end with an observed clean guest shutdown
        /// (forced stop, Emergency Stop, crash, supervisor restart since):
        /// without this flag the creation is refused (`state_conflict`,
        /// detail `reason` = `unclean_shutdown`); with it the baseline is
        /// recorded as **not verified** (`clean_shutdown: false`) and is
        /// never promoted to verified later.
        #[serde(default)]
        allow_unclean: bool,
    },
    /// Restore of a baseline with the VM off (UI only, F3): the VM's current
    /// state (standalone disk or previous overlay, and UEFI variables) is
    /// **discarded** and replaced by a new QCOW2 overlay backed by the
    /// baseline plus a fresh copy of the baseline's UEFI variables. The
    /// baseline's hash is verified first. The guest restarts with a normal
    /// boot at the next start. The reply arrives when done, as for
    /// `create_baseline` (same cancellation rules).
    RestoreBaseline {
        /// VM (must be `stopped`).
        vm_id: VmId,
        /// Baseline of the same VM.
        baseline_id: BaselineId,
    },
    /// Deletion of a baseline (UI only, F3; the UI asks for confirmation).
    /// Refused if the baseline is in use (the VM's current state is an
    /// overlay of it) or protected.
    DeleteBaseline {
        /// VM (must be `stopped`).
        vm_id: VmId,
        /// Baseline of the same VM.
        baseline_id: BaselineId,
    },
    /// User snapshots of a VM as a tree (UI only, ADR-0032).
    ListSnapshots {
        /// VM.
        vm_id: VmId,
    },
    /// Takes a user snapshot (UI only, never from agent sessions;
    /// ADR-0032): an internal snapshot of the VM's own disk, plus a copy of
    /// its UEFI variables and of its configuration. With the VM off the
    /// state is consistent; while it runs only the disk is saved, as after
    /// a power cut at that instant (no memory: WHPX cannot save it).
    /// Refused while an agent session targets the VM or while the VM runs
    /// on an overlay of a baseline.
    TakeSnapshot {
        /// VM (`stopped` or `running`).
        vm_id: VmId,
        /// Name shown in the snapshot manager.
        name: Label,
        /// Optional description (at most [`MAX_SNAPSHOT_DESCRIPTION_CHARS`]
        /// characters, line breaks allowed, no other control characters).
        #[serde(default)]
        description: String,
    },
    /// Reverts the VM to a user snapshot (UI only, ADR-0032): VM `stopped`
    /// only, never a hidden shutdown. The disk, the UEFI variables and the
    /// configuration (except the name) of the snapshot come back; the
    /// current state since the snapshot is lost; the guest starts with a
    /// normal boot. The reply arrives when done.
    RevertSnapshot {
        /// VM (must be `stopped`).
        vm_id: VmId,
        /// Snapshot of the same VM.
        snapshot_id: SnapshotId,
    },
    /// Deletes a user snapshot (UI only, ADR-0032; VM `stopped` or
    /// `running`): the other snapshots keep their state, its children move
    /// to its parent.
    DeleteSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot of the same VM.
        snapshot_id: SnapshotId,
    },
    /// Renames a user snapshot or changes its description (UI only).
    UpdateSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot of the same VM.
        snapshot_id: SnapshotId,
        /// New name.
        name: Label,
        /// New description.
        #[serde(default)]
        description: String,
    },
    /// Thumbnail of a snapshot taken while the VM ran (UI only): a small
    /// PNG of the screen at that instant, if one was kept.
    GetSnapshotThumbnail {
        /// VM.
        vm_id: VmId,
        /// Snapshot of the same VM.
        snapshot_id: SnapshotId,
    },
    /// Promotes a snapshot taken with the VM off to a baseline (UI only,
    /// ADR-0032 and ADR-0005): the snapshot's disk state is materialized
    /// into a standalone, read-only, hashed baseline with the snapshot's
    /// UEFI variables and configuration. VM `stopped`; long like
    /// `create_baseline`. Refused for snapshots taken while running (not a
    /// verified state).
    PromoteSnapshot {
        /// VM (must be `stopped`).
        vm_id: VmId,
        /// Snapshot of the same VM, taken with the VM off.
        snapshot_id: SnapshotId,
        /// Label of the new baseline.
        label: Label,
    },
    /// Session request of an agent (`pairing` role only, F4): refused while
    /// the agent access switch is off. The reply carries the verification
    /// code the user compares in the app; the decision is awaited with
    /// [`Request::AwaitSession`].
    RequestSession {
        /// Client name declared by the agent (free text, at most
        /// [`MAX_CLIENT_NAME_CHARS`] characters kept, never interpreted).
        client_name: String,
        /// Purpose declared by the agent (free text, at most
        /// [`MAX_PURPOSE_CHARS`] characters kept, never interpreted).
        purpose: String,
        /// VMs the agent would like (a hint for the user).
        #[serde(default)]
        requested_vms: Vec<VmId>,
        /// Capabilities the agent would like (a hint for the user).
        #[serde(default)]
        requested_capabilities: CapabilitySet,
        /// Network endpoints (names of the VM's allowlist) the agent would
        /// like with `network.endpoints` (a hint for the user; at most
        /// [`crate::MAX_NETWORK_ENDPOINTS`] names kept).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requested_endpoints: Vec<String>,
    },
    /// Waits for the user's decision on a session request of this
    /// connection (`pairing` role only). Long: the reply arrives when the
    /// user approves or denies, or when the request expires.
    AwaitSession {
        /// Request.
        request_id: SessionRequestId,
    },
    /// Call of an agent session bound to this connection (`pairing` role
    /// only). Checked by the core on receipt and right before forwarding.
    Agent {
        /// Session credential.
        session: SessionAuth,
        /// Call.
        call: AgentCall,
    },
    /// State of the agent access switch (UI only).
    GetAgentAccess,
    /// Agent access switch (UI only): off at every service start. Turning it
    /// off denies the pending requests and revokes the live sessions.
    SetAgentAccess {
        /// New state.
        enabled: bool,
    },
    /// Pending session requests (UI only).
    ListSessionRequests,
    /// Approval of a session request (UI only).
    ApproveSession {
        /// Request.
        request_id: SessionRequestId,
        /// What the user grants.
        approval: SessionApproval,
    },
    /// Denial of a session request (UI only).
    DenySession {
        /// Request.
        request_id: SessionRequestId,
    },
    /// Agent sessions, live first (UI only).
    ListSessions,
    /// Agent pause (UI only): agent mutations refused, the VM keeps running.
    PauseSession {
        /// Session.
        session_id: SessionId,
    },
    /// Resume after a pause (UI only).
    ResumeSession {
        /// Session.
        session_id: SessionId,
    },
    /// Revocation (UI only): the session ends, its queue is discarded, the
    /// agent connection is closed; the VM is not stopped.
    RevokeSession {
        /// Session.
        session_id: SessionId,
    },
    /// Input ownership of a VM (UI only): take control back or hand it to an
    /// agent session. Pressed keys and buttons are released on every change.
    SetInputOwner {
        /// VM.
        vm_id: VmId,
        /// New owner.
        owner: InputOwner,
    },
    /// Session reports, newest first (UI only, F5). Reports survive service
    /// restarts; sessions do not.
    ListSessionReports,
    /// Header of a report, without events (UI only).
    GetSessionReport {
        /// Session.
        session_id: SessionId,
    },
    /// A page of the events of a report (UI only).
    GetSessionReportEvents {
        /// Session.
        session_id: SessionId,
        /// First event (0-based).
        offset: u32,
        /// Events, 1..=[`crate::report::MAX_REPORT_EVENTS_PAGE`].
        limit: u32,
    },
    /// Chunk of a stored screenshot of a report (UI only).
    GetReportScreenshotChunk {
        /// Session.
        session_id: SessionId,
        /// Screenshot index in the report.
        shot: u32,
        /// Chunk index, from 0.
        index: u32,
    },
    /// Explicit judgement of the local user on a session (UI only): sets the
    /// outcome with the rationale, attributed to the user.
    SetSessionVerdict {
        /// Session.
        session_id: SessionId,
        /// Outcome.
        outcome: TestOutcome,
        /// Rationale (1..=2000 characters, plain text).
        rationale: String,
    },
    /// Runs an automatic verifier on the session's evidence (UI only).
    VerifySession {
        /// Session.
        session_id: SessionId,
        /// Verifier.
        verifier: SessionVerifier,
    },
    /// Export of a report to a file chosen by the user with the native save
    /// dialog (UI only; MCP can never choose a destination).
    ExportSessionReport {
        /// Session.
        session_id: SessionId,
        /// Format.
        format: ReportFormat,
        /// Absolute path of the new file.
        destination: String,
        /// Replace an existing file (the user confirmed it).
        #[serde(default)]
        overwrite: bool,
        /// Also write the stored screenshots next to the file.
        #[serde(default)]
        include_screenshots: bool,
    },
    /// Deletion of the report and screenshots of a terminated session (UI only).
    DeleteSessionReport {
        /// Session.
        session_id: SessionId,
    },
    /// Examination of a VMware VM before its import (UI only, never from
    /// agent sessions; ADR-0030): `vmx_path` is the `.vmx` file the user
    /// chose with the native picker. The `.vmx` and the VMDK descriptors
    /// are read as untrusted input, the disk chain is checked with
    /// `qemu-img info`; nothing is written. The reply says what would be
    /// imported, what not, and the warnings.
    PreviewVmwareImport {
        /// `.vmx` file chosen by the user.
        vmx_path: String,
    },
    /// Import of a VMware VM by conversion (UI only, never from agent
    /// sessions; ADR-0030). The `.vmx` and its disks are examined again (the
    /// preview is not trusted), the storage budget is checked, then the
    /// current state of the disk (snapshot chain flattened) is converted
    /// into a new QCOW2 disk of the catalog; the VMware files are opened
    /// read-only and never modified. Long: progress is published as
    /// [`ServiceEvent::VmwareImportProgress`] and the reply arrives when the
    /// VM exists. One import at a time; cancelled by
    /// [`Request::CancelVmwareImport`], the Emergency Stop, the revocation or
    /// closing of the connection and the service shutdown (nothing is
    /// left behind).
    ImportVmware {
        /// `.vmx` file chosen by the user.
        vmx_path: String,
        /// Name of the new VM.
        name: VmName,
        /// Display adapter (VMware SVGA only for Windows guests).
        #[serde(default)]
        display: crate::DisplayAdapter,
    },
    /// Cancels the VMware import in progress (UI only).
    CancelVmwareImport,
}

impl Request {
    /// Stable name of the operation, for audit and diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::GuestTools { .. } => "guest_tools",
            Self::GetCapabilities => "get_capabilities",
            Self::ListVms => "list_vms",
            Self::VmStatus { .. } => "vm_status",
            Self::CreateVm { .. } => "create_vm",
            Self::UpdateVm { .. } => "update_vm",
            Self::SubmitVm { .. } => "submit_vm",
            Self::Subscribe => "subscribe",
            Self::ListConnections => "list_connections",
            Self::RevokeConnection { .. } => "revoke_connection",
            Self::CreatePairingCredential => "create_pairing_credential",
            Self::EmergencyStop => "emergency_stop",
            Self::SetParentLossPolicy { .. } => "set_parent_loss_policy",
            Self::Shutdown => "shutdown",
            Self::ListMedia => "list_media",
            Self::ImportMedia { .. } => "import_media",
            Self::DeleteMedia { .. } => "delete_media",
            Self::DeleteVm { .. } => "delete_vm",
            Self::OpenConsole { .. } => "open_console",
            Self::ListBaselines { .. } => "list_baselines",
            Self::CreateBaseline { .. } => "create_baseline",
            Self::RestoreBaseline { .. } => "restore_baseline",
            Self::DeleteBaseline { .. } => "delete_baseline",
            Self::ListSnapshots { .. } => "list_snapshots",
            Self::TakeSnapshot { .. } => "take_snapshot",
            Self::RevertSnapshot { .. } => "revert_snapshot",
            Self::DeleteSnapshot { .. } => "delete_snapshot",
            Self::UpdateSnapshot { .. } => "update_snapshot",
            Self::GetSnapshotThumbnail { .. } => "get_snapshot_thumbnail",
            Self::PromoteSnapshot { .. } => "promote_snapshot",
            Self::RequestSession { .. } => "request_session",
            Self::AwaitSession { .. } => "await_session",
            Self::Agent { .. } => "agent",
            Self::GetAgentAccess => "get_agent_access",
            Self::SetAgentAccess { .. } => "set_agent_access",
            Self::ListSessionRequests => "list_session_requests",
            Self::ApproveSession { .. } => "approve_session",
            Self::DenySession { .. } => "deny_session",
            Self::ListSessions => "list_sessions",
            Self::PauseSession { .. } => "pause_session",
            Self::ResumeSession { .. } => "resume_session",
            Self::RevokeSession { .. } => "revoke_session",
            Self::SetInputOwner { .. } => "set_input_owner",
            Self::ListSessionReports => "list_session_reports",
            Self::GetSessionReport { .. } => "get_session_report",
            Self::GetSessionReportEvents { .. } => "get_session_report_events",
            Self::GetReportScreenshotChunk { .. } => "get_report_screenshot_chunk",
            Self::SetSessionVerdict { .. } => "set_session_verdict",
            Self::VerifySession { .. } => "verify_session",
            Self::ExportSessionReport { .. } => "export_session_report",
            Self::DeleteSessionReport { .. } => "delete_session_report",
            Self::PreviewVmwareImport { .. } => "preview_vmware_import",
            Self::ImportVmware { .. } => "import_vmware",
            Self::CancelVmwareImport => "cancel_vmware_import",
        }
    }
}

/// Identifier of a session request, assigned by the service.
pub type SessionRequestId = u64;

/// Longest client name kept from [`Request::RequestSession`].
pub const MAX_CLIENT_NAME_CHARS: usize = 64;
/// Longest purpose kept from [`Request::RequestSession`].
pub const MAX_PURPOSE_CHARS: usize = 512;
/// Lifetime of a pending session request, in seconds.
pub const SESSION_REQUEST_TTL_S: u64 = 300;
/// Shortest session duration the user can approve, in seconds.
pub const MIN_SESSION_DURATION_S: u32 = 60;
/// Longest session duration the user can approve, in seconds.
pub const MAX_SESSION_DURATION_S: u32 = 86_400;
/// Shortest lease, in milliseconds.
pub const MIN_LEASE_MS: u32 = 1_000;
/// Hard cap of one agent screenshot (PNG bytes), whatever the quotas.
pub const AGENT_MAX_SCREENSHOT_BYTES: u64 = 8 * 1024 * 1024;
/// Raw bytes per screenshot chunk (base64 in the reply fits a 64 KiB frame).
pub const SCREENSHOT_CHUNK_BYTES: u32 = 32 * 1024;

/// Quotas of an approval that does not set them.
pub const AGENT_DEFAULT_QUOTAS: Quotas = Quotas {
    max_operations: 10_000,
    max_input_events: 200_000,
    max_screenshots: 2_000,
    max_screenshot_bytes: 4 * 1024 * 1024,
    max_queued_operations: 8,
};

/// Credential of an agent session, carried by every [`Request::Agent`].
/// `Debug` redacts the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionAuth {
    /// Session.
    pub session_id: SessionId,
    /// Session secret received in [`ResponseBody::SessionGranted`].
    pub token: SessionToken,
}

/// Call of an agent session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum AgentCall {
    GuestTools {
        vm_id: VmId,
        run_id: RunId,
        #[serde(rename = "command")]
        call: crate::ToolCall,
    },
    /// VMs of the grant (`vm.list`).
    ListVms,
    /// Status of a VM of the grant (`vm.status`).
    VmStatus {
        /// VM.
        vm_id: VmId,
    },
    /// Action on a VM through the core's queue: `start`, `stop`,
    /// `force_stop`, `reset`, `input` (screenshots use
    /// [`AgentCall::Screenshot`]).
    Submit {
        /// Request with idempotency key.
        request: VmRequest,
    },
    /// Screenshot (`vm.screenshot`): metadata in the reply, image bytes
    /// fetched with [`AgentCall::ScreenshotChunk`].
    Screenshot {
        /// Idempotency key.
        operation_id: OperationId,
        /// VM.
        vm_id: VmId,
        /// Current run of the VM.
        expected_run_id: RunId,
    },
    /// Chunk `index` of the last screenshot of the session.
    ScreenshotChunk {
        /// Screenshot operation.
        operation_id: OperationId,
        /// Chunk index, from 0.
        index: u32,
    },
    /// An operation of this session (`vm.status`).
    Operation {
        /// Operation.
        operation_id: OperationId,
    },
    /// Lease renewal: due before the lease expires (at most 10 s).
    RenewLease,
    /// Ordinary end of the session by the agent (the VM is not stopped).
    EndSession,
    /// Baselines of the VM of the grant (`vm.status`, F5).
    ListBaselines {
        /// VM.
        vm_id: VmId,
    },
    /// Checkpoint with the VM off (`vm.snapshot`, F5): a new baseline,
    /// never a replacement; refused unless the VM is `stopped` (no hidden
    /// shutdown). Long: the reply arrives when done
    /// ([`ResponseBody::BaselineCreated`]).
    Snapshot {
        /// Idempotency key of the request (recorded in the report).
        operation_id: OperationId,
        /// VM.
        vm_id: VmId,
        /// Label (1..=200 characters, plain text; stored with the prefix
        /// `agent: `).
        label: String,
    },
    /// Restore with the VM off (`vm.restore`, F5): only the session's
    /// initial baseline or one created by this session; refused unless the
    /// VM is `stopped`. Long ([`ResponseBody::BaselineRestored`]).
    Restore {
        /// Idempotency key of the request (recorded in the report).
        operation_id: OperationId,
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
    /// LightVM events of the session and QEMU diagnostics of the current run,
    /// host paths removed (`vm.logs`, F5).
    Logs {
        /// VM.
        vm_id: VmId,
        /// Only events after this sequence number.
        #[serde(default)]
        after_seq: Option<u64>,
        /// Entries, 1..=[`crate::report::MAX_LOG_ENTRIES`] (default 50).
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Declaration of the agent about its result (F5): recorded as a claim,
    /// never as the outcome.
    Declare {
        /// Outcome the agent claims.
        claimed_outcome: TestOutcome,
        /// Summary (at most 2000 characters kept, plain text).
        summary: String,
    },
    /// Note of the adapter for the report: MCP tool name and a bounded
    /// summary (never typed text).
    ToolNote {
        /// MCP tool name (at most 64 characters kept).
        tool: String,
        /// Outcome.
        ok: bool,
        /// Error code, if any (at most 64 characters kept).
        #[serde(default)]
        error_code: Option<String>,
        /// Summary (at most 512 bytes serialized, otherwise replaced).
        #[serde(default)]
        detail: serde_json::Value,
    },
    /// Summary of this session's report (only the own session).
    Report {
        /// Session (absent = this session).
        #[serde(default)]
        session_id: Option<SessionId>,
    },
}

impl AgentCall {
    /// Stable name of the call, for diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ListVms => "list_vms",
            Self::GuestTools { .. } => "guest_tools",
            Self::VmStatus { .. } => "vm_status",
            Self::Submit { .. } => "submit",
            Self::Screenshot { .. } => "screenshot",
            Self::ScreenshotChunk { .. } => "screenshot_chunk",
            Self::Operation { .. } => "operation",
            Self::RenewLease => "renew_lease",
            Self::EndSession => "end_session",
            Self::ListBaselines { .. } => "list_baselines",
            Self::Snapshot { .. } => "snapshot",
            Self::Restore { .. } => "restore",
            Self::Logs { .. } => "logs",
            Self::Declare { .. } => "declare",
            Self::ToolNote { .. } => "tool_note",
            Self::Report { .. } => "report",
        }
    }

    /// `true` if the call mutates a VM (refused while the session is paused).
    #[must_use]
    pub const fn is_mutation(&self) -> bool {
        if let Self::GuestTools { call, .. } = self {
            return !matches!(call, crate::ToolCall::UiTree { .. });
        }
        matches!(
            self,
            Self::Submit { .. } | Self::Snapshot { .. } | Self::Restore { .. }
        )
    }

    /// VM the call is about, if any (reports).
    #[must_use]
    pub fn vm_id(&self) -> Option<VmId> {
        match self {
            Self::GuestTools { vm_id, .. }
            | Self::VmStatus { vm_id }
            | Self::Screenshot { vm_id, .. }
            | Self::ListBaselines { vm_id }
            | Self::Snapshot { vm_id, .. }
            | Self::Restore { vm_id, .. }
            | Self::Logs { vm_id, .. } => Some(*vm_id),
            Self::Submit { request } => Some(request.vm_id),
            _ => None,
        }
    }

    /// Operation of the call, if any (reports).
    #[must_use]
    pub fn operation_id(&self) -> Option<OperationId> {
        match self {
            Self::Submit { request } => Some(request.operation_id),
            Self::Screenshot { operation_id, .. }
            | Self::Operation { operation_id }
            | Self::Snapshot { operation_id, .. }
            | Self::Restore { operation_id, .. } => Some(*operation_id),
            _ => None,
        }
    }
}

/// What the user grants when approving a session request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionApproval {
    #[serde(default, skip_serializing_if = "crate::ToolGrant::is_empty")]
    pub tools: crate::ToolGrant,
    /// The single VM of the session (MVP).
    pub vm_id: VmId,
    /// Granted capabilities (not empty).
    pub capabilities: CapabilitySet,
    /// Duration of the grant in seconds
    /// ([`MIN_SESSION_DURATION_S`]..=[`MAX_SESSION_DURATION_S`]).
    pub duration_s: u32,
    /// Quotas; `None` = [`AGENT_DEFAULT_QUOTAS`].
    #[serde(default)]
    pub quotas: Option<Quotas>,
    /// Lease in milliseconds ([`MIN_LEASE_MS`]..=10000); `None` = 10000.
    #[serde(default)]
    pub lease_ms: Option<u32>,
    /// Hand input control of the VM to the agent at once.
    #[serde(default)]
    pub give_input: bool,
    /// Network endpoints of the VM granted with `network.endpoints`
    /// (names of [`crate::VmConfig::endpoints`]; required non-empty with
    /// that capability, empty without it). ADR-0033.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_endpoints: Vec<String>,
}

/// Owner of the input of a VM: the user or one agent session, never both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputOwner {
    /// The local user (default).
    #[default]
    User,
    /// An agent session.
    Agent {
        /// Session.
        session_id: SessionId,
    },
}

/// Pending session request, for the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionRequestInfo {
    /// Request.
    pub request_id: SessionRequestId,
    /// Agent connection.
    pub connection_id: ConnectionId,
    /// Client name declared by the agent (plain text).
    pub client_name: String,
    /// Peer PID reported by the operating system, diagnostic only.
    pub peer_pid: Option<u32>,
    /// Purpose declared by the agent (plain text).
    pub purpose: String,
    /// VMs requested (hint).
    pub requested_vms: Vec<VmId>,
    /// Capabilities requested (hint).
    pub requested_capabilities: CapabilitySet,
    /// Network endpoints requested (hint, plain text).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_endpoints: Vec<String>,
    /// Code shown to the agent and in the app.
    pub verification_code: String,
    /// Instant of the request.
    pub requested_at: UtcTimestamp,
    /// The request expires at this instant if not decided.
    pub expires_at: UtcTimestamp,
}

/// Quota consumption of a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionUsage {
    /// Accepted operations.
    pub operations: u32,
    /// Accepted input events.
    pub input_events: u32,
    /// Accepted screenshots.
    pub screenshots: u32,
}

/// Agent session, for the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    /// Named Tools resources granted to this session.
    #[serde(default, skip_serializing_if = "crate::ToolGrant::is_empty")]
    pub tools: crate::ToolGrant,
    /// Session.
    pub session_id: SessionId,
    /// VM of the grant.
    pub vm_id: VmId,
    /// Agent connection the session is bound to (absent once terminated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<ConnectionId>,
    /// Client name declared by the agent (plain text).
    pub client_name: String,
    /// Purpose declared by the agent (plain text).
    pub purpose: String,
    /// State.
    pub state: SessionState,
    /// Termination cause, if terminated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationCause>,
    /// Granted capabilities.
    pub capabilities: CapabilitySet,
    /// Granted network endpoints (`network.endpoints`, ADR-0033).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_endpoints: Vec<String>,
    /// Granted quotas.
    pub quotas: Quotas,
    /// Consumption.
    pub usage: SessionUsage,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Expiry of the grant.
    pub expires_at: UtcTimestamp,
    /// Expiry of the current lease.
    pub lease_expires_at: UtcTimestamp,
    /// `true` if the session owns the input of its VM.
    pub input_owner: bool,
}

/// A VM as seen by an agent session (no host details).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentVmView {
    /// VM.
    pub vm_id: VmId,
    /// Name.
    pub name: VmName,
    /// State.
    pub state: VmState,
    /// Current run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Display geometry, if known (for pointer coordinates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<DisplayGeometry>,
    /// `true` if this session owns the input of the VM.
    pub input_owner: bool,
    /// Network of the VM for this session (ADR-0033): the granted
    /// endpoints with the address and port to use inside the guest. Empty
    /// = the VM runs without network for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_endpoints: Vec<AgentEndpointView>,
}

/// A granted network endpoint as the agent sees it: what to use inside the
/// guest (never the host side).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentEndpointView {
    /// DNS name inside the guest.
    pub name: String,
    /// Virtual address inside the guest.
    pub virtual_ip: std::net::Ipv4Addr,
    /// TCP port.
    pub port: u16,
}

/// Outcome of a session request, in [`ServiceEvent::SessionRequestClosed`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SessionRequestOutcome {
    /// Approved: a session was created.
    Approved,
    /// Denied by the user (or by the agent access switch).
    Denied,
    /// Not decided in time.
    Expired,
    /// The agent connection closed.
    Withdrawn,
}

/// Messages accepted by the emergency endpoint (`lightvm-service
/// --emergency-stop`): one verb only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EmergencyCommand {
    /// Emergency Stop of every VM and agent session.
    EmergencyStop,
}

/// Name of the file, in the catalog directory, where the running service
/// publishes its emergency endpoint (not a secret).
pub const EMERGENCY_ENDPOINT_FILE: &str = "emergency.endpoint";

/// Summary of a VM for list and status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmSummary {
    /// VM.
    pub vm_id: VmId,
    /// Displayed name.
    pub name: VmName,
    /// Current state as seen by the core (derived from the process and QMP).
    pub state: VmState,
    /// Current run, if a process exists, or the last finished one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Current configuration (resources and limits), for the VM settings.
    pub config: VmConfig,
    /// Configuration revision, to be given in `update_vm`.
    pub config_revision: u64,
    /// Space occupied on the host by the VM's files (disk, firmware state),
    /// if measurable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_used_bytes: Option<u64>,
    /// Baselines and current state of the VM (F3).
    #[serde(default)]
    pub baselines: VmBaselines,
    /// User snapshots of the VM (ADR-0032). Additive field: older clients
    /// ignore it.
    #[serde(default)]
    pub snapshots: VmSnapshots,
    /// Sub-status of a start in progress (state `starting`): verification of
    /// the approved media and of the baseline before QEMU is launched
    /// (ADR-0008, ADR-0025). Absent outside a start, and for a start with
    /// nothing to verify. Additive field: older clients ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<StartProgress>,
    /// The guest's virtual CPUs stopped on an error and stay stopped (state
    /// `running`: the QEMU process is alive, the guest is not). Absent while
    /// the guest runs. Additive field: older clients ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpu_halt: Option<VcpuHalt>,
}

/// Guest whose virtual CPUs stopped and were not restarted automatically, in
/// [`VmSummary::vcpu_halt`]. A reset (`reset`) restarts the guest; a forced
/// stop ends the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcpuHalt {
    /// Why the virtual CPUs stopped.
    pub reason: VcpuHaltReason,
    /// Automatic resets done in the recent window before the service gave up
    /// (0 when the reason is not reset automatically).
    pub automatic_resets: u32,
}

/// Cause of a [`VcpuHalt`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VcpuHaltReason {
    /// Unrecoverable virtual CPU error reported by the accelerator (QEMU
    /// run state `internal-error`; for example a guest triple fault under
    /// WHPX, which real hardware would answer with a reset).
    InternalError,
    /// The guest reported a panic (QEMU run state `guest-panicked`).
    GuestPanicked,
}

/// Progress of the checks of a start, in [`VmSummary::start`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartProgress {
    /// What the service is doing.
    pub phase: StartPhase,
    /// Bytes hashed so far, all files of the start together.
    pub verified_bytes: u64,
    /// Bytes to hash in total (approved media plus baseline files).
    pub total_bytes: u64,
}

/// Phase of a start before the VM process exists.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StartPhase {
    /// SHA-256 of the approved media (installation and data ISO) recomputed
    /// through the protected handles (UI: "Verifying media…").
    VerifyingMedia,
    /// SHA-256 of the baseline the current state is an overlay of (base disk
    /// and UEFI variables) recomputed through the guard handles (UI:
    /// "Verifying baseline…").
    VerifyingBaseline,
    /// Checks done: QEMU is being launched.
    Launching,
}

/// Baseline facts of a VM, in [`VmSummary`] (F3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmBaselines {
    /// Baseline the VM's current state is an overlay of (set by
    /// `restore_baseline`); `None` = the VM runs on its standalone disk
    /// (never restored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<BaselineId>,
    /// Number of baselines of the VM.
    #[serde(default)]
    pub count: u32,
    /// Bytes used on the host by the baselines (disk images and UEFI
    /// variables).
    #[serde(default)]
    pub bytes: u64,
    /// Bytes used on the host by the current overlay and its UEFI variables
    /// (0 on the standalone disk).
    #[serde(default)]
    pub overlay_bytes: u64,
    /// Storage operation in progress on the VM: the VM is busy until it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<StorageOperation>,
}

/// Long storage operation in progress on a VM.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StorageOperation {
    /// `create_baseline`.
    CreateBaseline,
    /// `restore_baseline`.
    RestoreBaseline,
    /// `take_snapshot` (ADR-0032).
    TakeSnapshot,
    /// `revert_snapshot` (ADR-0032).
    RevertSnapshot,
    /// `delete_snapshot` (ADR-0032). (`promote_snapshot` shows as
    /// `create_baseline`.)
    DeleteSnapshot,
}

/// Longest description of a user snapshot, in characters.
pub const MAX_SNAPSHOT_DESCRIPTION_CHARS: usize = 1000;

/// Checks the description of a user snapshot: at most
/// [`MAX_SNAPSHOT_DESCRIPTION_CHARS`] characters, line breaks and tabs
/// allowed, no other control characters. Text only, never interpreted.
pub fn validate_snapshot_description(s: &str) -> Result<(), &'static str> {
    if s.chars().count() > MAX_SNAPSHOT_DESCRIPTION_CHARS {
        return Err("description too long (at most 1000 characters)");
    }
    if s.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err("control characters not allowed in the description");
    }
    Ok(())
}

/// Largest snapshot thumbnail kept, in bytes of PNG (fits one IPC frame).
pub const MAX_SNAPSHOT_THUMBNAIL_BYTES: usize = 40 * 1024;

/// User snapshot facts of a VM, in [`VmSummary`] (ADR-0032).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmSnapshots {
    /// Number of user snapshots.
    #[serde(default)]
    pub count: u32,
    /// Snapshot the current state descends from ("you are here"), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<SnapshotId>,
}

/// User snapshot of a VM (ADR-0032), for the snapshot manager.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotSummary {
    /// Snapshot.
    pub snapshot_id: SnapshotId,
    /// Owning VM.
    pub vm_id: VmId,
    /// Snapshot this one descends from; `None` = a root of the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SnapshotId>,
    /// Name given by the user.
    pub name: Label,
    /// Description given by the user (may be empty).
    #[serde(default)]
    pub description: String,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Taken while the VM ran: the disk state is like after a power cut at
    /// that instant (crash-consistent); `false` = taken with the VM off.
    pub taken_while_running: bool,
    /// The VM was off after an observed clean guest shutdown (or had never
    /// run) when the snapshot was taken: it can become a verified baseline.
    pub clean_shutdown: bool,
    /// The snapshot carries a copy of the UEFI variables.
    pub firmware_saved: bool,
    /// A thumbnail of the screen is available
    /// ([`Request::GetSnapshotThumbnail`]).
    pub has_thumbnail: bool,
    /// Disk size of the VM at that moment (GiB), from the saved
    /// configuration.
    pub disk_gib: u64,
    /// VM configuration revision when the snapshot was taken.
    pub config_revision: u64,
    /// Bytes of the snapshot's own files on the host (UEFI variables and
    /// thumbnail). The disk clusters the snapshot keeps live inside the VM's
    /// disk and are counted in the VM's disk usage.
    pub files_bytes: u64,
}

/// User snapshots of a VM as a tree, in [`ResponseBody::Snapshots`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotTree {
    /// Snapshots, oldest first; parents always before their children.
    #[serde(default)]
    pub snapshots: Vec<SnapshotSummary>,
    /// Snapshot the current state descends from ("you are here"): the last
    /// one taken or reverted to; `None` if the current state descends from
    /// no snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<SnapshotId>,
    /// Snapshots are available for this VM now (it runs on its own disk);
    /// otherwise `unavailable_reason` says why.
    pub available: bool,
    /// Why snapshots cannot be taken or reverted now (agent session on the
    /// VM, VM on an overlay of a baseline, TPM guest...), for the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// Baseline of a VM, for `list_baselines` and `create_baseline` (F3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BaselineSummary {
    /// Baseline.
    pub baseline_id: BaselineId,
    /// Owning VM.
    pub vm_id: VmId,
    /// Label given at creation.
    pub label: Label,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// `true` if created after an observed clean guest shutdown (or before
    /// any run); `false` = **not verified**: created after a forced or
    /// unobserved stop with explicit consent (`allow_unclean`).
    pub clean_shutdown: bool,
    /// Protected baseline (initial baseline of an agent session, F4):
    /// cannot be deleted.
    pub protected: bool,
    /// Baseline the source state derived from (the VM was running on an
    /// overlay of it when this baseline was created); `None` = created from
    /// the standalone disk. Informational only: every baseline is a
    /// standalone image (no backing chain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<BaselineId>,
    /// VM configuration revision at creation.
    pub config_revision: u64,
    /// Virtual size of the disk.
    pub virtual_size_bytes: u64,
    /// Bytes on the host (disk image and UEFI variables).
    pub size_bytes: u64,
    /// SHA-256 of the disk image, verified before every restore and start.
    pub disk_sha256: Sha256Digest,
    /// SHA-256 of the UEFI variables, if the baseline has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_sha256: Option<Sha256Digest>,
    /// The VM's current state is an overlay of this baseline.
    pub current: bool,
    /// In use (current overlay or an open run): `delete_baseline` is refused.
    pub in_use: bool,
}

/// Per-run display ticket for the console (ADR-0013, ROADMAP 6.4).
///
/// The UI connects to `socket_path` (a Unix socket in a directory private to
/// the user), writes `token` followed by `\n` and then speaks RFB. The
/// supervisor forwards the stream to QEMU's display only after checking the
/// token; the ticket is distinct from the QMP channel and from the IPC
/// credential, valid for one run, and revoked with it.
///
/// `Debug` redacts the token (it ends up in logs and panic messages).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleTicket {
    /// VM.
    pub vm_id: VmId,
    /// Run the ticket is valid for.
    pub run_id: RunId,
    /// Display socket to connect to.
    pub socket_path: String,
    /// Display credential in hexadecimal: never logged.
    #[schemars(extend("writeOnly" = true))]
    pub token: String,
    /// Display geometry, if already known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<DisplayGeometry>,
}

impl std::fmt::Debug for ConsoleTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleTicket")
            .field("vm_id", &self.vm_id)
            .field("run_id", &self.run_id)
            .field("socket_path", &self.socket_path)
            .field("token", &"<redacted>")
            .field("geometry", &self.geometry)
            .finish()
    }
}

/// Capabilities reported by `get_capabilities`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    /// Supported protocol versions.
    pub protocol_versions: Vec<u32>,
    /// Backend name.
    pub backend: String,
    /// Available accelerator, if detected.
    pub accelerator: Option<String>,
    /// Byte limit per frame.
    pub max_frame_bytes: u32,
    /// Active policy on UI loss.
    #[serde(default)]
    pub parent_loss_policy: ParentLossPolicy,
    /// Capacity of the host as measured by the backend, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostCapacity>,
}

/// Minimum allowed wait for the orderly shutdown on UI loss.
pub const MIN_PARENT_LOSS_POWERDOWN_S: u32 = 1;
/// Maximum allowed wait for the orderly shutdown on UI loss.
pub const MAX_PARENT_LOSS_POWERDOWN_S: u32 = 300;

/// Fate of the VMs when the UI that started the service ends without a
/// controlled shutdown (crash, forced termination). ADR-0015 part B.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentLossPolicy {
    /// Emergency Stop of all VMs, then shutdown of the service (default).
    #[default]
    KillVms,
    /// Orderly (ACPI) shutdown with a bounded wait, then `quit` and
    /// termination; finally shutdown of the service.
    ShutdownVms {
        /// Wait for the ACPI shutdown per VM, in seconds
        /// ([`MIN_PARENT_LOSS_POWERDOWN_S`]..=[`MAX_PARENT_LOSS_POWERDOWN_S`]).
        powerdown_s: u32,
    },
    /// The VMs stay on and the service stays active without a UI.
    KeepVms,
}

impl ParentLossPolicy {
    /// Validation of the limits.
    pub fn validate(self) -> Result<Self, crate::PublicError> {
        match self {
            Self::ShutdownVms { powerdown_s }
                if !(MIN_PARENT_LOSS_POWERDOWN_S..=MAX_PARENT_LOSS_POWERDOWN_S)
                    .contains(&powerdown_s) =>
            {
                Err(crate::PublicError::invalid_argument(format!(
                    "powerdown_s outside the range {MIN_PARENT_LOSS_POWERDOWN_S}..={MAX_PARENT_LOSS_POWERDOWN_S}"
                )))
            }
            p => Ok(p),
        }
    }
}

/// Active connection, for `list_connections`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionInfo {
    /// Connection identifier.
    pub connection_id: ConnectionId,
    /// Role of the credential.
    pub role: Role,
    /// Peer PID reported by the operating system, diagnostic only.
    pub peer_pid: Option<u32>,
    /// Name declared by the client, diagnostic only.
    pub client_name: Option<String>,
}

/// Outcome of the Emergency Stop reported to the client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmergencyStopReport {
    /// Non-UI connections revoked.
    pub revoked_connections: u32,
    /// Processes whose end the backend observed.
    pub processes_terminated: u32,
    /// True only if the end of all processes was observed.
    pub completed: bool,
}

/// Results of the operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseBody {
    GuestTools {
        reply: crate::ToolReply,
    },
    /// Response to `ping`.
    Pong,
    /// Response to `get_capabilities`.
    Capabilities(Capabilities),
    /// Response to `list_vms`.
    VmList {
        /// Visible VMs.
        vms: Vec<VmSummary>,
    },
    /// Response to `vm_status`.
    Vm {
        /// Status of the VM.
        vm: VmSummary,
    },
    /// Response to `create_vm`.
    VmCreated {
        /// Created VM, with identifier assigned by the core.
        vm: VmRecord,
    },
    /// Response to `update_vm`: VM with the new revision.
    VmUpdated {
        /// Updated VM.
        vm: VmRecord,
    },
    /// Response to `submit_vm`: acceptance and recorded outcome.
    Operation {
        /// Acceptance (or idempotent duplicate).
        admission: Admission,
        /// Record of the operation after execution, if available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<OperationRecord>,
    },
    /// Subscription accepted.
    Subscribed,
    /// Response to `list_connections`.
    Connections {
        /// Active connections.
        connections: Vec<ConnectionInfo>,
    },
    /// Connection revoked.
    Revoked {
        /// Revoked connection.
        connection_id: ConnectionId,
    },
    /// Pairing credential issued: it must be delivered out of band, never recorded.
    PairingCredential {
        /// Credential in hexadecimal.
        #[schemars(extend("writeOnly" = true))]
        credential: String,
        /// Expiry in seconds.
        expires_in_s: u64,
    },
    /// Outcome of the Emergency Stop.
    EmergencyStop(EmergencyStopReport),
    /// UI loss policy in force after `set_parent_loss_policy`.
    ParentLossPolicy {
        /// Active policy.
        policy: ParentLossPolicy,
    },
    /// Shutdown accepted.
    ShuttingDown,
    /// Response to `list_media`.
    MediaList {
        /// Approved media.
        media: Vec<MediaRecord>,
    },
    /// Response to `import_media`.
    MediaImported {
        /// New registry entry.
        media: MediaRecord,
    },
    /// Response to `delete_media`.
    MediaDeleted {
        /// Deleted medium.
        media_id: crate::MediaId,
    },
    /// Response to `delete_vm`.
    VmDeleted {
        /// Deleted VM.
        vm_id: VmId,
    },
    /// Response to `open_console`.
    Console {
        /// Display ticket.
        ticket: ConsoleTicket,
    },
    /// Response to `list_baselines`, oldest first.
    Baselines {
        /// VM.
        vm_id: VmId,
        /// Baselines of the VM.
        baselines: Vec<BaselineSummary>,
    },
    /// Response to `create_baseline`.
    BaselineCreated {
        /// New baseline.
        baseline: BaselineSummary,
    },
    /// Response to `restore_baseline`: the VM's current state is now a new
    /// overlay of the baseline.
    BaselineRestored {
        /// VM.
        vm_id: VmId,
        /// Restored baseline.
        baseline_id: BaselineId,
    },
    /// Response to `delete_baseline`.
    BaselineDeleted {
        /// VM.
        vm_id: VmId,
        /// Deleted baseline.
        baseline_id: BaselineId,
    },
    /// Response to `list_snapshots`.
    Snapshots {
        /// VM.
        vm_id: VmId,
        /// Snapshot tree.
        tree: SnapshotTree,
    },
    /// Response to `take_snapshot` and `update_snapshot`.
    Snapshot {
        /// The snapshot.
        snapshot: SnapshotSummary,
    },
    /// Response to `revert_snapshot`: the VM is now at the snapshot.
    SnapshotReverted {
        /// VM.
        vm_id: VmId,
        /// Snapshot reverted to.
        snapshot_id: SnapshotId,
        /// Configuration revision after the revert.
        config_revision: u64,
    },
    /// Response to `delete_snapshot`.
    SnapshotDeleted {
        /// VM.
        vm_id: VmId,
        /// Deleted snapshot.
        snapshot_id: SnapshotId,
    },
    /// Response to `get_snapshot_thumbnail`.
    SnapshotThumbnail {
        /// Snapshot.
        snapshot_id: SnapshotId,
        /// PNG, standard base64 with padding; `None` if none was kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        png_base64: Option<String>,
    },
    /// Response to `request_session`.
    SessionRequested {
        /// Request, for `await_session`.
        request_id: SessionRequestId,
        /// Code the user compares in the app.
        verification_code: String,
        /// Lifetime of the request.
        expires_in_s: u64,
    },
    /// Response to `await_session`: session approved and bound to this
    /// connection.
    SessionGranted {
        /// Session.
        session_id: SessionId,
        /// Session secret: never logged.
        token: SessionToken,
        /// Grant issued by the supervisor.
        grant: Grant,
        /// First lease.
        lease: Lease,
    },
    /// Agent access switch.
    AgentAccess {
        /// State.
        enabled: bool,
    },
    /// Response to `list_session_requests`.
    SessionRequests {
        /// Pending requests, oldest first.
        requests: Vec<SessionRequestInfo>,
    },
    /// A session (approval, pause, resume).
    Session {
        /// Session.
        session: SessionInfo,
    },
    /// Response to `deny_session`.
    SessionDenied {
        /// Request.
        request_id: SessionRequestId,
    },
    /// Response to `list_sessions`.
    Sessions {
        /// Live sessions first, then the most recent terminated ones.
        sessions: Vec<SessionInfo>,
    },
    /// Response to `revoke_session`.
    SessionRevoked {
        /// Session.
        session_id: SessionId,
        /// Discarded queue and requests already forwarded.
        report: RevocationReport,
    },
    /// Response to `set_input_owner`.
    InputOwner {
        /// VM.
        vm_id: VmId,
        /// Owner now.
        owner: InputOwner,
    },
    /// Response to the agent call `list_vms`.
    AgentVms {
        /// VMs of the grant.
        vms: Vec<AgentVmView>,
    },
    /// Response to the agent call `vm_status`.
    AgentVm {
        /// VM.
        vm: AgentVmView,
    },
    /// Response to the agent call `screenshot`.
    AgentScreenshot {
        /// Operation record.
        operation: OperationRecord,
        /// Metadata (VM, run, geometry, SHA-256, size).
        screenshot: Screenshot,
        /// Raw bytes per chunk.
        chunk_bytes: u32,
        /// Number of chunks to fetch.
        chunks: u32,
    },
    /// Response to the agent call `screenshot_chunk`.
    ScreenshotChunk {
        /// Screenshot operation.
        operation_id: OperationId,
        /// Chunk index.
        index: u32,
        /// Number of chunks.
        chunks: u32,
        /// Bytes of the chunk, standard base64 with padding.
        data: String,
    },
    /// Response to the agent call `operation`.
    OperationStatus {
        /// Operation record.
        operation: OperationRecord,
    },
    /// Response to the agent call `renew_lease`.
    Lease {
        /// New lease.
        lease: Lease,
    },
    /// Response to the agent call `end_session`.
    SessionEnded {
        /// Session.
        session_id: SessionId,
        /// Discarded queue and requests already forwarded.
        report: RevocationReport,
    },
    /// Response to `list_session_reports`.
    SessionReports {
        /// Reports, newest first.
        reports: Vec<SessionReportSummary>,
    },
    /// A report header (events empty: read them by page).
    SessionReport {
        /// Report.
        report: Box<SessionReport>,
    },
    /// Response to `get_session_report_events`.
    SessionReportEvents {
        /// Session.
        session_id: SessionId,
        /// Events recorded in total.
        total: u32,
        /// First event of the page.
        offset: u32,
        /// Events.
        events: Vec<ReportEvent>,
    },
    /// Response to `get_report_screenshot_chunk`.
    ReportScreenshotChunk {
        /// Session.
        session_id: SessionId,
        /// Screenshot index.
        shot: u32,
        /// Chunk index.
        index: u32,
        /// Number of chunks.
        chunks: u32,
        /// SHA-256 of the whole PNG.
        sha256: Sha256Digest,
        /// Bytes of the chunk, standard base64 with padding.
        data: String,
    },
    /// Response to `export_session_report`.
    SessionReportExported {
        /// Session.
        session_id: SessionId,
        /// Format.
        format: ReportFormat,
        /// Bytes written (report file).
        bytes: u64,
        /// SHA-256 of the report file.
        sha256: Sha256Digest,
        /// Screenshots written next to it.
        screenshots: u32,
    },
    /// Response to `delete_session_report`.
    SessionReportDeleted {
        /// Session.
        session_id: SessionId,
    },
    /// Response to the agent call `list_baselines`.
    AgentBaselines {
        /// VM.
        vm_id: VmId,
        /// Baselines, oldest first.
        baselines: Vec<AgentBaselineView>,
    },
    /// Response to the agent call `logs`.
    AgentLogs {
        /// VM.
        vm_id: VmId,
        /// LightVM entries after `after_seq`, oldest first.
        entries: Vec<LogEntry>,
        /// Last sequence returned (pass it as `after_seq` next time).
        last_seq: u64,
        /// QEMU diagnostics of the current run (last lines, host paths removed).
        qemu: Vec<LogEntry>,
    },
    /// Response to the agent calls `declare` and `tool_note`.
    Recorded {
        /// Report event recorded (0 if dropped by the limit).
        seq: u64,
    },
    /// Response to the agent call `report`.
    AgentReport {
        /// Summary of the session's report.
        report: SessionReportSummary,
    },
    /// Response to `preview_vmware_import`.
    VmwareImportPreview {
        /// What would be imported.
        preview: Box<VmwareImportPreview>,
    },
    /// Response to `import_vmware`: the VM exists with its converted disk.
    VmwareImported {
        /// New VM.
        vm: VmRecord,
    },
    /// Response to `cancel_vmware_import`.
    VmwareImportCancel {
        /// `true` if an import was in progress and is being cancelled.
        cancelled: bool,
    },
}

/// Longest text taken from a VMware file into a preview (display name,
/// guest type, file names): control characters removed, then truncated.
pub const MAX_VMWARE_TEXT_CHARS: usize = 128;

/// Outcome of the examination of a VMware VM (ADR-0030), for the UI. The
/// texts come from the user's files: they are shown, never interpreted.
/// No path is included, only file names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmwareImportPreview {
    /// File name of the `.vmx`.
    pub vmx_file: String,
    /// `displayName` of the VMware VM, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// VMware guest type as written in the `.vmx` (`guestOS`).
    pub vmware_guest_os: String,
    /// Configuration the import would create; the name is a suggestion
    /// from the VMware display name, the display the standard adapter.
    pub config: VmConfig,
    /// Main disk.
    pub disk: VmwareDiskPreview,
    /// Bytes needed on the catalog volume for the converted disk
    /// (estimate: the data of the disk chain, at most its virtual size).
    pub required_bytes: u64,
    /// Free bytes on the catalog volume, if measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
    /// What is not imported.
    #[serde(default)]
    pub not_imported: Vec<NotImported>,
    /// Warnings the user should read before importing.
    #[serde(default)]
    pub warnings: Vec<ImportWarning>,
    /// The VMware SVGA display adapter can be chosen (Windows guests).
    #[serde(default)]
    pub vmware_svga_offered: bool,
    /// The `.vmx` says the VMware Tools were installed in the guest (hint).
    #[serde(default)]
    pub vmware_tools_hint: bool,
}

/// Main disk of a VMware VM, in [`VmwareImportPreview`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VmwareDiskPreview {
    /// File name of the current VMDK (top of the snapshot chain).
    pub file_name: String,
    /// VMware slot of the disk (`scsi0:0`, `sata0:1`, `ide0:0`, `nvme0:0`).
    pub slot: String,
    /// Controller of the disk in VMware.
    pub vmware_controller: VmwareController,
    /// `createType` of the current VMDK.
    pub create_type: String,
    /// Virtual size of the disk.
    pub virtual_size_bytes: u64,
    /// Bytes allocated by the files of the chain on the host.
    pub allocated_bytes: u64,
    /// Snapshot levels flattened into the converted disk (0 = no snapshot).
    pub snapshot_levels: u32,
}

/// Disk controller of a VMware VM.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VmwareController {
    /// IDE.
    Ide,
    /// SATA (AHCI).
    Sata,
    /// NVMe.
    Nvme,
    /// SCSI, VMware paravirtual (`pvscsi`).
    Pvscsi,
    /// SCSI, LSI Logic parallel (`lsilogic`): not emulated by QEMU.
    LsiLogic,
    /// SCSI, LSI Logic SAS (`lsisas1068`).
    LsiSas1068,
    /// SCSI, BusLogic (`buslogic`): not emulated by QEMU.
    BusLogic,
}

/// Part of a VMware VM that is not imported, in [`VmwareImportPreview`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NotImported {
    /// What.
    pub item: NotImportedItem,
    /// Detail from the `.vmx` (for example a file name or a count), shown
    /// as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Kind of [`NotImported`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NotImportedItem {
    /// Network adapters: LightVM VMs are offline (ADR-0004).
    Network,
    /// Shared folders: recreated later through LightVM Tools (ADR-0031).
    SharedFolders,
    /// USB controllers and devices: recreated later through LightVM Tools.
    Usb,
    /// Sound card.
    Sound,
    /// 3D acceleration.
    Accelerated3d,
    /// Intermediate snapshots (only the current state is converted).
    Snapshots,
    /// A hard disk other than the main one.
    OtherDisk,
    /// A CD/DVD drive (an ISO can be imported as a data medium).
    CdRom,
    /// Floppy drive.
    Floppy,
    /// Serial port.
    SerialPort,
    /// Parallel port.
    ParallelPort,
    /// Virtual printer.
    Printer,
    /// VMware Tools features (clipboard, drag and drop, time sync):
    /// recreated later through LightVM Tools (ADR-0031).
    VmwareToolsFeatures,
}

/// Warning of a VMware import, in [`VmwareImportPreview`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportWarning {
    /// What.
    pub kind: ImportWarningKind,
    /// Values for the message (for example the VMware value and the one
    /// LightVM uses), shown as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Kind of [`ImportWarning`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImportWarningKind {
    /// Windows on a controller LightVM cannot keep (an LSI Logic parallel
    /// controller, which QEMU does not emulate, or an NVMe boot disk with
    /// legacy BIOS, which SeaBIOS cannot boot): Windows may not start (stop
    /// 0x7B) until the storage driver of the new controller is enabled in
    /// the guest, and the storage helper is not available for the automatic
    /// preparation (manual preparation, `docs/USER_GUIDE.md`).
    StorageDriverPreparation,
    /// As [`Self::StorageDriverPreparation`], but LightVM enables the
    /// guest's own driver of the new controller in the converted disk with
    /// the storage helper (offline `SYSTEM` hive edit; the VMware VM is not
    /// changed).
    StorageDriverEnabled,
    /// A controller QEMU does not emulate on a Linux guest: the disk is
    /// attached to the profile's controller (the initramfs must have its
    /// driver, as standard distributions do).
    ControllerReplaced,
    /// The guest memory was raised to the minimum of the guest system.
    MemoryRaised,
    /// The guest memory was lowered to the host limit.
    MemoryLowered,
    /// The vCPUs were lowered to the host limit.
    VcpusLowered,
    /// The VMware CPU topology could not be kept (one socket per vCPU).
    TopologyChanged,
    /// VMware uses the same guest type for Windows 8 and 8.1: LightVM
    /// supports 8.1 only.
    Windows8Or81,
    /// UEFI guest: the firmware starts with a new variable store, so the
    /// boot entries of VMware are not carried over (the default loader of
    /// the disk is used).
    NewUefiVariables,
    /// A disk of the snapshot chain is marked dirty (not closed cleanly).
    DirtyDisk,
}

/// Phase of a VMware import, in [`ServiceEvent::VmwareImportProgress`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VmwareImportPhase {
    /// Examination of the `.vmx` and of the disk chain.
    Checking,
    /// `qemu-img convert` running.
    Converting,
    /// The storage driver of the new controller being enabled in the
    /// converted disk (storage helper, ADR-0030).
    PreparingDisk,
    /// Check of the converted disk and creation of the VM.
    Finishing,
}

/// State of the service.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// In service.
    Running,
    /// Controlled shutdown in progress.
    Stopping,
}

/// Reason the agent sessions were stopped.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Parent process (UI) terminated.
    ParentLost,
    /// Emergency Stop.
    EmergencyStop,
    /// Controlled shutdown of the service.
    Shutdown,
}

/// Events published on the state channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceEvent {
    /// Change of the service state.
    ServiceState {
        /// New state.
        state: ServiceState,
    },
    /// Change of a VM's state.
    VmState {
        /// VM.
        vm_id: VmId,
        /// New state.
        state: VmState,
    },
    /// A connection was revoked.
    ConnectionRevoked {
        /// Revoked connection.
        connection_id: ConnectionId,
    },
    /// The agent sessions were stopped.
    AgentSessionsStopped {
        /// Reason.
        reason: StopReason,
    },
    /// The baselines or the storage operation of a VM changed (operation
    /// started or ended, baseline created, restored or deleted): re-read
    /// `vm_status` and `list_baselines` (F3).
    BaselinesChanged {
        /// VM.
        vm_id: VmId,
    },
    /// The user snapshots of a VM changed (taken, reverted, deleted,
    /// renamed, operation started or ended): re-read `list_snapshots`
    /// (ADR-0032).
    SnapshotsChanged {
        /// VM.
        vm_id: VmId,
    },
    /// Events lost because the client was too slow.
    EventsLost {
        /// Number of lost events.
        count: u64,
    },
    /// The agent access switch changed.
    AgentAccess {
        /// State.
        enabled: bool,
    },
    /// New pending session request.
    SessionRequest {
        /// Request.
        request: SessionRequestInfo,
    },
    /// A session request was decided, expired or withdrawn.
    SessionRequestClosed {
        /// Request.
        request_id: SessionRequestId,
        /// Outcome.
        outcome: SessionRequestOutcome,
    },
    /// A session was created, paused, resumed or terminated.
    SessionChanged {
        /// Session.
        session_id: SessionId,
        /// State now.
        state: SessionState,
        /// Termination cause, if terminated.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        termination: Option<TerminationCause>,
    },
    /// The input owner of a VM changed.
    InputOwner {
        /// VM.
        vm_id: VmId,
        /// Owner now.
        owner: InputOwner,
    },
    /// A session report was created or changed (termination, verdict,
    /// verification, deletion): re-read the list or the header (F5).
    SessionReportChanged {
        /// Session.
        session_id: SessionId,
    },
    /// Progress of the VMware import in progress (ADR-0030); published at
    /// most a few times per second. The end is the reply to
    /// `import_vmware`.
    VmwareImportProgress {
        /// Phase.
        phase: VmwareImportPhase,
        /// Percentage of the phase (0..=100).
        percent: u8,
    },
}

/// Reason the service closed the connection.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Connection or credential revoked.
    Revoked,
    /// Service shutting down.
    ServiceStopping,
}

/// Messages from the service to the client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Handshake accepted.
    HelloOk {
        /// Request of `hello`.
        request_id: RequestId,
        /// Negotiated version.
        protocol_version: u32,
        /// Identifier assigned to the connection.
        connection_id: ConnectionId,
        /// Role of the credential.
        role: Role,
    },
    /// Positive response.
    Response {
        /// Request.
        request_id: RequestId,
        /// Result.
        result: ResponseBody,
    },
    /// Error; `request_id` absent if the frame was not decodable.
    Error {
        /// Request, if known.
        request_id: Option<RequestId>,
        /// Error.
        error: ReplyError,
    },
    /// State event (after `subscribe`).
    Event {
        /// Event.
        event: ServiceEvent,
    },
    /// Close initiated by the service; the transport close follows.
    Closing {
        /// Reason.
        reason: CloseReason,
    },
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (screenshot chunks).
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let idx = |shift: u32| B64[((n >> shift) & 63) as usize] as char;
        out.push(idx(18));
        out.push(idx(12));
        out.push(if c.len() > 1 { idx(6) } else { '=' });
        out.push(if c.len() > 2 { idx(0) } else { '=' });
    }
    out
}

/// Decodes standard base64 with padding; `None` if malformed.
#[must_use]
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let quads = b.len() / 4;
    for (i, q) in b.chunks(4).enumerate() {
        let last = i + 1 == quads;
        let pad = q.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for (j, c) in q.iter().enumerate() {
            let v = if j >= 4 - pad { 0 } else { val(*c)? };
            n = (n << 6) | v;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

fn generate<T: JsonSchema>() -> Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Schemas of the IPC envelope (file name without extension, schema), in stable
/// order. The two root messages include all nested types in `$defs`.
#[must_use]
pub fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("ipc_client_message", generate::<ClientMessage>()),
        ("ipc_server_message", generate::<ServerMessage>()),
        ("parent_loss_policy", generate::<ParentLossPolicy>()),
        ("ipc_emergency_command", generate::<EmergencyCommand>()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, ProtocolError, ProtocolErrorCode, PublicError};

    #[test]
    fn parent_loss_policy_wire_shape_default_and_limits() {
        assert_eq!(ParentLossPolicy::default(), ParentLossPolicy::KillVms);
        assert_eq!(
            serde_json::to_string(&ParentLossPolicy::KillVms).unwrap(),
            r#"{"kind":"kill_vms"}"#
        );
        assert_eq!(
            serde_json::to_string(&ParentLossPolicy::ShutdownVms { powerdown_s: 10 }).unwrap(),
            r#"{"kind":"shutdown_vms","powerdown_s":10}"#
        );
        assert_eq!(
            serde_json::to_string(&ParentLossPolicy::KeepVms).unwrap(),
            r#"{"kind":"keep_vms"}"#
        );
        for ok in [1, 10, 300] {
            assert!(
                ParentLossPolicy::ShutdownVms { powerdown_s: ok }
                    .validate()
                    .is_ok()
            );
        }
        for bad in [0, 301, u32::MAX] {
            let e = ParentLossPolicy::ShutdownVms { powerdown_s: bad }
                .validate()
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidArgument);
        }
        // Capabilities of an older service without the field: default policy.
        let c: Capabilities = serde_json::from_str(
            r#"{"protocol_versions":[1],"backend":"x","accelerator":null,"max_frame_bytes":1}"#,
        )
        .unwrap();
        assert_eq!(c.parent_loss_policy, ParentLossPolicy::KillVms);
    }

    #[test]
    fn error_json_shape_uses_proto_types() {
        let e = ServerMessage::Error {
            request_id: Some(7),
            error: ReplyError::Operational(PublicError::unauthenticated("x")),
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"error","request_id":7,"error":{"kind":"operational","code":"unauthenticated","message":"x","retryable":false}}"#
        );
        let p = ServerMessage::Error {
            request_id: None,
            error: ReplyError::Protocol(ProtocolError {
                code: ProtocolErrorCode::MessageTooLarge,
                message: "y".into(),
            }),
        };
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"type":"error","request_id":null,"error":{"kind":"protocol","code":"message_too_large","message":"y"}}"#
        );
        let d = ReplyError::Operational(PublicError::new(ErrorCode::PermissionDenied, "z"));
        assert!(
            serde_json::to_string(&d)
                .unwrap()
                .contains("permission_denied")
        );
    }

    #[test]
    fn request_json_shape() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"request","request_id":3,"body":{"op":"ping"}}"#)
                .unwrap();
        assert_eq!(
            m,
            ClientMessage::Request {
                request_id: 3,
                body: Request::Ping
            }
        );
        let vm = VmId::from_unix_millis(1_760_000_000_000, [0; 10]);
        let m = ClientMessage::Request {
            request_id: 4,
            body: Request::VmStatus { vm_id: vm },
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&s).unwrap(), m);
    }

    #[test]
    fn unknown_ops_are_rejected() {
        assert!(
            serde_json::from_str::<ClientMessage>(
                r#"{"type":"request","request_id":3,"body":{"op":"qmp_execute"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn credential_is_write_only_in_schema() {
        let s = serde_json::to_string(&generate::<ClientMessage>()).unwrap();
        assert!(s.contains("writeOnly"));
    }

    #[test]
    fn base64_round_trip() {
        for n in 0..70usize {
            let v: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let e = base64_encode(&v);
            assert_eq!(base64_decode(&e).unwrap(), v, "{n}");
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert!(base64_decode("TQ=").is_none());
        assert!(base64_decode("T===").is_none());
        assert!(base64_decode("TQ==TWFu").is_none());
        assert!(base64_decode("TW!u").is_none());
    }

    #[test]
    fn session_auth_debug_redacts_the_token() {
        let token = SessionToken::from_bytes([0xab; 32]);
        let a = SessionAuth {
            session_id: SessionId::from_unix_millis(1_760_000_000_000, [2; 10]),
            token,
        };
        let d = format!("{a:?}");
        assert!(!d.contains("abab"), "{d}");
        let m = ClientMessage::Request {
            request_id: 1,
            body: Request::Agent {
                session: a,
                call: AgentCall::RenewLease,
            },
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(r#""op":"agent""#), "{s}");
        assert!(s.contains(r#""call":"renew_lease""#), "{s}");
        assert_eq!(serde_json::from_str::<ClientMessage>(&s).unwrap(), m);
        assert!(!format!("{m:?}").contains("abab"));
    }

    #[test]
    fn f4_wire_shapes() {
        assert_eq!(
            serde_json::to_string(&InputOwner::User).unwrap(),
            r#"{"kind":"user"}"#
        );
        assert_eq!(
            serde_json::to_string(&EmergencyCommand::EmergencyStop).unwrap(),
            r#"{"type":"emergency_stop"}"#
        );
        let a: SessionApproval = serde_json::from_str(
            r#"{"vm_id":"0199d1a4-6000-7000-8000-000000000000","capabilities":["vm.input"],"duration_s":600}"#,
        )
        .unwrap();
        assert_eq!(a.quotas, None);
        assert!(!a.give_input);
        let r: ClientMessage = serde_json::from_str(
            r#"{"type":"request","request_id":1,"body":{"op":"request_session","client_name":"x","purpose":"y"}}"#,
        )
        .unwrap();
        assert!(matches!(
            r,
            ClientMessage::Request {
                body: Request::RequestSession { .. },
                ..
            }
        ));
    }

    #[test]
    fn console_ticket_debug_redacts_the_token() {
        let token = "ab".repeat(32);
        let t = ConsoleTicket {
            vm_id: VmId::from_unix_millis(1_760_000_000_000, [0; 10]),
            run_id: RunId::from_unix_millis(1_760_000_000_000, [1; 10]),
            socket_path: "C:\\x\\d.sock".into(),
            token: token.clone(),
            geometry: None,
        };
        let d = format!("{t:?}");
        assert!(!d.contains(&token), "{d}");
        assert!(d.contains("redacted"));
        let body = format!("{:?}", ResponseBody::Console { ticket: t });
        assert!(!body.contains(&token));
    }
}
