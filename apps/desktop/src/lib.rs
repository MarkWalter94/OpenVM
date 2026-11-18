//! `lightvm-desktop`: native desktop interface of LightVM (egui/eframe).
//!
//! F2 (alpha manuale): Dashboard with the catalog VMs and their actions,
//! "New VM" wizard, console of running VMs through the supervisor's per-run
//! display ticket (ADR-0013) with keyboard and mouse sent as IPC input
//! batches, dialogs for deletion and for closing with running VMs, Settings
//! with QEMU paths, host capacity and the security facts of this version.
//!
//! F3: Baselines page of a VM (baselines with the VM off: creation with the
//! explicit consent after an unclean stop, restore, deletion), with the
//! long operations shown as progress and cancelled only by the Emergency
//! Stop.
//!
//! F4: Agents page (agent access switch, pairing of a client, session
//! requests with their verification code, approval of one VM with
//! capabilities, duration and limits, sessions with pause, revocation and
//! input control, session page) and the agent session indicator on every
//! page, full screen console included, with the Emergency Stop.
//!
//! F5: the Agents page reads as the AI Workspace (session running now with
//! what the agent did so far, then requests, access and the session
//! history from the supervisor's reports); the session page shows the
//! report (outcome with its source, the agent's declaration as a claim,
//! limits, versions, actions by page, screenshots on request) with the
//! user's verdict, the automatic check, export to a file chosen in the
//! native save dialog and deletion, each confirmed.
//!
//! The simulated framebuffer is a pattern generated in memory: it is not a
//! real console, it is shown only on request (`--simulated-console`, or
//! `--smoke`) and is never used in the product path (F2 exit criterion: no
//! screen with simulated data). The switches and
//! [`LightVmApp::set_simulated_console`] exist only with the `dev-tools`
//! feature (tests and measurements), never in the distributed binary.
//!
//! The UI is separate from the eframe loop (`main.rs`) so that it can be
//! driven headless with `egui_kittest` through the AccessKit tree, with a
//! [`service::Supervisor`] built by the test instead of the child process.

use std::time::{Duration, Instant};

pub mod agents;
pub mod baselines;
pub mod catalog_move;
pub mod clipboard;
pub mod clipboard_images;
pub mod clipboard_sync;
pub mod console;
pub mod dashboard;
pub mod diag;
pub mod dialogs;
pub mod file_drag;
pub mod keymap;
pub mod libraries;
pub mod media_library;
pub mod pairing;
pub mod render;
pub mod reports;
pub mod service;
pub mod settings;
pub mod snapshots;
pub mod theme;
pub mod transfers;
pub mod vm_settings;
pub mod vmware_import;
pub mod wizard;

use console::{ConsoleAction, LiveConsole};
use dashboard::{Confirm, DashboardAction};
use lightvm_proto::ipc::InputOwner;
use lightvm_proto::{
    BaselineId, Label, MediaKind, RunId, SessionId, SnapshotId, UtcTimestamp, VmId, VmState,
};
use render::RenderingInfo;
use service::{
    BaselineOp, RequestStatus, Service, ServiceCommand, ServiceConfig, ServiceState, SnapshotOp,
    Supervisor,
};
use settings::{SupervisorAction, SupervisorUiState, UiSettings};

pub use dashboard::{SupervisorQemu, dashboard_ui, dashboard_with_qemu_ui};

/// Body of `LightVmApp::state`, on the fields only (callable while
/// another field of the app is borrowed).
fn cached_state(
    service: &Option<Box<dyn Supervisor>>,
    cache: &std::cell::RefCell<Option<std::sync::Arc<ServiceState>>>,
) -> Option<std::sync::Arc<ServiceState>> {
    let s = service.as_ref()?;
    Some(
        cache
            .borrow_mut()
            .get_or_insert_with(|| std::sync::Arc::new(s.state()))
            .clone(),
    )
}

/// Folder picker: receives the title, returns the chosen folder.
pub type FolderPicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;
/// ISO picker: receives the title, returns the chosen file.
pub type FilePicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;
/// Save dialog: receives the proposed file name, returns the destination.
pub type SavePicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;

/// Save dialog for a session report: receives the proposed file name and
/// the format, returns the destination chosen by the user.
pub type ReportPicker =
    Box<dyn FnMut(&str, lightvm_proto::report::ReportFormat) -> Option<std::path::PathBuf>>;

/// Native save dialog for session reports (Windows and macOS).
pub fn native_report_picker() -> Option<ReportPicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(
            |name: &str, format: lightvm_proto::report::ReportFormat| {
                let (what, ext) = match format {
                    lightvm_proto::report::ReportFormat::Json => ("JSON report", "json"),
                    lightvm_proto::report::ReportFormat::Markdown => ("Markdown report", "md"),
                };
                rfd::FileDialog::new()
                    .set_title("Export the session report")
                    .set_file_name(name)
                    .add_filter(what, &[ext])
                    .save_file()
            },
        ))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Native folder picker (Windows and macOS; elsewhere no picker).
pub fn native_folder_picker() -> Option<FolderPicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(|title: &str| {
            rfd::FileDialog::new().set_title(title).pick_folder()
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Native ISO picker (Windows and macOS).
pub fn native_file_picker() -> Option<FilePicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(|title: &str| {
            rfd::FileDialog::new()
                .set_title(title)
                .add_filter("ISO image", &["iso"])
                .pick_file()
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Native picker of the `.vmx` file of a VMware VM (Windows and macOS).
pub fn native_vmx_picker() -> Option<FilePicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(|title: &str| {
            rfd::FileDialog::new()
                .set_title(title)
                .add_filter("VMware VM configuration", &["vmx"])
                .pick_file()
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Native save dialog for PNG screenshots (Windows and macOS).
pub fn native_save_picker() -> Option<SavePicker> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(Box::new(|name: &str| {
            rfd::FileDialog::new()
                .set_title("Save the screenshot")
                .set_file_name(name)
                .add_filter("PNG image", &["png"])
                .save_file()
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Width of the simulated framebuffer, in texels (reference ROADMAP 13: 1280×720).
pub const FB_WIDTH: usize = 1280;
/// Height of the simulated framebuffer, in texels.
pub const FB_HEIGHT: usize = 720;
/// Minimum interval between two framebuffer updates: at most 10 per second.
pub const FB_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Tolerated early wake-up on top of the one introduced by egui.
const FB_WAKE_TOLERANCE: Duration = Duration::from_millis(3);

/// Minimum wait between two console ticket requests for the same VM.
const TICKET_RETRY: Duration = Duration::from_secs(3);
/// Wait after a display disconnection before reconnecting.
const RECONNECT_AFTER: Duration = Duration::from_secs(2);
/// Maximum wait for the orderly stop of the VMs when closing the app; then
/// the supervisor shutdown terminates the remaining ones.
pub const CLOSE_STOP_TIMEOUT: Duration = Duration::from_secs(90);

/// Re-read interval of the VM list while a VM is starting.
pub const START_POLL: Duration = Duration::from_millis(500);

/// Side navigation items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Overview of the VMs.
    Dashboard,
    /// Console of the guest.
    Console,
    /// Agent access, requests and sessions (F4).
    Agents,
    /// Settings.
    Settings,
}

impl Section {
    /// All items, in side panel order.
    pub const ALL: [Self; 4] = [Self::Dashboard, Self::Console, Self::Agents, Self::Settings];

    /// Visible and accessible label (name of the AccessKit node).
    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Console => "Console",
            Self::Agents => "Agents",
            Self::Settings => "Settings",
        }
    }
}

/// Interface theme: dark by default (ROADMAP F2), light selectable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    /// Dark theme (default).
    #[default]
    Dark,
    /// Light theme.
    Light,
}

impl From<UiTheme> for egui::Theme {
    fn from(t: UiTheme) -> Self {
        match t {
            UiTheme::Dark => Self::Dark,
            UiTheme::Light => Self::Light,
        }
    }
}

/// Outcome of a background restart: `None` if it was only a stop.
type RestartOutcome = Option<Result<(Service, ServiceConfig), String>>;

/// Confirmation dialog open.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfirmDialog {
    kind: Confirm,
    name: String,
    revision: u64,
    run_id: Option<RunId>,
    /// The action was sent (deletion: waiting for the outcome).
    sent: bool,
    focus: bool,
}

/// Baseline action to confirm (F3).
#[derive(Clone, Debug, PartialEq, Eq)]
enum BaselineConfirm {
    /// Creation, with the label generated when the dialog opened.
    Create { label: Label },
    /// Restore: the current state is discarded.
    Restore { id: BaselineId, label: String },
    /// Deletion.
    Delete {
        id: BaselineId,
        label: String,
        size: u64,
    },
}

impl BaselineConfirm {
    /// The request recorded by the supervisor state is this one.
    fn matches(&self, op: &BaselineOp) -> bool {
        match (self, op) {
            (Self::Create { label }, BaselineOp::Create { label: l, .. }) => label == l,
            (Self::Restore { id, .. }, BaselineOp::Restore(i))
            | (Self::Delete { id, .. }, BaselineOp::Delete(i)) => id == i,
            _ => false,
        }
    }
}

/// Wait in the dialog for a creation or restore to show up as running on
/// the VM; then the dialog closes and the progress is on the page (the
/// Emergency Stop must stay reachable).
const BASELINE_DIALOG_WAIT: Duration = Duration::from_secs(2);

/// Baseline confirmation dialog open.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BaselineDialog {
    vm_id: VmId,
    name: String,
    kind: BaselineConfirm,
    /// Creation refused for an unclean stop: explicit consent asked.
    unclean: bool,
    /// Consent checkbox.
    consent: bool,
    /// The action was sent, at this instant.
    sent: Option<Instant>,
    focus: bool,
}

/// Snapshot action to confirm (ADR-0032).
#[derive(Clone, Debug, PartialEq, Eq)]
enum SnapshotConfirm {
    /// New snapshot: the name and description the user types.
    Take { name: String, description: String },
    /// Revert: the VM must be off; `vm_running` (at the opening) decides
    /// whether the confirmation offers to shut it down normally first and to
    /// snapshot the current state before it.
    Revert {
        id: SnapshotId,
        label: String,
        vm_running: bool,
        /// Run of the VM at the opening: the shutdown and the revert
        /// concern that run only.
        run_id: Option<RunId>,
        /// "Shut down first" checkbox (VM running only).
        shut_down: bool,
        /// "Take a snapshot of the current state first" checkbox.
        snapshot_current: bool,
    },
    /// Deletion.
    Delete { id: SnapshotId, label: String },
    /// New name and description: the current values, edited in place.
    Rename {
        id: SnapshotId,
        name: String,
        description: String,
    },
}

impl SnapshotConfirm {
    /// The request recorded by the supervisor state is this one.
    fn matches(&self, op: &SnapshotOp) -> bool {
        match (self, op) {
            (Self::Take { .. }, SnapshotOp::Take) => true,
            (Self::Revert { id, .. }, SnapshotOp::Revert(i))
            | (Self::Delete { id, .. }, SnapshotOp::Delete(i))
            | (Self::Rename { id, .. }, SnapshotOp::Rename(i)) => id == i,
            _ => false,
        }
    }
}

/// Wait in the dialog for a take or revert to show up as running on the VM;
/// then the dialog closes and the progress is on the page (the Emergency
/// Stop must stay reachable).
const SNAPSHOT_DIALOG_WAIT: Duration = Duration::from_secs(2);

/// Snapshot confirmation dialog open.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotDialog {
    vm_id: VmId,
    name: String,
    kind: SnapshotConfirm,
    /// The action was sent, at this instant.
    sent: Option<Instant>,
    focus: bool,
}

/// A revert of a running VM in progress (ADR-0032): the offered snapshot of
/// the current state and/or the orderly shutdown must finish before the
/// revert is sent (it needs the VM off).
struct RevertFlow {
    vm_id: VmId,
    snapshot_id: SnapshotId,
    /// Run the user confirmed the revert of: if the VM runs another one
    /// (it was started again) the flow is abandoned.
    run_id: RunId,
    /// What the flow is waiting for.
    waiting: RevertWaiting,
    /// The orderly stop was sent, at this instant ([`REVERT_STOP_TIMEOUT`]).
    stop_sent: Option<Instant>,
}

/// Longest wait for the orderly stop of a running-VM revert: the
/// supervisor's whole shutdown escalation (powerdown 60 s, quit 5 s,
/// termination 10 s) plus a margin. After it the flow is abandoned with a
/// message, so a later unrelated stop never triggers the revert.
const REVERT_STOP_TIMEOUT: Duration = Duration::from_secs(120);

/// What a [`RevertFlow`] is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RevertWaiting {
    /// The offered snapshot of the current state (offered while the VM ran).
    Take,
    /// The orderly shutdown of the VM.
    Stop,
}

/// Revocation of an agent session to confirm.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RevokeDialog {
    session_id: SessionId,
    client: String,
    vm_name: String,
    sent: bool,
    focus: bool,
}

/// Revocation of the unattended access to confirm (ADR-0039).
#[derive(Clone, Debug, PartialEq, Eq)]
struct StandingRevokeDialog {
    grant_id: lightvm_proto::StandingGrantId,
    vm_name: String,
    sent: bool,
    focus: bool,
}

/// Notice shown once per app run when the supervisor reports a live
/// unattended access at start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StandingNotice {
    /// The unattended access was not read yet.
    #[default]
    Unchecked,
    /// Shown, waiting for the user's acknowledgement.
    Open,
    /// Acknowledged, or nothing to show.
    Done,
}

/// Action on a session report to confirm (F5).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ReportDialogKind {
    /// Verdict of the user: the outcome chosen (none at first).
    Verdict(Option<lightvm_proto::TestOutcome>),
    /// Automatic check: the marker expected (none at first).
    Verify(Option<lightvm_proto::report::FixtureMarker>),
    /// Export: format, screenshots, file chosen, overwrite sent.
    Export {
        format: lightvm_proto::report::ReportFormat,
        screenshots: bool,
        path: Option<std::path::PathBuf>,
        overwrite: bool,
    },
    /// Deletion of the report and its screenshots.
    Delete,
}

/// Report dialog open.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReportDialog {
    session_id: SessionId,
    client: String,
    started: UtcTimestamp,
    kind: ReportDialogKind,
    sent: bool,
    focus: bool,
    /// Problem found by the app itself (no save dialog).
    note: Option<String>,
}

impl ReportDialog {
    fn op(&self) -> service::ReportOp {
        use service::ReportOp as O;
        match self.kind {
            ReportDialogKind::Verdict(_) => O::Verdict(self.session_id),
            ReportDialogKind::Verify(_) => O::Verify(self.session_id),
            ReportDialogKind::Export { .. } => O::Export(self.session_id),
            ReportDialogKind::Delete => O::Delete(self.session_id),
        }
    }
}

/// Re-read of the reports of live sessions (the supervisor publishes no
/// event for each agent action).
const REPORT_POLL: Duration = Duration::from_secs(3);

/// Proposed name of an exported report:
/// `lightvm-session-<client>-<yyyymmdd-hhmm>.json`.
pub fn report_file_name(
    client: &str,
    started: UtcTimestamp,
    format: lightvm_proto::report::ReportFormat,
) -> String {
    let stamp: String = started
        .to_string()
        .chars()
        .take(16)
        .filter(char::is_ascii_digit)
        .collect();
    let (date, time) = stamp.split_at(stamp.len().min(8));
    let client: String = sanitize_file_name(client).chars().take(40).collect();
    let ext = match format {
        lightvm_proto::report::ReportFormat::Json => "json",
        lightvm_proto::report::ReportFormat::Markdown => "md",
    };
    format!("lightvm-session-{client}-{date}-{time}.{ext}")
}

/// "Pair a client" dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PairingDialog {
    sent: bool,
    focus: bool,
}

/// Close of the app with running VMs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Closing {
    Ask {
        focus: bool,
    },
    /// Orderly stop of the VMs that were active when the user confirmed
    /// (only those are waited for).
    Stopping {
        started: Instant,
        vms: Vec<VmId>,
    },
    Finishing,
}

/// Wait for the OS to apply a full screen change before the window state
/// reported by the OS wins again.
const FULLSCREEN_SETTLE: Duration = Duration::from_secs(1);

/// Console ticket requested by the console page.
#[derive(Clone, Copy, Debug)]
struct TicketRequest {
    vm_id: VmId,
    at: Instant,
}

/// UI state.
pub struct LightVmApp {
    section: Section,
    theme: UiTheme,
    applied_theme: Option<UiTheme>,
    animate: bool,
    /// The Console page shows the simulated framebuffer when no VM console
    /// is open (off by default: no simulated data in the product path).
    simulated_console: bool,
    texture: Option<egui::TextureHandle>,
    pixels: Vec<egui::Color32>,
    phase: u32,
    last_fb_update: Option<Instant>,
    fb_updates: u64,
    frames: u64,
    tools_clipboard_pending: Option<(VmId, RunId)>,
    clipboard_sync: clipboard_sync::ClipboardSync,
    transfers: transfers::Transfers,
    live: Option<LiveConsole>,
    /// VM whose console the Console page shows.
    console_vm: Option<VmId>,
    ticket_request: Option<TicketRequest>,
    console_max_fps: u32,
    fullscreen: bool,
    /// Full screen change sent to the OS and not yet reported back by it.
    fullscreen_request: Option<(bool, Instant)>,
    chord_swallow: Vec<egui::Key>,
    screenshot_status: Option<(String, theme::Tone)>,
    service: Option<Box<dyn Supervisor>>,
    service_error: Option<String>,
    settings: UiSettings,
    settings_path: Option<std::path::PathBuf>,
    settings_error: Option<String>,
    service_exe: Option<std::path::PathBuf>,
    /// Catalog and QEMU folder the supervisor was started with.
    applied_service: Option<(std::path::PathBuf, Option<std::path::PathBuf>)>,
    folder_picker: Option<FolderPicker>,
    file_picker: Option<FilePicker>,
    /// Picker of the `.vmx` of a VMware VM (ADR-0030).
    vmx_picker: Option<FilePicker>,
    /// "Import VMware VM" page, from the choice of the `.vmx` to the outcome.
    vmware_import: Option<vmware_import::ImportFlow>,
    save_picker: Option<SavePicker>,
    report_picker: Option<ReportPicker>,
    /// Supervisor configuration given on the command line (`--catalog`):
    /// takes precedence over the Settings page, restarts included.
    cli_config: Option<ServiceConfig>,
    /// Restart or stop of the supervisor in progress outside the UI thread.
    restart: Option<std::sync::mpsc::Receiver<RestartOutcome>>,
    /// Move of the catalog (Settings), from confirmation to outcome.
    catalog_move: Option<catalog_move::MoveFlow>,
    /// Runner of the catalog move (the supervisor executable; fake in tests).
    move_runner: Option<catalog_move::MoveRunner>,
    vm_edit: Option<vm_settings::VmEdit>,
    wizard: Option<wizard::Wizard>,
    /// Systems the New VM wizard offers (the host's, fake in tests).
    guest_support: wizard::GuestSupport,
    dialog: Option<ConfirmDialog>,
    /// VM whose Baselines page is open (F3).
    baselines_vm: Option<VmId>,
    baseline_dialog: Option<BaselineDialog>,
    /// Deletion of an imported medium to confirm (Settings).
    media_dialog: Option<media_library::DeleteDialog>,
    /// Creation or restore sent by this UI: VM and instant (elapsed time).
    baseline_sent: Option<(VmId, Instant)>,
    /// VM whose Snapshots page is open (ADR-0032).
    snapshots_vm: Option<VmId>,
    snapshot_dialog: Option<SnapshotDialog>,
    /// Take or revert sent by this UI: VM and instant (elapsed time).
    snapshot_sent: Option<(VmId, Instant)>,
    /// A revert of a running VM in progress: snapshot of the current state
    /// and/or orderly shutdown before the revert itself.
    snapshot_revert_flow: Option<RevertFlow>,
    /// Agent session whose page is open (F4).
    agent_session: Option<SessionId>,
    /// Approval form of a session request.
    approve_form: Option<agents::ApproveForm>,
    revoke_dialog: Option<RevokeDialog>,
    pairing_dialog: Option<PairingDialog>,
    /// Creation form of the unattended access (ADR-0039).
    standing_form: Option<agents::StandingForm>,
    standing_revoke: Option<StandingRevokeDialog>,
    standing_notice: StandingNotice,
    /// Report screenshot shown on the session page (F5).
    shown_shot: Option<(SessionId, u32)>,
    /// Live session whose latest screenshot the AI Workspace shows.
    watch_screen: Option<SessionId>,
    /// Textures of the report screenshots shown (at most
    /// [`service::REPORT_SHOTS_KEPT`]).
    shot_textures: Vec<((SessionId, u32), egui::TextureHandle)>,
    report_dialog: Option<ReportDialog>,
    /// Last re-read of the live sessions' reports.
    report_poll: Option<Instant>,
    /// Fixed clock for the generated baseline labels (tests).
    fixed_now: Option<UtcTimestamp>,
    /// Last re-read of the VM list for the progress of a start.
    start_poll: Option<Instant>,
    closing: Option<Closing>,
    allow_close: bool,
    keyboard_nav: bool,
    discarded_pastes: u64,
    /// Effective rendering backend, set by the eframe loop (F1-16).
    pub rendering: RenderingInfo,
    /// Copy of the supervisor state taken once per frame (and again after
    /// a command, which changes it synchronously): the views share it
    /// instead of each copying the whole state.
    state_cache: std::cell::RefCell<Option<std::sync::Arc<ServiceState>>>,
    /// Why the last revert of a running VM was abandoned (VM, text).
    snapshot_revert_notice: Option<(VmId, String)>,
}

impl Default for LightVmApp {
    fn default() -> Self {
        Self::new(true)
    }
}

impl LightVmApp {
    /// Creates the UI. With `animate == false` the simulated framebuffer stays
    /// static and no periodic repaints are requested (useful to measure the
    /// real idle).
    pub fn new(animate: bool) -> Self {
        Self {
            section: Section::Dashboard,
            theme: UiTheme::Dark,
            applied_theme: None,
            animate,
            simulated_console: false,
            texture: None,
            pixels: vec![egui::Color32::BLACK; FB_WIDTH * FB_HEIGHT],
            phase: 0,
            last_fb_update: None,
            fb_updates: 0,
            frames: 0,
            tools_clipboard_pending: None,
            clipboard_sync: clipboard_sync::ClipboardSync::default(),
            transfers: transfers::Transfers::default(),
            live: None,
            console_vm: None,
            ticket_request: None,
            console_max_fps: console::DEFAULT_MAX_FPS,
            fullscreen: false,
            fullscreen_request: None,
            chord_swallow: Vec::new(),
            screenshot_status: None,
            service: None,
            service_error: None,
            settings: UiSettings::default(),
            settings_path: None,
            settings_error: None,
            service_exe: None,
            applied_service: None,
            folder_picker: None,
            file_picker: None,
            vmx_picker: None,
            vmware_import: None,
            save_picker: None,
            report_picker: None,
            cli_config: None,
            restart: None,
            catalog_move: None,
            move_runner: None,
            vm_edit: None,
            guest_support: lightvm_proto::GuestOs::runs_on_this_host_with,
            wizard: None,
            dialog: None,
            baselines_vm: None,
            baseline_dialog: None,
            media_dialog: None,
            baseline_sent: None,
            snapshots_vm: None,
            snapshot_dialog: None,
            snapshot_sent: None,
            snapshot_revert_flow: None,
            agent_session: None,
            approve_form: None,
            revoke_dialog: None,
            pairing_dialog: None,
            standing_form: None,
            standing_revoke: None,
            standing_notice: StandingNotice::Unchecked,
            shown_shot: None,
            watch_screen: None,
            shot_textures: Vec::new(),
            report_dialog: None,
            report_poll: None,
            fixed_now: None,
            start_poll: None,
            closing: None,
            allow_close: false,
            keyboard_nav: false,
            discarded_pastes: 0,
            rendering: RenderingInfo::default(),
            state_cache: std::cell::RefCell::new(None),
            snapshot_revert_notice: None,
        }
    }

    /// Creates the UI with an already built console (for example
    /// [`LiveConsole::synthetic`] in tests), shown on the Console page.
    pub fn with_live_console(console: LiveConsole) -> Self {
        let mut app = Self::new(false);
        app.console_vm = Some(console.target().vm_id);
        app.live = Some(console);
        app
    }

    /// Enables the simulated framebuffer on the Console page when no VM
    /// console is open (`--simulated-console`; measurements and tests).
    /// Only with the `dev-tools` feature: without it the simulated
    /// framebuffer cannot be turned on.
    #[cfg(feature = "dev-tools")]
    pub fn set_simulated_console(&mut self, on: bool) {
        self.simulated_console = on;
    }

    /// The simulated framebuffer is enabled.
    pub fn simulated_console(&self) -> bool {
        self.simulated_console
    }

    /// Maximum console updates per second (reduced in degraded rendering).
    pub fn set_console_max_fps(&mut self, fps: u32) {
        self.console_max_fps = fps.max(1);
    }

    /// Connects the supervisor started by the UI (F1-11) with the
    /// configuration used.
    pub fn set_service(&mut self, service: Service, cfg: &ServiceConfig) {
        self.set_supervisor_with_config(Box::new(service), cfg);
    }

    /// Connects a supervisor started with `cfg` (the child process, or a
    /// stand-in in tests): the app knows its catalog and QEMU folder.
    pub fn set_supervisor_with_config(&mut self, s: Box<dyn Supervisor>, cfg: &ServiceConfig) {
        self.set_supervisor(s);
        self.applied_service = Some((cfg.catalog.clone(), cfg.qemu_dir.clone()));
    }

    /// Connects a supervisor whose configuration the app does not know: the
    /// child process, or a stand-in in tests.
    pub fn set_supervisor(&mut self, s: Box<dyn Supervisor>) {
        self.invalidate_state();
        self.service = Some(s);
        self.service_error = None;
        self.section = Section::Dashboard;
        self.applied_service = None;
    }

    /// QEMU of the running supervisor, as far as the app knows it: missing
    /// only when the app started it with no QEMU folder.
    pub fn supervisor_qemu(&self) -> SupervisorQemu {
        match &self.applied_service {
            Some((_, None)) if self.service.is_some() => {
                if self.cli_config.is_some() {
                    SupervisorQemu::MissingFromCommandLine
                } else {
                    SupervisorQemu::MissingFromSettings
                }
            }
            _ => SupervisorQemu::Given,
        }
    }

    /// Supervisor executable (`--service-exe`); without it, the one next to
    /// the UI executable.
    pub fn set_service_exe(&mut self, p: Option<std::path::PathBuf>) {
        self.service_exe = p;
    }

    /// Folder picker of the Settings page (native in the app, fake in tests).
    pub fn set_folder_picker(&mut self, s: Option<FolderPicker>) {
        self.folder_picker = s;
    }

    /// Systems the New VM wizard offers: by default those this host runs
    /// ([`lightvm_proto::GuestOs::runs_on_this_host_with`]); tests pass another
    /// rule to exercise the other host family.
    pub fn set_guest_support(&mut self, s: wizard::GuestSupport) {
        self.guest_support = s;
    }

    /// ISO picker of the wizard (native in the app, fake in tests).
    pub fn set_file_picker(&mut self, s: Option<FilePicker>) {
        self.file_picker = s;
    }

    /// Picker of the `.vmx` of "Import VMware VM" (native in the app, fake
    /// in tests).
    pub fn set_vmx_picker(&mut self, s: Option<FilePicker>) {
        self.vmx_picker = s;
    }

    /// The "Import VMware VM" page, if open.
    pub fn vmware_import(&self) -> Option<&vmware_import::ImportFlow> {
        self.vmware_import.as_ref()
    }

    /// Save dialog of the console screenshot (native in the app, fake in tests).
    pub fn set_save_picker(&mut self, s: Option<SavePicker>) {
        self.save_picker = s;
    }

    /// Save dialog of the report export (native in the app, fake in tests).
    pub fn set_report_picker(&mut self, s: Option<ReportPicker>) {
        self.report_picker = s;
    }

    /// Runner of the catalog move: by default `lightvm-service
    /// --move-catalog`; tests pass a fake.
    pub fn set_move_runner(&mut self, r: Option<catalog_move::MoveRunner>) {
        self.move_runner = r;
    }

    /// Supervisor configuration given on the command line: it stays in use
    /// even when the supervisor is restarted from Settings.
    pub fn set_command_line_config(&mut self, cfg: ServiceConfig) {
        self.cli_config = Some(cfg);
    }

    /// Supervisor configuration: the command-line one if present, otherwise
    /// from the current settings.
    pub fn service_config(&self) -> Result<ServiceConfig, String> {
        if let Some(c) = &self.cli_config {
            return Ok(ServiceConfig {
                policy: self.settings.ui_loss_policy,
                ..c.clone()
            });
        }
        let executable = self
            .service_exe
            .clone()
            .or_else(ServiceConfig::executable_next_to_ui)
            .ok_or("supervisor executable not found")?;
        let catalog = self
            .settings
            .effective_catalog()
            .ok_or("catalog folder cannot be determined: choose it in Settings")?;
        Ok(ServiceConfig {
            executable,
            catalog,
            qemu_dir: self.settings.effective_qemu_dir(),
            runtime_dir: None,
            audit_log: None,
            policy: self.settings.ui_loss_policy,
            pairing_dir: None,
        })
    }

    /// (Re)starts the supervisor with the current settings; a supervisor
    /// already running is first stopped in a controlled way.
    pub fn start_service(&mut self, ctx: &egui::Context) {
        let _ = self.stop_service();
        let cfg = match self.service_config() {
            Ok(c) => c,
            Err(e) => {
                self.service_error = Some(e);
                return;
            }
        };
        let ctx = ctx.clone();
        match Service::start(&cfg, std::sync::Arc::new(move || ctx.request_repaint())) {
            Ok(s) => {
                let section = self.section;
                self.set_service(s, &cfg);
                self.section = section;
            }
            Err(e) => {
                diag::log(&format!("service: {e}"));
                self.service_error = Some(e.to_string());
            }
        }
    }

    /// Restart (or stop only) of the supervisor, on a separate thread: the
    /// orderly stop of the VMs can take tens of seconds and the window must
    /// stay responsive. The outcome arrives in [`Self::ui`].
    fn restart_in_background(&mut self, ctx: &egui::Context, restart: bool) {
        if self.restart.is_some() {
            return;
        }
        let cfg = if restart {
            match self.service_config() {
                Ok(c) => Some(c),
                Err(e) => {
                    self.service_error = Some(e);
                    return;
                }
            }
        } else {
            None
        };
        self.close_console();
        let old = self.service.take();
        self.invalidate_state();
        self.vm_edit = None;
        self.wizard = None;
        self.dialog = None;
        self.baselines_vm = None;
        self.baseline_dialog = None;
        self.snapshots_vm = None;
        self.snapshot_dialog = None;
        self.snapshot_revert_flow = None;
        self.media_dialog = None;
        self.agent_session = None;
        self.approve_form = None;
        self.revoke_dialog = None;
        self.pairing_dialog = None;
        self.standing_form = None;
        self.standing_revoke = None;
        self.service_error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("lightvm-service-restart".into())
            .spawn(move || {
                if let Some(mut s) = old {
                    let _ = s.stop(service::STOP_TIMEOUT);
                }
                let outcome = cfg.map(|cfg| {
                    let c = ctx.clone();
                    Service::start(&cfg, std::sync::Arc::new(move || c.request_repaint()))
                        .map(|s| (s, cfg))
                        .map_err(|e| e.to_string())
                });
                let _ = tx.send(outcome);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.restart = Some(rx),
            Err(e) => self.service_error = Some(format!("restart not started: {e}")),
        }
    }

    /// Collects the outcome of a background restart, if it has arrived.
    fn collect_restart(&mut self) {
        let Some(rx) = &self.restart else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(e) => e,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("supervisor restart interrupted".into()))
            }
        };
        self.restart = None;
        match outcome {
            Some(Ok((s, cfg))) => {
                let section = self.section;
                self.set_service(s, &cfg);
                self.section = section;
            }
            Some(Err(e)) => {
                diag::log(&format!("service: {e}"));
                self.service_error = Some(e);
            }
            None => {}
        }
    }

    /// Restart or stop of the supervisor in progress.
    pub fn restart_in_progress(&self) -> bool {
        self.restart.is_some()
    }

    fn supervisor_ui_state(&mut self) -> SupervisorUiState {
        let mut s = self.supervisor_ui_state_base();
        s.move_catalog = self.move_availability(s.running_vms).map(|_| ());
        s
    }

    fn supervisor_ui_state_base(&mut self) -> SupervisorUiState {
        let current = match &self.cli_config {
            Some(c) => Some((c.catalog.clone(), c.qemu_dir.clone())),
            None => self
                .settings
                .effective_catalog()
                .map(|c| (c, self.settings.effective_qemu_dir())),
        };
        let folder_picker = self.folder_picker.is_some();
        let command_line = self
            .cli_config
            .as_ref()
            .map(|c| (c.catalog.clone(), c.qemu_dir.clone()));
        if self.restart.is_some() {
            return SupervisorUiState {
                description: "restart or stop in progress…".into(),
                tone: theme::Tone::Warning,
                running: false,
                in_progress: true,
                running_vms: 0,
                pending_changes: false,
                folder_picker,
                command_line,
                move_catalog: Ok(()),
            };
        }
        match &mut self.service {
            Some(s) => {
                let alive = s.is_alive();
                let pid = s.pid();
                let st = self.state().unwrap_or_default();
                let (text, tone) = connection_state(alive, &st);
                SupervisorUiState {
                    description: format!("{text} (PID {pid})"),
                    tone,
                    running: alive,
                    in_progress: false,
                    running_vms: st.active_vms().count(),
                    pending_changes: current != self.applied_service,
                    folder_picker,
                    command_line,
                    move_catalog: Ok(()),
                }
            }
            None => SupervisorUiState {
                description: self
                    .service_error
                    .as_ref()
                    .map_or_else(|| "not started".into(), |e| format!("not started: {e}")),
                tone: if self.service_error.is_some() {
                    theme::Tone::Danger
                } else {
                    theme::Tone::Neutral
                },
                running: false,
                in_progress: false,
                running_vms: 0,
                pending_changes: false,
                folder_picker,
                command_line,
                move_catalog: Ok(()),
            },
        }
    }

    fn run_supervisor_action(&mut self, a: SupervisorAction, ctx: &egui::Context) {
        let mut choose = |title: &str| self.folder_picker.as_mut().and_then(|f| f(title));
        match a {
            SupervisorAction::ChooseCatalog => {
                if let Some(p) = choose("VM catalog folder") {
                    self.settings.catalog = Some(p);
                    self.save_settings();
                }
            }
            SupervisorAction::ChooseQemu => {
                if let Some(p) = choose("QEMU distribution folder") {
                    self.settings.qemu_dir = Some(p);
                    self.save_settings();
                }
            }
            SupervisorAction::DefaultCatalog => {
                self.settings.catalog = None;
                self.save_settings();
            }
            SupervisorAction::RemoveQemu => {
                self.settings.qemu_dir = None;
                self.save_settings();
            }
            SupervisorAction::MoveCatalog => self.begin_catalog_move(),
            SupervisorAction::Restart => self.restart_in_background(ctx, true),
            SupervisorAction::Stop => self.restart_in_background(ctx, false),
        }
    }

    /// Records a failed supervisor start, shown in the Dashboard.
    pub fn set_service_error(&mut self, error: String) {
        self.service_error = Some(error);
    }

    /// Settings read at startup, file to save them to and any read problem
    /// to show in Settings.
    pub fn set_settings(
        &mut self,
        settings: UiSettings,
        path: Option<std::path::PathBuf>,
        error: Option<String>,
    ) {
        self.theme = settings.theme;
        self.settings = settings;
        self.settings_path = path;
        self.settings_error = error;
    }

    /// Current settings.
    pub fn settings(&self) -> &UiSettings {
        &self.settings
    }

    fn save_settings(&mut self) {
        if let Some(p) = &self.settings_path {
            self.settings_error = self
                .settings
                .save(p)
                .err()
                .map(|e| format!("settings not saved: {e}"));
        }
    }

    /// Applies a new UI loss policy: saves it to file and, if the supervisor
    /// is connected, sends it immediately.
    fn change_policy(&mut self) {
        self.save_settings();
        if let Some(s) = &self.service {
            s.send(ServiceCommand::SetPolicy(self.settings.ui_loss_policy));
        }
        self.invalidate_state();
    }

    /// Opens a VM by name with the given changes (command line).
    /// Requires the supervisor; the result appears in the Dashboard.
    pub fn open_vm(&mut self, name: String, changes: vm_settings::VmChanges, create: bool) {
        self.section = Section::Dashboard;
        match &self.service {
            Some(s) => {
                s.send(ServiceCommand::OpenVm {
                    name,
                    changes,
                    create,
                });
                self.invalidate_state();
            }
            None => {
                // The cause of the failed start, if known, stays visible.
                let cause = self
                    .service_error
                    .take()
                    .unwrap_or_else(|| "supervisor not started".into());
                let e = format!("cannot open VM \"{name}\": {cause}");
                diag::log(&format!("service: {e}"));
                self.service_error = Some(e);
            }
        }
    }

    /// Edit of the settings of a VM open in the panel, if any.
    pub fn vm_edit(&self) -> Option<&vm_settings::VmEdit> {
        self.vm_edit.as_ref()
    }

    /// The "New VM" wizard, if open.
    pub fn wizard(&self) -> Option<&wizard::Wizard> {
        self.wizard.as_ref()
    }

    /// Connected supervisor, if any.
    pub fn service(&self) -> Option<&dyn Supervisor> {
        self.service.as_deref()
    }

    /// Controlled stop of the connected supervisor, if any.
    /// Returns the exit code and whether forcing was necessary.
    pub fn stop_service(&mut self) -> Option<(Option<i32>, bool)> {
        self.vm_edit = None;
        self.close_console();
        let mut s = self.service.take()?;
        self.invalidate_state();
        Some(s.stop(service::STOP_TIMEOUT))
    }

    /// Sets the visible section.
    pub fn set_section(&mut self, section: Section) {
        self.section = section;
    }

    /// `Paste` events (host clipboard) discarded so far (F1-13).
    pub fn discarded_pastes(&self) -> u64 {
        self.discarded_pastes
    }

    /// Records the `Paste` events discarded by `raw_input_hook`.
    pub fn count_discarded_pastes(&mut self, n: u32) {
        self.discarded_pastes += u64::from(n);
    }

    /// Console open on the Console page, if any.
    pub fn live_console(&self) -> Option<&LiveConsole> {
        self.live.as_ref()
    }

    /// Console open on the Console page, mutable (headless tests).
    #[doc(hidden)]
    pub fn live_console_mut(&mut self) -> Option<&mut LiveConsole> {
        self.live.as_mut()
    }

    /// Console input waits to be sent (at most one batch every
    /// [`console::INPUT_INTERVAL`]).
    pub fn console_input_pending(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(LiveConsole::has_pending_input)
    }

    /// The Console page is in full screen.
    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// A confirmation dialog is open.
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
            || self.baseline_dialog.is_some()
            || self.snapshot_dialog.is_some()
            || self.media_dialog.is_some()
            || self.revoke_dialog.is_some()
            || self.standing_revoke.is_some()
            || self.standing_notice == StandingNotice::Open
            || self.pairing_dialog.is_some()
            || self.report_dialog.is_some()
            || self.catalog_move.is_some()
    }

    /// Report screenshot shown on the session page, if any.
    pub fn shown_shot(&self) -> Option<(SessionId, u32)> {
        self.shown_shot
    }

    /// Agent session whose page is open, if any.
    pub fn agent_session(&self) -> Option<SessionId> {
        self.agent_session
    }

    /// Request whose approval form is open, if any.
    pub fn reviewing(&self) -> Option<lightvm_proto::ipc::SessionRequestId> {
        self.approve_form.as_ref().map(|f| f.request_id)
    }

    /// VM whose Baselines page is open, if any.
    pub fn baselines_vm(&self) -> Option<VmId> {
        self.baselines_vm
    }

    /// VM whose Snapshots page is open, if any.
    pub fn snapshots_vm(&self) -> Option<VmId> {
        self.snapshots_vm
    }

    /// Fixes the instant used for the generated baseline labels (tests and
    /// snapshots); `None` uses the system clock.
    #[doc(hidden)]
    pub fn set_fixed_time(&mut self, t: Option<UtcTimestamp>) {
        self.fixed_now = t;
    }

    fn now(&self) -> UtcTimestamp {
        self.fixed_now.unwrap_or_else(UtcTimestamp::now)
    }

    /// The close of the app with running VMs is in progress (dialog shown).
    pub fn closing(&self) -> bool {
        self.closing.is_some()
    }

    /// The app agreed to close (no running VMs, or orderly stop completed).
    pub fn close_allowed(&self) -> bool {
        self.allow_close
    }

    /// Selected item.
    pub fn section(&self) -> Section {
        self.section
    }

    /// Current theme.
    pub fn theme(&self) -> UiTheme {
        self.theme
    }

    /// Sets the theme (applied at the next frame).
    pub fn set_theme(&mut self, theme: UiTheme) {
        self.theme = theme;
    }

    /// Number of UI passes executed (frames drawn by the eframe loop).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Number of updates of the simulated framebuffer.
    pub fn fb_updates(&self) -> u64 {
        self.fb_updates
    }

    /// The supervisor state of this frame (see [`Self::state_cache`]).
    fn state(&self) -> Option<std::sync::Arc<ServiceState>> {
        cached_state(&self.service, &self.state_cache)
    }

    /// The next [`Self::state`] reads the supervisor state again.
    fn invalidate_state(&self) {
        self.state_cache.borrow_mut().take();
    }

    /// Supplies file-transfer dialogs, for integrations and headless tests.
    pub fn set_transfer_pickers(&mut self, pick: Option<FilePicker>, save: Option<SavePicker>) {
        self.transfers.set_pickers(pick, save);
    }

    /// Whether a console file operation is still running, on any page.
    pub fn file_transfer_active(&self) -> bool {
        self.transfers.busy()
    }

    /// The native shell consumes only the prepared, run-checked file selected
    /// by dragging its console row. The guest never supplies a host path.
    pub fn take_file_drag(&mut self) -> Option<std::path::PathBuf> {
        self.transfers.take_drag()
    }

    pub fn file_drag_failed(&mut self, error: String) {
        self.screenshot_status = Some((error, theme::Tone::Warning));
    }

    fn send(&self, c: ServiceCommand) {
        if let Some(s) = &self.service {
            s.send(c);
        }
        // The command changed the state synchronously (pending requests).
        self.invalidate_state();
    }

    /// Draws the whole interface in the root `Ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.frames += 1;
        // A fresh copy of the supervisor state for this frame.
        self.invalidate_state();
        let ctx = ui.ctx().clone();
        // F1-13: before any widget no `Paste`/`Copy`/`Cut` remains in the input.
        let filter = clipboard::filter_context(&ctx);
        self.count_discarded_pastes(filter.discarded);
        if self.applied_theme.is_none() {
            theme::install(&ctx);
        }
        if self.applied_theme != Some(self.theme) {
            ctx.set_theme(egui::Theme::from(self.theme));
            self.applied_theme = Some(self.theme);
            // This frame still uses the previous style: request another one
            // right away with the new one.
            ctx.request_repaint();
        }
        self.track_keyboard_navigation(&ctx);
        self.collect_restart();
        self.check_standing_notice();
        self.poll_starting(&ctx);
        self.sync_fullscreen(&ctx);
        self.handle_close_request(&ctx);
        let image_active = if self.section == Section::Console
            && ctx.input(|i| i.viewport().focused.unwrap_or(false))
            && !self.dialog_open()
            && self.closing.is_none()
        {
            self.live
                .as_ref()
                .map(|live| (live.target().vm_id, live.target().run_id))
        } else {
            None
        };
        if let Some(command) = self
            .transfers
            .poll(self.state().as_deref(), &ctx, image_active)
        {
            self.send(command);
        }
        let state = self.state();
        // The advance of a running-VM revert, whatever the page (the
        // offered snapshot, the orderly stop, then the revert), with a
        // frame at least once a second for its timeout.
        if let Some(st) = &state
            && self.snapshot_revert_flow.is_some()
        {
            self.snapshot_revert_flow_step(st, Instant::now());
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        let target = self
            .live
            .as_ref()
            .map(|live| (live.target().vm_id, live.target().run_id));
        let active = self.section == Section::Console
            && ctx.input(|i| i.viewport().focused.unwrap_or(false))
            && self.live.as_ref().is_some_and(LiveConsole::has_focus)
            && !self.dialog_open()
            && self.closing.is_none()
            && target.is_some_and(|(vm, run)| {
                state.as_ref().is_some_and(|s| {
                    s.connected
                        && !s.agents.agent_has_input(vm)
                        && s.vm(vm).is_some_and(|v| {
                            v.run_id == Some(run)
                                && v.state == VmState::Running
                                && v.config.tools.enabled
                                && v.config.tools.clipboard
                        })
                })
            });
        let blocked = self.tools_clipboard_pending.is_some()
            || self.transfers.busy()
            || state
                .as_ref()
                .and_then(|s| s.tools.as_ref())
                .is_some_and(|(_, _, status)| status.is_pending());
        if let Some(command) =
            self.clipboard_sync
                .poll(state.as_deref(), target, active, blocked, Instant::now())
        {
            self.send(command);
        }
        if self.clipboard_sync.busy() || (self.clipboard_sync.enabled && active) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // Console chords, removed from the input before any widget sees them.
        let chords = if self.section == Section::Console {
            let mut swallow = std::mem::take(&mut self.chord_swallow);
            let live = self.live.as_ref();
            let forwarded = |k: egui::Key| live.is_some_and(|l| l.is_forwarded(k));
            let c =
                ctx.input_mut(|i| console::take_chords(&mut i.events, &mut swallow, &forwarded));
            self.chord_swallow = swallow;
            c
        } else {
            console::Chords::default()
        };
        if chords.leave && self.fullscreen {
            self.set_fullscreen(&ctx, false);
        }

        let p = theme::Palette::of(ui);
        let immersive = self.fullscreen && self.section == Section::Console;
        // Agent session indicator: on every page, full screen included.
        self.agent_banner(ui, &p);
        if !immersive {
            self.side_panel(ui, &p);
            egui::Panel::bottom("lightvm_status")
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(p.sidebar_background)
                        .inner_margin(egui::Margin::symmetric(14, 4)),
                )
                .show(ui, |ui| status_bar_ui(ui, &self.rendering));
        }

        if self.section != Section::Console {
            if let Some(live) = &mut self.live {
                live.on_hidden(&ctx);
            }
            self.flush_console_input(&ctx);
            // A ticket that arrives after the page was left is not kept.
            if let Some(s) = &self.service {
                s.discard_console_ticket();
            }
            if self.fullscreen {
                self.set_fullscreen(&ctx, false);
            }
        }

        let margins = if immersive {
            egui::Margin::same(6)
        } else {
            egui::Margin {
                left: 32,
                right: 32,
                top: 26,
                bottom: 16,
            }
        };
        let background = if immersive {
            egui::Color32::BLACK
        } else {
            p.background
        };
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(background).inner_margin(margins))
            .show(ui, |ui| match self.section {
                Section::Dashboard => page(ui, "lightvm_page_dashboard", |ui| {
                    self.dashboard_page(ui);
                }),
                Section::Console => self.console_page(ui, &ctx, chords),
                Section::Agents => page(ui, "lightvm_page_agents", |ui| {
                    self.agents_page(ui);
                }),
                Section::Settings => page(ui, "lightvm_page_settings", |ui| {
                    self.settings_ui(ui, &ctx);
                }),
            });

        self.confirm_dialog_ui(ui);
        self.baseline_dialog_ui(ui);
        self.snapshot_dialog_ui(ui);
        self.media_dialog_ui(ui);
        self.revoke_dialog_ui(ui);
        self.standing_notice_ui(ui);
        self.standing_revoke_ui(ui);
        self.pairing_dialog_ui(ui);
        self.report_dialog_ui(ui);
        self.close_dialog_ui(ui, &ctx);
        self.catalog_move_ui(ui, &ctx);
        self.emergency_stop_over_dialogs(&ctx);
        // Second defence: no widget writes the host clipboard.
        clipboard::strip_clipboard_output(&ctx);
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        let (supervisor, supervisor_tone) = self.supervisor_summary();
        egui::Panel::left("lightvm_nav")
            .resizable(false)
            .exact_size(196.0)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar_background)
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 12,
                        top: 20,
                        bottom: 14,
                    }),
            )
            .show(ui, |ui| {
                brand(ui);
                ui.add_space(24.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                for s in Section::ALL {
                    let focus_ring = self.keyboard_nav;
                    if theme::nav_item(ui, s.label(), self.section == s, focus_ring).clicked() {
                        if s == Section::Dashboard && self.section == s {
                            // Back to the VM list from a VM's page.
                            self.baselines_vm = None;
                            self.snapshots_vm = None;
                        }
                        if s == Section::Agents && self.section == s {
                            self.agent_session = None;
                        }
                        self.section = s;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.spacing_mut().item_spacing.x = 7.0;
                        theme::dot(ui, p.tone(supervisor_tone));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(supervisor).small().color(p.text_weak),
                            )
                            .wrap(),
                        );
                    });
                });
            });
    }

    /// While a VM is starting (media and baseline verification, launch) the
    /// supervisor publishes no event for the progress: the VM list is
    /// re-read every [`START_POLL`].
    fn poll_starting(&mut self, ctx: &egui::Context) {
        let Some(st) = self.state() else { return };
        if !st.connected || !st.vms.iter().any(|v| v.state == VmState::Starting) {
            self.start_poll = None;
            return;
        }
        let now = Instant::now();
        if self
            .start_poll
            .is_none_or(|t| now.duration_since(t) >= START_POLL)
        {
            self.start_poll = Some(now);
            self.send(ServiceCommand::Refresh);
        }
        ctx.request_repaint_after(START_POLL);
    }

    /// Keyboard navigation in use: Tab or arrows since the last click.
    fn track_keyboard_navigation(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Key {
                        key: egui::Key::Tab,
                        pressed: true,
                        ..
                    } => self.keyboard_nav = true,
                    egui::Event::PointerButton { pressed: true, .. } => self.keyboard_nav = false,
                    _ => {}
                }
            }
        });
    }

    /// Supervisor state in a few words, for the sidebar: same text and tone
    /// as the Dashboard and Settings.
    fn supervisor_summary(&mut self) -> (String, theme::Tone) {
        if self.restart.is_some() {
            return ("Supervisor restarting".into(), theme::Tone::Warning);
        }
        if self.catalog_move_running() {
            return ("Moving the catalog".into(), theme::Tone::Warning);
        }
        let Some(s) = &mut self.service else {
            let tone = if self.service_error.is_some() {
                theme::Tone::Danger
            } else {
                theme::Tone::Neutral
            };
            return ("Supervisor not started".into(), tone);
        };
        let alive = s.is_alive();
        let (text, tone) = connection_state(alive, &self.state().unwrap_or_default());
        (format!("Supervisor {text}"), tone)
    }

    /// Indicator of a live agent session (or of a waiting request) above
    /// every page; refreshes the time left once a second.
    fn agent_banner(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        let Some(srv) = &mut self.service else { return };
        let alive = srv.is_alive();
        let state = self.state().unwrap_or_default();
        if state.agents.standing.live().is_some() {
            // Unattended access: its own bar, in the accent colour, above
            // the orange bar of a live session.
            let env = agents::AgentsEnv::new(&state, alive && state.connected, self.now());
            let mut action = None;
            egui::Panel::top("lightvm_standing_banner")
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(p.accent.linear_multiply(0.22))
                        .stroke(egui::Stroke::new(2.0, p.accent))
                        .inner_margin(egui::Margin::symmetric(14, 6)),
                )
                .show(ui, |ui| {
                    action = agents::standing_banner_ui(ui, &env);
                });
            if let Some(a) = action {
                self.run_agent_action(a, &state);
            }
        }
        let live = state.agents.live().next().is_some();
        let waiting = !state.agents.requests.is_empty();
        if !live && !(waiting && self.section != Section::Agents) {
            return;
        }
        if live || waiting {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        let env = agents::AgentsEnv::new(&state, alive && state.connected, self.now());
        let tone = if live { p.warning } else { p.accent };
        let mut action = None;
        egui::Panel::top("lightvm_agent_banner")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(tone.linear_multiply(0.16))
                    .stroke(egui::Stroke::new(1.0, tone))
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                action = agents::banner_ui(ui, &env, self.section != Section::Agents);
            });
        if let Some(a) = action {
            self.run_agent_action(a, &state);
        }
    }

    /// A dialog blocks the window, the indicator included: while an agent
    /// session or the unattended access is live the Emergency Stop stays
    /// reachable above it, where the indicator shows it.
    fn emergency_stop_over_dialogs(&mut self, ctx: &egui::Context) {
        if !self.dialog_open() && self.closing.is_none() {
            return;
        }
        if self
            .state()
            .is_none_or(|s| s.agents.live().next().is_none() && s.agents.standing.live().is_none())
        {
            return;
        }
        let at: Option<egui::Rect> =
            ctx.data(|d| d.get_temp(egui::Id::new(agents::BANNER_STOP_RECT)));
        let area = egui::Area::new(egui::Id::new("lightvm_emergency_over_dialogs"))
            .order(egui::Order::Tooltip);
        let area = match at {
            Some(r) => area.fixed_pos(r.min),
            None => area.anchor(egui::Align2::RIGHT_TOP, egui::vec2(-14.0, 6.0)),
        };
        let clicked = area
            .show(ctx, |ui| {
                let mut b =
                    theme::danger(ui, "Emergency Stop").fill(theme::Palette::of(ui).surface);
                if let Some(r) = at {
                    b = b.min_size(r.size());
                }
                let r = ui
                    .add(b)
                    .on_hover_text("Immediately terminates all VMs and revokes the agent sessions");
                theme::accessible_name(
                    &r,
                    egui::WidgetType::Button,
                    "Emergency Stop: stop all VMs and agent sessions now",
                );
                r.clicked()
            })
            .inner;
        if clicked {
            self.send(ServiceCommand::EmergencyStop);
        }
    }

    fn agents_page(&mut self, ui: &mut egui::Ui) {
        let Some(srv) = &mut self.service else {
            theme::page_header(ui, agents::TITLE, agents::DESCRIPTION, |_| {});
            theme::card(ui, |ui| {
                empty_state(
                    ui,
                    "Agent sessions are not available: the supervisor is not running.",
                    "Start it from Settings, Supervisor.",
                );
            });
            return;
        };
        let alive = srv.is_alive();
        let state = self.state().unwrap_or_default();
        // A form whose request is gone (decided, expired, withdrawn) closes,
        // unless its own approval just succeeded or failed.
        if let Some(f) = &self.approve_form {
            let approved = state
                .agents
                .approval
                .as_ref()
                .is_some_and(|a| a.request_id == f.request_id && !a.status.is_pending());
            let done = matches!(
                state.agents.approval.as_ref(),
                Some(a) if a.request_id == f.request_id && matches!(a.status, RequestStatus::Done(_))
            );
            if done || (state.agents.request(f.request_id).is_none() && !(f.sent && approved)) {
                self.approve_form = None;
            }
        }
        // The unattended access form closes once its creation succeeded (or
        // a live grant appeared meanwhile).
        if let Some(f) = &self.standing_form {
            let st = &state.agents.standing;
            let created = f.sent
                && st.control.as_ref().is_some_and(|c| {
                    c.op == service::StandingOp::Create
                        && matches!(c.status, RequestStatus::Done(()))
                });
            if created || st.live().is_some() {
                self.standing_form = None;
            }
        }
        let active = alive && state.connected;
        if active {
            self.poll_reports(&state);
        }
        let watch_shot = self.watched_shot(&state, active);
        self.sync_shot_textures(ui.ctx(), &state, watch_shot);
        let texture = |key: Option<(SessionId, u32)>| {
            key.map(|(session_id, shot)| agents::Shown {
                session_id,
                shot,
                texture: self
                    .shot_textures
                    .iter()
                    .find(|(k, _)| *k == (session_id, shot))
                    .map(|(_, t)| t),
            })
        };
        let env = agents::AgentsEnv {
            shown: texture(self.shown_shot),
            watch: self.watch_screen,
            watch_shot: texture(watch_shot),
            ..agents::AgentsEnv::new(&state, active, self.now())
        };
        let action = match self.agent_session {
            Some(id) => agents::session_page_ui(ui, &env, id),
            None => agents::agents_ui(ui, &env, &mut self.approve_form, &mut self.standing_form),
        };
        if let Some(a) = action {
            self.run_agent_action(a, &state);
        }
    }

    /// Re-reads, every [`REPORT_POLL`], the header and the last events of
    /// the live sessions' reports and the events page of the session page
    /// when its session is live: the supervisor publishes an event only
    /// when a report opens, ends or gets a verdict.
    fn poll_reports(&mut self, state: &ServiceState) {
        if self.report_poll.is_some_and(|t| t.elapsed() < REPORT_POLL) {
            return;
        }
        self.report_poll = Some(Instant::now());
        let live: Vec<SessionId> = state.agents.live().map(|s| s.session_id).collect();
        for id in &live {
            let total = state.reports.headers.get(id).map_or(0, |h| h.events_total);
            self.send(ServiceCommand::GetSessionReport(*id));
            self.send(ServiceCommand::GetSessionReportEvents {
                session_id: *id,
                offset: total.saturating_sub(service::TAIL_EVENTS),
                limit: service::TAIL_EVENTS,
                tail: true,
            });
        }
        if let Some(id) = self.agent_session
            && live.contains(&id)
        {
            let offset = state.reports.events.get(&id).map_or(0, |p| p.offset);
            self.send(ServiceCommand::GetSessionReportEvents {
                session_id: id,
                offset,
                limit: service::EVENTS_PAGE,
                tail: false,
            });
        }
    }

    /// Latest screenshot of the watched live session: requested once its
    /// index is known (the header counts the screenshots taken).
    fn watched_shot(&mut self, state: &ServiceState, active: bool) -> Option<(SessionId, u32)> {
        let id = self.watch_screen?;
        if state
            .agents
            .session(id)
            .is_none_or(|s| s.state == lightvm_proto::SessionState::Terminated)
        {
            self.watch_screen = None;
            return None;
        }
        let h = state.reports.headers.get(&id)?;
        let n = (h.screenshots_retained + h.screenshots_dropped).checked_sub(1)?;
        if active && state.reports.shot(id, n).is_none() {
            self.send(ServiceCommand::GetReportScreenshot {
                session_id: id,
                shot: n,
            });
        }
        Some((id, n))
    }

    /// Textures of the screenshots shown, made once each from the decoded
    /// pixels; the ones no longer shown are dropped.
    fn sync_shot_textures(
        &mut self,
        ctx: &egui::Context,
        state: &ServiceState,
        watch: Option<(SessionId, u32)>,
    ) {
        let wanted: Vec<(SessionId, u32)> =
            [self.shown_shot, watch].into_iter().flatten().collect();
        self.shot_textures.retain(|(k, _)| wanted.contains(k));
        for key in wanted {
            if self.shot_textures.iter().any(|(k, _)| *k == key) {
                continue;
            }
            if let Some(RequestStatus::Done(img)) = state.reports.shot(key.0, key.1) {
                let image = egui::ColorImage::from_rgba_unmultiplied(img.size, &img.rgba);
                let tex = ctx.load_texture(
                    format!("lightvm_report_shot_{}_{}", key.0, key.1),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                self.shot_textures.push((key, tex));
            }
        }
    }

    /// Opens a report dialog for a session whose report is known.
    fn open_report_dialog(&mut self, state: &ServiceState, id: SessionId, kind: ReportDialogKind) {
        let Some(r) = state.reports.summary(id) else {
            return;
        };
        self.report_dialog = Some(ReportDialog {
            session_id: id,
            client: r.client_name.clone(),
            started: r.started_at,
            kind,
            sent: false,
            focus: true,
            note: None,
        });
    }

    /// Executes a choice of the Agents views.
    fn run_agent_action(&mut self, a: agents::AgentAction, state: &ServiceState) {
        use agents::AgentAction as A;
        match a {
            A::SetAccess(on) => self.send(ServiceCommand::SetAgentAccess(on)),
            A::Pair => {
                self.pairing_dialog = Some(PairingDialog {
                    sent: false,
                    focus: true,
                });
            }
            A::Review(id) => {
                self.approve_form = state
                    .agents
                    .request(id)
                    .map(|r| agents::ApproveForm::new(r, &state.vms));
            }
            A::CancelReview => self.approve_form = None,
            A::Approve(request_id, approval) => {
                if let Some(f) = &mut self.approve_form {
                    f.sent = true;
                }
                self.send(ServiceCommand::ApproveSession {
                    request_id,
                    approval,
                });
            }
            A::Deny(id) => {
                if self
                    .approve_form
                    .as_ref()
                    .is_some_and(|f| f.request_id == id)
                {
                    self.approve_form = None;
                }
                self.send(ServiceCommand::DenySession(id));
            }
            A::Pause(id) => self.send(ServiceCommand::PauseSession(id)),
            A::Resume(id) => self.send(ServiceCommand::ResumeSession(id)),
            A::Revoke(id) => {
                if let Some(s) = state.agents.session(id) {
                    self.revoke_dialog = Some(RevokeDialog {
                        session_id: id,
                        client: s.client_name.clone(),
                        vm_name: state
                            .vm(s.vm_id)
                            .map_or_else(|| "its VM".into(), |v| v.name.as_str().to_owned()),
                        sent: false,
                        focus: true,
                    });
                }
            }
            A::TakeInput(vm_id) => self.send(ServiceCommand::SetInputOwner {
                vm_id,
                owner: InputOwner::User,
            }),
            A::GiveInput(vm_id, session_id) => {
                // Keys still held are released by the supervisor on the
                // handover.
                self.send(ServiceCommand::SetInputOwner {
                    vm_id,
                    owner: InputOwner::Agent { session_id },
                });
            }
            A::Details(id) => {
                if self.agent_session != Some(id) {
                    self.shown_shot = None;
                }
                self.agent_session = Some(id);
                self.section = Section::Agents;
                self.send(ServiceCommand::GetSessionReport(id));
                let offset = state.reports.events.get(&id).map_or(0, |p| p.offset);
                self.send(ServiceCommand::GetSessionReportEvents {
                    session_id: id,
                    offset,
                    limit: service::EVENTS_PAGE,
                    tail: false,
                });
            }
            A::Back => {
                self.agent_session = None;
                self.shown_shot = None;
            }
            // Leaving the Console page also leaves full screen.
            A::Open => {
                self.agent_session = None;
                self.shown_shot = None;
                self.section = Section::Agents;
            }
            A::EmergencyStop => self.send(ServiceCommand::EmergencyStop),
            A::Refresh => {
                self.send(ServiceCommand::GetAgentAccess);
                self.send(ServiceCommand::ListSessionRequests);
                self.send(ServiceCommand::ListSessions);
                self.send(ServiceCommand::ListSessionReports);
                if let Some(id) = self.agent_session {
                    self.send(ServiceCommand::GetSessionReport(id));
                }
            }
            A::EventsPage(session_id, offset) => {
                self.send(ServiceCommand::GetSessionReportEvents {
                    session_id,
                    offset,
                    limit: service::EVENTS_PAGE,
                    tail: false,
                });
            }
            A::ShowShot(session_id, shot) => {
                self.shown_shot = Some((session_id, shot));
                if !matches!(
                    state.reports.shot(session_id, shot),
                    Some(RequestStatus::Done(_) | RequestStatus::Pending)
                ) {
                    self.send(ServiceCommand::GetReportScreenshot { session_id, shot });
                }
            }
            A::HideShot => self.shown_shot = None,
            A::WatchScreen(id) => {
                self.watch_screen = id;
                if let Some(id) = id {
                    // The index of the latest screenshot is in the header.
                    self.send(ServiceCommand::GetSessionReport(id));
                }
            }
            A::Verdict(id) => {
                self.open_report_dialog(state, id, ReportDialogKind::Verdict(None));
            }
            A::Verify(id) => self.open_report_dialog(state, id, ReportDialogKind::Verify(None)),
            A::Export(id) => self.open_report_dialog(
                state,
                id,
                ReportDialogKind::Export {
                    format: lightvm_proto::report::ReportFormat::Markdown,
                    screenshots: false,
                    path: None,
                    overwrite: false,
                },
            ),
            A::DeleteReport(id) => self.open_report_dialog(state, id, ReportDialogKind::Delete),
            A::CreateStanding => {
                self.standing_form = Some(agents::StandingForm::new(&state.vms));
            }
            A::CancelStanding => self.standing_form = None,
            A::SubmitStanding(spec) => {
                if let Some(f) = &mut self.standing_form {
                    f.sent = true;
                }
                self.send(ServiceCommand::CreateStandingGrant(spec));
            }
            A::RevokeStanding(grant_id) => {
                let vm_name = state
                    .agents
                    .standing
                    .live()
                    .and_then(|g| state.vm(g.vm_id))
                    .map_or_else(|| "its VM".into(), |v| v.name.as_str().to_owned());
                self.standing_revoke = Some(StandingRevokeDialog {
                    grant_id,
                    vm_name,
                    sent: false,
                    focus: true,
                });
            }
            A::ConfirmStanding(grant_id) => {
                self.send(ServiceCommand::ConfirmStandingGrant(grant_id));
            }
        }
    }

    /// Once per app run, when the unattended access is first read: if one
    /// is live, the notice opens and waits for the user.
    fn check_standing_notice(&mut self) {
        if self.standing_notice != StandingNotice::Unchecked {
            return;
        }
        let Some(st) = self.state() else { return };
        if !st.agents.standing.loaded {
            return;
        }
        self.standing_notice = if st.agents.standing.live().is_some() {
            StandingNotice::Open
        } else {
            StandingNotice::Done
        };
    }

    /// Notice of a live unattended access at start (acknowledged with a
    /// button only).
    fn standing_notice_ui(&mut self, ui: &egui::Ui) {
        if self.standing_notice != StandingNotice::Open {
            return;
        }
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let Some(g) = state.agents.standing.live().cloned() else {
            // Revoked or expired meanwhile: nothing to acknowledge.
            self.standing_notice = StandingNotice::Done;
            return;
        };
        let env = agents::AgentsEnv::new(&state, state.connected, self.now());
        match agents::standing_notice_ui(ui, &env) {
            Some(agents::StandingNoticeChoice::Keep) => {
                self.standing_notice = StandingNotice::Done;
            }
            Some(agents::StandingNoticeChoice::Revoke) => {
                self.standing_notice = StandingNotice::Done;
                self.run_agent_action(agents::AgentAction::RevokeStanding(g.grant_id), &state);
            }
            None => {}
        }
    }

    /// Confirmation of the revocation of the unattended access.
    fn standing_revoke_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.standing_revoke else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let st = &state.agents.standing;
        let control = st
            .control
            .as_ref()
            .filter(|c| c.op == service::StandingOp::Revoke(d.grant_id));
        let mut error = None;
        let mut pending = false;
        if d.sent {
            match control.map(|c| &c.status) {
                Some(RequestStatus::Pending) => pending = true,
                Some(RequestStatus::Done(())) => {
                    self.standing_revoke = None;
                    return;
                }
                Some(RequestStatus::Failed(f)) => error = Some(f.message.clone()),
                _ => {}
            }
        }
        let gone = st.live().is_none_or(|g| g.grant_id != d.grant_id);
        if gone && !pending && error.is_none() {
            if d.sent {
                self.standing_revoke = None;
                return;
            }
            error = Some("The unattended access has already ended.".into());
        }
        let spec = dialogs::ConfirmSpec {
            title: format!("Revoke the unattended access to {}?", d.vm_name),
            body: "Its pairing credential stops working at once, also on an open connection, and its pairing file is removed. Its current session, if any, ends. The VM keeps running. To allow unattended access again you create a new one.".into(),
            confirm: "Revoke unattended access".into(),
            destructive: true,
        };
        let focus = std::mem::replace(&mut d.focus, false);
        match dialogs::confirm(
            ui,
            "lightvm_standing_revoke_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
        ) {
            Some(dialogs::Choice::Cancel) => self.standing_revoke = None,
            Some(dialogs::Choice::Confirm) if !gone => {
                d.sent = true;
                let id = d.grant_id;
                self.send(ServiceCommand::RevokeStandingGrant(id));
            }
            _ => {}
        }
    }

    /// Confirmation of a revocation.
    fn revoke_dialog_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.revoke_dialog else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let session = state.agents.session(d.session_id);
        let control = state
            .agents
            .control
            .as_ref()
            .filter(|c| c.op == service::AgentOp::Revoke(d.session_id));
        let mut error = None;
        let mut pending = false;
        if d.sent {
            match control.map(|c| &c.status) {
                Some(RequestStatus::Pending) => pending = true,
                Some(RequestStatus::Done(())) => {
                    self.revoke_dialog = None;
                    return;
                }
                Some(RequestStatus::Failed(f)) => error = Some(f.message.clone()),
                _ => {}
            }
        }
        let ended = session.is_none_or(|s| s.state == lightvm_proto::SessionState::Terminated);
        if ended && !pending && error.is_none() {
            if d.sent {
                self.revoke_dialog = None;
                return;
            }
            error = Some("The session has already ended.".into());
        }
        let spec = dialogs::ConfirmSpec {
            title: format!("Revoke the session of \u{201c}{}\u{201d}?", d.client),
            body: format!(
                "The agent loses access to \u{201c}{}\u{201d} at once and its connection is closed; it needs a new approval to come back. Actions it already sent may still complete. The VM keeps running.",
                d.vm_name
            ),
            confirm: "Revoke session".into(),
            destructive: true,
        };
        let focus = std::mem::replace(&mut d.focus, false);
        match dialogs::confirm(
            ui,
            "lightvm_revoke_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
        ) {
            Some(dialogs::Choice::Cancel) => self.revoke_dialog = None,
            Some(dialogs::Choice::Confirm) if !ended => {
                d.sent = true;
                let id = d.session_id;
                self.send(ServiceCommand::RevokeSession(id));
            }
            _ => {}
        }
    }

    /// Confirmation of a report action: verdict, automatic check, export
    /// (format, screenshots, then the native save dialog) or deletion.
    fn report_dialog_ui(&mut self, ui: &egui::Ui) {
        use lightvm_proto::report::{FixtureMarker, ReportFormat};
        let Some(d) = &mut self.report_dialog else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let op = d.op();
        let status = state
            .reports
            .action
            .as_ref()
            .filter(|a| a.op == op)
            .map(|a| a.status.clone());
        let mut pending = false;
        let mut error = d.note.clone();
        let mut exported = None;
        let mut conflict = false;
        if d.sent {
            match status {
                Some(RequestStatus::Pending) => pending = true,
                Some(RequestStatus::Done(service::ReportOutcome::Exported(x))) => {
                    exported = Some(x);
                }
                Some(RequestStatus::Done(_)) => {
                    let id = d.session_id;
                    let delete = d.kind == ReportDialogKind::Delete;
                    self.report_dialog = None;
                    if delete && self.agent_session == Some(id) {
                        self.agent_session = None;
                        self.shown_shot = None;
                    }
                    return;
                }
                Some(RequestStatus::Failed(f)) => {
                    let overwrite = matches!(
                        d.kind,
                        ReportDialogKind::Export {
                            overwrite: true,
                            ..
                        }
                    );
                    if matches!(d.kind, ReportDialogKind::Export { .. })
                        && f.code == Some(lightvm_proto::ErrorCode::StateConflict)
                        && !overwrite
                    {
                        conflict = true;
                        error = Some(
                            "A file with this name already exists there. Replace it, or Cancel and export again with another name."
                                .into(),
                        );
                    } else {
                        error = Some(f.message);
                    }
                }
                _ => {}
            }
        }
        let header = state.reports.headers.get(&d.session_id);
        let current = header.map_or_else(String::new, |h| {
            format!(
                " (now {}, {})",
                reports::outcome_text(h.outcome).to_lowercase(),
                reports::source_short(h.outcome_source)
            )
        });
        let stored = header.map_or(0, |h| h.screenshots_retained);
        let who = format!(
            "\u{201c}{}\u{201d}, started {}",
            d.client,
            baselines::format_time(d.started)
        );
        let spec = match &d.kind {
            ReportDialogKind::Verdict(_) => dialogs::ConfirmSpec {
                title: "Set your verdict".into(),
                body: format!(
                    "Session of {who}. Your verdict becomes the outcome{current}, recorded as decided by \u{201c}local user\u{201d} with the time. Earlier verdicts stay in the report's history; the agent's declaration is not changed."
                ),
                confirm: "Record verdict".into(),
                destructive: false,
            },
            ReportDialogKind::Verify(_) => dialogs::ConfirmSpec {
                title: "Run the automatic check".into(),
                body: format!(
                    "Session of {who}. LightVM checks the agent's last screenshot for the LightVM test fixture screen (reference blocks and marker). It is meant for test VMs that show the fixture; on any other screen the result is failed or inconclusive. The result replaces the outcome{current} and is attributed to the verifier."
                ),
                confirm: "Run check".into(),
                destructive: false,
            },
            ReportDialogKind::Export { .. } if exported.is_some() => dialogs::ConfirmSpec {
                title: "Report exported".into(),
                body: exported
                    .as_ref()
                    .map(reports::export_text)
                    .unwrap_or_default(),
                confirm: "Done".into(),
                destructive: false,
            },
            ReportDialogKind::Export { .. } => dialogs::ConfirmSpec {
                title: "Export the session report".into(),
                body: format!(
                    "Session of {who}. The report is written to a file you choose: objective, limits, every recorded action, screenshot hashes and the outcome. Typed text is never in it, only its length and SHA-256."
                ),
                confirm: if conflict {
                    "Replace the file".into()
                } else {
                    "Choose where to save\u{2026}".into()
                },
                destructive: conflict,
            },
            ReportDialogKind::Delete => dialogs::ConfirmSpec {
                title: "Delete the report?".into(),
                body: format!(
                    "Session of {who}. The report and its {stored} stored screenshot{} are removed from this computer. Copies you exported are not touched. This cannot be undone.",
                    if stored == 1 { "" } else { "s" }
                ),
                confirm: "Delete report".into(),
                destructive: true,
            },
        };
        let focus = std::mem::replace(&mut d.focus, false);
        let done = exported.is_some();
        let kind = &mut d.kind;
        let choice = dialogs::confirm_custom(
            ui,
            "lightvm_report_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
            |ui, enabled| match kind {
                ReportDialogKind::Verdict(o) => {
                    ui.add_space(10.0);
                    ui.add_enabled_ui(enabled, |ui| {
                        for (v, text) in reports::VERDICTS {
                            ui.radio_value(o, Some(v), text);
                        }
                    });
                    o.is_some()
                }
                ReportDialogKind::Verify(m) => {
                    ui.add_space(10.0);
                    ui.add_enabled_ui(enabled, |ui| {
                        ui.radio_value(
                            m,
                            Some(FixtureMarker::Present),
                            "The marker should be on screen",
                        );
                        ui.radio_value(
                            m,
                            Some(FixtureMarker::Absent),
                            "The marker should not be on screen",
                        );
                    });
                    m.is_some()
                }
                ReportDialogKind::Export {
                    format,
                    screenshots,
                    ..
                } if !done => {
                    ui.add_space(10.0);
                    ui.add_enabled_ui(enabled && !conflict, |ui| {
                        ui.radio_value(format, ReportFormat::Markdown, "Markdown, to read");
                        ui.radio_value(
                            format,
                            ReportFormat::Json,
                            "JSON, the canonical report for tools",
                        );
                        ui.add_space(6.0);
                        let text = format!(
                            "Also save the {stored} stored screenshot{} in a folder next to it",
                            if stored == 1 { "" } else { "s" }
                        );
                        ui.add_enabled(stored > 0, egui::Checkbox::new(screenshots, text));
                    });
                    if *screenshots {
                        theme::notice(
                            ui,
                            "Screenshots can show anything that was on the guest screen, passwords included.",
                            theme::Tone::Warning,
                        );
                    }
                    true
                }
                _ => true,
            },
        );
        match choice {
            Some(dialogs::Choice::Cancel) => self.report_dialog = None,
            Some(dialogs::Choice::Confirm) if done => self.report_dialog = None,
            Some(dialogs::Choice::Confirm) => {
                let id = d.session_id;
                let (client, started) = (d.client.clone(), d.started);
                let command = match &mut d.kind {
                    ReportDialogKind::Verdict(Some(o)) => Some(ServiceCommand::SetSessionVerdict {
                        session_id: id,
                        outcome: *o,
                        rationale: reports::USER_VERDICT_RATIONALE.into(),
                    }),
                    ReportDialogKind::Verify(Some(m)) => Some(ServiceCommand::VerifySession {
                        session_id: id,
                        verifier: lightvm_proto::report::SessionVerifier::FixtureMarker {
                            expected: *m,
                        },
                    }),
                    ReportDialogKind::Export {
                        format,
                        screenshots,
                        path: Some(path),
                        overwrite,
                    } if conflict => {
                        *overwrite = true;
                        Some(ServiceCommand::ExportSessionReport {
                            session_id: id,
                            format: *format,
                            destination: path.clone(),
                            overwrite: true,
                            include_screenshots: *screenshots,
                        })
                    }
                    ReportDialogKind::Export {
                        format,
                        screenshots,
                        path,
                        overwrite,
                    } => {
                        let name = report_file_name(&client, started, *format);
                        match self.report_picker.as_mut() {
                            None => {
                                d.note = Some("Save dialog not available on this platform.".into());
                                None
                            }
                            Some(pick) => pick(&name, *format).map(|chosen| {
                                *path = Some(chosen.clone());
                                *overwrite = false;
                                ServiceCommand::ExportSessionReport {
                                    session_id: id,
                                    format: *format,
                                    destination: chosen,
                                    overwrite: false,
                                    include_screenshots: *screenshots,
                                }
                            }),
                        }
                    }
                    ReportDialogKind::Delete => Some(ServiceCommand::DeleteSessionReport(id)),
                    _ => None,
                };
                if let Some(c) = command {
                    d.sent = true;
                    d.note = None;
                    self.send(c);
                }
            }
            None => {}
        }
    }

    /// "Pair a client": explanation, then where the pairing file is.
    fn pairing_dialog_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.pairing_dialog else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let now = self.fixed_now.unwrap_or_else(UtcTimestamp::now);
        let focus = std::mem::replace(&mut d.focus, false);
        let (spec, pending, error) = match (&state.agents.pairing, d.sent) {
            (RequestStatus::Done(f), true) => (
                dialogs::ConfirmSpec {
                    title: "Pairing file ready".into(),
                    body: format!(
                        "Written to {}, readable only by your user account, valid until {} ({}).\n\nStart the LightVM MCP adapter, or restart your MCP client, now: it reads the file and asks for a session. The request appears on the Agents page with a verification code to compare with the one the agent shows.",
                        f.path.display(),
                        agents::clock(f.expires_at),
                        agents::time_left(now, f.expires_at)
                    ),
                    confirm: "Done".into(),
                    destructive: false,
                },
                false,
                None,
            ),
            (status, sent) => (
                dialogs::ConfirmSpec {
                    title: "Pair an agent client".into(),
                    body: "LightVM issues a pairing credential valid for 10 minutes and writes it to a file in your user profile that only your user account can read. The LightVM MCP adapter reads it from there.\n\nThe credential is never shown, copied or logged, and it gives no access to any VM: the agent can only ask for a session, which you approve here.".into(),
                    confirm: "Write pairing file".into(),
                    destructive: false,
                },
                sent && status.is_pending(),
                match status {
                    RequestStatus::Failed(f) if sent => Some(f.message.clone()),
                    _ => None,
                },
            ),
        };
        let ready = d.sent && matches!(state.agents.pairing, RequestStatus::Done(_));
        match dialogs::confirm(
            ui,
            "lightvm_pairing_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
        ) {
            Some(dialogs::Choice::Cancel) => self.pairing_dialog = None,
            Some(dialogs::Choice::Confirm) if ready => self.pairing_dialog = None,
            Some(dialogs::Choice::Confirm) => {
                d.sent = true;
                self.send(ServiceCommand::CreatePairing);
            }
            None => {}
        }
    }

    fn dashboard_page(&mut self, ui: &mut egui::Ui) {
        let Some(srv) = &mut self.service else {
            theme::page_header(ui, "Dashboard", dashboard::DESCRIPTION, |_| {});
            if self.restart.is_some() {
                theme::notice(
                    ui,
                    "Supervisor restart in progress: running VMs are being stopped in an orderly way.",
                    theme::Tone::Warning,
                );
                ui.add_space(14.0);
            }
            if let Some(e) = &self.service_error {
                theme::notice(ui, &format!("Supervisor: {e}"), theme::Tone::Danger);
                ui.add_space(14.0);
            }
            theme::group_title(ui, "Virtual machines");
            theme::card(ui, |ui| {
                empty_state(
                    ui,
                    "The VM catalog is not available: the supervisor is not running.",
                    "Start it from Settings, Supervisor.",
                );
            });
            return;
        };
        let alive = srv.is_alive();
        let pid = srv.pid();
        let state = self.state().unwrap_or_default();
        let active = alive && state.connected;

        if self.wizard.is_some() {
            self.wizard_page(ui, &state, active);
            return;
        }
        if self.vmware_import.is_some() {
            self.vmware_import_page(ui, &state, active);
            return;
        }
        if let Some(vm_id) = self.baselines_vm {
            self.baselines_page(ui, &state, vm_id, active);
            return;
        }
        if let Some(vm_id) = self.snapshots_vm {
            self.snapshots_page(ui, &state, vm_id, active);
            return;
        }

        let qemu = self.supervisor_qemu();
        match dashboard_with_qemu_ui(ui, &state, pid, alive, qemu) {
            Some(DashboardAction::Command(c)) => self.send(c),
            Some(DashboardAction::QemuSettings) => self.section = Section::Settings,
            Some(DashboardAction::NewVm) => {
                self.vm_edit = None;
                self.wizard = Some(wizard::Wizard::with_guest_support(
                    state.host,
                    self.guest_support,
                ));
                self.send(ServiceCommand::ListMedia);
            }
            Some(DashboardAction::ImportVmware) => {
                self.vm_edit = None;
                self.choose_vmx(true);
            }
            Some(DashboardAction::Console(id)) => self.open_console(id),
            Some(DashboardAction::Baselines(id)) => {
                self.vm_edit = None;
                self.baselines_vm = Some(id);
                self.send(ServiceCommand::ListBaselines(id));
            }
            Some(DashboardAction::Snapshots(id)) => {
                self.vm_edit = None;
                self.snapshots_vm = Some(id);
                self.send(ServiceCommand::ListSnapshots(id));
            }
            Some(DashboardAction::Edit(id)) => {
                self.vm_edit = state.vm(id).map(|v| {
                    vm_settings::VmEdit::new(v.vm_id, v.config_revision, v.config.clone())
                });
                self.send(ServiceCommand::ListMedia);
            }
            Some(DashboardAction::Confirm(kind)) => {
                if let Some(v) = state.vm(kind.vm_id()) {
                    self.dialog = Some(ConfirmDialog {
                        kind,
                        name: v.name.as_str().to_owned(),
                        revision: v.config_revision,
                        run_id: v.run_id,
                        sent: false,
                        focus: true,
                    });
                }
            }
            None => {}
        }
        let Some(m) = &mut self.vm_edit else {
            return;
        };
        ui.add_space(14.0);
        let current = state.vm(m.vm_id);
        let stopped = active
            && current
                .is_some_and(|v| v.state == VmState::Stopped && v.baselines.operation.is_none());
        let max_vcpu = wizard::Bounds::new(state.host).max_vcpus;
        match vm_settings::vm_settings_ui(ui, m, stopped, max_vcpu, &state.media) {
            Some(vm_settings::EditOutcome::Save) => {
                let c = ServiceCommand::UpdateVm {
                    vm_id: m.vm_id,
                    revision: m.revision,
                    config: Box::new(m.config.clone()),
                };
                self.vm_edit = None;
                self.send(c);
            }
            Some(vm_settings::EditOutcome::Cancel) => self.vm_edit = None,
            None => {}
        }
    }

    fn baselines_page(
        &mut self,
        ui: &mut egui::Ui,
        state: &ServiceState,
        vm_id: VmId,
        active: bool,
    ) {
        let Some(vm) = state.vm(vm_id) else {
            // Deleted meanwhile.
            self.baselines_vm = None;
            return;
        };
        let request = state.baseline.as_ref().filter(|r| r.vm_id == vm_id);
        let any_pending = state
            .baseline
            .as_ref()
            .is_some_and(|r| r.status.is_pending());
        let running = vm.baselines.operation.is_some()
            || request
                .is_some_and(|r| r.status.is_pending() && !matches!(r.op, BaselineOp::Delete(_)));
        if !running && !any_pending {
            self.baseline_sent = None;
        }
        let elapsed = self
            .baseline_sent
            .filter(|(id, _)| *id == vm_id && running)
            .map(|(_, t)| t.elapsed());
        if running {
            // Elapsed time and the end of the operation, without polling
            // faster than once a second.
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        let list = state.baselines.get(&vm_id);
        let env = baselines::BaselinesEnv {
            vm,
            list,
            request,
            any_pending,
            active,
            available: dashboard::baselines_available(state),
            elapsed,
        };
        let open = |kind| BaselineDialog {
            vm_id,
            name: vm.name.as_str().to_owned(),
            kind,
            unclean: false,
            consent: false,
            sent: None,
            focus: true,
        };
        let find = |id: BaselineId| list.and_then(|l| l.items.iter().find(|b| b.baseline_id == id));
        match baselines::baselines_ui(ui, &env) {
            Some(baselines::BaselineAction::Back) => self.baselines_vm = None,
            Some(baselines::BaselineAction::EmergencyStop) => {
                self.send(ServiceCommand::EmergencyStop);
            }
            Some(baselines::BaselineAction::Refresh) => {
                self.send(ServiceCommand::Refresh);
                self.send(ServiceCommand::ListBaselines(vm_id));
            }
            Some(baselines::BaselineAction::Create) => {
                let items = list.map_or(&[][..], |l| &l.items[..]);
                let label = baselines::auto_label(items, self.now());
                self.baseline_dialog = Some(open(BaselineConfirm::Create { label }));
            }
            Some(baselines::BaselineAction::Restore(id)) => {
                if let Some(b) = find(id) {
                    self.baseline_dialog = Some(open(BaselineConfirm::Restore {
                        id,
                        label: b.label.as_str().to_owned(),
                    }));
                }
            }
            Some(baselines::BaselineAction::Delete(id)) => {
                if let Some(b) = find(id) {
                    self.baseline_dialog = Some(open(BaselineConfirm::Delete {
                        id,
                        label: b.label.as_str().to_owned(),
                        size: b.size_bytes,
                    }));
                }
            }
            None => {}
        }
    }

    /// The advance of a revert of a running VM (ADR-0032): the offered
    /// snapshot of the current state, then the orderly shutdown of the run
    /// the user confirmed, then the revert itself (it needs the VM off).
    /// Runs on every frame, whatever the page, so it is not lost by
    /// leaving the page. It is abandoned, with a message on the Snapshots
    /// page, if the stop is refused or fails, if the VM runs another run,
    /// or if the VM is not off within [`REVERT_STOP_TIMEOUT`].
    fn snapshot_revert_flow_step(&mut self, state: &ServiceState, now: Instant) {
        let Some(f) = &mut self.snapshot_revert_flow else {
            return;
        };
        let (vm_id, run_id) = (f.vm_id, f.run_id);
        let Some(vm) = state.vm(vm_id) else {
            // Deleted meanwhile.
            self.snapshot_revert_flow = None;
            return;
        };
        let abandon = |why: &str| (vm_id, format!("The revert was not done: {why}."));
        if vm.state.has_process() && vm.run_id != Some(run_id) {
            self.snapshot_revert_flow = None;
            self.snapshot_revert_notice =
                Some(abandon("the VM was started again before it was off"));
            return;
        }
        match f.waiting {
            RevertWaiting::Take => {
                let done = state
                    .snapshot
                    .as_ref()
                    .filter(|r| r.vm_id == vm_id && matches!(r.op, SnapshotOp::Take));
                match done.map(|r| &r.status) {
                    Some(RequestStatus::Done(_)) => {
                        f.waiting = RevertWaiting::Stop;
                        f.stop_sent = Some(now);
                        self.send(ServiceCommand::StopRun { vm_id, run_id });
                    }
                    // The snapshot was not taken: the failure is shown on
                    // the page; the VM still runs, the flow stops.
                    Some(RequestStatus::Failed(_)) => self.snapshot_revert_flow = None,
                    _ => {}
                }
            }
            RevertWaiting::Stop => {
                let stop = state
                    .stop
                    .as_ref()
                    .filter(|r| r.vm_id == vm_id && r.run_id == run_id);
                if let Some(service::StopRequest {
                    status: RequestStatus::Failed(e),
                    ..
                }) = stop
                {
                    self.snapshot_revert_flow = None;
                    self.snapshot_revert_notice = Some(abandon(&format!(
                        "the VM could not be shut down ({})",
                        e.message
                    )));
                    return;
                }
                if vm.state == VmState::Stopped {
                    let snapshot_id = f.snapshot_id;
                    self.snapshot_revert_flow = None;
                    self.snapshot_sent = Some((vm_id, Instant::now()));
                    self.send(ServiceCommand::RevertSnapshot { vm_id, snapshot_id });
                    return;
                }
                if f.stop_sent
                    .is_some_and(|t| now.saturating_duration_since(t) >= REVERT_STOP_TIMEOUT)
                {
                    self.snapshot_revert_flow = None;
                    self.snapshot_revert_notice = Some(abandon(
                        "the VM did not shut down in time; it was left as it is",
                    ));
                }
            }
        }
    }

    fn snapshots_page(
        &mut self,
        ui: &mut egui::Ui,
        state: &ServiceState,
        vm_id: VmId,
        active: bool,
    ) {
        let Some(vm) = state.vm(vm_id) else {
            // Deleted meanwhile.
            self.snapshots_vm = None;
            return;
        };
        let request = state.snapshot.as_ref().filter(|r| r.vm_id == vm_id);
        let any_pending = state
            .snapshot
            .as_ref()
            .is_some_and(|r| r.status.is_pending());
        let running = vm
            .baselines
            .operation
            .as_ref()
            .is_some_and(snapshots::is_snapshot_op)
            || request
                .is_some_and(|r| r.status.is_pending() && !matches!(r.op, SnapshotOp::Rename(_)));
        if !running && !any_pending && self.snapshot_revert_flow.is_none() {
            self.snapshot_sent = None;
        }
        let elapsed = self
            .snapshot_sent
            .filter(|(id, _)| *id == vm_id && running)
            .map(|(_, t)| t.elapsed());
        if running || self.snapshot_revert_flow.is_some() {
            // Elapsed time and the advance of the revert flow, without
            // polling faster than once a second.
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        let list = state.snapshots.get(&vm_id);
        let env = snapshots::SnapshotsEnv {
            vm,
            list,
            request,
            any_pending,
            active,
            available: dashboard::baselines_available(state),
            elapsed,
            notice: self
                .snapshot_revert_notice
                .as_ref()
                .filter(|(id, _)| *id == vm_id)
                .map(|(_, t)| t.as_str()),
        };
        let open = |kind| SnapshotDialog {
            vm_id,
            name: vm.name.as_str().to_owned(),
            kind,
            sent: None,
            focus: true,
        };
        let find = |id: SnapshotId| {
            list.and_then(|l| l.tree.as_ref())
                .and_then(|t| t.snapshots.iter().find(|s| s.snapshot_id == id))
        };
        match snapshots::snapshots_ui(ui, &env) {
            Some(snapshots::SnapshotAction::Back) => {
                self.snapshots_vm = None;
                self.snapshot_revert_notice = None;
            }
            Some(snapshots::SnapshotAction::EmergencyStop) => {
                self.send(ServiceCommand::EmergencyStop);
            }
            Some(snapshots::SnapshotAction::Refresh) => {
                self.send(ServiceCommand::Refresh);
                self.send(ServiceCommand::ListSnapshots(vm_id));
            }
            Some(snapshots::SnapshotAction::Take) => {
                let items = list
                    .and_then(|l| l.tree.as_ref())
                    .map_or(&[][..], |t| t.snapshots.as_slice());
                let label = snapshots::auto_label(items, self.now());
                self.snapshot_dialog = Some(open(SnapshotConfirm::Take {
                    name: label.as_str().to_owned(),
                    description: String::new(),
                }));
            }
            Some(snapshots::SnapshotAction::Revert(id)) => {
                if let Some(s) = find(id) {
                    self.snapshot_dialog = Some(open(SnapshotConfirm::Revert {
                        id,
                        label: s.name.as_str().to_owned(),
                        vm_running: vm.state == VmState::Running,
                        run_id: vm.run_id,
                        shut_down: false,
                        snapshot_current: false,
                    }));
                }
            }
            Some(snapshots::SnapshotAction::Delete(id)) => {
                if let Some(s) = find(id) {
                    self.snapshot_dialog = Some(open(SnapshotConfirm::Delete {
                        id,
                        label: s.name.as_str().to_owned(),
                    }));
                }
            }
            Some(snapshots::SnapshotAction::Rename(id)) => {
                if let Some(s) = find(id) {
                    self.snapshot_dialog = Some(open(SnapshotConfirm::Rename {
                        id,
                        name: s.name.as_str().to_owned(),
                        description: s.description.clone(),
                    }));
                }
            }
            None => {}
        }
    }

    /// Confirmation of a snapshot take, revert, deletion or rename
    /// (ADR-0032); for a revert of a running VM, the offer to shut it down
    /// normally first and to take a snapshot of the current state before it.
    fn snapshot_dialog_ui(&mut self, ui: &egui::Ui) {
        let now_ts = self.now();
        let Some(d) = &mut self.snapshot_dialog else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let vm = state.vm(d.vm_id);
        let list = state.snapshots.get(&d.vm_id);
        // The label of the offered snapshot of the current state (the
        // running-VM revert), computed without the dialog borrowed.
        let keep_label = list
            .and_then(|l| l.tree.as_ref())
            .map(|t| snapshots::auto_label(&t.snapshots, now_ts));
        let request = state
            .snapshot
            .as_ref()
            .filter(|r| r.vm_id == d.vm_id && d.kind.matches(&r.op));
        let long = !matches!(d.kind, SnapshotConfirm::Rename { .. });
        let mut error = None;
        let mut pending = false;
        if let Some(sent) = d.sent {
            match request.map(|r| (&r.op, &r.status)) {
                Some((_, RequestStatus::Pending)) => {
                    let started = vm.is_some_and(|v| {
                        v.baselines
                            .operation
                            .as_ref()
                            .is_some_and(snapshots::is_snapshot_op)
                    });
                    if long && (started || sent.elapsed() >= SNAPSHOT_DIALOG_WAIT) {
                        // Running: the progress is on the page.
                        self.snapshot_dialog = None;
                        return;
                    }
                    pending = true;
                    ui.ctx().request_repaint_after(Duration::from_millis(250));
                }
                Some((_, RequestStatus::Done(_))) => {
                    self.snapshot_dialog = None;
                    return;
                }
                Some((op, RequestStatus::Failed(f))) => {
                    error = Some(snapshots::error_text(op, f));
                }
                _ => {}
            }
        }
        let other_pending = d.sent.is_none()
            && state
                .snapshot
                .as_ref()
                .is_some_and(|r| r.status.is_pending());
        let exists = |id: &SnapshotId| {
            list.and_then(|l| l.tree.as_ref())
                .and_then(|t| t.snapshots.iter().find(|s| &s.snapshot_id == id))
        };
        let refusal = match (vm, &d.kind) {
            (None, _) => Some("The VM no longer exists."),
            (Some(v), _) if v.baselines.operation.is_some() && !pending => {
                Some("A storage operation is already in progress on this VM.")
            }
            _ if other_pending => Some("Another snapshot operation is in progress."),
            (
                _,
                SnapshotConfirm::Revert { id, .. }
                | SnapshotConfirm::Delete { id, .. }
                | SnapshotConfirm::Rename { id, .. },
            ) if exists(id).is_none() => Some("This snapshot no longer exists."),
            (
                Some(v),
                SnapshotConfirm::Revert {
                    vm_running: true,
                    run_id,
                    ..
                },
            ) if v.state.has_process() && v.run_id != *run_id => {
                Some("The VM was restarted meanwhile: open the revert again.")
            }
            _ => None,
        };
        if error.is_none() && !pending {
            error = refusal.map(str::to_owned);
        }
        let name = &d.name;
        let running_now = vm.is_some_and(|v| v.state == VmState::Running);
        let spec: Option<dialogs::ConfirmSpec> = match &d.kind {
            SnapshotConfirm::Take { .. } => Some(dialogs::ConfirmSpec {
                title: format!("Take a snapshot of \u{201c}{name}\u{201d}?"),
                body: if running_now {
                    "The VM is running: the disk is snapshotted exactly as the guest left it, like a power cut at that instant (crash-consistent), with a thumbnail of the screen. No memory is saved: reverting to it later restarts the guest from that disk state with a normal boot."
                        .to_owned()
                } else {
                    "The VM's disk state is saved exactly as it is, with a copy of the UEFI variables. No memory is saved: reverting to it later restarts the guest from that disk state with a normal boot."
                        .to_owned()
                },
                confirm: "Take snapshot".into(),
                destructive: false,
            }),
            SnapshotConfirm::Revert { label, .. } => Some(dialogs::ConfirmSpec {
                title: format!("Revert to \u{201c}{label}\u{201d}?"),
                body: format!(
                    "The current state of \u{201c}{name}\u{201d} is discarded: everything written to the disk since \u{201c}{label}\u{201d} and the UEFI variables changed after it are lost. No memory is ever saved by a snapshot: the guest starts from the saved disk state with a normal boot. Other snapshots are kept.\n\nThis cannot be undone."
                ),
                confirm: if running_now {
                    "Shut down and revert".into()
                } else {
                    "Discard and revert".into()
                },
                destructive: true,
            }),
            SnapshotConfirm::Delete { label, .. } => Some(dialogs::ConfirmSpec {
                title: format!("Delete snapshot \u{201c}{label}\u{201d}?"),
                body: "It is removed from the VM's disk. The other snapshots keep their state; the snapshot the VM's current state descends from stays usable. This cannot be undone."
                    .to_owned(),
                confirm: "Delete snapshot".into(),
                destructive: true,
            }),
            SnapshotConfirm::Rename { .. } => Some(dialogs::ConfirmSpec {
                title: format!("Rename snapshot of \u{201c}{name}\u{201d}?"),
                body: "A new name and description. Nothing changes on the disk.".to_owned(),
                confirm: "Save".into(),
                destructive: false,
            }),
        };
        let Some(spec) = spec else { return };
        let mut ready = true;
        let content = |ui: &mut egui::Ui, enabled: bool| {
            ui.add_space(10.0);
            match &mut d.kind {
                SnapshotConfirm::Take { name, description }
                | SnapshotConfirm::Rename {
                    name, description, ..
                } => {
                    ui.label(egui::RichText::new("Name").color(theme::Palette::of(ui).text_weak));
                    ui.add_enabled(
                        enabled,
                        egui::TextEdit::singleline(name).desired_width(360.0),
                    );
                    ui.label(
                        egui::RichText::new("Description (optional)")
                            .color(theme::Palette::of(ui).text_weak),
                    );
                    ui.add_enabled(
                        enabled,
                        egui::TextEdit::multiline(description)
                            .desired_width(360.0)
                            .desired_rows(2),
                    );
                    ready = Label::parse(name.trim()).is_ok()
                        && lightvm_proto::ipc::validate_snapshot_description(description).is_ok();
                }
                SnapshotConfirm::Revert {
                    shut_down,
                    snapshot_current,
                    ..
                } if running_now => {
                    ui.label(
                        egui::RichText::new("The VM is running, so first:")
                            .color(theme::Palette::of(ui).text_weak),
                    );
                    ui.add_enabled(
                        enabled,
                        egui::Checkbox::new(
                            shut_down,
                            format!("Shut down \u{201c}{name}\u{201d} normally first (a revert needs the VM off)"),
                        ),
                    );
                    if let Some(keep) = &keep_label {
                        ui.add_enabled(
                            enabled,
                            egui::Checkbox::new(
                                snapshot_current,
                                format!(
                                    "Before that, take a snapshot of the current state (\u{201c}{}\u{201d})",
                                    keep.as_str()
                                ),
                            ),
                        );
                    }
                    ready = *shut_down;
                }
                _ => {}
            }
            ready
        };
        let focus = std::mem::replace(&mut d.focus, false);
        let choice = dialogs::confirm_custom(
            ui,
            "lightvm_snapshot_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
            content,
        );
        match choice {
            Some(dialogs::Choice::Cancel) => self.snapshot_dialog = None,
            Some(dialogs::Choice::Confirm) if refusal.is_none() => {
                let vm_id = d.vm_id;
                let now = Instant::now();
                match &d.kind {
                    SnapshotConfirm::Take { name, description } => {
                        let Ok(name) = Label::parse(name.trim()) else {
                            return;
                        };
                        let description = description.clone();
                        d.sent = Some(now);
                        self.snapshot_sent = Some((vm_id, now));
                        self.send(ServiceCommand::TakeSnapshot {
                            vm_id,
                            name,
                            description,
                        });
                    }
                    SnapshotConfirm::Revert {
                        id,
                        label,
                        vm_running,
                        run_id,
                        snapshot_current,
                        ..
                    } => {
                        let id = *id;
                        let label = label.clone();
                        let running = *vm_running;
                        let snapshot_current = *snapshot_current;
                        let run_id = *run_id;
                        if running {
                            let Some(run_id) = run_id else { return };
                            self.snapshot_revert_notice = None;
                            // The dialog closes: the flow (offered snapshot,
                            // orderly stop, then the revert) runs on the page.
                            if snapshot_current {
                                let keep = keep_label
                                    .clone()
                                    .unwrap_or_else(|| snapshots::auto_label(&[], now_ts));
                                self.send(ServiceCommand::TakeSnapshot {
                                    vm_id,
                                    name: keep,
                                    description: format!(
                                        "Before the revert to \u{201c}{label}\u{201d}"
                                    ),
                                });
                                self.snapshot_revert_flow = Some(RevertFlow {
                                    vm_id,
                                    snapshot_id: id,
                                    run_id,
                                    waiting: RevertWaiting::Take,
                                    stop_sent: None,
                                });
                            } else {
                                self.send(ServiceCommand::StopRun { vm_id, run_id });
                                self.snapshot_revert_flow = Some(RevertFlow {
                                    vm_id,
                                    snapshot_id: id,
                                    run_id,
                                    waiting: RevertWaiting::Stop,
                                    stop_sent: Some(now),
                                });
                            }
                            self.snapshot_dialog = None;
                            return;
                        }
                        d.sent = Some(now);
                        self.snapshot_sent = Some((vm_id, now));
                        self.send(ServiceCommand::RevertSnapshot {
                            vm_id,
                            snapshot_id: id,
                        });
                    }
                    SnapshotConfirm::Delete { id, .. } => {
                        let id = *id;
                        d.sent = Some(now);
                        self.send(ServiceCommand::DeleteSnapshot {
                            vm_id,
                            snapshot_id: id,
                        });
                    }
                    SnapshotConfirm::Rename {
                        id,
                        name,
                        description,
                    } => {
                        let id = *id;
                        let Ok(name) = Label::parse(name.trim()) else {
                            return;
                        };
                        let description = description.clone();
                        d.sent = Some(now);
                        self.send(ServiceCommand::RenameSnapshot {
                            vm_id,
                            snapshot_id: id,
                            name,
                            description,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// Confirmation of a baseline creation, restore or deletion; for a
    /// creation refused because of an unclean stop, the explicit consent
    /// to a baseline marked "not verified".
    fn baseline_dialog_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.baseline_dialog else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let vm = state.vm(d.vm_id);
        let list = state.baselines.get(&d.vm_id);
        let request = state
            .baseline
            .as_ref()
            .filter(|r| r.vm_id == d.vm_id && d.kind.matches(&r.op));
        let long = !matches!(d.kind, BaselineConfirm::Delete { .. });
        let mut error = None;
        let mut pending = false;
        if let Some(sent) = d.sent {
            match request.map(|r| (&r.op, &r.status)) {
                Some((_, RequestStatus::Pending)) => {
                    let started = vm.is_some_and(|v| v.baselines.operation.is_some());
                    if long && (started || sent.elapsed() >= BASELINE_DIALOG_WAIT) {
                        // Running: the progress is on the page.
                        self.baseline_dialog = None;
                        return;
                    }
                    pending = true;
                    ui.ctx().request_repaint_after(Duration::from_millis(250));
                }
                Some((_, RequestStatus::Done(_))) => {
                    self.baseline_dialog = None;
                    return;
                }
                Some((op, RequestStatus::Failed(f))) => {
                    if matches!(d.kind, BaselineConfirm::Create { .. })
                        && f.reason() == Some("unclean_shutdown")
                        && !d.unclean
                    {
                        d.unclean = true;
                        d.consent = false;
                        d.sent = None;
                        d.focus = true;
                    } else {
                        error = Some(baselines::error_text(op, f));
                    }
                }
                _ => {}
            }
        }
        let other_pending = d.sent.is_none()
            && state
                .baseline
                .as_ref()
                .is_some_and(|r| r.status.is_pending());
        let exists =
            |id: &BaselineId| list.and_then(|l| l.items.iter().find(|b| b.baseline_id == *id));
        let refusal = match (vm, &d.kind) {
            (None, _) => Some("The VM no longer exists."),
            (Some(v), _) if v.state != VmState::Stopped => {
                Some("The VM is no longer stopped: stop it first.")
            }
            (Some(v), _) if v.baselines.operation.is_some() && !pending => {
                Some("A baseline operation is already in progress on this VM.")
            }
            _ if other_pending => Some("Another baseline operation is in progress."),
            (_, BaselineConfirm::Restore { id, .. }) if exists(id).is_none() => {
                Some("This baseline no longer exists.")
            }
            (_, BaselineConfirm::Delete { id, .. }) if !pending => match exists(id) {
                None => Some("This baseline no longer exists."),
                Some(b) => baselines::delete_refusal(b),
            },
            _ => None,
        };
        if error.is_none() && !pending {
            error = refusal.map(str::to_owned);
        }
        let name = &d.name;
        let spec = match &d.kind {
            BaselineConfirm::Create { label } if d.unclean => dialogs::ConfirmSpec {
                title: format!("\u{201c}{name}\u{201d} was not shut down cleanly"),
                body: format!(
                    "Its last run ended with a forced stop, an Emergency Stop, a crash or a supervisor restart, so its disk may hold incomplete writes. The safer way is to start it, shut it down from the guest, then create the baseline.\n\nIf you go on, \u{201c}{}\u{201d} is created anyway and stays marked Not verified.",
                    label.as_str()
                ),
                confirm: "Create unverified baseline".into(),
                destructive: false,
            },
            BaselineConfirm::Create { label } => dialogs::ConfirmSpec {
                title: format!("Create a baseline of \u{201c}{name}\u{201d}?"),
                body: format!(
                    "LightVM copies the VM's disk and UEFI variables, with the VM off, into the read-only baseline \u{201c}{}\u{201d}. Restoring it later brings the VM back to this state.\n\nFor a large disk this takes several minutes. The VM cannot be started meanwhile; only the Emergency Stop cancels the operation.",
                    label.as_str()
                ),
                confirm: "Create baseline".into(),
                destructive: false,
            },
            BaselineConfirm::Restore { label, .. } => {
                let since = match vm.and_then(|v| v.baselines.current) {
                    Some(cur) => format!(
                        "everything written since the restore of \u{201c}{}\u{201d}",
                        baselines::label_of(list, cur).unwrap_or("the current baseline")
                    ),
                    None => "the VM's disk as it is now".to_owned(),
                };
                dialogs::ConfirmSpec {
                    title: format!("Restore \u{201c}{label}\u{201d}?"),
                    body: format!(
                        "The current state of \u{201c}{name}\u{201d} is discarded: {since} and its UEFI variables are lost. The VM gets a fresh copy of the baseline and the guest starts from a normal boot (no RAM state is restored). Other baselines are kept.\n\nTo keep the current state, create a baseline of it first. This cannot be undone."
                    ),
                    confirm: "Discard and restore".into(),
                    destructive: true,
                }
            }
            BaselineConfirm::Delete { label, size, .. } => dialogs::ConfirmSpec {
                title: format!("Delete baseline \u{201c}{label}\u{201d}?"),
                body: format!(
                    "The baseline ({}) is removed from the host permanently. The VM's current state does not change. This cannot be undone.",
                    dashboard::format_bytes(*size)
                ),
                confirm: "Delete baseline".into(),
                destructive: true,
            },
        };
        let consent = if d.unclean {
            Some((
                "I understand: the baseline is marked Not verified",
                &mut d.consent,
            ))
        } else {
            None
        };
        let focus = std::mem::replace(&mut d.focus, false);
        let choice = dialogs::confirm_with_consent(
            ui,
            "lightvm_baseline_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
            consent,
        );
        match choice {
            Some(dialogs::Choice::Cancel) => self.baseline_dialog = None,
            Some(dialogs::Choice::Confirm) if refusal.is_none() => {
                let vm_id = d.vm_id;
                let command = match &d.kind {
                    BaselineConfirm::Create { label } => ServiceCommand::CreateBaseline {
                        vm_id,
                        label: label.clone(),
                        allow_unclean: d.unclean,
                    },
                    BaselineConfirm::Restore { id, .. } => ServiceCommand::RestoreBaseline {
                        vm_id,
                        baseline_id: *id,
                    },
                    BaselineConfirm::Delete { id, .. } => ServiceCommand::DeleteBaseline {
                        vm_id,
                        baseline_id: *id,
                    },
                };
                let now = Instant::now();
                d.sent = Some(now);
                if long {
                    self.baseline_sent = Some((vm_id, now));
                }
                self.send(command);
            }
            _ => {}
        }
    }

    fn wizard_page(&mut self, ui: &mut egui::Ui, state: &ServiceState, active: bool) {
        let Some(w) = &mut self.wizard else { return };
        if w.submitted && matches!(state.create, RequestStatus::Done(_)) {
            self.wizard = None;
            return;
        }
        let names: Vec<String> = state
            .vms
            .iter()
            .map(|v| v.name.as_str().to_owned())
            .collect();
        let env = wizard::WizardEnv {
            media: &state.media,
            import: &state.import,
            create: &state.create,
            host: state.host,
            existing_names: &names,
            file_picker: self.file_picker.is_some(),
            active,
        };
        match wizard::wizard_ui(ui, w, &env) {
            Some(wizard::WizardAction::Cancel) => self.wizard = None,
            Some(wizard::WizardAction::RefreshMedia) => self.send(ServiceCommand::ListMedia),
            Some(wizard::WizardAction::Create(config)) => self.send(ServiceCommand::Create(config)),
            Some(wizard::WizardAction::Import(kind)) => {
                let title = match kind {
                    MediaKind::InstallIso => "Choose the installation ISO",
                    MediaKind::DataIso => "Choose the data ISO",
                };
                let Some(path) = self.file_picker.as_mut().and_then(|f| f(title)) else {
                    return;
                };
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "ISO".into());
                let origin = declared_origin(&file);
                if let Some(w) = &mut self.wizard {
                    w.importing = Some((kind, file));
                }
                self.send(ServiceCommand::ImportMedia { path, kind, origin });
            }
            None => {}
        }
    }

    /// Native picker of the `.vmx`, then the preview request. With `open`
    /// the page opens even when nothing is chosen (it then says why no
    /// picker is available, or offers to choose again).
    fn choose_vmx(&mut self, open: bool) {
        let path = self
            .vmx_picker
            .as_mut()
            .and_then(|f| f("Choose the .vmx file of the VMware VM"));
        match path {
            Some(path) => {
                self.vmware_import = Some(vmware_import::ImportFlow::new(path.clone()));
                self.send(ServiceCommand::PreviewVmwareImport(path));
            }
            None if open && self.vmx_picker.is_none() => {
                // No picker on this platform: the page explains it.
                self.vmware_import =
                    Some(vmware_import::ImportFlow::new(std::path::PathBuf::new()));
            }
            None => {}
        }
    }

    fn vmware_import_page(&mut self, ui: &mut egui::Ui, state: &ServiceState, active: bool) {
        let Some(flow) = &mut self.vmware_import else {
            return;
        };
        let names: Vec<String> = state
            .vms
            .iter()
            .map(|v| v.name.as_str().to_owned())
            .collect();
        // A flow without a file (no picker): show the page without the
        // state of an earlier preview.
        let idle = service::VmwareImportState::default();
        let vmware = if flow.path.as_os_str().is_empty() {
            &idle
        } else {
            &state.vmware
        };
        let env = vmware_import::ImportEnv {
            state: vmware,
            existing_names: &names,
            active,
            file_picker: self.vmx_picker.is_some(),
        };
        match vmware_import::import_ui(ui, flow, &env) {
            Some(vmware_import::ImportAction::Close) => self.vmware_import = None,
            Some(vmware_import::ImportAction::ChooseOther) => self.choose_vmx(false),
            Some(vmware_import::ImportAction::Retry) => {
                let path = flow.path.clone();
                self.send(ServiceCommand::PreviewVmwareImport(path));
            }
            Some(vmware_import::ImportAction::Import {
                path,
                name,
                display,
            }) => self.send(ServiceCommand::ImportVmware {
                path,
                name,
                display,
            }),
            Some(vmware_import::ImportAction::CancelImport) => {
                self.send(ServiceCommand::CancelVmwareImport);
            }
            None => {}
        }
    }

    fn confirm_dialog_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.dialog else { return };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let vm = state.vm(d.kind.vm_id());
        let (spec, mut error, pending) = match d.kind {
            Confirm::ForceStop(_) => (
                dialogs::ConfirmSpec {
                    title: format!("Force stop \u{201c}{}\u{201d}?", d.name),
                    body: "The VM is terminated at once, like pulling the plug: unsaved work in the guest is lost and its disk may need a check at the next boot.".into(),
                    confirm: "Force stop".into(),
                    destructive: true,
                },
                None,
                false,
            ),
            Confirm::Reset(_) => (
                dialogs::ConfirmSpec {
                    title: format!("Reset \u{201c}{}\u{201d}?", d.name),
                    body: "The guest restarts at once, like pressing the reset button: unsaved work in the guest is lost.".into(),
                    confirm: "Reset".into(),
                    destructive: true,
                },
                None,
                false,
            ),
            Confirm::DetachInstall(_) => (
                dialogs::ConfirmSpec {
                    title: format!("Remove the installation media from “{}”?", d.name),
                    body: "The VM will boot from its own disk. The ISO stays in the registry and can be attached again from Edit.".into(),
                    confirm: "Remove media".into(),
                    destructive: false,
                },
                None,
                false,
            ),
            Confirm::Delete(_) => (
                dialogs::ConfirmSpec {
                    title: format!("Delete \u{201c}{}\u{201d}?", d.name),
                    body: format!(
                        "The VM \u{201c}{}\u{201d}, its virtual disk and its run files are removed permanently. Approved ISO media stay in the registry. This cannot be undone.",
                        d.name
                    ),
                    confirm: "Delete VM".into(),
                    destructive: true,
                },
                match (&state.delete, d.sent) {
                    (RequestStatus::Failed(f), true) => Some(if f.unsupported {
                        "This supervisor cannot delete VMs yet.".to_owned()
                    } else {
                        f.message.clone()
                    }),
                    _ => None,
                },
                d.sent && state.delete.is_pending(),
            ),
        };
        if d.sent
            && let (Confirm::Delete(id), RequestStatus::Done(done)) = (d.kind, &state.delete)
            && *done == id
        {
            self.dialog = None;
            return;
        }
        // The action is no longer possible (state changed meanwhile).
        let still_valid = match (d.kind, vm) {
            (Confirm::Delete(_), Some(v)) => {
                v.state == VmState::Stopped && v.baselines.operation.is_none()
            }
            // The run confirmed only: a VM started again meanwhile is a
            // different run, not forced off without a new confirmation.
            (Confirm::ForceStop(_), Some(v)) => v.state.has_process() && v.run_id == d.run_id,
            (Confirm::Reset(_), Some(v)) => v.state == VmState::Running && v.run_id == d.run_id,
            (Confirm::DetachInstall(_), Some(v)) => {
                v.state == VmState::Stopped
                    && v.config.install_media.is_some()
                    && v.baselines.operation.is_none()
            }
            (_, None) => false,
        };
        if !still_valid && !pending && error.is_none() {
            error = Some("The VM state changed: the action is no longer possible.".into());
        }
        let focus = std::mem::replace(&mut d.focus, false);
        let choice = dialogs::confirm(
            ui,
            "lightvm_confirm_dialog",
            &spec,
            pending,
            error.as_deref(),
            focus,
        );
        match choice {
            Some(dialogs::Choice::Cancel) => self.dialog = None,
            Some(dialogs::Choice::Confirm) if still_valid => {
                let command = match d.kind {
                    Confirm::ForceStop(id) => ServiceCommand::ForceStop {
                        vm_id: id,
                        run_id: d.run_id,
                    },
                    Confirm::Reset(id) => match d.run_id {
                        Some(run_id) => ServiceCommand::Reset { vm_id: id, run_id },
                        None => return,
                    },
                    Confirm::Delete(id) => ServiceCommand::Delete {
                        vm_id: id,
                        revision: d.revision,
                    },
                    Confirm::DetachInstall(id) => {
                        let Some(v) = vm else { return };
                        let mut config = v.config.clone();
                        config.install_media = None;
                        ServiceCommand::UpdateVm {
                            vm_id: id,
                            revision: v.config_revision,
                            config: Box::new(config),
                        }
                    }
                };
                if matches!(d.kind, Confirm::Delete(_)) {
                    d.sent = true;
                } else {
                    self.dialog = None;
                }
                self.send(command);
            }
            _ => {}
        }
    }

    /// Close request of the window: with running VMs it is cancelled and
    /// the close dialog opens.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        // A catalog move is cancelled from its dialog, not by closing.
        if self.catalog_move_running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            return;
        }
        let running = self.reachable_active_vms().len();
        if running == 0 && self.closing.is_none() {
            self.allow_close = true;
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.closing.is_none() {
            self.closing = Some(Closing::Ask { focus: true });
        }
    }

    /// VMs with a process according to a supervisor that can still be
    /// asked to stop them: none if it is gone, terminated or disconnected
    /// (its state is stale, and the VMs follow its UI loss policy anyway).
    fn reachable_active_vms(&mut self) -> Vec<VmId> {
        let Some(s) = &mut self.service else {
            return Vec::new();
        };
        if !s.is_alive() {
            return Vec::new();
        }
        let st = self.state().unwrap_or_default();
        if !st.connected {
            return Vec::new();
        }
        st.active_vms().map(|v| v.vm_id).collect()
    }

    /// The close goes on without (more) waiting: supervisor shut down in
    /// the background, then the window closes.
    fn finish_close(&mut self, ctx: &egui::Context) {
        self.restart_in_background(ctx, false);
        self.closing = Some(Closing::Finishing);
        ctx.request_repaint();
    }

    fn close_dialog_ui(&mut self, ui: &egui::Ui, ctx: &egui::Context) {
        let Some(c) = self.closing.clone() else {
            return;
        };
        let reachable = self.reachable_active_vms();
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        let view = match &c {
            Closing::Ask { .. } => {
                if reachable.is_empty() {
                    // Stopped meanwhile, or the supervisor is gone: nothing
                    // to ask, the close requested by the user goes on.
                    self.finish_close(ctx);
                    return;
                }
                let names: Vec<String> = reachable
                    .iter()
                    .filter_map(|id| state.vm(*id))
                    .map(|v| v.name.as_str().to_owned())
                    .collect();
                dialogs::CloseView::Ask { names }
            }
            Closing::Stopping { started, vms } => {
                let rows: Vec<(String, String)> = vms
                    .iter()
                    .filter_map(|id| state.vm(*id))
                    .map(|v| {
                        (
                            v.name.as_str().to_owned(),
                            dashboard::vm_state(v.state).0.into(),
                        )
                    })
                    .collect();
                // Only the VMs active when the user confirmed are waited for.
                let active = vms.iter().filter(|id| reachable.contains(id)).count();
                let elapsed = started.elapsed();
                if active == 0 || elapsed > CLOSE_STOP_TIMEOUT {
                    self.finish_close(ctx);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
                dialogs::CloseView::Stopping {
                    stopped: vms.len().saturating_sub(active),
                    vms: rows,
                    elapsed_s: elapsed.as_secs(),
                }
            }
            Closing::Finishing => {
                if self.restart.is_none() {
                    self.allow_close = true;
                    self.closing = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                dialogs::CloseView::Finishing
            }
        };
        let focus = matches!(c, Closing::Ask { focus: true });
        if focus {
            self.closing = Some(Closing::Ask { focus: false });
        }
        match dialogs::close(ui, &view, focus) {
            Some(dialogs::Choice::Cancel) => self.closing = None,
            Some(dialogs::Choice::Confirm) => {
                self.close_console();
                for id in &reachable {
                    self.send(ServiceCommand::Stop(*id));
                }
                self.closing = Some(Closing::Stopping {
                    started: Instant::now(),
                    vms: reachable,
                });
                ctx.request_repaint();
            }
            None => {}
        }
    }

    /// Opens the console of a VM: Console page, ticket requested.
    fn open_console(&mut self, vm_id: VmId) {
        if self.console_vm != Some(vm_id) {
            self.close_console();
        }
        self.console_vm = Some(vm_id);
        self.section = Section::Console;
        self.request_ticket(vm_id);
    }

    fn request_ticket(&mut self, vm_id: VmId) {
        self.ticket_request = Some(TicketRequest {
            vm_id,
            at: Instant::now(),
        });
        self.send(ServiceCommand::OpenConsole(vm_id));
    }

    /// Closes the display session and sends the key releases; a console
    /// ticket received and not used is discarded.
    fn close_console(&mut self) {
        let closed = self.live.as_ref().map(|l| l.target().vm_id);
        if let Some(mut live) = self.live.take() {
            for b in live.close() {
                self.send(ServiceCommand::Input(b));
            }
        }
        if let Some(s) = &self.service {
            s.discard_console_ticket();
            if let Some(vm_id) = closed {
                s.clear_input_error(vm_id);
            }
        }
        self.invalidate_state();
        self.ticket_request = None;
    }

    /// Sends the next console input batch, if due, and schedules a frame
    /// for the following one (at most one batch every
    /// [`console::INPUT_INTERVAL`]).
    fn flush_console_input(&mut self, ctx: &egui::Context) {
        let Some(live) = &mut self.live else { return };
        let now = Instant::now();
        let batch = live.take_input(now);
        let next = live.next_input_in(now);
        if let Some(b) = batch {
            self.send(ServiceCommand::Input(b));
        }
        if let Some(d) = next {
            ctx.request_repaint_after(d);
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen != on {
            self.fullscreen = on;
            self.fullscreen_request = Some((on, Instant::now()));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
    }

    /// Follows the full screen state reported by the OS (the user can leave
    /// full screen with OS shortcuts, or the OS can refuse it); a change
    /// requested by LightVM wins until the OS reports it or
    /// [`FULLSCREEN_SETTLE`] passes.
    fn sync_fullscreen(&mut self, ctx: &egui::Context) {
        let Some(os) = ctx.input(|i| i.viewport().fullscreen) else {
            return;
        };
        if let Some((wanted, at)) = self.fullscreen_request {
            if os != wanted && at.elapsed() < FULLSCREEN_SETTLE {
                ctx.request_repaint_after(FULLSCREEN_SETTLE);
                return;
            }
            self.fullscreen_request = None;
        }
        self.fullscreen = os;
    }

    /// Keeps the console aligned with the VM: connects with a fresh ticket
    /// when the VM runs (new run after a restart, or after a display
    /// disconnection), closes it when the VM stops.
    fn sync_console(&mut self, ctx: &egui::Context, state: &ServiceState) {
        let Some(vm_id) = self.console_vm else { return };
        let Some(vm) = state.vm(vm_id) else {
            self.close_console();
            self.console_vm = None;
            return;
        };
        if vm.state != VmState::Running {
            if self.live.is_some() {
                self.close_console();
            }
            return;
        }
        if let Some(live) = &self.live {
            let same_run = Some(live.target().run_id) == vm.run_id;
            let since = live.disconnected_at().map(|t| t.elapsed());
            let dropped = since.is_some_and(|e| e >= RECONNECT_AFTER);
            if same_run && !dropped {
                // Display dropped: the reconnection happens without input.
                if let Some(e) = since {
                    ctx.request_repaint_after(RECONNECT_AFTER.saturating_sub(e));
                }
                return;
            }
            self.close_console();
        }
        let Some(srv) = &self.service else { return };
        if let Some(ticket) = srv.take_console_ticket(vm_id)
            && Some(ticket.run_id) == vm.run_id
        {
            self.live = Some(LiveConsole::from_ticket(
                vm.name.as_str(),
                ticket,
                self.console_max_fps,
            ));
            return;
        }
        let failed_for_good = matches!(
            &state.console,
            Some(r) if r.vm_id == vm_id && matches!(&r.status, RequestStatus::Failed(f) if f.unsupported)
        );
        let pending =
            matches!(&state.console, Some(r) if r.vm_id == vm_id && r.status.is_pending());
        let due = self
            .ticket_request
            .is_none_or(|r| r.vm_id != vm_id || r.at.elapsed() >= TICKET_RETRY);
        if !failed_for_good && !pending && due {
            self.request_ticket(vm_id);
        }
    }

    fn console_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, chords: console::Chords) {
        if let Some(state) = self.state() {
            self.sync_console(ctx, &state);
            if self.live.is_none() && self.console_vm.is_some() {
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
        let state = self.state();
        // Only the refused input of this console (VM and run).
        let target = self
            .live
            .as_ref()
            .map(|l| (l.target().vm_id, l.target().run_id));
        let mut input_error = state
            .as_ref()
            .and_then(|s| s.input_error.as_ref())
            .filter(|e| target == Some((e.vm_id, e.run_id)))
            .map(|e| e.message.clone());
        if let Some((name, vm_id)) = self
            .live
            .as_ref()
            .map(|l| (l.target().vm_name.clone(), l.target().vm_id))
        {
            // F4: while an agent session owns the input, nothing typed or
            // clicked here is sent; "Take control" gives it back.
            let agent = state
                .as_ref()
                .filter(|s| s.agents.agent_has_input(vm_id))
                .map(|s| s.agents.input_session(vm_id).map(|x| x.client_name.clone()));
            if let Some(live) = &mut self.live {
                live.set_input_blocked(agent.is_some());
            }
            // The guest stopped on a vCPU error: input cannot reach it; the
            // notice says why instead of the bare refusal code.
            if let Some(h) = state
                .as_ref()
                .and_then(|s| s.vms.iter().find(|v| v.vm_id == vm_id))
                .and_then(dashboard::vcpu_halt)
            {
                input_error = Some(dashboard::vcpu_halt_text(&h));
            }
            if !self.fullscreen {
                theme::page_header(
                    ui,
                    &format!("Console · {name}"),
                    &format!(
                        "Click the screen to capture keyboard and mouse for the guest; {} gives them back.",
                        console::RELEASE_CHORD
                    ),
                    |_| {},
                );
                if let Some((text, tone)) = &self.screenshot_status {
                    theme::notice(ui, text, *tone);
                    ui.add_space(6.0);
                }
            }
            if let Some(client) = &agent {
                input_error = None;
                let taking = state.as_ref().is_some_and(|s| {
                    s.agents.control.as_ref().is_some_and(|c| {
                        c.status.is_pending()
                            && c.op == service::AgentOp::InputOwner(vm_id, InputOwner::User)
                    })
                });
                if agent_input_notice(ui, client.as_deref(), taking) {
                    self.send(ServiceCommand::SetInputOwner {
                        vm_id,
                        owner: InputOwner::User,
                    });
                }
                ui.add_space(4.0);
            }
            self.tools_clipboard_ui(ui, ctx, vm_id, state.as_deref(), agent.is_none());
            let tools_busy = self.tools_clipboard_pending.is_some()
                || self.clipboard_sync.busy()
                || state
                    .as_ref()
                    .and_then(|s| s.tools.as_ref())
                    .is_some_and(|(_, _, s)| s.is_pending());
            self.transfers.ui(ui, state.as_deref(), vm_id, tools_busy);
            if !self.dialog_open() && self.closing.is_none() {
                self.transfers.drops(
                    ctx,
                    ui.available_rect_before_wrap(),
                    state.as_deref(),
                    vm_id,
                    tools_busy,
                );
            }
            let (layout, fullscreen) = (self.settings.keyboard_layout, self.fullscreen);
            let modal_open = self.dialog_open() || self.closing.is_some();
            let action = self.live.as_mut().and_then(|live| {
                live.ui(
                    ui,
                    layout,
                    fullscreen,
                    chords,
                    input_error.as_deref(),
                    modal_open,
                )
            });
            self.flush_console_input(ctx);
            match action {
                Some(ConsoleAction::ToggleFullscreen) => self.set_fullscreen(ctx, !fullscreen),
                Some(ConsoleAction::ExitFullscreen) => self.set_fullscreen(ctx, false),
                Some(ConsoleAction::Screenshot) => self.save_screenshot(),
                Some(ConsoleAction::SetLayout(l)) => {
                    self.settings.keyboard_layout = l;
                    self.save_settings();
                }
                None => {}
            }
            return;
        }
        if self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
        if self.simulated_console && self.console_vm.is_none() {
            self.simulated_console_ui(ui, ctx);
            return;
        }
        page(ui, "lightvm_page_console", |ui| {
            theme::page_header(ui, "Console", CONSOLE_DESCRIPTION, |_| {});
            self.console_empty_state(ui, state.as_deref());
        });
    }

    fn tools_clipboard_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        vm_id: VmId,
        state: Option<&ServiceState>,
        input_owned: bool,
    ) {
        use lightvm_proto::{ToolCall, ToolReply};
        use service::RequestStatus;
        let Some(state) = state else {
            self.tools_clipboard_pending = None;
            return;
        };
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        let Some(vm) = state
            .vm(vm_id)
            .filter(|v| v.config.tools.enabled && v.config.tools.clipboard)
        else {
            self.tools_clipboard_pending = None;
            return;
        };
        let Some(run_id) = vm.run_id else {
            self.tools_clipboard_pending = None;
            return;
        };
        if let Some(target) = self.tools_clipboard_pending {
            if target != (vm_id, run_id) || !focused || !input_owned {
                self.tools_clipboard_pending = None;
            } else if let Some((v, r, status)) = &state.tools
                && (*v, *r) == target
            {
                match status {
                    RequestStatus::Done(ToolReply::Clipboard { text }) => {
                        let result =
                            arboard::Clipboard::new().and_then(|mut c| c.set_text(text.clone()));
                        self.screenshot_status = Some((
                            if result.is_ok() {
                                "Guest text copied to the host clipboard".into()
                            } else {
                                "Host clipboard is unavailable".into()
                            },
                            if result.is_ok() {
                                theme::Tone::Positive
                            } else {
                                theme::Tone::Warning
                            },
                        ));
                        self.tools_clipboard_pending = None;
                    }
                    RequestStatus::Failed(f) => {
                        self.screenshot_status = Some((f.message.clone(), theme::Tone::Warning));
                        self.tools_clipboard_pending = None;
                    }
                    _ => (),
                }
            }
        }
        let pending = state.tools.as_ref().is_some_and(|(_, _, s)| s.is_pending());
        ui.add_enabled_ui(
            focused
                && input_owned
                && !pending
                && !self.transfers.busy()
                && !self.clipboard_sync.enabled
                && !self.clipboard_sync.busy(),
            |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("Host text → Guest clipboard")
                        .on_hover_text("Read the host clipboard once and send text to this guest")
                        .clicked()
                    {
                        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                            Ok(text) if text.len() <= 64 * 1024 && !text.contains('\0') => {
                                self.send(ServiceCommand::GuestTools {
                                    vm_id,
                                    run_id,
                                    call: ToolCall::ClipboardWrite { text },
                                });
                            }
                            _ => {
                                self.screenshot_status = Some((
                                    "Host clipboard text is unavailable or exceeds 64 KiB".into(),
                                    theme::Tone::Warning,
                                ));
                            }
                        }
                    }
                    if ui
                        .button("Guest text → Host clipboard")
                        .on_hover_text(
                            "Read guest clipboard text once and replace the host clipboard",
                        )
                        .clicked()
                    {
                        self.tools_clipboard_pending = Some((vm_id, run_id));
                        self.send(ServiceCommand::GuestTools {
                            vm_id,
                            run_id,
                            call: ToolCall::ClipboardRead,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Host image → Guest clipboard").clicked() {
                        self.transfers
                            .clipboard_image((vm_id, run_id), false, ctx.clone());
                    }
                    if ui.button("Guest image → Host clipboard").clicked() {
                        self.transfers
                            .clipboard_image((vm_id, run_id), true, ctx.clone());
                    }
                });
            },
        );
        if ui.checkbox(&mut self.clipboard_sync.enabled, "Synchronize clipboard text while the guest screen has focus")
            .on_hover_text("Opt in for this console. Text is exchanged automatically only while it captures input, with no file transfer active.")
            .changed() {
                self.clipboard_sync.error = None;
            }
        if let Some(error) = &self.clipboard_sync.error {
            ui.label(error);
        }
    }

    /// Console page without an open display: why, and what to do.
    fn console_empty_state(&mut self, ui: &mut egui::Ui, state: Option<&ServiceState>) {
        let Some(state) = state else {
            theme::card(ui, |ui| {
                empty_state(
                    ui,
                    "No console connected.",
                    "The supervisor is not running: start it from Settings.",
                );
            });
            return;
        };
        if let Some(vm) = self.console_vm.and_then(|id| state.vm(id)) {
            let name = vm.name.as_str();
            theme::card(ui, |ui| {
                if vm.state != VmState::Running {
                    let (s, _) = dashboard::vm_state(vm.state);
                    empty_state(
                        ui,
                        &format!("{name} is not running ({}).", s.to_lowercase()),
                        "The console reconnects by itself when the VM is running again.",
                    );
                    return;
                }
                match &state.console {
                    Some(r) if r.vm_id == vm.vm_id => match &r.status {
                        RequestStatus::Failed(f) if f.unsupported => empty_state(
                            ui,
                            "The console is not available.",
                            "This supervisor does not provide display tickets yet.",
                        ),
                        RequestStatus::Failed(f) => {
                            empty_state(ui, "The console could not be opened.", &f.message);
                        }
                        _ => {
                            ui.vertical_centered(|ui| {
                                ui.add_space(14.0);
                                ui.spinner();
                                ui.label(format!("Opening the console of {name}…"));
                                ui.add_space(14.0);
                            });
                        }
                    },
                    _ => {
                        ui.vertical_centered(|ui| {
                            ui.add_space(14.0);
                            ui.spinner();
                            ui.label(format!("Opening the console of {name}…"));
                            ui.add_space(14.0);
                        });
                    }
                }
            });
            return;
        }
        let running: Vec<(VmId, String)> = state
            .vms
            .iter()
            .filter(|v| v.state == VmState::Running)
            .map(|v| (v.vm_id, v.name.as_str().to_owned()))
            .collect();
        let mut open = None;
        theme::card(ui, |ui| {
            if running.is_empty() {
                empty_state(
                    ui,
                    "No console connected.",
                    "No VM is running. Start one from the Dashboard, then choose Console.",
                );
                return;
            }
            ui.label("Running VMs:");
            for (id, name) in &running {
                ui.horizontal(|ui| {
                    ui.label(name);
                    let r = ui.button("Open console");
                    theme::accessible_name(
                        &r,
                        egui::WidgetType::Button,
                        &format!("Open the console of {name}"),
                    );
                    if r.clicked() {
                        open = Some(*id);
                    }
                });
            }
        });
        if let Some(id) = open {
            self.open_console(id);
        }
    }

    fn save_screenshot(&mut self) {
        let Some(live) = &self.live else { return };
        let png = match live.screenshot_png() {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                self.screenshot_status =
                    Some((format!("Screenshot failed: {e}"), theme::Tone::Danger));
                return;
            }
            None => {
                self.screenshot_status =
                    Some(("No screen to save yet.".into(), theme::Tone::Warning));
                return;
            }
        };
        let stamp: String = UtcTimestamp::now()
            .to_string()
            .chars()
            .take(19)
            .filter(|c| c.is_ascii_digit() || *c == 'T')
            .collect();
        let name = format!(
            "lightvm-{}-{}.png",
            sanitize_file_name(&live.target().vm_name),
            stamp.replace('T', "-")
        );
        let Some(path) = self.save_picker.as_mut().and_then(|f| f(&name)) else {
            if self.save_picker.is_none() {
                self.screenshot_status = Some((
                    "Save dialog not available on this platform.".into(),
                    theme::Tone::Warning,
                ));
            }
            return;
        };
        self.screenshot_status = Some(match std::fs::write(&path, png) {
            Ok(()) => (
                format!("Screenshot saved: {}", path.display()),
                theme::Tone::Positive,
            ),
            Err(e) => (format!("Screenshot not saved: {e}"), theme::Tone::Danger),
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        theme::page_header(
            ui,
            "Settings",
            "LightVM preferences, saved in the user profile.",
            |_| {},
        );
        if let Some(e) = &self.settings_error {
            theme::notice(ui, e, theme::Tone::Warning);
            ui.add_space(10.0);
        }

        theme::group_title(ui, "Appearance");
        theme::card(ui, |ui| {
            theme::row(ui, "Theme", None, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.selectable_value(&mut self.theme, UiTheme::Dark, "Dark");
                    ui.selectable_value(&mut self.theme, UiTheme::Light, "Light");
                });
            });
        });
        if self.theme != self.settings.theme {
            self.settings.theme = self.theme;
            self.save_settings();
        }

        ui.add_space(14.0);
        theme::group_title(ui, "Console");
        if theme::card(ui, |ui| {
            settings::console_ui(ui, &mut self.settings.keyboard_layout)
        }) {
            self.save_settings();
        }

        ui.add_space(14.0);
        theme::group_title(ui, "Supervisor");
        let state = self.supervisor_ui_state();
        let (action, changed) = theme::card(ui, |ui| {
            settings::supervisor_ui(ui, &mut self.settings, &state)
        });
        if changed {
            self.save_settings();
        }
        if let Some(a) = action {
            self.run_supervisor_action(a, ctx);
        }

        ui.add_space(14.0);
        theme::group_title(ui, "Host capacity");
        let st = self.state();
        theme::card(ui, |ui| {
            settings::host_ui(
                ui,
                st.as_ref().and_then(|s| s.host),
                st.as_ref().is_some_and(|s| s.connected),
            );
        });

        ui.add_space(14.0);
        theme::group_title(ui, "If LightVM closes unexpectedly");
        if theme::card(ui, |ui| {
            settings::policy_ui(ui, &mut self.settings.ui_loss_policy)
        }) {
            self.change_policy();
        }

        ui.add_space(14.0);
        theme::group_title(ui, "Security");
        let st = self.state();
        let grant = st.as_ref().and_then(|s| s.agents.standing.live());
        let vm_name = grant
            .and_then(|g| st.as_ref().and_then(|s| s.vm(g.vm_id)))
            .map_or("a deleted VM", |v| v.name.as_str());
        theme::card(ui, |ui| {
            settings::security_ui(
                ui,
                grant.map(|grant| settings::UnattendedSummary { vm_name, grant }),
            );
        });

        ui.add_space(14.0);
        theme::group_title(ui, "Rendering");
        let active = self.rendering.backend();
        if theme::card(ui, |ui| {
            settings::rendering_ui(ui, &mut self.settings.rendering, &active)
        }) {
            self.save_settings();
        }

        self.media_section(ui);

        if self.simulated_console {
            ui.add_space(14.0);
            theme::group_title(ui, "Diagnostics");
            theme::card(ui, |ui| {
                theme::row(
                    ui,
                    "Simulated console",
                    Some("Development build only: generates a test image. Not saved."),
                    |ui| ui.checkbox(&mut self.animate, "Update the simulated framebuffer"),
                );
            });
        }
    }

    /// Settings → Installation media: imported ISOs and their deletion.
    fn media_section(&mut self, ui: &mut egui::Ui) {
        let Some(st) = self.state() else { return };
        ui.add_space(14.0);
        theme::group_title(ui, "Installation media");
        let deleting = st.delete_media.is_pending();
        let chosen = theme::card(ui, |ui| {
            media_library::media_ui(ui, &st.media, &st.vms, st.connected, deleting)
        });
        if let Some(id) = chosen
            && let Some(m) = st.media.records.iter().find(|m| m.id == id)
        {
            self.media_dialog = Some(media_library::DeleteDialog::new(m));
        }
    }

    /// Confirmation of a media deletion; stays open with the reason if the
    /// supervisor refuses it.
    fn media_dialog_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.media_dialog else {
            return;
        };
        let state = cached_state(&self.service, &self.state_cache).unwrap_or_default();
        match media_library::dialog_ui(ui, d, &state) {
            media_library::DialogOutcome::Keep => {}
            media_library::DialogOutcome::Close => self.media_dialog = None,
            media_library::DialogOutcome::Delete(id) => {
                self.send(ServiceCommand::DeleteMedia(id));
            }
        }
    }

    fn simulated_console_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        theme::page_header(
            ui,
            "Console",
            &format!(
                "Simulated framebuffer {FB_WIDTH}×{FB_HEIGHT}, at most 10 updates/s. Not a real console."
            ),
            |_| {},
        );

        let now = Instant::now();
        // Deadlines on a fixed 100 ms grid. egui brings every
        // `request_repaint_after` forward by the expected duration of a frame
        // (`predicted_dt`, about 16 ms) and the system timer can add more
        // early wake-up: without this tolerance every early wake-up would
        // produce a burst of empty frames until the deadline.
        let predicted =
            Duration::try_from_secs_f32(ctx.input(|i| i.predicted_dt)).unwrap_or(Duration::ZERO);
        let tolerance = (predicted + FB_WAKE_TOLERANCE).min(FB_MIN_INTERVAL / 2);
        let due = match self.last_fb_update {
            None => true,
            Some(t) => self.animate && now + tolerance >= t + FB_MIN_INTERVAL,
        };
        if due {
            self.render_pattern();
            let image = egui::ColorImage::new([FB_WIDTH, FB_HEIGHT], self.pixels.clone());
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("console-fb", image, egui::TextureOptions::LINEAR));
                }
            }
            // Advance on the grid; if the delay exceeds one interval, restart from `now`.
            let next = match self.last_fb_update {
                Some(t) if now < t + 2 * FB_MIN_INTERVAL => t + FB_MIN_INTERVAL,
                _ => now,
            };
            self.last_fb_update = Some(next);
            self.fb_updates += 1;
        }
        if self.animate {
            // Repaint only at the next framebuffer update, no continuous polling.
            let target = self.last_fb_update.map_or(now, |t| t + FB_MIN_INTERVAL);
            ctx.request_repaint_after(target.saturating_duration_since(now));
        }

        if let Some(tex) = &self.texture {
            let avail = ui.available_size();
            let aspect = FB_WIDTH as f32 / FB_HEIGHT as f32;
            let mut size = egui::vec2(avail.x, avail.x / aspect);
            if size.y > avail.y {
                size = egui::vec2(avail.y * aspect, avail.y);
            }
            ui.add(egui::Image::new((tex.id(), size)).alt_text("Simulated console framebuffer"));
        }
    }

    /// Generates an animated gradient with a grid in the reused buffer (cost
    /// linear in the pixels).
    fn render_pattern(&mut self) {
        self.phase = (self.phase + 8) % (2 * FB_WIDTH as u32);
        let p = self.phase;
        for (y, row) in self
            .pixels
            .as_chunks_mut::<FB_WIDTH>()
            .0
            .iter_mut()
            .enumerate()
        {
            let yy = y as u32;
            for (x, px) in row.iter_mut().enumerate() {
                let xx = x as u32;
                let grid = xx.is_multiple_of(64) || yy.is_multiple_of(64);
                let r = (((xx + p) % FB_WIDTH as u32) * 255 / FB_WIDTH as u32) as u8;
                let g = (((yy + p / 2) % FB_HEIGHT as u32) * 255 / FB_HEIGHT as u32) as u8;
                let b = ((xx ^ yy).wrapping_add(p) & 0xff) as u8;
                *px = if grid {
                    egui::Color32::from_gray(230)
                } else {
                    egui::Color32::from_rgb(r, g, b)
                };
            }
        }
    }
}

impl Drop for LightVmApp {
    fn drop(&mut self) {
        self.close_console();
    }
}

impl std::fmt::Debug for LightVmApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LightVmApp")
            .field("section", &self.section)
            .field("theme", &self.theme)
            .field("animate", &self.animate)
            .field("simulated_console", &self.simulated_console)
            .field("frames", &self.frames)
            .field("fb_updates", &self.fb_updates)
            .field("rendering", &self.rendering)
            .field("discarded_pastes", &self.discarded_pastes)
            .finish_non_exhaustive()
    }
}

/// Origin declared to the registry for an ISO imported from the file picker:
/// the file name only (no folders), as plain text.
pub fn declared_origin(file_name: &str) -> String {
    let clean: String = file_name
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "ISO chosen by the user".into()
    } else {
        clean.to_owned()
    }
}

fn sanitize_file_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Notice above the console while an agent owns the input, with the
/// "Take control" button; returns `true` when it is clicked.
fn agent_input_notice(ui: &mut egui::Ui, client: Option<&str>, taking: bool) -> bool {
    let p = theme::Palette::of(ui);
    let mut clicked = false;
    egui::Frame::new()
        .fill(p.warning.linear_multiply(0.08))
        .stroke(egui::Stroke::new(1.0, p.warning.linear_multiply(0.4)))
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                theme::dot(ui, p.warning);
                let who = client.map_or_else(
                    || "an agent".to_owned(),
                    |c| format!("the agent \u{201c}{c}\u{201d}"),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Input controlled by {who}: your keyboard and mouse are not sent to the guest."
                    ))
                    .color(p.text),
                );
                let r = ui.add_enabled(
                    !taking,
                    theme::primary(ui, if taking { "Taking control…" } else { "Take control" }),
                );
                theme::accessible_name(
                    &r,
                    egui::WidgetType::Button,
                    "Take keyboard and mouse control back",
                );
                clicked = r.clicked();
            });
        });
    clicked
}

/// Description under the Console title when no console is connected.
const CONSOLE_DESCRIPTION: &str = "Screen of a running VM, with keyboard and mouse.";

/// Content of a page: vertical scrolling and maximum width, for lines that
/// stay readable on wide screens too.
fn page(ui: &mut egui::Ui, id: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // `set_max_width` can also widen: never beyond the visible space.
            ui.set_max_width(ui.available_width().min(theme::PAGE_WIDTH));
            content(ui);
            ui.add_space(12.0);
        });
}

/// Brand in the sidebar: an accent square with a stylised pixel screen and
/// the product name in the pixel face.
fn brand(ui: &mut egui::Ui) {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (r, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(r, 0.0, p.accent);
        let screen = egui::Rect::from_center_size(r.center(), egui::vec2(12.0, 8.0));
        painter.rect_filled(screen, 0.0, p.on_accent);
        let inner = screen.shrink(2.0);
        painter.rect_filled(inner, 0.0, p.accent);
        ui.add_space(4.0);
        let font = theme::pixel_font(ui, 22.0);
        ui.label(egui::RichText::new("LightVM").font(font).color(p.text));
    });
}

/// Empty state of a list: message and, below, a hint.
pub(crate) fn empty_state(ui: &mut egui::Ui, message: &str, hint: &str) {
    let p = theme::Palette::of(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(14.0);
        ui.add(egui::Label::new(egui::RichText::new(message).color(p.text)).wrap());
        ui.add(egui::Label::new(egui::RichText::new(hint).small().color(p.text_weak)).wrap());
        ui.add_space(14.0);
    });
}

/// Text and tone of the supervisor state, the same on every page.
pub fn connection_state(alive: bool, state: &ServiceState) -> (&'static str, theme::Tone) {
    if !alive {
        ("terminated", theme::Tone::Danger)
    } else if state.connected {
        ("connected", theme::Tone::Positive)
    } else if state.connection == service::CONNECTING {
        ("connecting", theme::Tone::Warning)
    } else {
        ("not connected", theme::Tone::Danger)
    }
}

/// Status bar: effective rendering backend and degraded mode (F1-16). The
/// text says everything even without colour.
pub fn status_bar_ui(ui: &mut egui::Ui, info: &RenderingInfo) {
    let p = theme::Palette::of(ui);
    let weak = |t: String| egui::RichText::new(t).small().color(p.text_weak);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        ui.label(weak(info.label()))
            .on_hover_text(info.explanation());
        if info.degraded() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                theme::dot(ui, p.warning);
                let text = format!(
                    "Degraded mode (software): console at {} updates/s",
                    render::DEGRADED_CONSOLE_FPS
                );
                ui.label(egui::RichText::new(text).small().color(p.warning))
                    .on_hover_text(info.explanation());
            });
        }
        if !info.failed.is_empty() {
            let names: Vec<&str> = info.failed.iter().map(|(t, _)| t.name()).collect();
            ui.label(weak(format!("unavailable: {}", names.join(", "))))
                .on_hover_text(info.explanation());
        }
    });
}

#[cfg(test)]
mod revert_flow_tests {
    use super::*;

    fn running_vm(vm_id: VmId, run_id: RunId) -> lightvm_proto::ipc::VmSummary {
        let name = lightvm_proto::VmName::parse("lab").unwrap();
        lightvm_proto::ipc::VmSummary {
            vm_id,
            name: name.clone(),
            state: VmState::Running,
            run_id: Some(run_id),
            last_run_id: Some(run_id),
            config: lightvm_proto::VmConfig {
                tools: Default::default(),
                name,
                guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
                vcpus: 2,
                memory_mib: 4096,
                disk_gib: 32,
                network: lightvm_proto::NetworkProfile::Offline,
                endpoints: Vec::new(),
                install_media: None,
                data_media: None,
                firmware: lightvm_proto::Firmware::Uefi,
                limits: Default::default(),
                imported: None,
            },
            config_revision: 1,
            disk_used_bytes: None,
            baselines: Default::default(),
            snapshots: Default::default(),
            start: Default::default(),
            vcpu_halt: None,
        }
    }

    /// The VM does not go off within the timeout: the flow is dropped with
    /// a message, and a stop much later does not revert it.
    #[test]
    fn revert_flow_is_dropped_after_the_stop_timeout() {
        let vm_id = VmId::new_v7();
        let run_id = RunId::new_v7();
        let mut app = LightVmApp::new(false);
        let t0 = Instant::now();
        app.snapshot_revert_flow = Some(RevertFlow {
            vm_id,
            snapshot_id: SnapshotId::new_v7(),
            run_id,
            waiting: RevertWaiting::Stop,
            stop_sent: Some(t0),
        });
        let mut state = ServiceState {
            vms: vec![running_vm(vm_id, run_id)],
            ..ServiceState::default()
        };
        app.snapshot_revert_flow_step(&state, t0 + REVERT_STOP_TIMEOUT / 2);
        assert!(app.snapshot_revert_flow.is_some(), "still waiting");
        app.snapshot_revert_flow_step(&state, t0 + REVERT_STOP_TIMEOUT);
        assert!(app.snapshot_revert_flow.is_none());
        assert!(
            app.snapshot_revert_notice
                .as_ref()
                .is_some_and(|(id, t)| *id == vm_id && t.contains("did not shut down in time"))
        );
        // Off much later: nothing happens (no flow left).
        state.vms[0].state = VmState::Stopped;
        state.vms[0].run_id = None;
        app.snapshot_revert_flow_step(&state, t0 + REVERT_STOP_TIMEOUT * 100);
        assert!(app.snapshot_revert_flow.is_none());
    }
}
