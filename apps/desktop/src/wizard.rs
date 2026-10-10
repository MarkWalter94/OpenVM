//! "New VM" wizard (ROADMAP F2): name, system, installation media, optional
//! data media, resources, network, summary.
//!
//! The VM name is the **only** text field of the whole interface: it is a
//! label validated live with [`VmName::parse`] and never reaches QEMU (which
//! uses `-name guest=lightvm-<run>`). Media are chosen among the approved
//! ones of the registry, or imported with the native file picker; resources
//! with sliders bounded by the host capacity; the network is offline only.
//! The view executes nothing: it returns a [`WizardAction`].

use lightvm_proto::{
    Firmware, GuestOs, HostCapacity, MediaId, MediaKind, MediaRecord, NetworkProfile,
    ResourceLimits, VmConfig, VmId, VmLimits, VmName,
};

use crate::dashboard::{format_bytes, format_mib, os_label};
use crate::service::{MediaState, RequestStatus};
use crate::theme::{self, Tone};

/// Accessible name of the name field.
pub const NAME_FIELD: &str = "VM name";

/// Smallest disk offered, in GiB.
pub const MIN_DISK_GIB: u64 = 8;
/// Smallest memory offered, in MiB.
pub const MIN_MEMORY_MIB: u64 = 512;

/// Steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// VM name.
    Name,
    /// Guest system.
    System,
    /// Installation media.
    Install,
    /// Optional data media.
    Data,
    /// vCPU, memory, disk.
    Resources,
    /// Network profile.
    Network,
    /// Summary and creation.
    Summary,
}

impl Step {
    /// All steps.
    pub const ALL: [Self; 7] = [
        Self::Name,
        Self::System,
        Self::Install,
        Self::Data,
        Self::Resources,
        Self::Network,
        Self::Summary,
    ];

    /// Title of the step.
    pub fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::System => "System",
            Self::Install => "Installation media",
            Self::Data => "Data media",
            Self::Resources => "Resources",
            Self::Network => "Network",
            Self::Summary => "Summary",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }

    fn previous(self) -> Self {
        Self::ALL[self.index().saturating_sub(1)]
    }
}

/// Limits offered by the sliders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Maximum vCPU.
    pub max_vcpus: u32,
    /// Maximum memory in MiB.
    pub max_memory_mib: u64,
    /// Maximum disk in GiB.
    pub max_disk_gib: u64,
    /// The limits come from the host capacity reported by the supervisor.
    pub from_host: bool,
}

impl Bounds {
    /// Per-VM limits of the supervisor (MVP defaults) intersected with the
    /// host capacity when reported; without it, the logical processors of
    /// this machine bound the vCPU and the supervisor checks the rest.
    pub fn new(host: Option<HostCapacity>) -> Self {
        let base = ResourceLimits::MVP_DEFAULT;
        match host {
            Some(h) => {
                let l = base.intersect(&h.as_limits());
                Self {
                    max_vcpus: l.max_vcpus.max(1),
                    max_memory_mib: l.max_memory_mib,
                    max_disk_gib: l.max_disk_gib,
                    from_host: true,
                }
            }
            None => {
                let cpus = std::thread::available_parallelism()
                    .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
                Self {
                    max_vcpus: base.max_vcpus.min(cpus).max(1),
                    max_memory_mib: base.max_memory_mib,
                    max_disk_gib: base.max_disk_gib,
                    from_host: false,
                }
            }
        }
    }

    /// The host can hold a VM at all (memory and disk above the minimum).
    pub fn sufficient(&self) -> bool {
        self.max_memory_mib >= MIN_MEMORY_MIB && self.max_disk_gib >= MIN_DISK_GIB
    }
}

/// Wizard state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wizard {
    /// Current step.
    pub step: Step,
    /// Name typed so far (validated on every change).
    pub name: String,
    /// Declared guest system.
    pub os: GuestOs,
    /// Chosen installation media.
    pub install_media: Option<MediaId>,
    /// Chosen data media (optional).
    pub data_media: Option<MediaId>,
    /// vCPU.
    pub vcpus: u32,
    /// Memory in MiB.
    pub memory_mib: u64,
    /// Disk in GiB.
    pub disk_gib: u64,
    /// Import in progress started from this wizard: kind and file name.
    pub importing: Option<(MediaKind, String)>,
    /// "Create" was pressed: waiting for the outcome.
    pub submitted: bool,
    focus_name: bool,
}

impl Wizard {
    /// New wizard with defaults fitting the host.
    pub fn new(host: Option<HostCapacity>) -> Self {
        let b = Bounds::new(host);
        Self {
            step: Step::Name,
            name: String::new(),
            os: GuestOs::UbuntuLtsX86_64,
            install_media: None,
            data_media: None,
            vcpus: 2.min(b.max_vcpus),
            memory_mib: 4096.min(b.max_memory_mib).max(MIN_MEMORY_MIB),
            disk_gib: 32.min(b.max_disk_gib).max(MIN_DISK_GIB),
            importing: None,
            submitted: false,
            focus_name: true,
        }
    }

    /// Configuration to create, if the name is valid.
    pub fn config(&self) -> Option<VmConfig> {
        Some(VmConfig {
            name: VmName::parse(self.name.clone()).ok()?,
            guest_os: self.os,
            vcpus: self.vcpus,
            memory_mib: self.memory_mib,
            disk_gib: self.disk_gib,
            network: NetworkProfile::Offline,
            install_media: self.install_media,
            data_media: self.data_media,
            firmware: Firmware::Uefi,
            limits: VmLimits::default(),
        })
    }
}

/// What the wizard asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WizardAction {
    /// Close without creating.
    Cancel,
    /// Open the native file picker and import an ISO of this kind.
    Import(MediaKind),
    /// Read the approved media again.
    RefreshMedia,
    /// Create the VM.
    Create(Box<VmConfig>),
}

/// Data the wizard shows, from the supervisor state.
#[derive(Clone, Copy, Debug)]
pub struct WizardEnv<'a> {
    /// Approved media.
    pub media: &'a MediaState,
    /// Last import.
    pub import: &'a RequestStatus<MediaRecord>,
    /// Last creation.
    pub create: &'a RequestStatus<VmId>,
    /// Host capacity, if reported.
    pub host: Option<HostCapacity>,
    /// Names already in the catalog.
    pub existing_names: &'a [String],
    /// A native file picker is available.
    pub file_picker: bool,
    /// The supervisor is connected.
    pub active: bool,
}

/// Outcome of the live validation of the name.
pub fn validate_name(name: &str, existing: &[String]) -> Result<VmName, String> {
    if name.is_empty() {
        return Err("Enter a name.".into());
    }
    let n = VmName::parse(name.to_owned()).map_err(|e| {
        let mut r = e.reason.to_owned();
        if let Some(c) = r.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        format!("{r}.")
    })?;
    if existing.iter().any(|e| e.eq_ignore_ascii_case(name)) {
        return Err("A VM with this name already exists.".into());
    }
    Ok(n)
}

fn media_label(m: &MediaRecord) -> (String, String) {
    let date: String = m.approved_at.to_string().chars().take(10).collect();
    let hex = m.sha256.to_hex();
    (
        m.declared_origin.as_str().to_owned(),
        format!(
            "{} · SHA-256 {}… · approved {date}",
            format_bytes(m.size_bytes),
            &hex[..12.min(hex.len())]
        ),
    )
}

/// Draws the wizard; returns the requested action.
pub fn wizard_ui(ui: &mut egui::Ui, w: &mut Wizard, env: &WizardEnv<'_>) -> Option<WizardAction> {
    let p = theme::Palette::of(ui);
    let mut action = None;
    theme::page_header(
        ui,
        "New VM",
        "Guided creation of a virtual machine. Nothing is created until the last step.",
        |_| {},
    );
    steps_indicator(ui, w.step);
    ui.add_space(12.0);

    let validation = validate_name(&w.name, env.existing_names);
    let bounds = Bounds::new(env.host);
    let mut go_next = false;
    theme::card(ui, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "Step {} of {}: {}",
                w.step.index() + 1,
                Step::ALL.len(),
                w.step.title()
            ))
            .size(15.0)
            .color(p.text),
        );
        ui.add_space(6.0);
        match w.step {
            Step::Name => go_next = name_step(ui, w, &validation),
            Step::System => system_step(ui, w),
            Step::Install => {
                media_step(ui, w, env, MediaKind::InstallIso, &mut action);
            }
            Step::Data => {
                media_step(ui, w, env, MediaKind::DataIso, &mut action);
            }
            Step::Resources => resources_step(ui, w, bounds),
            Step::Network => network_step(ui),
            Step::Summary => summary_step(ui, w, env),
        }
    });

    let can_next = match w.step {
        Step::Name => validation.is_ok(),
        Step::Install => w.install_media.is_some(),
        Step::Resources => bounds.sufficient(),
        _ => true,
    };
    let creating = w.submitted && env.create.is_pending();
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let size = egui::vec2(96.0, 0.0);
        if ui
            .add_enabled(!creating, egui::Button::new("Cancel").min_size(size))
            .clicked()
        {
            action = Some(WizardAction::Cancel);
        }
        let spacing = ui.spacing().item_spacing.x;
        ui.add_space((ui.available_width() - 2.0 * size.x - 2.0 * spacing).max(0.0));
        if ui
            .add_enabled(
                w.step != Step::Name && !creating,
                egui::Button::new("Back").min_size(size),
            )
            .clicked()
        {
            w.step = w.step.previous();
        }
        if w.step == Step::Summary {
            let label = if creating { "Creating…" } else { "Create" };
            let ok = env.active && !creating && w.config().is_some() && bounds.sufficient();
            if ui
                .add_enabled(ok, theme::primary(ui, label).min_size(size))
                .clicked()
                && let Some(c) = w.config()
            {
                w.submitted = true;
                action = Some(WizardAction::Create(Box::new(c)));
            }
        } else if (ui
            .add_enabled(can_next, theme::primary(ui, "Next").min_size(size))
            .clicked()
            || (go_next && can_next))
            && action.is_none()
        {
            w.step = w.step.next();
            if w.step == Step::Install || w.step == Step::Data {
                action = Some(WizardAction::RefreshMedia);
            }
        }
    });
    action
}

fn steps_indicator(ui: &mut egui::Ui, current: Step) {
    let p = theme::Palette::of(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, s) in Step::ALL.iter().enumerate() {
            if i > 0 {
                ui.label(egui::RichText::new("›").small().color(p.text_weak));
            }
            let text = format!("{} {}", i + 1, s.title());
            let rich = match s.cmp(&current) {
                std::cmp::Ordering::Equal => {
                    egui::RichText::new(text).small().strong().color(p.accent)
                }
                std::cmp::Ordering::Less => egui::RichText::new(text).small().color(p.text),
                std::cmp::Ordering::Greater => egui::RichText::new(text).small().color(p.text_weak),
            };
            ui.label(rich);
        }
    });
}

/// Returns `true` when Enter was pressed in the field.
fn name_step(ui: &mut egui::Ui, w: &mut Wizard, validation: &Result<VmName, String>) -> bool {
    let p = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new(
            "A label for the VM in LightVM: 1–64 characters, letters, digits, space, '-', '_' and '.'.",
        )
        .color(p.text_weak),
    );
    ui.add_space(4.0);
    let label = ui.label(egui::RichText::new("Name").color(p.text));
    let r = ui.add(
        egui::TextEdit::singleline(&mut w.name)
            .id_salt("lightvm_wizard_name")
            .char_limit(64)
            .desired_width(320.0)
            .hint_text("for example ubuntu-dev"),
    );
    let r = r.labelled_by(label.id);
    // Accessible name of the only text field of the UI.
    let (enabled, value) = (r.enabled(), w.name.clone());
    r.widget_info(|| {
        let mut i = egui::WidgetInfo::text_edit(enabled, value.clone(), value.clone(), "");
        i.label = Some(NAME_FIELD.into());
        i
    });
    if w.focus_name {
        r.request_focus();
        w.focus_name = false;
    }
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    match validation {
        Ok(_) => {
            theme::status(ui, "Valid name", Tone::Positive);
        }
        Err(e) if w.name.is_empty() => {
            ui.label(egui::RichText::new(e).small().color(p.text_weak));
        }
        Err(e) => {
            theme::status(ui, e, Tone::Danger);
        }
    }
    enter
}

fn system_step(ui: &mut egui::Ui, w: &mut Wizard) {
    let p = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new("Operating system that will be installed in the VM.")
            .color(p.text_weak),
    );
    ui.add_space(4.0);
    ui.radio_value(
        &mut w.os,
        GuestOs::UbuntuLtsX86_64,
        "Ubuntu LTS (x86-64), the reference system",
    );
    ui.radio_value(
        &mut w.os,
        GuestOs::OtherLinuxX86_64,
        "Other Linux (x86-64), without compatibility guarantees",
    );
}

fn media_step(
    ui: &mut egui::Ui,
    w: &mut Wizard,
    env: &WizardEnv<'_>,
    kind: MediaKind,
    action: &mut Option<WizardAction>,
) {
    let p = theme::Palette::of(ui);
    let install = kind == MediaKind::InstallIso;
    ui.label(
        egui::RichText::new(if install {
            "ISO image the VM boots from to install the system. LightVM keeps a verified read-only copy in its catalog."
        } else {
            "Optional read-only ISO to bring files into the guest, for example the software to test."
        })
        .color(p.text_weak),
    );
    ui.add_space(6.0);

    // Outcome of an import started from this step: select the new media.
    if let Some((k, _)) = &w.importing
        && *k == kind
    {
        match env.import {
            RequestStatus::Done(m) if m.kind == kind => {
                if install {
                    w.install_media = Some(m.id);
                } else {
                    w.data_media = Some(m.id);
                }
                w.importing = None;
            }
            RequestStatus::Failed(_) | RequestStatus::Done(_) => w.importing = None,
            _ => {}
        }
    }

    let selected = if install {
        &mut w.install_media
    } else {
        &mut w.data_media
    };
    if !install {
        ui.radio_value(selected, None, "No data media");
    }
    let list = env.media.of_kind(kind);
    for m in &list {
        let (title, detail) = media_label(m);
        ui.horizontal(|ui| {
            let r = ui.radio(*selected == Some(m.id), &title);
            if r.clicked() {
                *selected = Some(m.id);
            }
            ui.label(egui::RichText::new(detail).small().color(p.text_weak));
        });
    }
    if let Some(e) = &env.media.error {
        let text = if e.unsupported {
            "This supervisor does not provide the media registry yet.".to_owned()
        } else {
            format!("Approved media not available: {e}")
        };
        theme::notice(ui, &text, Tone::Warning);
    } else if list.is_empty() && env.media.loaded && install {
        ui.label(
            egui::RichText::new("No approved installation ISO yet: import one.").color(p.text_weak),
        );
    } else if !env.media.loaded && env.media.error.is_none() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new("Reading the approved media…").color(p.text_weak));
        });
    }

    ui.add_space(8.0);
    let importing = env.import.is_pending();
    ui.horizontal(|ui| {
        let text = if install {
            "Import ISO…"
        } else {
            "Import data ISO…"
        };
        let r = ui
            .add_enabled(
                env.active && env.file_picker && !importing,
                egui::Button::new(text),
            )
            .on_hover_text("Choose an ISO file: LightVM copies it into the catalog and verifies it")
            .on_disabled_hover_text(if env.file_picker {
                "An import is in progress or the supervisor is not connected"
            } else {
                "File picker not available on this platform"
            });
        if r.clicked() {
            *action = Some(WizardAction::Import(kind));
        }
        if importing {
            ui.spinner();
            let file = w
                .importing
                .as_ref()
                .map_or_else(|| "ISO".to_owned(), |(_, f)| f.clone());
            ui.label(egui::RichText::new(format!("Importing {file}…")).color(p.text));
        }
    });
    if importing {
        ui.label(
            egui::RichText::new(
                "The file is copied into the catalog and its SHA-256 is computed: this can take a few minutes.",
            )
            .small()
            .color(p.text_weak),
        );
    }
    if let RequestStatus::Failed(f) = env.import {
        let text = if f.unsupported {
            "This supervisor cannot import ISO files yet.".to_owned()
        } else {
            format!("Import failed: {f}")
        };
        theme::notice(ui, &text, Tone::Danger);
    }
}

fn slider_row<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    name: &str,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    adjust: impl FnOnce(egui::Slider<'_>) -> egui::Slider<'_>,
    text: impl Fn(T) -> String,
) {
    theme::row(ui, name, None, |ui| {
        ui.horizontal(|ui| {
            let r = ui.add(adjust(egui::Slider::new(value, range)).show_value(false));
            let (enabled, v) = (r.enabled(), value.to_f64());
            r.widget_info(|| egui::WidgetInfo::slider(enabled, v, name));
            ui.label(egui::RichText::new(text(*value)).color(theme::Palette::of(ui).text));
        });
    });
}

fn resources_step(ui: &mut egui::Ui, w: &mut Wizard, b: Bounds) {
    let p = theme::Palette::of(ui);
    let note = if b.from_host {
        format!(
            "Bounded by this host: up to {} vCPU, {} of memory and {} GiB of disk per VM.",
            b.max_vcpus,
            format_mib(b.max_memory_mib),
            b.max_disk_gib
        )
    } else {
        "The supervisor did not report the host capacity: it checks the limits when the VM is created.".into()
    };
    ui.label(egui::RichText::new(note).color(p.text_weak));
    if w.os == GuestOs::UbuntuLtsX86_64 {
        ui.label(
            egui::RichText::new(
                "Ubuntu recommends at least 2 vCPU, 4 GiB of memory and 25 GiB of disk.",
            )
            .small()
            .color(p.text_weak),
        );
    }
    ui.add_space(6.0);
    if !b.sufficient() {
        theme::notice(
            ui,
            "Not enough free memory or disk space on this host for a new VM.",
            Tone::Danger,
        );
        return;
    }
    w.vcpus = w.vcpus.clamp(1, b.max_vcpus);
    w.memory_mib = w.memory_mib.clamp(MIN_MEMORY_MIB, b.max_memory_mib);
    w.disk_gib = w.disk_gib.clamp(MIN_DISK_GIB, b.max_disk_gib);
    slider_row(
        ui,
        "Virtual processors",
        &mut w.vcpus,
        1..=b.max_vcpus,
        |s| s,
        |v| format!("{v} vCPU"),
    );
    slider_row(
        ui,
        "Memory",
        &mut w.memory_mib,
        MIN_MEMORY_MIB..=b.max_memory_mib,
        |s| s.step_by(256.0).logarithmic(true),
        format_mib,
    );
    slider_row(
        ui,
        "Disk",
        &mut w.disk_gib,
        MIN_DISK_GIB..=b.max_disk_gib,
        |s| s.logarithmic(true),
        |v| format!("{v} GiB"),
    );
    ui.label(
        egui::RichText::new(
            "The disk grows on demand up to this size; it can be enlarged later, never shrunk.",
        )
        .small()
        .color(p.text_weak),
    );
}

fn network_step(ui: &mut egui::Ui) {
    let p = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new(
            "In this version VMs have no network: nothing leaves the guest and nothing reaches it.",
        )
        .color(p.text_weak),
    );
    ui.add_space(4.0);
    let _ = ui.radio(true, "Offline: no network adapter");
    for label in [
        "Internet through a filtered NAT: not available in this version",
        "Bridged to the host network: not available in this version",
    ] {
        ui.add_enabled(false, egui::RadioButton::new(false, label))
            .on_disabled_hover_text("Planned after the MVP (ROADMAP R1)");
    }
}

fn summary_step(ui: &mut egui::Ui, w: &Wizard, env: &WizardEnv<'_>) {
    let p = theme::Palette::of(ui);
    let media_name = |id: Option<MediaId>, none: &str| {
        id.and_then(|id| env.media.records.iter().find(|m| m.id == id))
            .map_or_else(|| none.to_owned(), |m| media_label(m).0)
    };
    egui::Grid::new("lightvm_wizard_summary")
        .num_columns(2)
        .spacing([28.0, 6.0])
        .show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(egui::RichText::new(k).color(p.text_weak));
                ui.label(egui::RichText::new(v).color(p.text));
                ui.end_row();
            };
            row("Name", w.name.clone());
            row("System", os_label(w.os).into());
            row("Installation media", media_name(w.install_media, "none"));
            row("Data media", media_name(w.data_media, "none"));
            row("Virtual processors", format!("{} vCPU", w.vcpus));
            row("Memory", format_mib(w.memory_mib));
            row("Disk", format!("{} GiB", w.disk_gib));
            row("Network", "Offline".into());
            row("Firmware", "UEFI".into());
        });
    ui.add_space(6.0);
    if w.submitted {
        match env.create {
            RequestStatus::Pending => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Creating the VM and its disk…");
                });
            }
            RequestStatus::Failed(f) => {
                theme::notice(ui, &format!("Creation failed: {f}"), Tone::Danger);
            }
            _ => {}
        }
    }
    if !env.active {
        theme::notice(ui, "The supervisor is not connected.", Tone::Warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_is_live_and_friendly() {
        let existing = vec!["ubuntu-dev".to_owned()];
        assert!(validate_name("ubuntu-test", &existing).is_ok());
        assert_eq!(
            validate_name("Ubuntu-Dev", &existing).unwrap_err(),
            "A VM with this name already exists."
        );
        assert!(validate_name("", &existing).is_err());
        assert!(
            validate_name("../x", &existing)
                .unwrap_err()
                .starts_with("Allowed characters")
        );
        assert!(validate_name(" lead", &existing).is_err());
        assert!(validate_name(&"x".repeat(65), &existing).is_err());
    }

    #[test]
    fn bounds_follow_the_host() {
        let h = HostCapacity {
            logical_cpus: 4,
            total_memory_mib: 8192,
            reserved_memory_mib: 2048,
            free_storage_gib: 100,
            reserved_storage_gib: 20,
            accelerator_available: true,
            screenshot_available: true,
            input_available: true,
            baseline_available: false,
        };
        let b = Bounds::new(Some(h));
        assert_eq!(
            (b.max_vcpus, b.max_memory_mib, b.max_disk_gib),
            (4, 6144, 80)
        );
        assert!(b.sufficient());
        let w = Wizard::new(Some(h));
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 32));
        let tiny = HostCapacity {
            free_storage_gib: 10,
            ..h
        };
        assert!(!Bounds::new(Some(tiny)).sufficient());
    }
}
