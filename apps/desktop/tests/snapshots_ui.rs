//! Snapshots page (ADR-0032): the tree with "you are here", the take with a
//! name and a description (VM off or running), the revert (VM off; for a
//! running VM the offer to shut it down normally first and to snapshot the
//! current state before it), the delete and the rename, long operations and
//! the refusals of the supervisor in plain language. Whole app with a
//! stand-in supervisor.

mod common;

use common::{FakeSupervisor, connected, snapshot, snapshot_id, snapshots_list, vm, vm_id};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::LightVmApp;
use lightvm_desktop::service::{
    Failure, RequestStatus, ServiceCommand, ServiceState, SnapshotOp, SnapshotRequest, StopRequest,
};
use lightvm_proto::ipc::StorageOperation;
use lightvm_proto::{ErrorCode, Label, SnapshotId, UtcTimestamp, VmState};

/// Fixed instant of the generated labels: 2026-10-11 09:12 UTC.
fn now() -> UtcTimestamp {
    UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap()
}

const LABEL_3: &str = "Snapshot 3 \u{b7} 2026-10-11 09:12 UTC";

/// A stopped VM that ran (it has a disk), with two snapshots, the VM's
/// current state an overlay of the second one.
fn state() -> ServiceState {
    let mut v = vm(1, "lab", VmState::Stopped);
    v.run_id = Some(common::run_id(1));
    v.snapshots.count = 2;
    v.snapshots.current = Some(snapshot_id(2));
    let mut s = connected(vec![v]);
    if let Some(h) = &mut s.host {
        h.baseline_available = true;
    }
    s.snapshots.insert(vm_id(1), snapshots_list());
    s
}

fn harness(state: ServiceState) -> (Harness<'static, LightVmApp>, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    app.set_fixed_time(Some(now()));
    let h = Harness::builder()
        .with_size(egui::vec2(1100.0, 760.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
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

/// Dashboard → More → Snapshots…
fn open_page(h: &mut Harness<'_, LightVmApp>, name: &str) {
    h.run_steps(4);
    click(h, &format!("More actions for {name}"));
    click(h, &format!("Snapshots of {name}"));
}

fn set_request(hs: &common::Handles, op: SnapshotOp, status: RequestStatus<SnapshotId>) {
    hs.update(|s| {
        s.snapshot = Some(SnapshotRequest {
            vm_id: vm_id(1),
            op,
            status,
        });
    });
}

#[test]
fn page_lists_the_snapshots_and_the_current_state() {
    let (mut h, hs) = harness(state());
    h.run_steps(4);
    // Dashboard row: count and "you are here" in one line.
    h.get_by_label_contains("2 snapshots");
    h.get_by_label_contains("you are here: \u{201c}Before the update\u{201d}");
    open_page(&mut h, "lab");
    assert_eq!(h.state().snapshots_vm(), Some(vm_id(1)));
    assert_eq!(hs.actions(), [ServiceCommand::ListSnapshots(vm_id(1))]);
    h.get_by_label("Snapshots \u{b7} lab");
    // Summary: state, the snapshot the state descends from, count, memory.
    h.get_by_label("Stopped");
    h.get_by_label("the state descends from \u{201c}Before the update\u{201d}");
    h.get_by_label("2 \u{b7} 2.0 MiB of own files");
    h.get_by_label("never saved");
    // Both snapshots, with their badges and details.
    h.get_by_label("Clean install");
    h.get_by_label("Before the update");
    h.get_by_label("Clean");
    h.get_by_label("While running");
    // The caption of the summary and the pill of the current row.
    assert_eq!(h.query_all_by_label("You are here").count(), 2);
    assert_eq!(h.query_all_by_label("Firmware saved").count(), 2);
    assert_eq!(
        h.query_all_by_label_contains("Created 2026-10-10").count(),
        2
    );
    assert_eq!(
        h.query_all_by_label_contains("1.0 MiB of own files")
            .count(),
        2
    );
    // No text field anywhere: the name is chosen in the dialog.
    assert_eq!(common::text_fields(&h), 0);
    // Every row offers its three actions.
    assert!(enabled(
        &h,
        "Revert to snapshot \u{201c}Clean install\u{201d}"
    ));
    assert!(enabled(&h, "Delete snapshot \u{201c}Clean install\u{201d}"));
    assert!(enabled(
        &h,
        "Rename snapshot \u{201c}Before the update\u{201d}"
    ));
    // Back to the Dashboard.
    click(&mut h, "Back to the Dashboard");
    assert_eq!(h.state().snapshots_vm(), None);
    h.get_by_label("Virtual machines");
}

#[test]
fn take_is_confirmed_with_a_name_and_a_description() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Take a snapshot of lab");
    assert!(h.state().dialog_open());
    h.get_by_label("Take a snapshot of \u{201c}lab\u{201d}?");
    h.get_by_label_contains("No memory is saved");
    h.get_by_label_contains(
        "reverting to it later restarts the guest from that disk state with a normal boot",
    );
    h.get_by_label("Name");
    h.get_by_label("Description (optional)");
    assert_eq!(common::text_fields(&h), 2);
    // An invalid name (longer than 256 characters) keeps the action disabled.
    h.get_by_role(Role::TextInput).click();
    h.get_by_role(Role::TextInput).type_text(&"x".repeat(260));
    h.run_steps(2);
    assert!(!enabled(&h, "Take snapshot"));
    // Esc cancels without effect.
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    assert!(hs.actions().is_empty());

    // Confirmed with the generated name: a long operation.
    click(&mut h, "Take a snapshot of lab");
    click(&mut h, "Take snapshot");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::TakeSnapshot {
            vm_id: vm_id(1),
            name: Label::parse(LABEL_3).unwrap(),
            description: String::new(),
        }]
    );
    h.get_by_label("Waiting for the supervisor\u{2026}");
    // The supervisor reports the operation running: the dialog closes, the
    // page shows the progress and every change of the VM is disabled.
    hs.update(|s| s.vms[0].baselines.operation = Some(StorageOperation::TakeSnapshot));
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Taking the snapshot\u{2026}");
    h.get_by_label_contains("No memory is saved");
    for b in [
        "Take a snapshot of lab",
        "Revert to snapshot \u{201c}Clean install\u{201d}",
        "Delete snapshot \u{201c}Clean install\u{201d}",
    ] {
        assert!(!enabled(&h, b), "{b}");
    }
    // Done: the new snapshot in the tree, outcome stated.
    let mut new = snapshot(3, 1, LABEL_3, Some(snapshot_id(2)), false, true);
    new.created_at = now();
    hs.update(|s| {
        s.vms[0].baselines.operation = None;
        s.vms[0].snapshots.count = 3;
        s.vms[0].snapshots.current = Some(snapshot_id(3));
        s.snapshots
            .get_mut(&vm_id(1))
            .unwrap()
            .tree
            .as_mut()
            .unwrap()
            .snapshots
            .push(new);
    });
    set_request(&hs, SnapshotOp::Take, RequestStatus::Done(snapshot_id(3)));
    h.run_steps(4);
    h.get_by_label("Snapshot taken.");
    assert!(enabled(&h, "Take a snapshot of lab"));
}

#[test]
fn take_while_running_is_crash_consistent() {
    let mut s = state();
    s.vms[0].state = VmState::Running;
    let (mut h, hs) = harness(s);
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Take a snapshot of lab");
    h.get_by_label_contains(
        "the disk is snapshotted exactly as the guest left it, like a power cut at that instant (crash-consistent)",
    );
    click(&mut h, "Take snapshot");
    h.get_by_label("Waiting for the supervisor\u{2026}");
    hs.update(|s| s.vms[0].baselines.operation = Some(StorageOperation::TakeSnapshot));
    h.run_steps(4);
    h.get_by_label("Taking the snapshot (the VM is running)\u{2026}");
    h.get_by_label_contains("crash-consistent");
}

#[test]
fn revert_with_the_vm_off_discards_and_reverts() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Revert to snapshot \u{201c}Clean install\u{201d}");
    h.get_by_label("Revert to \u{201c}Clean install\u{201d}?");
    h.get_by_label_contains("is discarded");
    h.get_by_label_contains("the guest starts from the saved disk state with a normal boot");
    h.get_by_label_contains("This cannot be undone");
    // Keyboard focus on Cancel.
    assert!(button(&h, "Cancel").accesskit_node().is_focused());
    click(&mut h, "Discard and revert");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::RevertSnapshot {
            vm_id: vm_id(1),
            snapshot_id: snapshot_id(1),
        }]
    );
    h.get_by_label("Waiting for the supervisor\u{2026}");
    hs.update(|s| s.vms[0].baselines.operation = Some(StorageOperation::RevertSnapshot));
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Reverting to the snapshot\u{2026}");
    // Done: the current state now descends from the reverted snapshot.
    hs.update(|s| {
        s.vms[0].baselines.operation = None;
        s.vms[0].snapshots.current = Some(snapshot_id(1));
        s.snapshots
            .get_mut(&vm_id(1))
            .unwrap()
            .tree
            .as_mut()
            .unwrap()
            .current = Some(snapshot_id(1));
    });
    set_request(
        &hs,
        SnapshotOp::Revert(snapshot_id(1)),
        RequestStatus::Done(snapshot_id(1)),
    );
    h.run_steps(4);
    h.get_by_label("Reverted \u{201c}Clean install\u{201d}: the next start boots the guest from that disk state, with a normal boot.");
}

#[test]
fn revert_of_a_running_vm_shuts_down_first_and_offers_a_snapshot() {
    let mut s = state();
    s.vms[0].state = VmState::Running;
    let (mut h, hs) = harness(s);
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Revert to snapshot \u{201c}Clean install\u{201d}");
    h.get_by_label("Revert to \u{201c}Clean install\u{201d}?");
    h.get_by_label("The VM is running, so first:");
    let shut_down = "Shut down \u{201c}lab\u{201d} normally first (a revert needs the VM off)";
    // Without the consent to the shutdown the action is disabled.
    assert!(!enabled(&h, "Shut down and revert"));
    h.get_by_role_and_label(Role::CheckBox, shut_down).click();
    h.run_steps(4);
    assert!(enabled(&h, "Shut down and revert"));
    // Only the shutdown: the stop, then the revert once the VM is off.
    click(&mut h, "Shut down and revert");
    assert!(!h.state().dialog_open());
    assert_eq!(hs.actions(), [stop_run()]);
    // The VM still runs: the flow waits for the orderly shutdown.
    h.run_steps(4);
    assert_eq!(hs.actions(), [stop_run()]);
    hs.update(|s| {
        s.vms[0].state = VmState::Stopped;
        s.vms[0].run_id = None;
    });
    h.run_steps(4);
    assert_eq!(
        hs.actions(),
        [
            stop_run(),
            ServiceCommand::RevertSnapshot {
                vm_id: vm_id(1),
                snapshot_id: snapshot_id(1),
            },
        ]
    );
}

#[test]
fn revert_of_a_running_vm_keeps_the_current_state_first() {
    let mut s = state();
    s.vms[0].state = VmState::Running;
    let (mut h, hs) = harness(s);
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Revert to snapshot \u{201c}Clean install\u{201d}");
    let shut_down = "Shut down \u{201c}lab\u{201d} normally first (a revert needs the VM off)";
    let keep =
        format!("Before that, take a snapshot of the current state (\u{201c}{LABEL_3}\u{201d})");
    h.get_by_role_and_label(Role::CheckBox, shut_down).click();
    h.get_by_role_and_label(Role::CheckBox, &keep).click();
    h.run_steps(4);
    click(&mut h, "Shut down and revert");
    assert!(!h.state().dialog_open());
    assert_eq!(
        hs.actions(),
        [ServiceCommand::TakeSnapshot {
            vm_id: vm_id(1),
            name: Label::parse(LABEL_3).unwrap(),
            description: "Before the revert to \u{201c}Clean install\u{201d}".into(),
        }]
    );
    // The offered snapshot completes: the orderly shutdown starts.
    set_request(&hs, SnapshotOp::Take, RequestStatus::Done(snapshot_id(3)));
    h.run_steps(4);
    assert_eq!(hs.actions().last(), Some(&stop_run()));
    // The VM is off: the revert.
    hs.update(|s| {
        s.vms[0].state = VmState::Stopped;
        s.vms[0].run_id = None;
    });
    h.run_steps(4);
    assert_eq!(
        hs.actions().last(),
        Some(&ServiceCommand::RevertSnapshot {
            vm_id: vm_id(1),
            snapshot_id: snapshot_id(1),
        })
    );
}

fn stop_run() -> ServiceCommand {
    ServiceCommand::StopRun {
        vm_id: vm_id(1),
        run_id: common::run_id(1),
    }
}

/// Confirms "Shut down and revert" on a running VM (shutdown only).
fn confirm_running_revert(h: &mut Harness<'_, LightVmApp>, hs: &common::Handles) {
    open_page(h, "lab");
    hs.clear();
    click(h, "Revert to snapshot \u{201c}Clean install\u{201d}");
    let shut_down = "Shut down \u{201c}lab\u{201d} normally first (a revert needs the VM off)";
    h.get_by_role_and_label(Role::CheckBox, shut_down).click();
    h.run_steps(4);
    click(h, "Shut down and revert");
    assert_eq!(hs.actions(), [stop_run()]);
}

/// The orderly stop is refused: the flow ends with the reason on the page,
/// and a later stop of the VM does not revert it.
#[test]
fn revert_of_a_running_vm_stops_when_the_shutdown_fails() {
    let mut s = state();
    s.vms[0].state = VmState::Running;
    let (mut h, hs) = harness(s);
    confirm_running_revert(&mut h, &hs);
    hs.update(|s| {
        s.stop = Some(StopRequest {
            vm_id: vm_id(1),
            run_id: common::run_id(1),
            status: RequestStatus::Failed(Failure::new("stop: failed (state_conflict)")),
        });
    });
    h.run_steps(4);
    h.get_by_label(
        "The revert was not done: the VM could not be shut down (stop: failed (state_conflict)).",
    );
    // Stopped later (by the user, hours later): no revert without a new
    // confirmation.
    hs.update(|s| {
        s.vms[0].state = VmState::Stopped;
        s.vms[0].run_id = None;
    });
    h.run_steps(4);
    assert_eq!(hs.actions(), [stop_run()]);
}

/// The VM is started again (another run) before it was seen off: the flow
/// is abandoned, the new run is not reverted when it stops.
#[test]
fn revert_of_a_running_vm_is_abandoned_on_a_new_run() {
    let mut s = state();
    s.vms[0].state = VmState::Running;
    let (mut h, hs) = harness(s);
    confirm_running_revert(&mut h, &hs);
    hs.update(|s| s.vms[0].run_id = Some(common::run_id(7)));
    h.run_steps(4);
    h.get_by_label("The revert was not done: the VM was started again before it was off.");
    hs.update(|s| {
        s.vms[0].state = VmState::Stopped;
        s.vms[0].run_id = None;
    });
    h.run_steps(4);
    assert_eq!(hs.actions(), [stop_run()]);
}

#[test]
fn delete_waits_for_the_outcome() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Delete snapshot \u{201c}Before the update\u{201d}");
    h.get_by_label("Delete snapshot \u{201c}Before the update\u{201d}?");
    h.get_by_label_contains("The other snapshots keep their state");
    h.get_by_label_contains("This cannot be undone");
    click(&mut h, "Delete snapshot");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::DeleteSnapshot {
            vm_id: vm_id(1),
            snapshot_id: snapshot_id(2),
        }]
    );
    h.get_by_label("Waiting for the supervisor\u{2026}");
    // Done: the snapshot is gone, the tree keeps the other one.
    hs.update(|s| {
        let tree = s
            .snapshots
            .get_mut(&vm_id(1))
            .unwrap()
            .tree
            .as_mut()
            .unwrap();
        tree.snapshots.retain(|x| x.snapshot_id != snapshot_id(2));
        tree.current = Some(snapshot_id(1));
        s.vms[0].snapshots.count = 1;
        s.vms[0].snapshots.current = Some(snapshot_id(1));
    });
    set_request(
        &hs,
        SnapshotOp::Delete(snapshot_id(2)),
        RequestStatus::Done(snapshot_id(2)),
    );
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Snapshot deleted. The other snapshots keep their state.");
    assert!(h.query_by_label("Before the update").is_none());
}

#[test]
fn rename_edits_the_name_and_the_description() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Rename snapshot \u{201c}Before the update\u{201d}");
    h.get_by_label("Rename snapshot of \u{201c}lab\u{201d}?");
    h.get_by_label_contains("Nothing changes on the disk");
    assert_eq!(common::text_fields(&h), 2);
    h.get_by_role(Role::MultilineTextInput).click();
    h.run_steps(2);
    h.get_by_role(Role::MultilineTextInput)
        .type_text("kept for the demo");
    h.run_steps(2);
    click(&mut h, "Save");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::RenameSnapshot {
            vm_id: vm_id(1),
            snapshot_id: snapshot_id(2),
            name: Label::parse("Before the update").unwrap(),
            description: "kept for the demo".into(),
        }]
    );
    h.get_by_label("Waiting for the supervisor\u{2026}");
    set_request(
        &hs,
        SnapshotOp::Rename(snapshot_id(2)),
        RequestStatus::Done(snapshot_id(2)),
    );
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Snapshot renamed.");
}

#[test]
fn supervisor_refusals_are_explained() {
    let cases: [(Failure, &str); 6] = [
        (
            Failure::coded(ErrorCode::VmBusy, "snapshot: vm_busy: busy"),
            "the VM is busy (a start, a stop or another operation is in progress, or one of its disks is open in another program)",
        ),
        (
            Failure::coded(ErrorCode::ResourceLimit, "snapshot: resource_limit: space")
                .with_detail("needed_bytes", (6u64 << 30).to_string())
                .with_detail("free_bytes", (2u64 << 30).to_string()),
            "not enough free space on the catalog drive. It needs about 6.0 GiB, 2.0 GiB is free and LightVM keeps a reserve.",
        ),
        (
            Failure::coded(
                ErrorCode::StateConflict,
                "snapshot: state_conflict: reverting to a snapshot needs the VM stopped: shut it down first",
            ),
            "the VM is running, it must be stopped. The Snapshots page offers to shut it down normally first",
        ),
        (
            Failure::coded(
                ErrorCode::StateConflict,
                "snapshot: state_conflict: operation cancelled by the Emergency Stop",
            ),
            "cancelled by the Emergency Stop. Nothing was changed, the VM is as it was.",
        ),
        (
            Failure::coded(
                ErrorCode::Unsupported,
                "snapshot: unsupported: no image tool",
            ),
            "this supervisor cannot handle snapshots",
        ),
        (
            Failure::coded(ErrorCode::NotFound, "snapshot: not found"),
            "the VM or the snapshot no longer exists.",
        ),
    ];
    for (f, expected) in cases {
        let mut s = state();
        s.snapshot = Some(SnapshotRequest {
            vm_id: vm_id(1),
            op: SnapshotOp::Take,
            status: RequestStatus::Failed(f),
        });
        let (mut h, _hs) = harness(s);
        open_page(&mut h, "lab");
        h.get_by_label_contains(expected);
    }
}

#[test]
fn a_running_operation_shows_its_progress_and_disables_the_changes() {
    let mut s = state();
    s.vms[0].baselines.operation = Some(StorageOperation::TakeSnapshot);
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    h.get_by_label("Taking the snapshot\u{2026}");
    h.get_by_label("Taking snapshot");
    for b in [
        "Take a snapshot of lab",
        "Revert to snapshot \u{201c}Clean install\u{201d}",
        "Delete snapshot \u{201c}Clean install\u{201d}",
        "Rename snapshot \u{201c}Clean install\u{201d}",
    ] {
        assert!(!enabled(&h, b), "{b}");
    }
    // The Emergency Stop stays reachable on the page.
    click(&mut h, "Emergency Stop");
}

#[test]
fn one_snapshot_operation_at_a_time() {
    let mut s = state();
    s.snapshot = Some(SnapshotRequest {
        vm_id: vm_id(2),
        op: SnapshotOp::Take,
        status: RequestStatus::Pending,
    });
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    assert!(!enabled(&h, "Take a snapshot of lab"));
    assert!(!enabled(
        &h,
        "Revert to snapshot \u{201c}Clean install\u{201d}"
    ));
    assert!(!enabled(
        &h,
        "Delete snapshot \u{201c}Clean install\u{201d}"
    ));
}

#[test]
fn empty_and_unread_lists() {
    let mut s = state();
    s.snapshots
        .insert(vm_id(1), lightvm_desktop::service::SnapshotList::default());
    let (mut h, hs) = harness(s);
    open_page(&mut h, "lab");
    h.get_by_label("Reading the snapshots\u{2026}");
    hs.update(|s| {
        s.snapshots.insert(
            vm_id(1),
            lightvm_desktop::service::SnapshotList {
                loaded: true,
                tree: Some(common::snapshot_tree(1, None, vec![])),
                error: None,
            },
        );
    });
    h.run_steps(4);
    h.get_by_label("No snapshots yet.");
    // A read error is shown.
    hs.update(|s| {
        s.snapshots.get_mut(&vm_id(1)).unwrap().error =
            Some(Failure::new("snapshots: connection closed"));
    });
    h.run_steps(4);
    h.get_by_label("The snapshots could not be read: snapshots: connection closed");
}

#[test]
fn missing_capability_disables_the_page() {
    let mut s = state();
    s.host.as_mut().unwrap().baseline_available = false;
    let (mut h, _hs) = harness(s);
    h.run_steps(4);
    click(&mut h, "More actions for lab");
    assert!(!enabled(&h, "Snapshots of lab"));
}

/// Graphical snapshots (F1-19, ADR-0022 criterion 4): the page and the
/// confirmation of a take.
mod images {
    use super::*;

    /// One wgpu device at a time (see visual_snapshots.rs).
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn img_harness(state: ServiceState) -> Harness<'static, LightVmApp> {
        let (fake, _hs) = FakeSupervisor::new(state);
        let mut app = LightVmApp::new(false);
        app.set_supervisor(fake);
        app.set_fixed_time(Some(now()));
        let mut h = Harness::builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
        h.run();
        // No blinking text cursor in the images.
        h.ctx
            .all_styles_mut(|s| s.visuals.text_cursor.blink = false);
        h.run();
        h
    }

    #[test]
    fn snapshot_snapshots_page() {
        let _serial = serial();
        let mut h = img_harness(state());
        open_page(&mut h, "lab");
        h.snapshot("snapshots_page");
    }

    #[test]
    fn snapshot_take_dialog() {
        let _serial = serial();
        let mut h = img_harness(state());
        open_page(&mut h, "lab");
        click(&mut h, "Take a snapshot of lab");
        h.run();
        h.snapshot("snapshots_take_dialog");
    }
}
