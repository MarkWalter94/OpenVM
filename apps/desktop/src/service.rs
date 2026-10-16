//! Supervisor as a child process of the UI (F1-11, ADR-0002; prerequisite of
//! P-2 in `docs/STATUS.md`).
//!
//! The UI starts `lightvm-service` with structured arguments and keeps three
//! anonymous pipes (contract in `crates/lightvm-service/src/main.rs`):
//! - stdout: a single JSON `ready` line with IPC endpoint and UI credential;
//! - stdin: life pipe (EOF = UI terminated) and `shutdown` line for the
//!   controlled stop;
//! - stderr: diagnostics without secrets; inherited, or appended to
//!   `lightvm-service.log` when the app writes its diagnostic log
//!   ([`crate::diag`]).
//!
//! After the `ready` line the UI talks to the service only via authenticated
//! IPC, with six connections: requests (reply by `request_id`), events
//! (`subscribe`), a fast lane for console tickets and keyboard/mouse input
//! (never queued behind a start or a stop), a slow lane for ISO imports
//! (copy and SHA-256 of several GiB), a storage lane for baseline creation
//! and restore (F3: full copy and hash of the disk, minutes, no timeout)
//! and a start lane (the reply to a start arrives after the verification of
//! media and baseline and the launch of QEMU, seconds). Each lane matches
//! its replies by `request_id` on its own connection. Reads of a frame
//! cannot be interrupted safely, so the lanes do not share a connection.
//!
//! The UI opens neither QMP nor VM files: start, stop, input and Emergency
//! Stop go through the service core (ROADMAP 6.1). The console display is
//! reached only with the per-run ticket of `open_console` (ADR-0013).

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::DisplayAdapter;
use lightvm_proto::ipc::{
    BaselineSummary, ConsoleTicket, InputOwner, ParentLossPolicy, Request, ResponseBody,
    ServerMessage, ServiceEvent, SessionApproval, SessionInfo, SessionRequestId,
    SessionRequestInfo, SessionRequestOutcome, SnapshotTree, VmSummary, VmwareImportPhase,
    VmwareImportPreview,
};
use lightvm_proto::report::{
    ReportEvent, ReportFormat, SessionReport, SessionReportSummary, SessionVerifier,
};
use lightvm_proto::{
    BaselineId, ErrorCode, HostCapacity, InputBatch, InputEvent, KeyAction, Label, MediaId,
    MediaKind, MediaRecord, MouseButton, OperationId, OperationStatus, PointerButton, ReplyError,
    RunId, SessionId, SessionState, Sha256Digest, SnapshotId, TerminationCause, TestOutcome,
    UtcTimestamp, VmAction, VmConfig, VmId, VmName, VmRecord, VmRequest,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use tokio::sync::mpsc;

/// Maximum wait for the `ready` line.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum wait for the exit after the controlled stop request: beyond the
/// service stop timeout (30 s, then Emergency Stop of the remaining VMs), so
/// the UI does not kill it halfway through the orderly stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(45);
/// Wait before sending again an input batch refused by the service rate
/// limit (`resource_limit`).
pub const INPUT_RETRY_DELAY: Duration = Duration::from_millis(60);
/// Commands waiting on the fast lane beyond which pointer-only input
/// batches are dropped (keys, buttons and console tickets never are).
pub const FAST_LANE_CAPACITY: usize = 32;

/// Start parameters of the supervisor.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// `lightvm-service` executable.
    pub executable: PathBuf,
    /// Catalog directory.
    pub catalog: PathBuf,
    /// Approved QEMU distribution; without it, the service refuses starts.
    pub qemu_dir: Option<PathBuf>,
    /// Runtime directory (IPC socket on Unix, private QEMU sockets).
    pub runtime_dir: Option<PathBuf>,
    /// Audit file; without it, the service uses the user profile
    /// (`%LOCALAPPDATA%\LightVM\audit.jsonl` on Windows).
    pub audit_log: Option<PathBuf>,
    /// Fate of the VMs if the UI terminates without a controlled stop.
    pub policy: ParentLossPolicy,
    /// Folder of the pairing file for agent clients (F4); `None`: the
    /// per-user default ([`crate::pairing::default_dir`]).
    pub pairing_dir: Option<PathBuf>,
}

impl ServiceConfig {
    /// Executable next to the UI one (`lightvm-service[.exe]`).
    pub fn executable_next_to_ui() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.with_file_name(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX)))
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.executable);
        c.arg("--catalog").arg(&self.catalog);
        if let Some(q) = &self.qemu_dir {
            c.arg("--qemu-dir").arg(q);
        }
        if let Some(r) = &self.runtime_dir {
            c.arg("--runtime-dir").arg(r);
        }
        if let Some(a) = &self.audit_log {
            c.arg("--audit-log").arg(a);
        }
        match self.policy {
            ParentLossPolicy::KillVms => {
                c.args(["--parent-loss-policy", "kill-vms"]);
            }
            ParentLossPolicy::ShutdownVms { powerdown_s } => {
                c.args(["--parent-loss-policy", "shutdown-vms"])
                    .arg("--parent-loss-powerdown-s")
                    .arg(powerdown_s.to_string());
            }
            ParentLossPolicy::KeepVms => {
                c.args(["--parent-loss-policy", "keep-vms"]);
            }
        }
        c.args(["--parent-watch", "stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(crate::diag::service_stderr().unwrap_or_else(Stdio::inherit));
        c
    }
}

/// Initial state of the connection, before the handshake.
pub const CONNECTING: &str = "connecting";

/// Failure of a request, readable, with the "not provided by this
/// supervisor" case recognisable (`unsupported`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The supervisor does not implement the operation.
    pub unsupported: bool,
    /// Readable description.
    pub message: String,
    /// Error code of the supervisor, if the failure came from its reply
    /// (`None`: connection or protocol problem).
    pub code: Option<ErrorCode>,
    /// Structured details of the reply (for example `reason`), without
    /// secrets.
    pub details: BTreeMap<String, String>,
}

impl Failure {
    /// Failure from a description.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// Failure with the code of a supervisor reply (stand-ins build them
    /// too).
    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            unsupported: code == ErrorCode::Unsupported,
            message: message.into(),
            code: Some(code),
            details: BTreeMap::new(),
        }
    }

    /// Adds a detail (`reason`, `needed_bytes`, ...).
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: impl Into<String>) -> Self {
        self.details.insert(key.to_owned(), value.into());
        self
    }

    /// The supervisor refused a start because this host lacks the capability
    /// of starting VMs (`unsupported` with capability `vm.start`): what
    /// happens when it runs without a QEMU distribution.
    pub fn start_unavailable(&self) -> bool {
        self.unsupported
            && self.details.get("capability").map(String::as_str)
                == Some(lightvm_proto::Capability::VmStart.as_str())
    }

    /// Value of the `reason` detail, if any.
    pub fn reason(&self) -> Option<&str> {
        self.details.get("reason").map(String::as_str)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// State of a request whose outcome a view waits for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RequestStatus<T> {
    /// Never requested.
    #[default]
    Idle,
    /// Sent, outcome not arrived yet.
    Pending,
    /// Succeeded.
    Done(T),
    /// Failed.
    Failed(Failure),
}

impl<T> RequestStatus<T> {
    /// Waiting for the outcome.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Approved media known to the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    /// The list was received at least once.
    pub loaded: bool,
    /// Approved media.
    pub records: Vec<MediaRecord>,
    /// Last read error, if any.
    pub error: Option<Failure>,
}

impl MediaState {
    /// Media of a kind, most recent first.
    pub fn of_kind(&self, kind: MediaKind) -> Vec<&MediaRecord> {
        let mut v: Vec<&MediaRecord> = self.records.iter().filter(|m| m.kind == kind).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.approved_at));
        v
    }
}

/// Request of a console ticket. The ticket itself (secret) is not part of
/// the state: it is handed over once with [`Supervisor::take_console_ticket`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleRequest {
    /// VM.
    pub vm_id: VmId,
    /// Outcome: on success, the run the ticket is valid for.
    pub status: RequestStatus<RunId>,
}

/// Baselines of a VM known to the UI (F3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaselineList {
    /// The list was received at least once.
    pub loaded: bool,
    /// Baselines, oldest first.
    pub items: Vec<BaselineSummary>,
    /// Last read error, if any.
    pub error: Option<Failure>,
}

/// Baseline operation requested by the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaselineOp {
    /// Creation with the given label.
    Create {
        /// Label of the new baseline.
        label: Label,
        /// Consent to a baseline after an unclean stop ("not verified").
        allow_unclean: bool,
    },
    /// Restore of a baseline.
    Restore(BaselineId),
    /// Deletion of a baseline.
    Delete(BaselineId),
}

/// Last baseline operation requested by the UI and its outcome (on
/// success, the baseline concerned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineRequest {
    /// VM.
    pub vm_id: VmId,
    /// Operation.
    pub op: BaselineOp,
    /// Outcome.
    pub status: RequestStatus<BaselineId>,
}

/// Snapshots of a VM known to the UI (ADR-0032).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotList {
    /// The tree was received at least once.
    pub loaded: bool,
    /// The tree, as last read.
    pub tree: Option<SnapshotTree>,
    /// Last read error, if any.
    pub error: Option<Failure>,
}

/// Snapshot operation requested by the UI (ADR-0032).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotOp {
    /// New snapshot (name and description in the request).
    Take,
    /// Revert to a snapshot (VM off).
    Revert(SnapshotId),
    /// Deletion of a snapshot.
    Delete(SnapshotId),
    /// New name and description of a snapshot.
    Rename(SnapshotId),
}

/// Last snapshot operation requested by the UI and its outcome (on
/// success, the snapshot concerned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRequest {
    /// VM.
    pub vm_id: VmId,
    /// Operation.
    pub op: SnapshotOp,
    /// Outcome.
    pub status: RequestStatus<SnapshotId>,
}

/// Control of the agent sessions requested by the UI (F4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentOp {
    /// Agent access switch.
    SetAccess(bool),
    /// Denial of a session request.
    Deny(SessionRequestId),
    /// Pause of a session.
    Pause(SessionId),
    /// Resume of a paused session.
    Resume(SessionId),
    /// Revocation of a session.
    Revoke(SessionId),
    /// Input ownership of a VM.
    InputOwner(VmId, InputOwner),
}

/// Last agent control requested by the UI and its outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentControl {
    /// Operation.
    pub op: AgentOp,
    /// Outcome.
    pub status: RequestStatus<()>,
}

/// Last session approval sent by the UI and its outcome (the session).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    /// Request approved.
    pub request_id: SessionRequestId,
    /// Outcome.
    pub status: RequestStatus<SessionId>,
}

/// Session event seen by the app, for the session page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLogEntry {
    /// Instant the app saw it.
    pub at: UtcTimestamp,
    /// Session concerned, if any.
    pub session_id: Option<SessionId>,
    /// Agent connection of a session request, until the session it led to
    /// is known (then [`Self::session_id`] is set).
    pub connection_id: Option<lightvm_proto::ipc::ConnectionId>,
    /// Readable description (plain text).
    pub text: String,
}

/// Entries kept in [`AgentState::log`].
pub const AGENT_LOG_ENTRIES: usize = 200;

/// Agent access, session requests and sessions known to the UI (F4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentState {
    /// Agent access switch, once read (off at every supervisor start).
    pub access: Option<bool>,
    /// Pending session requests, oldest first.
    pub requests: Vec<SessionRequestInfo>,
    /// Sessions: live first, then the most recent terminated ones.
    pub sessions: Vec<SessionInfo>,
    /// The session list was received at least once.
    pub sessions_loaded: bool,
    /// Input owner of the VMs, as last reported by the supervisor.
    pub input_owners: BTreeMap<VmId, InputOwner>,
    /// Last approval.
    pub approval: Option<Approval>,
    /// Last control (switch, deny, pause, resume, revoke, input).
    pub control: Option<AgentControl>,
    /// Last pairing file written for an agent client.
    pub pairing: RequestStatus<crate::pairing::PairingFile>,
    /// Last read error (access, requests, sessions).
    pub error: Option<Failure>,
    /// Session events seen by the app, oldest first (at most
    /// [`AGENT_LOG_ENTRIES`]).
    pub log: VecDeque<AgentLogEntry>,
    /// Last state of each session recorded in [`Self::log`] (events and
    /// replies can arrive in either order: each change is recorded once).
    pub logged: BTreeMap<SessionId, SessionState>,
}

impl AgentState {
    /// Sessions not terminated (active or paused).
    pub fn live(&self) -> impl Iterator<Item = &SessionInfo> {
        self.sessions
            .iter()
            .filter(|s| s.state != SessionState::Terminated)
    }

    /// Session by identifier.
    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.session_id == id)
    }

    /// Pending request by identifier.
    pub fn request(&self, id: SessionRequestId) -> Option<&SessionRequestInfo> {
        self.requests.iter().find(|r| r.request_id == id)
    }

    /// Live session that owns the input of `vm`, if any. An owner reported
    /// for a session known to be terminated counts as the user.
    pub fn input_session(&self, vm: VmId) -> Option<&SessionInfo> {
        match self.input_owners.get(&vm) {
            Some(InputOwner::Agent { session_id }) => self
                .session(*session_id)
                .filter(|s| s.state != SessionState::Terminated),
            Some(InputOwner::User) => None,
            None => self.live().find(|s| s.vm_id == vm && s.input_owner),
        }
    }

    /// An agent owns the input of `vm` (the user's input is not sent).
    pub fn agent_has_input(&self, vm: VmId) -> bool {
        match self.input_owners.get(&vm) {
            // Not listed yet (event before the list): still the agent's.
            Some(InputOwner::Agent { session_id }) => self
                .session(*session_id)
                .is_none_or(|s| s.state != SessionState::Terminated),
            Some(InputOwner::User) => false,
            None => self.input_session(vm).is_some(),
        }
    }

    /// Records a session event seen by the app.
    pub fn record(&mut self, at: UtcTimestamp, session_id: Option<SessionId>, text: String) {
        self.push_log(AgentLogEntry {
            at,
            session_id,
            connection_id: None,
            text,
        });
    }

    /// Records an event of a session request of an agent connection; it
    /// joins the session's events once the session is known.
    pub fn record_request(
        &mut self,
        at: UtcTimestamp,
        connection_id: lightvm_proto::ipc::ConnectionId,
        text: String,
    ) {
        self.push_log(AgentLogEntry {
            at,
            session_id: None,
            connection_id: Some(connection_id),
            text,
        });
        self.adopt_log();
    }

    fn push_log(&mut self, e: AgentLogEntry) {
        if self.log.len() >= AGENT_LOG_ENTRIES {
            self.log.pop_front();
        }
        self.log.push_back(e);
    }

    /// Request events of a connection become events of the session bound
    /// to that connection.
    fn adopt_log(&mut self) {
        for s in &self.sessions {
            let Some(c) = s.connection_id else { continue };
            for e in &mut self.log {
                if e.session_id.is_none() && e.connection_id == Some(c) {
                    e.session_id = Some(s.session_id);
                }
            }
        }
    }

    /// Replaces the session list (re-read).
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>) {
        self.sessions = sessions;
        self.sessions_loaded = true;
        self.adopt_log();
    }

    /// Replaces or adds a session in the list.
    pub fn upsert(&mut self, session: SessionInfo) {
        match self
            .sessions
            .iter_mut()
            .find(|s| s.session_id == session.session_id)
        {
            Some(s) => *s = session,
            None => self.sessions.insert(0, session),
        }
        self.adopt_log();
    }
}

/// Action on a session report requested by the UI (F5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportOp {
    /// Verdict of the local user.
    Verdict(SessionId),
    /// Automatic verifier.
    Verify(SessionId),
    /// Export to a file chosen by the user.
    Export(SessionId),
    /// Deletion of the report and its screenshots.
    Delete(SessionId),
}

impl ReportOp {
    /// Session of the report.
    pub fn session(self) -> SessionId {
        match self {
            Self::Verdict(s) | Self::Verify(s) | Self::Export(s) | Self::Delete(s) => s,
        }
    }
}

/// Export written by the supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportDone {
    /// File chosen by the user.
    pub path: PathBuf,
    /// Format.
    pub format: ReportFormat,
    /// Bytes of the report file.
    pub bytes: u64,
    /// SHA-256 of the report file.
    pub sha256: Sha256Digest,
    /// Screenshots written next to it.
    pub screenshots: u32,
}

/// Outcome of a report action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    /// Verdict or verification recorded: the report was re-read.
    Updated,
    /// Export written.
    Exported(ExportDone),
    /// Report deleted.
    Deleted,
}

/// Last report action and its outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportControl {
    /// Action.
    pub op: ReportOp,
    /// Outcome.
    pub status: RequestStatus<ReportOutcome>,
}

/// A page of the events of a report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventsPage {
    /// Events recorded in total.
    pub total: u32,
    /// First event of the page (0-based).
    pub offset: u32,
    /// Events.
    pub events: Vec<ReportEvent>,
}

/// Stored screenshot of a report, read on demand, checked against its
/// SHA-256 and decoded. Kept in memory only (never written by the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShotImage {
    /// SHA-256 of the PNG, checked.
    pub sha256: Sha256Digest,
    /// Size of the PNG in bytes.
    pub png_bytes: usize,
    /// Width and height.
    pub size: [usize; 2],
    /// RGBA pixels.
    pub rgba: Arc<Vec<u8>>,
}

/// Screenshots kept in memory at once (the oldest one is forgotten).
pub const REPORT_SHOTS_KEPT: usize = 3;
/// Largest screenshot read for display.
pub const MAX_SHOT_BYTES: usize = 64 << 20;
/// Events per page on the session page.
pub const EVENTS_PAGE: u32 = lightvm_proto::report::MAX_REPORT_EVENTS_PAGE;
/// Recent events shown for a live session in the AI Workspace.
pub const TAIL_EVENTS: u32 = 5;

/// Session reports known to the UI (F5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportsState {
    /// The list was received at least once.
    pub loaded: bool,
    /// Reports, newest first.
    pub list: Vec<SessionReportSummary>,
    /// Last list read error.
    pub error: Option<Failure>,
    /// Headers read (events empty).
    pub headers: BTreeMap<SessionId, SessionReport>,
    /// Last header read error per report.
    pub header_errors: BTreeMap<SessionId, Failure>,
    /// Page of events shown on the session page.
    pub events: BTreeMap<SessionId, EventsPage>,
    /// Last events read error per report.
    pub events_errors: BTreeMap<SessionId, Failure>,
    /// Last events of live sessions (AI Workspace).
    pub tail: BTreeMap<SessionId, EventsPage>,
    /// Screenshots read on demand, oldest first (at most
    /// [`REPORT_SHOTS_KEPT`]).
    pub shots: Vec<((SessionId, u32), RequestStatus<ShotImage>)>,
    /// Last action (verdict, verification, export, deletion).
    pub action: Option<ReportControl>,
}

impl ReportsState {
    /// Summary of a report.
    pub fn summary(&self, id: SessionId) -> Option<&SessionReportSummary> {
        self.list.iter().find(|r| r.session_id == id)
    }

    /// Screenshot read for display, if requested.
    pub fn shot(&self, id: SessionId, shot: u32) -> Option<&RequestStatus<ShotImage>> {
        self.shots
            .iter()
            .find(|(k, _)| *k == (id, shot))
            .map(|(_, v)| v)
    }

    /// Records the state of a screenshot read, forgetting the oldest one.
    pub fn set_shot(&mut self, id: SessionId, shot: u32, status: RequestStatus<ShotImage>) {
        self.shots.retain(|(k, _)| *k != (id, shot));
        while self.shots.len() >= REPORT_SHOTS_KEPT {
            self.shots.remove(0);
        }
        self.shots.push(((id, shot), status));
    }

    /// Forgets everything about a deleted report.
    pub fn forget(&mut self, id: SessionId) {
        self.list.retain(|r| r.session_id != id);
        self.headers.remove(&id);
        self.events.remove(&id);
        self.events_errors.remove(&id);
        self.tail.remove(&id);
        self.shots.retain(|((s, _), _)| *s != id);
    }
}

/// Verifies and decodes a PNG read from a report: the SHA-256 must be the
/// one the supervisor announced (the stored file may have been altered).
pub fn decode_shot(png: &[u8], sha256: Sha256Digest) -> Result<ShotImage, String> {
    use sha2::Digest as _;
    let got: [u8; 32] = sha2::Sha256::digest(png).into();
    if got != *sha256.as_bytes() {
        return Err("the screenshot does not match its SHA-256: not shown".into());
    }
    let (size, rgba) = decode_png(png)?;
    Ok(ShotImage {
        sha256,
        png_bytes: png.len(),
        size,
        rgba: Arc::new(rgba),
    })
}

/// Decodes a PNG to RGBA pixels (8-bit; palette, grey and 16-bit expanded).
pub fn decode_png(png: &[u8]) -> Result<([usize; 2], Vec<u8>), String> {
    let mut d = png::Decoder::new(std::io::Cursor::new(png));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d
        .read_info()
        .map_err(|e| format!("not a readable PNG: {e}"))?;
    let len = r
        .output_buffer_size()
        .ok_or_else(|| "PNG too large".to_owned())?;
    if len > 4 * 16384 * 16384 {
        return Err("PNG too large".into());
    }
    let mut buf = vec![0; len];
    let info = r
        .next_frame(&mut buf)
        .map_err(|e| format!("not a readable PNG: {e}"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => px
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[g, a]| [g, g, g, a])
            .collect(),
        png::ColorType::Indexed => return Err("PNG palette not expanded".into()),
    };
    if rgba.len() != w * h * 4 {
        return Err("PNG with an unexpected layout".into());
    }
    Ok(([w, h], rgba))
}

/// Plain description of a termination cause.
pub fn termination_text(c: TerminationCause) -> &'static str {
    match c {
        TerminationCause::Completed => "ended by the agent",
        TerminationCause::Revoked => "revoked by you",
        TerminationCause::EmergencyStop => "Emergency Stop",
        TerminationCause::GrantExpired => "time limit reached",
        TerminationCause::LeaseExpired => "the agent stopped responding; its VM was stopped",
        TerminationCause::ClientLost => "the agent disconnected; its VM was stopped",
        TerminationCause::UiClosed => "LightVM closed; the VM was stopped",
        TerminationCause::SupervisorLost => "supervisor restarted",
        TerminationCause::HostResumed => "host resumed from sleep",
        TerminationCause::VmTerminated => "the VM stopped",
        TerminationCause::Restored => "baseline restored",
    }
}

/// State seen by the UI, updated by the IPC threads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceState {
    /// Last explicit guest Tools operation (payload Debug is redacted).
    pub tools: Option<(VmId, RunId, RequestStatus<lightvm_proto::ToolReply>)>,
    /// Connection state, readable.
    pub connection: String,
    /// Connection established and handshake succeeded.
    pub connected: bool,
    /// Backend reported by the service.
    pub backend: Option<String>,
    /// Accelerator reported by the service.
    pub accelerator: Option<String>,
    /// Host capacity reported by the service, if measured.
    pub host: Option<HostCapacity>,
    /// VMs of the catalog.
    pub vms: Vec<VmSummary>,
    /// Outcome of the last requested action, readable.
    pub last_outcome: Option<String>,
    /// Text put in [`Self::last_outcome`] by the last start refused because
    /// the host cannot start VMs ([`Failure::start_unavailable`]): while the
    /// two are equal, the Dashboard can name the cause when the supervisor
    /// has no QEMU.
    pub start_unavailable_outcome: Option<String>,
    /// UI loss policy reported by the service.
    pub policy: Option<ParentLossPolicy>,
    /// Approved media of the registry.
    pub media: MediaState,
    /// Last ISO import.
    pub import: RequestStatus<MediaRecord>,
    /// Last media deletion.
    pub delete_media: RequestStatus<MediaId>,
    /// Last VM creation.
    pub create: RequestStatus<VmId>,
    /// Last VM deletion.
    pub delete: RequestStatus<VmId>,
    /// Last console ticket request.
    pub console: Option<ConsoleRequest>,
    /// Last refused input batch, readable (cleared by the next accepted one).
    pub input_error: Option<String>,
    /// Baselines per VM, as last read (F3).
    pub baselines: BTreeMap<VmId, BaselineList>,
    /// Last baseline operation (creation, restore, deletion).
    pub baseline: Option<BaselineRequest>,
    /// Snapshots per VM, as last read (ADR-0032).
    pub snapshots: BTreeMap<VmId, SnapshotList>,
    /// Last snapshot operation (take, revert, delete, rename).
    pub snapshot: Option<SnapshotRequest>,
    /// Agent access and sessions (F4).
    pub agents: AgentState,
    /// Session reports (F5).
    pub reports: ReportsState,
    /// Import of a VMware VM (ADR-0030).
    pub vmware: VmwareImportState,
}

/// Import of a VMware VM as seen by the UI (ADR-0030).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VmwareImportState {
    /// `.vmx` the last preview was asked for.
    pub preview_path: Option<PathBuf>,
    /// Last preview.
    pub preview: RequestStatus<Box<VmwareImportPreview>>,
    /// Last import (reply after the conversion).
    pub import: RequestStatus<VmRecord>,
    /// Last progress event of the import in progress (cleared at every new
    /// import and when it ends).
    pub progress: Option<(VmwareImportPhase, u8)>,
    /// Last cancellation request: `true` if an import was being cancelled.
    pub cancel: RequestStatus<bool>,
}

impl ServiceState {
    /// VM by identifier.
    pub fn vm(&self, id: VmId) -> Option<&VmSummary> {
        self.vms.iter().find(|v| v.vm_id == id)
    }

    /// VMs with a QEMU process (starting, running, stopping, recovering).
    pub fn active_vms(&self) -> impl Iterator<Item = &VmSummary> {
        self.vms.iter().filter(|v| v.state.has_process())
    }
}

/// Actions requested by the UI from the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceCommand {
    /// A bounded Tools operation for the current VM run.
    GuestTools {
        vm_id: VmId,
        run_id: RunId,
        call: lightvm_proto::ToolCall,
    },
    /// Re-reads the VM list and the capabilities.
    Refresh,
    /// Creates a VM in the service's catalog (wizard).
    Create(Box<VmConfig>),
    /// Starts a VM.
    Start(VmId),
    /// Orderly stop of a VM (ACPI, then termination after the service
    /// timeout).
    Stop(VmId),
    /// Forced stop with OS primitives.
    ForceStop(VmId),
    /// Guest reset of the given run.
    Reset {
        /// VM.
        vm_id: VmId,
        /// Run to reset (a later run is not touched).
        run_id: RunId,
    },
    /// Deletion of a stopped VM.
    Delete {
        /// VM.
        vm_id: VmId,
        /// Revision the deletion is based on.
        revision: u64,
    },
    /// Emergency Stop (priority path of the service).
    EmergencyStop,
    /// Changes the fate of the VMs on UI loss (applies immediately).
    SetPolicy(ParentLossPolicy),
    /// Saves resources and limits of a stopped VM.
    UpdateVm {
        /// VM.
        vm_id: VmId,
        /// Revision the change is based on.
        revision: u64,
        /// New configuration.
        config: Box<VmConfig>,
    },
    /// Opens a VM by name (command line): applies the requested changes if
    /// the VM is stopped, then starts it.
    OpenVm {
        /// VM name.
        name: String,
        /// Changes to resources and limits.
        changes: crate::vm_settings::VmChanges,
        /// If the VM does not exist, create it with the default values plus
        /// the changes (without installation media).
        create: bool,
    },
    /// Re-reads the approved media.
    ListMedia,
    /// Imports an ISO chosen by the user with the native file picker.
    ImportMedia {
        /// File chosen in the picker.
        path: PathBuf,
        /// Kind of media.
        kind: MediaKind,
        /// Origin declared to the registry (text, never interpreted).
        origin: String,
    },
    /// Deletes an imported medium no VM uses (asks for confirmation first).
    DeleteMedia(MediaId),
    /// Requests a per-run console ticket.
    OpenConsole(VmId),
    /// Keyboard and mouse towards the guest.
    Input(InputBatch),
    /// Re-reads the baselines of a VM.
    ListBaselines(VmId),
    /// Creates a baseline of a stopped VM (long: storage lane).
    CreateBaseline {
        /// VM.
        vm_id: VmId,
        /// Label of the new baseline.
        label: Label,
        /// Consent to a baseline after an unclean stop, recorded as not
        /// verified.
        allow_unclean: bool,
    },
    /// Restores a baseline: the current state is discarded (long: storage
    /// lane).
    RestoreBaseline {
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
    /// Deletes a baseline that is not in use.
    DeleteBaseline {
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
    /// Re-reads the snapshots of a VM (ADR-0032).
    ListSnapshots(VmId),
    /// Takes a snapshot of a VM, off or running (long: storage lane).
    TakeSnapshot {
        /// VM.
        vm_id: VmId,
        /// Name given by the user.
        name: Label,
        /// Description given by the user.
        description: String,
    },
    /// Reverts a VM to a snapshot (VM off; long: storage lane).
    RevertSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
    },
    /// Deletes a snapshot (VM off or running; long: storage lane).
    DeleteSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
    },
    /// New name and description of a snapshot.
    RenameSnapshot {
        /// VM.
        vm_id: VmId,
        /// Snapshot.
        snapshot_id: SnapshotId,
        /// New name.
        name: Label,
        /// New description.
        description: String,
    },
    /// Re-reads the agent access switch (F4).
    GetAgentAccess,
    /// Turns agent access on or off (off: pending requests denied, live
    /// sessions revoked, pairing file removed).
    SetAgentAccess(bool),
    /// Re-reads the pending session requests.
    ListSessionRequests,
    /// Re-reads the sessions.
    ListSessions,
    /// Approves a session request.
    ApproveSession {
        /// Request.
        request_id: SessionRequestId,
        /// What the user grants.
        approval: SessionApproval,
    },
    /// Denies a session request.
    DenySession(SessionRequestId),
    /// Pauses a session (agent mutations refused, VM running).
    PauseSession(SessionId),
    /// Resumes a paused session.
    ResumeSession(SessionId),
    /// Revokes a session (VM not stopped).
    RevokeSession(SessionId),
    /// Sets the input owner of a VM (user, or a session of that VM).
    SetInputOwner {
        /// VM.
        vm_id: VmId,
        /// New owner.
        owner: InputOwner,
    },
    /// Issues a pairing credential and writes it to the pairing file.
    CreatePairing,
    /// Re-reads the session reports (F5).
    ListSessionReports,
    /// Reads the header of a report.
    GetSessionReport(SessionId),
    /// Reads a page of the events of a report.
    GetSessionReportEvents {
        /// Session.
        session_id: SessionId,
        /// First event (0-based).
        offset: u32,
        /// Events (1..=50).
        limit: u32,
        /// For the AI Workspace (last events), not the session page.
        tail: bool,
    },
    /// Reads a stored screenshot of a report (all its chunks), on demand.
    GetReportScreenshot {
        /// Session.
        session_id: SessionId,
        /// Screenshot index in the report.
        shot: u32,
    },
    /// Records the verdict of the local user.
    SetSessionVerdict {
        /// Session.
        session_id: SessionId,
        /// Outcome.
        outcome: TestOutcome,
        /// Rationale (plain text).
        rationale: String,
    },
    /// Runs an automatic verifier on the session's evidence.
    VerifySession {
        /// Session.
        session_id: SessionId,
        /// Verifier.
        verifier: SessionVerifier,
    },
    /// Exports a report to the file the user chose in the save dialog.
    ExportSessionReport {
        /// Session.
        session_id: SessionId,
        /// Format.
        format: ReportFormat,
        /// File chosen by the user (absolute).
        destination: PathBuf,
        /// Replace an existing file (the user confirmed it).
        overwrite: bool,
        /// Also write the stored screenshots next to it.
        include_screenshots: bool,
    },
    /// Deletes the report and the screenshots of a terminated session.
    DeleteSessionReport(SessionId),
    /// Examines the `.vmx` chosen by the user with the native file picker
    /// (ADR-0030): what would be imported, what not, warnings.
    PreviewVmwareImport(PathBuf),
    /// Imports the VMware VM by conversion (long: the reply arrives when the
    /// converted disk and the VM exist).
    ImportVmware {
        /// `.vmx` chosen in the picker.
        path: PathBuf,
        /// Name of the new VM.
        name: VmName,
        /// Display adapter.
        display: DisplayAdapter,
    },
    /// Cancels the VMware import in progress.
    CancelVmwareImport,
}

impl ServiceCommand {
    /// Agent control of the command, if it is one.
    pub fn agent_op(&self) -> Option<AgentOp> {
        Some(match self {
            Self::SetAgentAccess(on) => AgentOp::SetAccess(*on),
            Self::DenySession(id) => AgentOp::Deny(*id),
            Self::PauseSession(id) => AgentOp::Pause(*id),
            Self::ResumeSession(id) => AgentOp::Resume(*id),
            Self::RevokeSession(id) => AgentOp::Revoke(*id),
            Self::SetInputOwner { vm_id, owner } => AgentOp::InputOwner(*vm_id, *owner),
            _ => return None,
        })
    }
}

impl ServiceCommand {
    /// Report action of the command, if it is one (reads excluded).
    pub fn report_op(&self) -> Option<ReportOp> {
        Some(match self {
            Self::SetSessionVerdict { session_id, .. } => ReportOp::Verdict(*session_id),
            Self::VerifySession { session_id, .. } => ReportOp::Verify(*session_id),
            Self::ExportSessionReport { session_id, .. } => ReportOp::Export(*session_id),
            Self::DeleteSessionReport(id) => ReportOp::Delete(*id),
            _ => return None,
        })
    }

    /// Baseline operation of the command, if it is one (list excluded).
    pub fn baseline_op(&self) -> Option<(VmId, BaselineOp)> {
        match self {
            Self::CreateBaseline {
                vm_id,
                label,
                allow_unclean,
            } => Some((
                *vm_id,
                BaselineOp::Create {
                    label: label.clone(),
                    allow_unclean: *allow_unclean,
                },
            )),
            Self::RestoreBaseline { vm_id, baseline_id } => {
                Some((*vm_id, BaselineOp::Restore(*baseline_id)))
            }
            Self::DeleteBaseline { vm_id, baseline_id } => {
                Some((*vm_id, BaselineOp::Delete(*baseline_id)))
            }
            _ => None,
        }
    }

    /// Snapshot operation of the command, if it is one (list excluded).
    pub fn snapshot_op(&self) -> Option<(VmId, SnapshotOp)> {
        Some(match self {
            Self::TakeSnapshot { vm_id, .. } => (*vm_id, SnapshotOp::Take),
            Self::RevertSnapshot { vm_id, snapshot_id } => {
                (*vm_id, SnapshotOp::Revert(*snapshot_id))
            }
            Self::DeleteSnapshot { vm_id, snapshot_id } => {
                (*vm_id, SnapshotOp::Delete(*snapshot_id))
            }
            Self::RenameSnapshot {
                vm_id, snapshot_id, ..
            } => (*vm_id, SnapshotOp::Rename(*snapshot_id)),
            _ => return None,
        })
    }
}

/// Supervisor seen by the UI: the real child process ([`Service`]) or, in
/// headless tests, a stand-in built by the test.
pub trait Supervisor: Send {
    /// PID of the supervisor.
    fn pid(&self) -> u32;
    /// Copy of the current state.
    fn state(&self) -> ServiceState;
    /// Queues an action. Returns `false` if the channel is closed.
    fn send(&self, c: ServiceCommand) -> bool;
    /// The supervisor is still alive.
    fn is_alive(&mut self) -> bool;
    /// Controlled stop; returns the exit code and whether forcing was needed.
    fn stop(&mut self, timeout: Duration) -> (Option<i32>, bool);
    /// Hands over (once) the console ticket received for `vm_id`.
    fn take_console_ticket(&self, vm_id: VmId) -> Option<ConsoleTicket>;
    /// Drops a console ticket received and not handed over (the console
    /// was closed or its page left): the token does not stay in memory.
    fn discard_console_ticket(&self);
}

/// Supervisor start errors.
#[derive(Debug)]
pub enum StartError {
    /// Process not started.
    Spawn(std::io::Error),
    /// The service exited before the `ready` line (exit code, if known).
    ExitedBeforeReady(Option<i32>),
    /// No `ready` line within [`READY_TIMEOUT`].
    Timeout,
    /// Invalid `ready` line.
    InvalidLine(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "starting lightvm-service failed: {e}"),
            Self::ExitedBeforeReady(Some(3)) => {
                write!(
                    f,
                    "the catalog is already used by another lightvm-service instance"
                )
            }
            Self::ExitedBeforeReady(c) => {
                write!(f, "lightvm-service exited before being ready (code {c:?})")
            }
            Self::Timeout => write!(f, "lightvm-service did not report being ready"),
            Self::InvalidLine(m) => write!(f, "invalid lightvm-service start line: {m}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Data of the `ready` line (the credential is never logged).
struct Ready {
    pid: u32,
    endpoint: String,
    credential: Credential,
}

fn parse_ready(line: &str) -> Result<Ready, StartError> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|e| StartError::InvalidLine(e.to_string()))?;
    if v.get("event").and_then(|e| e.as_str()) != Some("ready") {
        return Err(StartError::InvalidLine("event other than ready".into()));
    }
    let pid = v
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| StartError::InvalidLine("missing pid".into()))?;
    let endpoint = v
        .get("endpoint")
        .and_then(|e| e.as_str())
        .ok_or_else(|| StartError::InvalidLine("missing endpoint".into()))?
        .to_owned();
    let credential = v
        .get("credential")
        .and_then(|c| c.as_str())
        .and_then(Credential::from_hex)
        .ok_or_else(|| StartError::InvalidLine("missing or malformed credential".into()))?;
    Ok(Ready {
        pid,
        endpoint,
        credential,
    })
}

/// Console ticket received and not yet handed to the console.
type TicketSlot = Arc<Mutex<Option<ConsoleTicket>>>;

/// Supervisor started by the UI. On drop it requests the controlled stop
/// and, once [`STOP_TIMEOUT`] has expired, kills the process.
pub struct Service {
    child: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    commands: mpsc::UnboundedSender<ServiceCommand>,
    state: Arc<Mutex<ServiceState>>,
    ticket: TicketSlot,
    /// Folder of the pairing file, removed when the service stops.
    pairing_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The ticket slot holds the display token: only whether it is full.
        let ticket = if lock(&self.ticket).is_some() {
            "<redacted>"
        } else {
            "none"
        };
        f.debug_struct("Service")
            .field("pid", &self.pid)
            .field("state", &self.state)
            .field("console_ticket", &ticket)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Service {
    /// Starts the service, waits for the `ready` line and opens the IPC
    /// connections in their own threads. `repaint` is called at every state
    /// change.
    pub fn start(
        cfg: &ServiceConfig,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, StartError> {
        let mut child = cfg.command().spawn().map_err(StartError::Spawn)?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or(StartError::Timeout)?;

        // The `ready` line is read in a thread: `read_line` has no timeout.
        let (tx_line, rx_line) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("service-stdout".into())
            .spawn(move || {
                let mut r = BufReader::new(stdout);
                let mut line = String::new();
                let outcome = r.read_line(&mut line).map(|n| (n, line));
                let _ = tx_line.send(outcome);
                // The rest of stdout is not used: discarded until closed.
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            })
            .map_err(StartError::Spawn)?;
        let ready = match rx_line.recv_timeout(READY_TIMEOUT) {
            Ok(Ok((n, line))) if n > 0 => parse_ready(line.trim()),
            Ok(_) => Err(StartError::ExitedBeforeReady(
                child.wait().ok().and_then(|s| s.code()),
            )),
            Err(_) => Err(StartError::Timeout),
        };
        let ready = match ready {
            Ok(p) => p,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

        let state = Arc::new(Mutex::new(ServiceState {
            connection: CONNECTING.into(),
            ..ServiceState::default()
        }));
        let ticket: TicketSlot = Arc::new(Mutex::new(None));
        let (commands, rx) = mpsc::unbounded_channel();
        let Ready {
            pid,
            endpoint,
            credential,
        } = ready;
        let pairing_dir = cfg.pairing_dir.clone().or_else(crate::pairing::default_dir);
        // A pairing file of a previous run holds a credential that did not
        // survive it.
        if let Some(d) = &pairing_dir {
            crate::pairing::remove(d);
        }
        // The IPC tasks keep only a weak sender: when the `Service` is
        // dropped the command channel closes and the IPC thread ends.
        let lanes = Lanes {
            state: state.clone(),
            ticket: ticket.clone(),
            repaint,
            tx: commands.downgrade(),
            endpoint: Arc::from(endpoint.as_str()),
            pairing_dir: pairing_dir.clone(),
        };
        std::thread::Builder::new()
            .name("service-ipc".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        lanes.update(|s| s.connection = format!("IPC runtime not started: {e}"));
                        return;
                    }
                };
                rt.block_on(ipc_loop(endpoint, credential, rx, lanes));
            })
            .map_err(StartError::Spawn)?;
        Ok(Self {
            child,
            stdin,
            pid,
            commands,
            state,
            ticket,
            pairing_dir,
        })
    }

    /// PID of the supervisor reported in the `ready` line.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Copy of the current state.
    pub fn state(&self) -> ServiceState {
        lock(&self.state).clone()
    }

    /// Queues an action. Returns `false` if the IPC channel is closed (the
    /// connection to the supervisor ended). The requests whose outcome a
    /// view waits for become `Pending` right away, so a stale outcome is
    /// never mistaken for the new one.
    pub fn send(&self, c: ServiceCommand) -> bool {
        send_command(&self.commands, &self.state, &self.ticket, c)
    }

    /// The service process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Controlled stop: `shutdown` line on stdin, then closing of the life
    /// pipe; once `timeout` has expired, forced termination. Returns the exit
    /// code and whether forcing was necessary.
    pub fn stop(&mut self, timeout: Duration) -> (Option<i32>, bool) {
        if let Some(d) = &self.pairing_dir {
            crate::pairing::remove(d);
        }
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"shutdown\n").and_then(|()| s.flush());
        }
        let t0 = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(st)) => return (st.code(), false),
                Ok(None) if t0.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let code = self.child.wait().ok().and_then(|s| s.code());
        (code, true)
    }
}

impl Supervisor for Service {
    fn pid(&self) -> u32 {
        Service::pid(self)
    }
    fn state(&self) -> ServiceState {
        Service::state(self)
    }
    fn send(&self, c: ServiceCommand) -> bool {
        Service::send(self, c)
    }
    fn is_alive(&mut self) -> bool {
        Service::is_alive(self)
    }
    fn stop(&mut self, timeout: Duration) -> (Option<i32>, bool) {
        Service::stop(self, timeout)
    }
    fn take_console_ticket(&self, vm_id: VmId) -> Option<ConsoleTicket> {
        let mut slot = lock(&self.ticket);
        if slot.as_ref().is_some_and(|t| t.vm_id == vm_id) {
            slot.take()
        } else {
            None
        }
    }
    fn discard_console_ticket(&self) {
        *lock(&self.ticket) = None;
    }
}

/// Body of [`Service::send`].
fn send_command(
    commands: &mpsc::UnboundedSender<ServiceCommand>,
    state: &Mutex<ServiceState>,
    ticket: &Mutex<Option<ConsoleTicket>>,
    c: ServiceCommand,
) -> bool {
    if commands.is_closed() {
        mark_failed(
            &mut lock(state),
            &c,
            Failure::new("supervisor connection closed"),
        );
        return false;
    }
    mark_pending(&mut lock(state), ticket, &c);
    let ok = commands.send(c.clone()).is_ok();
    if !ok {
        mark_failed(
            &mut lock(state),
            &c,
            Failure::new("supervisor connection closed"),
        );
    }
    ok
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(d) = &self.pairing_dir {
            crate::pairing::remove(d);
        }
        if self.is_alive() {
            let _ = self.stop(STOP_TIMEOUT);
        } else {
            let _ = self.child.wait();
        }
    }
}

/// Marks as `Pending` the request a view waits for. Public for the test
/// stand-ins of [`Supervisor`].
pub fn mark_pending(
    s: &mut ServiceState,
    ticket: &Mutex<Option<ConsoleTicket>>,
    c: &ServiceCommand,
) {
    match c {
        ServiceCommand::GuestTools { vm_id, run_id, .. } => {
            s.tools = Some((*vm_id, *run_id, RequestStatus::Pending))
        }
        ServiceCommand::Create(_) => s.create = RequestStatus::Pending,
        ServiceCommand::ImportMedia { .. } => s.import = RequestStatus::Pending,
        ServiceCommand::DeleteMedia(_) => s.delete_media = RequestStatus::Pending,
        ServiceCommand::Delete { .. } => s.delete = RequestStatus::Pending,
        ServiceCommand::OpenConsole(vm_id) => {
            *lock(ticket) = None;
            s.console = Some(ConsoleRequest {
                vm_id: *vm_id,
                status: RequestStatus::Pending,
            });
        }
        ServiceCommand::ApproveSession { request_id, .. } => {
            s.agents.approval = Some(Approval {
                request_id: *request_id,
                status: RequestStatus::Pending,
            });
        }
        ServiceCommand::CreatePairing => s.agents.pairing = RequestStatus::Pending,
        ServiceCommand::GetReportScreenshot { session_id, shot } => {
            s.reports
                .set_shot(*session_id, *shot, RequestStatus::Pending);
        }
        ServiceCommand::PreviewVmwareImport(path) => {
            s.vmware.preview_path = Some(path.clone());
            s.vmware.preview = RequestStatus::Pending;
        }
        ServiceCommand::ImportVmware { .. } => {
            s.vmware.import = RequestStatus::Pending;
            s.vmware.progress = None;
            s.vmware.cancel = RequestStatus::Idle;
        }
        ServiceCommand::CancelVmwareImport => s.vmware.cancel = RequestStatus::Pending,
        c => {
            if let Some(op) = c.report_op() {
                s.reports.action = Some(ReportControl {
                    op,
                    status: RequestStatus::Pending,
                });
            } else if let Some((vm_id, op)) = c.baseline_op() {
                s.baseline = Some(BaselineRequest {
                    vm_id,
                    op,
                    status: RequestStatus::Pending,
                });
            } else if let Some((vm_id, op)) = c.snapshot_op() {
                s.snapshot = Some(SnapshotRequest {
                    vm_id,
                    op,
                    status: RequestStatus::Pending,
                });
            } else if let Some(op) = c.agent_op() {
                s.agents.control = Some(AgentControl {
                    op,
                    status: RequestStatus::Pending,
                });
            }
        }
    }
}

/// Requests still waiting for an outcome when the connection ends fail:
/// no view keeps waiting for a supervisor that is gone.
fn fail_pending(s: &mut ServiceState, why: &str) {
    fn fail<T>(r: &mut RequestStatus<T>, why: &str) {
        if r.is_pending() {
            *r = RequestStatus::Failed(Failure::new(why));
        }
    }
    fail(&mut s.import, why);
    fail(&mut s.delete_media, why);
    fail(&mut s.create, why);
    fail(&mut s.delete, why);
    if let Some(r) = &mut s.console {
        fail(&mut r.status, why);
    }
    if let Some(r) = &mut s.baseline {
        fail(&mut r.status, why);
    }
    if let Some(r) = &mut s.snapshot {
        fail(&mut r.status, why);
    }
    if let Some(r) = &mut s.agents.approval {
        fail(&mut r.status, why);
    }
    if let Some(r) = &mut s.agents.control {
        fail(&mut r.status, why);
    }
    fail(&mut s.agents.pairing, why);
    fail(&mut s.vmware.preview, why);
    fail(&mut s.vmware.import, why);
    fail(&mut s.vmware.cancel, why);
    s.vmware.progress = None;
    for (_, r) in &mut s.reports.shots {
        fail(r, why);
    }
    if let Some(r) = &mut s.reports.action {
        fail(&mut r.status, why);
    }
    s.connected = false;
}

fn mark_failed(s: &mut ServiceState, c: &ServiceCommand, f: Failure) {
    match c {
        ServiceCommand::GuestTools { vm_id, run_id, .. } => {
            s.tools = Some((*vm_id, *run_id, RequestStatus::Failed(f)))
        }
        ServiceCommand::Create(_) => s.create = RequestStatus::Failed(f),
        ServiceCommand::ImportMedia { .. } => s.import = RequestStatus::Failed(f),
        ServiceCommand::DeleteMedia(_) => s.delete_media = RequestStatus::Failed(f),
        ServiceCommand::Delete { .. } => s.delete = RequestStatus::Failed(f),
        ServiceCommand::OpenConsole(vm_id) => {
            s.console = Some(ConsoleRequest {
                vm_id: *vm_id,
                status: RequestStatus::Failed(f),
            });
        }
        ServiceCommand::Input(_) => s.input_error = Some(f.message),
        ServiceCommand::ListBaselines(vm_id) => {
            s.baselines.entry(*vm_id).or_default().error = Some(f);
        }
        ServiceCommand::ListSnapshots(vm_id) => {
            s.snapshots.entry(*vm_id).or_default().error = Some(f);
        }
        ServiceCommand::GetAgentAccess
        | ServiceCommand::ListSessionRequests
        | ServiceCommand::ListSessions => s.agents.error = Some(f),
        ServiceCommand::ApproveSession { request_id, .. } => {
            s.agents.approval = Some(Approval {
                request_id: *request_id,
                status: RequestStatus::Failed(f),
            });
        }
        ServiceCommand::CreatePairing => s.agents.pairing = RequestStatus::Failed(f),
        ServiceCommand::PreviewVmwareImport(_) => s.vmware.preview = RequestStatus::Failed(f),
        ServiceCommand::ImportVmware { .. } => {
            s.vmware.import = RequestStatus::Failed(f);
            s.vmware.progress = None;
        }
        ServiceCommand::CancelVmwareImport => s.vmware.cancel = RequestStatus::Failed(f),
        ServiceCommand::ListSessionReports => s.reports.error = Some(f),
        ServiceCommand::GetSessionReport(id) => {
            if f.code == Some(ErrorCode::NotFound) {
                // Deleted (or pruned) meanwhile.
                s.reports.forget(*id);
            }
            s.reports.header_errors.insert(*id, f);
        }
        ServiceCommand::GetSessionReportEvents {
            session_id, tail, ..
        } => {
            if !tail {
                s.reports.events_errors.insert(*session_id, f);
            }
        }
        ServiceCommand::GetReportScreenshot { session_id, shot } => {
            s.reports
                .set_shot(*session_id, *shot, RequestStatus::Failed(f));
        }
        c if c.report_op().is_some() => {
            if let Some(op) = c.report_op() {
                s.reports.action = Some(ReportControl {
                    op,
                    status: RequestStatus::Failed(f),
                });
            }
        }
        c if c.agent_op().is_some() => {
            if let Some(op) = c.agent_op() {
                s.agents.control = Some(AgentControl {
                    op,
                    status: RequestStatus::Failed(f),
                });
            }
        }
        c if c.snapshot_op().is_some() => {
            if let Some((vm_id, op)) = c.snapshot_op() {
                s.snapshot = Some(SnapshotRequest {
                    vm_id,
                    op,
                    status: RequestStatus::Failed(f),
                });
            }
        }
        c => match c.baseline_op() {
            Some((vm_id, op)) => {
                s.baseline = Some(BaselineRequest {
                    vm_id,
                    op,
                    status: RequestStatus::Failed(f),
                });
            }
            None => {
                if matches!(c, ServiceCommand::Start(_) | ServiceCommand::OpenVm { .. })
                    && f.start_unavailable()
                {
                    s.start_unavailable_outcome = Some(f.message.clone());
                }
                s.last_outcome = Some(f.message);
            }
        },
    }
}

/// Readable label of the state of an operation.
pub fn operation_status_label(s: OperationStatus) -> &'static str {
    match s {
        OperationStatus::Queued => "queued",
        OperationStatus::Forwarded => "in progress",
        OperationStatus::Succeeded => "succeeded",
        OperationStatus::Failed => "failed",
        OperationStatus::Discarded => "discarded",
    }
}

fn describe_error(m: &ServerMessage) -> String {
    failure(m).message
}

fn failure(m: &ServerMessage) -> Failure {
    match m {
        ServerMessage::Error { error, .. } => match error {
            ReplyError::Operational(e) => Failure {
                details: e.details.clone().unwrap_or_default(),
                ..Failure::coded(e.code, format!("{}: {}", e.code.as_str(), e.message))
            },
            ReplyError::Protocol(e) => Failure::new(format!("protocol: {e}")),
        },
        ServerMessage::Closing { reason } => {
            Failure::new(format!("connection closed by the service: {reason:?}"))
        }
        other => Failure::new(format!("unexpected reply: {other:?}")),
    }
}

async fn connect(endpoint: &str, cred: &Credential) -> Result<Client, String> {
    let mut c = Client::connect(endpoint)
        .await
        .map_err(|e| format!("IPC connection failed: {e}"))?;
    match c.hello(cred).await {
        Ok(ServerMessage::HelloOk { .. }) => Ok(c),
        Ok(m) => Err(describe_error(&m)),
        Err(e) => Err(format!("handshake failed: {e}")),
    }
}

/// Shared parts of the IPC tasks.
#[derive(Clone)]
struct Lanes {
    state: Arc<Mutex<ServiceState>>,
    ticket: TicketSlot,
    repaint: Arc<dyn Fn() + Send + Sync>,
    /// Weak: the IPC tasks must not keep the command channel open.
    tx: mpsc::WeakUnboundedSender<ServiceCommand>,
    /// IPC endpoint (not a secret), for the pairing file.
    endpoint: Arc<str>,
    /// Folder of the pairing file.
    pairing_dir: Option<PathBuf>,
}

impl Lanes {
    fn update(&self, f: impl FnOnce(&mut ServiceState)) {
        f(&mut lock(&self.state));
        (self.repaint)();
    }

    /// Queues a follow-up command, if the service is still in use.
    fn queue(&self, c: ServiceCommand) {
        if let Some(tx) = self.tx.upgrade() {
            let _ = tx.send(c);
        }
    }
}

/// Bounded queue of the fast lane (console tickets and input).
#[derive(Default)]
struct FastQueue {
    items: Mutex<VecDeque<ServiceCommand>>,
    wake: tokio::sync::Notify,
    closed: AtomicBool,
}

/// Input batch made only of pointer moves: superseded by any later one
/// (absolute positions), so it can be dropped when the lane is congested.
fn pointer_only(c: &ServiceCommand) -> bool {
    matches!(c, ServiceCommand::Input(b)
        if b.events.iter().all(|e| matches!(e, InputEvent::PointerMove(_))))
}

/// Adds a command to the fast lane queue. Beyond [`FAST_LANE_CAPACITY`]
/// the waiting pointer-only batches are dropped; key and button events
/// and ticket requests are always kept.
fn push_fast(q: &mut VecDeque<ServiceCommand>, c: ServiceCommand) {
    if q.len() >= FAST_LANE_CAPACITY {
        q.retain(|w| !pointer_only(w));
        if q.len() >= FAST_LANE_CAPACITY && pointer_only(&c) {
            return;
        }
    }
    q.push_back(c);
}

impl FastQueue {
    fn push(&self, c: ServiceCommand) {
        push_fast(&mut lock(&self.items), c);
        self.wake.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Next command; `None` once closed and empty.
    async fn next(&self) -> Option<ServiceCommand> {
        loop {
            if let Some(c) = lock(&self.items).pop_front() {
                return Some(c);
            }
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            self.wake.notified().await;
        }
    }
}

async fn ipc_loop(
    endpoint: String,
    cred: Credential,
    mut rx: mpsc::UnboundedReceiver<ServiceCommand>,
    lanes: Lanes,
) {
    let connections = async {
        let r = connect(&endpoint, &cred).await?;
        let e = connect(&endpoint, &cred).await?;
        let f = connect(&endpoint, &cred).await?;
        let s = connect(&endpoint, &cred).await?;
        let b = connect(&endpoint, &cred).await?;
        let st = connect(&endpoint, &cred).await?;
        Ok::<_, String>((r, e, f, s, b, st))
    }
    .await;
    drop(cred);
    let (mut requests, mut events, mut fast, mut slow, mut storage, mut starts) = match connections
    {
        Ok(x) => x,
        Err(m) => {
            lanes.update(|s| s.connection = m);
            return;
        }
    };
    match events.request(Request::Subscribe).await {
        Ok(ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }) => {}
        Ok(m) => {
            let d = describe_error(&m);
            lanes.update(|s| s.connection = format!("event subscription refused: {d}"));
            return;
        }
        Err(e) => {
            lanes.update(|s| s.connection = format!("event subscription failed: {e}"));
            return;
        }
    }
    lanes.update(|s| {
        s.connection = "connected".into();
        s.connected = true;
    });
    lanes.queue(ServiceCommand::Refresh);
    lanes.queue(ServiceCommand::ListMedia);
    lanes.queue(ServiceCommand::GetAgentAccess);
    lanes.queue(ServiceCommand::ListSessionRequests);
    lanes.queue(ServiceCommand::ListSessions);
    lanes.queue(ServiceCommand::ListSessionReports);

    let ev_lanes = lanes.clone();
    let event_reader = async move {
        loop {
            match events.next_event().await {
                Ok(Ok(ServiceEvent::VmState { vm_id, state })) => {
                    ev_lanes.update(|s| {
                        if let Some(v) = s.vms.iter_mut().find(|v| v.vm_id == vm_id) {
                            v.state = state;
                        }
                    });
                    // The run_id changes on start: the list is re-read.
                    ev_lanes.queue(ServiceCommand::Refresh);
                }
                Ok(Ok(ServiceEvent::BaselinesChanged { vm_id })) => {
                    // Operation started or ended, baseline created, restored
                    // or deleted: VM summary (operation, current state) and
                    // list re-read.
                    ev_lanes.queue(ServiceCommand::Refresh);
                    ev_lanes.queue(ServiceCommand::ListBaselines(vm_id));
                }
                Ok(Ok(ServiceEvent::SnapshotsChanged { vm_id })) => {
                    // Snapshot taken, reverted to, renamed or deleted: the
                    // VM summary (operation, count, "you are here") and the
                    // tree are re-read.
                    ev_lanes.queue(ServiceCommand::Refresh);
                    ev_lanes.queue(ServiceCommand::ListSnapshots(vm_id));
                }
                Ok(Ok(ServiceEvent::VmwareImportProgress { phase, percent })) => {
                    ev_lanes.update(|s| {
                        // Only while this UI waits for an import: a late
                        // event after the reply is not shown.
                        if s.vmware.import.is_pending() {
                            s.vmware.progress = Some((phase, percent.min(100)));
                        }
                    });
                }
                Ok(Ok(ev)) => agent_event(&ev_lanes, ev),
                Ok(Err(m)) => {
                    let d = describe_error(&m);
                    ev_lanes.update(|s| {
                        s.connection = d;
                        s.connected = false;
                    });
                    return;
                }
                Err(e) => {
                    ev_lanes.update(|s| {
                        s.connection = format!("event channel closed: {e}");
                        s.connected = false;
                    });
                    return;
                }
            }
        }
    };

    let fast_queue = Arc::new(FastQueue::default());
    let (slow_tx, mut slow_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let (storage_tx, mut storage_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let (start_tx, mut start_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let main_lanes = lanes.clone();
    let fast_in = fast_queue.clone();
    // Ends when every strong sender is gone (the `Service` was dropped).
    let executor = async move {
        while let Some(c) = rx.recv().await {
            match c {
                // Input, tickets and the user's control over agents are
                // never queued behind a start, a stop or a long request.
                ServiceCommand::Input(_)
                | ServiceCommand::OpenConsole(_)
                | ServiceCommand::PauseSession(_)
                | ServiceCommand::ResumeSession(_)
                | ServiceCommand::RevokeSession(_)
                | ServiceCommand::SetInputOwner { .. }
                | ServiceCommand::SetAgentAccess(_)
                // The cancellation of a VMware import never waits behind it.
                | ServiceCommand::CancelVmwareImport => fast_in.push(c),
                // Imports read whole ISOs (hash), exports copy files (an export can write
                // hundreds of MiB of screenshots); a VMware preview reads the
                // VM's files and runs `qemu-img info`.
                ServiceCommand::ImportMedia { .. }
                | ServiceCommand::ExportSessionReport { .. }
                | ServiceCommand::PreviewVmwareImport(_) => {
                    let _ = slow_tx.send(c);
                }
                // A VMware import converts a whole disk: like a baseline,
                // the reply arrives when it ends, without timeout. A
                // snapshot operation (take, revert, delete) is the same:
                // a disk operation the supervisor settles before replying.
                ServiceCommand::CreateBaseline { .. }
                | ServiceCommand::RestoreBaseline { .. }
                | ServiceCommand::TakeSnapshot { .. }
                | ServiceCommand::RevertSnapshot { .. }
                | ServiceCommand::DeleteSnapshot { .. }
                | ServiceCommand::ImportVmware { .. } => {
                    let _ = storage_tx.send(c);
                }
                ServiceCommand::Start(_) | ServiceCommand::OpenVm { .. } => {
                    let _ = start_tx.send(c);
                }
                c => run(&mut requests, &main_lanes, c).await,
            }
        }
        fast_in.close();
        drop(slow_tx);
        drop(storage_tx);
        drop(start_tx);
    };
    let fast_lanes = lanes.clone();
    let fast_lane = async move {
        while let Some(c) = fast_queue.next().await {
            run(&mut fast, &fast_lanes, c).await;
        }
    };
    let slow_lanes = lanes.clone();
    let slow_lane = async move {
        while let Some(c) = slow_rx.recv().await {
            run(&mut slow, &slow_lanes, c).await;
        }
    };
    // Baseline creation and restore: the reply arrives when the operation
    // ends (minutes for a large disk), without timeout; only the Emergency
    // Stop, the closing of the connection or the service shutdown cancel it.
    // Starts: the reply arrives after the verification of the media and of
    // the baseline and the launch of QEMU (seconds); the VM list is polled
    // meanwhile on the request lane for the progress.
    let start_lanes = lanes.clone();
    let start_lane = async move {
        while let Some(c) = start_rx.recv().await {
            run(&mut starts, &start_lanes, c).await;
        }
    };
    let storage_lanes = lanes.clone();
    let storage_lane = async move {
        while let Some(c) = storage_rx.recv().await {
            run(&mut storage, &storage_lanes, c).await;
        }
    };
    // The loop ends when the service is no longer used (executor and lanes
    // done) or when the event connection closes (the supervisor is gone):
    // the command channel is then dropped, so `Service::send` returns
    // `false`, and the requests still waiting are marked as failed.
    tokio::select! {
        () = event_reader => {}
        _ = async { tokio::join!(executor, fast_lane, slow_lane, storage_lane, start_lane) } => {}
    }
    lanes.update(|s| fail_pending(s, "supervisor connection closed"));
}

/// Agent events (F4): the state the event carries is applied at once, the
/// lists are re-read, the event is recorded for the session page.
fn agent_event(lanes: &Lanes, ev: ServiceEvent) {
    let now = UtcTimestamp::now();
    match ev {
        ServiceEvent::AgentAccess { enabled } => {
            lanes.update(|s| {
                s.agents.access = Some(enabled);
                let text = if enabled {
                    "Agent access turned on"
                } else {
                    "Agent access turned off"
                };
                s.agents.record(now, None, text.into());
            });
            if !enabled && let Some(d) = &lanes.pairing_dir {
                crate::pairing::remove(d);
                lanes.update(|s| s.agents.pairing = RequestStatus::Idle);
            }
            lanes.queue(ServiceCommand::ListSessionRequests);
            lanes.queue(ServiceCommand::ListSessions);
        }
        ServiceEvent::SessionRequest { request } => lanes.update(|s| {
            let text = format!(
                "Session request from \u{201c}{}\u{201d}, code {}",
                request.client_name, request.verification_code
            );
            s.agents.record_request(now, request.connection_id, text);
            s.agents
                .requests
                .retain(|r| r.request_id != request.request_id);
            s.agents.requests.push(request);
        }),
        ServiceEvent::SessionRequestClosed {
            request_id,
            outcome,
        } => {
            lanes.update(|s| {
                let request = s.agents.request(request_id).map(|r| {
                    (
                        format!("\u{201c}{}\u{201d}", r.client_name),
                        r.connection_id,
                    )
                });
                s.agents.requests.retain(|r| r.request_id != request_id);
                let what = match outcome {
                    SessionRequestOutcome::Approved => "approved",
                    SessionRequestOutcome::Denied => "denied",
                    SessionRequestOutcome::Expired => "expired",
                    SessionRequestOutcome::Withdrawn => "withdrawn by the agent",
                };
                match request {
                    Some((c, conn)) => {
                        s.agents
                            .record_request(now, conn, format!("Request from {c} {what}"));
                    }
                    None => s
                        .agents
                        .record(now, None, format!("Session request {what}")),
                }
            });
            lanes.queue(ServiceCommand::ListSessions);
        }
        ServiceEvent::SessionChanged {
            session_id,
            state,
            termination,
        } => {
            lanes.update(|s| session_changed(s, session_id, state, termination, now));
            lanes.queue(ServiceCommand::ListSessions);
        }
        ServiceEvent::InputOwner { vm_id, owner } => {
            lanes.update(|s| set_input_owner(s, vm_id, owner, now));
            lanes.queue(ServiceCommand::ListSessions);
        }
        ServiceEvent::AgentSessionsStopped { .. } => {
            lanes.queue(ServiceCommand::ListSessionRequests);
            lanes.queue(ServiceCommand::ListSessions);
        }
        ServiceEvent::EventsLost { .. } => {
            lanes.queue(ServiceCommand::Refresh);
            lanes.queue(ServiceCommand::GetAgentAccess);
            lanes.queue(ServiceCommand::ListSessionRequests);
            lanes.queue(ServiceCommand::ListSessions);
            lanes.queue(ServiceCommand::ListSessionReports);
        }
        ServiceEvent::SessionReportChanged { session_id } => {
            let follow = report_changed(&lock(&lanes.state), session_id);
            for c in follow {
                lanes.queue(c);
            }
        }
        _ => {}
    }
}

/// Re-reads after `session_report_changed`: the list, and the header and
/// the events page of that report if the app shows them.
pub fn report_changed(s: &ServiceState, session_id: SessionId) -> Vec<ServiceCommand> {
    let mut v = vec![ServiceCommand::ListSessionReports];
    let r = &s.reports;
    if r.headers.contains_key(&session_id) || r.events.contains_key(&session_id) {
        v.push(ServiceCommand::GetSessionReport(session_id));
    }
    if let Some(p) = r.events.get(&session_id) {
        v.push(ServiceCommand::GetSessionReportEvents {
            session_id,
            offset: p.offset,
            limit: EVENTS_PAGE,
            tail: false,
        });
    }
    v
}

/// Applies a `session_changed` event and records it once.
fn session_changed(
    s: &mut ServiceState,
    session_id: SessionId,
    state: SessionState,
    termination: Option<TerminationCause>,
    now: UtcTimestamp,
) {
    let known = s.agents.logged.insert(session_id, state);
    let text = match (state, known) {
        (SessionState::Active, None) => "Session started".to_owned(),
        (SessionState::Active, Some(SessionState::Active))
        | (SessionState::Paused, Some(SessionState::Paused))
        | (SessionState::Terminated, Some(SessionState::Terminated)) => return,
        (SessionState::Active, Some(_)) => "Session resumed".to_owned(),
        (SessionState::Paused, _) => "Session paused".to_owned(),
        (SessionState::Terminated, _) => match termination {
            Some(c) => format!("Session ended: {}", termination_text(c)),
            None => "Session ended".to_owned(),
        },
    };
    if let Some(x) = s
        .agents
        .sessions
        .iter_mut()
        .find(|x| x.session_id == session_id)
    {
        x.state = state;
        x.termination = termination.or(x.termination);
    }
    s.agents.record(now, Some(session_id), text);
}

/// Records a new input owner of a VM (event or reply), once.
fn set_input_owner(s: &mut ServiceState, vm_id: VmId, owner: InputOwner, now: UtcTimestamp) {
    let previous = s.agents.input_owners.insert(vm_id, owner);
    for x in &mut s.agents.sessions {
        if x.vm_id == vm_id {
            x.input_owner =
                matches!(owner, InputOwner::Agent { session_id } if session_id == x.session_id);
        }
    }
    if previous == Some(owner) {
        return;
    }
    let (session, text) = match owner {
        InputOwner::Agent { session_id } => {
            (Some(session_id), "Keyboard and mouse handed to the agent")
        }
        InputOwner::User => (
            match previous {
                Some(InputOwner::Agent { session_id }) => Some(session_id),
                _ => None,
            },
            "Keyboard and mouse back to you",
        ),
    };
    if session.is_some() {
        s.agents.record(now, session, text.into());
    }
}

/// Records a successful agent control.
fn control_done(s: &mut ServiceState, op: AgentOp) {
    s.agents.control = Some(AgentControl {
        op,
        status: RequestStatus::Done(()),
    });
}

/// Executes a command and records the failure where the views look for it.
async fn run(c: &mut Client, lanes: &Lanes, command: ServiceCommand) {
    let kind = command.clone();
    match execute(c, lanes, command).await {
        Ok(()) => (lanes.repaint)(),
        Err(f) => lanes.update(|s| mark_failed(s, &kind, f)),
    }
}

async fn request(c: &mut Client, body: Request) -> Result<ServerMessage, Failure> {
    c.request(body)
        .await
        .map_err(|e| Failure::new(e.to_string()))
}

fn context(what: &str, f: Failure) -> Failure {
    Failure {
        message: format!("{what}: {}", f.message),
        ..f
    }
}

async fn execute(c: &mut Client, lanes: &Lanes, command: ServiceCommand) -> Result<(), Failure> {
    let state = &*lanes.state;
    match command {
        ServiceCommand::GuestTools {
            vm_id,
            run_id,
            call,
        } => {
            match request(
                c,
                Request::GuestTools {
                    vm_id,
                    run_id,
                    call,
                },
            )
            .await?
            {
                ServerMessage::Response {
                    result: ResponseBody::GuestTools { reply },
                    ..
                } => {
                    lanes.update(|s| s.tools = Some((vm_id, run_id, RequestStatus::Done(reply))));
                    Ok(())
                }
                m => Err(context("guest Tools", failure(&m))),
            }
        }

        ServiceCommand::Refresh => {
            let caps = request(c, Request::GetCapabilities).await?;
            if let ServerMessage::Response {
                result: ResponseBody::Capabilities(k),
                ..
            } = caps
            {
                let mut s = lock(state);
                s.backend = Some(k.backend);
                s.accelerator = k.accelerator;
                s.policy = Some(k.parent_loss_policy);
                s.host = k.host;
            }
            match request(c, Request::ListVms).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmList { vms },
                    ..
                } => {
                    // Baselines and snapshots not read yet, or changed
                    // since: re-read, so the Dashboard can name the
                    // baseline a VM is on and the snapshots it has.
                    let stale = {
                        let mut s = lock(state);
                        s.vms = vms;
                        let mut stale = Vec::new();
                        for v in s.vms.iter() {
                            let count = v.baselines.count as usize;
                            if (count > 0 || v.baselines.current.is_some())
                                && s.baselines
                                    .get(&v.vm_id)
                                    .is_none_or(|l| !l.loaded || l.items.len() != count)
                            {
                                stale.push(Some(ServiceCommand::ListBaselines(v.vm_id)));
                            }
                            let count = v.snapshots.count as usize;
                            if (count > 0 || v.snapshots.current.is_some())
                                && s.snapshots.get(&v.vm_id).is_none_or(|l| {
                                    !l.loaded
                                        || l.tree
                                            .as_ref()
                                            .is_none_or(|t| t.snapshots.len() != count)
                                })
                            {
                                stale.push(Some(ServiceCommand::ListSnapshots(v.vm_id)));
                            }
                        }
                        stale
                    };
                    for c in stale.into_iter().flatten() {
                        lanes.queue(c);
                    }
                    Ok(())
                }
                m => Err(failure(&m)),
            }
        }
        ServiceCommand::ListMedia => match request(c, Request::ListMedia).await? {
            ServerMessage::Response {
                result: ResponseBody::MediaList { media },
                ..
            } => {
                let mut s = lock(state);
                s.media = MediaState {
                    loaded: true,
                    records: media,
                    error: None,
                };
                Ok(())
            }
            m => {
                let f = context("approved media", failure(&m));
                lock(state).media.error = Some(f);
                Ok(())
            }
        },
        ServiceCommand::ImportMedia { path, kind, origin } => {
            let body = Request::ImportMedia {
                source_path: path.to_string_lossy().into_owned(),
                media_kind: kind,
                declared_origin: origin,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::MediaImported { media },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.media.records.retain(|m| m.id != media.id);
                        s.media.records.push(media.clone());
                        s.import = RequestStatus::Done(media);
                        s.last_outcome = Some("ISO imported".into());
                    }
                    lanes.queue(ServiceCommand::ListMedia);
                    Ok(())
                }
                m => Err(context("import", failure(&m))),
            }
        }
        ServiceCommand::DeleteMedia(media_id) => {
            match request(c, Request::DeleteMedia { media_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::MediaDeleted { media_id },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.media.records.retain(|m| m.id != media_id);
                        s.delete_media = RequestStatus::Done(media_id);
                        s.last_outcome = Some("medium deleted".into());
                    }
                    lanes.queue(ServiceCommand::ListMedia);
                    Ok(())
                }
                m => Err(context("media deletion", failure(&m))),
            }
        }
        ServiceCommand::Create(config) => {
            match request(c, Request::CreateVm { config: *config }).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmCreated { vm },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.last_outcome = Some(format!("created: {}", vm.config.name.as_str()));
                        s.create = RequestStatus::Done(vm.id);
                    }
                    refresh_after(c, lanes).await;
                    Ok(())
                }
                m => Err(context("creation", failure(&m))),
            }
        }
        ServiceCommand::Delete { vm_id, revision } => {
            let body = Request::DeleteVm {
                vm_id,
                expected_revision: revision,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmDeleted { vm_id },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        let name = s.vm(vm_id).map(|v| v.name.as_str().to_owned());
                        s.vms.retain(|v| v.vm_id != vm_id);
                        s.delete = RequestStatus::Done(vm_id);
                        s.last_outcome =
                            Some(format!("deleted: {}", name.unwrap_or_else(|| "VM".into())));
                    }
                    refresh_after(c, lanes).await;
                    Ok(())
                }
                m => Err(context("deletion", failure(&m))),
            }
        }
        ServiceCommand::OpenConsole(vm_id) => {
            match request(c, Request::OpenConsole { vm_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::Console { ticket },
                    ..
                } => {
                    let run_id = ticket.run_id;
                    *lock(&lanes.ticket) = Some(ticket);
                    lock(state).console = Some(ConsoleRequest {
                        vm_id,
                        status: RequestStatus::Done(run_id),
                    });
                    Ok(())
                }
                m => Err(context("console", failure(&m))),
            }
        }
        ServiceCommand::Input(batch) => {
            // Refused by the rate limit: the same batch once more after a
            // short wait, then (if still refused) only its key and button
            // releases, so the guest is not left with a key held down.
            let releases = releases_of(&batch);
            let mut attempts = vec![batch.clone(), batch];
            if let Some(r) = releases {
                attempts.push(r);
            }
            let mut last = None;
            for (i, b) in attempts.into_iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(INPUT_RETRY_DELAY).await;
                }
                let req = VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: b.vm_id,
                    expected_run_id: Some(b.run_id),
                    action: VmAction::Input(b),
                };
                match request(c, Request::SubmitVm { request: req }).await? {
                    ServerMessage::Response {
                        result: ResponseBody::Operation { operation, .. },
                        ..
                    } => {
                        let error = operation
                            .and_then(|o| o.error)
                            .map(|code| format!("input refused: {}", code.as_str()));
                        let mut s = lock(state);
                        if s.input_error != error {
                            s.input_error = error;
                        }
                        return Ok(());
                    }
                    m if rate_limited(&m) => last = Some(m),
                    m => return Err(context("input", failure(&m))),
                }
            }
            Err(context(
                "input",
                last.map_or_else(|| Failure::new("not sent"), |m| failure(&m)),
            ))
        }
        ServiceCommand::SetPolicy(policy) => {
            match request(c, Request::SetParentLossPolicy { policy }).await? {
                ServerMessage::Response {
                    result: ResponseBody::ParentLossPolicy { policy },
                    ..
                } => {
                    let mut s = lock(state);
                    s.policy = Some(policy);
                    s.last_outcome = Some(format!(
                        "if the UI closes unexpectedly: {}",
                        crate::settings::describe_policy(policy)
                    ));
                    Ok(())
                }
                m => Err(context("setting", failure(&m))),
            }
        }
        ServiceCommand::UpdateVm {
            vm_id,
            revision,
            config,
        } => {
            update_vm(c, state, vm_id, revision, *config).await?;
            refresh_after(c, lanes).await;
            Ok(())
        }
        ServiceCommand::OpenVm {
            name,
            changes,
            create,
        } => {
            Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await?;
            let found = lock(state)
                .vms
                .iter()
                .find(|v| v.name.as_str() == name)
                .cloned();
            let vm = match found {
                Some(v) => v,
                None if create => {
                    let mut config =
                        crate::vm_settings::default_config(&name).map_err(Failure::new)?;
                    changes.apply(&mut config);
                    Box::pin(execute(c, lanes, ServiceCommand::Create(Box::new(config)))).await?;
                    lock(state)
                        .vms
                        .iter()
                        .find(|v| v.name.as_str() == name)
                        .cloned()
                        .ok_or_else(|| {
                            Failure::new(format!("VM \"{name}\" not found after creation"))
                        })?
                }
                None => {
                    return Err(Failure::new(format!(
                        "VM \"{name}\" not found in the catalog"
                    )));
                }
            };
            let stopped = vm.state == lightvm_proto::VmState::Stopped;
            if !changes.is_empty() {
                if !stopped {
                    return Err(Failure::new(format!(
                        "VM \"{name}\" running: resources and limits can be changed only while the VM is stopped"
                    )));
                }
                let mut config = vm.config.clone();
                changes.apply(&mut config);
                if config != vm.config {
                    update_vm(c, state, vm.vm_id, vm.config_revision, config).await?;
                }
            }
            let start = if stopped {
                vm_action(c, state, vm.vm_id, None, VmAction::Start, "start").await
            } else {
                Ok(())
            };
            // The list is re-read even if the start fails: the saved changes
            // stay visible. A failed re-read does not hide the outcome of
            // the start.
            refresh_after(c, lanes).await;
            start
        }
        ServiceCommand::Start(vm_id) => {
            vm_action(c, state, vm_id, None, VmAction::Start, "start").await
        }
        ServiceCommand::Stop(vm_id) => {
            vm_action(c, state, vm_id, None, VmAction::Stop, "stop").await
        }
        ServiceCommand::ForceStop(vm_id) => {
            vm_action(c, state, vm_id, None, VmAction::ForceStop, "force stop").await
        }
        ServiceCommand::Reset { vm_id, run_id } => {
            vm_action(c, state, vm_id, Some(run_id), VmAction::Reset, "reset").await
        }
        ServiceCommand::ListBaselines(vm_id) => {
            match request(c, Request::ListBaselines { vm_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::Baselines { baselines, .. },
                    ..
                } => {
                    lock(state).baselines.insert(
                        vm_id,
                        BaselineList {
                            loaded: true,
                            items: baselines,
                            error: None,
                        },
                    );
                    Ok(())
                }
                m => Err(context("baselines", failure(&m))),
            }
        }
        ServiceCommand::CreateBaseline {
            vm_id,
            label,
            allow_unclean,
        } => {
            let op = BaselineOp::Create {
                label: label.clone(),
                allow_unclean,
            };
            let body = Request::CreateBaseline {
                vm_id,
                label,
                allow_unclean,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::BaselineCreated { baseline },
                    ..
                } => {
                    baseline_done(
                        lanes,
                        vm_id,
                        op,
                        baseline.baseline_id,
                        format!("baseline created: {}", baseline.label.as_str()),
                    );
                    Ok(())
                }
                m => Err(context("baseline", failure(&m))),
            }
        }
        ServiceCommand::RestoreBaseline { vm_id, baseline_id } => {
            match request(c, Request::RestoreBaseline { vm_id, baseline_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::BaselineRestored { .. },
                    ..
                } => {
                    let label = baseline_label(state, vm_id, baseline_id);
                    baseline_done(
                        lanes,
                        vm_id,
                        BaselineOp::Restore(baseline_id),
                        baseline_id,
                        format!("baseline restored: {label}"),
                    );
                    Ok(())
                }
                m => Err(context("restore", failure(&m))),
            }
        }
        ServiceCommand::DeleteBaseline { vm_id, baseline_id } => {
            let label = baseline_label(state, vm_id, baseline_id);
            match request(c, Request::DeleteBaseline { vm_id, baseline_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::BaselineDeleted { .. },
                    ..
                } => {
                    if let Some(l) = lock(state).baselines.get_mut(&vm_id) {
                        l.items.retain(|b| b.baseline_id != baseline_id);
                    }
                    baseline_done(
                        lanes,
                        vm_id,
                        BaselineOp::Delete(baseline_id),
                        baseline_id,
                        format!("baseline deleted: {label}"),
                    );
                    Ok(())
                }
                m => Err(context("baseline deletion", failure(&m))),
            }
        }
        ServiceCommand::ListSnapshots(vm_id) => {
            match request(c, Request::ListSnapshots { vm_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::Snapshots { tree, .. },
                    ..
                } => {
                    lock(state).snapshots.insert(
                        vm_id,
                        SnapshotList {
                            loaded: true,
                            tree: Some(tree),
                            error: None,
                        },
                    );
                    Ok(())
                }
                m => Err(context("snapshots", failure(&m))),
            }
        }
        ServiceCommand::TakeSnapshot {
            vm_id,
            name,
            description,
        } => {
            let body = Request::TakeSnapshot {
                vm_id,
                name,
                description,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::Snapshot { snapshot },
                    ..
                } => {
                    snapshot_done(
                        lanes,
                        vm_id,
                        SnapshotOp::Take,
                        snapshot.snapshot_id,
                        format!("snapshot taken: {}", snapshot.name.as_str()),
                    );
                    Ok(())
                }
                m => Err(context("snapshot", failure(&m))),
            }
        }
        ServiceCommand::RevertSnapshot { vm_id, snapshot_id } => {
            let label = snapshot_label(state, vm_id, snapshot_id);
            match request(c, Request::RevertSnapshot { vm_id, snapshot_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::SnapshotReverted { .. },
                    ..
                } => {
                    snapshot_done(
                        lanes,
                        vm_id,
                        SnapshotOp::Revert(snapshot_id),
                        snapshot_id,
                        format!("reverted to: {label}"),
                    );
                    Ok(())
                }
                m => Err(context("revert", failure(&m))),
            }
        }
        ServiceCommand::DeleteSnapshot { vm_id, snapshot_id } => {
            let label = snapshot_label(state, vm_id, snapshot_id);
            match request(c, Request::DeleteSnapshot { vm_id, snapshot_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::SnapshotDeleted { .. },
                    ..
                } => {
                    if let Some(t) = lock(state)
                        .snapshots
                        .get_mut(&vm_id)
                        .and_then(|l| l.tree.as_mut())
                    {
                        t.snapshots.retain(|s| s.snapshot_id != snapshot_id);
                        t.snapshots.iter_mut().for_each(|s| {
                            if s.parent == Some(snapshot_id) {
                                s.parent = None;
                            }
                        });
                        if t.current == Some(snapshot_id) {
                            t.current = None;
                        }
                    }
                    snapshot_done(
                        lanes,
                        vm_id,
                        SnapshotOp::Delete(snapshot_id),
                        snapshot_id,
                        format!("snapshot deleted: {label}"),
                    );
                    Ok(())
                }
                m => Err(context("snapshot deletion", failure(&m))),
            }
        }
        ServiceCommand::RenameSnapshot {
            vm_id,
            snapshot_id,
            name,
            description,
        } => {
            let body = Request::UpdateSnapshot {
                vm_id,
                snapshot_id,
                name,
                description,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::Snapshot { snapshot },
                    ..
                } => {
                    if let Some(s) = lock(state)
                        .snapshots
                        .get_mut(&vm_id)
                        .and_then(|l| l.tree.as_mut())
                        .and_then(|t| {
                            t.snapshots
                                .iter_mut()
                                .find(|s| s.snapshot_id == snapshot.snapshot_id)
                        })
                    {
                        s.name = snapshot.name.clone();
                        s.description = snapshot.description.clone();
                    }
                    snapshot_done(
                        lanes,
                        vm_id,
                        SnapshotOp::Rename(snapshot_id),
                        snapshot.snapshot_id,
                        format!("snapshot renamed: {}", snapshot.name.as_str()),
                    );
                    Ok(())
                }
                m => Err(context("snapshot rename", failure(&m))),
            }
        }
        ServiceCommand::GetAgentAccess => match request(c, Request::GetAgentAccess).await? {
            ServerMessage::Response {
                result: ResponseBody::AgentAccess { enabled },
                ..
            } => {
                lock(state).agents.access = Some(enabled);
                Ok(())
            }
            m => Err(context("agent access", failure(&m))),
        },
        ServiceCommand::SetAgentAccess(on) => {
            match request(c, Request::SetAgentAccess { enabled: on }).await? {
                ServerMessage::Response {
                    result: ResponseBody::AgentAccess { enabled },
                    ..
                } => {
                    if !enabled && let Some(d) = &lanes.pairing_dir {
                        crate::pairing::remove(d);
                    }
                    {
                        let mut s = lock(state);
                        s.agents.access = Some(enabled);
                        if !enabled {
                            s.agents.pairing = RequestStatus::Idle;
                        }
                        control_done(&mut s, AgentOp::SetAccess(on));
                    }
                    lanes.queue(ServiceCommand::ListSessionRequests);
                    lanes.queue(ServiceCommand::ListSessions);
                    Ok(())
                }
                m => Err(context("agent access", failure(&m))),
            }
        }
        ServiceCommand::ListSessionRequests => {
            match request(c, Request::ListSessionRequests).await? {
                ServerMessage::Response {
                    result: ResponseBody::SessionRequests { requests },
                    ..
                } => {
                    lock(state).agents.requests = requests;
                    Ok(())
                }
                m => Err(context("session requests", failure(&m))),
            }
        }
        ServiceCommand::ListSessions => match request(c, Request::ListSessions).await? {
            ServerMessage::Response {
                result: ResponseBody::Sessions { sessions },
                ..
            } => {
                let mut s = lock(state);
                s.agents.set_sessions(sessions);
                s.agents.error = None;
                Ok(())
            }
            m => Err(context("agent sessions", failure(&m))),
        },
        ServiceCommand::ApproveSession {
            request_id,
            approval,
        } => {
            let body = Request::ApproveSession {
                request_id,
                approval,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::Session { session },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        let id = session.session_id;
                        let (vm, give) = (session.vm_id, session.input_owner);
                        s.last_outcome =
                            Some(format!("agent session approved: {}", session.client_name));
                        s.agents.upsert(session);
                        if give {
                            set_input_owner(
                                &mut s,
                                vm,
                                InputOwner::Agent { session_id: id },
                                UtcTimestamp::now(),
                            );
                        }
                        s.agents.approval = Some(Approval {
                            request_id,
                            status: RequestStatus::Done(id),
                        });
                    }
                    // The request leaves the list with its `closed` event.
                    lanes.queue(ServiceCommand::ListSessionRequests);
                    lanes.queue(ServiceCommand::ListSessions);
                    Ok(())
                }
                m => Err(context("approval", failure(&m))),
            }
        }
        ServiceCommand::DenySession(request_id) => {
            match request(c, Request::DenySession { request_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::SessionDenied { .. },
                    ..
                } => {
                    control_done(&mut lock(state), AgentOp::Deny(request_id));
                    // The request leaves the list with its `closed` event.
                    lanes.queue(ServiceCommand::ListSessionRequests);
                    Ok(())
                }
                m => Err(context("denial", failure(&m))),
            }
        }
        ServiceCommand::PauseSession(session_id) => {
            let body = Request::PauseSession { session_id };
            session_reply(c, state, body, AgentOp::Pause(session_id), "pause").await
        }
        ServiceCommand::ResumeSession(session_id) => {
            let body = Request::ResumeSession { session_id };
            session_reply(c, state, body, AgentOp::Resume(session_id), "resume").await
        }
        ServiceCommand::RevokeSession(session_id) => {
            match request(c, Request::RevokeSession { session_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::SessionRevoked { report, .. },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.last_outcome = Some(format!(
                            "agent session revoked: {} queued operations discarded, {} already sent",
                            report.discarded.len(),
                            report.already_forwarded.len()
                        ));
                        control_done(&mut s, AgentOp::Revoke(session_id));
                    }
                    lanes.queue(ServiceCommand::ListSessions);
                    Ok(())
                }
                m => Err(context("revocation", failure(&m))),
            }
        }
        ServiceCommand::SetInputOwner { vm_id, owner } => {
            match request(c, Request::SetInputOwner { vm_id, owner }).await? {
                ServerMessage::Response {
                    result: ResponseBody::InputOwner { vm_id, owner },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        set_input_owner(&mut s, vm_id, owner, UtcTimestamp::now());
                        control_done(&mut s, AgentOp::InputOwner(vm_id, owner));
                    }
                    lanes.queue(ServiceCommand::ListSessions);
                    Ok(())
                }
                m => Err(context("input control", failure(&m))),
            }
        }
        ServiceCommand::CreatePairing => {
            let dir = lanes
                .pairing_dir
                .clone()
                .ok_or_else(|| Failure::new("no folder for the pairing file"))?;
            match request(c, Request::CreatePairingCredential).await? {
                ServerMessage::Response {
                    result:
                        ResponseBody::PairingCredential {
                            credential,
                            expires_in_s,
                        },
                    ..
                } => {
                    let secs = i64::try_from(expires_in_s).unwrap_or(i64::MAX / 1000);
                    let expires_at = UtcTimestamp::now()
                        .checked_add_millis(secs.saturating_mul(1000))
                        .unwrap_or_else(UtcTimestamp::now);
                    let endpoint = lanes.endpoint.clone();
                    // File system and `icacls`: off the IPC task.
                    let written = tokio::task::spawn_blocking(move || {
                        crate::pairing::write(&dir, &endpoint, credential, expires_at)
                    })
                    .await
                    .map_err(|e| Failure::new(format!("pairing file not written: {e}")))?
                    .map_err(Failure::new)?;
                    let mut s = lock(state);
                    s.agents.pairing = RequestStatus::Done(crate::pairing::PairingFile {
                        path: written,
                        expires_at,
                    });
                    Ok(())
                }
                m => Err(context("pairing", failure(&m))),
            }
        }
        ServiceCommand::ListSessionReports => {
            match request(c, Request::ListSessionReports).await? {
                ServerMessage::Response {
                    result: ResponseBody::SessionReports { reports },
                    ..
                } => {
                    let mut s = lock(state);
                    s.reports.list = reports;
                    s.reports.loaded = true;
                    s.reports.error = None;
                    Ok(())
                }
                m => Err(context("session reports", failure(&m))),
            }
        }
        ServiceCommand::GetSessionReport(session_id) => {
            match request(c, Request::GetSessionReport { session_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::SessionReport { report },
                    ..
                } => {
                    let mut s = lock(state);
                    let r = &mut s.reports;
                    let summary = report.summary();
                    match r.list.iter_mut().find(|x| x.session_id == session_id) {
                        Some(x) => *x = summary,
                        None => {
                            r.list.push(summary);
                            r.list.sort_by_key(|x| std::cmp::Reverse(x.started_at));
                        }
                    }
                    r.header_errors.remove(&session_id);
                    r.headers.insert(session_id, *report);
                    Ok(())
                }
                m => Err(context("session report", failure(&m))),
            }
        }
        ServiceCommand::GetSessionReportEvents {
            session_id,
            offset,
            limit,
            tail,
        } => {
            let body = Request::GetSessionReportEvents {
                session_id,
                offset,
                limit,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result:
                        ResponseBody::SessionReportEvents {
                            total,
                            offset,
                            events,
                            ..
                        },
                    ..
                } => {
                    let page = EventsPage {
                        total,
                        offset,
                        events,
                    };
                    let mut s = lock(state);
                    if tail {
                        s.reports.tail.insert(session_id, page);
                    } else {
                        s.reports.events_errors.remove(&session_id);
                        s.reports.events.insert(session_id, page);
                    }
                    Ok(())
                }
                m => Err(context("report events", failure(&m))),
            }
        }
        ServiceCommand::GetReportScreenshot { session_id, shot } => {
            let (png, sha256) = read_shot(c, session_id, shot).await?;
            let image = tokio::task::spawn_blocking(move || decode_shot(&png, sha256))
                .await
                .map_err(|e| Failure::new(format!("screenshot not decoded: {e}")))?
                .map_err(Failure::new)?;
            lock(state)
                .reports
                .set_shot(session_id, shot, RequestStatus::Done(image));
            Ok(())
        }
        ServiceCommand::SetSessionVerdict {
            session_id,
            outcome,
            rationale,
        } => {
            let body = Request::SetSessionVerdict {
                session_id,
                outcome,
                rationale,
            };
            report_reply(c, lanes, body, ReportOp::Verdict(session_id), "verdict").await
        }
        ServiceCommand::VerifySession {
            session_id,
            verifier,
        } => {
            let body = Request::VerifySession {
                session_id,
                verifier,
            };
            report_reply(c, lanes, body, ReportOp::Verify(session_id), "verification").await
        }
        ServiceCommand::ExportSessionReport {
            session_id,
            format,
            destination,
            overwrite,
            include_screenshots,
        } => {
            let Some(path) = destination.to_str().map(str::to_owned) else {
                return Err(Failure::coded(
                    ErrorCode::InvalidArgument,
                    "export: the chosen path is not valid Unicode; choose another name or folder",
                ));
            };
            let body = Request::ExportSessionReport {
                session_id,
                format,
                destination: path,
                overwrite,
                include_screenshots,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result:
                        ResponseBody::SessionReportExported {
                            format,
                            bytes,
                            sha256,
                            screenshots,
                            ..
                        },
                    ..
                } => {
                    let mut s = lock(state);
                    s.reports.action = Some(ReportControl {
                        op: ReportOp::Export(session_id),
                        status: RequestStatus::Done(ReportOutcome::Exported(ExportDone {
                            path: destination,
                            format,
                            bytes,
                            sha256,
                            screenshots,
                        })),
                    });
                    Ok(())
                }
                m => Err(context("export", failure(&m))),
            }
        }
        ServiceCommand::DeleteSessionReport(session_id) => {
            match request(c, Request::DeleteSessionReport { session_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::SessionReportDeleted { .. },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.reports.forget(session_id);
                        s.reports.action = Some(ReportControl {
                            op: ReportOp::Delete(session_id),
                            status: RequestStatus::Done(ReportOutcome::Deleted),
                        });
                    }
                    lanes.queue(ServiceCommand::ListSessionReports);
                    Ok(())
                }
                m => Err(context("deletion", failure(&m))),
            }
        }
        ServiceCommand::PreviewVmwareImport(path) => {
            let body = Request::PreviewVmwareImport {
                vmx_path: path.to_string_lossy().into_owned(),
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmwareImportPreview { preview },
                    ..
                } => {
                    let mut s = lock(state);
                    // A newer preview (another file) supersedes this one.
                    if s.vmware.preview_path.as_ref() == Some(&path) {
                        s.vmware.preview = RequestStatus::Done(preview);
                    }
                    Ok(())
                }
                m => Err(context("VMware VM", failure(&m))),
            }
        }
        ServiceCommand::ImportVmware {
            path,
            name,
            display,
        } => {
            let body = Request::ImportVmware {
                vmx_path: path.to_string_lossy().into_owned(),
                name,
                display,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmwareImported { vm },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.last_outcome = Some(format!("imported: {}", vm.config.name.as_str()));
                        s.vmware.progress = None;
                        s.vmware.import = RequestStatus::Done(vm);
                    }
                    refresh_after(c, lanes).await;
                    Ok(())
                }
                m => Err(context("VMware import", failure(&m))),
            }
        }
        ServiceCommand::CancelVmwareImport => {
            match request(c, Request::CancelVmwareImport).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmwareImportCancel { cancelled },
                    ..
                } => {
                    lock(state).vmware.cancel = RequestStatus::Done(cancelled);
                    Ok(())
                }
                m => Err(context("cancellation", failure(&m))),
            }
        }
        ServiceCommand::EmergencyStop => match request(c, Request::EmergencyStop).await? {
            ServerMessage::Response {
                result: ResponseBody::EmergencyStop(r),
                ..
            } => {
                // The Emergency Stop revokes every non-UI credential: the
                // pairing file is useless.
                if let Some(d) = &lanes.pairing_dir {
                    crate::pairing::remove(d);
                }
                let mut s = lock(state);
                s.agents.pairing = RequestStatus::Idle;
                s.last_outcome = Some(format!(
                    "Emergency Stop: {} processes terminated, completed: {}",
                    r.processes_terminated,
                    if r.completed { "yes" } else { "no" }
                ));
                lanes.queue(ServiceCommand::ListSessionRequests);
                lanes.queue(ServiceCommand::ListSessions);
                Ok(())
            }
            m => Err(failure(&m)),
        },
    }
}

/// Verdict or verification: the reply carries the updated report.
async fn report_reply(
    c: &mut Client,
    lanes: &Lanes,
    body: Request,
    op: ReportOp,
    what: &str,
) -> Result<(), Failure> {
    match request(c, body).await? {
        ServerMessage::Response {
            result: ResponseBody::SessionReport { report },
            ..
        } => {
            let follow = {
                let mut s = lock(&lanes.state);
                let id = report.session_id;
                let summary = report.summary();
                if let Some(x) = s.reports.list.iter_mut().find(|x| x.session_id == id) {
                    *x = summary;
                }
                s.reports.headers.insert(id, *report);
                s.reports.action = Some(ReportControl {
                    op,
                    status: RequestStatus::Done(ReportOutcome::Updated),
                });
                // The verdict appears among the events.
                report_changed(&s, op.session())
            };
            for c in follow {
                lanes.queue(c);
            }
            Ok(())
        }
        m => Err(context(what, failure(&m))),
    }
}

/// Reads every chunk of a stored screenshot: PNG bytes and announced
/// SHA-256 (checked by [`decode_shot`]).
async fn read_shot(
    c: &mut Client,
    session_id: SessionId,
    shot: u32,
) -> Result<(Vec<u8>, Sha256Digest), Failure> {
    let mut png = Vec::new();
    let mut index = 0;
    let mut expected: Option<(u32, Sha256Digest)> = None;
    loop {
        let body = Request::GetReportScreenshotChunk {
            session_id,
            shot,
            index,
        };
        match request(c, body).await? {
            ServerMessage::Response {
                result:
                    ResponseBody::ReportScreenshotChunk {
                        session_id: sid,
                        shot: n,
                        index: i,
                        chunks,
                        sha256,
                        data,
                    },
                ..
            } => {
                if sid != session_id || n != shot || i != index {
                    return Err(Failure::new("screenshot: chunk of another screenshot"));
                }
                match expected {
                    None => expected = Some((chunks, sha256)),
                    Some(e) if e != (chunks, sha256) => {
                        return Err(Failure::new("screenshot changed while it was read"));
                    }
                    Some(_) => {}
                }
                let bytes = lightvm_proto::ipc::base64_decode(&data)
                    .ok_or_else(|| Failure::new("screenshot: malformed chunk"))?;
                png.extend_from_slice(&bytes);
                if png.len() > MAX_SHOT_BYTES {
                    return Err(Failure::new("screenshot too large to show"));
                }
                index += 1;
                if index >= chunks {
                    return Ok((png, sha256));
                }
            }
            m => return Err(context("screenshot", failure(&m))),
        }
    }
}

/// Pause or resume: the reply carries the session.
async fn session_reply(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    body: Request,
    op: AgentOp,
    what: &str,
) -> Result<(), Failure> {
    match request(c, body).await? {
        ServerMessage::Response {
            result: ResponseBody::Session { session },
            ..
        } => {
            let mut s = lock(state);
            s.agents.upsert(session);
            control_done(&mut s, op);
            Ok(())
        }
        m => Err(context(what, failure(&m))),
    }
}

/// Label of a known baseline, for the outcome line.
fn baseline_label(state: &Mutex<ServiceState>, vm_id: VmId, id: BaselineId) -> String {
    lock(state)
        .baselines
        .get(&vm_id)
        .and_then(|l| l.items.iter().find(|b| b.baseline_id == id))
        .map_or_else(|| "baseline".into(), |b| b.label.as_str().to_owned())
}

/// Records a successful baseline operation and re-reads the VM list and the
/// baselines (queued: the storage lane does not wait for them).
fn baseline_done(lanes: &Lanes, vm_id: VmId, op: BaselineOp, id: BaselineId, outcome: String) {
    lanes.update(|s| {
        s.baseline = Some(BaselineRequest {
            vm_id,
            op,
            status: RequestStatus::Done(id),
        });
        s.last_outcome = Some(outcome);
    });
    lanes.queue(ServiceCommand::Refresh);
    lanes.queue(ServiceCommand::ListBaselines(vm_id));
}

/// Name of a known snapshot, for the outcome line.
fn snapshot_label(state: &Mutex<ServiceState>, vm_id: VmId, id: SnapshotId) -> String {
    lock(state)
        .snapshots
        .get(&vm_id)
        .and_then(|l| l.tree.as_ref())
        .and_then(|t| t.snapshots.iter().find(|s| s.snapshot_id == id))
        .map_or_else(|| "snapshot".into(), |s| s.name.as_str().to_owned())
}

/// Records a successful snapshot operation and re-reads the VM list and the
/// snapshots (queued: the storage lane does not wait for them).
fn snapshot_done(lanes: &Lanes, vm_id: VmId, op: SnapshotOp, id: SnapshotId, outcome: String) {
    lanes.update(|s| {
        s.snapshot = Some(SnapshotRequest {
            vm_id,
            op,
            status: RequestStatus::Done(id),
        });
        s.last_outcome = Some(outcome);
    });
    lanes.queue(ServiceCommand::Refresh);
    lanes.queue(ServiceCommand::ListSnapshots(vm_id));
}

/// Re-reads the list after a successful change: a failure is reported in
/// `last_outcome` only, the change itself stays successful.
async fn refresh_after(c: &mut Client, lanes: &Lanes) {
    if let Err(f) = Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await {
        lock(&lanes.state).last_outcome = Some(format!("VM list not refreshed: {f}"));
    }
}

/// The service refused the request because of its rate limit.
fn rate_limited(m: &ServerMessage) -> bool {
    matches!(m, ServerMessage::Error {
        error: ReplyError::Operational(e),
        ..
    } if e.code == ErrorCode::ResourceLimit)
}

/// Key and button releases of a batch, as a batch of their own (`None` if
/// there are none, or if the batch has nothing else).
fn releases_of(b: &InputBatch) -> Option<InputBatch> {
    let events: Vec<InputEvent> = b
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                InputEvent::Key(KeyAction { down: false, .. })
                    | InputEvent::PointerButton(PointerButton { down: false, .. })
            )
        })
        .cloned()
        .collect();
    // Wheel "releases" are part of a click, not a held button.
    let events: Vec<InputEvent> = events
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                InputEvent::PointerButton(PointerButton {
                    button: MouseButton::WheelUp | MouseButton::WheelDown,
                    ..
                })
            )
        })
        .collect();
    (!events.is_empty() && events.len() < b.events.len()).then(|| InputBatch {
        events,
        ..b.clone()
    })
}

async fn update_vm(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    vm_id: VmId,
    revision: u64,
    config: VmConfig,
) -> Result<(), Failure> {
    let body = Request::UpdateVm {
        vm_id,
        expected_revision: revision,
        config,
    };
    match request(c, body).await? {
        ServerMessage::Response {
            result: ResponseBody::VmUpdated { vm },
            ..
        } => {
            lock(state).last_outcome = Some(format!("settings saved: {}", vm.config.name.as_str()));
            Ok(())
        }
        m => Err(context("settings", failure(&m))),
    }
}

async fn vm_action(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    vm_id: VmId,
    expected_run_id: Option<RunId>,
    action: VmAction,
    name: &str,
) -> Result<(), Failure> {
    let req = VmRequest {
        operation_id: OperationId::new_v7(),
        vm_id,
        expected_run_id,
        action,
    };
    match request(c, Request::SubmitVm { request: req }).await? {
        ServerMessage::Response {
            result: ResponseBody::Operation { operation, .. },
            ..
        } => {
            let outcome = operation.map_or_else(
                || format!("{name}: accepted"),
                |o| match o.error {
                    Some(code) => {
                        format!(
                            "{name}: {} ({})",
                            operation_status_label(o.status),
                            code.as_str()
                        )
                    }
                    None => format!("{name}: {}", operation_status_label(o.status)),
                },
            );
            lock(state).last_outcome = Some(outcome);
            Ok(())
        }
        m => Err(context(name, failure(&m))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ready_line() {
        let cred = Credential::generate().unwrap();
        let line = format!(
            r#"{{"event":"ready","pid":42,"endpoint":"\\\\.\\pipe\\x","protocol_versions":[1],"credential":"{}"}}"#,
            cred.to_hex()
        );
        let p = parse_ready(&line).unwrap();
        assert_eq!(p.pid, 42);
        assert_eq!(p.endpoint, r"\\.\pipe\x");
        assert!(p.credential.ct_eq(&cred));
    }

    #[test]
    fn invalid_ready_line() {
        for line in [
            "",
            "not json",
            r#"{"event":"other","pid":1,"endpoint":"e","credential":"00"}"#,
            r#"{"event":"ready","endpoint":"e","credential":"00"}"#,
            r#"{"event":"ready","pid":1,"credential":"00"}"#,
            r#"{"event":"ready","pid":1,"endpoint":"e","credential":"zz"}"#,
        ] {
            assert!(
                matches!(parse_ready(line), Err(StartError::InvalidLine(_))),
                "{line}"
            );
        }
    }

    #[test]
    fn unsupported_is_recognised() {
        let m = ServerMessage::Error {
            request_id: Some(1),
            error: ReplyError::Operational(lightvm_proto::PublicError::unsupported("later")),
        };
        let f = failure(&m);
        assert!(f.unsupported);
        assert!(f.message.contains("later"));
    }

    #[test]
    fn pending_is_set_synchronously_and_ticket_cleared() {
        let mut s = ServiceState::default();
        let slot = Mutex::new(None);
        let vm = VmId::new_v7();
        mark_pending(&mut s, &slot, &ServiceCommand::OpenConsole(vm));
        assert_eq!(
            s.console,
            Some(ConsoleRequest {
                vm_id: vm,
                status: RequestStatus::Pending
            })
        );
        mark_failed(
            &mut s,
            &ServiceCommand::Delete {
                vm_id: vm,
                revision: 1,
            },
            Failure::new("x"),
        );
        assert_eq!(s.delete, RequestStatus::Failed(Failure::new("x")));
    }

    /// A start refused because the host cannot start VMs (no QEMU) is
    /// recognised, so the Dashboard can name the cause; other refusals are
    /// not.
    #[test]
    fn start_refused_for_missing_capability_is_recognised() {
        let vm = VmId::new_v7();
        let refused = Failure::coded(
            ErrorCode::Unsupported,
            "start: unsupported: capability not available on this host",
        )
        .with_detail("capability", "vm.start");
        assert!(refused.start_unavailable());
        let mut s = ServiceState::default();
        mark_failed(&mut s, &ServiceCommand::Start(vm), refused.clone());
        assert_eq!(s.last_outcome.as_deref(), Some(refused.message.as_str()));
        assert_eq!(s.start_unavailable_outcome, s.last_outcome);

        // Other capability, other code, other command: not recognised.
        let other =
            Failure::coded(ErrorCode::Unsupported, "x").with_detail("capability", "vm.stop");
        assert!(!other.start_unavailable());
        let denied =
            Failure::coded(ErrorCode::PermissionDenied, "y").with_detail("capability", "vm.start");
        assert!(!denied.start_unavailable());
        let mut s = ServiceState::default();
        mark_failed(&mut s, &ServiceCommand::Stop(vm), refused);
        assert_eq!(s.start_unavailable_outcome, None);
    }

    /// In-process IPC server speaking the real framing on the real local
    /// transport, with scripted replies: drives `ipc_loop` without a
    /// supervisor process.
    mod fake {
        use super::*;
        use lightvm_proto::ipc::{ClientMessage, Role};
        use lightvm_service::frame;
        use lightvm_service::transport::{Accepted, BoxedStream, EndpointOptions, Listener};

        pub(super) type Script =
            Arc<dyn Fn(&Request) -> Result<ResponseBody, ReplyError> + Send + Sync>;

        pub(super) struct Server {
            pub(super) endpoint: String,
            pub(super) requests: Arc<Mutex<Vec<Request>>>,
            /// Set: the event connection is closed (supervisor gone).
            pub(super) close_events: Arc<AtomicBool>,
            /// Events to push on the event connection.
            pub(super) events: Arc<Mutex<Vec<ServiceEvent>>>,
            /// Set: the reply to `create_baseline` or to a start is held back (a long
            /// operation) until it is cleared.
            pub(super) hold_baseline: Arc<AtomicBool>,
        }

        pub(super) fn start(script: Script) -> Server {
            let dir = std::env::temp_dir().join(format!(
                "lightvm-fake-ipc-{}-{}",
                std::process::id(),
                OperationId::new_v7()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // The endpoint accepts only a private directory.
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let opts = EndpointOptions {
                runtime_dir: Some(dir.clone()),
                catalog_dir: dir,
            };
            let requests = Arc::new(Mutex::new(Vec::new()));
            let close_events = Arc::new(AtomicBool::new(false));
            let events = Arc::new(Mutex::new(Vec::new()));
            let hold_baseline = Arc::new(AtomicBool::new(false));
            let (r2, c2) = (requests.clone(), close_events.clone());
            let (e2, h2) = (events.clone(), hold_baseline.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = Listener::bind(&opts).unwrap();
                    tx.send(listener.endpoint().to_owned()).unwrap();
                    loop {
                        if let Ok(Accepted::Ok(stream, _)) = listener.accept().await {
                            tokio::spawn(serve(
                                stream,
                                script.clone(),
                                r2.clone(),
                                c2.clone(),
                                e2.clone(),
                                h2.clone(),
                            ));
                        }
                    }
                });
            });
            Server {
                endpoint: rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                requests,
                close_events,
                events,
                hold_baseline,
            }
        }

        async fn serve(
            mut s: BoxedStream,
            script: Script,
            requests: Arc<Mutex<Vec<Request>>>,
            close: Arc<AtomicBool>,
            events: Arc<Mutex<Vec<ServiceEvent>>>,
            hold: Arc<AtomicBool>,
        ) {
            let max = frame::DEFAULT_MAX_FRAME;
            loop {
                let Ok(m) = frame::read_frame::<_, ClientMessage>(&mut s, max).await else {
                    return;
                };
                let reply = match m {
                    ClientMessage::Hello { request_id, .. } => ServerMessage::HelloOk {
                        request_id,
                        protocol_version: 1,
                        connection_id: 1,
                        role: Role::Ui,
                    },
                    ClientMessage::Request { request_id, body } => {
                        lock(&requests).push(body.clone());
                        if body == Request::Subscribe {
                            let ok = ServerMessage::Response {
                                request_id,
                                result: ResponseBody::Subscribed,
                            };
                            if frame::write_frame(&mut s, &ok, max).await.is_err() {
                                return;
                            }
                            while !close.load(Ordering::SeqCst) {
                                let pending: Vec<ServiceEvent> =
                                    std::mem::take(&mut *lock(&events));
                                for event in pending {
                                    let m = ServerMessage::Event { event };
                                    if frame::write_frame(&mut s, &m, max).await.is_err() {
                                        return;
                                    }
                                }
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            return;
                        }
                        let long = matches!(
                            body,
                            Request::CreateBaseline { .. } | Request::ImportVmware { .. }
                        ) || matches!(&body, Request::SubmitVm { request }
                                if request.action == VmAction::Start);
                        if long {
                            while hold.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                        match script(&body) {
                            Ok(result) => ServerMessage::Response { request_id, result },
                            Err(error) => ServerMessage::Error {
                                request_id: Some(request_id),
                                error,
                            },
                        }
                    }
                };
                if frame::write_frame(&mut s, &reply, max).await.is_err() {
                    return;
                }
            }
        }
    }

    struct Loop {
        commands: Option<mpsc::UnboundedSender<ServiceCommand>>,
        state: Arc<Mutex<ServiceState>>,
        ticket: TicketSlot,
        thread: std::thread::JoinHandle<()>,
        pairing_dir: PathBuf,
    }

    impl Loop {
        fn start(endpoint: String) -> Self {
            let state = Arc::new(Mutex::new(ServiceState {
                connection: CONNECTING.into(),
                ..ServiceState::default()
            }));
            let ticket: TicketSlot = Arc::new(Mutex::new(None));
            let (commands, rx) = mpsc::unbounded_channel();
            let pairing_dir = std::env::temp_dir().join(format!(
                "lightvm-fake-pairing-{}-{}",
                std::process::id(),
                OperationId::new_v7()
            ));
            let lanes = Lanes {
                state: state.clone(),
                ticket: ticket.clone(),
                repaint: Arc::new(|| {}),
                tx: commands.downgrade(),
                endpoint: Arc::from(endpoint.as_str()),
                pairing_dir: Some(pairing_dir.clone()),
            };
            let cred = Credential::generate().unwrap();
            let thread = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(ipc_loop(endpoint, cred, rx, lanes));
            });
            let l = Self {
                commands: Some(commands),
                state,
                ticket,
                thread,
                pairing_dir,
            };
            l.wait("connection", |s| s.connected);
            l
        }

        fn send(&self, c: ServiceCommand) -> bool {
            send_command(
                self.commands.as_ref().unwrap(),
                &self.state,
                &self.ticket,
                c,
            )
        }

        fn state(&self) -> ServiceState {
            lock(&self.state).clone()
        }

        fn wait(&self, what: &str, ok: impl Fn(&ServiceState) -> bool) {
            let t0 = Instant::now();
            while !ok(&self.state()) {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "{what}: {:?}",
                    self.state()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn wait_thread_end(&self) {
            let t0 = Instant::now();
            while !self.thread.is_finished() {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "IPC thread still running"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn base_reply(r: &Request) -> Result<ResponseBody, ReplyError> {
        match r {
            Request::ListMedia => Ok(ResponseBody::MediaList { media: Vec::new() }),
            // Everything else fails, the VM list included: every refresh fails.
            _ => Err(ReplyError::Operational(lightvm_proto::PublicError::new(
                ErrorCode::InfrastructureError,
                "scripted failure",
            ))),
        }
    }

    fn operation_ok() -> ResponseBody {
        ResponseBody::Operation {
            admission: lightvm_proto::Admission {
                operation_id: OperationId::new_v7(),
                status: OperationStatus::Succeeded,
                duplicate: false,
            },
            operation: None,
        }
    }

    fn rate_limit() -> ReplyError {
        ReplyError::Operational(lightvm_proto::PublicError::resource_limit(
            "request rate over the limit",
        ))
    }

    fn submitted_inputs(server: &fake::Server) -> Vec<InputBatch> {
        lock(&server.requests)
            .iter()
            .filter_map(|r| match r {
                Request::SubmitVm { request } => match &request.action {
                    VmAction::Input(b) => Some(b.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn batch(events: Vec<InputEvent>) -> InputBatch {
        InputBatch {
            vm_id: VmId::new_v7(),
            run_id: RunId::new_v7(),
            geometry: lightvm_proto::DisplayGeometry::new(640, 480).unwrap(),
            events,
        }
    }

    fn key(q: &str, down: bool) -> InputEvent {
        InputEvent::Key(KeyAction {
            qcode: lightvm_proto::QCode::parse(q).unwrap(),
            down,
        })
    }

    /// Finding 7: the deletion succeeded; the re-read of the list that
    /// follows fails: the deletion is reported as done, the refresh failure
    /// only in the last outcome.
    #[test]
    fn successful_delete_is_not_reported_failed_when_the_refresh_fails() {
        let server = fake::start(Arc::new(|r: &Request| match r {
            Request::DeleteVm { vm_id, .. } => Ok(ResponseBody::VmDeleted { vm_id: *vm_id }),
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        let vm = VmId::new_v7();
        assert!(l.send(ServiceCommand::Delete {
            vm_id: vm,
            revision: 1
        }));
        l.wait("deletion outcome", |s| !s.delete.is_pending());
        assert_eq!(l.state().delete, RequestStatus::Done(vm));
        // The refresh that follows fails: recorded in the last outcome only.
        l.wait("refresh failure recorded", |s| {
            s.last_outcome
                .as_deref()
                .is_some_and(|o| o.starts_with("VM list not refreshed"))
        });
        let s = l.state();
        assert_eq!(s.delete, RequestStatus::Done(vm), "{s:?}");
    }

    /// Finding 1: an input batch refused by the rate limit is sent again
    /// once after a short wait.
    #[test]
    fn rate_limited_input_is_retried() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = calls.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::SubmitVm { .. } => {
                if c2.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(rate_limit())
                } else {
                    Ok(operation_ok())
                }
            }
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        let b = batch(vec![key("a", true), key("a", false)]);
        assert!(l.send(ServiceCommand::Input(b.clone())));
        let t0 = Instant::now();
        while submitted_inputs(&server).len() < 2 {
            assert!(t0.elapsed() < Duration::from_secs(10), "no retry");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        let sent = submitted_inputs(&server);
        assert_eq!(sent, [b.clone(), b]);
        assert_eq!(l.state().input_error, None);
    }

    /// Finding 1: still refused after the retry: the key and button
    /// releases are sent on their own, so nothing stays held in the guest.
    #[test]
    fn releases_are_sent_alone_when_input_stays_refused() {
        let server = fake::start(Arc::new(|r: &Request| match r {
            Request::SubmitVm { request } => match &request.action {
                VmAction::Input(b) if b.events.len() == 1 => Ok(operation_ok()),
                _ => Err(rate_limit()),
            },
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        let b = batch(vec![
            InputEvent::PointerMove(lightvm_proto::PointerMove { x: 5, y: 5 }),
            key("ctrl", false),
        ]);
        assert!(l.send(ServiceCommand::Input(b.clone())));
        let t0 = Instant::now();
        while submitted_inputs(&server).len() < 3 {
            assert!(t0.elapsed() < Duration::from_secs(10), "releases not sent");
            std::thread::sleep(Duration::from_millis(10));
        }
        let sent = submitted_inputs(&server);
        assert_eq!(sent[2].events, [key("ctrl", false)]);
    }

    /// Finding 8: when the `Service` (the only strong sender) is dropped,
    /// the IPC executor, its lanes and the thread end, even though the
    /// event connection is still open.
    #[test]
    fn ipc_thread_ends_when_the_service_is_dropped() {
        let server = fake::start(Arc::new(base_reply));
        let mut l = Loop::start(server.endpoint.clone());
        // The loop is idle, the event connection open.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!l.thread.is_finished());
        drop(l.commands.take());
        l.wait_thread_end();
    }

    /// Finding 8: once the supervisor is gone (event connection closed) the
    /// loop ends, `send` returns `false` and waiting requests fail.
    #[test]
    fn send_returns_false_once_the_supervisor_is_gone() {
        let server = fake::start(Arc::new(base_reply));
        let l = Loop::start(server.endpoint.clone());
        // A console ticket request that never gets an answer in time.
        lock(&l.state).console = Some(ConsoleRequest {
            vm_id: VmId::new_v7(),
            status: RequestStatus::Pending,
        });
        server.close_events.store(true, Ordering::SeqCst);
        l.wait_thread_end();
        let s = l.state();
        assert!(!s.connected);
        assert!(matches!(
            s.console.map(|c| c.status),
            Some(RequestStatus::Failed(_))
        ));
        assert!(!l.send(ServiceCommand::Delete {
            vm_id: VmId::new_v7(),
            revision: 1
        }));
        assert!(matches!(l.state().delete, RequestStatus::Failed(_)));
    }

    fn baseline(vm_id: VmId, label: &str) -> BaselineSummary {
        BaselineSummary {
            baseline_id: BaselineId::new_v7(),
            vm_id,
            label: Label::parse(label).unwrap(),
            created_at: UtcTimestamp::from_unix_millis(1_791_710_000_000).unwrap(),
            clean_shutdown: true,
            protected: false,
            derived_from: None,
            config_revision: 1,
            virtual_size_bytes: 8 << 30,
            size_bytes: 3 << 30,
            disk_sha256: Sha256Digest::from_bytes([7; 32]),
            firmware_sha256: None,
            current: false,
            in_use: false,
        }
    }

    fn requested(server: &fake::Server, f: impl Fn(&Request) -> bool) -> bool {
        lock(&server.requests).iter().any(f)
    }

    /// F3: a baseline creation is long (the reply arrives when it ends,
    /// without timeout): it runs on its own lane, so the VM list, the
    /// baselines list and a deletion are answered meanwhile; its outcome
    /// then re-reads the list.
    #[test]
    fn baseline_creation_does_not_block_other_requests() {
        let vm = VmId::new_v7();
        let created = baseline(vm, "Baseline 1");
        let c2 = created.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::ListBaselines { vm_id } => Ok(ResponseBody::Baselines {
                vm_id: *vm_id,
                baselines: Vec::new(),
            }),
            Request::CreateBaseline { .. } => Ok(ResponseBody::BaselineCreated {
                baseline: c2.clone(),
            }),
            Request::DeleteBaseline { .. } => Err(ReplyError::Operational(
                lightvm_proto::PublicError::state_conflict("baseline in use")
                    .with_detail("reason", "baseline_referenced"),
            )),
            r => base_reply(r),
        }));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        let label = Label::parse("Baseline 1").unwrap();
        assert!(l.send(ServiceCommand::CreateBaseline {
            vm_id: vm,
            label: label.clone(),
            allow_unclean: false,
        }));
        assert!(matches!(
            l.state().baseline,
            Some(BaselineRequest {
                status: RequestStatus::Pending,
                ..
            })
        ));
        l.wait("create_baseline received", |_| {
            requested(&server, |r| matches!(r, Request::CreateBaseline { .. }))
        });
        // Held: the other lanes keep working.
        assert!(l.send(ServiceCommand::ListBaselines(vm)));
        l.wait("list answered during the creation", |s| {
            s.baselines.get(&vm).is_some_and(|b| b.loaded)
        });
        assert!(l.state().baseline.unwrap().status.is_pending());
        server.hold_baseline.store(false, Ordering::SeqCst);
        l.wait("creation done", |s| {
            s.baseline.as_ref().is_some_and(|b| !b.status.is_pending())
        });
        let s = l.state();
        assert_eq!(
            s.baseline,
            Some(BaselineRequest {
                vm_id: vm,
                op: BaselineOp::Create {
                    label,
                    allow_unclean: false
                },
                status: RequestStatus::Done(created.baseline_id),
            })
        );
        // The refused deletion keeps the code and the reason of the reply.
        let id = BaselineId::new_v7();
        assert!(l.send(ServiceCommand::DeleteBaseline {
            vm_id: vm,
            baseline_id: id
        }));
        l.wait("deletion refused", |s| {
            matches!(&s.baseline, Some(b) if matches!(b.op, BaselineOp::Delete(_)) && !b.status.is_pending())
        });
        let Some(BaselineRequest {
            status: RequestStatus::Failed(f),
            ..
        }) = l.state().baseline
        else {
            panic!("not failed")
        };
        assert_eq!(f.code, Some(ErrorCode::StateConflict));
        assert_eq!(f.reason(), Some("baseline_referenced"));
    }

    /// ADR-0030: the `.vmx` path goes to the service as chosen; the preview
    /// is kept; the import is a long request whose progress arrives as
    /// events and whose cancellation is not queued behind it.
    #[test]
    fn vmware_import_preview_progress_and_cancel() {
        use lightvm_proto::ipc::{
            VmwareController, VmwareDiskPreview, VmwareImportPhase, VmwareImportPreview,
        };
        let preview = VmwareImportPreview {
            vmx_file: "lab.vmx".into(),
            display_name: None,
            vmware_guest_os: "ubuntu-64".into(),
            config: crate::vm_settings::default_config("lab").unwrap(),
            disk: VmwareDiskPreview {
                file_name: "lab.vmdk".into(),
                slot: "sata0:0".into(),
                vmware_controller: VmwareController::Sata,
                create_type: "monolithicSparse".into(),
                virtual_size_bytes: 16 << 30,
                allocated_bytes: 1 << 30,
                snapshot_levels: 0,
            },
            required_bytes: 1 << 30,
            free_bytes: None,
            not_imported: Vec::new(),
            warnings: Vec::new(),
            vmware_svga_offered: false,
            vmware_tools_hint: false,
        };
        let record = VmRecord {
            id: VmId::new_v7(),
            config: preview.config.clone(),
            config_revision: 1,
            persisted_state: lightvm_proto::VmState::Stopped,
            created_at: UtcTimestamp::now(),
        };
        let (p2, r2) = (preview.clone(), record.clone());
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::PreviewVmwareImport { .. } => Ok(ResponseBody::VmwareImportPreview {
                preview: Box::new(p2.clone()),
            }),
            Request::ImportVmware { .. } => Ok(ResponseBody::VmwareImported { vm: r2.clone() }),
            Request::CancelVmwareImport => Ok(ResponseBody::VmwareImportCancel { cancelled: true }),
            r => base_reply(r),
        }));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        let vmx = PathBuf::from(r"D:\VMs\lab\lab.vmx");
        assert!(l.send(ServiceCommand::PreviewVmwareImport(vmx.clone())));
        l.wait(
            "preview",
            |s| matches!(&s.vmware.preview, RequestStatus::Done(p) if **p == preview),
        );
        assert!(requested(&server, |r| matches!(
            r,
            Request::PreviewVmwareImport { vmx_path } if vmx_path == r"D:\VMs\lab\lab.vmx"
        )));
        let name = VmName::parse("lab").unwrap();
        assert!(l.send(ServiceCommand::ImportVmware {
            path: vmx,
            name: name.clone(),
            display: DisplayAdapter::Standard,
        }));
        l.wait("import received", |_| {
            requested(
                &server,
                |r| matches!(r, Request::ImportVmware { name: n, .. } if *n == name),
            )
        });
        lock(&server.events).push(ServiceEvent::VmwareImportProgress {
            phase: VmwareImportPhase::Converting,
            percent: 37,
        });
        l.wait("progress", |s| {
            s.vmware.progress == Some((VmwareImportPhase::Converting, 37))
        });
        // Held import: the cancellation is answered meanwhile.
        assert!(l.send(ServiceCommand::CancelVmwareImport));
        l.wait("cancel answered", |s| {
            s.vmware.cancel == RequestStatus::Done(true)
        });
        assert!(l.state().vmware.import.is_pending());
        server.hold_baseline.store(false, Ordering::SeqCst);
        l.wait("import done", |s| {
            s.vmware.import == RequestStatus::Done(record.clone())
        });
        assert_eq!(l.state().vmware.progress, None);
    }

    /// A start replies only after the verification of media and baseline
    /// and the launch: it has its own lane, the VM list is read meanwhile.
    #[test]
    fn start_does_not_block_other_requests() {
        let server = fake::start(Arc::new(|r: &Request| match r {
            Request::SubmitVm { .. } => Ok(operation_ok()),
            Request::ListBaselines { vm_id } => Ok(ResponseBody::Baselines {
                vm_id: *vm_id,
                baselines: Vec::new(),
            }),
            r => base_reply(r),
        }));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        let vm = VmId::new_v7();
        assert!(l.send(ServiceCommand::Start(vm)));
        l.wait("start received", |_| {
            requested(&server, |r| matches!(r, Request::SubmitVm { .. }))
        });
        assert!(l.send(ServiceCommand::ListBaselines(vm)));
        l.wait("list answered during the start", |s| {
            s.baselines.get(&vm).is_some_and(|b| b.loaded)
        });
        assert_ne!(
            l.state().last_outcome.as_deref(),
            Some("start: accepted"),
            "start still waiting"
        );
        server.hold_baseline.store(false, Ordering::SeqCst);
        l.wait("start outcome", |s| {
            s.last_outcome.as_deref() == Some("start: accepted")
        });
    }

    /// F3: `baselines_changed` re-reads the VM list and the baselines of
    /// the VM.
    #[test]
    fn baselines_changed_rereads_the_list() {
        let vm = VmId::new_v7();
        let b = baseline(vm, "Clean install");
        let b2 = b.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::ListBaselines { vm_id } => Ok(ResponseBody::Baselines {
                vm_id: *vm_id,
                baselines: vec![b2.clone()],
            }),
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        lock(&server.events).push(ServiceEvent::BaselinesChanged { vm_id: vm });
        l.wait("list re-read after the event", |s| {
            s.baselines.get(&vm).is_some_and(|l| l.items == [b.clone()])
        });
        assert!(requested(&server, |r| *r == Request::ListBaselines { vm_id: vm }));
    }

    /// A baseline operation still waiting when the supervisor goes away
    /// fails (no view keeps waiting).
    #[test]
    fn pending_baseline_operation_fails_when_the_supervisor_is_gone() {
        let server = fake::start(Arc::new(base_reply));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        assert!(l.send(ServiceCommand::CreateBaseline {
            vm_id: VmId::new_v7(),
            label: Label::parse("x").unwrap(),
            allow_unclean: true,
        }));
        server.close_events.store(true, Ordering::SeqCst);
        l.wait_thread_end();
        assert!(matches!(
            l.state().baseline.map(|b| b.status),
            Some(RequestStatus::Failed(_))
        ));
    }

    /// The fast lane stays bounded: pointer-only batches are dropped when
    /// it is congested, keys and ticket requests never.
    #[test]
    fn fast_lane_is_bounded_and_keeps_keys() {
        let mut q = VecDeque::new();
        let mv = || {
            ServiceCommand::Input(batch(vec![InputEvent::PointerMove(
                lightvm_proto::PointerMove { x: 1, y: 1 },
            )]))
        };
        for i in 0..200 {
            push_fast(&mut q, mv());
            if i % 10 == 0 {
                push_fast(&mut q, ServiceCommand::Input(batch(vec![key("a", false)])));
            }
            assert!(q.len() <= FAST_LANE_CAPACITY + 20, "{}", q.len());
        }
        let keys = q.iter().filter(|c| !pointer_only(c)).count();
        assert_eq!(keys, 20, "key batches dropped");
        push_fast(&mut q, ServiceCommand::OpenConsole(VmId::new_v7()));
        assert!(matches!(q.back(), Some(ServiceCommand::OpenConsole(_))));
    }

    /// Finding 14: `Debug` of the service does not show the display token,
    /// and the ticket can be discarded.
    #[test]
    fn debug_redacts_the_console_ticket() {
        let token = "feedface".repeat(8);
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (commands, _rx) = mpsc::unbounded_channel();
        let s = Service {
            child,
            stdin: None,
            pid: 1,
            commands,
            state: Arc::new(Mutex::new(ServiceState::default())),
            pairing_dir: None,
            ticket: Arc::new(Mutex::new(Some(ConsoleTicket {
                vm_id: VmId::new_v7(),
                run_id: RunId::new_v7(),
                socket_path: "display".into(),
                token: token.clone(),
                geometry: None,
            }))),
        };
        let shown = format!("{s:?}");
        assert!(!shown.contains(&token), "{shown}");
        assert!(shown.contains("<redacted>"));
        s.discard_console_ticket();
        assert!(lock(&s.ticket).is_none());
    }

    fn session_info(id: SessionId, vm: VmId, state: SessionState, input: bool) -> SessionInfo {
        SessionInfo {
            tools: Default::default(),
            session_id: id,
            vm_id: vm,
            connection_id: Some(7),
            client_name: "test-agent".into(),
            purpose: "Check the editor".into(),
            state,
            termination: None,
            capabilities: [lightvm_proto::Capability::VmInput].into(),
            network_endpoints: Vec::new(),
            quotas: lightvm_proto::ipc::AGENT_DEFAULT_QUOTAS,
            usage: Default::default(),
            created_at: UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap(),
            expires_at: UtcTimestamp::from_unix_millis(1_791_713_520_000).unwrap(),
            lease_expires_at: UtcTimestamp::from_unix_millis(1_791_709_930_000).unwrap(),
            input_owner: input,
        }
    }

    /// F4: agent access, approval, input owner, revocation and pairing go
    /// through the IPC with the expected requests; the events update the
    /// state and the session log; the pairing credential lands only in the
    /// private file, never in the state.
    #[test]
    fn agent_sessions_over_ipc() {
        let vm = VmId::new_v7();
        let sid = SessionId::new_v7();
        let cred = "5a".repeat(32);
        let cred2 = cred.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::GetAgentAccess => Ok(ResponseBody::AgentAccess { enabled: false }),
            Request::SetAgentAccess { enabled } => {
                Ok(ResponseBody::AgentAccess { enabled: *enabled })
            }
            Request::ListSessionRequests => Ok(ResponseBody::SessionRequests {
                requests: Vec::new(),
            }),
            Request::ListSessions => Ok(ResponseBody::Sessions {
                sessions: Vec::new(),
            }),
            Request::ApproveSession { approval, .. } => Ok(ResponseBody::Session {
                session: session_info(
                    sid,
                    approval.vm_id,
                    SessionState::Active,
                    approval.give_input,
                ),
            }),
            Request::SetInputOwner { vm_id, owner } => Ok(ResponseBody::InputOwner {
                vm_id: *vm_id,
                owner: *owner,
            }),
            Request::RevokeSession { session_id } => Ok(ResponseBody::SessionRevoked {
                session_id: *session_id,
                report: Default::default(),
            }),
            Request::CreatePairingCredential => Ok(ResponseBody::PairingCredential {
                credential: cred2.clone(),
                expires_in_s: 600,
            }),
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        l.wait("access read", |s| s.agents.access == Some(false));

        assert!(l.send(ServiceCommand::SetAgentAccess(true)));
        l.wait("access on", |s| s.agents.access == Some(true));

        // A request arrives as an event.
        let request = SessionRequestInfo {
            request_id: 3,
            connection_id: 7,
            client_name: "test-agent".into(),
            peer_pid: None,
            purpose: "Check the editor".into(),
            requested_vms: vec![vm],
            requested_capabilities: Default::default(),
            requested_endpoints: Vec::new(),
            verification_code: "482-913".into(),
            requested_at: UtcTimestamp::now(),
            expires_at: UtcTimestamp::now(),
        };
        lock(&server.events).push(ServiceEvent::SessionRequest { request });
        l.wait("request listed", |s| s.agents.requests.len() == 1);

        let approval = SessionApproval {
            tools: Default::default(),
            vm_id: vm,
            capabilities: [lightvm_proto::Capability::VmInput].into(),
            duration_s: 3600,
            quotas: None,
            lease_ms: None,
            give_input: true,
            network_endpoints: Vec::new(),
        };
        assert!(l.send(ServiceCommand::ApproveSession {
            request_id: 3,
            approval: approval.clone(),
        }));
        l.wait(
            "approved",
            |s| matches!(&s.agents.approval, Some(a) if a.status == RequestStatus::Done(sid)),
        );
        // The request leaves the list with the re-read that follows.
        l.wait("request gone", |s| s.agents.requests.is_empty());
        let st = l.state();
        assert!(st.agents.agent_has_input(vm));
        assert!(lock(&server.requests).contains(&Request::ApproveSession {
            request_id: 3,
            approval
        }));

        // Taking control back: reply and event both say user.
        assert!(l.send(ServiceCommand::SetInputOwner {
            vm_id: vm,
            owner: InputOwner::User
        }));
        l.wait("input back", |s| !s.agents.agent_has_input(vm));
        lock(&server.events).push(ServiceEvent::InputOwner {
            vm_id: vm,
            owner: InputOwner::User,
        });
        lock(&server.events).push(ServiceEvent::SessionChanged {
            session_id: sid,
            state: SessionState::Paused,
            termination: None,
        });
        l.wait("pause recorded", |s| {
            s.agents.log.iter().any(|e| e.text == "Session paused")
        });
        let st = l.state();
        let texts: Vec<&str> = st
            .agents
            .log
            .iter()
            .filter(|e| e.session_id == Some(sid))
            .map(|e| e.text.as_str())
            .collect();
        // The input owner change is recorded once, though seen twice.
        assert_eq!(
            texts,
            [
                "Session request from \u{201c}test-agent\u{201d}, code 482-913",
                "Keyboard and mouse handed to the agent",
                "Keyboard and mouse back to you",
                "Session paused"
            ],
            "{:?}",
            st.agents.log
        );

        // Pairing: file written, state without the credential.
        assert!(l.send(ServiceCommand::CreatePairing));
        l.wait("pairing", |s| !s.agents.pairing.is_pending());
        let st = l.state();
        let RequestStatus::Done(f) = &st.agents.pairing else {
            panic!("{:?}", st.agents.pairing)
        };
        assert_eq!(f.path, l.pairing_dir.join(crate::pairing::FILE_NAME));
        let text = std::fs::read_to_string(&f.path).unwrap();
        assert!(text.contains(&cred));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["endpoint"], server.endpoint.as_str());
        assert!(!format!("{st:?}").contains(&cred));

        // Revocation, then access off: the pairing file is removed.
        assert!(l.send(ServiceCommand::RevokeSession(sid)));
        l.wait("revoked", |s| {
            matches!(&s.agents.control, Some(c) if c.op == AgentOp::Revoke(sid) && c.status == RequestStatus::Done(()))
        });
        assert!(l.send(ServiceCommand::SetAgentAccess(false)));
        l.wait("access off", |s| s.agents.access == Some(false));
        assert!(!f.path.exists());
        let _ = std::fs::remove_dir_all(&l.pairing_dir);
    }
    fn test_report(id: SessionId, outcome: TestOutcome) -> SessionReport {
        use lightvm_proto::report::{OutcomeSource, ReportVersions};
        let t = UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap();
        SessionReport {
            tools: Default::default(),
            report_format: lightvm_proto::report::REPORT_FORMAT.into(),
            session_id: id,
            vm_id: VmId::new_v7(),
            vm_name: "lab".into(),
            client_name: "test-agent".into(),
            objective: "Check the editor".into(),
            started_at: t,
            ended_at: Some(t),
            state: SessionState::Terminated,
            termination: Some(TerminationCause::Completed),
            outcome,
            outcome_source: if outcome == TestOutcome::Inconclusive {
                OutcomeSource::Termination
            } else {
                OutcomeSource::User
            },
            run_ids: Vec::new(),
            baseline_id: None,
            baseline: None,
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vm_manifest_sha256: Sha256Digest::from_bytes([1; 32]),
            vm_config_revision: 1,
            policy_revision: lightvm_proto::PolicyRevision(1),
            capabilities: Default::default(),
            network_endpoints: Vec::new(),
            quotas: lightvm_proto::ipc::AGENT_DEFAULT_QUOTAS,
            expires_at: t,
            versions: ReportVersions::default(),
            agent_declaration: None,
            verdicts: Vec::new(),
            errors: Default::default(),
            screenshots_retained: 2,
            screenshots_dropped: 0,
            events_total: 1,
            events_dropped: 0,
            events: Vec::new(),
            report_sha256: None,
        }
    }

    /// F5: reports list, header, events, a screenshot read in two chunks,
    /// checked and decoded, an altered one refused, verdict, export with
    /// the existing file confirmed, deletion, and `session_report_changed`.
    #[test]
    fn session_reports_over_ipc() {
        use sha2::Digest as _;
        let sid = SessionId::new_v7();
        let mut png_bytes = Vec::new();
        {
            let mut e = png::Encoder::new(&mut png_bytes, 3, 2);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&[10, 20, 30].repeat(6)).unwrap();
        }
        let sha = Sha256Digest::from_bytes(sha2::Sha256::digest(&png_bytes).into());
        let half = png_bytes.len() / 2;
        let parts = [png_bytes[..half].to_vec(), png_bytes[half..].to_vec()];
        // Outcome kept by the stand-in supervisor.
        let current = Arc::new(Mutex::new(TestOutcome::Inconclusive));
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::ListSessionReports => Ok(ResponseBody::SessionReports {
                reports: vec![test_report(sid, *lock(&current)).summary()],
            }),
            Request::GetSessionReport { session_id } => Ok(ResponseBody::SessionReport {
                report: Box::new(test_report(*session_id, *lock(&current))),
            }),
            Request::GetSessionReportEvents {
                session_id, offset, ..
            } => Ok(ResponseBody::SessionReportEvents {
                session_id: *session_id,
                total: 1,
                offset: *offset,
                events: vec![ReportEvent {
                    seq: 1,
                    at: UtcTimestamp::now(),
                    body: lightvm_proto::report::ReportEventBody::Note {
                        text: "hello".into(),
                    },
                }],
            }),
            Request::GetReportScreenshotChunk {
                session_id,
                shot,
                index,
            } => Ok(ResponseBody::ReportScreenshotChunk {
                session_id: *session_id,
                shot: *shot,
                index: *index,
                chunks: 2,
                // Screenshot 1: its file no longer matches the hash.
                sha256: if *shot == 0 {
                    sha
                } else {
                    Sha256Digest::from_bytes([0; 32])
                },
                data: lightvm_proto::ipc::base64_encode(&parts[*index as usize]),
            }),
            Request::SetSessionVerdict {
                session_id,
                outcome,
                ..
            } => {
                *lock(&current) = *outcome;
                Ok(ResponseBody::SessionReport {
                    report: Box::new(test_report(*session_id, *outcome)),
                })
            }
            Request::ExportSessionReport {
                session_id,
                format,
                overwrite,
                ..
            } => {
                if *overwrite {
                    Ok(ResponseBody::SessionReportExported {
                        session_id: *session_id,
                        format: *format,
                        bytes: 1234,
                        sha256: Sha256Digest::from_bytes([7; 32]),
                        screenshots: 2,
                    })
                } else {
                    Err(ReplyError::Operational(lightvm_proto::PublicError::new(
                        ErrorCode::StateConflict,
                        "destination exists",
                    )))
                }
            }
            Request::DeleteSessionReport { session_id } => Ok(ResponseBody::SessionReportDeleted {
                session_id: *session_id,
            }),
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        l.wait("reports listed", |s| {
            s.reports.loaded && s.reports.list.len() == 1
        });

        assert!(l.send(ServiceCommand::GetSessionReport(sid)));
        assert!(l.send(ServiceCommand::GetSessionReportEvents {
            session_id: sid,
            offset: 0,
            limit: EVENTS_PAGE,
            tail: false,
        }));
        l.wait("header and events", |s| {
            s.reports.headers.contains_key(&sid) && s.reports.events.contains_key(&sid)
        });

        for shot in [0, 1] {
            assert!(l.send(ServiceCommand::GetReportScreenshot {
                session_id: sid,
                shot
            }));
        }
        l.wait("screenshots", |s| {
            [0, 1].iter().all(|n| {
                matches!(
                    s.reports.shot(sid, *n),
                    Some(RequestStatus::Done(_) | RequestStatus::Failed(_))
                )
            })
        });
        let st = l.state();
        let Some(RequestStatus::Done(img)) = st.reports.shot(sid, 0) else {
            panic!("{:?}", st.reports.shots)
        };
        assert_eq!(img.size, [3, 2]);
        assert_eq!(img.sha256, sha);
        assert_eq!(&img.rgba[..4], &[10, 20, 30, 255]);
        let Some(RequestStatus::Failed(f)) = st.reports.shot(sid, 1) else {
            panic!("{:?}", st.reports.shots)
        };
        assert!(f.message.contains("does not match its SHA-256"), "{f:?}");

        assert!(l.send(ServiceCommand::SetSessionVerdict {
            session_id: sid,
            outcome: TestOutcome::Passed,
            rationale: "r".into(),
        }));
        l.wait("verdict", |s| {
            s.reports
                .headers
                .get(&sid)
                .is_some_and(|h| h.outcome == TestOutcome::Passed)
                && s.reports.list[0].outcome == TestOutcome::Passed
        });

        let dest = std::env::temp_dir().join("lightvm-report-test.md");
        let export = |overwrite| ServiceCommand::ExportSessionReport {
            session_id: sid,
            format: ReportFormat::Markdown,
            destination: dest.clone(),
            overwrite,
            include_screenshots: true,
        };
        assert!(l.send(export(false)));
        l.wait("conflict", |s| {
            matches!(&s.reports.action, Some(a)
                if matches!(&a.status, RequestStatus::Failed(f) if f.code == Some(ErrorCode::StateConflict)))
        });
        assert!(l.send(export(true)));
        l.wait("exported", |s| {
            matches!(&s.reports.action, Some(a)
                if matches!(&a.status, RequestStatus::Done(ReportOutcome::Exported(x)) if x.path == dest && x.screenshots == 2))
        });
        assert!(
            lock(&server.requests).contains(&Request::ExportSessionReport {
                session_id: sid,
                format: ReportFormat::Markdown,
                destination: dest.to_str().unwrap().into(),
                overwrite: true,
                include_screenshots: true,
            })
        );

        // The event re-reads the list and the report shown.
        let header_reads = || {
            lock(&server.requests)
                .iter()
                .filter(|r| matches!(r, Request::GetSessionReport { .. }))
                .count()
        };
        let before = header_reads();
        lock(&server.events).push(ServiceEvent::SessionReportChanged { session_id: sid });
        l.wait("re-read", |_| header_reads() > before);

        assert!(l.send(ServiceCommand::DeleteSessionReport(sid)));
        l.wait("deleted", |s| {
            matches!(&s.reports.action, Some(a) if a.status == RequestStatus::Done(ReportOutcome::Deleted))
                && !s.reports.headers.contains_key(&sid)
        });
    }
}
