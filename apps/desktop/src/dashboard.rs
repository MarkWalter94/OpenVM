//! Dashboard (ROADMAP F2): supervisor state and the catalog VMs with system,
//! state, configured vCPU/RAM, virtual disk size and space used, and the
//! actions allowed by the state. The view executes nothing: it returns the
//! chosen [`DashboardAction`], so it can be tested without processes.

use egui::WidgetType;
use lightvm_proto::ipc::{StartPhase, StartProgress, VmSummary};
use lightvm_proto::{GuestOs, VmId, VmState};

use crate::service::{BaselineList, ServiceCommand, ServiceState};
use crate::theme::{self, Tone};

/// Description under the Dashboard title.
pub const DESCRIPTION: &str = "Virtual machines of the local catalog.";

/// Actions that ask for a confirmation before being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Forced stop (like pulling the plug).
    ForceStop(VmId),
    /// Guest reset.
    Reset(VmId),
    /// Deletion of the VM and its disk.
    Delete(VmId),
    /// Removal of the installation media (after the installation).
    DetachInstall(VmId),
}

impl Confirm {
    /// VM concerned.
    pub fn vm_id(self) -> VmId {
        match self {
            Self::ForceStop(v) | Self::Reset(v) | Self::Delete(v) | Self::DetachInstall(v) => v,
        }
    }
}

/// Choice made in the Dashboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardAction {
    /// Command to send to the supervisor as is.
    Command(ServiceCommand),
    /// Open the "New VM" wizard.
    NewVm,
    /// Open the console of a running VM.
    Console(VmId),
    /// Open the settings panel of a VM.
    Edit(VmId),
    /// Open the Baselines page of a VM (F3).
    Baselines(VmId),
    /// Ask a confirmation.
    Confirm(Confirm),
}

/// Text and tone of the state of a VM.
pub fn vm_state(s: VmState) -> (&'static str, Tone) {
    match s {
        VmState::Stopped => ("Stopped", Tone::Neutral),
        VmState::Starting => ("Starting", Tone::Warning),
        VmState::Running => ("Running", Tone::Positive),
        VmState::Stopping => ("Stopping", Tone::Warning),
        VmState::Failed => ("Error", Tone::Danger),
        VmState::Recovering => ("Recovering", Tone::Warning),
    }
}

/// Name of the declared guest system.
pub fn os_label(os: GuestOs) -> &'static str {
    match os {
        GuestOs::UbuntuLtsX86_64 => "Ubuntu LTS",
        GuestOs::OtherLinuxX86_64 => "Other Linux",
    }
}

/// Memory in MiB, readable.
pub fn format_mib(mib: u64) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{} GiB", mib / 1024)
    } else if mib >= 1024 {
        format!("{:.1} GiB", mib as f64 / 1024.0)
    } else {
        format!("{mib} MiB")
    }
}

/// Size in bytes, readable (binary units).
pub fn format_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Configuration line shown under the VM name.
pub fn vm_details(v: &VmSummary) -> String {
    let c = &v.config;
    let disk = match v.disk_used_bytes {
        Some(b) => format!("disk {} GiB, {} used", c.disk_gib, format_bytes(b)),
        None => format!("disk {} GiB", c.disk_gib),
    };
    format!(
        "{} · {} vCPU · {} RAM · {disk}",
        os_label(c.guest_os),
        c.vcpus,
        format_mib(c.memory_mib)
    )
}

/// Text of the checks of a start in progress, with the percentage while
/// hashing: `Verifying baseline… 42 %`. The text carries the meaning; the
/// progress bar only repeats it.
pub fn start_progress_text(p: &StartProgress) -> String {
    let what = match p.phase {
        StartPhase::VerifyingMedia => "Verifying installation media…",
        StartPhase::VerifyingBaseline => "Verifying baseline…",
        StartPhase::Launching => return "Launching…".to_owned(),
    };
    match start_fraction(p) {
        Some(f) => format!("{what} {} %", (f * 100.0).floor() as u32),
        None => what.to_owned(),
    }
}

/// Fraction of the bytes verified, if known.
pub fn start_fraction(p: &StartProgress) -> Option<f32> {
    (p.total_bytes > 0 && p.phase != StartPhase::Launching)
        .then(|| (p.verified_bytes.min(p.total_bytes) as f64 / p.total_bytes as f64) as f32)
}

/// Space on the host above which the VM disk is considered to hold an
/// installed system (a fresh disk occupies a few MiB).
pub const INSTALLED_DISK_BYTES: u64 = 1 << 30;

/// The VM is stopped, still has the installation media attached and its
/// disk already holds data (an installed system): it would boot the
/// installer again. The service does not report the last run of a stopped
/// VM, so the disk usage is the signal.
pub fn install_media_pending(v: &VmSummary) -> bool {
    v.state == VmState::Stopped
        && v.config.install_media.is_some()
        && (v.run_id.is_some() || v.disk_used_bytes.is_some_and(|b| b >= INSTALLED_DISK_BYTES))
}

/// Width of the actions column of a row.
const ACTIONS_WIDTH: f32 = 292.0;

fn action_button(
    ui: &mut egui::Ui,
    enabled: bool,
    button: egui::Button<'_>,
    name: &str,
    hover: &str,
) -> bool {
    let r = ui
        .add_enabled(enabled, button.min_size(egui::vec2(72.0, 0.0)))
        .on_hover_text(hover)
        .on_disabled_hover_text(hover);
    theme::accessible_name(&r, WidgetType::Button, name);
    r.clicked()
}

/// The supervisor can handle baselines (unknown capacity: let it decide).
pub fn baselines_available(state: &ServiceState) -> bool {
    state.host.is_none_or(|h| h.baseline_available)
}

fn vm_row(
    ui: &mut egui::Ui,
    v: &VmSummary,
    active: bool,
    baselines: (Option<&BaselineList>, bool),
    choice: &mut Option<DashboardAction>,
) {
    let p = theme::Palette::of(ui);
    let name = v.name.as_str();
    let (state_text, tone) = vm_state(v.state);
    let (list, baselines_available) = baselines;
    // A baseline operation in progress: the VM is busy.
    let busy = v.baselines.operation;
    let busy_hover = "A baseline operation is in progress on this VM";
    ui.horizontal(|ui| {
        ui.set_min_height(46.0);
        let left = (ui.available_width() - ACTIONS_WIDTH - 12.0).max(150.0);
        ui.allocate_ui_with_layout(
            egui::vec2(left, 46.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(left);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(name).size(15.0).color(p.text))
                            .truncate(),
                    );
                    theme::pill(ui, state_text, tone);
                    if let Some(op) = busy {
                        theme::pill(ui, crate::baselines::operation_label(op), Tone::Warning);
                    }
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(vm_details(v))
                            .small()
                            .color(p.text_weak),
                    )
                    .wrap(),
                );
                if let Some(progress) = v.start.filter(|_| v.state == VmState::Starting) {
                    let text = start_progress_text(&progress);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if let Some(f) = start_fraction(&progress) {
                            let bar = ui.add(
                                egui::ProgressBar::new(f)
                                    .desired_width(140.0)
                                    .desired_height(6.0),
                            );
                            theme::accessible_name(&bar, WidgetType::ProgressIndicator, &text);
                        } else {
                            ui.spinner();
                        }
                        ui.label(egui::RichText::new(&text).small().color(p.text));
                    });
                }
                if let Some(line) = crate::baselines::dashboard_line(v, list) {
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).small().color(p.text_weak))
                            .wrap(),
                    );
                }
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(ACTIONS_WIDTH, 46.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let running = v.state == VmState::Running;
                // Main action: Start when stopped, orderly Stop otherwise.
                if matches!(v.state, VmState::Stopped | VmState::Failed) {
                    let hover = if v.state == VmState::Failed {
                        "VM in error: the supervisor brings it back to Stopped"
                    } else if busy.is_some() {
                        busy_hover
                    } else {
                        "Start the VM"
                    };
                    if action_button(
                        ui,
                        active && v.state == VmState::Stopped && busy.is_none(),
                        theme::primary(ui, "Start"),
                        &format!("Start {name}"),
                        hover,
                    ) {
                        *choice = Some(DashboardAction::Command(ServiceCommand::Start(v.vm_id)));
                    }
                } else if action_button(
                    ui,
                    active && running,
                    egui::Button::new("Stop"),
                    &format!("Stop {name}"),
                    "Orderly shutdown: the guest is asked to power off",
                ) {
                    *choice = Some(DashboardAction::Command(ServiceCommand::Stop(v.vm_id)));
                }
                if action_button(
                    ui,
                    active && running,
                    egui::Button::new("Console"),
                    &format!("Open the console of {name}"),
                    "Screen, keyboard and mouse of the running VM",
                ) {
                    *choice = Some(DashboardAction::Console(v.vm_id));
                }
                let r = ui
                    .add_enabled(active, theme::subtle("Edit"))
                    .on_hover_text(if busy.is_some() {
                        busy_hover
                    } else {
                        "Resources and limits (editable while the VM is stopped)"
                    });
                theme::accessible_name(&r, WidgetType::Button, &format!("Edit {name}"));
                if r.clicked() {
                    *choice = Some(DashboardAction::Edit(v.vm_id));
                }
                let more = ui.add_enabled_ui(active, |ui| {
                    ui.menu_button("More", |ui| {
                        ui.set_min_width(180.0);
                        let force = ui
                            .add_enabled(v.state.has_process(), egui::Button::new("Force stop…"))
                            .on_hover_text("Terminates the VM at once, like pulling the plug");
                        if force.clicked() {
                            *choice = Some(DashboardAction::Confirm(Confirm::ForceStop(v.vm_id)));
                        }
                        let reset = ui
                            .add_enabled(running && v.run_id.is_some(), egui::Button::new("Reset…"))
                            .on_hover_text("Restarts the guest at once, like the reset button");
                        if reset.clicked() {
                            *choice = Some(DashboardAction::Confirm(Confirm::Reset(v.vm_id)));
                        }
                        ui.separator();
                        let b = ui
                            .add_enabled(baselines_available, egui::Button::new("Baselines…"))
                            .on_hover_text(
                                "Read-only copies of the disk taken with the VM off: create, restore, delete",
                            )
                            .on_disabled_hover_text(
                                "Not available: the supervisor has no QEMU disk image tool",
                            );
                        theme::accessible_name(
                            &b,
                            WidgetType::Button,
                            &format!("Baselines of {name}"),
                        );
                        if b.clicked() {
                            *choice = Some(DashboardAction::Baselines(v.vm_id));
                        }
                        ui.separator();
                        let delete = ui
                            .add_enabled(
                                v.state == VmState::Stopped && busy.is_none(),
                                egui::Button::new(egui::RichText::new("Delete…").color(p.danger)),
                            )
                            .on_hover_text("Removes the VM, its disk and its baselines (only when stopped)")
                            .on_disabled_hover_text("Stop the VM to delete it");
                        if delete.clicked() {
                            *choice = Some(DashboardAction::Confirm(Confirm::Delete(v.vm_id)));
                        }
                    })
                });
                theme::accessible_name(
                    &more.inner.response,
                    WidgetType::Button,
                    &format!("More actions for {name}"),
                );
            },
        );
    });
    if install_media_pending(v) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new(
                    "Installation finished? Remove the installation media so the VM boots from its disk.",
                )
                .small()
                .color(p.text_weak),
            );
            let r = ui.add_enabled(
                active && busy.is_none(),
                theme::subtle("Remove installation media…"),
            );
            theme::accessible_name(
                &r,
                WidgetType::Button,
                &format!("Remove the installation media of {name}"),
            );
            if r.clicked() {
                *choice = Some(DashboardAction::Confirm(Confirm::DetachInstall(v.vm_id)));
            }
        });
    }
}

/// Dashboard with the supervisor: connection state, catalog VMs with their
/// actions, Emergency Stop and "New VM".
pub fn dashboard_ui(
    ui: &mut egui::Ui,
    state: &ServiceState,
    pid: u32,
    alive: bool,
) -> Option<DashboardAction> {
    let p = theme::Palette::of(ui);
    let mut choice = None;
    let active = alive && state.connected;
    theme::page_header(ui, "Dashboard", DESCRIPTION, |ui| {
        let stop = theme::danger(ui, "Emergency Stop");
        if ui
            .add_enabled(active, stop)
            .on_hover_text("Immediately terminates all VMs and revokes the agent sessions")
            .clicked()
        {
            choice = Some(DashboardAction::Command(ServiceCommand::EmergencyStop));
        }
        if ui
            .add_enabled(active, egui::Button::new("Refresh"))
            .clicked()
        {
            choice = Some(DashboardAction::Command(ServiceCommand::Refresh));
        }
        if ui
            .add_enabled(active, theme::primary(ui, "New VM"))
            .on_hover_text("Guided creation: name, system, installation ISO, resources")
            .clicked()
        {
            choice = Some(DashboardAction::NewVm);
        }
    });

    let (connection, tone) = crate::connection_state(alive, state);
    theme::card(ui, |ui| {
        let caption = |ui: &mut egui::Ui, t: &str| {
            ui.label(egui::RichText::new(t).small().color(p.text_weak));
        };
        egui::Grid::new("lightvm_supervisor_summary")
            .num_columns(4)
            .min_col_width(120.0)
            .spacing([24.0, 4.0])
            .show(ui, |ui| {
                caption(ui, &format!("Supervisor · PID {pid}"));
                caption(ui, "Backend");
                caption(ui, "Acceleration");
                caption(ui, "If the UI closes");
                ui.end_row();
                theme::status(ui, connection, tone).on_hover_text(&state.connection);
                ui.label(state.backend.as_deref().unwrap_or("—"));
                ui.label(state.accelerator.as_deref().unwrap_or("not detected"));
                ui.add(
                    egui::Label::new(
                        state
                            .policy
                            .map_or_else(|| "—".into(), crate::settings::describe_policy),
                    )
                    .wrap(),
                );
                ui.end_row();
            });
    });
    if alive && !state.connected && tone == Tone::Danger {
        ui.add_space(8.0);
        theme::notice(ui, &state.connection, Tone::Danger);
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
            crate::empty_state(
                ui,
                "No virtual machines yet.",
                "Choose New VM: the wizard asks for a name, the installation ISO and the resources.",
            );
            return;
        }
        for (i, v) in state.vms.iter().enumerate() {
            if i > 0 {
                theme::divider(ui);
            }
            let baselines = (state.baselines.get(&v.vm_id), baselines_available(state));
            ui.push_id(v.vm_id, |ui| vm_row(ui, v, active, baselines, &mut choice));
        }
    });
    choice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_readable() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(
            format_bytes(3 * 1024 * 1024 * 1024 + 300 * 1024 * 1024),
            "3.3 GiB"
        );
        assert_eq!(format_bytes(200 * 1024 * 1024), "200 MiB");
        assert_eq!(format_mib(2048), "2 GiB");
        assert_eq!(format_mib(1536), "1.5 GiB");
        assert_eq!(format_mib(512), "512 MiB");
    }
}
