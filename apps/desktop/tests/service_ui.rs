//! Supervisor panel (F1-11) driven headless with `egui_kittest`: state shown,
//! buttons enabled only when the action is allowed, command returned on
//! click. No process: the state is built in the test.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::{ServiceCommand, ServiceState};
use lightvm_desktop::service_ui;
use lightvm_proto::ipc::VmSummary;
use lightvm_proto::{VmId, VmName, VmState};

struct Fixture {
    state: ServiceState,
    alive: bool,
    choices: Vec<ServiceCommand>,
}

fn vm(name: &str, state: VmState) -> VmSummary {
    VmSummary {
        vm_id: VmId::new_v7(),
        name: VmName::parse(name).unwrap(),
        state,
        run_id: None,
        config: lightvm_proto::VmConfig {
            name: VmName::parse(name).unwrap(),
            guest_os: lightvm_proto::GuestOs::UbuntuLtsX86_64,
            vcpus: 2,
            memory_mib: 2048,
            disk_gib: 16,
            network: lightvm_proto::NetworkProfile::Offline,
            install_media: None,
            data_media: None,
            firmware: lightvm_proto::Firmware::Uefi,
            limits: Default::default(),
        },
        config_revision: 1,
        disk_used_bytes: None,
    }
}

fn connected(vms: Vec<VmSummary>) -> ServiceState {
    ServiceState {
        connection: "connected".into(),
        connected: true,
        backend: Some("qemu".into()),
        accelerator: Some("whpx".into()),
        vms,
        last_outcome: None,
        policy: Some(lightvm_proto::ipc::ParentLossPolicy::KillVms),
    }
}

fn harness(state: ServiceState, alive: bool) -> Harness<'static, Fixture> {
    Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .build_ui_state(
            |ui, p: &mut Fixture| {
                if let Some(c) = service_ui(ui, &p.state, 4242, p.alive) {
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

fn enabled(h: &Harness<'_, Fixture>, label: &str) -> Vec<bool> {
    h.get_all_by_role_and_label(Role::Button, label)
        .map(|n| !n.accesskit_node().is_disabled())
        .collect()
}

#[test]
fn state_and_accelerator_are_shown_as_text() {
    let mut h = harness(connected(vec![]), true);
    h.run();
    h.get_by_label("Supervisor · PID 4242");
    h.get_by_label("connected");
    h.get_by_label("qemu");
    h.get_by_label("whpx");
    h.get_by_label("terminate the VMs immediately");
    h.get_by_label("No VM in the supervisor's catalog.");
}

#[test]
fn start_only_if_stopped_stop_only_if_running() {
    let stopped = vm("stopped", VmState::Stopped);
    let running = vm("running", VmState::Running);
    let starting = vm("starting", VmState::Starting);
    let mut h = harness(
        connected(vec![stopped.clone(), running.clone(), starting]),
        true,
    );
    h.run();
    // A single main action per row: Start if stopped, Stop otherwise,
    // enabled only when allowed.
    assert_eq!(enabled(&h, "Start"), [true]);
    assert_eq!(enabled(&h, "Stop"), [true, false]);
    h.get_by_label("Stopped");
    h.get_by_label("Running");
    h.get_by_label("Starting");

    h.get_by_role_and_label(Role::Button, "Start").click();
    h.run();
    h.get_all_by_role_and_label(Role::Button, "Stop")
        .next()
        .unwrap()
        .click();
    h.run();
    assert_eq!(
        h.state().choices,
        [
            ServiceCommand::Start(stopped.vm_id),
            ServiceCommand::Stop(running.vm_id)
        ]
    );
}

#[test]
fn emergency_stop_returns_the_command() {
    let mut h = harness(connected(vec![vm("a", VmState::Running)]), true);
    h.run();
    h.get_by_role_and_label(Role::Button, "Emergency Stop")
        .click();
    h.run();
    assert_eq!(h.state().choices, [ServiceCommand::EmergencyStop]);
}

#[test]
fn no_action_with_service_terminated_or_not_connected() {
    for (alive, connected_) in [(false, true), (true, false)] {
        let mut state = connected(vec![
            vm("stopped", VmState::Stopped),
            vm("running", VmState::Running),
        ]);
        state.connected = connected_;
        let mut h = harness(state, alive);
        h.run();
        for label in ["Refresh", "Emergency Stop", "Start", "Stop", "Edit"] {
            assert!(
                enabled(&h, label).iter().all(|a| !a),
                "{label} enabled with alive={alive} connected={connected_}"
            );
        }
    }
    let mut h = harness(connected(vec![]), false);
    h.run();
    h.get_by_label("terminated");
}
