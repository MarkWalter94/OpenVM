//! Supervisor started without a QEMU folder: the Dashboard says so in a
//! notice, Start is disabled with the reason, and a start refused anyway is
//! explained instead of "capability not available on this host". With a QEMU
//! folder (or a supervisor whose configuration the app does not know) nothing
//! changes. The configuration is given explicitly: the tests do not depend on
//! whether this host has QEMU installed.

mod common;

use std::path::PathBuf;

use common::{FakeSupervisor, Handles, connected, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::dashboard::{NO_QEMU, NO_QEMU_COMMAND_LINE};
use lightvm_desktop::service::{ServiceCommand, ServiceConfig};
use lightvm_desktop::{LightVmApp, Section, SupervisorQemu};
use lightvm_proto::VmState;
use lightvm_proto::ipc::ParentLossPolicy;

const REFUSED: &str = "start: unsupported: capability not available on this host";

fn config(qemu_dir: Option<PathBuf>) -> ServiceConfig {
    ServiceConfig {
        executable: PathBuf::from("lightvm-service"),
        catalog: PathBuf::from("catalog"),
        qemu_dir,
        runtime_dir: None,
        audit_log: None,
        policy: ParentLossPolicy::KillVms,
        pairing_dir: None,
    }
}

/// App with a stopped VM, connected to a stand-in supervisor started with
/// `cfg` (`None`: configuration unknown to the app).
fn app(cfg: Option<&ServiceConfig>, command_line: bool) -> (Harness<'static, LightVmApp>, Handles) {
    let (fake, hs) = FakeSupervisor::new(connected(vec![vm(1, "vista", VmState::Stopped)]));
    let mut app = LightVmApp::new(false);
    if let (true, Some(c)) = (command_line, cfg) {
        app.set_command_line_config(c.clone());
    }
    match cfg {
        Some(c) => app.set_supervisor_with_config(fake, c),
        None => app.set_supervisor(fake),
    }
    let h = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    (h, hs)
}

fn start_enabled(h: &Harness<'_, LightVmApp>) -> bool {
    !h.get_by_role_and_label(Role::Button, "Start vista")
        .accesskit_node()
        .is_disabled()
}

fn refused_start(hs: &Handles) {
    hs.update(|s| {
        s.last_outcome = Some(REFUSED.into());
        s.start_unavailable_outcome = Some(REFUSED.into());
    });
}

#[test]
fn without_qemu_the_dashboard_explains_and_start_is_disabled() {
    let (mut h, hs) = app(Some(&config(None)), false);
    h.run();
    assert_eq!(
        h.state().supervisor_qemu(),
        SupervisorQemu::MissingFromSettings
    );
    h.get_by_label(NO_QEMU);
    assert!(!start_enabled(&h));
    h.get_by_role_and_label(Role::Button, "Start vista").click();
    h.run();
    assert!(
        !hs.actions()
            .iter()
            .any(|c| matches!(c, ServiceCommand::Start(_))),
        "start sent without QEMU"
    );
    // The button leads to Settings, where the QEMU folder is chosen.
    h.get_by_role_and_label(Role::Button, "Open the QEMU settings")
        .click();
    h.run();
    assert_eq!(h.state().section(), Section::Settings);
}

#[test]
fn without_qemu_a_refused_start_names_the_cause() {
    let (mut h, hs) = app(Some(&config(None)), false);
    refused_start(&hs);
    h.run();
    h.get_by_label(&format!("Last action: start refused. {NO_QEMU}"));
    assert!(
        h.query_by_label(&format!("Last action: {REFUSED}"))
            .is_none()
    );
    // A later, different outcome is shown as is.
    hs.update(|s| s.last_outcome = Some("VM list not refreshed: x".into()));
    h.run();
    h.get_by_label("Last action: VM list not refreshed: x");
}

#[test]
fn command_line_without_qemu_dir_points_to_the_option() {
    let (mut h, _hs) = app(Some(&config(None)), true);
    h.run();
    assert_eq!(
        h.state().supervisor_qemu(),
        SupervisorQemu::MissingFromCommandLine
    );
    h.get_by_label(NO_QEMU_COMMAND_LINE);
    assert!(h.query_by_label(NO_QEMU).is_none());
    // The Settings choice is not used with --catalog: no button there.
    assert!(h.query_by_label("Open the QEMU settings").is_none());
    assert!(!start_enabled(&h));
}

#[test]
fn with_qemu_nothing_changes() {
    for (cfg, command_line) in [
        (Some(config(Some(PathBuf::from("qemu-11.1.0")))), false),
        (Some(config(Some(PathBuf::from("qemu-11.1.0")))), true),
        // Configuration unknown to the app: nothing to report.
        (None, false),
    ] {
        let (mut h, hs) = app(cfg.as_ref(), command_line);
        refused_start(&hs);
        h.run();
        assert_eq!(h.state().supervisor_qemu(), SupervisorQemu::Given);
        assert!(h.query_by_label(NO_QEMU).is_none());
        assert!(h.query_by_label(NO_QEMU_COMMAND_LINE).is_none());
        assert!(h.query_by_label("Open the QEMU settings").is_none());
        h.get_by_label(&format!("Last action: {REFUSED}"));
        assert!(start_enabled(&h));
        h.get_by_role_and_label(Role::Button, "Start vista").click();
        h.run();
        assert!(
            hs.actions()
                .iter()
                .any(|c| matches!(c, ServiceCommand::Start(_)))
        );
    }
}
