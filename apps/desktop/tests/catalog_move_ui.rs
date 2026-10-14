//! Settings → Move catalog…: availability (all VMs stopped, supervisor
//! started by this window), confirmation, progress with Cancel, outcome,
//! settings pointed to the new folder only after a successful move. The
//! move itself is a fake runner here (the real one is `lightvm-service
//! --move-catalog`, tested in `vm-storage` and `lightvm-service`).

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{FakeSupervisor, Handles, connected, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::catalog_move::{MoveOutcome, MoveRequest, MoveRunner, MoveStatus};
use lightvm_desktop::service::ServiceConfig;
use lightvm_desktop::settings::UiSettings;
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::VmState;
use lightvm_proto::ipc::ParentLossPolicy;

const MOVE: &str = "Move the catalog and its VMs to another folder";

struct Setup {
    base: PathBuf,
    catalog: PathBuf,
    settings_file: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn setup(name: &str) -> Setup {
    let base = std::env::temp_dir().join(format!(
        "lightvm-move-ui-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let catalog = base.join("catalog");
    std::fs::create_dir_all(&catalog).unwrap();
    // Only its existence is checked by the UI.
    std::fs::write(catalog.join("catalog.sqlite3"), b"").unwrap();
    Setup {
        settings_file: base.join("ui-settings.json"),
        catalog,
        base,
    }
}

fn config(catalog: &Path) -> ServiceConfig {
    ServiceConfig {
        executable: PathBuf::from("lightvm-service"),
        catalog: catalog.to_path_buf(),
        qemu_dir: None,
        runtime_dir: None,
        audit_log: None,
        policy: ParentLossPolicy::KillVms,
        pairing_dir: None,
    }
}

/// App on the Settings page, supervisor started by the app on the catalog of
/// `s`, folder picker answering `<base>/new`, the given move runner.
fn app(
    s: &Setup,
    vm_state: VmState,
    runner: MoveRunner,
) -> (Harness<'static, LightVmApp>, Handles) {
    let (fake, hs) = FakeSupervisor::new(connected(vec![vm(1, "ubuntu", vm_state)]));
    let mut app = LightVmApp::new(false);
    app.set_settings(
        UiSettings {
            catalog: Some(s.catalog.clone()),
            ..UiSettings::default()
        },
        Some(s.settings_file.clone()),
        None,
    );
    app.set_supervisor_with_config(fake, &config(&s.catalog));
    // The restart after the move fails at once: no such executable.
    app.set_service_exe(Some(s.base.join("no-such-lightvm-service")));
    let new = s.base.join("new");
    app.set_folder_picker(Some(Box::new(move |_t: &str| Some(new.clone()))));
    app.set_move_runner(Some(runner));
    app.set_section(Section::Settings);
    let h = Harness::builder()
        .with_size(egui::vec2(1280.0, 2600.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    (h, hs)
}

fn wait_for(h: &mut Harness<'_, LightVmApp>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        h.step();
        if h.query_by_label(label).is_some() {
            // The progress dialog keeps repainting (spinner): no `run`.
            h.run_steps(2);
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("`{label}` never shown");
}

type Seen = Arc<Mutex<Option<MoveRequest>>>;

fn runner(seen: &Seen, outcome: MoveOutcome) -> MoveRunner {
    let seen = seen.clone();
    Arc::new(move |req, progress, _cancel| {
        *seen.lock().unwrap() = Some(req.clone());
        progress(MoveStatus {
            phase: "copying".into(),
            done_bytes: 50,
            total_bytes: 100,
        });
        outcome.clone()
    })
}

fn moved() -> MoveOutcome {
    MoveOutcome::Moved {
        files: 12,
        bytes: 3 << 30,
        vms: 1,
        source_removed: true,
        source_cleanup_error: None,
    }
}

fn open_confirmation(h: &mut Harness<'_, LightVmApp>) {
    h.run();
    h.get_by_role_and_label(Role::Button, MOVE).click();
    h.run();
    h.get_by_label("Move the catalog?");
}

#[test]
fn move_stops_the_supervisor_and_points_settings_to_the_new_folder() {
    let s = setup("ok");
    let seen = Seen::default();
    let (mut h, hs) = app(&s, VmState::Stopped, runner(&seen, moved()));
    // The plain folder change says the VMs stay behind.
    h.run();
    assert!(
        h.query_all_by_label_contains("the VMs already in the catalog are not moved")
            .next()
            .is_some()
    );
    open_confirmation(&mut h);
    // Nothing happens before the confirmation.
    assert!(seen.lock().unwrap().is_none());
    assert!(!*hs.stopped.lock().unwrap());
    h.get_by_role_and_label(Role::Button, "Move catalog")
        .click();
    wait_for(&mut h, "Catalog moved");

    let req = seen.lock().unwrap().clone().unwrap();
    assert_eq!(req.from, s.catalog);
    assert_eq!(req.to, s.base.join("new"));
    assert!(!req.keep_source);
    assert!(*hs.stopped.lock().unwrap(), "supervisor not stopped first");
    let new = Some(s.base.join("new"));
    assert_eq!(h.state().settings().catalog, new);
    assert_eq!(UiSettings::load(&s.settings_file).0.catalog, new);
    h.get_by_role_and_label(Role::Button, "OK").click();
    h.run();
    assert!(!h.state().catalog_move_open());
}

#[test]
fn keep_source_is_passed_to_the_move() {
    let s = setup("keep");
    let seen = Seen::default();
    let (mut h, _hs) = app(&s, VmState::Stopped, runner(&seen, moved()));
    open_confirmation(&mut h);
    h.get_by_label_contains("Keep the original folder").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Move catalog")
        .click();
    wait_for(&mut h, "Catalog moved");
    assert!(seen.lock().unwrap().as_ref().unwrap().keep_source);
}

#[test]
fn failed_move_keeps_the_settings_and_shows_the_cause() {
    let s = setup("fail");
    let seen = Seen::default();
    let (mut h, _hs) = app(
        &s,
        VmState::Stopped,
        runner(&seen, MoveOutcome::Failed("not enough free space".into())),
    );
    open_confirmation(&mut h);
    h.get_by_role_and_label(Role::Button, "Move catalog")
        .click();
    wait_for(&mut h, "Catalog not moved");
    assert!(
        h.query_all_by_label_contains("not enough free space")
            .next()
            .is_some()
    );
    assert_eq!(h.state().settings().catalog, Some(s.catalog.clone()));
}

#[test]
fn cancel_during_the_move_keeps_the_catalog_where_it_was() {
    let s = setup("cancel");
    let r: MoveRunner = Arc::new(|_req, progress, cancel| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cancel.load(Ordering::SeqCst) && Instant::now() < deadline {
            progress(MoveStatus {
                phase: "copying".into(),
                done_bytes: 1 << 20,
                total_bytes: 8 << 20,
            });
            std::thread::sleep(Duration::from_millis(20));
        }
        MoveOutcome::Cancelled
    });
    let (mut h, _hs) = app(&s, VmState::Stopped, r);
    open_confirmation(&mut h);
    // Cancel in the confirmation: nothing starts.
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.run();
    assert!(!h.state().catalog_move_open());
    open_confirmation(&mut h);
    h.get_by_role_and_label(Role::Button, "Move catalog")
        .click();
    wait_for(&mut h, "Copying the files");
    h.get_by_label("Moving the catalog…");
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    wait_for(&mut h, "Move cancelled");
    assert_eq!(h.state().settings().catalog, Some(s.catalog.clone()));
}

fn move_disabled(h: &Harness<'_, LightVmApp>) -> bool {
    h.get_by_role_and_label(Role::Button, MOVE)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn move_needs_every_vm_stopped_and_a_supervisor_started_here() {
    let s = setup("disabled");
    let seen = Seen::default();
    let (mut h, hs) = app(&s, VmState::Running, runner(&seen, moved()));
    h.run();
    assert!(move_disabled(&h), "enabled with a running VM");
    hs.update(|st| st.vms[0].state = VmState::Stopped);
    h.run();
    assert!(!move_disabled(&h));

    // Catalog from the command line.
    let (fake, _hs) = FakeSupervisor::new(connected(vec![vm(1, "ubuntu", VmState::Stopped)]));
    let mut cli = LightVmApp::new(false);
    cli.set_command_line_config(config(&s.catalog));
    cli.set_supervisor_with_config(fake, &config(&s.catalog));
    cli.set_folder_picker(Some(Box::new(|_t: &str| None)));
    cli.set_section(Section::Settings);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 2600.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), cli);
    h.run();
    assert!(move_disabled(&h), "enabled with --catalog");

    // No catalog in the folder yet.
    std::fs::remove_file(s.catalog.join("catalog.sqlite3")).unwrap();
    let (mut h, _hs) = app(&s, VmState::Stopped, runner(&seen, moved()));
    h.run();
    assert!(move_disabled(&h), "enabled without a catalog");
    assert!(seen.lock().unwrap().is_none());
}
