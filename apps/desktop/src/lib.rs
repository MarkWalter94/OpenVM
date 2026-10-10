//! `lightvm-desktop`: native desktop interface of LightVM (egui/eframe).
//!
//! Status: F0 spike, step 1 (ROADMAP section 15). It verifies the real
//! minimal eframe build, the rendering backends and the "console as texture"
//! path (ROADMAP 6.0 and 6.4). The simulated framebuffer is a pattern
//! generated in memory: it is not a real console, it is shown only on request
//! (`--simulated-console`, or `--smoke` without a real console) and must not be
//! used in the product path (F2 exit criterion: no screen with simulated
//! data).
//!
//! F0-04 adds the real console (`console`): with `--vnc-socket`/`--qmp-socket`
//! the Console panel shows the framebuffer received from the read-only RFB
//! client and forwards keyboard and mouse only via QMP.
//!
//! F1-05 (backlog F1-13, F1-14, F1-16, F1-19): blocking of host clipboard text
//! (`clipboard`), diagnostics of the Linux libraries loaded at runtime
//! (`libraries`), automatic backend selection with status bar (`render`), VM
//! list and egui_kittest snapshots.
//!
//! The UI is separate from the eframe loop (`main.rs`) so that it can be
//! driven headless with `egui_kittest` through the AccessKit tree.

use std::time::{Duration, Instant};

pub mod clipboard;
pub mod console;
pub mod keymap;
pub mod libraries;
pub mod render;
pub mod service;
pub mod settings;
pub mod theme;
pub mod vm_settings;

use console::{ConsoleConfig, LiveConsole};
use render::RenderingInfo;
use service::{Service, ServiceCommand, ServiceConfig, ServiceState};
use settings::{SupervisorAction, SupervisorUiState, UiSettings};

/// Folder picker: receives the title, returns the chosen folder.
pub type FolderPicker = Box<dyn FnMut(&str) -> Option<std::path::PathBuf>>;

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

/// Row of the VM list shown in the Dashboard.
///
/// In F1 there is no catalog yet: in the product path the list is empty and
/// the Dashboard says so. The rows come from the catalog in F2; tests can set
/// them with [`LightVmApp::set_vm_list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmRow {
    /// VM name.
    pub name: String,
    /// Declared operating system.
    pub os: String,
    /// Readable state (text, not only colour).
    pub state: String,
    /// The VM is running.
    pub running: bool,
}

/// Width of the simulated framebuffer, in texels (reference ROADMAP 13: 1280×720).
pub const FB_WIDTH: usize = 1280;
/// Height of the simulated framebuffer, in texels.
pub const FB_HEIGHT: usize = 720;
/// Minimum interval between two framebuffer updates: at most 10 per second.
pub const FB_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Tolerated early wake-up on top of the one introduced by egui.
const FB_WAKE_TOLERANCE: Duration = Duration::from_millis(3);

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

/// UI state.
pub struct LightVmApp {
    section: Section,
    theme: UiTheme,
    applied_theme: Option<UiTheme>,
    animate: bool,
    /// The Console page shows the simulated framebuffer when no real console
    /// is configured (off by default: no simulated data in the product path).
    simulated_console: bool,
    texture: Option<egui::TextureHandle>,
    pixels: Vec<egui::Color32>,
    phase: u32,
    last_fb_update: Option<Instant>,
    fb_updates: u64,
    frames: u64,
    live: Option<LiveConsole>,
    vm_list: Vec<VmRow>,
    service: Option<Service>,
    service_error: Option<String>,
    settings: UiSettings,
    settings_path: Option<std::path::PathBuf>,
    settings_error: Option<String>,
    service_exe: Option<std::path::PathBuf>,
    /// Catalog and QEMU folder the supervisor was started with.
    applied_service: Option<(std::path::PathBuf, Option<std::path::PathBuf>)>,
    folder_picker: Option<FolderPicker>,
    /// Supervisor configuration given on the command line (`--catalog`):
    /// takes precedence over the Settings page, restarts included.
    cli_config: Option<ServiceConfig>,
    /// Restart or stop of the supervisor in progress outside the UI thread.
    restart: Option<std::sync::mpsc::Receiver<RestartOutcome>>,
    vm_edit: Option<vm_settings::VmEdit>,
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
            vm_list: Vec::new(),
            service: None,
            service_error: None,
            settings: UiSettings::default(),
            settings_path: None,
            settings_error: None,
            service_exe: None,
            applied_service: None,
            folder_picker: None,
            cli_config: None,
            restart: None,
            vm_edit: None,
            discarded_pastes: 0,
            rendering: RenderingInfo::default(),
        }
    }

    /// Creates the UI with an already built console (for example
    /// [`LiveConsole::synthetic`] in tests).
    pub fn with_live_console(console: LiveConsole) -> Self {
        let mut app = Self::new(false);
        app.live = Some(console);
        app
    }

    /// Enables the simulated framebuffer on the Console page when no real
    /// console is configured (`--simulated-console`; measurements and tests).
    pub fn set_simulated_console(&mut self, on: bool) {
        self.simulated_console = on;
    }

    /// The simulated framebuffer is enabled.
    pub fn simulated_console(&self) -> bool {
        self.simulated_console
    }

    /// Replaces the VM list shown in the Dashboard.
    pub fn set_vm_list(&mut self, rows: Vec<VmRow>) {
        self.vm_list = rows;
    }

    /// Connects the supervisor started by the UI (F1-11) with the
    /// configuration used. The Dashboard then shows the VMs of the service's
    /// catalog.
    pub fn set_service(&mut self, service: Service, cfg: &ServiceConfig) {
        self.service = Some(service);
        self.applied_service = Some((cfg.catalog.clone(), cfg.qemu_dir.clone()));
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

    /// Restart (or stop only) of the supervisor from Settings, on a separate
    /// thread: the orderly stop of the VMs can take tens of seconds and the
    /// window must stay responsive. The outcome arrives in [`Self::ui`].
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
        let old = self.service.take();
        self.vm_edit = None;
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
                    running_vms: st.vms.iter().filter(|v| v.state.has_process()).count(),
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

    /// Connected supervisor, if any.
    pub fn service(&self) -> Option<&Service> {
        self.service.as_ref()
    }

    /// Controlled stop of the connected supervisor, if any.
    /// Returns the exit code and whether forcing was necessary.
    pub fn stop_service(&mut self) -> Option<(Option<i32>, bool)> {
        self.vm_edit = None;
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

    /// Creates the UI with the real console (read-only RFB + QMP input).
    pub fn with_console(config: ConsoleConfig) -> Self {
        let mut app = Self::new(false);
        app.live = Some(LiveConsole::new(config));
        app
    }

    /// Real console, if configured.
    pub fn live_console(&self) -> Option<&LiveConsole> {
        self.live.as_ref()
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

    /// Draws the whole interface in the root `Ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.frames += 1;
        let ctx = ui.ctx().clone();
        // F1-13: before any widget no `Paste` remains in the input.
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
        self.collect_restart();
        let p = theme::Palette::of(ui);
        let (supervisor, supervisor_tone) = self.supervisor_summary();

        egui::Panel::left("lightvm_nav")
            .resizable(false)
            .exact_size(208.0)
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
                    if theme::nav_item(ui, s.label(), self.section == s).clicked() {
                        self.section = s;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.spacing_mut().item_spacing.x = 7.0;
                        theme::dot(ui, p.tone(supervisor_tone));
                        ui.label(egui::RichText::new(supervisor).small().color(p.text_weak));
                    });
                });
            });

        egui::Panel::bottom("lightvm_status")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar_background)
                    .inner_margin(egui::Margin::symmetric(14, 4)),
            )
            .show(ui, |ui| status_bar_ui(ui, &self.rendering));

        if self.section != Section::Console
            && let Some(live) = &mut self.live
        {
            live.on_hidden();
        }

        let margins = egui::Margin {
            left: 32,
            right: 32,
            top: 26,
            bottom: 16,
        };
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(p.background).inner_margin(margins))
            .show(ui, |ui| match self.section {
                Section::Dashboard => page(ui, "lightvm_page_dashboard", |ui| {
                    self.dashboard_ui(ui);
                }),
                Section::Console => self.console_ui(ui, &ctx),
                Section::Settings => page(ui, "lightvm_page_settings", |ui| {
                    self.settings_ui(ui, &ctx);
                }),
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

    fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        let Some(srv) = &mut self.service else {
            theme::page_header(ui, "Dashboard", DASHBOARD_DESCRIPTION, |_| {});
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
            vm_list_ui(ui, &self.vm_list);
            return;
        };
        let alive = srv.is_alive();
        let state = srv.state();
        match service_ui(ui, &state, srv.pid(), alive) {
            Some(ServiceCommand::OpenSettings(id)) => {
                self.vm_edit = state.vms.iter().find(|v| v.vm_id == id).map(|v| {
                    vm_settings::VmEdit::new(v.vm_id, v.config_revision, v.config.clone())
                });
            }
            Some(c) => {
                srv.send(c);
            }
            None => {}
        }
        let Some(m) = &mut self.vm_edit else {
            return;
        };
        ui.add_space(14.0);
        let current = state.vms.iter().find(|v| v.vm_id == m.vm_id);
        let stopped = alive
            && state.connected
            && current.is_some_and(|v| v.state == lightvm_proto::VmState::Stopped);
        let max_vcpu = std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
        match vm_settings::vm_settings_ui(ui, m, stopped, max_vcpu) {
            Some(vm_settings::EditOutcome::Save) => {
                srv.send(ServiceCommand::UpdateVm {
                    vm_id: m.vm_id,
                    revision: m.revision,
                    config: Box::new(m.config.clone()),
                });
                self.vm_edit = None;
            }
            Some(vm_settings::EditOutcome::Cancel) => self.vm_edit = None,
            None => {}
        }
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
        theme::group_title(ui, "If LightVM closes unexpectedly");
        if theme::card(ui, |ui| {
            settings::policy_ui(ui, &mut self.settings.ui_loss_policy)
        }) {
            self.change_policy();
        }

        ui.add_space(14.0);
        theme::group_title(ui, "Rendering");
        let active = self.rendering.backend();
        if theme::card(ui, |ui| {
            settings::rendering_ui(ui, &mut self.settings.rendering, &active)
        }) {
            self.save_settings();
        }

        if self.simulated_console && self.live.is_none() {
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

    fn console_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(live) = &mut self.live {
            live.ui(ui, ctx);
            return;
        }
        if !self.simulated_console {
            page(ui, "lightvm_page_console", |ui| {
                theme::page_header(ui, "Console", CONSOLE_DESCRIPTION, |_| {});
                theme::card(ui, |ui| {
                    empty_state(
                        ui,
                        "No console connected.",
                        "The console of VMs started from the Dashboard arrives in F2 (supervisor display transport, ADR-0013). For the F0 spike console, start LightVM with --vnc-socket and --qmp-socket.",
                    );
                });
            });
            return;
        }
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
    /// linear in the pixels). The copy to `ColorImage` allocates a new buffer
    /// per update: acceptable in the spike, to be revisited with the real
    /// display client.
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

/// Description under the Dashboard title.
const DASHBOARD_DESCRIPTION: &str = "Virtual machines of the local catalog.";

/// Description under the Console title when no console is connected.
const CONSOLE_DESCRIPTION: &str = "Screen of the guest, with keyboard and mouse.";

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
fn empty_state(ui: &mut egui::Ui, message: &str, hint: &str) {
    let p = theme::Palette::of(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(14.0);
        ui.label(egui::RichText::new(message).color(p.text));
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

/// Text and tone of the state of a VM.
pub fn vm_state(s: lightvm_proto::VmState) -> (&'static str, theme::Tone) {
    use lightvm_proto::VmState;
    match s {
        VmState::Stopped => ("Stopped", theme::Tone::Neutral),
        VmState::Starting => ("Starting", theme::Tone::Warning),
        VmState::Running => ("Running", theme::Tone::Positive),
        VmState::Stopping => ("Stopping", theme::Tone::Warning),
        VmState::Failed => ("Error", theme::Tone::Danger),
        VmState::Recovering => ("Recovering", theme::Tone::Warning),
    }
}

fn format_mib(mib: u64) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{} GiB", mib / 1024)
    } else {
        format!("{mib} MiB")
    }
}

/// Configuration summary shown under the VM name.
fn vm_summary(c: &lightvm_proto::VmConfig) -> String {
    let os = match c.guest_os {
        lightvm_proto::GuestOs::UbuntuLtsX86_64 => "Ubuntu LTS",
        lightvm_proto::GuestOs::OtherLinuxX86_64 => "Linux",
    };
    format!(
        "{os} · {} vCPU · {} RAM · disk {} GiB",
        c.vcpus,
        format_mib(c.memory_mib),
        c.disk_gib
    )
}

/// Row of a VM: name and summary on the left, state and actions on the right.
fn vm_row(
    ui: &mut egui::Ui,
    name: &str,
    detail: &str,
    state: (&str, theme::Tone),
    actions: impl FnOnce(&mut egui::Ui),
) {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(egui::RichText::new(name).size(15.0).color(p.text));
            ui.label(egui::RichText::new(detail).small().color(p.text_weak));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            actions(ui);
            ui.add_space(6.0);
            theme::pill(ui, state.0, state.1);
        });
    });
}

/// "Virtual machines" list without supervisor: name, system and state, or
/// the declared empty state.
pub fn vm_list_ui(ui: &mut egui::Ui, rows: &[VmRow]) {
    theme::group_title(ui, "Virtual machines");
    theme::card(ui, |ui| {
        if rows.is_empty() {
            empty_state(
                ui,
                "No VM registered.",
                "The catalog and the guided creation arrive in F2.",
            );
            return;
        }
        for (i, r) in rows.iter().enumerate() {
            if i > 0 {
                theme::divider(ui);
            }
            let tone = if r.running {
                theme::Tone::Positive
            } else {
                theme::Tone::Neutral
            };
            vm_row(ui, &r.name, &r.os, (&r.state, tone), |_| {});
        }
    });
}

/// Dashboard with the supervisor (F1-11): connection state, catalog VMs with
/// start, stop and settings, Emergency Stop. Returns the chosen action; it
/// executes nothing by itself, so it can be tested without processes.
pub fn service_ui(
    ui: &mut egui::Ui,
    state: &ServiceState,
    pid: u32,
    alive: bool,
) -> Option<ServiceCommand> {
    let p = theme::Palette::of(ui);
    let mut choice = None;
    let active = alive && state.connected;
    theme::page_header(ui, "Dashboard", DASHBOARD_DESCRIPTION, |ui| {
        let stop = theme::danger(ui, "Emergency Stop");
        if ui
            .add_enabled(active, stop)
            .on_hover_text("Immediately terminates all VMs and revokes the agent sessions")
            .clicked()
        {
            choice = Some(ServiceCommand::EmergencyStop);
        }
        if ui
            .add_enabled(active, egui::Button::new("Refresh"))
            .clicked()
        {
            choice = Some(ServiceCommand::Refresh);
        }
    });

    let (connection, tone) = connection_state(alive, state);
    theme::card(ui, |ui| {
        let caption = |ui: &mut egui::Ui, t: &str| {
            ui.label(egui::RichText::new(t).small().color(p.text_weak));
        };
        egui::Grid::new("lightvm_supervisor_summary")
            .num_columns(4)
            .min_col_width(150.0)
            .spacing([28.0, 4.0])
            .show(ui, |ui| {
                caption(ui, &format!("Supervisor · PID {pid}"));
                caption(ui, "Backend");
                caption(ui, "Acceleration");
                caption(ui, "If the UI closes");
                ui.end_row();
                theme::status(ui, connection, tone).on_hover_text(&state.connection);
                ui.label(state.backend.as_deref().unwrap_or("—"));
                ui.label(state.accelerator.as_deref().unwrap_or("not detected"));
                ui.label(
                    state
                        .policy
                        .map_or_else(|| "—".into(), settings::describe_policy),
                );
                ui.end_row();
            });
    });
    if alive && !state.connected && tone == theme::Tone::Danger {
        ui.add_space(8.0);
        theme::notice(ui, &state.connection, theme::Tone::Danger);
    }
    if let Some(e) = &state.last_outcome {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!("Last action: {e}"))
                .small()
                .color(p.text_weak),
        );
    }

    ui.add_space(18.0);
    theme::group_title(ui, "Virtual machines");
    theme::card(ui, |ui| {
        if state.vms.is_empty() {
            empty_state(
                ui,
                "No VM in the supervisor's catalog.",
                "To create one: lightvm-desktop --open-vm <name> --create",
            );
            return;
        }
        for (i, v) in state.vms.iter().enumerate() {
            if i > 0 {
                theme::divider(ui);
            }
            ui.push_id(v.vm_id, |ui| {
                vm_row(
                    ui,
                    v.name.as_str(),
                    &vm_summary(&v.config),
                    vm_state(v.state),
                    |ui| {
                        use lightvm_proto::VmState;
                        match v.state {
                            VmState::Stopped => {
                                if ui
                                    .add_enabled(active, theme::primary(ui, "Start"))
                                    .clicked()
                                {
                                    choice = Some(ServiceCommand::Start(v.vm_id));
                                }
                            }
                            VmState::Failed => {
                                ui.add_enabled(false, egui::Button::new("Start"))
                                    .on_disabled_hover_text(
                                        "VM in error: the supervisor brings it back to Stopped",
                                    );
                            }
                            s => {
                                if ui
                                    .add_enabled(
                                        active && s == VmState::Running,
                                        egui::Button::new("Stop"),
                                    )
                                    .clicked()
                                {
                                    choice = Some(ServiceCommand::Stop(v.vm_id));
                                }
                            }
                        }
                        if ui
                            .add_enabled(active, theme::subtle("Edit"))
                            .on_hover_text(
                                "Resources and limits of the VM (editable while the VM is stopped)",
                            )
                            .clicked()
                        {
                            choice = Some(ServiceCommand::OpenSettings(v.vm_id));
                        }
                    },
                );
            });
        }
    });
    choice
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
