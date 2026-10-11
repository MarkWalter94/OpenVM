//! Dialogs (F2): deletion, reset and forced stop confirmations, removal of
//! the installation media, close of the app with running VMs. Whole app
//! with a stand-in supervisor.

mod common;

use common::{FakeSupervisor, connected, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::LightVmApp;
use lightvm_desktop::service::{Failure, RequestStatus, ServiceCommand, ServiceState};
use lightvm_proto::VmState;

fn harness(state: ServiceState) -> (Harness<'static, LightVmApp>, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(state);
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    let h = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    (h, hs)
}

fn more(h: &mut Harness<'_, LightVmApp>, name: &str, item: &str) {
    h.get_by_role_and_label(Role::Button, &format!("More actions for {name}"))
        .click();
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, item).click();
    h.run_steps(4);
}

fn enabled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    !h.get_by_role_and_label(Role::Button, label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn delete_names_the_vm_and_waits_for_the_outcome() {
    let (mut h, hs) = harness(connected(vec![vm(1, "old-lab", VmState::Stopped)]));
    h.run_steps(4);
    more(&mut h, "old-lab", "Delete…");
    assert!(h.state().dialog_open());
    h.get_by_label("Delete “old-lab”?");
    h.get_by_label_contains("its virtual disk and its run files are removed permanently");
    // Keyboard focus on Cancel; Esc closes without effect.
    assert!(
        h.get_by_role_and_label(Role::Button, "Cancel")
            .accesskit_node()
            .is_focused()
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert!(!h.state().dialog_open());
    assert!(hs.actions().is_empty());

    more(&mut h, "old-lab", "Delete…");
    h.get_by_role_and_label(Role::Button, "Delete VM").click();
    h.run_steps(4);
    assert_eq!(
        hs.actions(),
        [ServiceCommand::Delete {
            vm_id: common::vm_id(1),
            revision: 3
        }]
    );
    h.get_by_label("Waiting for the supervisor…");
    assert!(!enabled(&h, "Delete VM"));
    // Failure (supervisor without the operation): stated, dialog open.
    hs.update(|s| {
        s.delete = RequestStatus::Failed(Failure {
            unsupported: true,
            message: "deletion: unsupported".into(),
            ..Failure::default()
        });
    });
    h.run_steps(4);
    h.get_by_label("This supervisor cannot delete VMs yet.");
    h.get_by_role_and_label(Role::Button, "Delete VM").click();
    h.run_steps(4);
    hs.update(|s| {
        s.delete = RequestStatus::Done(common::vm_id(1));
        s.vms.clear();
    });
    h.run_steps(4);
    assert!(!h.state().dialog_open(), "closed after the deletion");
}

#[test]
fn reset_and_force_stop_ask_and_carry_the_run() {
    let (mut h, hs) = harness(connected(vec![vm(2, "busy", VmState::Running)]));
    h.run_steps(4);
    more(&mut h, "busy", "Reset…");
    h.get_by_label("Reset “busy”?");
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.run_steps(4);
    assert!(hs.actions().is_empty());
    more(&mut h, "busy", "Reset…");
    h.get_by_role_and_label(Role::Button, "Reset").click();
    h.run_steps(4);
    more(&mut h, "busy", "Force stop…");
    h.get_by_label("Force stop “busy”?");
    h.get_by_role_and_label(Role::Button, "Force stop").click();
    h.run_steps(4);
    assert_eq!(
        hs.actions(),
        [
            ServiceCommand::Reset {
                vm_id: common::vm_id(2),
                run_id: common::run_id(2)
            },
            ServiceCommand::ForceStop(common::vm_id(2)),
        ]
    );
    assert!(!h.state().dialog_open());
}

/// The VM state changes while the dialog is open: the action is refused.
#[test]
fn stale_confirmation_is_refused() {
    let (mut h, hs) = harness(connected(vec![vm(1, "x", VmState::Stopped)]));
    h.run_steps(4);
    more(&mut h, "x", "Delete…");
    hs.update(|s| {
        s.vms[0].state = VmState::Starting;
        s.vms[0].run_id = Some(common::run_id(1));
    });
    h.run_steps(4);
    h.get_by_label("The VM state changed: the action is no longer possible.");
    h.get_by_role_and_label(Role::Button, "Delete VM").click();
    h.run_steps(4);
    assert!(hs.actions().is_empty());
}

#[test]
fn installation_media_removal_after_confirmation() {
    let mut v = vm(1, "installed", VmState::Stopped);
    v.run_id = Some(common::run_id(1));
    v.config.install_media = Some(common::media_id(1));
    let expected = {
        let mut c = v.config.clone();
        c.install_media = None;
        c
    };
    let (mut h, hs) = harness(connected(vec![v]));
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "Remove the installation media of installed")
        .click();
    h.run_steps(4);
    h.get_by_label("Remove the installation media from “installed”?");
    h.get_by_role_and_label(Role::Button, "Remove media")
        .click();
    h.run_steps(4);
    assert_eq!(
        hs.actions(),
        [ServiceCommand::UpdateVm {
            vm_id: common::vm_id(1),
            revision: 3,
            config: Box::new(expected),
        }]
    );
}

fn request_close(h: &mut Harness<'_, LightVmApp>) {
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    h.step();
    h.run_steps(4);
}

fn close_cancelled(h: &Harness<'_, LightVmApp>) -> bool {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|o| o.commands.contains(&egui::ViewportCommand::CancelClose))
}

#[test]
fn close_without_running_vms_is_immediate() {
    let (mut h, _hs) = harness(connected(vec![vm(1, "idle", VmState::Stopped)]));
    h.run_steps(4);
    request_close(&mut h);
    assert!(h.state().close_allowed());
    assert!(!h.state().closing());
}

#[test]
fn close_with_running_vms_asks_then_stops_them() {
    let (mut h, hs) = harness(connected(vec![
        vm(1, "web", VmState::Running),
        vm(2, "db", VmState::Running),
        vm(3, "off", VmState::Stopped),
    ]));
    h.run_steps(4);
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    h.step();
    assert!(close_cancelled(&h), "close cancelled while VMs run");
    h.run_steps(4);
    assert!(h.state().closing());
    assert!(!h.state().close_allowed());
    h.get_by_label("2 VMs are running");
    h.get_by_label_contains("web, db.");
    // Cancel: nothing stopped, the app stays open.
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.run_steps(4);
    assert!(!h.state().closing());
    assert!(hs.actions().is_empty());

    request_close(&mut h);
    h.get_by_role_and_label(Role::Button, "Stop them and quit")
        .click();
    h.run_steps(4);
    assert_eq!(
        hs.actions(),
        [
            ServiceCommand::Stop(common::vm_id(1)),
            ServiceCommand::Stop(common::vm_id(2))
        ]
    );
    h.get_by_label("Stopping the VMs…");
    // Only the VMs running at the confirmation are counted.
    h.get_by_label_contains("0 of 2 stopped");
    assert!(h.query_by_label("off: Stopped").is_none());
    hs.update(|s| s.vms[0].state = VmState::Stopping);
    h.run_steps(4);
    h.get_by_label("web: Stopping");
    hs.update(|s| {
        for v in &mut s.vms {
            v.state = VmState::Stopped;
        }
    });
    // Supervisor shut down in the background, then the window closes.
    let t0 = std::time::Instant::now();
    while !h.state().close_allowed() {
        assert!(t0.elapsed().as_secs() < 10, "close not completed");
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.step();
    }
    assert!(
        *hs.stopped.lock().unwrap(),
        "supervisor stopped before closing"
    );
    let closed = h
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|o| o.commands.contains(&egui::ViewportCommand::Close));
    assert!(closed, "close command sent");
}

/// Waits (stepping the UI) until the app agreed to close and sent `Close`.
fn wait_closed(h: &mut Harness<'_, LightVmApp>, within: std::time::Duration) {
    let t0 = std::time::Instant::now();
    while !h.state().close_allowed() {
        assert!(t0.elapsed() < within, "close not completed");
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.step();
    }
}

/// Finding 2: VMs listed as running by a supervisor that is terminated or
/// disconnected do not hold the close (no 90 s wait).
#[test]
fn close_with_a_dead_or_disconnected_supervisor_is_immediate() {
    let (mut h, hs) = harness(connected(vec![vm(1, "web", VmState::Running)]));
    h.run_steps(4);
    *hs.stopped.lock().unwrap() = true;
    request_close(&mut h);
    assert!(
        h.state().close_allowed(),
        "dead supervisor: immediate close"
    );
    assert!(!h.state().closing());

    let (mut h, hs) = harness(connected(vec![vm(1, "web", VmState::Running)]));
    h.run_steps(4);
    hs.update(|s| {
        s.connected = false;
        s.connection = "event channel closed".into();
    });
    request_close(&mut h);
    assert!(h.state().close_allowed(), "disconnected: immediate close");
}

/// Finding 2: the supervisor dies while the VMs are being stopped: the
/// close completes without waiting for the timeout.
#[test]
fn supervisor_lost_while_stopping_finishes_the_close() {
    let (mut h, hs) = harness(connected(vec![vm(1, "web", VmState::Running)]));
    h.run_steps(4);
    request_close(&mut h);
    h.get_by_role_and_label(Role::Button, "Stop them and quit")
        .click();
    h.run_steps(4);
    h.get_by_label("Stopping the VMs…");
    *hs.stopped.lock().unwrap() = true;
    wait_closed(&mut h, std::time::Duration::from_secs(5));
}

/// Finding 12: the running VMs stop while the question is shown: the
/// close requested by the user goes on instead of being dropped.
#[test]
fn vms_stopped_while_asking_let_the_close_proceed() {
    let (mut h, hs) = harness(connected(vec![vm(1, "web", VmState::Running)]));
    h.run_steps(4);
    request_close(&mut h);
    h.get_by_label("1 VM is running");
    hs.update(|s| s.vms[0].state = VmState::Stopped);
    wait_closed(&mut h, std::time::Duration::from_secs(5));
    assert!(hs.actions().is_empty(), "nothing to stop");
    assert!(
        *hs.stopped.lock().unwrap(),
        "supervisor stopped before closing"
    );
}

/// Finding 12: a VM started after the confirmation is not waited for (the
/// supervisor shutdown stops it).
#[test]
fn only_vms_active_at_confirmation_are_waited_for() {
    let (mut h, hs) = harness(connected(vec![
        vm(1, "web", VmState::Running),
        vm(2, "later", VmState::Stopped),
    ]));
    h.run_steps(4);
    request_close(&mut h);
    h.get_by_role_and_label(Role::Button, "Stop them and quit")
        .click();
    h.run_steps(4);
    assert_eq!(hs.actions(), [ServiceCommand::Stop(common::vm_id(1))]);
    hs.update(|s| {
        s.vms[0].state = VmState::Stopped;
        s.vms[1].state = VmState::Running;
        s.vms[1].run_id = Some(common::run_id(2));
    });
    wait_closed(&mut h, std::time::Duration::from_secs(5));
}
