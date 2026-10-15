//! Session reports (ROADMAP 9, F5): canonical JSON of an agent session,
//! verdicts with their source, events in order, screenshots by hash, and the
//! agent-side views used by the MCP tools `vm.logs`, `vm.snapshot`,
//! `vm.restore`.
//!
//! Rules:
//! - the report is recorded by the service, not by the agent: every agent
//!   call, every lifecycle step and every screenshot (hash, geometry, size);
//! - the agent's declaration is a claim, never an outcome: completing the
//!   conversation never yields `passed`. The outcome comes from the
//!   termination cause (default), an automatic verifier or an explicit
//!   judgement of the local user, each attributed ([`OutcomeSource`]);
//! - typed text is never recorded (only length and SHA-256), no host path,
//!   no token; every text is plain text for the viewers (no markup, no links).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BaselineId, CapabilitySet, ErrorCode, GuestOs, OperationId, PolicyRevision, Quotas, RunId,
    SessionId, SessionState, Sha256Digest, TerminationCause, TestOutcome, UtcTimestamp, VmId,
    VmState,
};

/// Value of [`SessionReport::report_format`].
pub const REPORT_FORMAT: &str = "lightvm-session-report-v1";
/// Events per page of `get_session_report_events`.
pub const MAX_REPORT_EVENTS_PAGE: u32 = 50;
/// Longest verdict rationale, in characters.
pub const MAX_RATIONALE_CHARS: usize = 2000;
/// Longest agent declaration summary, in characters.
pub const MAX_DECLARATION_CHARS: usize = 2000;
/// Longest tool name of a tool note, in characters.
pub const MAX_TOOL_NAME_CHARS: usize = 64;
/// Largest serialized `detail` of a tool note, in bytes.
pub const MAX_TOOL_NOTE_DETAIL_BYTES: usize = 512;
/// Screenshots stored per session (later ones: hash only).
pub const MAX_REPORT_SCREENSHOTS: u32 = 300;
/// Screenshot bytes stored per session.
pub const MAX_REPORT_SCREENSHOT_BYTES: u64 = 512 * 1024 * 1024;
/// Events kept per session (later ones are counted as dropped).
pub const MAX_REPORT_EVENTS: u32 = 20_000;
/// Reports kept in the catalog (oldest terminated ones are pruned).
pub const MAX_REPORTS_KEPT: usize = 100;
/// Entries per `logs` reply.
pub const MAX_LOG_ENTRIES: u32 = 200;
/// QEMU log lines per `logs` reply.
pub const MAX_QEMU_LOG_LINES: usize = 100;

/// Where the current outcome of a report comes from.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSource {
    /// Live session: not decided yet (the outcome reads `inconclusive`).
    Pending,
    /// Default from the termination cause.
    Termination,
    /// Automatic verifier (named in the verdict).
    Verifier,
    /// Explicit judgement of the local user in the app.
    User,
}

/// A verdict on the session, with its source and rationale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Verdict {
    /// Outcome.
    pub outcome: TestOutcome,
    /// Source.
    pub source: OutcomeSource,
    /// Verifier name and version, for [`OutcomeSource::Verifier`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<String>,
    /// Who decided: `service`, `verifier`, `local user`.
    pub decided_by: String,
    /// Reason (plain text).
    pub rationale: String,
    /// Instant.
    pub at: UtcTimestamp,
}

/// Declaration of the agent at the end of its work: a claim, never an outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentDeclaration {
    /// Outcome the agent claims.
    pub claimed_outcome: TestOutcome,
    /// Summary (plain text).
    pub summary: String,
    /// Instant.
    pub at: UtcTimestamp,
}

/// Baseline the session started from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportBaseline {
    /// Baseline.
    pub baseline_id: BaselineId,
    /// Label (plain text).
    pub label: String,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Created after an observed clean shutdown.
    pub clean_shutdown: bool,
    /// SHA-256 of the disk image.
    pub disk_sha256: Sha256Digest,
    /// SHA-256 of the UEFI variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_sha256: Option<Sha256Digest>,
}

/// Versions of the components that served the session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportVersions {
    /// `lightvm-service` version.
    pub service: String,
    /// IPC protocol version.
    pub ipc_protocol: u32,
    /// MCP protocol revision declared by the adapter, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_protocol: Option<String>,
    /// Backend name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Accelerator in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
}

/// Event of a report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportEvent {
    /// Order in the report, from 1.
    pub seq: u64,
    /// UTC instant.
    pub at: UtcTimestamp,
    /// Content.
    #[serde(flatten)]
    pub body: ReportEventBody,
}

/// Content of a [`ReportEvent`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportEventBody {
    /// Session lifecycle step.
    Session {
        /// State now.
        state: SessionState,
        /// Termination cause.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        termination: Option<TerminationCause>,
    },
    /// Agent call seen by the service (refusals included).
    Call {
        /// Call name.
        call: String,
        /// VM.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vm_id: Option<VmId>,
        /// Run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        /// Operation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_id: Option<OperationId>,
        /// Outcome.
        ok: bool,
        /// Error code, if refused or failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Tool note of the adapter (MCP tool name and a bounded summary).
    Tool {
        /// MCP tool.
        tool: String,
        /// Outcome.
        ok: bool,
        /// Error code (operational or adapter), if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
        /// Bounded summary (never typed text: length and SHA-256 only).
        #[serde(default)]
        detail: serde_json::Value,
    },
    /// Screenshot taken by the agent.
    Screenshot {
        /// Index among the screenshots of the report, from 0.
        shot: u32,
        /// Run.
        run_id: RunId,
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// SHA-256 of the PNG.
        sha256: Sha256Digest,
        /// PNG size.
        bytes: u64,
        /// Stored in the session artefacts (retention limits).
        retained: bool,
    },
    /// State change of the session's VM.
    VmState {
        /// State now.
        state: VmState,
        /// Run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
    },
    /// A verdict was recorded.
    Verdict {
        /// Outcome.
        outcome: TestOutcome,
        /// Source.
        source: OutcomeSource,
    },
    /// The agent declared its result.
    Declaration {
        /// Claimed outcome.
        claimed_outcome: TestOutcome,
    },
    /// Network event of the session's VM (ADR-0033): a connection through
    /// the endpoint allowlist ended, an attempt was refused, or the run's
    /// network was set up (`kind` of the audit: `network_setup`,
    /// `network_connection`, `network_denied`, `network_policy`).
    Network {
        /// Audit action name.
        action: String,
        /// Details (addresses and counts, never content).
        detail: crate::NetworkAudit,
    },
    /// Tools metadata, never file contents or clipboard/UI values.
    Tools {
        action: String,
        outcome: crate::AuditOutcome,
        detail: crate::ToolAudit,
    },
    /// Service note (plain text).
    Note {
        /// Text.
        text: String,
    },
}

/// Canonical report of an agent session (`report.json`). In
/// `get_session_report` replies `events` is empty: events are read by page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionReport {
    /// Named Tools resources authorized for this session.
    #[serde(default, skip_serializing_if = "crate::ToolGrant::is_empty")]
    pub tools: crate::ToolGrant,
    /// [`REPORT_FORMAT`].
    pub report_format: String,
    /// Session.
    pub session_id: SessionId,
    /// VM of the grant.
    pub vm_id: VmId,
    /// VM name at approval.
    pub vm_name: String,
    /// Client name declared by the agent (plain text).
    pub client_name: String,
    /// Objective (purpose declared by the agent and approved by the user).
    pub objective: String,
    /// Approval instant.
    pub started_at: UtcTimestamp,
    /// Termination instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UtcTimestamp>,
    /// Session state.
    pub state: SessionState,
    /// Termination cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationCause>,
    /// Current outcome.
    pub outcome: TestOutcome,
    /// Source of the current outcome.
    pub outcome_source: OutcomeSource,
    /// Runs used, in order of first use.
    pub run_ids: Vec<RunId>,
    /// Baseline the VM's current state was an overlay of at approval
    /// (`None` = standalone disk).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_id: Option<BaselineId>,
    /// Facts of that baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<ReportBaseline>,
    /// Declared guest OS.
    pub guest_os: GuestOs,
    /// SHA-256 of the canonical JSON of the VM configuration at approval.
    pub vm_manifest_sha256: Sha256Digest,
    /// VM configuration revision at approval.
    pub vm_config_revision: u64,
    /// Policy revision of the grant.
    pub policy_revision: PolicyRevision,
    /// Granted capabilities.
    pub capabilities: CapabilitySet,
    /// Granted network endpoints (`network.endpoints`, ADR-0033).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_endpoints: Vec<String>,
    /// Granted quotas.
    pub quotas: Quotas,
    /// Grant expiry.
    pub expires_at: UtcTimestamp,
    /// Component versions.
    pub versions: ReportVersions,
    /// Declaration of the agent (claim only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_declaration: Option<AgentDeclaration>,
    /// Verdicts in order (the last one is the current outcome).
    #[serde(default)]
    pub verdicts: Vec<Verdict>,
    /// Errors of agent calls and tools, by code.
    #[serde(default)]
    pub errors: BTreeMap<String, u32>,
    /// Screenshots stored.
    pub screenshots_retained: u32,
    /// Screenshots not stored (retention limits): hash only.
    pub screenshots_dropped: u32,
    /// Events recorded.
    pub events_total: u32,
    /// Events not recorded (limit).
    pub events_dropped: u32,
    /// Events (empty in header replies).
    #[serde(default)]
    pub events: Vec<ReportEvent>,
    /// SHA-256 of the canonical JSON with this field absent: detects
    /// alterations, it is not a tamper-proof seal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_sha256: Option<Sha256Digest>,
}

impl SessionReport {
    /// Summary for lists.
    #[must_use]
    pub fn summary(&self) -> SessionReportSummary {
        SessionReportSummary {
            session_id: self.session_id,
            vm_id: self.vm_id,
            vm_name: self.vm_name.clone(),
            client_name: self.client_name.clone(),
            objective: self.objective.clone(),
            started_at: self.started_at,
            ended_at: self.ended_at,
            state: self.state,
            termination: self.termination,
            outcome: self.outcome,
            outcome_source: self.outcome_source,
            events: self.events_total,
            screenshots: self.screenshots_retained + self.screenshots_dropped,
        }
    }
}

/// Summary of a report, for lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionReportSummary {
    /// Session.
    pub session_id: SessionId,
    /// VM.
    pub vm_id: VmId,
    /// VM name.
    pub vm_name: String,
    /// Client name (plain text).
    pub client_name: String,
    /// Objective (plain text).
    pub objective: String,
    /// Approval instant.
    pub started_at: UtcTimestamp,
    /// Termination instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UtcTimestamp>,
    /// Session state.
    pub state: SessionState,
    /// Termination cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationCause>,
    /// Current outcome.
    pub outcome: TestOutcome,
    /// Source of the outcome.
    pub outcome_source: OutcomeSource,
    /// Events recorded.
    pub events: u32,
    /// Screenshots taken.
    pub screenshots: u32,
}

/// Marker expected by the fixture verifier.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FixtureMarker {
    /// `LIGHTVM-FX1` on the data disk (after `fx.sh scrivi`).
    Present,
    /// No marker.
    Absent,
}

/// Automatic verifier the user can run on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionVerifier {
    /// ADR-0019 pixel verifier on the last screenshot of the session.
    FixtureMarker {
        /// Expected marker.
        expected: FixtureMarker,
    },
}

/// Export format.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReportFormat {
    /// Canonical JSON.
    Json,
    /// Markdown view (texts escaped).
    Markdown,
}

/// A baseline as seen by an agent session (no host details).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentBaselineView {
    /// Baseline.
    pub baseline_id: BaselineId,
    /// Label (plain text).
    pub label: String,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Created after an observed clean shutdown.
    pub clean_shutdown: bool,
    /// Initial baseline of the session (protected: never replaced by agents).
    pub initial: bool,
    /// Created by this session (`vm.snapshot`).
    pub created_by_session: bool,
    /// The VM's current state is an overlay of it.
    pub current: bool,
    /// The session may restore it (initial or created by the session).
    pub restorable: bool,
}

/// Source of a log entry.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    /// LightVM events of the session.
    Lightvm,
    /// QEMU diagnostics of the current run (host paths removed).
    Qemu,
}

/// Entry of `vm.logs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogEntry {
    /// Sequence (report event `seq` for LightVM entries, line number for QEMU).
    pub seq: u64,
    /// Instant (for QEMU lines: the instant of the read).
    pub at: UtcTimestamp,
    /// Source.
    pub source: LogSource,
    /// Text (plain text, control characters removed).
    pub text: String,
}
