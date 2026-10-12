//! "New VM" wizard (ROADMAP F2, ADR-0027): name, installation media, system,
//! optional data media, resources, network, summary.
//!
//! The system step is prefilled from what the supervisor detected in the
//! chosen installation ISO ([`MediaRecord::detected_guest`]); the user
//! confirms or corrects it. When the media is not recognized no system is
//! preselected: the user must choose one before going on. The guest system
//! decides the firmware (a choice only for Windows 8.1 and 10) and the
//! hardware profile, and suggests the resources. Only the systems this host
//! runs are offered (x86 guests on x86 hosts, Windows 11 ARM64 on Apple
//! Silicon, ADR-0028); a detected system the host cannot run is shown but
//! not preselected.
//!
//! The VM name is the **only** text field of the whole interface: it is a
//! label validated live with [`VmName::parse`] and never reaches QEMU (which
//! uses `-name guest=lightvm-<run>`). Media are chosen among the approved
//! ones of the registry, or imported with the native file picker; resources
//! with sliders bounded by the host capacity; the network is offline only.
//! The view executes nothing: it returns a [`WizardAction`].

use lightvm_proto::{
    DetectedGuest, Firmware, GuestArch, GuestOs, HostCapacity, MediaId, MediaKind, MediaRecord,
    NetworkProfile, ResourceLimits, ResourceTemplate, VmConfig, VmId, VmLimits, VmName,
};

use crate::dashboard::{format_bytes, format_mib, os_label};
use crate::service::{MediaState, RequestStatus};
use crate::theme::{self, Tone};

/// Accessible name of the name field.
pub const NAME_FIELD: &str = "VM name";

/// Smallest disk of any guest system, in GiB.
pub const MIN_DISK_GIB: u64 = 8;
/// Smallest memory of any guest system, in MiB.
pub const MIN_MEMORY_MIB: u64 = 256;

/// Which guest systems the host can run (ADR-0007, ADR-0028): the wizard
/// offers only these. The app uses [`GuestOs::runs_on_this_host`]; tests
/// replace it to exercise the other host family.
pub type GuestSupport = fn(GuestOs) -> bool;

/// System whose suggested resources fill the sliders before any system is
/// chosen: the first system the host runs (Ubuntu LTS on x86 hosts,
/// Windows 11 ARM64 on Apple Silicon). It is never preselected: until the
/// user (or the detection) picks a system, [`Wizard::os`] stays `None`.
fn initial_resources_os(supported: GuestSupport) -> GuestOs {
    GuestOs::ALL
        .into_iter()
        .find(|os| supported(*os))
        .unwrap_or(GuestOs::UbuntuLtsX86_64)
}

/// [`GuestSupport`] compared by what it accepts, not by address.
#[derive(Clone, Copy, Debug)]
struct Supported(GuestSupport);

impl PartialEq for Supported {
    fn eq(&self, other: &Self) -> bool {
        GuestOs::ALL.into_iter().all(|os| self.0(os) == other.0(os))
    }
}

impl Eq for Supported {}

/// Suffix of a detected system the host cannot run.
pub const NOT_SUPPORTED_ON_HOST: &str = " — not supported on this host";

/// Hint on the System step while no system is chosen.
pub const CHOOSE_SYSTEM_HINT: &str = "Choose the system to continue";

/// Steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// VM name.
    Name,
    /// Installation media.
    Install,
    /// Guest system and firmware, prefilled from the installation media.
    System,
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
        Self::Install,
        Self::System,
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

    /// The host can hold a VM at all (memory and disk above the smallest
    /// minimum of any guest system).
    pub fn sufficient(&self) -> bool {
        self.max_memory_mib >= MIN_MEMORY_MIB && self.max_disk_gib >= MIN_DISK_GIB
    }

    /// The host can hold a VM of this guest system (memory and disk above
    /// its minimums).
    pub fn sufficient_for(&self, os: GuestOs) -> bool {
        self.max_memory_mib >= os.min_memory_mib() && self.max_disk_gib >= os.min_disk_gib()
    }
}

/// Firmware as shown to the user.
pub fn firmware_label(f: Firmware) -> &'static str {
    match f {
        Firmware::Uefi => "UEFI",
        Firmware::Bios => "Legacy BIOS (SeaBIOS)",
    }
}

/// Firmware of `os` as shown to the user: guests with a TPM boot with
/// Secure Boot on (ADR-0028).
pub fn firmware_text(os: GuestOs, f: Firmware) -> &'static str {
    if os.needs_tpm() && f == Firmware::Uefi {
        "UEFI with Secure Boot"
    } else {
        firmware_label(f)
    }
}

/// One-line summary of the virtual hardware of the profile of `os`
/// (ADR-0027 and ADR-0028 tables): the firmware does not change the devices.
pub fn hardware_summary(os: GuestOs) -> &'static str {
    match os {
        GuestOs::UbuntuLtsX86_64 | GuestOs::OtherLinuxX86_64 => {
            "virtio disk, virtio display and input"
        }
        GuestOs::WindowsXp => "IDE disk, standard VGA, USB tablet",
        GuestOs::WindowsVista | GuestOs::Windows7 | GuestOs::Windows81 | GuestOs::Windows10 => {
            "AHCI disk, standard VGA, USB tablet"
        }
        GuestOs::Windows11Arm64 => {
            "NVMe disk, ramfb display, USB keyboard and tablet, TPM 2.0, UEFI with Secure Boot"
        }
    }
}

/// Description of a detected guest: "Windows 7, 64-bit, build 7601 — Ultimate".
pub fn detected_label(d: &DetectedGuest) -> String {
    let mut s = os_label(d.os).to_owned();
    match d.arch {
        Some(GuestArch::X86) => s.push_str(", 32-bit"),
        Some(GuestArch::X86_64) => s.push_str(", 64-bit"),
        Some(GuestArch::Aarch64) => s.push_str(", ARM64"),
        None => {}
    }
    if let Some(b) = d.build {
        s.push_str(&format!(", build {b}"));
    }
    if let Some(e) = &d.edition {
        s.push_str(" — ");
        s.push_str(e.as_str());
    }
    s
}

/// Wizard state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wizard {
    /// Current step.
    pub step: Step,
    /// Name typed so far (validated on every change).
    pub name: String,
    /// Declared guest system: detected from the installation media or
    /// chosen by the user; `None` until then (nothing is preselected).
    pub os: Option<GuestOs>,
    /// Firmware; meaningful only once `os` is set, then always one of its
    /// `allowed_firmware()`.
    pub firmware: Firmware,
    /// The user picked the system explicitly: changing the installation
    /// media no longer applies the detection.
    pub os_chosen: bool,
    /// The user moved a resource slider: changing the system no longer
    /// applies its suggested resources.
    pub resources_edited: bool,
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
    /// What was detected in the chosen installation media, if anything.
    pub detected: Option<DetectedGuest>,
    /// Installation media whose detection was applied (`None` before any).
    detected_for: Option<MediaId>,
    focus_name: bool,
    /// Systems the host can run: the only ones offered and preselected.
    supported: Supported,
}

impl Wizard {
    /// New wizard with defaults fitting the host; it offers the systems
    /// this host runs ([`GuestOs::runs_on_this_host`]).
    pub fn new(host: Option<HostCapacity>) -> Self {
        Self::with_guest_support(host, GuestOs::runs_on_this_host)
    }

    /// New wizard offering the systems `supported` accepts.
    pub fn with_guest_support(host: Option<HostCapacity>, supported: GuestSupport) -> Self {
        let b = Bounds::new(host);
        let base = initial_resources_os(supported);
        let t = base.template(None);
        let (vcpus, memory_mib, disk_gib) = (t.vcpus, t.memory_mib, t.disk_gib);
        Self {
            step: Step::Name,
            name: String::new(),
            os: None,
            firmware: base.recommended_firmware(None),
            os_chosen: false,
            resources_edited: false,
            install_media: None,
            data_media: None,
            vcpus: vcpus.min(b.max_vcpus),
            memory_mib: memory_mib.min(b.max_memory_mib).max(base.min_memory_mib()),
            disk_gib: disk_gib.min(b.max_disk_gib).max(base.min_disk_gib()),
            importing: None,
            submitted: false,
            detected: None,
            detected_for: None,
            focus_name: true,
            supported: Supported(supported),
        }
    }

    /// The host can run `os`: it is offered in the System step.
    pub fn supports(&self, os: GuestOs) -> bool {
        self.supported.0(os)
    }

    /// Sets the guest system: the firmware becomes the recommended one for
    /// the detected architecture, and the resources its standard template
    /// unless the user already edited them (always at least the minimums;
    /// the resources step then bounds them by the host).
    pub fn set_os(&mut self, os: GuestOs) {
        let arch = self.arch();
        self.firmware = os.recommended_firmware(arch);
        self.os = Some(os);
        if !self.resources_edited {
            self.apply_template();
        }
        self.memory_mib = self.memory_mib.max(os.min_memory_mib());
        if let Some(cap) = os.max_memory_mib(arch) {
            self.memory_mib = self.memory_mib.min(cap);
        }
        self.disk_gib = self.disk_gib.max(os.min_disk_gib());
    }

    /// Architecture of the chosen installation media, if detected.
    pub fn arch(&self) -> Option<GuestArch> {
        self.detected.as_ref().and_then(|d| d.arch)
    }

    /// Standard template of the chosen system for the media architecture.
    pub fn template(&self) -> Option<ResourceTemplate> {
        self.os.map(|os| os.template(self.arch()))
    }

    /// Puts the resources back to the template of the chosen system (the
    /// resources step then bounds them by the host) and forgets the user's
    /// edits: changing the system applies its template again.
    pub fn apply_template(&mut self) {
        if let Some(t) = self.template() {
            self.vcpus = t.vcpus;
            self.memory_mib = t.memory_mib;
            self.disk_gib = t.disk_gib;
        }
        self.resources_edited = false;
    }

    /// Largest memory the slider offers: the host bound, and for 32-bit
    /// Windows at most what the guest can address.
    pub fn max_memory(&self, b: Bounds) -> u64 {
        self.os
            .and_then(|os| os.max_memory_mib(self.arch()))
            .map_or(b.max_memory_mib, |cap| cap.min(b.max_memory_mib))
    }

    /// Template of the chosen system bounded by the host, as
    /// `(vcpus, memory_mib, disk_gib)`. Only meaningful when the host is
    /// sufficient for the system ([`Bounds::sufficient_for`]).
    pub fn template_within(&self, b: Bounds) -> Option<(u32, u64, u64)> {
        let os = self.os?;
        let t = os.template(self.arch());
        let max_mem = self.max_memory(b).max(os.min_memory_mib());
        let max_disk = b.max_disk_gib.max(os.min_disk_gib());
        Some((
            t.vcpus.clamp(1, b.max_vcpus.max(1)),
            t.memory_mib.clamp(os.min_memory_mib(), max_mem),
            t.disk_gib.clamp(os.min_disk_gib(), max_disk),
        ))
    }

    /// Applies the detection of the chosen installation media once per
    /// media: the system detected, or no system when not recognized or not
    /// runnable on this host (the user must choose one), unless the user
    /// already picked a system.
    /// `media` are the records to look the media up in; nothing happens
    /// while it is not among them.
    pub fn apply_detection<'a>(&mut self, media: impl IntoIterator<Item = &'a MediaRecord>) {
        if self.install_media == self.detected_for {
            return;
        }
        let Some(id) = self.install_media else {
            self.detected_for = None;
            self.detected = None;
            return;
        };
        let Some(record) = media.into_iter().find(|m| m.id == id) else {
            return;
        };
        self.detected_for = Some(id);
        self.detected = record.detected_guest.clone();
        if !self.os_chosen {
            match self.detected.as_ref().map(|d| d.os) {
                Some(os) if self.supports(os) => self.set_os(os),
                _ => self.os = None,
            }
        } else if let Some(os) = self.os {
            // The user's system stays; the firmware, the template and the
            // memory cap follow the architecture of the new media.
            self.set_os(os);
        }
    }

    /// Configuration to create, if the name is valid and a system is
    /// chosen.
    pub fn config(&self) -> Option<VmConfig> {
        let os = self.os?;
        Some(VmConfig {
            name: VmName::parse(self.name.clone()).ok()?,
            guest_os: os,
            vcpus: self.vcpus,
            memory_mib: self.memory_mib,
            disk_gib: self.disk_gib,
            network: NetworkProfile::Offline,
            install_media: self.install_media,
            data_media: self.data_media,
            firmware: if os.allowed_firmware().contains(&self.firmware) {
                self.firmware
            } else {
                os.recommended_firmware(None)
            },
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
    // The media just imported may not be in the list yet.
    let imported = match env.import {
        RequestStatus::Done(m) => Some(m),
        _ => None,
    };
    w.apply_detection(env.media.records.iter().chain(imported));
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
            Step::System => system_step(ui, w, bounds),
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
        Step::System => w.os.is_some(),
        Step::Resources => w.os.is_some_and(|os| bounds.sufficient_for(os)),
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
            let ok = env.active
                && !creating
                && w.config().is_some()
                && w.os.is_some_and(|os| bounds.sufficient_for(os));
            if ui
                .add_enabled(ok, theme::primary(ui, label).min_size(size))
                .clicked()
                && let Some(c) = w.config()
            {
                w.submitted = true;
                action = Some(WizardAction::Create(Box::new(c)));
            }
        } else if ({
            let next = ui.add_enabled(can_next, theme::primary(ui, "Next").min_size(size));
            if w.step == Step::System && w.os.is_none() {
                next.on_disabled_hover_text(CHOOSE_SYSTEM_HINT)
            } else {
                next
            }
        }
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

fn system_step(ui: &mut egui::Ui, w: &mut Wizard, b: Bounds) {
    let p = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new(
            "Operating system that will be installed in the VM: it decides the virtual hardware.",
        )
        .color(p.text_weak),
    );
    ui.add_space(4.0);
    match &w.detected {
        Some(d) if w.supports(d.os) => {
            theme::status(
                ui,
                &format!("Detected: {}", detected_label(d)),
                Tone::Positive,
            );
        }
        Some(d) => {
            theme::status(
                ui,
                &format!("Detected: {}{NOT_SUPPORTED_ON_HOST}", detected_label(d)),
                Tone::Warning,
            );
        }
        None => {
            theme::status(ui, "Not recognized: choose the system", Tone::Warning);
        }
    }
    ui.add_space(6.0);
    // Two columns, Linux then Windows, with only the systems this host
    // runs (an empty column is not shown): the reading (and Tab) order
    // stays the order of `GuestOs::ALL`.
    let mut picked = None;
    let supported = w.supported.0;
    let columns: Vec<(&str, Vec<GuestOs>)> = [("Linux", false), ("Windows", true)]
        .into_iter()
        .map(|(caption, windows)| {
            let list = GuestOs::ALL
                .into_iter()
                .filter(|o| o.is_windows() == windows && supported(*o))
                .collect::<Vec<_>>();
            (caption, list)
        })
        .filter(|(_, list)| !list.is_empty())
        .collect();
    let arm_host = columns
        .iter()
        .all(|(_, l)| l.iter().all(|o| o.is_aarch64()));
    ui.horizontal_top(|ui| {
        for (caption, list) in &columns {
            ui.allocate_ui_with_layout(
                egui::vec2(380.0, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(380.0);
                    group_caption(ui, caption);
                    for &os in list {
                        if ui.radio(w.os == Some(os), os_choice_label(os)).clicked() {
                            picked = Some(os);
                        }
                    }
                },
            );
        }
    });
    ui.add_space(4.0);
    if let Some(os) = picked {
        w.os_chosen = true;
        if Some(os) != w.os {
            w.set_os(os);
        }
    }
    ui.label(
        egui::RichText::new(if arm_host {
            ARM_HOST_NOTE
        } else {
            X86_HOST_NOTE
        })
        .small()
        .color(p.text_weak),
    );
    ui.add_space(8.0);
    let Some(os) = w.os else {
        // Nothing preselected: firmware and hardware follow the choice.
        for k in ["Firmware", "Virtual hardware"] {
            theme::row(ui, k, None, |ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("—").color(p.text_weak));
            });
        }
        ui.add_space(4.0);
        theme::status(ui, CHOOSE_SYSTEM_HINT, Tone::Warning);
        return;
    };
    let allowed = os.allowed_firmware();
    if allowed.len() > 1 {
        theme::row(
            ui,
            "Firmware",
            Some("Legacy BIOS for 32-bit media or installations that need it."),
            |ui| {
                for f in allowed {
                    ui.radio_value(&mut w.firmware, *f, firmware_label(*f));
                }
            },
        );
    } else {
        let fixed = allowed.first().copied().unwrap_or_default();
        theme::row(ui, "Firmware", Some("Fixed by the system."), |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(firmware_text(os, fixed)).color(p.text));
        });
    }
    theme::row(ui, "Virtual hardware", None, |ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(hardware_summary(os)).color(p.text));
    });
    if !b.sufficient_for(os) {
        ui.add_space(4.0);
        theme::notice(ui, &insufficient_text(os), Tone::Danger);
    }
}

/// Note under the systems on x86 hosts.
pub const X86_HOST_NOTE: &str = "Windows guests run offline, without a network adapter. Windows 8 and Windows 11 are not supported.";
/// Note under the systems on Apple Silicon hosts (ADR-0028).
pub const ARM_HOST_NOTE: &str = "Windows guests run offline, without a network adapter. On this Mac only Windows 11 ARM64 runs: bring your own installation ISO and licence.";

fn os_choice_label(os: GuestOs) -> &'static str {
    match os {
        GuestOs::UbuntuLtsX86_64 => "Ubuntu LTS (x86-64), the reference system",
        GuestOs::OtherLinuxX86_64 => "Other Linux (x86-64), without compatibility guarantees",
        GuestOs::WindowsXp => "Windows XP",
        GuestOs::WindowsVista => "Windows Vista",
        GuestOs::Windows7 => "Windows 7",
        GuestOs::Windows81 => "Windows 8.1",
        GuestOs::Windows10 => "Windows 10",
        GuestOs::Windows11Arm64 => "Windows 11 (ARM64)",
    }
}

fn group_caption(ui: &mut egui::Ui, text: &str) {
    let p = theme::Palette::of(ui);
    ui.label(
        egui::RichText::new(text)
            .small()
            .strong()
            .color(p.text_weak),
    );
}

/// Note under the template when the memory slider is capped by the guest.
pub const MEMORY_CAP_NOTE: &str = "32-bit Windows uses at most 4 GiB of memory.";

/// Name of the standard template of `os` for the media architecture:
/// "Windows 7 64-bit"; just "Windows 7" when the architecture is unknown.
/// Linux guests are x86-64 only and carry no suffix; Windows 11 is ARM64
/// only: "Windows 11 ARM64".
pub fn template_name(os: GuestOs, arch: Option<GuestArch>) -> String {
    if os.is_aarch64() {
        return format!("{} ARM64", os_label(os));
    }
    match (os.is_windows(), arch) {
        (true, Some(GuestArch::X86)) => format!("{} 32-bit", os_label(os)),
        (true, Some(GuestArch::X86_64)) => format!("{} 64-bit", os_label(os)),
        _ => os_label(os).to_owned(),
    }
}

/// "Template: Windows 7 64-bit — 2 vCPU, 4 GiB RAM, 40 GiB disk."
pub fn template_label(os: GuestOs, arch: Option<GuestArch>) -> String {
    let t = os.template(arch);
    format!(
        "Template: {} — {} vCPU, {} RAM, {} GiB disk.",
        template_name(os, arch),
        t.vcpus,
        format_mib(t.memory_mib),
        t.disk_gib
    )
}

fn insufficient_text(os: GuestOs) -> String {
    format!(
        "Not enough free memory or disk space on this host for a {} VM: it needs at least {} of memory and {} GiB of disk.",
        os_label(os),
        format_mib(os.min_memory_mib()),
        os.min_disk_gib()
    )
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

    let supported = w.supported.0;
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
        let (title, mut detail) = media_label(m);
        if install && let Some(d) = &m.detected_guest {
            detail.push_str(" · ");
            detail.push_str(&detected_label(d));
            if !supported(d.os) {
                detail.push_str(NOT_SUPPORTED_ON_HOST);
            }
        }
        ui.horizontal(|ui| {
            let r = ui.radio(*selected == Some(m.id), &title);
            if r.clicked() {
                *selected = Some(m.id);
            }
            ui.add(egui::Label::new(egui::RichText::new(detail).small().color(p.text_weak)).wrap());
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
) -> bool {
    theme::row(ui, name, None, |ui| {
        ui.horizontal(|ui| {
            let r = ui.add(adjust(egui::Slider::new(value, range)).show_value(false));
            let (enabled, v) = (r.enabled(), value.to_f64());
            r.widget_info(|| egui::WidgetInfo::slider(enabled, v, name));
            ui.label(egui::RichText::new(text(*value)).color(theme::Palette::of(ui).text));
            r.changed()
        })
        .inner
    })
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
    let Some(os) = w.os else {
        // Not reachable through Next (the System step requires a choice).
        theme::notice(ui, CHOOSE_SYSTEM_HINT, Tone::Warning);
        return;
    };
    let arch = w.arch();
    let (min_mem, min_disk) = (os.min_memory_mib(), os.min_disk_gib());
    ui.label(
        egui::RichText::new(format!(
            "{} Minimum: {} of memory and {min_disk} GiB of disk.",
            template_label(os, arch),
            format_mib(min_mem),
        ))
        .small()
        .color(p.text_weak),
    );
    if os.max_memory_mib(arch).is_some() {
        ui.label(
            egui::RichText::new(MEMORY_CAP_NOTE)
                .small()
                .color(p.text_weak),
        );
    }
    ui.add_space(6.0);
    // The minimums of the system may exceed what the host offers.
    if !b.sufficient_for(os) {
        theme::notice(ui, &insufficient_text(os), Tone::Danger);
        return;
    }
    let max_mem = w.max_memory(b).max(min_mem);
    w.vcpus = w.vcpus.clamp(1, b.max_vcpus);
    w.memory_mib = w.memory_mib.clamp(min_mem, max_mem);
    w.disk_gib = w.disk_gib.clamp(min_disk, b.max_disk_gib);
    let mut edited = slider_row(
        ui,
        "Virtual processors",
        &mut w.vcpus,
        1..=b.max_vcpus,
        |s| s,
        |v| format!("{v} vCPU"),
    );
    edited |= slider_row(
        ui,
        "Memory",
        &mut w.memory_mib,
        min_mem..=max_mem,
        |s| s.step_by(256.0).logarithmic(true),
        format_mib,
    );
    edited |= slider_row(
        ui,
        "Disk",
        &mut w.disk_gib,
        min_disk..=b.max_disk_gib,
        |s| s.logarithmic(true),
        |v| format!("{v} GiB"),
    );
    if edited {
        w.resources_edited = true;
    }
    let at_template = w.template_within(b) == Some((w.vcpus, w.memory_mib, w.disk_gib));
    let r = ui
        .add_enabled(!at_template, egui::Button::new("Reset to template"))
        .on_hover_text("Back to the standard template of the system, within this host")
        .on_disabled_hover_text("The resources already match the template");
    if r.clicked() {
        w.apply_template();
        if let Some((v, m, d)) = w.template_within(b) {
            (w.vcpus, w.memory_mib, w.disk_gib) = (v, m, d);
        }
    }
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
            let dash = || "—".to_owned();
            row("System", w.os.map_or_else(dash, |os| os_label(os).into()));
            row(
                "Firmware",
                w.os.map_or_else(dash, |os| firmware_text(os, w.firmware).into()),
            );
            row(
                "Virtual hardware",
                w.os.map_or_else(dash, |os| hardware_summary(os).into()),
            );
            row("Installation media", media_name(w.install_media, "none"));
            row("Data media", media_name(w.data_media, "none"));
            row("Virtual processors", format!("{} vCPU", w.vcpus));
            row("Memory", format_mib(w.memory_mib));
            row("Disk", format!("{} GiB", w.disk_gib));
            row("Network", "Offline".into());
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

    /// An x86 host (every build but Apple Silicon macOS).
    fn x86_host(os: GuestOs) -> bool {
        !os.is_aarch64()
    }

    /// An Apple Silicon host (ADR-0028).
    fn arm_host(os: GuestOs) -> bool {
        os.is_aarch64()
    }

    /// Wizard of an x86 host, whatever the build host.
    fn x86_wizard(host: Option<HostCapacity>) -> Wizard {
        Wizard::with_guest_support(host, x86_host)
    }

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
        let w = x86_wizard(Some(h));
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 32));
        let tiny = HostCapacity {
            free_storage_gib: 10,
            ..h
        };
        assert!(!Bounds::new(Some(tiny)).sufficient());
    }

    fn record(n: u8, detected: Option<DetectedGuest>) -> MediaRecord {
        MediaRecord {
            id: MediaId::from_unix_millis(1_760_000_200_000 + u64::from(n), [n; 10]),
            kind: MediaKind::InstallIso,
            sha256: lightvm_proto::Sha256Digest::from_bytes([n; 32]),
            size_bytes: 1 << 30,
            declared_origin: lightvm_proto::Label::parse("x.iso").unwrap(),
            approved_at: lightvm_proto::UtcTimestamp::from_unix_millis(1_760_000_000_000).unwrap(),
            detected_guest: detected,
        }
    }

    fn detected(os: GuestOs, arch: Option<GuestArch>) -> Option<DetectedGuest> {
        Some(DetectedGuest {
            os,
            arch,
            build: None,
            edition: None,
        })
    }

    fn with_media(m: &MediaRecord) -> Wizard {
        let mut w = x86_wizard(None);
        w.name = "lab".into();
        w.install_media = Some(m.id);
        w.apply_detection([m]);
        w
    }

    #[test]
    fn detection_prefills_system_firmware_and_resources() {
        let m = record(
            1,
            Some(DetectedGuest {
                os: GuestOs::Windows7,
                arch: Some(GuestArch::X86_64),
                build: Some(7601),
                edition: Some(lightvm_proto::Label::parse("Windows 7 ULTIMATE").unwrap()),
            }),
        );
        let w = with_media(&m);
        assert_eq!(
            (w.os, w.firmware),
            (Some(GuestOs::Windows7), Firmware::Bios)
        );
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 40));
        assert_eq!(
            detected_label(w.detected.as_ref().unwrap()),
            "Windows 7, 64-bit, build 7601 — Windows 7 ULTIMATE"
        );
        let c = w.config().unwrap();
        assert_eq!(
            (c.guest_os, c.firmware),
            (GuestOs::Windows7, Firmware::Bios)
        );
    }

    #[test]
    fn windows_10_firmware_follows_the_architecture() {
        let w = with_media(&record(
            1,
            detected(GuestOs::Windows10, Some(GuestArch::X86)),
        ));
        assert_eq!(
            (w.os, w.firmware),
            (Some(GuestOs::Windows10), Firmware::Bios)
        );
        let w = with_media(&record(
            2,
            detected(GuestOs::Windows10, Some(GuestArch::X86_64)),
        ));
        assert_eq!(
            (w.os, w.firmware),
            (Some(GuestOs::Windows10), Firmware::Uefi)
        );
        assert_eq!(w.config().unwrap().firmware, Firmware::Uefi);
        let w = with_media(&record(3, detected(GuestOs::Windows81, None)));
        assert_eq!(
            (w.os, w.firmware),
            (Some(GuestOs::Windows81), Firmware::Uefi)
        );
        let w = with_media(&record(4, detected(GuestOs::WindowsXp, None)));
        assert_eq!(
            (w.os, w.firmware),
            (Some(GuestOs::WindowsXp), Firmware::Bios)
        );
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (1, 1024, 16));
    }

    #[test]
    fn changing_media_reapplies_detection_unless_the_user_chose() {
        let win7 = record(1, detected(GuestOs::Windows7, Some(GuestArch::X86_64)));
        let unknown = record(2, None);
        let xp = record(3, detected(GuestOs::WindowsXp, Some(GuestArch::X86)));
        let all = [win7.clone(), unknown.clone(), xp.clone()];
        let mut w = with_media(&win7);
        assert_eq!(w.os, Some(GuestOs::Windows7));
        // Not recognized: the selection is cleared, nothing preselected,
        // no configuration until the user chooses.
        w.install_media = Some(unknown.id);
        w.apply_detection(&all);
        assert_eq!(w.os, None);
        assert!(w.detected.is_none());
        assert!(w.config().is_none(), "no VM without a system");
        w.install_media = Some(xp.id);
        w.apply_detection(&all);
        assert_eq!(w.os, Some(GuestOs::WindowsXp));
        // Explicit choice: kept when the media changes again.
        w.os_chosen = true;
        w.set_os(GuestOs::Windows10);
        assert_eq!(w.firmware, Firmware::Bios, "32-bit media → BIOS");
        w.install_media = Some(win7.id);
        w.apply_detection(&all);
        assert_eq!(w.os, Some(GuestOs::Windows10));
        assert_eq!(w.detected.as_ref().unwrap().os, GuestOs::Windows7);
        // The firmware and the template follow the new 64-bit media.
        assert_eq!(w.firmware, Firmware::Uefi, "64-bit media → UEFI");
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 64));
        // Media not in the list yet: nothing applied until it appears.
        let mut w = x86_wizard(None);
        w.install_media = Some(xp.id);
        w.apply_detection(&all[..1]);
        assert_eq!(w.os, None);
        w.apply_detection(&all);
        assert_eq!(w.os, Some(GuestOs::WindowsXp));
    }

    #[test]
    fn unrecognized_media_preselects_nothing() {
        let unknown = record(2, None);
        let mut w = with_media(&unknown);
        assert_eq!(w.os, None, "no system preselected");
        assert!(w.config().is_none());
        // The user's choice applies firmware and suggested resources.
        w.os_chosen = true;
        w.set_os(GuestOs::Windows10);
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 64));
        assert_eq!(w.config().unwrap().guest_os, GuestOs::Windows10);
        // An explicit choice survives a change to another unrecognized media.
        let other = record(5, None);
        w.install_media = Some(other.id);
        w.apply_detection([&other]);
        assert_eq!(w.os, Some(GuestOs::Windows10));
    }

    #[test]
    fn system_change_keeps_edited_resources_above_the_minimums() {
        let mut w = x86_wizard(None);
        w.set_os(GuestOs::Windows10);
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 64));
        w.set_os(GuestOs::WindowsXp);
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (1, 1024, 16));
        // Edited by the user: kept, raised to the minimums of the system.
        w.resources_edited = true;
        w.memory_mib = 768;
        w.disk_gib = 12;
        w.vcpus = 3;
        w.set_os(GuestOs::Windows10);
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (3, 2048, 32));
        w.set_os(GuestOs::UbuntuLtsX86_64);
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (3, 2048, 32));
    }

    #[test]
    fn minimums_above_the_host_are_reported() {
        let h = HostCapacity {
            logical_cpus: 2,
            total_memory_mib: 3072,
            reserved_memory_mib: 1536,
            free_storage_gib: 35,
            reserved_storage_gib: 20,
            accelerator_available: true,
            screenshot_available: true,
            input_available: true,
            baseline_available: false,
        };
        let b = Bounds::new(Some(h));
        assert!(b.sufficient_for(GuestOs::WindowsXp));
        assert!(b.sufficient_for(GuestOs::UbuntuLtsX86_64));
        assert!(!b.sufficient_for(GuestOs::Windows10), "{b:?}");
        assert!(!b.sufficient_for(GuestOs::Windows7), "disk {b:?}");
    }

    #[test]
    fn config_per_system() {
        for os in GuestOs::ALL {
            let mut w = x86_wizard(None);
            w.name = "lab".into();
            w.set_os(os);
            let c = w.config().unwrap();
            assert_eq!(c.guest_os, os);
            assert_eq!(c.firmware, os.recommended_firmware(None), "{os:?}");
            assert!(os.allowed_firmware().contains(&c.firmware));
            assert!(c.memory_mib >= os.min_memory_mib() && c.disk_gib >= os.min_disk_gib());
            assert_eq!(c.network, NetworkProfile::Offline);
        }
        let mut w = x86_wizard(None);
        w.name = "lab".into();
        w.set_os(GuestOs::Windows10);
        w.firmware = Firmware::Bios;
        assert_eq!(w.config().unwrap().firmware, Firmware::Bios);
        // A firmware the system does not allow never leaves the wizard.
        w.os = Some(GuestOs::WindowsXp);
        w.firmware = Firmware::Uefi;
        assert_eq!(w.config().unwrap().firmware, Firmware::Bios);
    }

    #[test]
    fn templates_follow_the_media_architecture() {
        let w = with_media(&record(
            1,
            detected(GuestOs::Windows7, Some(GuestArch::X86)),
        ));
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 2048, 32));
        assert_eq!(
            template_label(GuestOs::Windows7, Some(GuestArch::X86)),
            "Template: Windows 7 32-bit — 2 vCPU, 2 GiB RAM, 32 GiB disk."
        );
        assert_eq!(
            template_label(GuestOs::Windows7, Some(GuestArch::X86_64)),
            "Template: Windows 7 64-bit — 2 vCPU, 4 GiB RAM, 40 GiB disk."
        );
        assert_eq!(template_name(GuestOs::Windows7, None), "Windows 7");
        assert_eq!(
            template_label(GuestOs::WindowsXp, None),
            "Template: Windows XP — 1 vCPU, 1 GiB RAM, 16 GiB disk."
        );
        assert_eq!(
            template_label(GuestOs::Windows10, None),
            "Template: Windows 10 — 2 vCPU, 4 GiB RAM, 64 GiB disk."
        );
        assert_eq!(template_name(GuestOs::UbuntuLtsX86_64, None), "Ubuntu LTS");
        // 32-bit Windows: memory capped at 4 GiB whatever the host offers.
        let b = Bounds::new(None);
        assert!(b.max_memory_mib > 4096, "{b:?}");
        assert_eq!(w.max_memory(b), 4096);
        let w64 = with_media(&record(
            2,
            detected(GuestOs::Windows7, Some(GuestArch::X86_64)),
        ));
        assert_eq!(w64.max_memory(b), b.max_memory_mib);
    }

    #[test]
    fn reset_to_template_restores_after_edits() {
        let mut w = with_media(&record(
            1,
            detected(GuestOs::Windows10, Some(GuestArch::X86_64)),
        ));
        let b = Bounds::new(None);
        assert_eq!(w.template_within(b), Some((2, 4096, 64)));
        w.resources_edited = true;
        (w.vcpus, w.memory_mib, w.disk_gib) = (4, 8192, 100);
        w.set_os(GuestOs::Windows81);
        assert_eq!(
            (w.vcpus, w.memory_mib, w.disk_gib),
            (4, 8192, 100),
            "edits kept"
        );
        w.apply_template();
        assert!(!w.resources_edited);
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (2, 4096, 50));
        // A small host bounds the template.
        let small = Bounds::new(Some(HostCapacity {
            logical_cpus: 1,
            total_memory_mib: 6144,
            reserved_memory_mib: 3072,
            free_storage_gib: 60,
            reserved_storage_gib: 20,
            accelerator_available: true,
            screenshot_available: true,
            input_available: true,
            baseline_available: false,
        }));
        let (v, m, d) = w.template_within(small).unwrap();
        assert_eq!(v, 1);
        assert_eq!(m, 4096.min(small.max_memory_mib).max(2048), "{small:?}");
        assert_eq!(d, 50.min(small.max_disk_gib).max(32), "{small:?}");
    }

    #[test]
    fn labels_of_firmware_and_hardware() {
        assert_eq!(firmware_label(Firmware::Uefi), "UEFI");
        assert_eq!(firmware_label(Firmware::Bios), "Legacy BIOS (SeaBIOS)");
        assert_eq!(
            hardware_summary(GuestOs::UbuntuLtsX86_64),
            "virtio disk, virtio display and input"
        );
        assert_eq!(
            hardware_summary(GuestOs::Windows10),
            "AHCI disk, standard VGA, USB tablet"
        );
        assert_eq!(
            hardware_summary(GuestOs::WindowsXp),
            "IDE disk, standard VGA, USB tablet"
        );
    }

    #[test]
    fn windows_11_arm64_labels() {
        let d = DetectedGuest {
            os: GuestOs::Windows11Arm64,
            arch: Some(GuestArch::Aarch64),
            build: Some(26200),
            edition: Some(lightvm_proto::Label::parse("Windows 11 Home").unwrap()),
        };
        assert_eq!(
            detected_label(&d),
            "Windows 11, ARM64, build 26200 — Windows 11 Home"
        );
        assert_eq!(
            template_name(GuestOs::Windows11Arm64, Some(GuestArch::Aarch64)),
            "Windows 11 ARM64"
        );
        assert_eq!(
            template_name(GuestOs::Windows11Arm64, None),
            "Windows 11 ARM64"
        );
        assert_eq!(
            hardware_summary(GuestOs::Windows11Arm64),
            "NVMe disk, ramfb display, USB keyboard and tablet, TPM 2.0, UEFI with Secure Boot"
        );
        assert_eq!(
            firmware_text(GuestOs::Windows11Arm64, Firmware::Uefi),
            "UEFI with Secure Boot"
        );
        assert_eq!(firmware_text(GuestOs::Windows10, Firmware::Uefi), "UEFI");
    }

    #[test]
    fn detection_preselects_only_what_the_host_runs() {
        let w11 = record(
            7,
            Some(DetectedGuest {
                os: GuestOs::Windows11Arm64,
                arch: Some(GuestArch::Aarch64),
                build: Some(26200),
                edition: None,
            }),
        );
        let w10 = record(8, detected(GuestOs::Windows10, Some(GuestArch::X86_64)));
        // Apple Silicon: Windows 11 ARM64 prefilled with its template.
        let mut w = Wizard::with_guest_support(None, arm_host);
        w.name = "w11".into();
        w.install_media = Some(w11.id);
        w.apply_detection([&w11]);
        assert_eq!(
            (w.os, w.firmware),
            (Some(GuestOs::Windows11Arm64), Firmware::Uefi)
        );
        assert_eq!((w.vcpus, w.memory_mib, w.disk_gib), (4, 6144, 64));
        assert_eq!(w.config().unwrap().guest_os, GuestOs::Windows11Arm64);
        // Apple Silicon, x86 media: detected but not preselected.
        w.install_media = Some(w10.id);
        w.apply_detection([&w10]);
        assert_eq!(w.os, None);
        assert_eq!(w.detected.as_ref().unwrap().os, GuestOs::Windows10);
        // x86 host, ARM64 media: the same.
        let mut w = x86_wizard(None);
        w.install_media = Some(w11.id);
        w.apply_detection([&w11]);
        assert_eq!(w.os, None);
        assert!(!w.supports(GuestOs::Windows11Arm64));
        assert!(w.supports(GuestOs::UbuntuLtsX86_64));
    }

    #[test]
    fn initial_resources_follow_the_host() {
        assert_eq!(initial_resources_os(x86_host), GuestOs::UbuntuLtsX86_64);
        assert_eq!(initial_resources_os(arm_host), GuestOs::Windows11Arm64);
        let w = Wizard::with_guest_support(None, arm_host);
        assert_eq!(w.os, None, "never preselected");
        assert!(w.memory_mib >= GuestOs::Windows11Arm64.min_memory_mib());
        assert!(w.disk_gib >= GuestOs::Windows11Arm64.min_disk_gib());
        // The app's default is the build host.
        let w = Wizard::new(None);
        for os in GuestOs::ALL {
            assert_eq!(w.supports(os), os.runs_on_this_host(), "{os:?}");
        }
    }
}
