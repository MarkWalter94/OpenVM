//! Dashboard (F2) driven headless with `egui_kittest`: state shown as text,
//! actions enabled only when the state allows them, action returned on
//! click. No process: the state is built in the test.

mod common;

use common::{connected, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::dashboard::{Confirm, DashboardAction};
use lightvm_desktop::dashboard_ui;
use lightvm_desktop::service::{ServiceCommand, ServiceState};
use lightvm_proto::VmState;

struct Fixture {
    state: ServiceState,
    alive: bool,
    choices: Vec<DashboardAction>,
}

fn harness(state: ServiceState, alive: bool) -> Harness<'static, Fixture> {
    Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_ui_state(
            |ui, p: &mut Fixture| {
                if let Some(c) = dashboard_ui(ui, &p.state, 4242, p.alive) {
                    p.choices.push(c);
                }
            },
            Fixture {
                state,
                alive,
                choices: Vec::new(),
            },
        )
}

fn enabled(h: &Harness<'_, Fixture>, label: &str) -> bool {
    !h.get_by_role_and_label(Role::Button, label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn state_resources_and_disk_are_shown_as_text() {
    let mut h = harness(connected(vec![vm(1, "ubuntu-dev", VmState::Running)]), true);
    h.run();
    h.get_by_label("Supervisor · PID 4242");
    h.get_by_label("connected");
    h.get_by_label("whpx");
    h.get_by_label("terminate the VMs immediately");
    h.get_by_label("ubuntu-dev");
    h.get_by_label("Running");
    h.get_by_label("Ubuntu LTS · 2 vCPU · 4 GiB RAM · disk 32 GiB, 5.2 GiB used");
}

#[test]
fn empty_catalog_points_to_new_vm() {
    let mut h = harness(connected(vec![]), true);
    h.run();
    h.get_by_label("No virtual machines yet.");
    assert!(enabled(&h, "New VM"));
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run();
    assert_eq!(h.state().choices, [DashboardAction::NewVm]);
}

/// Matrix of the actions enabled by state.
#[test]
fn actions_enabled_by_state() {
    let vms = vec![
        vm(1, "stopped", VmState::Stopped),
        vm(2, "running", VmState::Running),
        vm(3, "starting", VmState::Starting),
        vm(4, "failed", VmState::Failed),
    ];
    let mut h = harness(connected(vms), true);
    h.run();
    assert!(enabled(&h, "Start stopped"));
    assert!(!enabled(&h, "Start failed"));
    assert!(enabled(&h, "Stop running"));
    assert!(!enabled(&h, "Stop starting"));
    assert!(enabled(&h, "Open the console of running"));
    for name in ["stopped", "starting", "failed"] {
        assert!(
            !enabled(&h, &format!("Open the console of {name}")),
            "{name}"
        );
    }
    for name in ["stopped", "running", "starting", "failed"] {
        assert!(enabled(&h, &format!("Edit {name}")));
        assert!(enabled(&h, &format!("More actions for {name}")));
    }
    // "More" of the running VM: forced stop and reset yes, deletion no.
    let more = |h: &mut Harness<'_, Fixture>, name: &str| {
        h.get_by_role_and_label(Role::Button, &format!("More actions for {name}"))
            .click();
        h.run();
    };
    more(&mut h, "running");
    assert!(enabled(&h, "Force stop…"));
    assert!(enabled(&h, "Reset…"));
    assert!(!enabled(&h, "Delete…"));
    h.get_by_role_and_label(Role::Button, "Reset…").click();
    h.run();
    more(&mut h, "stopped");
    assert!(!enabled(&h, "Force stop…"));
    assert!(!enabled(&h, "Reset…"));
    assert!(enabled(&h, "Delete…"));
    h.get_by_role_and_label(Role::Button, "Delete…").click();
    h.run();
    more(&mut h, "starting");
    assert!(enabled(&h, "Force stop…"));
    assert!(!enabled(&h, "Reset…"));
    assert!(!enabled(&h, "Delete…"));

    h.key_press(egui::Key::Escape);
    h.run();
    h.get_by_role_and_label(Role::Button, "Start stopped")
        .click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Stop running")
        .click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Open the console of running")
        .click();
    h.run();
    let s = h.state();
    let id = |n: u8| common::vm_id(n);
    assert_eq!(
        s.choices,
        [
            DashboardAction::Confirm(Confirm::Reset(id(2))),
            DashboardAction::Confirm(Confirm::Delete(id(1))),
            DashboardAction::Command(ServiceCommand::Start(id(1))),
            DashboardAction::Command(ServiceCommand::Stop(id(2))),
            DashboardAction::Console(id(2)),
        ]
    );
}

#[test]
fn emergency_stop_returns_the_command() {
    let mut h = harness(connected(vec![vm(1, "a", VmState::Running)]), true);
    h.run();
    h.get_by_role_and_label(Role::Button, "Emergency Stop")
        .click();
    h.run();
    assert_eq!(
        h.state().choices,
        [DashboardAction::Command(ServiceCommand::EmergencyStop)]
    );
}

#[test]
fn no_action_with_service_terminated_or_not_connected() {
    for (alive, connected_) in [(false, true), (true, false)] {
        let mut state = connected(vec![
            vm(1, "stopped", VmState::Stopped),
            vm(2, "running", VmState::Running),
        ]);
        state.connected = connected_;
        let mut h = harness(state, alive);
        h.run();
        for label in [
            "Refresh",
            "Emergency Stop",
            "New VM",
            "Start stopped",
            "Stop running",
            "Open the console of running",
            "Edit stopped",
            "More actions for running",
        ] {
            assert!(
                !enabled(&h, label),
                "{label} enabled with alive={alive} connected={connected_}"
            );
        }
    }
    let mut h = harness(connected(vec![]), false);
    h.run();
    h.get_by_label("terminated");
}

/// A VM stopped after a run with the installation media still attached
/// shows the hint to remove it; the button asks for a confirmation.
#[test]
fn install_media_hint_after_first_run() {
    // As the real service reports it: no run id for a stopped VM, the
    // installed system shows in the disk usage.
    let mut installed = vm(1, "installed", VmState::Stopped);
    installed.run_id = None;
    installed.disk_used_bytes = Some(6_900_000_000);
    installed.config.install_media = Some(common::media_id(1));
    let mut fresh = vm(2, "fresh", VmState::Stopped);
    fresh.disk_used_bytes = Some(3 << 20);
    fresh.config.install_media = Some(common::media_id(1));
    let mut h = harness(connected(vec![installed, fresh]), true);
    h.run();
    assert_eq!(
        h.query_all_by_label(
            "Installation finished? Remove the installation media so the VM boots from its disk."
        )
        .count(),
        1,
        "only the VM whose disk holds a system shows the hint"
    );
    h.get_by_role_and_label(Role::Button, "Remove the installation media of installed")
        .click();
    h.run();
    assert_eq!(
        h.state().choices,
        [DashboardAction::Confirm(Confirm::DetachInstall(
            common::vm_id(1)
        ))]
    );
}
