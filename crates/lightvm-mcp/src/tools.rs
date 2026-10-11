//! Tool catalogue v0.2 (ROADMAP 9): parameters, structured results and
//! their schemas, descriptions shown to the model.
//!
//! MCP tool names use `_` instead of the `.` of the roadmap catalogue
//! (`vm_list` ↔ `vm.list`): several clients and model APIs only accept
//! `[A-Za-z0-9_-]` in tool names. Each tool's title carries the catalogue
//! name. No tool takes a host path, a QEMU option or a QMP command; VMs,
//! baselines, operations and sessions are referenced by identifiers.

use std::sync::Arc;

use rmcp::model::{Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use lightvm_proto::report::{AgentBaselineView, LogEntry, SessionReportSummary};
use lightvm_proto::{
    DisplayGeometry, ErrorCode, OperationId, OperationKind, OperationStatus, RunId, SessionId,
    Sha256Digest, TestOutcome, UtcTimestamp, VmId, VmState,
};

use crate::schema::schema_for;

// ----- parameters -------------------------------------------------------------

/// No parameter.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// `session_request`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionRequestParams {
    /// Objective of the session, shown to the user who approves it
    /// (plain text, 1..=512 characters).
    #[schemars(length(min = 1, max = 512))]
    pub purpose: String,
}

/// `session_status`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionStatusParams {
    /// Seconds to wait for the user's decision while the request is
    /// pending (0..=50, default 30).
    #[serde(default)]
    #[schemars(range(max = 50))]
    pub wait_s: Option<u32>,
}

/// A VM of the session.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
}

/// `vm_stop`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmStopParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// `true` = forced stop (process terminated, unclean); default: ordinary
    /// ACPI shutdown request.
    #[serde(default)]
    pub force: Option<bool>,
}

/// Size of the returned screenshot image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    /// Display size.
    #[default]
    Full,
    /// Half width and height (fewer tokens); coordinates stay in display pixels.
    Half,
}

/// `vm_screenshot`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// `full` (default) or `half`.
    #[serde(default)]
    pub scale: Option<Scale>,
}

/// `vm_mouse_move`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MouseMoveParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// X in display pixels (0 = left).
    pub x: u32,
    /// Y in display pixels (0 = top).
    pub y: u32,
}

/// Mouse button.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    /// Left (default).
    #[default]
    Left,
    /// Middle.
    Middle,
    /// Right.
    Right,
}

/// `vm_mouse_click`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MouseClickParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// X in display pixels; with `y`, the pointer moves there first.
    #[serde(default)]
    pub x: Option<u32>,
    /// Y in display pixels.
    #[serde(default)]
    pub y: Option<u32>,
    /// Button (default `left`).
    #[serde(default)]
    pub button: Option<Button>,
    /// Double click.
    #[serde(default)]
    pub double: Option<bool>,
}

/// `vm_keyboard_type`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// Text: printable ASCII, `\n` (Enter) and `\t` (Tab) only, 1..=1024
    /// characters. Never recorded: the report keeps its length and SHA-256.
    #[schemars(length(min = 1, max = 1024))]
    pub text: String,
    /// Keyboard layout of the guest; only `us` is supported.
    #[serde(default)]
    pub layout: Option<String>,
}

/// `vm_keyboard_press`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PressParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// Key or chord: `ret`, `esc`, `tab`, `ctrl+alt+t`, `ctrl+c`, `alt+f4`,
    /// `up`, `f5`… (QEMU key names; aliases enter, space, escape, control,
    /// super, pageup, pagedown, del).
    #[schemars(length(min = 1, max = 64))]
    pub keys: String,
    /// Repetitions (1..=20, default 1).
    #[serde(default)]
    #[schemars(range(min = 1, max = 20))]
    pub repeat: Option<u32>,
}

/// `vm_snapshot`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotParams {
    /// VM identifier from `vm_list` (the VM must be stopped).
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// Label of the checkpoint (plain text, 1..=200 characters).
    #[schemars(length(min = 1, max = 200))]
    pub label: String,
}

/// `vm_restore`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreParams {
    /// VM identifier from `vm_list` (the VM must be stopped).
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// Baseline from `vm_baselines` with `restorable: true`.
    #[schemars(length(min = 1, max = 64))]
    pub baseline_id: String,
}

/// `vm_logs`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogsParams {
    /// VM identifier from `vm_list`.
    #[schemars(length(min = 1, max = 64))]
    pub vm_id: String,
    /// Only LightVM entries after this sequence (`last_seq` of a previous call).
    #[serde(default)]
    pub after_seq: Option<u64>,
    /// Entries (1..=200, default 50).
    #[serde(default)]
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u32>,
}

/// `operation_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationParams {
    /// Operation identifier returned by a previous tool.
    #[schemars(length(min = 1, max = 64))]
    pub operation_id: String,
}

/// `report_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportParams {
    /// Session identifier (default: this session; other sessions are not visible).
    #[serde(default)]
    #[schemars(length(min = 1, max = 64))]
    pub session_id: Option<String>,
}

/// `session_declare`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclareParams {
    /// Outcome you believe the test had. Recorded as your claim: the
    /// outcome of the report is decided by a verifier or by the user.
    pub claimed_outcome: TestOutcome,
    /// What you did and observed (plain text, 1..=2000 characters).
    #[schemars(length(min = 1, max = 2000))]
    pub summary: String,
}

// ----- results ----------------------------------------------------------------

/// State of the adapter's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No session requested.
    None,
    /// Waiting for the user's approval in the LightVM app.
    Pending,
    /// Approved: VM tools available within the grant.
    Active,
    /// Denied, expired, revoked or ended: request a new session.
    Closed,
}

/// Result of the session tools.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SessionOut {
    /// State.
    pub state: Phase,
    /// What to do next (plain text).
    pub message: String,
    /// Code the user compares in the app before approving.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_code: Option<String>,
    /// The pending request expires at this instant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_expires_at: Option<UtcTimestamp>,
    /// Session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// VMs of the grant.
    pub vm_ids: Vec<VmId>,
    /// Granted capabilities (`vm.list`, `vm.input`…).
    pub capabilities: Vec<String>,
    /// Grant expiry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UtcTimestamp>,
    /// Reason of the closure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A VM of the grant.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VmOut {
    /// VM.
    pub vm_id: VmId,
    /// Name.
    pub name: String,
    /// State.
    pub state: VmState,
    /// Current run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Display size in pixels (pointer coordinates use it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayGeometry>,
    /// `true` if this session owns the VM's mouse and keyboard.
    pub input_owner: bool,
}

/// `vm_list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VmListOut {
    /// VMs of the grant.
    pub vms: Vec<VmOut>,
}

/// An operation.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OperationOut {
    /// Operation (pass it to `operation_get`).
    pub operation_id: OperationId,
    /// VM.
    pub vm_id: VmId,
    /// Kind.
    pub kind: OperationKind,
    /// Status (`forwarded` = in progress).
    pub status: OperationStatus,
    /// Run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Error code if failed or discarded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

/// `vm_screenshot` metadata (the image is in the image content).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScreenshotOut {
    /// VM.
    pub vm_id: VmId,
    /// Run.
    pub run_id: RunId,
    /// Screenshot operation.
    pub operation_id: OperationId,
    /// Capture instant.
    pub captured_at: UtcTimestamp,
    /// Display size: pointer coordinates are in these pixels.
    pub display: DisplayGeometry,
    /// Returned image width.
    pub image_width: u32,
    /// Returned image height.
    pub image_height: u32,
    /// Scale of the returned image.
    pub scale: Scale,
    /// Returned PNG size.
    pub image_bytes: u64,
    /// SHA-256 of the returned PNG.
    pub image_sha256: Sha256Digest,
    /// SHA-256 of the full PNG recorded in the session report.
    pub source_sha256: Sha256Digest,
}

/// Mouse and key presses.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InputOut {
    /// VM.
    pub vm_id: VmId,
    /// Run.
    pub run_id: RunId,
    /// Input operations sent.
    pub operations: u32,
    /// Input events sent.
    pub events: u32,
}

/// `vm_keyboard_type`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TypeOut {
    /// VM.
    pub vm_id: VmId,
    /// Run.
    pub run_id: RunId,
    /// Characters typed.
    pub chars: u32,
    /// Layout used.
    pub layout: String,
    /// SHA-256 of the text (the text itself is never recorded).
    pub text_sha256: Sha256Digest,
}

/// `vm_snapshot`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotOut {
    /// VM.
    pub vm_id: VmId,
    /// New baseline (restorable by this session).
    pub baseline_id: lightvm_proto::BaselineId,
    /// Label (prefixed with `agent: `).
    pub label: String,
    /// Creation instant.
    pub created_at: UtcTimestamp,
    /// Created after an observed clean shutdown.
    pub clean_shutdown: bool,
}

/// `vm_restore`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RestoreOut {
    /// VM.
    pub vm_id: VmId,
    /// Restored baseline.
    pub baseline_id: lightvm_proto::BaselineId,
    /// What changed (plain text).
    pub message: String,
}

/// `vm_baselines`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BaselinesOut {
    /// VM.
    pub vm_id: VmId,
    /// Baselines, oldest first.
    pub baselines: Vec<AgentBaselineView>,
}

/// `vm_logs`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LogsOut {
    /// VM.
    pub vm_id: VmId,
    /// LightVM events of the session.
    pub entries: Vec<LogEntry>,
    /// Pass as `after_seq` to read only newer entries.
    pub last_seq: u64,
    /// QEMU diagnostics of the current run (host paths removed).
    pub qemu: Vec<LogEntry>,
}

/// `report_get`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReportOut {
    /// Summary of the session report.
    pub report: SessionReportSummary,
}

/// `session_declare`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DeclareOut {
    /// Recorded in the report.
    pub recorded: bool,
    /// Event sequence in the report.
    pub seq: u64,
    /// Reminder (plain text).
    pub note: String,
}

/// `session_end`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EndOut {
    /// Session.
    pub session_id: SessionId,
    /// Queued operations discarded.
    pub discarded: u32,
    /// Operations already forwarded (they may still have effects).
    pub already_forwarded: u32,
}

// ----- definitions ------------------------------------------------------------

/// A tool and its output schema.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// MCP definition.
    pub tool: Tool,
    /// Output schema (validated before returning).
    pub output: Value,
}

fn def<P: JsonSchema, O: JsonSchema>(
    name: &'static str,
    catalogue: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
) -> ToolDef {
    let output = schema_for::<O>();
    let mut ann = ToolAnnotations::new()
        .read_only(read_only)
        .destructive(destructive);
    ann.open_world_hint = Some(false);
    ann.title = Some(format!("LightVM {catalogue}"));
    let tool = Tool::new(name, description, schema_for::<P>())
        .with_title(format!("LightVM {catalogue}"))
        .with_raw_output_schema(Arc::new(output.clone()))
        .with_annotations(ann);
    ToolDef {
        tool,
        output: Value::Object(output),
    }
}

/// The catalogue.
#[must_use]
pub fn catalogue() -> Vec<ToolDef> {
    vec![
        def::<SessionRequestParams, SessionOut>(
            "session_request",
            "session.request",
            "Ask the LightVM user for a session. Nothing is accessible before the user approves it in the LightVM app: show the returned verification code to the user, who compares it with the one in the app, then call session_status. The purpose is shown to the user as the session objective.",
            false,
            false,
        ),
        def::<SessionStatusParams, SessionOut>(
            "session_status",
            "session.status",
            "State of the session request (waits up to wait_s seconds while pending). When active, lists the VM and the capabilities the user granted.",
            true,
            false,
        ),
        def::<NoParams, EndOut>(
            "session_end",
            "session.end",
            "End the session (the VM is not stopped). A new session needs a new approval.",
            false,
            false,
        ),
        def::<DeclareParams, DeclareOut>(
            "session_declare",
            "session.declare",
            "Declare the outcome you believe the test had, with a short summary. It is recorded as your claim in the session report; the report outcome is decided by a verifier or by the user, never by this declaration.",
            false,
            false,
        ),
        def::<NoParams, VmListOut>(
            "vm_list",
            "vm.list",
            "VMs of the session (filtered by the grant).",
            true,
            false,
        ),
        def::<VmParams, VmOut>(
            "vm_status",
            "vm.status",
            "State, current run, display size and input ownership of a VM.",
            true,
            false,
        ),
        def::<VmParams, OperationOut>(
            "vm_start",
            "vm.start",
            "Start the VM (needs vm.start). Returns when QEMU is running or the start failed; the guest then boots (wait with vm_screenshot).",
            false,
            false,
        ),
        def::<VmStopParams, OperationOut>(
            "vm_stop",
            "vm.stop",
            "Stop the VM (needs vm.stop): ordinary ACPI shutdown request by default (poll vm_status until stopped), or force=true to terminate the process (unclean).",
            false,
            true,
        ),
        def::<VmParams, OperationOut>(
            "vm_reset",
            "vm.reset",
            "Hardware reset of the running VM (needs vm.reset; unsaved guest state is lost).",
            false,
            true,
        ),
        def::<ScreenshotParams, ScreenshotOut>(
            "vm_screenshot",
            "vm.screenshot",
            "Screenshot of the running VM as a PNG image (needs vm.screenshot), with VM, run and display size. scale=half returns a half-size image; pointer coordinates are always display pixels.",
            true,
            false,
        ),
        def::<MouseMoveParams, InputOut>(
            "vm_mouse_move",
            "vm.mouse.move",
            "Move the pointer to display pixel (x, y) (needs vm.input and input control handed to the session by the user).",
            false,
            false,
        ),
        def::<MouseClickParams, InputOut>(
            "vm_mouse_click",
            "vm.mouse.click",
            "Click (optionally at display pixel x, y first) with the left, middle or right button; double=true for a double click (needs vm.input and input control).",
            false,
            false,
        ),
        def::<TypeParams, TypeOut>(
            "vm_keyboard_type",
            "vm.keyboard.type",
            "Type text in the VM with the US keyboard layout (the guest must use the US layout): printable ASCII, \\n for Enter and \\t for Tab, up to 1024 characters per call; other characters are refused before anything is typed. The text is not recorded (only its length and SHA-256). Needs vm.input and input control.",
            false,
            false,
        ),
        def::<PressParams, InputOut>(
            "vm_keyboard_press",
            "vm.keyboard.press",
            "Press a key or chord, e.g. ret, esc, tab, backspace, up, f5, ctrl+alt+t, ctrl+c, alt+f4 (QEMU key names; repeat up to 20). Needs vm.input and input control.",
            false,
            false,
        ),
        def::<SnapshotParams, SnapshotOut>(
            "vm_snapshot",
            "vm.snapshot",
            "Checkpoint of the stopped VM's disk and UEFI variables as a new baseline (needs vm.snapshot). Refused while the VM is running: stop it first (no hidden shutdown). Never replaces an existing baseline. Long operation.",
            false,
            false,
        ),
        def::<RestoreParams, RestoreOut>(
            "vm_restore",
            "vm.restore",
            "Restore the stopped VM to the session's initial baseline or to a checkpoint created by this session (needs vm.restore): the VM's current disk state is discarded. Refused while the VM is running. Long operation.",
            false,
            true,
        ),
        def::<VmParams, BaselinesOut>(
            "vm_baselines",
            "vm.baselines",
            "Baselines of the VM with the ones this session may restore (initial baseline and own checkpoints).",
            true,
            false,
        ),
        def::<LogsParams, LogsOut>(
            "vm_logs",
            "vm.logs",
            "LightVM events of this session (calls, VM states, screenshots) and the QEMU diagnostics of the current run, host paths removed (needs vm.logs). Not the guest's own logs.",
            true,
            false,
        ),
        def::<OperationParams, OperationOut>(
            "operation_get",
            "operation.get",
            "Status of an operation of this session by identifier (needs vm.status).",
            true,
            false,
        ),
        def::<ReportParams, ReportOut>(
            "report_get",
            "report.get",
            "Summary of this session's report (events, screenshots, current outcome and its source).",
            true,
            false,
        ),
    ]
}
