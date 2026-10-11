//! Baselines page (F3): list, creation (with the explicit consent after an
//! unclean stop), restore, deletion, long operations and the refusals of
//! the supervisor in plain language. Whole app with a stand-in supervisor.

mod common;

use common::{FakeSupervisor, baseline, baseline_id, connected, vm, vm_id};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::LightVmApp;
use lightvm_desktop::service::{
    BaselineList, BaselineOp, BaselineRequest, Failure, RequestStatus, ServiceCommand, ServiceState,
};
use lightvm_proto::ipc::StorageOperation;
use lightvm_proto::{ErrorCode, Label, UtcTimestamp, VmState};

/// Fixed instant of the generated labels: 2026-10-11 09:12 UTC.
fn now() -> UtcTimestamp {
    UtcTimestamp::from_unix_millis(1_791_709_920_000).unwrap()
}

const LABEL_3: &str = "Baseline 3 \u{b7} 2026-10-11 09:12 UTC";

/// A stopped VM that ran (it has a disk), with two baselines, the VM's
/// current state an overlay of the first one.
fn state() -> ServiceState {
    let mut v = vm(1, "lab", VmState::Stopped);
    v.run_id = Some(common::run_id(1));
    v.baselines.current = Some(baseline_id(1));
    v.baselines.count = 2;
    v.baselines.bytes = 7 << 30;
    v.baselines.overlay_bytes = 300 << 20;
    let mut s = connected(vec![v]);
    if let Some(h) = &mut s.host {
        h.baseline_available = true;
    }
    s.baselines.insert(
        vm_id(1),
        BaselineList {
            loaded: true,
            items: vec![
                {
                    let mut b = baseline(1, 1, "Clean install");
                    b.current = true;
                    b.in_use = true;
                    b
                },
                {
                    let mut b = baseline(2, 1, "Before the update");
                    b.clean_shutdown = false;
                    b.derived_from = Some(baseline_id(1));
                    b
                },
            ],
            error: None,
        },
    );
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

/// Dashboard → More → Baselines…
fn open_page(h: &mut Harness<'_, LightVmApp>, name: &str) {
    h.run_steps(4);
    click(h, &format!("More actions for {name}"));
    click(h, &format!("Baselines of {name}"));
}

fn set_request(
    hs: &common::Handles,
    op: BaselineOp,
    status: RequestStatus<lightvm_proto::BaselineId>,
) {
    hs.update(|s| {
        s.baseline = Some(BaselineRequest {
            vm_id: vm_id(1),
            op,
            status,
        });
    });
}

#[test]
fn page_lists_the_baselines_and_the_current_state() {
    let (mut h, hs) = harness(state());
    h.run_steps(4);
    // Dashboard row: current state and baselines in one line.
    h.get_by_label_contains("On baseline \u{201c}Clean install\u{201d}");
    h.get_by_label_contains("300 MiB of changes since the restore");
    h.get_by_label_contains("2 baselines (7.0 GiB)");
    open_page(&mut h, "lab");
    assert_eq!(h.state().baselines_vm(), Some(vm_id(1)));
    assert_eq!(hs.actions(), [ServiceCommand::ListBaselines(vm_id(1))]);
    h.get_by_label("Baselines \u{b7} lab");
    h.get_by_label("Running on baseline \u{201c}Clean install\u{201d}");
    h.get_by_label("300 MiB");
    // Both baselines, with their badges and details.
    h.get_by_label("Clean install");
    h.get_by_label("Before the update");
    h.get_by_label("Current");
    h.get_by_label("Verified");
    h.get_by_label("Not verified");
    h.get_by_label_contains("taken on \u{201c}Clean install\u{201d}");
    assert_eq!(
        h.query_all_by_label_contains("3.0 GiB on the host").count(),
        2
    );
    // No text field anywhere: the label is generated.
    assert_eq!(common::text_fields(&h), 0);
    // The current baseline cannot be deleted; the other one can.
    assert!(!enabled(&h, "Delete baseline Clean install"));
    assert!(enabled(&h, "Delete baseline Before the update"));
    assert!(enabled(&h, "Restore baseline Clean install"));
    assert!(enabled(&h, "Create a baseline of lab"));
    // Back to the Dashboard.
    click(&mut h, "Back to the Dashboard");
    assert_eq!(h.state().baselines_vm(), None);
    h.get_by_label("Virtual machines");
}

#[test]
fn creation_is_confirmed_and_shown_as_a_long_operation() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Create a baseline of lab");
    assert!(h.state().dialog_open());
    h.get_by_label("Create a baseline of \u{201c}lab\u{201d}?");
    h.get_by_label_contains(&format!("read-only baseline \u{201c}{LABEL_3}\u{201d}"));
    h.get_by_label_contains("only the Emergency Stop cancels the operation");
    assert_eq!(common::text_fields(&h), 0);
    // Esc cancels without effect.
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    assert!(hs.actions().is_empty());

    click(&mut h, "Create a baseline of lab");
    click(&mut h, "Create baseline");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::CreateBaseline {
            vm_id: vm_id(1),
            label: Label::parse(LABEL_3).unwrap(),
            allow_unclean: false,
        }]
    );
    h.get_by_label("Waiting for the supervisor…");
    // The supervisor reports the operation running: the dialog closes, the
    // page shows the progress and every change of the VM is disabled.
    hs.update(|s| s.vms[0].baselines.operation = Some(StorageOperation::CreateBaseline));
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Creating the baseline…");
    h.get_by_label_contains("only the Emergency Stop cancels the operation");
    h.get_by_label("Creating baseline");
    for b in [
        "Create a baseline of lab",
        "Restore baseline Clean install",
        "Delete baseline Before the update",
    ] {
        assert!(!enabled(&h, b), "{b}");
    }
    // The Emergency Stop stays reachable on the page.
    hs.clear();
    click(&mut h, "Emergency Stop");
    assert_eq!(hs.actions(), [ServiceCommand::EmergencyStop]);
    // Dashboard: the VM cannot be started meanwhile.
    click(&mut h, "Back to the Dashboard");
    assert!(!enabled(&h, "Start lab"));
    h.get_by_label("Creating baseline");

    // Done: the new baseline in the list, outcome stated.
    let mut new = baseline(3, 1, LABEL_3);
    new.created_at = now();
    hs.update(|s| {
        s.vms[0].baselines.operation = None;
        s.vms[0].baselines.count = 3;
        s.baselines.get_mut(&vm_id(1)).unwrap().items.push(new);
    });
    set_request(
        &hs,
        BaselineOp::Create {
            label: Label::parse(LABEL_3).unwrap(),
            allow_unclean: false,
        },
        RequestStatus::Done(baseline_id(3)),
    );
    h.run_steps(4);
    assert!(enabled(&h, "Start lab"));
    open_page(&mut h, "lab");
    h.get_by_label(&format!("Baseline \u{201c}{LABEL_3}\u{201d} created."));
    assert!(enabled(&h, "Create a baseline of lab"));
}

#[test]
fn unclean_stop_requires_explicit_consent() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Create a baseline of lab");
    click(&mut h, "Create baseline");
    // Refused: the last run did not end with a clean guest shutdown.
    set_request(
        &hs,
        BaselineOp::Create {
            label: Label::parse(LABEL_3).unwrap(),
            allow_unclean: false,
        },
        RequestStatus::Failed(
            Failure::coded(
                ErrorCode::StateConflict,
                "baseline: state_conflict: unclean",
            )
            .with_detail("reason", "unclean_shutdown"),
        ),
    );
    h.run_steps(4);
    assert!(h.state().dialog_open());
    h.get_by_label("\u{201c}lab\u{201d} was not shut down cleanly");
    h.get_by_label_contains("stays marked Not verified");
    // Without the consent the action is disabled.
    assert!(!enabled(&h, "Create unverified baseline"));
    h.get_by_role_and_label(
        Role::CheckBox,
        "I understand: the baseline is marked Not verified",
    )
    .click();
    h.run_steps(4);
    assert!(enabled(&h, "Create unverified baseline"));
    click(&mut h, "Create unverified baseline");
    assert_eq!(
        hs.actions().last(),
        Some(&ServiceCommand::CreateBaseline {
            vm_id: vm_id(1),
            label: Label::parse(LABEL_3).unwrap(),
            allow_unclean: true,
        })
    );
}

#[test]
fn restore_explains_the_loss_and_reports_integrity_errors() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Restore baseline Before the update");
    h.get_by_label("Restore \u{201c}Before the update\u{201d}?");
    h.get_by_label_contains(
        "everything written since the restore of \u{201c}Clean install\u{201d} and its UEFI variables are lost",
    );
    h.get_by_label_contains("normal boot");
    // Keyboard focus on Cancel.
    assert!(button(&h, "Cancel").accesskit_node().is_focused());
    click(&mut h, "Discard and restore");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::RestoreBaseline {
            vm_id: vm_id(1),
            baseline_id: baseline_id(2),
        }]
    );
    // The baseline's files were altered: refused, stated in plain words.
    set_request(
        &hs,
        BaselineOp::Restore(baseline_id(2)),
        RequestStatus::Failed(
            Failure::coded(
                ErrorCode::StateConflict,
                "restore: state_conflict: hash mismatch",
            )
            .with_detail("reason", "integrity"),
        ),
    );
    h.run_steps(4);
    let text = "The baseline was not restored: its files were changed or are missing on the host, so LightVM will not use it. Restore another baseline, or delete this one.";
    assert_eq!(h.query_all_by_label(text).count(), 2, "dialog and page");
    click(&mut h, "Cancel");
    assert!(!h.state().dialog_open());
    h.get_by_label(text);
}

#[test]
fn restore_from_the_standalone_disk_warns_about_the_whole_disk() {
    let mut s = state();
    s.vms[0].baselines.current = None;
    for b in &mut s.baselines.get_mut(&vm_id(1)).unwrap().items {
        b.current = false;
        b.in_use = false;
    }
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    h.get_by_label("Standalone disk");
    click(&mut h, "Restore baseline Clean install");
    h.get_by_label_contains("the VM's disk as it is now and its UEFI variables are lost");
    h.get_by_label_contains("To keep the current state, create a baseline of it first.");
}

#[test]
fn deletion_waits_for_the_outcome_and_explains_refusals() {
    let (mut h, hs) = harness(state());
    open_page(&mut h, "lab");
    hs.clear();
    click(&mut h, "Delete baseline Before the update");
    h.get_by_label("Delete baseline \u{201c}Before the update\u{201d}?");
    h.get_by_label_contains("(3.0 GiB) is removed from the host permanently");
    click(&mut h, "Delete baseline");
    assert_eq!(
        hs.actions(),
        [ServiceCommand::DeleteBaseline {
            vm_id: vm_id(1),
            baseline_id: baseline_id(2),
        }]
    );
    h.get_by_label("Waiting for the supervisor…");
    assert!(!enabled(&h, "Delete baseline"));
    // Refused: in use (the state changed meanwhile).
    set_request(
        &hs,
        BaselineOp::Delete(baseline_id(2)),
        RequestStatus::Failed(
            Failure::coded(ErrorCode::StateConflict, "deletion: state_conflict: in use")
                .with_detail("reason", "baseline_referenced"),
        ),
    );
    h.run_steps(4);
    assert!(h.state().dialog_open());
    assert_eq!(
        h.query_all_by_label_contains("it is in use, the VM's current state is built on it")
            .count(),
        2
    );
    // Protected.
    set_request(
        &hs,
        BaselineOp::Delete(baseline_id(2)),
        RequestStatus::Failed(
            Failure::coded(ErrorCode::StateConflict, "x")
                .with_detail("reason", "baseline_protected"),
        ),
    );
    h.run_steps(4);
    assert_eq!(
        h.query_all_by_label_contains("it is protected (it belongs to an agent session)")
            .count(),
        2
    );
    // Retried and done: the dialog closes, the baseline is gone.
    click(&mut h, "Delete baseline");
    hs.update(|s| {
        s.baselines.get_mut(&vm_id(1)).unwrap().items.truncate(1);
        s.vms[0].baselines.count = 1;
    });
    set_request(
        &hs,
        BaselineOp::Delete(baseline_id(2)),
        RequestStatus::Done(baseline_id(2)),
    );
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    h.get_by_label("Baseline deleted.");
    assert!(h.query_by_label("Before the update").is_none());
}

#[test]
fn protected_baselines_cannot_be_deleted() {
    let mut s = state();
    s.baselines.get_mut(&vm_id(1)).unwrap().items[1].protected = true;
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    h.get_by_label("Protected");
    assert!(!enabled(&h, "Delete baseline Before the update"));
    assert!(enabled(&h, "Restore baseline Before the update"));
}

#[test]
fn supervisor_refusals_are_explained() {
    let cases: [(Failure, &str); 6] = [
        (
            Failure::coded(ErrorCode::VmBusy, "baseline: vm_busy: busy"),
            "the VM is busy (a start, a stop or another baseline operation is in progress, or one of its disks is open in another program)",
        ),
        (
            Failure::coded(ErrorCode::ResourceLimit, "baseline: resource_limit: space")
                .with_detail("needed_bytes", (6u64 << 30).to_string())
                .with_detail("free_bytes", (2u64 << 30).to_string()),
            "not enough free space on the catalog drive. It needs about 6.0 GiB, 2.0 GiB is free and LightVM keeps 1 GiB in reserve.",
        ),
        (
            Failure::coded(ErrorCode::ResourceLimit, "baseline: resource_limit: budget")
                .with_detail("needed_bytes", (6u64 << 30).to_string())
                .with_detail("budget_gib", "4"),
            "it would exceed the VM's storage budget of 4 GiB (it needs about 6.0 GiB)",
        ),
        (
            Failure::coded(
                ErrorCode::StateConflict,
                "baseline: state_conflict: operation cancelled by the Emergency Stop",
            ),
            "cancelled by the Emergency Stop. Nothing was changed, the VM is as it was.",
        ),
        (
            Failure::coded(
                ErrorCode::Unsupported,
                "baseline: unsupported: no image tool",
            ),
            "this supervisor cannot handle baselines",
        ),
        (
            Failure::coded(
                ErrorCode::StateConflict,
                "baseline: state_conflict: the VM has no disk yet: start it once before taking a baseline",
            ),
            "the VM has no disk yet. Start it once and install the system",
        ),
    ];
    for (f, expected) in cases {
        let mut s = state();
        s.baseline = Some(BaselineRequest {
            vm_id: vm_id(1),
            op: BaselineOp::Create {
                label: Label::parse(LABEL_3).unwrap(),
                allow_unclean: false,
            },
            status: RequestStatus::Failed(f),
        });
        let (mut h, _hs) = harness(s);
        open_page(&mut h, "lab");
        h.get_by_label_contains(expected);
    }
}

#[test]
fn running_vm_and_missing_capability_disable_the_changes() {
    let mut s = state();
    s.vms[0].state = VmState::Running;
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    h.get_by_label("Stop the VM first: baselines are created and restored with the VM off.");
    for b in [
        "Create a baseline of lab",
        "Restore baseline Before the update",
        "Delete baseline Before the update",
    ] {
        assert!(!enabled(&h, b), "{b}");
    }

    // The run is not known after a stop: the creation stays possible (a VM
    // that never ran is refused by the supervisor, explained in plain words).
    let mut s = state();
    s.vms[0].run_id = None;
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    assert!(enabled(&h, "Create a baseline of lab"));

    // Supervisor without the image tool: the page is not offered.
    let mut s = state();
    s.host.as_mut().unwrap().baseline_available = false;
    let (mut h, _hs) = harness(s);
    h.run_steps(4);
    click(&mut h, "More actions for lab");
    assert!(!enabled(&h, "Baselines of lab"));
}

#[test]
fn empty_and_unread_lists() {
    let mut s = state();
    s.vms[0].baselines = Default::default();
    s.baselines.clear();
    let (mut h, hs) = harness(s);
    open_page(&mut h, "lab");
    h.get_by_label("Reading the baselines…");
    hs.update(|s| {
        s.baselines.insert(
            vm_id(1),
            BaselineList {
                loaded: true,
                ..Default::default()
            },
        );
    });
    h.run_steps(4);
    h.get_by_label("No baselines yet.");
    h.get_by_label("Standalone disk");
    // A read error is shown.
    hs.update(|s| {
        s.baselines.get_mut(&vm_id(1)).unwrap().error =
            Some(Failure::new("baselines: connection closed"));
    });
    h.run_steps(4);
    h.get_by_label("The list could not be read: baselines: connection closed");
}

/// Another VM's baseline operation is waiting: one at a time.
#[test]
fn one_baseline_operation_at_a_time() {
    let mut s = state();
    let mut other = vm(2, "other", VmState::Stopped);
    other.run_id = Some(common::run_id(2));
    s.vms.push(other);
    s.baseline = Some(BaselineRequest {
        vm_id: vm_id(2),
        op: BaselineOp::Delete(baseline_id(9)),
        status: RequestStatus::Pending,
    });
    let (mut h, _hs) = harness(s);
    open_page(&mut h, "lab");
    assert!(!enabled(&h, "Create a baseline of lab"));
    assert!(!enabled(&h, "Restore baseline Before the update"));
}
