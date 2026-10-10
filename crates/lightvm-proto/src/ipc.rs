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
//!   referenced by [`VmId`], media by registry [`crate::MediaId`]. Two
//!   exceptions, both reserved to the `ui` role and never to agent sessions:
//!   [`Request::ImportMedia`] carries the file the user chose with the native
//!   picker of the UI (opened read-only as a regular file and copied into the
//!   catalog, never passed to QEMU), and [`ConsoleTicket`] returns the path of
//!   the per-run display socket (ADR-0013);
//! - errors are [`ReplyError`]: operational ([`crate::PublicError`]) or
//!   protocol ([`crate::ProtocolError`]), never mixed up. Missing, malformed,
//!   unknown or revoked credential → `unauthenticated`; insufficient role
//!   → `permission_denied` (see [`crate::ErrorCode`]).

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};

use crate::{
    Admission, DisplayGeometry, HostCapacity, MediaKind, MediaRecord, OperationRecord,
    PROTOCOL_VERSION, ReplyError, RunId, VmConfig, VmId, VmName, VmRecord, VmRequest, VmState,
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
    /// (UI only, never from agent sessions): read-only managed copy, SHA-256
    /// and registry row (ADR-0008). The path is opened as a regular file
    /// without following links and is never passed to QEMU.
    ImportMedia {
        /// File chosen by the user.
        source_path: String,
        /// Kind of media.
        media_kind: MediaKind,
        /// Origin declared by the user (free text, never interpreted).
        declared_origin: String,
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
}

impl Request {
    /// Stable name of the operation, for audit and diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
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
            Self::DeleteVm { .. } => "delete_vm",
            Self::OpenConsole { .. } => "open_console",
        }
    }
}

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
    /// Events lost because the client was too slow.
    EventsLost {
        /// Number of lost events.
        count: u64,
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
