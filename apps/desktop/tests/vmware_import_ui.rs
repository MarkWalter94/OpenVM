//! "Import VMware VM" (ADR-0030) in the whole app, driven headless with a
//! stand-in supervisor and a fake `.vmx` picker: entry on the Dashboard,
//! preview with what is imported, what not and the warnings, the name of
//! the new VM, progress with cancellation, outcome.

mod common;

use std::path::PathBuf;

use common::{FakeSupervisor, connected, vm, vmware_preview_linux, vmware_preview_lsilogic};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::LightVmApp;
use lightvm_desktop::service::{Failure, RequestStatus, ServiceCommand, ServiceState};
use lightvm_proto::ipc::VmwareImportPhase;
use lightvm_proto::{DisplayAdapter, ErrorCode, VmName, VmRecord, VmState};

fn vmx() -> PathBuf {
    PathBuf::from(r"D:\VMs\Windows 7 x64\Windows 7 x64.vmx")
}

fn harness(state: ServiceState, picker: bool) -> (Harness<'static, LightVmApp>, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    if picker {
        app.set_vmx_picker(Some(Box::new(|_t: &str| Some(vmx()))));
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run_steps(4);
    (h, hs)
}

fn button<'a>(h: &'a Harness<'_, LightVmApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(Role::Button, label)
}

fn enabled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    !button(h, label).accesskit_node().is_disabled()
}

fn click(h: &mut Harness<'_, LightVmApp>, label: &str) {
    button(h, label).click();
    h.run_steps(4);
}

/// Opens the page with the fake picker and answers the preview.
fn open_with(
    h: &mut Harness<'_, LightVmApp>,
    hs: &common::Handles,
    preview: RequestStatus<Box<lightvm_proto::ipc::VmwareImportPreview>>,
) {
    click(h, "Import VMware VM");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::PreviewVmwareImport(vmx())],
        "the chosen .vmx is examined"
    );
    h.get_by_label_contains("Examining \u{201c}Windows 7 x64.vmx\u{201d}");
    hs.update(|s| s.vmware.preview = preview);
    h.run_steps(4);
}

#[test]
fn entry_on_the_dashboard_opens_the_picker() {
    let (mut h, hs) = harness(connected(vec![]), true);
    assert!(enabled(&h, "Import VMware VM"));
    hs.clear();
    click(&mut h, "Import VMware VM");
    assert!(h.state().vmware_import().is_some());
    assert_eq!(hs.actions(), [ServiceCommand::PreviewVmwareImport(vmx())]);
    // Cancel closes the page, nothing else is sent.
    click(&mut h, "Cancel");
    assert!(h.state().vmware_import().is_none());
    assert_eq!(hs.actions().len(), 1);
}

#[test]
fn without_a_picker_the_page_says_why() {
    let (mut h, hs) = harness(connected(vec![]), false);
    hs.clear();
    click(&mut h, "Import VMware VM");
    h.get_by_label("File picker not available on this platform.");
    assert!(!enabled(&h, "Choose another .vmx…"));
    assert!(hs.actions().is_empty());
}

#[test]
fn lsilogic_windows_preview_warns_and_imports() {
    let (mut h, hs) = harness(connected(vec![]), true);
    hs.clear();
    open_with(
        &mut h,
        &hs,
        RequestStatus::Done(Box::new(vmware_preview_lsilogic())),
    );
    h.get_by_label("windows7-64");
    h.get_by_label_contains("Windows 7 x64-000002.vmdk · scsi0:0 on LSI Logic parallel SCSI");
    h.get_by_label("2 levels, flattened into the converted disk");
    h.get_by_label("2 vCPU (1 socket × 2 cores)");
    h.get_by_label("SATA (AHCI) (VMware: LSI Logic parallel SCSI)");
    h.get_by_label_contains("Windows may not start: storage driver preparation needed");
    h.get_by_label_contains("Shared folders: they will come later through LightVM Tools");
    h.get_by_label_contains(
        "USB controllers and devices: they will come later through LightVM Tools",
    );
    h.get_by_label_contains("Network adapters: LightVM VMs are offline");
    h.get_by_label_contains("imported as a data medium from the media library");
    h.get_by_label_contains("Space needed on the catalog volume");
    // Name prefilled from the VMware display name; SVGA offered.
    assert_eq!(
        h.get_by_role(Role::TextInput).value().as_deref(),
        Some("Windows 7 x64 - VS2010")
    );
    h.get_by_label("VMware SVGA II: uses the VMware Tools display driver of the guest")
        .click();
    h.run_steps(4);
    hs.clear();
    click(&mut h, "Import");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::ImportVmware {
            path: vmx(),
            name: VmName::parse("Windows 7 x64 - VS2010").unwrap(),
            display: DisplayAdapter::VmwareSvga,
        }]
    );
}

#[test]
fn clean_linux_preview_has_no_display_choice() {
    let (mut h, hs) = harness(connected(vec![]), true);
    open_with(
        &mut h,
        &hs,
        RequestStatus::Done(Box::new(vmware_preview_linux())),
    );
    h.get_by_label("VMware Paravirtual SCSI (PVSCSI) (VMware: VMware Paravirtual SCSI)");
    h.get_by_label("none");
    h.get_by_label_contains("boot entries of VMware are not carried over");
    assert!(
        h.query_by_label_contains("storage driver preparation")
            .is_none()
    );
    assert!(h.query_by_label_contains("VMware SVGA II").is_none());
    hs.clear();
    click(&mut h, "Import");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::ImportVmware {
            path: vmx(),
            name: VmName::parse("debian12").unwrap(),
            display: DisplayAdapter::Standard,
        }]
    );
}

#[test]
fn invalid_or_taken_name_blocks_the_import() {
    // "debian12" exists already.
    let (mut h, hs) = harness(connected(vec![vm(1, "debian12", VmState::Stopped)]), true);
    open_with(
        &mut h,
        &hs,
        RequestStatus::Done(Box::new(vmware_preview_linux())),
    );
    h.get_by_label("A VM with this name already exists.");
    assert!(!enabled(&h, "Import"));
    // A character VmName refuses.
    h.get_by_role(Role::TextInput).type_text("/");
    h.run_steps(4);
    assert!(h.query_by_label("Valid name").is_none());
    assert!(!enabled(&h, "Import"));

    // Another name: valid, the import can start.
    let (mut h, hs) = harness(connected(vec![vm(1, "debian12", VmState::Stopped)]), true);
    open_with(
        &mut h,
        &hs,
        RequestStatus::Done(Box::new(vmware_preview_linux())),
    );
    h.get_by_role(Role::TextInput).type_text("x");
    h.run_steps(4);
    h.get_by_label("Valid name");
    assert!(enabled(&h, "Import"));
}

#[test]
fn not_enough_space_blocks_the_import() {
    let (mut h, hs) = harness(connected(vec![]), true);
    let mut p = vmware_preview_linux();
    p.free_bytes = Some(1 << 30);
    open_with(&mut h, &hs, RequestStatus::Done(Box::new(p)));
    h.get_by_label_contains("Not enough free space");
    assert!(!enabled(&h, "Import"));
}

#[test]
fn refused_preview_explains_why() {
    let (mut h, hs) = harness(connected(vec![]), true);
    open_with(
        &mut h,
        &hs,
        RequestStatus::Failed(Failure::coded(
            ErrorCode::StateConflict,
            "VMware VM: state_conflict: the VM is suspended: resume it in VMware and shut it down, then import it again",
        )),
    );
    h.get_by_label(
        "This VM cannot be imported: The VM is suspended: resume it in VMware and shut it down, then import it again",
    );
    hs.clear();
    click(&mut h, "Examine again");
    assert_eq!(hs.actions(), [ServiceCommand::PreviewVmwareImport(vmx())]);
}

#[test]
fn progress_cancel_and_outcomes() {
    let (mut h, hs) = harness(connected(vec![]), true);
    open_with(
        &mut h,
        &hs,
        RequestStatus::Done(Box::new(vmware_preview_linux())),
    );
    click(&mut h, "Import");
    h.get_by_label("Importing \u{201c}debian12\u{201d}");
    hs.update(|s| s.vmware.progress = Some((VmwareImportPhase::Converting, 42)));
    h.run_steps(4);
    h.get_by_label("Converting the disk…");
    hs.clear();
    click(&mut h, "Cancel import");
    assert_eq!(hs.actions(), [ServiceCommand::CancelVmwareImport]);
    assert!(!enabled(&h, "Cancel import"));
    h.get_by_label("Cancelling: the partial disk is removed…");
    // The service cancels: the import fails with the cancellation.
    hs.update(|s| {
        s.vmware.cancel = RequestStatus::Done(true);
        s.vmware.import = RequestStatus::Failed(Failure::coded(
            ErrorCode::StateConflict,
            "VMware import: state_conflict: import cancelled",
        ));
    });
    h.run_steps(4);
    h.get_by_label_contains("Import cancelled: no VM was created");
    // Back to the preview, import again: this time it fails.
    hs.clear();
    click(&mut h, "Back to the preview");
    assert_eq!(hs.actions(), [ServiceCommand::PreviewVmwareImport(vmx())]);
    hs.update(|s| s.vmware.preview = RequestStatus::Done(Box::new(vmware_preview_linux())));
    h.run_steps(4);
    click(&mut h, "Import");
    hs.update(|s| {
        s.vmware.import = RequestStatus::Failed(Failure::coded(
            ErrorCode::ResourceLimit,
            "VMware import: resource_limit: not enough free space on the catalog volume (qemu-img convert)",
        ));
    });
    h.run_steps(4);
    h.get_by_label_contains(
        "The import failed: Not enough free space on the catalog volume (qemu-img convert)",
    );
    // Third attempt succeeds: the page shows the VM and closes.
    click(&mut h, "Back to the preview");
    hs.update(|s| s.vmware.preview = RequestStatus::Done(Box::new(vmware_preview_linux())));
    h.run_steps(4);
    click(&mut h, "Import");
    let p = vmware_preview_linux();
    hs.update(|s| {
        s.vmware.import = RequestStatus::Done(VmRecord {
            id: common::vm_id(9),
            config: p.config.clone(),
            config_revision: 1,
            persisted_state: VmState::Stopped,
            created_at: common::now(),
        });
    });
    h.run_steps(4);
    h.get_by_label_contains("Imported: \u{201c}debian12\u{201d} is in the catalog");
    click(&mut h, "Go to the VM");
    assert!(h.state().vmware_import().is_none());
}
