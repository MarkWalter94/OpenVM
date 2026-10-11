//! Progress of a start (F3): verification of the installation media and of
//! the baseline before QEMU is launched, shown on the Dashboard row as text
//! with a percentage (not colour only) and an accessible progress bar; the
//! VM list is re-read about every 500 ms while a VM is starting.

mod common;

use std::time::Duration;

use common::{FakeSupervisor, connected, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use lightvm_desktop::LightVmApp;
use lightvm_desktop::service::ServiceCommand;
use lightvm_proto::VmState;
use lightvm_proto::ipc::{StartPhase, StartProgress};

fn refreshes(hs: &common::Handles) -> usize {
    hs.sent()
        .iter()
        .filter(|c| **c == ServiceCommand::Refresh)
        .count()
}

#[test]
fn start_progress_is_shown_and_polled() {
    let mut v = vm(1, "lab", VmState::Starting);
    v.start = Some(StartProgress {
        phase: StartPhase::VerifyingMedia,
        verified_bytes: 42,
        total_bytes: 100,
    });
    let (fake, hs) = FakeSupervisor::new(connected(vec![v]));
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Label, "Verifying installation media… 42 %");
    h.get_by_role_and_label(
        Role::ProgressIndicator,
        "Verifying installation media… 42 %",
    );
    // Polled at once, then not again before the interval.
    assert_eq!(refreshes(&hs), 1);
    h.run_steps(4);
    assert_eq!(refreshes(&hs), 1);
    std::thread::sleep(lightvm_desktop::START_POLL + Duration::from_millis(50));
    h.run_steps(2);
    assert_eq!(refreshes(&hs), 2);

    hs.update(|s| {
        s.vms[0].start = Some(StartProgress {
            phase: StartPhase::VerifyingBaseline,
            verified_bytes: 3 << 30,
            total_bytes: 4 << 30,
        });
    });
    h.run_steps(2);
    h.get_by_role_and_label(Role::Label, "Verifying baseline… 75 %");
    hs.update(|s| {
        s.vms[0].start = Some(StartProgress {
            phase: StartPhase::Launching,
            verified_bytes: 4 << 30,
            total_bytes: 4 << 30,
        });
    });
    h.run_steps(2);
    h.get_by_label("Launching…");

    // Running: no progress, no more polling.
    hs.update(|s| {
        s.vms[0].state = VmState::Running;
        s.vms[0].start = None;
    });
    h.run_steps(2);
    assert!(h.query_by_label("Launching…").is_none());
    let n = refreshes(&hs);
    std::thread::sleep(lightvm_desktop::START_POLL + Duration::from_millis(50));
    h.run_steps(2);
    assert_eq!(refreshes(&hs), n);
}
