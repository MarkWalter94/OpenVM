//! F2 end-to-end test against a real supervisor and a real QEMU (ignored by
//! default): the product path of the UI, driven headless with
//! `egui_kittest`. ISO import through the (fake) file picker, VM created in
//! the wizard, start from the Dashboard, console opened with the per-run
//! display ticket, framebuffer received, keyboard sent as `Input` batches,
//! screenshot saved, orderly stop and deletion.
//!
//! It goes through neither winit nor the operating system (the scancode →
//! `egui::Key` layer remains untested here).
//!
//! ```text
//! LIGHTVM_QEMU_DIR=C:/Woarkareas/OpenVM/lab/qemu/qemu-11.1.0 \
//! LIGHTVM_ISO=C:/Woarkareas/OpenVM/lab/media/<ubuntu>.iso \
//! cargo test -p lightvm-desktop --test real_console -- --ignored --nocapture
//! ```
//!
//! The `lightvm-service` executable is looked up as in `service_child.rs`.
//! The catalog (with the managed copy of the ISO) is created in the temporary
//! directory and removed at the end unless `LIGHTVM_E2E_KEEP` is set.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use lightvm_desktop::service::{Service, ServiceConfig};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::VmState;
use lightvm_proto::ipc::ParentLossPolicy;

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

fn wait(
    h: &mut Harness<'_, LightVmApp>,
    what: &str,
    secs: u64,
    ok: impl Fn(&mut Harness<'_, LightVmApp>) -> bool,
) {
    let t0 = Instant::now();
    while !ok(h) {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(100));
        h.run_steps(3);
    }
    println!("E2E {what}: {:.1} s", t0.elapsed().as_secs_f64());
}

fn vm_state(h: &Harness<'_, LightVmApp>, name: &str) -> Option<VmState> {
    h.state()
        .service()?
        .state()
        .vms
        .iter()
        .find(|v| v.name.as_str() == name)
        .map(|v| v.state)
}

fn enabled(h: &Harness<'_, LightVmApp>, label: &str) -> bool {
    h.query_by_role_and_label(Role::Button, label)
        .is_some_and(|n| !n.accesskit_node().is_disabled())
}

#[test]
#[ignore = "requires a real QEMU and an ISO: set LIGHTVM_QEMU_DIR and LIGHTVM_ISO"]
fn wizard_start_and_console_with_real_qemu() {
    let (Some(qemu), Some(iso)) = (
        std::env::var_os("LIGHTVM_QEMU_DIR").map(PathBuf::from),
        std::env::var_os("LIGHTVM_ISO").map(PathBuf::from),
    ) else {
        eprintln!("LIGHTVM_QEMU_DIR or LIGHTVM_ISO not set: test skipped");
        return;
    };
    let exe = service_executable().expect("lightvm-service built");
    let dir = std::env::temp_dir().join(format!("lightvm-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = ServiceConfig {
        executable: exe,
        catalog: dir.join("catalog"),
        qemu_dir: Some(qemu),
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: ParentLossPolicy::KillVms,
        pairing_dir: Some(dir.join("pairing")),
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
    let shot = dir.join("e2e-console.png");
    let shot2 = shot.clone();
    app.set_save_picker(Some(Box::new(move |_n: &str| Some(shot2.clone()))));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 1300.0))
        .with_max_steps(10_000_000)
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    *ctx_holder.lock().unwrap() = Some(h.ctx.clone());
    h.run_steps(3);
    wait(&mut h, "supervisor connected", 30, |h| enabled(h, "New VM"));

    // Wizard: name, system, import of the ISO, resources, create.
    h.get_by_role_and_label(Role::Button, "New VM").click();
    h.run_steps(3);
    h.run_steps(3);
    h.get_by_role(Role::TextInput).type_text("e2e-ubuntu");
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Next").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Next").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Import ISO…").click();
    h.run_steps(3);
    wait(&mut h, "ISO imported and selected", 900, |h| {
        let failed = h.state().service().and_then(|s| match s.state().import {
            lightvm_desktop::service::RequestStatus::Failed(f) => Some(f.message),
            _ => None,
        });
        assert!(failed.is_none(), "import failed: {failed:?}");
        enabled(h, "Next")
    });
    for _ in 0..8 {
        if h.state()
            .wizard()
            .is_some_and(|w| w.step == lightvm_desktop::wizard::Step::Summary)
        {
            break;
        }
        assert!(
            enabled(&h, "Next"),
            "wizard blocked: {:?}",
            h.state().wizard()
        );
        h.get_by_role_and_label(Role::Button, "Next").click();
        h.run_steps(3);
    }
    println!(
        "E2E wizard: {:?}",
        h.state()
            .wizard()
            .map(|w| (w.step, w.vcpus, w.memory_mib, w.disk_gib))
    );
    h.get_by_label("Step 7 of 7: Summary");
    h.get_by_role_and_label(Role::Button, "Create").click();
    h.run_steps(3);
    wait(&mut h, "VM created", 120, |h| {
        h.state().wizard().is_none() && vm_state(h, "e2e-ubuntu").is_some()
    });

    // Start, then console from the Dashboard.
    h.get_by_role_and_label(Role::Button, "Start e2e-ubuntu")
        .click();
    h.run_steps(3);
    wait(&mut h, "VM running", 120, |h| {
        vm_state(h, "e2e-ubuntu") == Some(VmState::Running)
    });
    wait(&mut h, "console button enabled", 30, |h| {
        enabled(h, "Open the console of e2e-ubuntu")
    });
    h.get_by_role_and_label(Role::Button, "Open the console of e2e-ubuntu")
        .click();
    h.run_steps(3);
    assert_eq!(h.state().section(), Section::Console);
    wait(&mut h, "framebuffer received", 120, |h| {
        h.state()
            .live_console()
            .is_some_and(|c| c.geometry().0 > 0 && c.stats.texture_uploads > 0)
    });
    // Let the installer draw its first screen.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(45) {
        std::thread::sleep(Duration::from_millis(200));
        h.run_steps(3);
    }
    let (geom, link, stats) = {
        let c = h.state().live_console().unwrap();
        (c.geometry(), c.link().describe().0, c.stats.clone())
    };
    println!("E2E console geometry={geom:?} link=\"{link}\" stats={stats:?}");
    h.get_by_label("Console of e2e-ubuntu").click();
    h.run_steps(3);
    for k in [egui::Key::ArrowDown, egui::Key::ArrowUp] {
        h.key_press(k);
        h.run_steps(3);
        std::thread::sleep(Duration::from_millis(300));
        h.run_steps(3);
    }
    let input_error = h.state().service().and_then(|s| s.state().input_error);
    println!("E2E input_error={input_error:?}");
    h.get_by_role_and_label(Role::Button, "Screenshot").click();
    h.run_steps(3);
    let png = std::fs::read(&shot).expect("screenshot saved");
    println!("E2E screenshot={} bytes={}", shot.display(), png.len());

    // Orderly stop of the supervisor (VMs stopped by it).
    let mut app = h.into_state();
    let outcome = app.stop_service();
    println!("E2E service_stop={outcome:?}");
    // The catalog holds a copy of the ISO: removed unless asked to keep it.
    if std::env::var_os("LIGHTVM_E2E_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        println!("E2E catalog kept: {}", dir.display());
    }
}
