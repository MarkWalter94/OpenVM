//! `lightvm-desktop`: native desktop interface of LightVM (egui/eframe).
//!
//! F2 (alpha manuale): Dashboard with the catalog VMs and their actions,
//! "New VM" wizard, console of running VMs through the supervisor's per-run
//! display ticket (ADR-0013) with keyboard and mouse sent as IPC input
//! batches, dialogs for deletion and for closing with running VMs, Settings
//! with QEMU paths, host capacity and the security facts of this version.
//!
//! The simulated framebuffer is a pattern generated in memory: it is not a
//! real console, it is shown only on request (`--simulated-console`, or
//! `--smoke`) and is never used in the product path (F2 exit criterion: no
//! screen with simulated data).
//!
//! The UI is separate from the eframe loop (`main.rs`) so that it can be
//! driven headless with `egui_kittest` through the AccessKit tree, with a
//! [`service::Supervisor`] built by the test instead of the child process.

use std::time::{Duration, Instant};

pub mod clipboard;
pub mod console;
pub mod dashboard;
pub mod dialogs;
pub mod keymap;
pub mod libraries;
pub mod render;
pub mod service;
pub mod settings;
pub mod theme;
pub mod vm_settings;
pub mod wizard;

use console::{ConsoleAction, LiveConsole};
use dashboard::{Confirm, DashboardAction};
use lightvm_proto::{MediaKind, RunId, VmId, VmState};
use render::RenderingInfo;
use service::{RequestStatus, Service, ServiceCommand, ServiceConfig, ServiceState, Supervisor};
use settings::{SupervisorAction, SupervisorUiState, UiSettings};

pub use dashboard::dashboard_ui;

/// Folder picker: receives the title, returns the chosen folder.
pub type FolderPicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;
/// ISO picker: receives the title, returns the chosen file.
pub type FilePicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;
/// Save dialog: receives the proposed file name, returns the destination.
pub type SavePicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;

/// Native folder picker (Windows only; elsewhere no picker).
pub fn native_folder_picker() -> Option<FolderPicker> {
    #[cfg(windows)]
    {
        Some(Box::new(|title: &str| {
            rfd::FileDialog::new().set_title(title).pick_folder()
        }))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Native ISO picker (Windows only).
pub fn native_file_picker() -> Option<FilePicker> {
    #[cfg(windows)]
    {
        Some(Box::new(|title: &str| {
            rfd::FileDialog::new()
                .set_title(title)
                .add_filter("ISO image", &["iso"])
                .pick_file()
        }))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Native save dialog for PNG screenshots (Windows only).
pub fn native_save_picker() -> Option<SavePicker> {
    #[cfg(windows)]
    {
        Some(Box::new(|name: &str| {
            rfd::FileDialog::new()
                .set_title("Save the screenshot")
                .set_file_name(name)
                .add_filter("PNG image", &["png"])
                .save_file()
        }))
    }
    #[cfg(not(windows))]
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

/// Side navigation items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Overview of the VMs.
    Dashboard,
    /// Console of the guest.
    Console,
    /// Settings.
    Settings,
}

impl Section {
    /// All items, in side panel order.
    pub const ALL: [Self; 3] = [Self::Dashboard, Self::Console, Self::Settings];

    /// Visible and accessible label (name of the AccessKit node).
    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Console => "Console",
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

/// Close of the app with running VMs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Closing {
    Ask { focus: bool },
    Stopping { started: Instant },
    Finishing,
}

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
    live: Option<LiveConsole>,
    /// VM whose console the Console page shows.
    console_vm: Option<VmId>,
    ticket_request: Option<TicketRequest>,
    console_max_fps: u32,
    fullscreen: bool,
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
    save_picker: Option<SavePicker>,
    /// Supervisor configuration given on the command line (`--catalog`):
    /// takes precedence over the Settings page, restarts included.
    cli_config: Option<ServiceConfig>,
    /// Restart or stop of the supervisor in progress outside the UI thread.
    restart: Option<std::sync::mpsc::Receiver<RestartOutcome>>,
    vm_edit: Option<vm_settings::VmEdit>,
    wizard: Option<wizard::Wizard>,
    dialog: Option<ConfirmDialog>,
    closing: Option<Closing>,
    allow_close: bool,
    keyboard_nav: bool,
    discarded_pastes: u64,
    /// Effective rendering backend, set by the eframe loop (F1-16).
    pub rendering: RenderingInfo,
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
            live: None,
            console_vm: None,
            ticket_request: None,
            console_max_fps: console::DEFAULT_MAX_FPS,
            fullscreen: false,
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
            save_picker: None,
            cli_config: None,
            restart: None,
            vm_edit: None,
            wizard: None,
            dialog: None,
            closing: None,
            allow_close: false,
            keyboard_nav: false,
            discarded_pastes: 0,
            rendering: RenderingInfo::default(),
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
        self.set_supervisor(Box::new(service));
        self.applied_service = Some((cfg.catalog.clone(), cfg.qemu_dir.clone()));
    }

    /// Connects a supervisor: the child process, or a stand-in in tests.
    pub fn set_supervisor(&mut self, s: Box<dyn Supervisor>) {
        self.service = Some(s);
        self.service_error = None;
        self.section = Section::Dashboard;
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

    /// ISO picker of the wizard (native in the app, fake in tests).
    pub fn set_file_picker(&mut self, s: Option<FilePicker>) {
        self.file_picker = s;
    }

    /// Save dialog of the console screenshot (native in the app, fake in tests).
    pub fn set_save_picker(&mut self, s: Option<SavePicker>) {
        self.save_picker = s;
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
            qemu_dir: self.settings.qemu_dir.clone(),
            runtime_dir: None,
            audit_log: None,
            policy: self.settings.ui_loss_policy,
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
                eprintln!("service: {e}");
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
        self.vm_edit = None;
        self.wizard = None;
        self.dialog = None;
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
                eprintln!("service: {e}");
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
        let current = match &self.cli_config {
            Some(c) => Some((c.catalog.clone(), c.qemu_dir.clone())),
            None => self
                .settings
                .effective_catalog()
                .map(|c| (c, self.settings.qemu_dir.clone())),
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
            };
        }
        match &mut self.service {
            Some(s) => {
                let alive = s.is_alive();
                let st = s.state();
                let (text, tone) = connection_state(alive, &st);
                SupervisorUiState {
                    description: format!("{text} (PID {})", s.pid()),
                    tone,
                    running: alive,
                    in_progress: false,
                    running_vms: st.active_vms().count(),
                    pending_changes: current != self.applied_service,
                    folder_picker,
                    command_line,
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
            }
            None => {
                // The cause of the failed start, if known, stays visible.
                let cause = self
                    .service_error
                    .take()
                    .unwrap_or_else(|| "supervisor not started".into());
                let e = format!("cannot open VM \"{name}\": {cause}");
                eprintln!("service: {e}");
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

    /// The Console page is in full screen.
    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// A confirmation dialog is open.
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
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

    fn state(&self) -> Option<ServiceState> {
        self.service.as_ref().map(|s| s.state())
    }

    fn send(&self, c: ServiceCommand) {
        if let Some(s) = &self.service {
            s.send(c);
        }
    }

    /// Draws the whole interface in the root `Ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.frames += 1;
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
        self.handle_close_request(&ctx);

        // Console chords, removed from the input before any widget sees them.
        let chords = if self.section == Section::Console {
            let mut swallow = std::mem::take(&mut self.chord_swallow);
            let c = ctx.input_mut(|i| console::take_chords(&mut i.events, &mut swallow));
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
                live.on_hidden();
            }
            self.flush_console_input();
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
                Section::Settings => page(ui, "lightvm_page_settings", |ui| {
                    self.settings_ui(ui, &ctx);
                }),
            });

        self.confirm_dialog_ui(ui);
        self.close_dialog_ui(ui, &ctx);
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
        let Some(s) = &mut self.service else {
            let tone = if self.service_error.is_some() {
                theme::Tone::Danger
            } else {
                theme::Tone::Neutral
            };
            return ("Supervisor not started".into(), tone);
        };
        let alive = s.is_alive();
        let (text, tone) = connection_state(alive, &s.state());
        (format!("Supervisor {text}"), tone)
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
        let state = srv.state();
        let active = alive && state.connected;

        if self.wizard.is_some() {
            self.wizard_page(ui, &state, active);
            return;
        }

        match dashboard_ui(ui, &state, pid, alive) {
            Some(DashboardAction::Command(c)) => self.send(c),
            Some(DashboardAction::NewVm) => {
                self.vm_edit = None;
                self.wizard = Some(wizard::Wizard::new(state.host));
                self.send(ServiceCommand::ListMedia);
            }
            Some(DashboardAction::Console(id)) => self.open_console(id),
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
        let stopped = active && current.is_some_and(|v| v.state == VmState::Stopped);
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

    fn confirm_dialog_ui(&mut self, ui: &egui::Ui) {
        let Some(d) = &mut self.dialog else { return };
        let state = self.service.as_ref().map(|s| s.state()).unwrap_or_default();
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
            (Confirm::Delete(_), Some(v)) => v.state == VmState::Stopped,
            (Confirm::ForceStop(_), Some(v)) => v.state.has_process(),
            (Confirm::Reset(_), Some(v)) => v.state == VmState::Running && v.run_id == d.run_id,
            (Confirm::DetachInstall(_), Some(v)) => {
                v.state == VmState::Stopped && v.config.install_media.is_some()
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
                    Confirm::ForceStop(id) => ServiceCommand::ForceStop(id),
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
        let running = self.state().map_or(0, |s| s.active_vms().count());
        if running == 0 && self.closing.is_none() {
            self.allow_close = true;
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.closing.is_none() {
            self.closing = Some(Closing::Ask { focus: true });
        }
    }

    fn close_dialog_ui(&mut self, ui: &egui::Ui, ctx: &egui::Context) {
        let Some(c) = self.closing.clone() else {
            return;
        };
        let state = self.state().unwrap_or_default();
        let view = match &c {
            Closing::Ask { .. } => {
                let names: Vec<String> = state
                    .active_vms()
                    .map(|v| v.name.as_str().to_owned())
                    .collect();
                if names.is_empty() && self.service.is_some() {
                    // Stopped meanwhile: nothing to ask.
                    self.closing = None;
                    return;
                }
                dialogs::CloseView::Ask { names }
            }
            Closing::Stopping { started } => {
                let vms: Vec<(String, String)> = state
                    .vms
                    .iter()
                    .map(|v| {
                        (
                            v.name.as_str().to_owned(),
                            dashboard::vm_state(v.state).0.into(),
                        )
                    })
                    .collect();
                let active = state.active_vms().count();
                let elapsed = started.elapsed();
                if self.service.is_none() || active == 0 || elapsed > CLOSE_STOP_TIMEOUT {
                    self.restart_in_background(ctx, false);
                    self.closing = Some(Closing::Finishing);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
                dialogs::CloseView::Stopping {
                    stopped: vms.len().saturating_sub(active),
                    vms,
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
                for v in state.active_vms() {
                    self.send(ServiceCommand::Stop(v.vm_id));
                }
                self.closing = Some(Closing::Stopping {
                    started: Instant::now(),
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

    /// Closes the display session and sends the key releases.
    fn close_console(&mut self) {
        if let Some(mut live) = self.live.take() {
            for b in live.close() {
                self.send(ServiceCommand::Input(b));
            }
        }
        self.ticket_request = None;
    }

    fn flush_console_input(&mut self) {
        let Some(live) = &mut self.live else { return };
        let batches = live.take_input();
        for b in batches {
            self.send(ServiceCommand::Input(b));
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen != on {
            self.fullscreen = on;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
    }

    /// Keeps the console aligned with the VM: connects with a fresh ticket
    /// when the VM runs (new run after a restart, or after a display
    /// disconnection), closes it when the VM stops.
    fn sync_console(&mut self, state: &ServiceState) {
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
            let dropped = live
                .disconnected_at()
                .is_some_and(|t| t.elapsed() >= RECONNECT_AFTER);
            if same_run && !dropped {
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
            self.sync_console(&state);
            if self.live.is_none() && self.console_vm.is_some() {
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
        let state = self.state();
        let input_error = state.as_ref().and_then(|s| s.input_error.clone());
        if let Some(name) = self.live.as_ref().map(|l| l.target().vm_name.clone()) {
            if !self.fullscreen {
                theme::page_header(
                    ui,
                    &format!("Console · {name}"),
                    &format!(
                        "Click the screen to send keyboard and mouse to the guest; {} gives them back.",
                        console::LEAVE_CHORD
                    ),
                    |_| {},
                );
                if let Some((text, tone)) = &self.screenshot_status {
                    theme::notice(ui, text, *tone);
                    ui.add_space(6.0);
                }
            }
            let (layout, fullscreen) = (self.settings.keyboard_layout, self.fullscreen);
            let action = self
                .live
                .as_mut()
                .and_then(|live| live.ui(ui, layout, fullscreen, chords, input_error.as_deref()));
            self.flush_console_input();
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
            self.console_empty_state(ui, state.as_ref());
        });
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
        let stamp: String = lightvm_proto::UtcTimestamp::now()
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
        theme::card(ui, settings::security_ui);

        ui.add_space(14.0);
        theme::group_title(ui, "Rendering");
        let active = self.rendering.backend();
        if theme::card(ui, |ui| {
            settings::rendering_ui(ui, &mut self.settings.rendering, &active)
        }) {
            self.save_settings();
        }

        if self.simulated_console {
            ui.add_space(14.0);
            theme::group_title(ui, "Diagnostics");
            theme::card(ui, |ui| {
                theme::row(
                    ui,
                    "Simulated console",
                    Some("Enabled with --simulated-console: generates a test image. Not saved."),
                    |ui| ui.checkbox(&mut self.animate, "Update the simulated framebuffer"),
                );
            });
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

/// Brand in the sidebar: an accent square with a stylised screen and the
/// product name.
fn brand(ui: &mut egui::Ui) {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (r, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(r, egui::CornerRadius::same(6), p.accent);
        let screen = egui::Rect::from_center_size(r.center(), egui::vec2(12.0, 9.0));
        painter.rect_stroke(
            screen,
            egui::CornerRadius::same(2),
            egui::Stroke::new(1.5, p.on_accent),
            egui::StrokeKind::Middle,
        );
        ui.add_space(4.0);
        ui.label(egui::RichText::new("LightVM").size(17.0).color(p.text));
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
