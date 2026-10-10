//! Headless tests of the real supervisor path, without UI and without QEMU:
//! real IPC → `vm_core::Core` → `vm-storage` SQLite catalog in a temporary
//! directory → audit on a JSON Lines file; fake VM backend (declared: no
//! process, no boot).

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightvm_proto::ipc::{Request, ResponseBody, Role, ServerMessage, ServiceEvent};
use lightvm_proto::{
    Actor, AuditAction, AuditEvent, AuditOutcome, BackendObservation, ErrorCode, OperationId,
    OperationStatus, ReplyError, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{
    UnavailableBackend, UnavailableHost, unavailable_capacity,
};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{Calls, FakeHost, FakeVmBackend, host_capacity, sample_config};
use lightvm_service::lock::LockError;
use lightvm_service::server::StartError;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(10);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    /// Observations of the fake backend (simulated process exits).
    observations: Observations,
}

type Observations = std::sync::Arc<std::sync::Mutex<Vec<(VmId, BackendObservation)>>>;

fn audit_path(dir: &Path) -> PathBuf {
    dir.join("audit").join("audit.jsonl")
}

fn config(dir: &Path) -> ServiceConfig {
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg
}

/// Service with real SQLite catalog and file audit, fake VM backend.
async fn start_sqlite(dir: &Path) -> Svc {
    for _ in 0..40 {
        let store = match SqliteStore::open(dir) {
            Ok(s) => s,
            Err(vm_storage::StorageError::CatalogLocked(_)) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("opening the catalog: {e}"),
        };
        let audit = FileAuditSink::open(&audit_path(dir)).expect("file audit");
        let calls = Calls::default();
        let observations = Observations::default();
        let backend = FakeVmBackend {
            calls: calls.clone(),
            observations: observations.clone(),
            ..FakeVmBackend::default()
        };
        let inspector = ManagedMediaInspector::new(store.root());
        let engine = CoreEngine::new(
            store,
            audit,
            backend,
            inspector,
            core_config(host_capacity()),
        )
        .expect("core");
        let host = FakeHost {
            calls: calls.clone(),
        };
        match start(config(dir), Box::new(engine), Box::new(host)).await {
            Ok((handle, ui)) => {
                return Svc {
                    handle,
                    ui,
                    calls,
                    observations,
                };
            }
            Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("start: {e}"),
        }
    }
    panic!("catalog not released");
}

async fn connect_ui(svc: &Svc) -> Client {
    let mut c = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .expect("timeout")
        .expect("connection");
    match c.hello(&svc.ui).await.expect("hello") {
        ServerMessage::HelloOk { role: Role::Ui, .. } => c,
        other => panic!("{other:?}"),
    }
}

fn error_code(m: &ServerMessage) -> Option<ErrorCode> {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => Some(p.code),
        _ => None,
    }
}

async fn submit(c: &mut Client, vm_id: VmId, action: VmAction) -> ServerMessage {
    c.request(Request::SubmitVm {
        request: VmRequest {
            operation_id: OperationId::new_v7(),
            vm_id,
            expected_run_id: None,
            action,
        },
    })
    .await
    .unwrap()
}

fn expect_succeeded(m: &ServerMessage) {
    match m {
        ServerMessage::Response {
            result:
                ResponseBody::Operation {
                    operation: Some(op),
                    ..
                },
            ..
        } => assert_eq!(op.status, OperationStatus::Succeeded, "{op:?}"),
        other => panic!("{other:?}"),
    }
}

/// Graceful stop accepted: completed (blocking backend) or forwarded (the
/// QEMU backend stops in the background; the end is observed later).
fn expect_accepted_stop(m: &ServerMessage) {
    match m {
        ServerMessage::Response {
            result:
                ResponseBody::Operation {
                    operation: Some(op),
                    ..
                },
            ..
        } => assert!(
            matches!(
                op.status,
                OperationStatus::Succeeded | OperationStatus::Forwarded
            ),
            "{op:?}"
        ),
        other => panic!("{other:?}"),
    }
}

/// Waits (up to 3 minutes) for the VM to be `stopped`.
async fn wait_stopped(c: &mut Client, vm_id: VmId) {
    for _ in 0..1800 {
        if state_of(c, vm_id).await == VmState::Stopped {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("VM not stopped");
}

async fn state_of(c: &mut Client, vm_id: VmId) -> VmState {
    match c.request(Request::VmStatus { vm_id }).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm },
            ..
        } => vm.state,
        other => panic!("{other:?}"),
    }
}

fn read_audit(dir: &Path) -> Vec<AuditEvent> {
    std::fs::read_to_string(audit_path(dir))
        .expect("audit file")
        .lines()
        .map(|l| serde_json::from_str(l).expect("audit line = AuditEvent"))
        .collect()
}

fn has(events: &[AuditEvent], action: AuditAction, outcome: AuditOutcome) -> bool {
    events
        .iter()
        .any(|e| e.action == action && e.outcome == outcome)
}

#[tokio::test(flavor = "multi_thread")]
async fn create_start_stop_emergency_and_denials_are_persisted_and_audited() {
    let dir = temp_dir("core-sqlite");
    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    assert!(matches!(
        ui.request(Request::Subscribe).await.unwrap(),
        ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }
    ));

    // Valid creation.
    let vm_id = match ui
        .request(Request::CreateVm {
            config: sample_config("prova-f1"),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => {
            assert_eq!(vm.persisted_state, VmState::Stopped);
            vm.id
        }
        other => panic!("{other:?}"),
    };
    // Core denial: invalid configuration (no effect).
    let mut bad = sample_config("invalid");
    bad.vcpus = 0;
    let m = ui.request(Request::CreateVm { config: bad }).await.unwrap();
    assert_eq!(error_code(&m), Some(ErrorCode::InvalidArgument));

    // Start → running; stop → stopped.
    expect_succeeded(&submit(&mut ui, vm_id, VmAction::Start).await);
    assert_eq!(state_of(&mut ui, vm_id).await, VmState::Running);
    expect_succeeded(&submit(&mut ui, vm_id, VmAction::Stop).await);
    assert_eq!(state_of(&mut ui, vm_id).await, VmState::Stopped);

    // Core denial: stopping a powered-off VM.
    let m = submit(&mut ui, vm_id, VmAction::Stop).await;
    assert_eq!(error_code(&m), Some(ErrorCode::StateConflict));

    // Emergency Stop with the VM running: priority path, then the core
    // records the stop (asynchronous with respect to the response).
    expect_succeeded(&submit(&mut ui, vm_id, VmAction::Start).await);
    match ui.request(Request::EmergencyStop).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::EmergencyStop(r),
            ..
        } => assert!(r.completed),
        other => panic!("{other:?}"),
    }
    let mut stopped = false;
    for _ in 0..100 {
        if state_of(&mut ui, vm_id).await == VmState::Stopped {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(stopped, "VM not stopped after the Emergency Stop");

    let calls = svc.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            "vm_start:prova-f1:media=0".to_string(),
            "vm_shutdown".into(),
            "vm_start:prova-f1:media=0".into(),
            "emergency_stop_all".into(),
            "vm_force_stop".into(),
        ]
    );

    // State events published on the channel.
    let mut states = Vec::new();
    while let Ok(Ok(Ok(ev))) =
        tokio::time::timeout(Duration::from_millis(200), ui.next_event()).await
    {
        if let ServiceEvent::VmState { vm_id: v, state } = ev {
            assert_eq!(v, vm_id);
            states.push(state);
        }
    }
    assert!(states.contains(&VmState::Running), "{states:?}");
    assert_eq!(states.last(), Some(&VmState::Stopped), "{states:?}");

    svc.handle.shutdown().await;

    // File audit: one JSON line per event, `seq` strictly increasing.
    let events = read_audit(&dir);
    for w in events.windows(2) {
        assert!(w[1].seq > w[0].seq, "seq not increasing: {w:?}");
    }
    assert!(
        events
            .iter()
            .all(|e| e.actor != Actor::System || e.action == AuditAction::VmStateObserved)
    );
    let create_ok = events
        .iter()
        .find(|e| e.action == AuditAction::VmCreate && e.outcome == AuditOutcome::Succeeded)
        .expect("creation recorded");
    assert_eq!(create_ok.vm_id, Some(vm_id));
    let create_denied = events
        .iter()
        .find(|e| e.action == AuditAction::VmCreate && e.outcome == AuditOutcome::Denied)
        .expect("creation denial recorded");
    assert_eq!(create_denied.error, Some(ErrorCode::InvalidArgument));
    for outcome in [
        AuditOutcome::Accepted,
        AuditOutcome::Forwarded,
        AuditOutcome::Succeeded,
    ] {
        assert!(
            has(&events, AuditAction::VmStart, outcome),
            "vm_start {outcome:?}"
        );
        assert!(
            has(&events, AuditAction::VmStop, outcome),
            "vm_stop {outcome:?}"
        );
    }
    let stop_denied = events
        .iter()
        .find(|e| e.action == AuditAction::VmStop && e.outcome == AuditOutcome::Denied)
        .expect("stop denial recorded");
    assert_eq!(stop_denied.error, Some(ErrorCode::StateConflict));
    assert!(has(
        &events,
        AuditAction::EmergencyStop,
        AuditOutcome::Accepted
    ));
    assert!(has(
        &events,
        AuditAction::EmergencyStop,
        AuditOutcome::Succeeded
    ));
    // No secret in the file: the UI credential does not appear.
    let raw = std::fs::read_to_string(audit_path(&dir)).unwrap();
    assert!(!raw.contains(&svc.ui.to_hex()));

    // Restart on the same catalog: the VM is persisted in SQLite, powered off;
    // the audit continues appending to the same file.
    let before = events.len();
    let svc2 = start_sqlite(&dir).await;
    let mut ui2 = connect_ui(&svc2).await;
    match ui2.request(Request::ListVms).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::VmList { vms },
            ..
        } => {
            assert_eq!(vms.len(), 1);
            assert_eq!(vms[0].vm_id, vm_id);
            assert_eq!(vms[0].name.as_str(), "prova-f1");
            assert_eq!(vms[0].state, VmState::Stopped);
        }
        other => panic!("{other:?}"),
    }
    expect_succeeded(&submit(&mut ui2, vm_id, VmAction::Start).await);
    // Controlled service shutdown: the running VMs are stopped through the
    // core (stop audit).
    svc2.handle.shutdown().await;
    let after = read_audit(&dir);
    assert!(after.len() > before);
    assert!(
        after[before..]
            .iter()
            .any(|e| e.action == AuditAction::VmStop && e.outcome == AuditOutcome::Succeeded),
        "stop at exit not recorded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_qemu_start_is_unsupported_and_audited() {
    let dir = temp_dir("core-noqemu");
    let store = SqliteStore::open(&dir).unwrap();
    let audit = FileAuditSink::open(&audit_path(&dir)).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine = CoreEngine::new(
        store,
        audit,
        UnavailableBackend,
        inspector,
        core_config(unavailable_capacity(256)),
    )
    .unwrap();
    let (handle, ui) = start(config(&dir), Box::new(engine), Box::new(UnavailableHost))
        .await
        .unwrap();
    let svc = Svc {
        handle,
        ui,
        calls: Calls::default(),
        observations: Observations::default(),
    };
    let mut c = connect_ui(&svc).await;
    let vm_id = match c
        .request(Request::CreateVm {
            config: sample_config("without-qemu"),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm.id,
        other => panic!("{other:?}"),
    };
    let m = submit(&mut c, vm_id, VmAction::Start).await;
    assert_eq!(error_code(&m), Some(ErrorCode::Unsupported));
    assert_eq!(state_of(&mut c, vm_id).await, VmState::Stopped);
    svc.handle.shutdown().await;
    let events = read_audit(&dir);
    let denied = events
        .iter()
        .find(|e| e.action == AuditAction::VmStart && e.outcome == AuditOutcome::Denied)
        .expect("denial recorded");
    assert_eq!(denied.error, Some(ErrorCode::Unsupported));
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_role_cannot_touch_vms() {
    let dir = temp_dir("core-pairing");
    let svc = start_sqlite(&dir).await;
    let pairing = svc.handle.issue_pairing_credential().unwrap();
    let mut p = Client::connect(svc.handle.endpoint()).await.unwrap();
    assert!(matches!(
        p.hello(&pairing).await.unwrap(),
        ServerMessage::HelloOk {
            role: Role::Pairing,
            ..
        }
    ));
    for req in [
        Request::CreateVm {
            config: sample_config("forbidden"),
        },
        Request::VmStatus {
            vm_id: VmId::new_v7(),
        },
    ] {
        let m = p.request(req).await.unwrap();
        assert_eq!(error_code(&m), Some(ErrorCode::PermissionDenied));
    }
    svc.handle.shutdown().await;
    assert!(
        svc.calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| !c.starts_with("vm_"))
    );
}

/// Real end-to-end without UI: handshake → create VM → start (real QEMU with
/// an accelerator, no TCG fallback) → status → stop → Emergency Stop.
/// Requires `LIGHTVM_E2E_QEMU_DIR` with the approved QEMU distribution
/// (hashes in `qemu_backend::probe`); the VM has the primary qcow2 disk created
/// by the service on first start (empty), no ISO, and stays in the UEFI
/// firmware. Run with `cargo test -p lightvm-service --test core_path
/// -- --ignored --nocapture`.
#[test]
#[ignore = "requires approved QEMU and an accelerator (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_create_start_status_stop() {
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"),
    );
    let dir = temp_dir("e2e-qemu");
    // Short root for the QMP/VNC sockets (107-byte limit).
    let private_root = std::env::temp_dir().join(format!("lvm-e2e-{}", std::process::id()));
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir, store.root(), private_root);
    // The UEFI firmware without an operating system does not respond to ACPI
    // powerdown: short wait, then `quit`.
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(3),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let t0 = std::time::Instant::now();
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    eprintln!("probe completed in {:?}: {capacity:?}", t0.elapsed());
    assert!(capacity.accelerator_available, "accelerator not available");
    let audit = FileAuditSink::open(&audit_path(&dir)).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let (handle, ui) = start(config(&dir), Box::new(engine), Box::new(host))
            .await
            .unwrap();
        let svc = Svc {
            handle,
            ui,
            calls: Calls::default(),
            observations: Observations::default(),
        };
        let mut c = connect_ui(&svc).await;
        match c.request(Request::GetCapabilities).await.unwrap() {
            ServerMessage::Response {
                result: ResponseBody::Capabilities(caps),
                ..
            } => eprintln!("capabilities: {caps:?}"),
            other => panic!("{other:?}"),
        }
        let mut cfg = sample_config("e2e-qemu");
        cfg.vcpus = 1;
        cfg.memory_mib = 1024;
        let vm_id = match c.request(Request::CreateVm { config: cfg }).await.unwrap() {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm.id,
            other => panic!("{other:?}"),
        };
        let t = std::time::Instant::now();
        let m = submit(&mut c, vm_id, VmAction::Start).await;
        eprintln!("start: {:?} → {m:?}", t.elapsed());
        expect_succeeded(&m);
        assert_eq!(state_of(&mut c, vm_id).await, VmState::Running);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(state_of(&mut c, vm_id).await, VmState::Running);
        let t = std::time::Instant::now();
        let m = submit(&mut c, vm_id, VmAction::Stop).await;
        expect_accepted_stop(&m);
        wait_stopped(&mut c, vm_id).await;
        eprintln!("stop: {:?} → {m:?}", t.elapsed());
        let disk = dir
            .join("vms")
            .join(vm_id.to_string())
            .join("primary.qcow2");
        let size = std::fs::metadata(&disk)
            .unwrap_or_else(|e| panic!("primary disk missing {}: {e}", disk.display()))
            .len();
        eprintln!("primary disk: {} ({size} bytes)", disk.display());

        // Second start (same disk) and Emergency Stop on the real process.
        expect_succeeded(&submit(&mut c, vm_id, VmAction::Start).await);
        let t = std::time::Instant::now();
        match c.request(Request::EmergencyStop).await.unwrap() {
            ServerMessage::Response {
                result: ResponseBody::EmergencyStop(r),
                ..
            } => {
                eprintln!("Emergency Stop: {:?} → {r:?}", t.elapsed());
                assert!(r.completed);
                assert_eq!(r.processes_terminated, 1);
            }
            other => panic!("{other:?}"),
        }
        let mut stopped = false;
        for _ in 0..200 {
            if state_of(&mut c, vm_id).await == VmState::Stopped {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(stopped, "VM not stopped after the Emergency Stop");
        svc.handle.shutdown().await;
        let events = read_audit(&dir);
        assert!(has(&events, AuditAction::VmStart, AuditOutcome::Succeeded));
        assert!(has(&events, AuditAction::VmStop, AuditOutcome::Succeeded));
        assert!(has(
            &events,
            AuditAction::EmergencyStop,
            AuditOutcome::Succeeded
        ));
        eprintln!("audit lines: {}", events.len());
    });
}

/// VM settings via IPC (ADR-0017): resources and limits of a powered-off VM,
/// expected revision, persistence in SQLite after restart, `vm_configure`
/// audit; pairing role without permission.
#[tokio::test(flavor = "multi_thread")]
async fn update_vm_via_ipc_is_persisted_and_audited() {
    use lightvm_proto::VmLimits;
    let dir = temp_dir("core-update-vm");
    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    let vm = match ui
        .request(Request::CreateVm {
            config: sample_config("settings"),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    };
    let mut cfg = vm.config.clone();
    cfg.vcpus = 1;
    cfg.disk_gib += 4;
    cfg.limits = VmLimits {
        cpu_max_percent: 40,
        memory_margin_mib: 768,
        storage_budget_gib: Some(12),
    };
    match ui
        .request(Request::UpdateVm {
            vm_id: vm.id,
            expected_revision: 1,
            config: cfg.clone(),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::VmUpdated { vm },
            ..
        } => assert_eq!((vm.config_revision, &vm.config), (2, &cfg)),
        other => panic!("{other:?}"),
    }
    // Same revision a second time: conflict, no effect.
    let m = ui
        .request(Request::UpdateVm {
            vm_id: vm.id,
            expected_revision: 1,
            config: vm.config.clone(),
        })
        .await
        .unwrap();
    assert_eq!(error_code(&m), Some(ErrorCode::StateConflict));
    // The pairing role does not modify VMs.
    let pairing = svc.handle.issue_pairing_credential().unwrap();
    let mut p = Client::connect(svc.handle.endpoint()).await.unwrap();
    p.hello(&pairing).await.unwrap();
    let m = p
        .request(Request::UpdateVm {
            vm_id: vm.id,
            expected_revision: 2,
            config: vm.config.clone(),
        })
        .await
        .unwrap();
    assert_eq!(error_code(&m), Some(ErrorCode::PermissionDenied));
    drop(p);
    drop(ui);
    svc.handle.shutdown().await;

    // After the restart the configuration comes from SQLite.
    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    match ui
        .request(Request::VmStatus { vm_id: vm.id })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm: s },
            ..
        } => assert_eq!((s.config_revision, s.config), (2, cfg)),
        other => panic!("{other:?}"),
    }
    svc.handle.shutdown().await;
    let events = read_audit(&dir);
    assert!(has(
        &events,
        AuditAction::VmConfigure,
        AuditOutcome::Succeeded
    ));
    assert!(has(&events, AuditAction::VmConfigure, AuditOutcome::Denied));
}

/// `qemu-system-x86_64.exe` processes that are children of this process (the
/// backend runs in the in-process service): PIDs.
#[cfg(windows)]
fn qemu_children() -> Vec<u32> {
    let cmd = format!(
        "Get-CimInstance Win32_Process -Filter \"ParentProcessId={} and Name='qemu-system-x86_64.exe'\" | \
         ForEach-Object {{ $_.ProcessId }}",
        std::process::id()
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines().filter_map(|l| l.trim().parse().ok()).collect()
}

/// F1 exit criterion through the service (not just the backend APIs):
/// IPC → core → SQLite → `qemu-backend` → real QEMU, at least 20 start/stop
/// cycles on the same VM (primary disk created on first start), ordinary stop
/// alternated with a direct `quit`, Emergency Stop in the last cycle, no QEMU
/// process left after each cycle, complete audit.
/// `LIGHTVM_E2E_QEMU_DIR` required; `LIGHTVM_F1_CYCLES` optional (20).
#[cfg(windows)]
#[test]
#[ignore = "requires approved QEMU and an accelerator (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_cycles_through_the_service() {
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"),
    );
    let cycles: usize = std::env::var("LIGHTVM_F1_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let dir = temp_dir("e2e-cycles");
    let private_root = std::env::temp_dir().join(format!("lvm-cyc-{}", std::process::id()));
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir, store.root(), private_root);
    // The firmware does not respond to ACPI: short wait, then `quit`.
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(1),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(capacity.accelerator_available, "accelerator not available");
    let audit = FileAuditSink::open(&audit_path(&dir)).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir2 = dir.clone();
    rt.block_on(async move {
        let (handle, ui) = start(config(&dir2), Box::new(engine), Box::new(host))
            .await
            .unwrap();
        let svc = Svc {
            handle,
            ui,
            calls: Calls::default(),
            observations: Observations::default(),
        };
        let mut c = connect_ui(&svc).await;
        let mut cfg = sample_config("e2e-cycles");
        cfg.vcpus = 1;
        cfg.memory_mib = 1024;
        cfg.disk_gib = 8;
        let vm_id = match c.request(Request::CreateVm { config: cfg }).await.unwrap() {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm.id,
            other => panic!("{other:?}"),
        };
        let mut starts = Vec::new();
        let mut stops = Vec::new();
        for i in 1..=cycles {
            let t = std::time::Instant::now();
            expect_succeeded(&submit(&mut c, vm_id, VmAction::Start).await);
            let t_start = t.elapsed();
            assert_eq!(state_of(&mut c, vm_id).await, VmState::Running, "cycle {i}");
            let children = qemu_children();
            assert_eq!(children.len(), 1, "cycle {i}: QEMU processes {children:?}");
            tokio::time::sleep(Duration::from_millis(500)).await;
            let t = std::time::Instant::now();
            let mode = if i == cycles {
                match c.request(Request::EmergencyStop).await.unwrap() {
                    ServerMessage::Response {
                        result: ResponseBody::EmergencyStop(r),
                        ..
                    } => {
                        assert!(r.completed, "cycle {i}: {r:?}");
                        assert_eq!(r.processes_terminated, 1, "cycle {i}");
                    }
                    other => panic!("{other:?}"),
                }
                "emergency_stop"
            } else if i % 2 == 0 {
                expect_accepted_stop(&submit(&mut c, vm_id, VmAction::Stop).await);
                "stop"
            } else {
                expect_succeeded(&submit(&mut c, vm_id, VmAction::ForceStop).await);
                "force_stop"
            };
            let mut halted = false;
            for _ in 0..200 {
                if state_of(&mut c, vm_id).await == VmState::Stopped {
                    halted = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let t_stop = t.elapsed();
            assert!(halted, "cycle {i}: VM not stopped ({mode})");
            let leftover = qemu_children();
            assert!(leftover.is_empty(), "cycle {i}: leftover QEMU {leftover:?}");
            eprintln!(
                "cycle {i}: start {:.1} ms, {mode} {:.1} ms, pid {:?}, leftover 0",
                t_start.as_secs_f64() * 1000.0,
                t_stop.as_secs_f64() * 1000.0,
                children
            );
            starts.push(t_start.as_secs_f64() * 1000.0);
            stops.push((mode, t_stop.as_secs_f64() * 1000.0));
        }
        let disk = dir2.join("vms").join(vm_id.to_string()).join("primary.qcow2");
        assert!(disk.exists(), "primary disk missing");
        svc.handle.shutdown().await;
        let events = read_audit(&dir2);
        let n = |a: AuditAction| {
            events
                .iter()
                .filter(|e| e.action == a && e.outcome == AuditOutcome::Succeeded)
                .count()
        };
        starts.sort_by(f64::total_cmp);
        eprintln!(
            "SUMMARY {cycles} cycles through the service: start ms min {:.1} median {:.1} max {:.1}; \
             audit succeeded: vm_start {}, vm_stop (including force_stop) {}, emergency_stop {}; stops {stops:?}",
            starts[0],
            starts[starts.len() / 2],
            starts[starts.len() - 1],
            n(AuditAction::VmStart),
            n(AuditAction::VmStop),
            n(AuditAction::EmergencyStop),
        );
        assert_eq!(n(AuditAction::VmStart), cycles);
        assert_eq!(n(AuditAction::VmStop), cycles - 1);
        assert!(cycles >= 20 || std::env::var("LIGHTVM_F1_CYCLES").is_ok());
    });
    assert!(
        qemu_children().is_empty(),
        "leftover QEMU after the service shutdown"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ADR-0017 through the service with real QEMU: change via IPC of a powered-off
/// VM (disk 8 → 10 GiB, CPU cap, memory margin), restart with the disk grown
/// by `qemu-img resize`; storage budget exceeded → start rejected with
/// `resource_limit` without QEMU processes.
#[cfg(windows)]
#[test]
#[ignore = "requires approved QEMU and an accelerator (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_vm_settings() {
    use lightvm_proto::VmLimits;
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"),
    );
    let dir = temp_dir("e2e-settings");
    let private_root = std::env::temp_dir().join(format!("lvm-set-{}", std::process::id()));
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir.clone(), store.root(), private_root);
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(1),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    let audit = FileAuditSink::open(&audit_path(&dir)).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir2 = dir.clone();
    rt.block_on(async move {
        let (handle, ui) = start(config(&dir2), Box::new(engine), Box::new(host))
            .await
            .unwrap();
        let svc = Svc {
            handle,
            ui,
            calls: Calls::default(),
            observations: Observations::default(),
        };
        let mut c = connect_ui(&svc).await;
        let mut cfg = sample_config("e2e-settings");
        cfg.vcpus = 2;
        cfg.memory_mib = 1024;
        cfg.disk_gib = 8;
        let vm = match c.request(Request::CreateVm { config: cfg }).await.unwrap() {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm,
            other => panic!("{other:?}"),
        };
        // First start: disk created at 8 GiB.
        expect_succeeded(&submit(&mut c, vm.id, VmAction::Start).await);
        expect_succeeded(&submit(&mut c, vm.id, VmAction::ForceStop).await);
        let disk = dir2
            .join("vms")
            .join(vm.id.to_string())
            .join("primary.qcow2");
        let qemu_img = qemu_backend::ManagedPath::verify(qemu_dir.join("qemu-img.exe")).unwrap();
        let disk_mp = qemu_backend::ManagedPath::verify(&disk).unwrap();
        let size = |m: &qemu_backend::ManagedPath| {
            qemu_backend::prepare::disk_virtual_size(
                &qemu_img,
                m,
                qemu_backend::cmdline::DiskFormat::Qcow2,
                Duration::from_secs(30),
            )
            .unwrap()
        };
        assert_eq!(size(&disk_mp), 8 << 30);

        // Settings: disk 10 GiB, CPU cap 25 %, margin 768 MiB.
        let mut updated = vm.config.clone();
        updated.disk_gib = 10;
        updated.limits = VmLimits {
            cpu_max_percent: 25,
            memory_margin_mib: 768,
            storage_budget_gib: None,
        };
        let vm2 = match c
            .request(Request::UpdateVm {
                vm_id: vm.id,
                expected_revision: 1,
                config: updated.clone(),
            })
            .await
            .unwrap()
        {
            ServerMessage::Response {
                result: ResponseBody::VmUpdated { vm },
                ..
            } => vm,
            other => panic!("{other:?}"),
        };
        expect_succeeded(&submit(&mut c, vm.id, VmAction::Start).await);
        assert_eq!(state_of(&mut c, vm.id).await, VmState::Running);
        assert_eq!(qemu_children().len(), 1);
        eprintln!("start with limits {:?}: running", updated.limits);
        expect_succeeded(&submit(&mut c, vm.id, VmAction::ForceStop).await);
        assert_eq!(size(&disk_mp), 10 << 30);
        eprintln!("disk grown: {} bytes", size(&disk_mp));

        // 1 GiB storage budget exceeded by a 2 GiB file in the VM.
        let mut with_budget = updated.clone();
        with_budget.limits.storage_budget_gib = Some(1);
        match c
            .request(Request::UpdateVm {
                vm_id: vm.id,
                expected_revision: vm2.config_revision,
                config: with_budget,
            })
            .await
            .unwrap()
        {
            ServerMessage::Response {
                result: ResponseBody::VmUpdated { .. },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        let extra = dir2.join("vms").join(vm.id.to_string()).join("overlays");
        std::fs::create_dir_all(&extra).unwrap();
        let f = std::fs::File::create(extra.join("filler.bin")).unwrap();
        f.set_len(2 << 30).unwrap();
        drop(f);
        let m = submit(&mut c, vm.id, VmAction::Start).await;
        eprintln!("start with budget exceeded: {m:?}");
        let rejected = match &m {
            ServerMessage::Response {
                result:
                    ResponseBody::Operation {
                        operation: Some(op),
                        ..
                    },
                ..
            } => op.status == OperationStatus::Failed && op.error == Some(ErrorCode::ResourceLimit),
            other => error_code(other) == Some(ErrorCode::ResourceLimit),
        };
        assert!(rejected, "{m:?}");
        assert!(qemu_children().is_empty());
        std::fs::remove_file(extra.join("filler.bin")).unwrap();
        svc.handle.shutdown().await;
        let events = read_audit(&dir2);
        assert!(has(
            &events,
            AuditAction::VmConfigure,
            AuditOutcome::Succeeded
        ));
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// Restart after a supervisor terminated with a running VM: in the catalog the
/// VM is `running` with an open run. The new service (no surviving process)
/// brings it back to `stopped` and closes the run.
#[tokio::test(flavor = "multi_thread")]
async fn restart_reconciles_running_vm_and_closes_open_run() {
    let dir = temp_dir("core-restart");
    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    let vm_id = match ui
        .request(Request::CreateVm {
            config: sample_config("restart"),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm.id,
        other => panic!("{other:?}"),
    };
    drop(ui);
    svc.handle.shutdown().await;

    // "Crash": VM running with an open run, written directly into the catalog.
    {
        let mut cat = vm_storage::Catalog::open(&dir).unwrap();
        cat.with_tx(|tx| {
            let sid = tx.vm_ids()?[0];
            tx.set_vm_lifecycle(&sid, vm_storage::Lifecycle::Running)?;
            tx.insert_run(&vm_storage::RunRecord {
                run_id: vm_storage::RunId::new_v7(),
                vm_id: sid,
                baseline_id: None,
                overlay_disk_id: None,
                vars_fw_id: None,
                state: vm_storage::RunState::Running,
                started_at: vm_storage::now_rfc3339(),
                ended_at: None,
                termination_cause: None,
            })
        })
        .unwrap();
    }

    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    assert_eq!(state_of(&mut ui, vm_id).await, VmState::Stopped);
    drop(ui);
    svc.handle.shutdown().await;
    let mut cat = vm_storage::Catalog::open(&dir).unwrap();
    let open = cat
        .read(|tx| {
            let sid = tx.vm_ids()?[0];
            tx.open_runs(&sid)
        })
        .unwrap();
    assert!(open.is_empty(), "runs still open: {open:?}");
}

/// A process that exits on its own (shutdown from the guest, crash) produces
/// the `vm_state` event without any client making requests.
#[tokio::test(flavor = "multi_thread")]
async fn unrequested_exit_is_published_without_requests() {
    let dir = temp_dir("core-exit");
    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    let vm_id = match ui
        .request(Request::CreateVm {
            config: sample_config("exits-on-its-own"),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm.id,
        other => panic!("{other:?}"),
    };
    expect_succeeded(&submit(&mut ui, vm_id, VmAction::Start).await);
    let run_id = match ui.request(Request::VmStatus { vm_id }).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm },
            ..
        } => vm.run_id.expect("active run"),
        other => panic!("{other:?}"),
    };
    assert!(matches!(
        ui.request(Request::Subscribe).await.unwrap(),
        ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }
    ));
    svc.observations.lock().unwrap().push((
        vm_id,
        BackendObservation::ProcessExited {
            run_id,
            expected: false,
        },
    ));
    let mut seen = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen.is_none() && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(Ok(ServiceEvent::VmState { vm_id: v, state }))) =
            tokio::time::timeout(Duration::from_millis(500), ui.next_event()).await
            && v == vm_id
            && !state.has_process()
        {
            seen = Some(state);
        }
    }
    assert!(seen.is_some(), "no event after the process exit");
    svc.handle.shutdown().await;
}
