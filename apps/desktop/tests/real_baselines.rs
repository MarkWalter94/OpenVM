//! F3 end-to-end test of the Baselines page against a real supervisor, a
//! real QEMU and the real `qemu-img` (ignored by default), driven headless
//! with `egui_kittest` through the AccessKit tree, as a user would: VM
//! created in the wizard, start, orderly stop (or forced stop), Baselines
//! page from the Dashboard row, creation (with the explicit consent if the
//! stop was not clean), restore, start on the overlay, second baseline taken
//! on the overlay, restore of it, deletion of the first one, refusal to
//! delete the current one, VM deletion.
//!
//! ```text
//! LIGHTVM_QEMU_DIR=C:/Woarkareas/OpenVM/lab/qemu/qemu-11.1.0 \
//! LIGHTVM_ISO=C:/Woarkareas/OpenVM/lab/media/<small>.iso \
//! LIGHTVM_E2E_EVIDENCE=C:/Woarkareas/OpenVM/lab/f3/ui \
//! cargo test -p lightvm-desktop --test real_baselines -- --ignored --nocapture
//! ```
//!
//! The ISO is only the installation media the wizard asks for: the baseline
//! path does not need a guest system (the VM's disk is what is copied). With
//! `LIGHTVM_E2E_EVIDENCE` the screens are rendered to PNG files there. The
//! catalog is created in the temporary directory and removed at the end
//! unless `LIGHTVM_E2E_KEEP` is set.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::LightVmApp;
use lightvm_desktop::service::{RequestStatus, Service, ServiceConfig, ServiceState};
use lightvm_proto::VmState;
use lightvm_proto::ipc::ParentLossPolicy;

const VM: &str = "e2e-baselines";

fn service_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LIGHTVM_SERVICE_EXE") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe
        .parent()?
        .parent()?
        .join(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX));
    p.exists().then_some(p)
}

fn state(h: &Harness<'_, LightVmApp>) -> ServiceState {
    h.state().service().map(|s| s.state()).unwrap_or_default()
}

fn wait(
    h: &mut Harness<'_, LightVmApp>,
    what: &str,
    secs: u64,
    ok: impl Fn(&mut Harness<'_, LightVmApp>) -> bool,
) -> Duration {
    let t0 = Instant::now();
    while !ok(h) {
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timeout: {what}; state: {:?}",
            state(h).baseline
        );
        std::thread::sleep(Duration::from_millis(100));
        h.run_steps(3);
    }
    let e = t0.elapsed();
    println!("E2E {what}: {:.1} s", e.as_secs_f64());
    e
}

fn vm_state(h: &Harness<'_, LightVmApp>) -> Option<VmState> {
    state(h)
        .vms
        .iter()
        .find(|v| v.name.as_str() == VM)
        .map(|v| v.state)
}

fn enabled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    h.query_by_role_and_label(Role::Button, label)
        .is_some_and(|n| !n.accesskit_node().is_disabled())
}

fn click(h: &mut Harness<'_, LightVmApp>, label: &str) {
    h.get_by_role_and_label(Role::Button, label).click();
    h.run_steps(3);
}

fn evidence(h: &mut Harness<'_, LightVmApp>, name: &str) {
    let Some(dir) = std::env::var_os("LIGHTVM_E2E_EVIDENCE").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    match h.render() {
        Ok(img) => {
            img.save(&path).unwrap();
            println!("E2E evidence {}", path.display());
        }
        Err(e) => println!("E2E evidence {name} not rendered: {e}"),
    }
}

/// Labels of the VM's baselines, oldest first, with (verified, current).
fn baselines(h: &Harness<'_, LightVmApp>) -> Vec<(String, bool, bool)> {
    let s = state(h);
    let Some(v) = s.vms.iter().find(|v| v.name.as_str() == VM) else {
        return Vec::new();
    };
    s.baselines.get(&v.vm_id).map_or_else(Vec::new, |l| {
        l.items
            .iter()
            .map(|b| (b.label.as_str().to_owned(), b.clean_shutdown, b.current))
            .collect()
    })
}

/// Waits for the outcome of the baseline operation just confirmed; fails
/// on a refusal.
fn wait_outcome(h: &mut Harness<'_, LightVmApp>, what: &str, secs: u64) -> Duration {
    let d = wait(h, what, secs, |h| {
        state(h).baseline.is_some_and(|b| !b.status.is_pending())
    });
    match state(h).baseline.map(|b| b.status) {
        Some(RequestStatus::Done(_)) => {}
        other => panic!("{what}: {other:?}"),
    }
    // Event, VM list and baselines re-read.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(100));
        h.run_steps(3);
    }
    d
}

/// Create baseline… → Create baseline; if the supervisor refuses for an
/// unclean stop, the consent is given. Returns whether it was asked.
fn create_baseline(h: &mut Harness<'_, LightVmApp>, n: usize) -> bool {
    click(h, &format!("Create a baseline of {VM}"));
    evidence(h, &format!("{n}-create-dialog"));
    click(h, "Create baseline");
    let unclean_title = format!("\u{201c}{VM}\u{201d} was not shut down cleanly");
    wait(h, "creation outcome or consent", 1800, |h| {
        h.query_by_label(&unclean_title).is_some()
            || state(h).baseline.is_some_and(|b| !b.status.is_pending())
    });
    // A refusal switches the dialog to the consent at the next frame.
    h.run_steps(3);
    let asked = h.query_by_label(&unclean_title).is_some();
    if asked {
        evidence(h, &format!("{n}-create-unclean-consent"));
        assert!(!enabled(h, "Create unverified baseline"));
        h.get_by_role_and_label(
            Role::CheckBox,
            "I understand: the baseline is marked Not verified",
        )
        .click();
        h.run_steps(3);
        click(h, "Create unverified baseline");
        let d = wait_outcome(h, "baseline created (not verified)", 1800);
        println!("E2E create (unclean consent) took {:.1} s", d.as_secs_f64());
    } else {
        match state(h).baseline.map(|b| b.status) {
            Some(RequestStatus::Done(_)) => println!("E2E create (verified) done"),
            other => panic!("creation: {other:?}"),
        }
    }
    evidence(h, &format!("{n}-after-create"));
    asked
}

fn stop_vm(h: &mut Harness<'_, LightVmApp>) -> &'static str {
    click(h, &format!("Stop {VM}"));
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(120) {
        if vm_state(h) == Some(VmState::Stopped) {
            println!(
                "E2E orderly stop: stopped in {:.1} s",
                t0.elapsed().as_secs_f64()
            );
            return "orderly";
        }
        std::thread::sleep(Duration::from_millis(200));
        h.run_steps(3);
    }
    // The guest ignored the power button: forced stop, confirmed.
    click(h, &format!("More actions for {VM}"));
    click(h, "Force stop…");
    click(h, "Force stop");
    wait(h, "VM stopped (forced)", 60, |h| {
        vm_state(h) == Some(VmState::Stopped)
    });
    "forced"
}

fn start_vm(h: &mut Harness<'_, LightVmApp>, what: &str) {
    wait(h, "Start enabled", 60, |h| {
        enabled(h, &format!("Start {VM}"))
    });
    click(h, &format!("Start {VM}"));
    wait(h, what, 300, |h| vm_state(h) == Some(VmState::Running));
    // Let the firmware run for a while.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(200));
        h.run_steps(3);
    }
}

fn open_baselines(h: &mut Harness<'_, LightVmApp>) {
    click(h, &format!("More actions for {VM}"));
    click(h, &format!("Baselines of {VM}"));
    wait(h, "baselines read", 30, |h| {
        let s = state(h);
        s.vms
            .iter()
            .find(|v| v.name.as_str() == VM)
            .and_then(|v| s.baselines.get(&v.vm_id))
            .is_some_and(|l| l.loaded)
    });
}

#[test]
#[ignore = "requires a real QEMU and an ISO: set LIGHTVM_QEMU_DIR and LIGHTVM_ISO"]
fn baselines_page_with_real_qemu() {
    let (Some(qemu), Some(iso)) = (
        std::env::var_os("LIGHTVM_QEMU_DIR").map(PathBuf::from),
        std::env::var_os("LIGHTVM_ISO").map(PathBuf::from),
    ) else {
        eprintln!("LIGHTVM_QEMU_DIR or LIGHTVM_ISO not set: test skipped");
        return;
    };
    let exe = service_executable().expect("lightvm-service built");
    let dir = std::env::temp_dir().join(format!("lightvm-e2e-f3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = ServiceConfig {
        executable: exe,
        catalog: dir.join("catalog"),
        qemu_dir: Some(qemu),
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: ParentLossPolicy::KillVms,
    };
    let ctx_holder: Arc<std::sync::Mutex<Option<egui::Context>>> = Arc::default();
    let c2 = ctx_holder.clone();
    let service = Service::start(
        &cfg,
        Arc::new(move || {
            if let Some(c) = c2.lock().unwrap().as_ref() {
                c.request_repaint();
            }
        }),
    )
    .expect("supervisor start");
    let mut app = LightVmApp::new(false);
    app.set_service(service, &cfg);
    let iso2 = iso.clone();
    app.set_file_picker(Some(Box::new(move |_t: &str| Some(iso2.clone()))));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 760.0))
        .with_max_steps(10_000_000)
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    *ctx_holder.lock().unwrap() = Some(h.ctx.clone());
    h.run_steps(3);
    wait(&mut h, "supervisor connected", 30, |h| enabled(h, "New VM"));
    let caps = state(&h).host.map(|c| c.baseline_available);
    println!("E2E baseline_available={caps:?}");

    // Wizard.
    click(&mut h, "New VM");
    h.run_steps(3);
    h.get_by_role(Role::TextInput).type_text(VM);
    h.run_steps(3);
    click(&mut h, "Next");
    click(&mut h, "Next");
    click(&mut h, "Import ISO…");
    wait(&mut h, "ISO imported", 900, |h| enabled(h, "Next"));
    // Next until the summary (each step may wait for the supervisor).
    let t0 = Instant::now();
    while h.query_by_role_and_label(Role::Button, "Create").is_none() {
        if t0.elapsed() > Duration::from_secs(120) {
            evidence(&mut h, "wizard-blocked");
            panic!("wizard blocked: {:?}", h.state().wizard());
        }
        if enabled(&h, "Next") {
            click(&mut h, "Next");
        } else {
            std::thread::sleep(Duration::from_millis(100));
            h.run_steps(3);
        }
    }
    click(&mut h, "Create");
    wait(&mut h, "VM created", 120, |h| {
        h.state().wizard().is_none() && vm_state(h).is_some()
    });

    // First run, then stop.
    start_vm(&mut h, "VM running (first run)");
    let first_stop = stop_vm(&mut h);

    // Baselines page from the Dashboard row.
    open_baselines(&mut h);
    h.get_by_label("No baselines yet.");
    h.get_by_label("Standalone disk");
    evidence(&mut h, "0-empty");
    let asked = create_baseline(&mut h, 1);
    println!("E2E first stop: {first_stop}; consent asked: {asked}");
    let list = baselines(&h);
    println!("E2E baselines after create: {list:?}");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].1, !asked, "verified iff no consent was needed");
    let first = list[0].0.clone();
    if asked {
        h.get_by_label("Not verified");
    } else {
        h.get_by_label("Verified");
    }

    // Restore the first baseline.
    click(&mut h, &format!("Restore baseline {first}"));
    evidence(&mut h, "2-restore-dialog");
    click(&mut h, "Discard and restore");
    let d = wait_outcome(&mut h, "first baseline restored", 1800);
    println!("E2E restore took {:.1} s", d.as_secs_f64());
    h.get_by_label(&format!("Running on baseline \u{201c}{first}\u{201d}"));
    assert!(baselines(&h)[0].2, "current after the restore");
    assert!(
        !enabled(&h, &format!("Delete baseline {first}")),
        "the current baseline cannot be deleted"
    );
    evidence(&mut h, "3-after-restore");

    // Start on the overlay (the baseline is verified first), stop.
    click(&mut h, "Back to the Dashboard");
    h.get_by_label_contains(&format!("On baseline \u{201c}{first}\u{201d}"));
    evidence(&mut h, "4-dashboard-on-baseline");
    start_vm(&mut h, "VM running on the overlay");
    let second_stop = stop_vm(&mut h);
    println!("E2E second stop: {second_stop}");

    // Second baseline, taken on the overlay; restore it; delete the first.
    open_baselines(&mut h);
    create_baseline(&mut h, 5);
    let list = baselines(&h);
    println!("E2E baselines after the second create: {list:?}");
    assert_eq!(list.len(), 2);
    let second = list[1].0.clone();
    click(&mut h, &format!("Restore baseline {second}"));
    click(&mut h, "Discard and restore");
    wait_outcome(&mut h, "second baseline restored", 1800);
    h.get_by_label(&format!("Running on baseline \u{201c}{second}\u{201d}"));
    assert!(enabled(&h, &format!("Delete baseline {first}")));
    click(&mut h, &format!("Delete baseline {first}"));
    evidence(&mut h, "6-delete-dialog");
    click(&mut h, "Delete baseline");
    wait_outcome(&mut h, "first baseline deleted", 120);
    let list = baselines(&h);
    println!("E2E baselines at the end: {list:?}");
    assert_eq!(list.len(), 1);
    assert!(list[0].2);
    h.get_by_label("Baseline deleted.");
    evidence(&mut h, "7-final");

    // VM deletion (baselines included).
    click(&mut h, "Back to the Dashboard");
    click(&mut h, &format!("More actions for {VM}"));
    click(&mut h, "Delete…");
    click(&mut h, "Delete VM");
    wait(&mut h, "VM deleted", 120, |h| vm_state(h).is_none());

    let mut app = h.into_state();
    let outcome = app.stop_service();
    println!("E2E service_stop={outcome:?}");
    if let Ok(audit) = std::fs::read_to_string(dir.join("audit.jsonl")) {
        for l in audit.lines().filter(|l| l.contains("baseline_")) {
            println!("E2E audit {l}");
        }
    }
    if std::env::var_os("LIGHTVM_E2E_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        println!("E2E catalog kept: {}", dir.display());
    }
}
