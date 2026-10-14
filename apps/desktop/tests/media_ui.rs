//! Settings → Installation media: the imported ISOs with the VMs that use
//! them, and their deletion (confirmed, refused while a VM uses the medium,
//! refusals of the supervisor explained). Whole app with a stand-in
//! supervisor.

mod common;

use common::{FakeSupervisor, connected, media_id, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::{Failure, RequestStatus, ServiceCommand, ServiceState};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::{ErrorCode, VmState};

/// Medium 1 (installation) used by "web-test"; medium 2 (data) unused.
fn state() -> ServiceState {
    let mut v = vm(1, "web-test", VmState::Stopped);
    v.config.install_media = Some(media_id(1));
    connected(vec![v, vm(2, "other", VmState::Running)])
}

fn harness(state: ServiceState) -> (Harness<'static, LightVmApp>, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    app.set_section(Section::Settings);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 2600.0))
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

const INSTALL: &str = "Delete medium ubuntu-24.04.3-live-server-amd64.iso";
const DATA: &str = "Delete medium tools.iso";

#[test]
fn media_are_listed_with_the_vms_that_use_them() {
    let (h, _hs) = harness(state());
    h.get_by_label("Installation media");
    h.get_by_label("ubuntu-24.04.3-live-server-amd64.iso");
    h.get_by_label("tools.iso");
    h.get_by_label_contains("used by \u{201c}web-test\u{201d}");
    h.get_by_label_contains("not used by any VM");
    // In use: Delete disabled (the reason is the hover text).
    assert!(!enabled(&h, INSTALL));
    assert!(enabled(&h, DATA));
}

#[test]
fn deletion_is_confirmed_and_waits_for_the_outcome() {
    let (mut h, hs) = harness(state());
    hs.clear();
    click(&mut h, DATA);
    assert!(h.state().dialog_open());
    h.get_by_label("Delete \u{201c}tools.iso\u{201d}?");
    h.get_by_label_contains("is removed from the host permanently");
    // Cancel: nothing sent.
    click(&mut h, "Cancel");
    assert!(!h.state().dialog_open());
    assert!(hs.actions().is_empty());

    click(&mut h, DATA);
    click(&mut h, "Delete medium");
    assert_eq!(hs.actions(), [ServiceCommand::DeleteMedia(media_id(2))]);
    h.get_by_label("Waiting for the supervisor…");
    assert!(!enabled(&h, "Delete medium"));

    // Done: the dialog closes and the list no longer shows the medium.
    hs.update(|s| {
        s.media.records.retain(|m| m.id != media_id(2));
        s.delete_media = RequestStatus::Done(media_id(2));
    });
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    assert!(h.query_by_label("tools.iso").is_none());
}

#[test]
fn refusals_of_the_supervisor_are_explained() {
    let (mut h, hs) = harness(state());
    click(&mut h, DATA);
    click(&mut h, "Delete medium");
    // A VM started using it meanwhile (another client): refused, the VM
    // named, the dialog stays open.
    hs.update(|s| {
        s.delete_media = RequestStatus::Failed(
            Failure::coded(
                ErrorCode::StateConflict,
                "media deletion: state_conflict: the medium is used by 1 VM: \u{201c}late\u{201d}; remove it from their settings or delete them first",
            )
            .with_detail("reason", "media_in_use"),
        );
    });
    h.run_steps(4);
    assert!(h.state().dialog_open());
    h.get_by_label_contains("The medium was not deleted: it is used by 1 VM: \u{201c}late\u{201d}");
    // Held by a run.
    hs.update(|s| {
        s.delete_media =
            RequestStatus::Failed(Failure::coded(ErrorCode::VmBusy, "media deletion: vm_busy"));
    });
    h.run_steps(4);
    h.get_by_label_contains("holds it open. Stop the VM and try again.");
}

#[test]
fn a_medium_that_became_used_cannot_be_confirmed() {
    let (mut h, hs) = harness(state());
    click(&mut h, DATA);
    hs.clear();
    hs.update(|s| s.vms[1].config.data_media = Some(media_id(2)));
    h.run_steps(4);
    h.get_by_label_contains("Used by \u{201c}other\u{201d}");
    click(&mut h, "Delete medium");
    assert!(hs.actions().is_empty());
}

#[test]
fn no_deletion_without_the_supervisor_and_empty_list() {
    let mut s = state();
    s.connected = false;
    let (h, _hs) = harness(s);
    assert!(!enabled(&h, DATA));

    let mut s = state();
    s.media.records.clear();
    s.vms[0].config.install_media = None;
    let (h, _hs) = harness(s);
    h.get_by_label("No imported media.");
}

/// ADR-0029: a medium used in place shows where the file is; deleting it
/// says the file is kept; a file no longer found is shown as missing with
/// what to do; a medium imported by an earlier version is LightVM's copy.
#[test]
fn location_missing_file_and_deletion_of_a_file_used_in_place() {
    let mut s = state();
    s.vms[0].config.install_media = None;
    let (mut h, hs) = harness(s);
    h.get_by_label_contains(
        r"D:\ISO\ubuntu-24.04.3-live-server-amd64.iso · used where it is, not copied",
    );
    h.get_by_label_contains("Read-only copy in the LightVM catalog");
    assert!(h.query_by_label("File missing").is_none());
    click(&mut h, INSTALL);
    h.get_by_label_contains("The ISO file itself");
    h.get_by_label_contains("is kept where it is and is not changed");
    click(&mut h, "Cancel");

    hs.update(|s| s.media.records[0].missing = true);
    h.run_steps(4);
    h.get_by_label("File missing");
    h.get_by_label_contains(
        r"File not found at D:\ISO\ubuntu-24.04.3-live-server-amd64.iso: put it back there, or import it again",
    );
    // Still deletable (only the registry row goes).
    assert!(enabled(&h, INSTALL));
}
