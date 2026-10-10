//! Settings of a VM (ADR-0017): resources (vCPU, RAM, disk) and execution
//! limits (CPU cap, memory margin, storage budget). They can be changed only
//! while the VM is stopped; the disk can only grow. No text fields: sliders
//! without a value box and choice menus. The limits are enforced by the
//! supervisor, the UI only proposes them.

use lightvm_proto::{
    MAX_MEMORY_MARGIN_MIB, MIN_CPU_MAX_PERCENT, MIN_MEMORY_MARGIN_MIB, VmConfig, VmId, VmLimits,
};

use egui::WidgetType;

use crate::theme::{self, Tone};

/// Minimum guest memory offered (the core refuses smaller values).
pub const RAM_MIN_MIB: u64 = 512;
/// Maximum guest memory offered by the slider; the core still enforces the
/// host limits.
pub const RAM_MAX_MIB: u64 = 256 * 1024;
/// Maximum disk offered by the slider, in GiB.
pub const DISK_MAX_GIB: u64 = 4096;
/// Memory margins offered, in MiB.
pub const MEMORY_MARGINS_MIB: [u32; 8] = [256, 512, 768, 1024, 1536, 2048, 4096, 8192];
/// Storage budgets offered, in GiB (besides "none").
pub const STORAGE_BUDGETS_GIB: [u64; 8] = [5, 10, 20, 50, 100, 200, 500, 1000];

/// Edit of a VM in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmEdit {
    /// VM.
    pub vm_id: VmId,
    /// Revision the edit is based on.
    pub revision: u64,
    /// Configuration when the panel was opened.
    pub original: VmConfig,
    /// Edited configuration.
    pub config: VmConfig,
}

impl VmEdit {
    /// New edit starting from the current configuration.
    pub fn new(vm_id: VmId, revision: u64, config: VmConfig) -> Self {
        Self {
            vm_id,
            revision,
            original: config.clone(),
            config,
        }
    }
}

/// Outcome of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// Save the edited configuration.
    Save,
    /// Close without saving.
    Cancel,
}

/// Changes requested on the command line (`--open-vm` with the resource and
/// limit options); missing fields stay unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmChanges {
    /// vCPU.
    pub vcpus: Option<u32>,
    /// RAM in MiB.
    pub memory_mib: Option<u64>,
    /// Disk in GiB.
    pub disk_gib: Option<u64>,
    /// CPU cap as a percentage.
    pub cpu_max_percent: Option<u8>,
    /// Memory margin in MiB.
    pub memory_margin_mib: Option<u32>,
    /// Storage budget: `Some(None)` removes it.
    pub storage_budget_gib: Option<Option<u64>>,
}

impl VmChanges {
    /// No change requested.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Applies the changes to a configuration.
    pub fn apply(&self, c: &mut VmConfig) {
        if let Some(v) = self.vcpus {
            c.vcpus = v;
        }
        if let Some(v) = self.memory_mib {
            c.memory_mib = v;
        }
        if let Some(v) = self.disk_gib {
            c.disk_gib = v;
        }
        if let Some(v) = self.cpu_max_percent {
            c.limits.cpu_max_percent = v;
        }
        if let Some(v) = self.memory_margin_mib {
            c.limits.memory_margin_mib = v;
        }
        if let Some(v) = self.storage_budget_gib {
            c.limits.storage_budget_gib = v;
        }
    }
}

/// Default configuration of a VM created from the command line: Ubuntu LTS,
/// 2 vCPU, 2048 MiB, 16 GiB disk, offline profile, UEFI, default limits.
pub fn default_config(name: &str) -> Result<VmConfig, String> {
    Ok(VmConfig {
        name: lightvm_proto::VmName::parse(name)
            .map_err(|e| format!("invalid VM name \"{name}\": {e}"))?,
        guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: lightvm_proto::NetworkProfile::Offline,
        install_media: None,
        data_media: None,
        firmware: lightvm_proto::Firmware::Uefi,
        limits: VmLimits::default(),
    })
}

/// Slider without a value box, followed by the value as text.
fn slider<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    name: &str,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    adjust: impl FnOnce(egui::Slider<'_>) -> egui::Slider<'_>,
    text: impl Fn(T) -> String,
) -> egui::Response {
    let p = theme::Palette::of(ui);
    ui.horizontal(|ui| {
        // `Edits`: opening the panel does not correct the saved value (out of
        // range or off step it stays as it is until the user moves the
        // slider), so there is no phantom change to save.
        let r = ui.add(
            adjust(egui::Slider::new(value, range))
                .clamping(egui::SliderClamping::Edits)
                .show_value(false),
        );
        // Accessible name: without text next to it the slider would be anonymous.
        let (enabled, v) = (r.enabled(), value.to_f64());
        r.widget_info(|| egui::WidgetInfo::slider(enabled, v, name));
        ui.label(egui::RichText::new(text(*value)).color(p.text));
        r
    })
    .inner
}

/// Caption of a group of rows inside the card.
fn group_caption(ui: &mut egui::Ui, text: &str) {
    let p = theme::Palette::of(ui);
    ui.label(egui::RichText::new(text).small().color(p.text_weak));
}

/// VM settings panel. `stopped`: the VM is stopped and editable; `max_vcpu`:
/// logical processors of the host.
pub fn vm_settings_ui(
    ui: &mut egui::Ui,
    m: &mut VmEdit,
    stopped: bool,
    max_vcpu: u32,
) -> Option<EditOutcome> {
    let mut outcome = None;
    theme::group_title(
        ui,
        &format!("Resources and limits of {}", m.original.name.as_str()),
    );
    theme::card(ui, |ui| {
        if !stopped {
            theme::notice(
                ui,
                "Stop the VM to change resources and limits.",
                Tone::Warning,
            );
            ui.add_space(6.0);
        }
        let disk_min = m.original.disk_gib;
        ui.add_enabled_ui(stopped, |ui| {
            let c = &mut m.config;
            group_caption(ui, "Resources");
            theme::row(ui, "Virtual processors", None, |ui| {
                slider(
                    ui,
                    "Virtual processors",
                    &mut c.vcpus,
                    1..=max_vcpu.max(1),
                    |s| s,
                    |v| format!("{v} vCPU"),
                )
            });
            theme::row(ui, "Memory (RAM)", None, |ui| {
                slider(
                    ui,
                    "Memory",
                    &mut c.memory_mib,
                    RAM_MIN_MIB..=RAM_MAX_MIB,
                    |s| s.step_by(256.0).logarithmic(true),
                    |v| format!("{v} MiB"),
                )
            });
            theme::row(ui, "Disk", Some("Can only grow."), |ui| {
                slider(
                    ui,
                    "Disk",
                    &mut c.disk_gib,
                    disk_min..=DISK_MAX_GIB.max(disk_min),
                    |s| s.logarithmic(true),
                    |v| format!("{v} GiB"),
                )
            });
            theme::divider(ui);
            group_caption(ui, "Execution limits, enforced by the supervisor");
            let l = &mut c.limits;
            theme::row(
                ui,
                "Maximum CPU",
                Some("Percentage of the vCPU time."),
                |ui| {
                    slider(
                        ui,
                        "Maximum CPU",
                        &mut l.cpu_max_percent,
                        MIN_CPU_MAX_PERCENT..=100,
                        |s| s.step_by(5.0),
                        |v| {
                            if v >= 100 {
                                "no cap (100 %)".into()
                            } else {
                                format!("{v} %")
                            }
                        },
                    )
                },
            );
            theme::row(
                ui,
                "Memory margin",
                Some("Memory granted to the process beyond the guest RAM."),
                |ui| {
                    let r = egui::ComboBox::from_id_salt("lightvm_memory_margin")
                        .selected_text(format!("{} MiB", l.memory_margin_mib))
                        .show_ui(ui, |ui| {
                            for v in MEMORY_MARGINS_MIB {
                                if (MIN_MEMORY_MARGIN_MIB..=MAX_MEMORY_MARGIN_MIB).contains(&v) {
                                    ui.selectable_value(
                                        &mut l.memory_margin_mib,
                                        v,
                                        format!("{v} MiB"),
                                    );
                                }
                            }
                        });
                    theme::accessible_name(&r.response, WidgetType::ComboBox, "Memory margin");
                },
            );
            let budget =
                |b: Option<u64>| b.map_or_else(|| "none".to_owned(), |g| format!("{g} GiB"));
            theme::row(
                ui,
                "Storage budget",
                Some("Maximum space taken by the VM files."),
                |ui| {
                    let r = egui::ComboBox::from_id_salt("lightvm_storage_budget")
                        .selected_text(budget(l.storage_budget_gib))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut l.storage_budget_gib, None, "none");
                            for g in STORAGE_BUDGETS_GIB {
                                ui.selectable_value(
                                    &mut l.storage_budget_gib,
                                    Some(g),
                                    format!("{g} GiB"),
                                );
                            }
                        });
                    theme::accessible_name(&r.response, WidgetType::ComboBox, "Storage budget");
                },
            );
        });
        theme::divider(ui);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(stopped, theme::subtle("Default limits"))
                .clicked()
            {
                m.config.limits = VmLimits::default();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let changed = m.config != m.original;
                let save = theme::primary(ui, "Save settings");
                if ui.add_enabled(stopped && changed, save).clicked() {
                    outcome = Some(EditOutcome::Save);
                }
                if ui.button("Close").clicked() {
                    outcome = Some(EditOutcome::Cancel);
                }
            });
        });
    });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VmConfig {
        VmConfig {
            name: lightvm_proto::VmName::parse("test").unwrap(),
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vcpus: 2,
            memory_mib: 2048,
            disk_gib: 16,
            network: lightvm_proto::NetworkProfile::Offline,
            install_media: None,
            data_media: None,
            firmware: lightvm_proto::Firmware::Uefi,
            limits: VmLimits::default(),
        }
    }

    #[test]
    fn changes_from_command_line() {
        let mut c = cfg();
        assert!(VmChanges::default().is_empty());
        let m = VmChanges {
            vcpus: Some(4),
            disk_gib: Some(32),
            cpu_max_percent: Some(50),
            storage_budget_gib: Some(Some(20)),
            ..VmChanges::default()
        };
        assert!(!m.is_empty());
        m.apply(&mut c);
        assert_eq!((c.vcpus, c.memory_mib, c.disk_gib), (4, 2048, 32));
        assert_eq!(c.limits.cpu_max_percent, 50);
        assert_eq!(c.limits.memory_margin_mib, 1024);
        assert_eq!(c.limits.storage_budget_gib, Some(20));
        VmChanges {
            storage_budget_gib: Some(None),
            ..VmChanges::default()
        }
        .apply(&mut c);
        assert_eq!(c.limits.storage_budget_gib, None);
    }
}
