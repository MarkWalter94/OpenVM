//! UI settings, changed only from the Settings page and saved by the app in a
//! JSON file (the user does not edit it by hand): supervisor (catalog, QEMU
//! distribution, automatic start), fate of the VMs if the UI terminates
//! without a controlled stop (ADR-0015 part B), preferred rendering backend
//! (ADR-0014; applies from the next start).
//!
//! The file lives in the user profile (`%LOCALAPPDATA%\LightVM\ui-settings.json`
//! on Windows, `$XDG_CONFIG_HOME/lightvm/ui-settings.json` or
//! `~/.config/lightvm/…` elsewhere) or where `--settings` says. A missing file
//! counts as default settings; an unreadable or invalid file is reported and
//! replaced by the defaults, without blocking the start.
//!
//! Compatibility: earlier versions wrote `impostazioni-ui.json` with Italian
//! keys. If the new file does not exist but the legacy one does, the legacy
//! file is read ([`UiSettings::load_or_legacy`]); the old keys are accepted as
//! serde aliases. Saving always writes the new file.

use std::path::{Path, PathBuf};

use egui::WidgetType;
use lightvm_proto::ipc::ParentLossPolicy;
use serde::{Deserialize, Serialize};

use crate::theme::{self, Tone};

/// File name in the user profile.
pub const FILE: &str = "ui-settings.json";
/// File name used by earlier versions, read only if [`FILE`] does not exist.
pub const LEGACY_FILE: &str = "impostazioni-ui.json";

/// Name of the default catalog folder in the user profile.
pub const CATALOG_DIR: &str = "catalog";
/// Default catalog folder of earlier versions, still used if it exists and
/// [`CATALOG_DIR`] does not (the VMs it contains are not moved).
pub const LEGACY_CATALOG_DIR: &str = "catalogo";

/// Waits offered for the orderly shutdown, in seconds (no text field in the
/// UI).
pub const SHUTDOWN_WAITS_S: [u32; 6] = [5, 10, 30, 60, 120, 300];

/// "Keep the VMs running" selectable from the UI: only once a new UI can
/// reconnect to the supervisor (ADR-0015 part B, criterion 4).
pub const KEEP_VMS_AVAILABLE: bool = false;

/// Default wait when the orderly shutdown is chosen.
pub const DEFAULT_WAIT_S: u32 = 10;

/// Preferred rendering backend. The chosen backend is tried first; the others
/// remain as fallback, so an unavailable choice does not prevent the window
/// from opening. WARP chosen explicitly is the only attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderingPreference {
    /// glow → wgpu (hardware) → WARP (default: glow stays within the memory
    /// target, `F1-05`).
    #[default]
    #[serde(alias = "automatico")]
    Automatic,
    /// glow first (like automatic).
    Glow,
    /// wgpu on hardware first, then glow and WARP. Higher memory (D-12).
    Wgpu,
    /// Software rasterizer only (degraded mode).
    Warp,
}

impl RenderingPreference {
    /// All choices, in the order shown.
    pub const ALL: [Self; 4] = [Self::Automatic, Self::Glow, Self::Wgpu, Self::Warp];

    /// Equivalent value of `--renderer`.
    pub fn as_argument(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Glow => "glow,wgpu,warp",
            Self::Wgpu => "wgpu,glow,warp",
            Self::Warp => "warp",
        }
    }

    /// Label shown in Settings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic: glow, then wgpu, then WARP (default)",
            Self::Glow => "glow (OpenGL) first",
            Self::Wgpu => "wgpu (DX12/Vulkan) first: uses more memory",
            Self::Warp => "Software only (WARP): slow, for sessions without a GPU",
        }
    }
}

/// Persistent UI settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// Fate of the VMs when the UI is lost (default: terminate them).
    #[serde(alias = "politica_perdita_ui")]
    pub ui_loss_policy: ParentLossPolicy,
    /// Preferred rendering backend (applies from the next start).
    pub rendering: RenderingPreference,
    /// Start the supervisor when LightVM opens.
    #[serde(alias = "avvio_supervisor")]
    pub start_supervisor: bool,
    /// Catalog folder; `None` = default in the user profile.
    #[serde(alias = "catalogo")]
    pub catalog: Option<PathBuf>,
    /// Approved QEMU distribution; `None` = none (VM start refused by the
    /// supervisor with a clear message).
    pub qemu_dir: Option<PathBuf>,
    /// Interface theme.
    #[serde(alias = "tema")]
    pub theme: crate::UiTheme,
    /// Keyboard layout configured in the guests (console).
    pub keyboard_layout: crate::keymap::KeyboardLayout,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            ui_loss_policy: ParentLossPolicy::default(),
            rendering: RenderingPreference::default(),
            start_supervisor: true,
            catalog: None,
            qemu_dir: None,
            theme: crate::UiTheme::Dark,
            keyboard_layout: crate::keymap::KeyboardLayout::Us,
        }
    }
}

/// Default catalog in the user profile: [`CATALOG_DIR`], or the folder of
/// earlier versions if only that one exists.
pub fn default_catalog() -> Option<PathBuf> {
    let dir = default_path()?.parent()?.to_path_buf();
    let new = dir.join(CATALOG_DIR);
    let legacy = dir.join(LEGACY_CATALOG_DIR);
    Some(if !new.exists() && legacy.is_dir() {
        legacy
    } else {
        new
    })
}

impl UiSettings {
    /// Effective catalog (chosen or default).
    pub fn effective_catalog(&self) -> Option<PathBuf> {
        self.catalog.clone().or_else(default_catalog)
    }

    /// Effective QEMU folder: the chosen one, else the one found on this
    /// host (see [`detected_qemu_dir`]).
    pub fn effective_qemu_dir(&self) -> Option<PathBuf> {
        self.qemu_dir.clone().or_else(detected_qemu_dir)
    }
}

/// QEMU found on this host, used when no folder is chosen (the Settings page
/// shows it as "(found automatically)"). The supervisor still verifies the
/// binaries against the approved hashes, so a QEMU found here that is not the
/// approved distribution is refused when the supervisor starts.
///
/// - macOS: Homebrew (`/opt/homebrew` on Apple Silicon, `/usr/local` on
///   Intel), the folder whose `bin` holds `qemu-system-aarch64`.
/// - Windows: the first folder holding [`WINDOWS_QEMU_EXE`] among
///   [`windows_qemu_candidates`].
/// - Linux: `None`. No physical Linux host has been verified, so there is no
///   location to trust.
pub fn detected_qemu_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return ["/opt/homebrew", "/usr/local"]
            .into_iter()
            .map(PathBuf::from)
            .find(|d| d.join("bin").join("qemu-system-aarch64").is_file());
    }
    if cfg!(windows) {
        return find_windows_qemu(&WindowsQemuEnv::from_host(), |d| {
            d.join(WINDOWS_QEMU_EXE).is_file()
        });
    }
    None
}

/// QEMU executable that marks a QEMU folder on Windows.
pub const WINDOWS_QEMU_EXE: &str = "qemu-system-x86_64.exe";

/// Folder of the verified per-user extraction (user guide, option A), under
/// `%LOCALAPPDATA%\Programs`; also the folder of the project's lab QEMU.
pub const WINDOWS_USER_QEMU_DIR: &str = "qemu-11.1.0";

/// Ancestors of the executable examined when looking for the cargo `target`
/// directory of a development build.
pub const DEV_BUILD_MAX_DEPTH: usize = 6;

/// Host values the Windows search reads, kept apart so the search can be
/// tested without touching the real host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsQemuEnv {
    /// `%LOCALAPPDATA%`.
    pub local_app_data: Option<PathBuf>,
    /// `%ProgramFiles%`.
    pub program_files: Option<PathBuf>,
    /// `PATH` as is (`;`-separated, entries possibly quoted).
    pub path: Option<String>,
    /// Running executable ([`std::env::current_exe`]).
    pub current_exe: Option<PathBuf>,
}

impl WindowsQemuEnv {
    /// Values of the running process.
    pub fn from_host() -> Self {
        Self {
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            program_files: std::env::var_os("ProgramFiles").map(PathBuf::from),
            path: std::env::var_os("PATH").map(|p| p.to_string_lossy().into_owned()),
            current_exe: std::env::current_exe().ok(),
        }
    }
}

/// Folders where QEMU is looked for on Windows, in order:
/// 1. `%LOCALAPPDATA%\Programs\qemu-11.1.0` (user guide, option A);
/// 2. `%ProgramFiles%\qemu` (installer and winget, options B and C);
/// 3. each directory of `PATH`;
/// 4. development builds only: the project's lab QEMU, see
///    [`dev_lab_qemu_dir`].
pub fn windows_qemu_candidates(env: &WindowsQemuEnv) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = &env.local_app_data {
        out.push(d.join("Programs").join(WINDOWS_USER_QEMU_DIR));
    }
    if let Some(d) = &env.program_files {
        out.push(d.join("qemu"));
    }
    if let Some(p) = &env.path {
        out.extend(split_windows_path(p));
    }
    if let Some(d) = env.current_exe.as_deref().and_then(dev_lab_qemu_dir) {
        out.push(d);
    }
    out
}

/// First of [`windows_qemu_candidates`] for which `has_qemu` is true (on the
/// real host: the folder holds [`WINDOWS_QEMU_EXE`]).
pub fn find_windows_qemu(
    env: &WindowsQemuEnv,
    has_qemu: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    windows_qemu_candidates(env)
        .into_iter()
        .find(|d| has_qemu(d))
}

/// Directories of a Windows `PATH`: `;`-separated, quotes removed, empty
/// entries skipped.
pub fn split_windows_path(path: &str) -> Vec<PathBuf> {
    path.split(';')
        .map(|e| e.trim().replace('"', ""))
        .filter(|e| !e.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Development convenience: when `exe` lies inside a cargo `target`
/// directory (looked for among its nearest [`DEV_BUILD_MAX_DEPTH`]
/// ancestors), the project's lab QEMU,
/// `<folder containing target>\lab\qemu\qemu-11.1.0`. An installed LightVM is
/// not inside a `target` directory and gets `None`. The caller checks that
/// the folder holds QEMU.
pub fn dev_lab_qemu_dir(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .take(DEV_BUILD_MAX_DEPTH)
        .find(|a| {
            a.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("target"))
        })
        .and_then(Path::parent)
        .map(|root| root.join("lab").join("qemu").join(WINDOWS_USER_QEMU_DIR))
}

/// Actions requested by the Supervisor section; the app executes them (the
/// folder picker is native and not available in headless tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorAction {
    /// Choose the catalog folder.
    ChooseCatalog,
    /// Go back to the default catalog.
    DefaultCatalog,
    /// Move the whole catalog, VMs included, to another folder.
    MoveCatalog,
    /// Choose the QEMU distribution folder.
    ChooseQemu,
    /// No QEMU distribution.
    RemoveQemu,
    /// Apply the settings by restarting the supervisor.
    Restart,
    /// Stop the supervisor.
    Stop,
}

/// Supervisor state shown in the section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupervisorUiState {
    /// Status text (connected, not started, error).
    pub description: String,
    /// Tone of the indicator, the same as in the sidebar.
    pub tone: Tone,
    /// The supervisor is running.
    pub running: bool,
    /// Restart or stop in progress: actions disabled.
    pub in_progress: bool,
    /// Running VMs that a restart would stop.
    pub running_vms: usize,
    /// The supervisor settings differ from the ones in use.
    pub pending_changes: bool,
    /// The folder picker is available on this platform.
    pub folder_picker: bool,
    /// Catalog and QEMU given on the command line (`--catalog`), in use
    /// instead of the ones chosen here.
    pub command_line: Option<(PathBuf, Option<PathBuf>)>,
    /// "Move catalog…" is available, or why not (shown on hover).
    pub move_catalog: Result<(), String>,
}

fn display_path(p: Option<&Path>, empty: &str) -> String {
    p.map_or_else(|| empty.to_owned(), |p| p.display().to_string())
}

/// Supervisor section of the Settings page: paths shown as non-editable
/// text, chosen with the native folder picker; automatic start; restart to
/// apply. Returns the requested action and, separately, whether a value
/// editable here (automatic start) changed.
pub fn supervisor_ui(
    ui: &mut egui::Ui,
    s: &mut UiSettings,
    state: &SupervisorUiState,
) -> (Option<SupervisorAction>, bool) {
    let mut action = None;
    theme::row(ui, "Status", None, |ui| {
        theme::status(ui, &state.description, state.tone)
    });
    if let Some((catalog, qemu)) = &state.command_line {
        ui.add_space(4.0);
        theme::notice(
            ui,
            &format!(
                "In use from the command line: catalog {}, QEMU {}. The choices below apply to starts without --catalog.",
                catalog.display(),
                display_path(qemu.as_deref(), "none")
            ),
            Tone::Accent,
        );
    }
    theme::divider(ui);
    theme::row(
        ui,
        "VM catalog",
        Some("Configurations and disks of the virtual machines."),
        |ui| {
            theme::path_value(
                ui,
                &display_path(s.effective_catalog().as_deref(), "cannot be determined"),
            );
            ui.horizontal(|ui| {
                let r = ui.add_enabled(state.folder_picker, egui::Button::new("Browse…"));
                theme::accessible_name(&r, WidgetType::Button, "Choose the catalog folder");
                if r.clicked() {
                    action = Some(SupervisorAction::ChooseCatalog);
                }
                let r = ui.add_enabled(s.catalog.is_some(), theme::subtle("Default"));
                theme::accessible_name(&r, WidgetType::Button, "Go back to the default catalog");
                if r.clicked() {
                    action = Some(SupervisorAction::DefaultCatalog);
                }
                let r = ui.add_enabled(
                    state.move_catalog.is_ok() && !state.in_progress,
                    egui::Button::new("Move catalog…"),
                );
                theme::accessible_name(
                    &r,
                    WidgetType::Button,
                    "Move the catalog and its VMs to another folder",
                );
                let r = match &state.move_catalog {
                    Err(why) => r.on_disabled_hover_text(why.as_str()),
                    Ok(()) => r,
                };
                if r.clicked() {
                    action = Some(SupervisorAction::MoveCatalog);
                }
            });
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Browse… and Default only change the folder LightVM uses: the VMs already in the catalog are not moved and stay in the old folder. Move catalog… takes them along.",
                    )
                    .color(theme::Palette::of(ui).text_weak),
                )
                .wrap(),
            );
        },
    );
    theme::divider(ui);
    theme::row(
        ui,
        "QEMU distribution",
        Some("Approved qemu-system and qemu-img; the supervisor verifies their hashes."),
        |ui| {
            match s.qemu_dir.as_deref() {
                Some(d) => {
                    theme::path_value(ui, &d.display().to_string());
                }
                None => match detected_qemu_dir() {
                    Some(d) => {
                        theme::path_value(ui, &format!("{} (found automatically)", d.display()));
                    }
                    None => {
                        theme::status(ui, "not set: VMs cannot be started", Tone::Warning);
                    }
                },
            }
            ui.horizontal(|ui| {
                let r = ui.add_enabled(state.folder_picker, egui::Button::new("Browse…"));
                theme::accessible_name(&r, WidgetType::Button, "Choose the QEMU folder");
                if r.clicked() {
                    action = Some(SupervisorAction::ChooseQemu);
                }
                let r = ui.add_enabled(s.qemu_dir.is_some(), theme::subtle("None"));
                theme::accessible_name(&r, WidgetType::Button, "No QEMU distribution");
                if r.clicked() {
                    action = Some(SupervisorAction::RemoveQemu);
                }
            });
        },
    );
    theme::divider(ui);
    let changed = theme::row(ui, "Startup", None, |ui| {
        ui.checkbox(
            &mut s.start_supervisor,
            "Start the supervisor when LightVM opens",
        )
        .changed()
    });
    if !state.folder_picker {
        ui.add_space(4.0);
        theme::notice(
            ui,
            "Folder picker not available on this platform: use --catalog and --qemu-dir.",
            Tone::Neutral,
        );
    }
    if state.pending_changes && state.running {
        ui.add_space(4.0);
        theme::notice(
            ui,
            "Changes not applied yet: restart the supervisor.",
            Tone::Warning,
        );
    }
    if state.running && state.running_vms > 0 {
        ui.add_space(4.0);
        let text = if state.running_vms == 1 {
            "Restart or stop shuts down the running VM (controlled stop).".to_owned()
        } else {
            format!(
                "Restart or stop shuts down the {} running VMs (controlled stop).",
                state.running_vms
            )
        };
        theme::notice(ui, &text, Tone::Warning);
    }
    theme::divider(ui);
    ui.horizontal(|ui| {
        let text = if state.running {
            "Apply and restart the supervisor"
        } else {
            "Start the supervisor"
        };
        let start = theme::primary(ui, text);
        if ui.add_enabled(!state.in_progress, start).clicked() {
            action = Some(SupervisorAction::Restart);
        }
        if ui
            .add_enabled(
                state.running && !state.in_progress,
                egui::Button::new("Stop the supervisor"),
            )
            .clicked()
        {
            action = Some(SupervisorAction::Stop);
        }
    });
    (action, changed)
}

/// Default path in the user profile, if it can be determined.
pub fn default_path() -> Option<PathBuf> {
    profile_dir().map(|d| d.join(FILE))
}

/// Path of the legacy settings file in the user profile, if it can be
/// determined.
pub fn legacy_default_path() -> Option<PathBuf> {
    profile_dir().map(|d| d.join(LEGACY_FILE))
}

pub(crate) fn profile_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("LightVM"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("lightvm"))
    }
}

impl UiSettings {
    /// Reads the file. Missing → defaults without error; unreadable or
    /// invalid → defaults with the description of the problem.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (Self::default(), Some(format!("settings not read: {e}")));
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(i) if i.ui_loss_policy == ParentLossPolicy::KeepVms && !KEEP_VMS_AVAILABLE => (
                UiSettings {
                    ui_loss_policy: ParentLossPolicy::default(),
                    ..i
                },
                Some("\"keep the VMs running\" is not available without reconnection: using \"terminate the VMs immediately\"".into()),
            ),
            Ok(i) => match i.ui_loss_policy.validate() {
                Ok(_) => (i, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("invalid settings: {}", e.message)),
                ),
            },
            Err(e) => (Self::default(), Some(format!("invalid settings: {e}"))),
        }
    }

    /// Like [`Self::load`], but if `path` does not exist and `legacy` does,
    /// reads `legacy` (settings file of earlier versions). Saving must still
    /// go to `path`.
    pub fn load_or_legacy(path: &Path, legacy: &Path) -> (Self, Option<String>) {
        if !path.exists() && legacy.exists() {
            Self::load(legacy)
        } else {
            Self::load(path)
        }
    }

    /// Writes the file atomically (temporary file alongside, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

/// Readable description of the policy.
pub fn describe_policy(p: ParentLossPolicy) -> String {
    match p {
        ParentLossPolicy::KillVms => "terminate the VMs immediately".into(),
        ParentLossPolicy::ShutdownVms { powerdown_s } => {
            format!("orderly shutdown (wait {powerdown_s} s), then termination")
        }
        ParentLossPolicy::KeepVms => "keep the VMs running".into(),
    }
}

/// Content of the card about the UI loss policy. Returns `true` if the user
/// changed the choice. Only radio buttons and a menu: no text field.
pub fn policy_ui(ui: &mut egui::Ui, p: &mut ParentLossPolicy) -> bool {
    let before = *p;
    let colours = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new(
            "Crash or forced termination of the window: what to do with the running VMs.",
        )
        .color(colours.text_weak),
    );
    ui.add_space(4.0);
    let wait = match *p {
        ParentLossPolicy::ShutdownVms { powerdown_s } => powerdown_s,
        _ => DEFAULT_WAIT_S,
    };
    if ui
        .radio(
            *p == ParentLossPolicy::KillVms,
            "Terminate the VMs immediately (default)",
        )
        .clicked()
    {
        *p = ParentLossPolicy::KillVms;
    }
    ui.horizontal(|ui| {
        let chosen = matches!(*p, ParentLossPolicy::ShutdownVms { .. });
        if ui
            .radio(chosen, "Orderly shutdown, then termination")
            .clicked()
        {
            *p = ParentLossPolicy::ShutdownVms { powerdown_s: wait };
        }
        ui.add_enabled_ui(chosen, |ui| {
            let r = egui::ComboBox::from_id_salt("lightvm_shutdown_wait")
                .width(110.0)
                .selected_text(format!("wait {wait} s"))
                .show_ui(ui, |ui| {
                    for s in SHUTDOWN_WAITS_S {
                        if ui.selectable_label(wait == s, format!("{s} s")).clicked() {
                            *p = ParentLossPolicy::ShutdownVms { powerdown_s: s };
                        }
                    }
                });
            theme::accessible_name(&r.response, WidgetType::ComboBox, "Shutdown wait");
        });
    });
    // ROADMAP 6.1 and ADR-0015 B4: keeping the VMs running is allowed only
    // once a new UI can reconnect to the supervisor, not available yet.
    if ui
        .add_enabled(
            KEEP_VMS_AVAILABLE,
            egui::RadioButton::new(
                *p == ParentLossPolicy::KeepVms,
                "Keep the VMs running (requires UI reconnection, not available yet)",
            ),
        )
        .on_disabled_hover_text(
            "Without reconnection no UI could control those VMs any more (ADR-0015 part B).",
        )
        .clicked()
    {
        *p = ParentLossPolicy::KeepVms;
    }
    *p != before
}

/// Content of the host capacity card: what the supervisor measured, and the
/// resulting per-VM maxima offered by the wizard. Read only.
pub fn host_ui(ui: &mut egui::Ui, host: Option<lightvm_proto::HostCapacity>, connected: bool) {
    let colours = theme::Palette::of(ui);
    let Some(h) = host else {
        let text = if connected {
            "Not reported by the supervisor: the limits are checked when a VM is created or started."
        } else {
            "Available when the supervisor is connected."
        };
        ui.label(egui::RichText::new(text).color(colours.text_weak));
        return;
    };
    let mib = crate::dashboard::format_mib;
    let yes_no = |b: bool| if b { "available" } else { "not available" };
    let max = crate::wizard::Bounds::new(Some(h));
    let rows: [(&str, String); 6] = [
        ("Logical processors", h.logical_cpus.to_string()),
        (
            "Memory",
            format!(
                "{} total, {} reserved for the host",
                mib(h.total_memory_mib),
                mib(h.reserved_memory_mib)
            ),
        ),
        (
            "Catalog storage",
            format!(
                "{} GiB free, {} GiB kept free",
                h.free_storage_gib, h.reserved_storage_gib
            ),
        ),
        (
            "Hardware acceleration",
            if h.accelerator_available {
                "available".into()
            } else {
                "not available: VMs cannot be started".into()
            },
        ),
        (
            "Screenshots and input",
            format!(
                "screenshots {}, input {}",
                yes_no(h.screenshot_available),
                yes_no(h.input_available)
            ),
        ),
        (
            "Maximum per VM",
            format!(
                "{} vCPU, {} memory, {} GiB disk",
                max.max_vcpus,
                mib(max.max_memory_mib),
                max.max_disk_gib
            ),
        ),
    ];
    for (i, (k, v)) in rows.iter().enumerate() {
        if i > 0 {
            theme::divider(ui);
        }
        theme::row(ui, k, None, |ui| {
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(v).color(colours.text)).wrap());
        });
    }
}

/// Features of this version, stated plainly. Read only: none of them can be
/// enabled from the UI.
pub const SECURITY_FACTS: [(&str, &str); 3] = [
    ("Network", "Offline only: VMs have no network adapter."),
    (
        "Clipboard",
        "Not shared: nothing is copied between host and guest.",
    ),
    (
        "Shared folders",
        "Not available: files reach the guest only as read-only data ISOs.",
    ),
];

/// What agents can do, stated plainly (managed on the AI Workspace page).
pub const AGENT_SESSIONS_FACT: &str = "Only after you approve a request on the AI Workspace page, for one VM and a limited time. Agent access is off every time LightVM starts.";

/// Unattended access as the Security card states it: the live grant's VM
/// name and the grant, or none.
#[derive(Clone, Copy, Debug)]
pub struct UnattendedSummary<'a> {
    /// Name of the grant's VM.
    pub vm_name: &'a str,
    /// The live grant.
    pub grant: &'a lightvm_proto::StandingGrant,
}

/// Content of the Security card. `unattended`: the live unattended access
/// (standing grant, ADR-0039), if any.
pub fn security_ui(ui: &mut egui::Ui, unattended: Option<UnattendedSummary<'_>>) {
    let colours = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new("Fixed in this version: these features cannot be turned on.")
            .color(colours.text_weak),
    );
    ui.add_space(4.0);
    for (i, (k, v)) in SECURITY_FACTS.iter().enumerate() {
        if i > 0 {
            theme::divider(ui);
        }
        theme::row(ui, k, None, |ui| {
            ui.horizontal_wrapped(|ui| {
                theme::pill(ui, "Unavailable", Tone::Neutral);
                ui.add(egui::Label::new(egui::RichText::new(*v).color(colours.text)).wrap());
            });
        });
    }
    theme::divider(ui);
    theme::row(ui, "Agent sessions", None, |ui| {
        ui.horizontal_wrapped(|ui| {
            theme::pill(ui, "Your approval", Tone::Neutral);
            ui.add(
                egui::Label::new(egui::RichText::new(AGENT_SESSIONS_FACT).color(colours.text))
                    .wrap(),
            );
        });
    });
    theme::divider(ui);
    theme::row(ui, "Unattended agent access", None, |ui| {
        ui.horizontal_wrapped(|ui| {
            let text = match unattended {
                Some(u) => {
                    let suspended =
                        u.grant.state == lightvm_proto::StandingGrantState::Suspended;
                    if suspended {
                        theme::pill(ui, "Suspended", Tone::Warning);
                    } else {
                        theme::pill(ui, "Active", Tone::Warning);
                    }
                    format!(
                        "On {} until {}: its agent's sessions are approved without asking you{}. Review or revoke it on the AI Workspace page, Unattended access; the Emergency Stop revokes it too.",
                        u.vm_name,
                        crate::baselines::format_time(u.grant.expires_at),
                        if suspended {
                            " (suspended: the VM configuration changed, confirm it again to resume)"
                        } else {
                            ""
                        }
                    )
                }
                None => {
                    theme::pill(ui, "Off", Tone::Neutral);
                    "Off: every agent session needs your approval. It can be created for one VM, for at most 7 days, on the AI Workspace page, Unattended access.".to_owned()
                }
            };
            ui.add(egui::Label::new(egui::RichText::new(text).color(colours.text)).wrap());
        });
    });
}

/// Content of the Console card: declared guest keyboard layout. Returns
/// `true` if it changed.
pub fn console_ui(ui: &mut egui::Ui, layout: &mut crate::keymap::KeyboardLayout) -> bool {
    let before = *layout;
    theme::row(
        ui,
        "Guest keyboard layout",
        Some("Keys are sent by physical position: set the same layout inside the guest."),
        |ui| {
            for l in crate::keymap::KeyboardLayout::ALL {
                ui.radio_value(layout, l, l.description());
            }
        },
    );
    theme::divider(ui);
    theme::row(ui, "Console shortcuts", None, |ui| {
        let colours = theme::Palette::of(ui);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!(
                "Click the console to capture keyboard and mouse; {} alone releases them. {}: full screen on or off. {}: leave the console (exit full screen, release keyboard and mouse). Neither reaches the guest.",
                crate::console::RELEASE_CHORD,
                crate::console::FULLSCREEN_CHORD,
                crate::console::LEAVE_CHORD
            ))
            .color(colours.text),
        );
    });
    *layout != before
}

/// Content of the rendering backend card. Returns `true` if the choice
/// changes. `active` is the backend in use in this run.
pub fn rendering_ui(ui: &mut egui::Ui, p: &mut RenderingPreference, active: &str) -> bool {
    let before = *p;
    let colours = theme::Palette::of(ui);
    theme::row(ui, "In use now", None, |ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(active).color(colours.text));
    });
    theme::divider(ui);
    theme::row(
        ui,
        "Preferred backend",
        Some("Applies from the next start; --renderer on the command line takes precedence."),
        |ui| {
            for choice in RenderingPreference::ALL {
                if ui.radio(*p == choice, choice.label()).clicked() {
                    *p = choice;
                }
            }
        },
    );
    *p != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "lightvm-settings-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        d.join(FILE)
    }

    /// Paths built with `join`, so the tests hold on every host.
    fn p(parts: &[&str]) -> PathBuf {
        parts.iter().collect()
    }

    fn windows_env() -> WindowsQemuEnv {
        WindowsQemuEnv {
            local_app_data: Some(p(&["C:", "Users", "u", "AppData", "Local"])),
            program_files: Some(p(&["C:", "Program Files"])),
            path: Some(r#"C:\Windows;;"D:\tools\qemu bin" ; E:\bin;"#.to_owned()),
            current_exe: Some(p(&["E:", "proj", "target", "release", "lightvm.exe"])),
        }
    }

    #[test]
    fn windows_candidates_are_in_order() {
        assert_eq!(
            windows_qemu_candidates(&windows_env()),
            vec![
                p(&[
                    "C:",
                    "Users",
                    "u",
                    "AppData",
                    "Local",
                    "Programs",
                    "qemu-11.1.0"
                ]),
                p(&["C:", "Program Files", "qemu"]),
                PathBuf::from(r"C:\Windows"),
                PathBuf::from(r"D:\tools\qemu bin"),
                PathBuf::from(r"E:\bin"),
                p(&["E:", "proj", "lab", "qemu", "qemu-11.1.0"]),
            ]
        );
        assert!(windows_qemu_candidates(&WindowsQemuEnv::default()).is_empty());
    }

    #[test]
    fn first_folder_with_qemu_wins() {
        let env = windows_env();
        let all = windows_qemu_candidates(&env);
        // Every candidate holds QEMU: the per-user extraction wins.
        assert_eq!(find_windows_qemu(&env, |_| true), Some(all[0].clone()));
        // Only Program Files and a PATH entry: Program Files wins.
        let found = find_windows_qemu(&env, |d| {
            d == p(&["C:", "Program Files", "qemu"]) || d == Path::new(r"E:\bin")
        });
        assert_eq!(found, Some(p(&["C:", "Program Files", "qemu"])));
        // Only a PATH entry.
        let found = find_windows_qemu(&env, |d| d == Path::new(r"D:\tools\qemu bin"));
        assert_eq!(found, Some(PathBuf::from(r"D:\tools\qemu bin")));
        // Only the lab QEMU of the development build.
        let lab = p(&["E:", "proj", "lab", "qemu", "qemu-11.1.0"]);
        assert_eq!(find_windows_qemu(&env, |d| d == lab), Some(lab.clone()));
    }

    #[test]
    fn nothing_found_gives_none() {
        assert_eq!(find_windows_qemu(&windows_env(), |_| false), None);
        assert_eq!(
            find_windows_qemu(&WindowsQemuEnv::default(), |_| true),
            None
        );
    }

    #[test]
    fn path_is_split_on_semicolons() {
        assert_eq!(
            split_windows_path(r#" C:\a ;"C:\Program Files\b";;C:\c;"#),
            vec![
                PathBuf::from(r"C:\a"),
                PathBuf::from(r"C:\Program Files\b"),
                PathBuf::from(r"C:\c"),
            ]
        );
        assert!(split_windows_path("").is_empty());
        assert!(split_windows_path(";;").is_empty());
    }

    #[test]
    fn lab_qemu_only_for_development_builds() {
        let lab = p(&["E:", "OpenVM", "lab", "qemu", "qemu-11.1.0"]);
        // Release build and a custom CARGO_TARGET_DIR under target.
        assert_eq!(
            dev_lab_qemu_dir(&p(&["E:", "OpenVM", "target", "release", "lightvm.exe"])),
            Some(lab.clone())
        );
        assert_eq!(
            dev_lab_qemu_dir(&p(&[
                "E:",
                "OpenVM",
                "target",
                "regression-build",
                "debug",
                "deps",
                "t.exe"
            ])),
            Some(lab)
        );
        // Installed copy: no target directory.
        assert_eq!(
            dev_lab_qemu_dir(&p(&["C:", "Program Files", "LightVM", "lightvm.exe"])),
            None
        );
        // The walk is bounded: a target directory too far up is ignored.
        let mut deep = p(&["E:", "x", "target"]);
        for _ in 0..DEV_BUILD_MAX_DEPTH {
            deep.push("d");
        }
        deep.push("lightvm.exe");
        assert_eq!(dev_lab_qemu_dir(&deep), None);
        // The executable itself named target does not count.
        assert_eq!(dev_lab_qemu_dir(&p(&["E:", "x", "target"])), None);
    }

    #[test]
    fn missing_counts_as_default() {
        let (i, err) = UiSettings::load(&tmp("missing"));
        assert_eq!(i.ui_loss_policy, ParentLossPolicy::KillVms);
        assert!(err.is_none());
    }

    #[test]
    fn save_and_reload() {
        let p = tmp("save");
        let i = UiSettings {
            ui_loss_policy: ParentLossPolicy::ShutdownVms { powerdown_s: 30 },
            rendering: RenderingPreference::Wgpu,
            start_supervisor: false,
            catalog: Some(PathBuf::from(r"D:\vm\catalog")),
            qemu_dir: Some(PathBuf::from(r"D:\qemu")),
            theme: crate::UiTheme::Light,
            keyboard_layout: crate::keymap::KeyboardLayout::It,
        };
        i.save(&p).unwrap();
        assert_eq!(UiSettings::load(&p), (i, None));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn missing_fields_count_as_default() {
        let p = tmp("partial");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"rendering":"wgpu"}"#).unwrap();
        let (i, err) = UiSettings::load(&p);
        assert!(err.is_none());
        assert_eq!(i.rendering, RenderingPreference::Wgpu);
        assert_eq!(i.ui_loss_policy, ParentLossPolicy::KillVms);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A file written by earlier versions (`impostazioni-ui.json`, Italian
    /// keys and variant names) is read when the new file does not exist;
    /// saving then writes the new file with the new keys.
    #[test]
    fn legacy_file_is_loaded_when_new_one_is_missing() {
        let p = tmp("legacy");
        let dir = p.parent().unwrap().to_path_buf();
        let legacy = dir.join(LEGACY_FILE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &legacy,
            r#"{
                "politica_perdita_ui": {"kind": "shutdown_vms", "powerdown_s": 60},
                "rendering": "automatico",
                "avvio_supervisor": false,
                "catalogo": "D:\\vm\\catalogo",
                "qemu_dir": "D:\\qemu",
                "tema": "light"
            }"#,
        )
        .unwrap();
        let expected = UiSettings {
            ui_loss_policy: ParentLossPolicy::ShutdownVms { powerdown_s: 60 },
            rendering: RenderingPreference::Automatic,
            start_supervisor: false,
            catalog: Some(PathBuf::from(r"D:\vm\catalogo")),
            qemu_dir: Some(PathBuf::from(r"D:\qemu")),
            theme: crate::UiTheme::Light,
            keyboard_layout: crate::keymap::KeyboardLayout::Us,
        };
        let (i, err) = UiSettings::load_or_legacy(&p, &legacy);
        assert!(err.is_none(), "{err:?}");
        assert_eq!(i, expected);

        i.save(&p).unwrap();
        let saved = std::fs::read_to_string(&p).unwrap();
        assert!(saved.contains("\"ui_loss_policy\""), "{saved}");
        assert!(saved.contains("\"automatic\""), "{saved}");
        // With both files present, the new one wins.
        let mut changed = expected.clone();
        changed.start_supervisor = true;
        changed.save(&p).unwrap();
        assert_eq!(UiSettings::load_or_legacy(&p, &legacy), (changed, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keep_vms_from_file_falls_back_to_default_until_reconnection_exists() {
        let p = tmp("keep");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            r#"{"ui_loss_policy":{"kind":"keep_vms"},"rendering":"glow"}"#,
        )
        .unwrap();
        let (i, err) = UiSettings::load(&p);
        assert_eq!(i.ui_loss_policy, ParentLossPolicy::KillVms);
        assert_eq!(i.rendering, RenderingPreference::Glow);
        assert!(err.is_some());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn every_preference_yields_a_valid_plan() {
        for pref in RenderingPreference::ALL {
            assert!(
                crate::render::Plan::from_args(Some(pref.as_argument()), false).is_ok(),
                "{pref:?}"
            );
        }
    }

    #[test]
    fn invalid_file_falls_back_to_default_with_error() {
        for (name, text) in [
            ("json", "not json"),
            (
                "limit",
                r#"{"ui_loss_policy":{"kind":"shutdown_vms","powerdown_s":0}}"#,
            ),
        ] {
            let p = tmp(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
            let (i, err) = UiSettings::load(&p);
            assert_eq!(i, UiSettings::default(), "{name}");
            assert!(err.is_some(), "{name}");
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }
}
