//! Real supervisor started by the UI as a child process (F1-11): `ready`
//! line, two authenticated IPC connections, event subscription, VM list and
//! controlled stop through the life pipe. No QEMU: without `--qemu-dir` the
//! service uses the "not configured" backend.
//!
//! The `lightvm-service` executable is looked up in `LIGHTVM_SERVICE_EXE` or
//! in the build directory next to the tests (present after
//! `cargo build --workspace` or `cargo test --workspace`). If it is missing,
//! the test says so on stderr and runs nothing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightvm_desktop::service::{Service, ServiceCommand, ServiceConfig, StartError};
use lightvm_proto::ipc::ParentLossPolicy;

fn service_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LIGHTVM_SERVICE_EXE") {
        return Some(PathBuf::from(p));
    }
    // target/<profile>/deps/<test>.exe → target/<profile>/lightvm-service.exe
    let exe = std::env::current_exe().ok()?;
    let p = exe
        .parent()?
        .parent()?
        .join(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX));
    p.exists().then_some(p)
}

fn temp_catalog(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lightvm-desktop-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for(s: &Service, what: &str, ok: impl Fn(&Service) -> bool) {
    let t0 = Instant::now();
    while !ok(s) {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "{what}: state {:?}",
            s.state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ui_starts_the_supervisor_and_stops_it() {
    let Some(exe) = service_executable() else {
        eprintln!("lightvm-service not built: test skipped (cargo build --workspace)");
        return;
    };
    let dir = temp_catalog("child");
    let cfg = ServiceConfig {
        executable: exe,
        catalog: dir.join("catalog"),
        qemu_dir: None,
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: ParentLossPolicy::default(),
        pairing_dir: Some(dir.join("pairing")),
    };
    let mut s = Service::start(&cfg, Arc::new(|| {})).expect("supervisor start");
    assert!(s.pid() > 0);
    wait_for(&s, "connection", |s| s.state().connected);
    wait_for(&s, "capabilities", |s| s.state().backend.is_some());
    let st = s.state();
    assert_eq!(st.connection, "connected");
    assert!(st.vms.is_empty(), "new catalog: {:?}", st.vms);
    assert!(s.is_alive());

    // Emergency Stop without VMs: completed, no process.
    assert!(s.send(ServiceCommand::EmergencyStop));
    wait_for(&s, "Emergency Stop outcome", |s| {
        s.state().last_outcome.is_some()
    });
    let outcome = s.state().last_outcome.unwrap();
    assert!(
        outcome.starts_with("Emergency Stop: 0 processes"),
        "{outcome}"
    );

    // Second instance on the same catalog: refused with code 3.
    match Service::start(&cfg, Arc::new(|| {})) {
        Err(StartError::ExitedBeforeReady(Some(3))) => {}
        other => panic!("second instance: {other:?}"),
    }

    let (code, forced) = s.stop(Duration::from_secs(15));
    assert_eq!(code, Some(0), "regular exit");
    assert!(!forced, "controlled stop without forced termination");
    assert!(!s.is_alive());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings → Move catalog… with the real executable: refused while a
/// supervisor holds the catalog and for a destination inside it; then a
/// catalog written by a real supervisor is moved by `lightvm-service
/// --move-catalog` and a supervisor starts on the new folder.
#[test]
fn catalog_moves_with_the_real_executable() {
    use lightvm_desktop::catalog_move::{MoveOutcome, MoveRequest, child_runner};
    use std::sync::atomic::AtomicBool;
    let Some(exe) = service_executable() else {
        eprintln!("lightvm-service not built: test skipped (cargo build --workspace)");
        return;
    };
    let dir = temp_catalog("move");
    let cfg = |catalog: PathBuf| ServiceConfig {
        executable: exe.clone(),
        catalog,
        qemu_dir: None,
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: ParentLossPolicy::default(),
        pairing_dir: Some(dir.join("pairing")),
    };
    let mut s =
        Service::start(&cfg(dir.join("catalog")), Arc::new(|| {})).expect("supervisor start");
    wait_for(&s, "connection", |s| s.state().connected);
    let run = child_runner();
    let no_cancel = AtomicBool::new(false);

    let req = MoveRequest {
        executable: exe.clone(),
        from: dir.join("catalog"),
        to: dir.join("moved"),
        keep_source: false,
    };
    match run(&req, &|_| {}, &no_cancel) {
        MoveOutcome::Failed(m) => assert!(m.contains("in use"), "{m}"),
        other => panic!("move with the supervisor running: {other:?}"),
    }
    let (code, _) = s.stop(Duration::from_secs(15));
    assert_eq!(code, Some(0));
    drop(s);

    let inside = MoveRequest {
        to: dir.join("catalog").join("inside"),
        ..req.clone()
    };
    assert!(matches!(
        run(&inside, &|_| {}, &no_cancel),
        MoveOutcome::Failed(_)
    ));
    assert!(!dir.join("catalog").join("inside").exists());

    let phases = std::sync::Mutex::new(Vec::new());
    let outcome = run(&req, &|p| phases.lock().unwrap().push(p.phase), &no_cancel);
    assert!(
        matches!(
            outcome,
            MoveOutcome::Moved {
                source_removed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(phases.lock().unwrap().iter().any(|p| p == "checking"));
    assert!(!dir.join("catalog").exists());
    assert!(dir.join("moved").join("catalog.sqlite3").is_file());

    let mut s = Service::start(&cfg(dir.join("moved")), Arc::new(|| {}))
        .expect("start on the moved catalog");
    wait_for(&s, "connection", |s| s.state().connected);
    let _ = s.stop(Duration::from_secs(15));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// VM settings and opening from the command line through a real supervisor
/// without QEMU: change saved (revision 2), `OpenVm` applies the changes and
/// tries the start, refused because QEMU is not configured; non-existent VM
/// reported.
#[test]
fn vm_settings_and_open_vm_without_qemu() {
    use lightvm_desktop::vm_settings::VmChanges;
    use lightvm_proto::{Firmware, GuestOs, NetworkProfile, VmConfig, VmName};
    let Some(exe) = service_executable() else {
        eprintln!("lightvm-service not built: test skipped (cargo build --workspace)");
        return;
    };
    let dir = temp_catalog("settings");
    let cfg = ServiceConfig {
        executable: exe,
        catalog: dir.join("catalog"),
        qemu_dir: None,
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: ParentLossPolicy::default(),
        pairing_dir: Some(dir.join("pairing")),
    };
    let mut s = Service::start(&cfg, Arc::new(|| {})).expect("supervisor start");
    wait_for(&s, "connection", |s| s.state().connected);
    let vm = VmConfig {
        name: VmName::parse("from-command-line").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 2,
        memory_mib: 2048,
        disk_gib: 16,
        network: NetworkProfile::Offline,
        endpoints: Vec::new(),
        install_media: None,
        data_media: None,
        firmware: Firmware::Uefi,
        limits: Default::default(),
        imported: None,
    };
    assert!(s.send(ServiceCommand::Create(Box::new(vm))));
    wait_for(&s, "creation", |s| s.state().vms.len() == 1);
    let v = s.state().vms[0].clone();
    let mut new = v.config.clone();
    new.limits.cpu_max_percent = 30;
    assert!(s.send(ServiceCommand::UpdateVm {
        vm_id: v.vm_id,
        revision: v.config_revision,
        config: Box::new(new),
    }));
    wait_for(&s, "revision 2", |s| {
        s.state()
            .vms
            .first()
            .is_some_and(|v| v.config_revision == 2)
    });
    assert_eq!(s.state().vms[0].config.limits.cpu_max_percent, 30);

    // Command line: changes applied, start refused without QEMU.
    assert!(s.send(ServiceCommand::OpenVm {
        name: "from-command-line".into(),
        changes: VmChanges {
            vcpus: Some(1),
            disk_gib: Some(20),
            storage_budget_gib: Some(Some(10)),
            ..VmChanges::default()
        },
        create: false,
    }));
    wait_for(&s, "revision 3", |s| {
        s.state()
            .vms
            .first()
            .is_some_and(|v| v.config_revision == 3)
    });
    let c = s.state().vms[0].config.clone();
    assert_eq!(
        (c.vcpus, c.disk_gib, c.limits.storage_budget_gib),
        (1, 20, Some(10))
    );
    wait_for(&s, "start outcome", |s| {
        s.state()
            .last_outcome
            .is_some_and(|e| e.starts_with("start") || e.contains("unsupported"))
    });
    eprintln!("outcome: {:?}", s.state().last_outcome);

    // Non-existent VM.
    assert!(s.send(ServiceCommand::OpenVm {
        name: "does-not-exist".into(),
        changes: VmChanges::default(),
        create: false,
    }));
    wait_for(&s, "VM not found", |s| {
        s.state()
            .last_outcome
            .is_some_and(|e| e.contains("not found"))
    });
    // `--create`: VM created with the default values plus the changes.
    assert!(s.send(ServiceCommand::OpenVm {
        name: "created".into(),
        changes: VmChanges {
            memory_mib: Some(1024),
            cpu_max_percent: Some(20),
            ..VmChanges::default()
        },
        create: true,
    }));
    wait_for(&s, "VM created", |s| s.state().vms.len() == 2);
    let created = s
        .state()
        .vms
        .into_iter()
        .find(|v| v.name.as_str() == "created")
        .unwrap();
    assert_eq!(created.config.memory_mib, 1024);
    assert_eq!(created.config.limits.cpu_max_percent, 20);
    let (code, forced) = s.stop(Duration::from_secs(15));
    assert_eq!((code, forced), (Some(0), false));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_executable_is_a_start_error() {
    let dir = temp_catalog("missing");
    let cfg = ServiceConfig {
        executable: dir.join("does-not-exist.exe"),
        catalog: dir.join("catalog"),
        qemu_dir: None,
        runtime_dir: None,
        audit_log: None,
        policy: ParentLossPolicy::default(),
        pairing_dir: Some(dir.join("pairing")),
    };
    match Service::start(&cfg, Arc::new(|| {})) {
        Err(StartError::Spawn(_)) => {}
        other => panic!("expected spawn error: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Variable that makes [`ui_crash_with_real_vm`] run the "UI" part.
const UI_ROLE: &str = "LIGHTVM_P2_UI_ROLE";

fn pid_alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.contains(&format!("\"{pid}\""))
}

/// Direct children of a process: (PID, executable name).
fn children(pid: u32) -> Vec<(u32, String)> {
    let cmd = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={pid}' | \
         ForEach-Object {{ \"$($_.ProcessId) $($_.Name)\" }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let (p, n) = l.trim().split_once(' ')?;
            Some((p.parse().ok()?, n.to_owned()))
        })
        .collect()
}

/// "UI" part of the P-2 test: starts the supervisor with QEMU, creates and
/// starts a VM, prints `READY <service pid>` and waits to be killed.
fn ui_role(qemu_dir: PathBuf) -> ! {
    use lightvm_proto::{Firmware, GuestOs, NetworkProfile, VmConfig, VmName, VmState};
    let exe = service_executable().expect("lightvm-service built");
    let dir = PathBuf::from(std::env::var_os("LIGHTVM_P2_DIR").unwrap());
    let cfg = ServiceConfig {
        executable: exe,
        catalog: dir.join("catalog"),
        qemu_dir: Some(qemu_dir),
        runtime_dir: Some(dir.join("run")),
        audit_log: Some(dir.join("audit.jsonl")),
        policy: p2_policy(),
        pairing_dir: Some(dir.join("pairing")),
    };
    let s = Service::start(&cfg, Arc::new(|| {})).expect("supervisor start");
    wait_for(&s, "connection", |s| s.state().connected);
    let vm = VmConfig {
        name: VmName::parse("p2-crash-ui").unwrap(),
        guest_os: GuestOs::UbuntuLtsX86_64,
        vcpus: 1,
        memory_mib: 1024,
        disk_gib: 8,
        network: NetworkProfile::Offline,
        endpoints: Vec::new(),
        install_media: None,
        data_media: None,
        firmware: Firmware::Uefi,
        limits: Default::default(),
        imported: None,
    };
    assert!(s.send(ServiceCommand::Create(Box::new(vm))));
    wait_for(&s, "creation", |s| s.state().vms.len() == 1);
    let vm_id = s.state().vms[0].vm_id;
    assert!(s.send(ServiceCommand::Start(vm_id)));
    wait_for(&s, "VM running", |s| {
        s.state().vms.first().map(|v| v.state) == Some(VmState::Running)
    });
    println!("READY {}", s.pid());
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    // `Service` is never dropped: the UI "dies" without a controlled stop.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Policy of the P-2 test passed to the test UI (`kill` or `shutdown:N`).
const P2_POLICY: &str = "LIGHTVM_P2_POLICY";

fn p2_policy() -> ParentLossPolicy {
    match std::env::var(P2_POLICY).ok().as_deref() {
        Some(v) if v.starts_with("shutdown:") => ParentLossPolicy::ShutdownVms {
            powerdown_s: v["shutdown:".len()..].parse().unwrap(),
        },
        _ => ParentLossPolicy::KillVms,
    }
}

/// P-2 (`docs/STATUS.md`): crash of the UI that started the supervisor, with
/// a real VM running. The UI is killed with `TerminateProcess`; the
/// supervisor must detect the closing of the life pipe, apply the chosen
/// policy and exit, without orphan QEMU processes. Returns the end times of
/// QEMU and of the service and the relevant audit lines.
fn ui_crash_test(test_name: &str, policy: &str) -> Option<(Duration, Duration, String)> {
    let qemu_dir = std::env::var_os("LIGHTVM_QEMU_DIR").map(PathBuf::from)?;
    if std::env::var_os(UI_ROLE).is_some() {
        ui_role(qemu_dir);
    }
    assert!(
        service_executable().is_some(),
        "lightvm-service not built (cargo build --workspace)"
    );
    let dir = temp_catalog("p2");
    let mut ui = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            test_name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(UI_ROLE, "1")
        .env(P2_POLICY, policy)
        .env("LIGHTVM_P2_DIR", &dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = ui.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for l in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            // libtest prints `test <name> ... ` without a newline before the output.
            if let Some((_, p)) = l.split_once("READY ") {
                let _ = tx.send(p.trim().parse::<u32>().unwrap());
            }
        }
    });
    let svc_pid = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(p) => p,
        Err(e) => {
            // No process left behind if the test UI is not ready.
            let _ = ui.kill();
            let _ = ui.wait();
            panic!("the test UI did not start the VM: {e}");
        }
    };
    let qemu: Vec<u32> = children(svc_pid)
        .into_iter()
        .filter(|(_, n)| n.eq_ignore_ascii_case("qemu-system-x86_64.exe"))
        .map(|(p, _)| p)
        .collect();
    eprintln!(
        "policy {policy}: UI pid {} → service pid {svc_pid} → QEMU {qemu:?}",
        ui.id()
    );
    assert_eq!(qemu.len(), 1, "one QEMU process child of the service");

    let t0 = Instant::now();
    ui.kill().unwrap();
    let _ = ui.wait();
    let mut service_end = None;
    let mut qemu_end = None;
    while t0.elapsed() < Duration::from_secs(120) {
        if qemu_end.is_none() && !pid_alive(qemu[0]) {
            qemu_end = Some(t0.elapsed());
        }
        if service_end.is_none() && !pid_alive(svc_pid) {
            service_end = Some(t0.elapsed());
        }
        if qemu_end.is_some() && service_end.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("after killing the UI: QEMU terminated in {qemu_end:?}, service in {service_end:?}");
    let audit = default_audit(&dir).unwrap_or_default();
    eprintln!("audit:\n{audit}");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        qemu_end.expect("orphan QEMU after the UI crash"),
        service_end.expect("service still alive after the UI crash"),
        audit,
    ))
}

/// Default policy (`kill_vms`): Emergency Stop right after the UI loss, then
/// service stop.
#[test]
#[ignore = "requires a real QEMU with accelerator: set LIGHTVM_QEMU_DIR"]
fn ui_crash_with_real_vm() {
    let Some((qemu, service, audit)) = ui_crash_test("ui_crash_with_real_vm", "kill") else {
        eprintln!("LIGHTVM_QEMU_DIR not set: test skipped");
        return;
    };
    assert!(qemu < Duration::from_secs(5), "QEMU terminated in {qemu:?}");
    assert!(
        service < Duration::from_secs(10),
        "service terminated in {service:?}"
    );
    assert!(
        audit.contains(r#""action":"emergency_stop","outcome":"succeeded""#),
        "{audit}"
    );
}

/// `shutdown_vms` with a 3 s wait: the firmware does not respond to ACPI, so
/// `quit` after 3 s; stop recorded as succeeded, no timeout.
#[test]
#[ignore = "requires a real QEMU with accelerator: set LIGHTVM_QEMU_DIR"]
fn ui_crash_with_real_vm_orderly_shutdown() {
    let Some((qemu, service, audit)) =
        ui_crash_test("ui_crash_with_real_vm_orderly_shutdown", "shutdown:3")
    else {
        eprintln!("LIGHTVM_QEMU_DIR not set: test skipped");
        return;
    };
    assert!(
        qemu < Duration::from_secs(15),
        "QEMU terminated in {qemu:?}"
    );
    assert!(
        service < Duration::from_secs(20),
        "service terminated in {service:?}"
    );
    assert!(
        audit.contains(r#""action":"vm_stop","outcome":"succeeded""#),
        "{audit}"
    );
}

/// Stop audit lines, if the file is in the test catalog.
fn default_audit(dir: &std::path::Path) -> Option<String> {
    let mut lines = Vec::new();
    for e in walk(dir) {
        if e.extension().is_some_and(|x| x == "jsonl") {
            let t = std::fs::read_to_string(&e).ok()?;
            lines.extend(
                t.lines()
                    .filter(|l| {
                        l.contains("vm_stop")
                            || l.contains("emergency_stop")
                            || l.contains("parent")
                    })
                    .map(str::to_owned),
            );
        }
    }
    Some(lines.join("\n"))
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
