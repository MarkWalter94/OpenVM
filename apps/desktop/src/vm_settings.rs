//! Settings of a VM (ADR-0017): resources (vCPU, RAM, disk), attached media
//! (installation and data ISO from the approved registry) and execution
//! limits (CPU cap, memory margin, storage budget) and the network (ADR-0033:
//! mode and endpoint list). They can be changed only while the VM is stopped;
//! the disk can only grow. Sliders without a value box and choice menus; the
//! only text fields are those of the network endpoints (name, host, port). The limits are enforced by the
//! supervisor, the UI only proposes them.

use lightvm_proto::{
    MAX_MEMORY_MARGIN_MIB, MIN_CPU_MAX_PERCENT, MIN_MEMORY_MARGIN_MIB, MediaId, MediaKind,
    NetworkEndpoint, NetworkProfile, VmConfig, VmId, VmLimits,
};

use crate::service::MediaState;

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
    /// Text of the port field of each endpoint (parsed into the
    /// configuration as the user types).
    pub ports: Vec<String>,
}

impl VmEdit {
    /// New edit starting from the current configuration.
    pub fn new(vm_id: VmId, revision: u64, config: VmConfig) -> Self {
        Self {
            vm_id,
            revision,
            original: config.clone(),
            ports: config
                .endpoints
                .iter()
                .map(|e| e.port.to_string())
                .collect(),
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
        tools: Default::default(),
        name: lightvm_proto::VmName::parse(name)
            .map_err(|e| format!("invalid VM name \"{name}\": {e}"))?,
        guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: NetworkProfile::Offline,
        endpoints: Vec::new(),
        install_media: None,
        data_media: None,
        firmware: lightvm_proto::Firmware::Uefi,
        limits: VmLimits::default(),
        imported: None,
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

/// Short description of an approved media item, or of an identifier no
/// longer in the registry.
pub fn media_name(media: &MediaState, id: MediaId) -> String {
    media.records.iter().find(|m| m.id == id).map_or_else(
        || "medium no longer in the registry".to_owned(),
        |m| {
            format!(
                "{} ({})",
                m.declared_origin.as_str(),
                crate::dashboard::format_bytes(m.size_bytes)
            )
        },
    )
}

/// Row of the Media group: attached medium, menu to pick another approved
/// one of the same kind, "Remove".
fn media_row(
    ui: &mut egui::Ui,
    label: &str,
    description: &str,
    slot: &mut Option<MediaId>,
    kind: MediaKind,
    media: &MediaState,
) {
    let p = theme::Palette::of(ui);
    theme::row(ui, label, Some(description), |ui| {
        ui.add_space(4.0);
        let current = slot.map_or_else(|| "none".to_owned(), |id| media_name(media, id));
        ui.add(egui::Label::new(egui::RichText::new(&current).color(p.text)).wrap());
        ui.horizontal(|ui| {
            let r = egui::ComboBox::from_id_salt(("lightvm_media", label))
                .width(220.0)
                .selected_text("Choose another…")
                .show_ui(ui, |ui| {
                    for m in media.of_kind(kind) {
                        ui.selectable_value(slot, Some(m.id), media_name(media, m.id));
                    }
                });
            theme::accessible_name(
                &r.response,
                WidgetType::ComboBox,
                &format!("Choose the {}", label.to_lowercase()),
            );
            let r = ui.add_enabled(slot.is_some(), theme::subtle("Remove"));
            theme::accessible_name(
                &r,
                WidgetType::Button,
                &format!("Remove the {}", label.to_lowercase()),
            );
            if r.clicked() {
                *slot = None;
            }
        });
    });
}

/// Text of a network mode in the menu.
pub fn network_mode_text(m: NetworkProfile) -> &'static str {
    match m {
        NetworkProfile::Offline => "Offline (no network adapter)",
        NetworkProfile::Nat => "NAT (manual use only)",
        NetworkProfile::Endpoints => "Endpoint allowlist",
        NetworkProfile::FilteredInternet => "Filtered Internet (experimental)",
    }
}

/// What is wrong with the network settings, if anything (the checks of the
/// supervisor, run here to keep "Save" disabled).
pub fn network_error(c: &VmConfig) -> Option<String> {
    if c.network != NetworkProfile::Offline && c.guest_os.is_aarch64() {
        return Some("Windows 11 ARM64 guests can only be offline.".into());
    }
    lightvm_proto::validate_network(c.network, &c.endpoints)
        .err()
        .map(|e| e.message)
}

/// Network group (ADR-0033): the mode, and the endpoints of the allowlist
/// (name used inside the guest, host and TCP port of the real service; the
/// virtual address is assigned here).
fn network_ui(ui: &mut egui::Ui, c: &mut VmConfig, ports: &mut Vec<String>) {
    let p = theme::Palette::of(ui);
    group_caption(ui, "Network");
    ports.resize(c.endpoints.len(), String::new());
    theme::row(
        ui,
        "Mode",
        Some(
            "Agent sessions never use NAT: they get the endpoints granted to them, or no network.",
        ),
        |ui| {
            let r = egui::ComboBox::from_id_salt("lightvm_network_mode")
                .selected_text(network_mode_text(c.network))
                .show_ui(ui, |ui| {
                    for m in [
                        NetworkProfile::Offline,
                        NetworkProfile::Nat,
                        NetworkProfile::Endpoints,
                        NetworkProfile::FilteredInternet,
                    ] {
                        ui.selectable_value(&mut c.network, m, network_mode_text(m));
                    }
                });
            theme::accessible_name(&r.response, WidgetType::ComboBox, "Network mode");
        },
    );
    match c.network {
        NetworkProfile::Offline => {}
        NetworkProfile::Nat => {
            theme::notice(
                ui,
                "NAT: the guest reaches this computer, the local network and the Internet, like VMware NAT. While an agent session uses the VM it starts with the session's endpoints or without network, and a session cannot start while the VM runs with NAT.",
                Tone::Warning,
            );
        }
        NetworkProfile::FilteredInternet => {
            theme::notice(
                ui,
                "Filtered Internet: only the exact DNS destinations below on TCP 80 or 443. Public IPv4 only; this computer, LAN, non-default host routes, IPv6 and UDP are blocked. Requires Windows. Each new connection and persistent connection is checked against host routes; an allowed destination can receive guest data. Experimental: full VPN and browser qualification is pending.",
                Tone::Warning,
            );
        }
        NetworkProfile::Endpoints => {
            theme::notice(
                ui,
                "Endpoint allowlist: the guest reaches only the endpoints below (TCP only), by name or by virtual address; nothing else on this computer, the local network or the Internet. Every endpoint receives whatever the guest sends to it.",
                Tone::Accent,
            );
        }
    }
    if c.network == NetworkProfile::Offline {
        return;
    }
    let filtered = c.network == NetworkProfile::FilteredInternet;
    let mut remove = None;
    for (i, e) in c.endpoints.iter_mut().enumerate() {
        ui.horizontal_wrapped(|ui| {
            let name = ui.add(
                egui::TextEdit::singleline(&mut e.name)
                    .hint_text(if filtered {
                        "www.example.com"
                    } else {
                        "name, e.g. sql.lan"
                    })
                    .desired_width(130.0),
            );
            theme::accessible_name(
                &name,
                WidgetType::TextEdit,
                &format!("Name of endpoint {}", i + 1),
            );
            if filtered {
                e.host = e.name.clone();
            } else {
                ui.label("\u{2192}");
                let host = ui.add(
                    egui::TextEdit::singleline(&mut e.host)
                        .hint_text("host, e.g. 127.0.0.1")
                        .desired_width(150.0),
                );
                theme::accessible_name(
                    &host,
                    WidgetType::TextEdit,
                    &format!("Host of endpoint {}", i + 1),
                );
            }
            ui.label(":");
            let port = ui.add(
                egui::TextEdit::singleline(&mut ports[i])
                    .hint_text("port")
                    .desired_width(56.0),
            );
            theme::accessible_name(
                &port,
                WidgetType::TextEdit,
                &format!("Port of endpoint {}", i + 1),
            );
            e.port = ports[i].trim().parse().unwrap_or(0);
            ui.label(
                egui::RichText::new(format!("guest {}", e.virtual_ip))
                    .small()
                    .color(p.text_weak),
            );
            let r = ui.add(theme::subtle("Remove"));
            theme::accessible_name(
                &r,
                WidgetType::Button,
                &format!("Remove endpoint {}", i + 1),
            );
            if r.clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        c.endpoints.remove(i);
        ports.remove(i);
    }
    ui.horizontal(|ui| {
        let next = lightvm_proto::next_endpoint_ip(&c.endpoints);
        let full = c.endpoints.len() >= lightvm_proto::MAX_NETWORK_ENDPOINTS || next.is_none();
        if ui
            .add_enabled(!full, theme::subtle("Add endpoint"))
            .clicked()
            && let Some(ip) = next
        {
            c.endpoints.push(NetworkEndpoint {
                name: String::new(),
                host: String::new(),
                port: 0,
                virtual_ip: ip,
            });
            ports.push(String::new());
        }
    });
    if let Some(e) = network_error(c) {
        theme::notice(ui, &e, Tone::Danger);
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Only TCP is carried. SQL Server must listen on a fixed TCP port: a named instance found through the SQL Browser (UDP 1434) is not reachable.",
            )
            .small()
            .color(p.text_weak),
        )
        .wrap(),
    );
}

/// VM settings panel. `stopped`: the VM is stopped and editable; `max_vcpu`:
/// logical processors of the host; `media`: approved media, for the Media
/// group.
pub fn vm_settings_ui(
    ui: &mut egui::Ui,
    m: &mut VmEdit,
    stopped: bool,
    max_vcpu: u32,
    media: &MediaState,
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
            if let Some(h) = c.imported {
                // ADR-0030: fixed at import, shown read-only and sent back
                // unchanged.
                theme::divider(ui);
                group_caption(ui, "Imported from VMware");
                let p = theme::Palette::of(ui);
                let mut text = format!(
                    "Disk controller: {} · display: {}",
                    crate::vmware_import::controller_label(h.disk_controller),
                    crate::vmware_import::display_label(h.display, c.guest_os)
                );
                if let Some(k) = h.cores_per_socket.filter(|k| *k > 1) {
                    text.push_str(&format!(
                        " · {k} cores per socket (the vCPUs stay a multiple of {k})"
                    ));
                }
                ui.add(egui::Label::new(egui::RichText::new(text).color(p.text_weak)).wrap());
                if h.storage_driver_preparation {
                    theme::notice(
                        ui,
                        crate::vmware_import::STORAGE_DRIVER_WARNING,
                        Tone::Warning,
                    );
                }
            }
            theme::divider(ui);
            group_caption(ui, "Media");
            media_row(
                ui,
                "Installation media",
                "Boots first while attached: remove it once the system is installed.",
                &mut c.install_media,
                MediaKind::InstallIso,
                media,
            );
            media_row(
                ui,
                "Data media",
                "Read-only ISO visible to the guest.",
                &mut c.data_media,
                MediaKind::DataIso,
                media,
            );
            theme::divider(ui);
            network_ui(ui, c, &mut m.ports);
            theme::divider(ui);
            tools_ui(ui, &mut c.tools);
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
                let valid = network_error(&m.config).is_none();
                let save = theme::primary(ui, "Save settings");
                if ui.add_enabled(stopped && changed && valid, save).clicked() {
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

/// Explicit host integration; folders are selected by the user, never an agent.
fn tools_ui(ui: &mut egui::Ui, tools: &mut lightvm_proto::ToolsConfig) {
    group_caption(ui, "LightVM Tools");
    ui.checkbox(&mut tools.enabled, "Enable LightVM Tools for this VM");
    ui.label("Install the guest component from the LIGHTVM_TOOLS CD. Agent access requires a separate session approval.");
    ui.add_enabled_ui(tools.enabled, |ui| {
        ui.checkbox(&mut tools.clipboard, "Allow clipboard text and images");
        ui.checkbox(
            &mut tools.files_transfer,
            "Allow transfers to guest inbox and from guest outbox",
        );
        ui.checkbox(
            &mut tools.ui_inspect,
            "Allow inspection of guest application controls",
        );
        ui.add_enabled(
            tools.ui_inspect,
            egui::Checkbox::new(
                &mut tools.ui_act,
                "Allow actions on guest application controls",
            ),
        );
        if !tools.ui_inspect {
            tools.ui_act = false;
        }
        ui.label("Shared host folders (read-only unless Write is ticked)");
        let mut remove = None;
        for (i, share) in tools.shares.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut share.name)
                            .hint_text("Share name")
                            .desired_width(120.0),
                    );
                    ui.label(&share.root);
                    ui.checkbox(&mut share.writable, "Write");
                    if ui.button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            tools.shares.remove(i);
        }
        if tools.shares.len() < 32
            && ui.button("Add shared folder…").clicked()
            && let Some(mut picker) = crate::native_folder_picker()
            && let Some(folder) = picker("Choose a host folder to share with this VM")
        {
            let name = (1..=1000)
                .map(|n| format!("Share{n}"))
                .find(|n| !tools.shares.iter().any(|s| s.name.eq_ignore_ascii_case(n)))
                .unwrap_or_else(|| "Share".into());
            tools.shares.push(lightvm_proto::ToolShare {
                name,
                root: folder.to_string_lossy().into_owned(),
                writable: false,
            });
        }
        if let Err(e) = tools.validate() {
            theme::notice(ui, &e.message, Tone::Warning);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VmConfig {
        VmConfig {
            tools: Default::default(),
            name: lightvm_proto::VmName::parse("test").unwrap(),
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vcpus: 2,
            memory_mib: 2048,
            disk_gib: 16,
            network: NetworkProfile::Offline,
            endpoints: Vec::new(),
            install_media: None,
            data_media: None,
            firmware: lightvm_proto::Firmware::Uefi,
            limits: VmLimits::default(),
            imported: None,
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
