//! "Import VMware VM" page (ADR-0030): the `.vmx` chosen with the native
//! file picker is examined by the supervisor, the page shows what would be
//! imported, what is not, and the warnings; the user names the new VM,
//! chooses the display adapter when it is offered, and starts the
//! conversion, which can be cancelled.
//!
//! The view executes nothing: it returns an [`ImportAction`]. The texts the
//! preview carries from the VMware files (display name, guest type, file
//! names) are shown as plain text, never interpreted. No path is shown: the
//! supervisor sends file names only.

use std::path::PathBuf;

use lightvm_proto::ipc::{
    ImportWarning, ImportWarningKind, NotImportedItem, VmwareController, VmwareImportPhase,
    VmwareImportPreview,
};
use lightvm_proto::{DiskController, DisplayAdapter, Firmware, GuestOs, VmName, VmRecord};

use crate::dashboard::{format_bytes, format_mib, os_label};
use crate::service::{Failure, RequestStatus, VmwareImportState};
use crate::theme::{self, Tone};
use crate::wizard::{firmware_text, validate_name};

/// Title of the page and label of the Dashboard button.
pub const TITLE: &str = "Import VMware VM";

/// Description under the title.
pub const DESCRIPTION: &str = "A VMware Workstation or Player VM is converted into a new LightVM VM. The VMware files are only read: they are never modified and the VM keeps working in VMware.";

/// Accessible name of the name field.
pub const NAME_FIELD: &str = "Name of the imported VM";

/// Text of the LSI Logic warning (ADR-0030).
pub const STORAGE_DRIVER_WARNING: &str = "Windows may not start: storage driver preparation needed. The disk was on an LSI Logic SCSI controller, which LightVM cannot emulate: Windows stops at boot (STOP 0x7B) until the driver of the new controller is enabled. Prepare the VM in VMware before importing, as described in the user guide, section \u{201c}Importing a VMware VM\u{201d}, \u{201c}LSI Logic disks\u{201d}.";

/// Import page state, from the choice of the `.vmx` to the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportFlow {
    /// `.vmx` chosen with the picker.
    pub path: PathBuf,
    /// File name of the `.vmx`, for the texts.
    pub file_name: String,
    /// Name of the new VM (prefilled from the preview).
    pub name: String,
    /// The name was prefilled from the preview.
    pub name_prefilled: bool,
    /// Give the keyboard focus to the name field at the next frame.
    pub focus_name: bool,
    /// Display adapter.
    pub display: DisplayAdapter,
    /// The import was requested: the page shows progress and outcome.
    pub submitted: bool,
}

impl ImportFlow {
    /// New flow for `path`.
    pub fn new(path: PathBuf) -> Self {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".vmx".into());
        Self {
            path,
            file_name,
            name: String::new(),
            name_prefilled: false,
            focus_name: false,
            display: DisplayAdapter::Standard,
            submitted: false,
        }
    }
}

/// What the page asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportAction {
    /// Close the page (nothing is imported, or the import ended).
    Close,
    /// Choose another `.vmx` with the picker.
    ChooseOther,
    /// Examine the same `.vmx` again.
    Retry,
    /// Start the import.
    Import {
        /// `.vmx`.
        path: PathBuf,
        /// Name of the new VM.
        name: VmName,
        /// Display adapter.
        display: DisplayAdapter,
    },
    /// Cancel the import in progress.
    CancelImport,
}

/// What the page reads from the app.
#[derive(Debug)]
pub struct ImportEnv<'a> {
    /// Preview, import and progress of the supervisor.
    pub state: &'a VmwareImportState,
    /// Names already in the catalog.
    pub existing_names: &'a [String],
    /// The supervisor is connected.
    pub active: bool,
    /// A native file picker is available.
    pub file_picker: bool,
}

/// LightVM name of a disk controller.
pub fn controller_label(c: DiskController) -> &'static str {
    match c {
        DiskController::Virtio => "virtio",
        DiskController::Ahci => "SATA (AHCI)",
        DiskController::Ide => "IDE",
        DiskController::Nvme => "NVMe",
        DiskController::Pvscsi => "VMware Paravirtual SCSI (PVSCSI)",
        DiskController::LsiSas1068 => "LSI Logic SAS 1068",
    }
}

/// Name of a VMware disk controller.
pub fn vmware_controller_label(c: VmwareController) -> &'static str {
    match c {
        VmwareController::Ide => "IDE",
        VmwareController::Sata => "SATA",
        VmwareController::Nvme => "NVMe",
        VmwareController::Pvscsi => "VMware Paravirtual SCSI",
        VmwareController::LsiLogic => "LSI Logic parallel SCSI",
        VmwareController::LsiSas1068 => "LSI Logic SAS",
        VmwareController::BusLogic => "BusLogic SCSI",
    }
}

/// Name of a display adapter.
pub fn display_label(d: DisplayAdapter, os: GuestOs) -> &'static str {
    match d {
        DisplayAdapter::Standard if os.is_windows() => "Standard VGA",
        DisplayAdapter::Standard => "virtio display",
        DisplayAdapter::VmwareSvga => "VMware SVGA II",
    }
}

/// What a part that is not imported means for the user.
pub fn not_imported_text(item: NotImportedItem) -> &'static str {
    match item {
        NotImportedItem::Network => "Network adapters: LightVM VMs are offline, without network",
        NotImportedItem::SharedFolders => {
            "Shared folders: they will come later through LightVM Tools"
        }
        NotImportedItem::Usb => {
            "USB controllers and devices: they will come later through LightVM Tools"
        }
        NotImportedItem::Sound => "Sound card",
        NotImportedItem::Accelerated3d => "3D acceleration",
        NotImportedItem::Snapshots => {
            "Snapshots: only the current state is converted, the snapshots are not kept"
        }
        NotImportedItem::OtherDisk => "Other hard disk: only the main disk is converted",
        NotImportedItem::CdRom => {
            "CD/DVD drive: an ISO can be imported as a data medium from the media library (Settings)"
        }
        NotImportedItem::Floppy => "Floppy drive",
        NotImportedItem::SerialPort => "Serial port",
        NotImportedItem::ParallelPort => "Parallel port",
        NotImportedItem::Printer => "Virtual printer",
        NotImportedItem::VmwareToolsFeatures => {
            "VMware Tools features (clipboard, drag and drop, time sync): they will come later through LightVM Tools"
        }
    }
}

/// Text of a warning.
pub fn warning_text(w: &ImportWarning) -> String {
    let base = match w.kind {
        ImportWarningKind::StorageDriverPreparation => STORAGE_DRIVER_WARNING,
        ImportWarningKind::ControllerReplaced => {
            "The disk controller of the VM cannot be emulated: the disk is attached to the standard controller of the system. Standard Linux distributions start; a custom initramfs may lack the driver."
        }
        ImportWarningKind::MemoryRaised => {
            "The memory was raised to the minimum LightVM accepts for this system."
        }
        ImportWarningKind::MemoryLowered => {
            "The memory was lowered to what this computer can give the VM."
        }
        ImportWarningKind::VcpusLowered => {
            "The virtual processors were lowered to what this computer has."
        }
        ImportWarningKind::TopologyChanged => {
            "The CPU topology of VMware (cores per socket) could not be kept: one socket per virtual processor is used."
        }
        ImportWarningKind::Windows8Or81 => {
            "VMware uses the same guest type for Windows 8 and 8.1: LightVM supports Windows 8.1 only."
        }
        ImportWarningKind::NewUefiVariables => {
            "UEFI VM: the firmware starts with new settings, so the boot entries of VMware are not carried over; the default boot loader of the disk is used."
        }
        ImportWarningKind::DirtyDisk => {
            "A disk file was not closed cleanly by VMware: the guest may check its file system at the first start."
        }
    };
    match &w.detail {
        Some(d) if !d.is_empty() => format!("{base} ({d})"),
        _ => base.to_owned(),
    }
}

/// Text of a phase of the import.
pub fn phase_text(phase: VmwareImportPhase) -> &'static str {
    match phase {
        VmwareImportPhase::Checking => "Checking the VMware files…",
        VmwareImportPhase::Converting => "Converting the disk…",
        VmwareImportPhase::Finishing => "Checking the converted disk and creating the VM…",
    }
}

/// Message of a failure without the context and code prefixes added by
/// the client ("VMware VM: invalid_argument: …").
pub fn plain_message(f: &Failure) -> String {
    let mut m = f.message.as_str();
    for prefix in ["VMware VM: ", "VMware import: ", "cancellation: "] {
        if let Some(rest) = m.strip_prefix(prefix) {
            m = rest;
            break;
        }
    }
    if let Some(code) = f.code
        && let Some(rest) = m.strip_prefix(&format!("{}: ", code.as_str()))
    {
        m = rest;
    }
    let mut s = m.to_owned();
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

/// Enough free space on the catalog volume for the converted disk (unknown
/// free space counts as enough: the supervisor checks again).
pub fn space_ok(p: &VmwareImportPreview) -> bool {
    p.free_bytes.is_none_or(|f| f >= p.required_bytes)
}

/// Draws the page; returns the requested action.
pub fn import_ui(
    ui: &mut egui::Ui,
    flow: &mut ImportFlow,
    env: &ImportEnv<'_>,
) -> Option<ImportAction> {
    let mut action = None;
    theme::page_header(ui, TITLE, DESCRIPTION, |_| {});
    let s = env.state;
    if flow.submitted {
        outcome_view(ui, flow, env, &mut action);
        return action;
    }
    match &s.preview {
        RequestStatus::Idle => {
            let text = if env.file_picker {
                "Choose the .vmx file of the VMware VM."
            } else {
                "File picker not available on this platform."
            };
            theme::card(ui, |ui| {
                ui.label(text);
            });
            buttons(ui, env, &mut action, false, None);
        }
        RequestStatus::Pending => {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Examining \u{201c}{}\u{201d} and its disks…",
                        flow.file_name
                    ));
                });
            });
            buttons(ui, env, &mut action, false, None);
        }
        RequestStatus::Failed(f) => {
            theme::card(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("\u{201c}{}\u{201d}", flow.file_name)).strong(),
                );
                ui.add_space(6.0);
                theme::notice(
                    ui,
                    &format!("This VM cannot be imported: {}", plain_message(f)),
                    Tone::Danger,
                );
            });
            buttons(ui, env, &mut action, true, None);
        }
        RequestStatus::Done(p) => {
            if !flow.name_prefilled {
                flow.name = p.config.name.as_str().to_owned();
                flow.name_prefilled = true;
                flow.focus_name = true;
            }
            if !p.vmware_svga_offered {
                flow.display = DisplayAdapter::Standard;
            }
            let validation = validate_name(&flow.name, env.existing_names);
            preview_view(ui, flow, p, &validation);
            let ok = env.active && validation.is_ok() && space_ok(p);
            let import = validation
                .ok()
                .filter(|_| ok)
                .map(|name| ImportAction::Import {
                    path: flow.path.clone(),
                    name,
                    display: flow.display,
                });
            buttons(ui, env, &mut action, true, Some(import));
            if matches!(action, Some(ImportAction::Import { .. })) {
                flow.submitted = true;
            }
        }
    }
    action
}

/// Bottom row: Cancel, Choose another file and, with a preview, Import.
fn buttons(
    ui: &mut egui::Ui,
    env: &ImportEnv<'_>,
    action: &mut Option<ImportAction>,
    retry: bool,
    import: Option<Option<ImportAction>>,
) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let size = egui::vec2(96.0, 0.0);
        if ui.add(egui::Button::new("Cancel").min_size(size)).clicked() {
            *action = Some(ImportAction::Close);
        }
        if ui
            .add_enabled(
                env.active && env.file_picker,
                egui::Button::new("Choose another .vmx…"),
            )
            .clicked()
        {
            *action = Some(ImportAction::ChooseOther);
        }
        if retry
            && import.is_none()
            && ui
                .add_enabled(env.active, egui::Button::new("Examine again"))
                .clicked()
        {
            *action = Some(ImportAction::Retry);
        }
        if let Some(import) = import {
            ui.add_space((ui.available_width() - size.x).max(0.0));
            let r = ui
                .add_enabled(
                    import.is_some(),
                    theme::primary(ui, "Import").min_size(size),
                )
                .on_disabled_hover_text(if env.active {
                    "Enter a valid name; the catalog volume needs enough free space"
                } else {
                    "The supervisor is not connected"
                });
            if r.clicked()
                && let Some(a) = import
            {
                *action = Some(a);
            }
        }
    });
}

fn grid_row(ui: &mut egui::Ui, k: &str, v: String) {
    let p = theme::Palette::of(ui);
    ui.label(egui::RichText::new(k).color(p.text_weak));
    ui.add(egui::Label::new(egui::RichText::new(v).color(p.text)).wrap());
    ui.end_row();
}

fn preview_view(
    ui: &mut egui::Ui,
    flow: &mut ImportFlow,
    pr: &VmwareImportPreview,
    validation: &Result<VmName, String>,
) {
    let p = theme::Palette::of(ui);
    let c = &pr.config;
    let hw = c.imported;
    let d = &pr.disk;

    // Warnings first: they decide whether the import makes sense.
    if !pr.warnings.is_empty() {
        theme::group_title(ui, "Warnings");
        for w in &pr.warnings {
            let tone = if w.kind == ImportWarningKind::StorageDriverPreparation {
                Tone::Danger
            } else {
                Tone::Warning
            };
            theme::notice(ui, &warning_text(w), tone);
            ui.add_space(4.0);
        }
    }

    theme::group_title(ui, "VMware VM");
    theme::card(ui, |ui| {
        egui::Grid::new("lightvm_vmware_source")
            .num_columns(2)
            .spacing([28.0, 6.0])
            .show(ui, |ui| {
                grid_row(ui, "File", pr.vmx_file.clone());
                if let Some(n) = &pr.display_name {
                    grid_row(ui, "VMware name", n.clone());
                }
                grid_row(ui, "Guest type", pr.vmware_guest_os.clone());
                grid_row(
                    ui,
                    "Disk",
                    format!(
                        "{} · {} on {} ({})",
                        d.file_name,
                        d.slot,
                        vmware_controller_label(d.vmware_controller),
                        d.create_type
                    ),
                );
                grid_row(
                    ui,
                    "Disk size",
                    format!(
                        "{} virtual · {} used on the host",
                        format_bytes(d.virtual_size_bytes),
                        format_bytes(d.allocated_bytes)
                    ),
                );
                grid_row(
                    ui,
                    "Snapshots",
                    match d.snapshot_levels {
                        0 => "none".to_owned(),
                        1 => "1 level, flattened into the converted disk".to_owned(),
                        n => format!("{n} levels, flattened into the converted disk"),
                    },
                );
            });
    });

    theme::group_title(ui, "New LightVM VM");
    theme::card(ui, |ui| {
        let label = ui.label(egui::RichText::new("Name").color(p.text));
        let r = ui.add(
            egui::TextEdit::singleline(&mut flow.name)
                .id_salt("lightvm_vmware_name")
                .char_limit(64)
                .desired_width(320.0),
        );
        let r = r.labelled_by(label.id);
        if flow.focus_name {
            r.request_focus();
            flow.focus_name = false;
        }
        let (enabled, value) = (r.enabled(), flow.name.clone());
        r.widget_info(|| {
            let mut i = egui::WidgetInfo::text_edit(enabled, value.clone(), value.clone(), "");
            i.label = Some(NAME_FIELD.into());
            i
        });
        match validation {
            Ok(_) => {
                theme::status(ui, "Valid name", Tone::Positive);
            }
            Err(e) => {
                theme::status(ui, e, Tone::Danger);
            }
        }
        ui.add_space(8.0);
        egui::Grid::new("lightvm_vmware_target")
            .num_columns(2)
            .spacing([28.0, 6.0])
            .show(ui, |ui| {
                grid_row(ui, "System", os_label(c.guest_os).into());
                grid_row(ui, "Firmware", firmware_text(c.guest_os, c.firmware).into());
                let cpus = match hw.and_then(|h| h.cores_per_socket) {
                    Some(k) if k > 1 && c.vcpus > k => {
                        format!("{} vCPU ({} sockets × {k} cores)", c.vcpus, c.vcpus / k)
                    }
                    Some(k) if k > 1 => format!("{} vCPU (1 socket × {k} cores)", c.vcpus),
                    _ => format!("{} vCPU", c.vcpus),
                };
                grid_row(ui, "Virtual processors", cpus);
                grid_row(ui, "Memory", format_mib(c.memory_mib));
                grid_row(
                    ui,
                    "Disk",
                    format!("{} GiB, converted to QCOW2", c.disk_gib),
                );
                if let Some(h) = hw {
                    grid_row(
                        ui,
                        "Disk controller",
                        format!(
                            "{} (VMware: {})",
                            controller_label(h.disk_controller),
                            vmware_controller_label(d.vmware_controller)
                        ),
                    );
                }
                grid_row(ui, "Network", "Offline".into());
            });
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Display adapter").color(p.text));
        if pr.vmware_svga_offered {
            ui.radio_value(
                &mut flow.display,
                DisplayAdapter::Standard,
                "Standard VGA (works with every Windows)",
            );
            ui.radio_value(
                &mut flow.display,
                DisplayAdapter::VmwareSvga,
                "VMware SVGA II: uses the VMware Tools display driver of the guest",
            );
            if pr.vmware_tools_hint {
                ui.label(
                    egui::RichText::new("The VMware Tools appear to be installed in this VM.")
                        .small()
                        .color(p.text_weak),
                );
            }
        } else {
            ui.label(
                egui::RichText::new(display_label(DisplayAdapter::Standard, c.guest_os))
                    .color(p.text_weak),
            );
        }
    });

    theme::group_title(ui, "Imported");
    theme::card(ui, |ui| {
        let mut items = vec![
            format!(
                "System: {} ({})",
                os_label(c.guest_os),
                firmware_text(c.guest_os, c.firmware)
            ),
            format!(
                "Processors and memory: {} vCPU, {}",
                c.vcpus,
                format_mib(c.memory_mib)
            ),
            if d.snapshot_levels > 0 {
                "Disk: the current state, snapshots flattened into one QCOW2 disk".to_owned()
            } else {
                "Disk: converted into a QCOW2 disk of the LightVM catalog".to_owned()
            },
        ];
        if let Some(h) = hw {
            items.push(format!(
                "Disk controller: {}",
                controller_label(h.disk_controller)
            ));
        }
        if c.firmware == Firmware::Uefi {
            items.push("Firmware: UEFI, with new firmware settings".into());
        }
        for i in items {
            ui.label(format!("• {i}"));
        }
    });

    theme::group_title(ui, "Not imported");
    theme::card(ui, |ui| {
        if pr.not_imported.is_empty() {
            ui.label(egui::RichText::new("Nothing else in this VM.").color(p.text_weak));
        }
        for n in &pr.not_imported {
            let text = match &n.detail {
                Some(d) if !d.is_empty() => format!("• {} ({d})", not_imported_text(n.item)),
                _ => format!("• {}", not_imported_text(n.item)),
            };
            ui.add(egui::Label::new(text).wrap());
        }
    });

    ui.add_space(8.0);
    let space = match pr.free_bytes {
        Some(f) => format!(
            "Space needed on the catalog volume: {} · free: {}",
            format_bytes(pr.required_bytes),
            format_bytes(f)
        ),
        None => format!(
            "Space needed on the catalog volume: {}",
            format_bytes(pr.required_bytes)
        ),
    };
    if space_ok(pr) {
        theme::status(ui, &space, Tone::Neutral);
    } else {
        theme::notice(
            ui,
            &format!("{space}. Not enough free space: free some space or move the catalog."),
            Tone::Danger,
        );
    }
}

/// Progress, then the outcome of the import.
fn outcome_view(
    ui: &mut egui::Ui,
    flow: &mut ImportFlow,
    env: &ImportEnv<'_>,
    action: &mut Option<ImportAction>,
) {
    let s = env.state;
    let size = egui::vec2(96.0, 0.0);
    match &s.import {
        RequestStatus::Idle | RequestStatus::Pending => {
            let cancelling = s.cancel.is_pending() || matches!(s.cancel, RequestStatus::Done(true));
            theme::card(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("Importing \u{201c}{}\u{201d}", flow.name))
                        .strong(),
                );
                ui.add_space(6.0);
                let (phase, percent) = s.progress.unwrap_or((VmwareImportPhase::Checking, 0));
                let text = if cancelling {
                    "Cancelling: the partial disk is removed…"
                } else {
                    phase_text(phase)
                };
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(text);
                });
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(f32::from(percent) / 100.0)
                        .show_percentage()
                        .desired_width(ui.available_width().min(520.0)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "The VMware files are only read. Cancelling leaves nothing behind.",
                    )
                    .small()
                    .color(theme::Palette::of(ui).text_weak),
                );
                if let RequestStatus::Failed(f) = &s.cancel {
                    theme::notice(
                        ui,
                        &format!("Cancellation refused: {}", plain_message(f)),
                        Tone::Warning,
                    );
                }
            });
            ui.add_space(14.0);
            if ui
                .add_enabled(
                    env.active && !cancelling,
                    theme::danger(ui, "Cancel import").min_size(size),
                )
                .clicked()
            {
                *action = Some(ImportAction::CancelImport);
            }
        }
        RequestStatus::Done(vm) => {
            done_view(ui, vm);
            ui.add_space(14.0);
            if ui
                .add(theme::primary(ui, "Go to the VM").min_size(size))
                .clicked()
            {
                *action = Some(ImportAction::Close);
            }
        }
        RequestStatus::Failed(f) => {
            let cancelled = matches!(s.cancel, RequestStatus::Done(true));
            theme::card(ui, |ui| {
                if cancelled {
                    theme::notice(
                        ui,
                        "Import cancelled: no VM was created and the partial disk was removed.",
                        Tone::Warning,
                    );
                } else {
                    theme::notice(
                        ui,
                        &format!("The import failed: {}", plain_message(f)),
                        Tone::Danger,
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "No VM was created. The VMware files were not modified.",
                        )
                        .small()
                        .color(theme::Palette::of(ui).text_weak),
                    );
                }
            });
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Close").min_size(size)).clicked() {
                    *action = Some(ImportAction::Close);
                }
                if ui
                    .add_enabled(env.active, egui::Button::new("Back to the preview"))
                    .clicked()
                {
                    flow.submitted = false;
                    *action = Some(ImportAction::Retry);
                }
            });
        }
    }
}

fn done_view(ui: &mut egui::Ui, vm: &VmRecord) {
    theme::card(ui, |ui| {
        theme::notice(
            ui,
            &format!(
                "Imported: \u{201c}{}\u{201d} is in the catalog with its converted disk.",
                vm.config.name.as_str()
            ),
            Tone::Positive,
        );
        if vm
            .config
            .imported
            .is_some_and(|h| h.storage_driver_preparation)
        {
            ui.add_space(6.0);
            theme::notice(ui, STORAGE_DRIVER_WARNING, Tone::Warning);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightvm_proto::ErrorCode;

    #[test]
    fn failure_messages_lose_the_client_prefixes() {
        let f = Failure::coded(
            ErrorCode::StateConflict,
            "VMware VM: state_conflict: the VM is suspended: resume it in VMware",
        );
        assert_eq!(
            plain_message(&f),
            "The VM is suspended: resume it in VMware"
        );
        assert_eq!(plain_message(&Failure::new("other: x")), "Other: x");
    }

    #[test]
    fn storage_driver_warning_says_windows_may_not_start() {
        let t = warning_text(&ImportWarning {
            kind: ImportWarningKind::StorageDriverPreparation,
            detail: None,
        });
        assert!(t.starts_with("Windows may not start: storage driver preparation needed"));
        assert!(t.contains("Importing a VMware VM"));
    }
}
