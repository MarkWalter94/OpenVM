//! Test headless del percorso reale del supervisor, senza UI e senza QEMU:
//! IPC reale → `vm_core::Core` → catalogo SQLite di `vm-storage` in una
//! directory temporanea → audit su file JSON Lines; backend delle VM finto
//! (dichiarato: nessun processo, nessun boot).

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightvm_proto::ipc::{Request, ResponseBody, Role, ServerMessage, ServiceEvent};
use lightvm_proto::{
    Actor, AuditAction, AuditEvent, AuditOutcome, ErrorCode, OperationId, OperationStatus,
    ReplyError, VmAction, VmId, VmRequest, VmState,
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
}

fn audit_path(dir: &Path) -> PathBuf {
    dir.join("audit").join("audit.jsonl")
}

fn config(dir: &Path) -> ServiceConfig {
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg
}

/// Servizio con catalogo SQLite e audit su file reali, backend delle VM finto.
async fn start_sqlite(dir: &Path) -> Svc {
    for _ in 0..40 {
        let store = match SqliteStore::open(dir) {
            Ok(s) => s,
            Err(vm_storage::StorageError::CatalogLocked(_)) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("apertura del catalogo: {e}"),
        };
        let audit = FileAuditSink::open(&audit_path(dir)).expect("audit su file");
        let calls = Calls::default();
        let backend = FakeVmBackend {
            calls: calls.clone(),
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
            Ok((handle, ui)) => return Svc { handle, ui, calls },
            Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("avvio: {e}"),
        }
    }
    panic!("catalogo non rilasciato");
}

async fn connect_ui(svc: &Svc) -> Client {
    let mut c = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .expect("timeout")
        .expect("connessione");
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
        .expect("file di audit")
        .lines()
        .map(|l| serde_json::from_str(l).expect("riga di audit = AuditEvent"))
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

    // Creazione valida.
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
    // Diniego del core: configurazione non valida (nessun effetto).
    let mut bad = sample_config("non-valida");
    bad.vcpus = 0;
    let m = ui.request(Request::CreateVm { config: bad }).await.unwrap();
    assert_eq!(error_code(&m), Some(ErrorCode::InvalidArgument));

    // Avvio → running; arresto → stopped.
    expect_succeeded(&submit(&mut ui, vm_id, VmAction::Start).await);
    assert_eq!(state_of(&mut ui, vm_id).await, VmState::Running);
    expect_succeeded(&submit(&mut ui, vm_id, VmAction::Stop).await);
    assert_eq!(state_of(&mut ui, vm_id).await, VmState::Stopped);

    // Diniego del core: arresto di una VM spenta.
    let m = submit(&mut ui, vm_id, VmAction::Stop).await;
    assert_eq!(error_code(&m), Some(ErrorCode::StateConflict));

    // Emergency Stop con la VM in esecuzione: percorso prioritario, poi il
    // core registra l'arresto (asincrono rispetto alla risposta).
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
    assert!(stopped, "VM non arrestata dopo l'Emergency Stop");

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

    // Eventi di stato pubblicati sul canale.
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

    // Audit su file: una riga JSON per evento, `seq` strettamente crescente.
    let events = read_audit(&dir);
    for w in events.windows(2) {
        assert!(w[1].seq > w[0].seq, "seq non crescente: {w:?}");
    }
    assert!(
        events
            .iter()
            .all(|e| e.actor != Actor::System || e.action == AuditAction::VmStateObserved)
    );
    let create_ok = events
        .iter()
        .find(|e| e.action == AuditAction::VmCreate && e.outcome == AuditOutcome::Succeeded)
        .expect("creazione registrata");
    assert_eq!(create_ok.vm_id, Some(vm_id));
    let create_denied = events
        .iter()
        .find(|e| e.action == AuditAction::VmCreate && e.outcome == AuditOutcome::Denied)
        .expect("diniego di creazione registrato");
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
        .expect("diniego di arresto registrato");
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
    // Nessun segreto nel file: la credenziale UI non compare.
    let raw = std::fs::read_to_string(audit_path(&dir)).unwrap();
    assert!(!raw.contains(&svc.ui.to_hex()));

    // Riavvio sullo stesso catalogo: la VM è persistita in SQLite, spenta;
    // l'audit prosegue in aggiunta nello stesso file.
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
    // Arresto controllato del servizio: le VM in esecuzione vengono arrestate
    // tramite il core (audit dell'arresto).
    svc2.handle.shutdown().await;
    let after = read_audit(&dir);
    assert!(after.len() > before);
    assert!(
        after[before..]
            .iter()
            .any(|e| e.action == AuditAction::VmStop && e.outcome == AuditOutcome::Succeeded),
        "arresto all'uscita non registrato"
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
    };
    let mut c = connect_ui(&svc).await;
    let vm_id = match c
        .request(Request::CreateVm {
            config: sample_config("senza-qemu"),
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
        .expect("diniego registrato");
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
            config: sample_config("vietata"),
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

/// End-to-end reale e senza UI: handshake → crea VM → avvia (QEMU reale con
/// acceleratore, nessun ripiego TCG) → stato → arresta → Emergency Stop.
/// Richiede `LIGHTVM_E2E_QEMU_DIR` con la distribuzione QEMU approvata
/// (hash in `qemu_backend::probe`); la VM ha il disco principale qcow2 creato
/// dal servizio al primo avvio (vuoto), nessuna ISO, e resta nel firmware UEFI. Eseguire con `cargo test -p lightvm-service --test core_path
/// -- --ignored --nocapture`.
#[test]
#[ignore = "richiede QEMU approvato e un acceleratore (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_create_start_status_stop() {
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR non impostata"),
    );
    let dir = temp_dir("e2e-qemu");
    // Radice corta per i socket QMP/VNC (limite di 107 byte).
    let private_root = std::env::temp_dir().join(format!("lvm-e2e-{}", std::process::id()));
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir, store.root(), private_root);
    // Il firmware UEFI senza sistema operativo non risponde a ACPI powerdown:
    // attesa breve, poi `quit`.
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(3),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let t0 = std::time::Instant::now();
    let (host, vm_backend, capacity) = probe(setup).expect("probe di QEMU");
    eprintln!("probe completato in {:?}: {capacity:?}", t0.elapsed());
    assert!(
        capacity.accelerator_available,
        "acceleratore non disponibile"
    );
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
        };
        let mut c = connect_ui(&svc).await;
        match c.request(Request::GetCapabilities).await.unwrap() {
            ServerMessage::Response {
                result: ResponseBody::Capabilities(caps),
                ..
            } => eprintln!("capacità: {caps:?}"),
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
        eprintln!("avvio: {:?} → {m:?}", t.elapsed());
        expect_succeeded(&m);
        assert_eq!(state_of(&mut c, vm_id).await, VmState::Running);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(state_of(&mut c, vm_id).await, VmState::Running);
        let t = std::time::Instant::now();
        let m = submit(&mut c, vm_id, VmAction::Stop).await;
        eprintln!("arresto: {:?} → {m:?}", t.elapsed());
        expect_succeeded(&m);
        assert_eq!(state_of(&mut c, vm_id).await, VmState::Stopped);
        let disk = dir
            .join("vms")
            .join(vm_id.to_string())
            .join("primary.qcow2");
        let size = std::fs::metadata(&disk)
            .unwrap_or_else(|e| panic!("disco principale assente {}: {e}", disk.display()))
            .len();
        eprintln!("disco principale: {} ({size} byte)", disk.display());

        // Secondo avvio (stesso disco) ed Emergency Stop sul processo reale.
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
        assert!(stopped, "VM non arrestata dopo l'Emergency Stop");
        svc.handle.shutdown().await;
        let events = read_audit(&dir);
        assert!(has(&events, AuditAction::VmStart, AuditOutcome::Succeeded));
        assert!(has(&events, AuditAction::VmStop, AuditOutcome::Succeeded));
        assert!(has(
            &events,
            AuditAction::EmergencyStop,
            AuditOutcome::Succeeded
        ));
        eprintln!("righe di audit: {}", events.len());
    });
}

/// Impostazioni della VM via IPC (ADR-0017): risorse e limiti di una VM spenta,
/// revisione attesa, persistenza in SQLite dopo il riavvio, audit
/// `vm_configure`; ruolo di pairing senza permesso.
#[tokio::test(flavor = "multi_thread")]
async fn update_vm_via_ipc_is_persisted_and_audited() {
    use lightvm_proto::VmLimits;
    let dir = temp_dir("core-update-vm");
    let svc = start_sqlite(&dir).await;
    let mut ui = connect_ui(&svc).await;
    let vm = match ui
        .request(Request::CreateVm {
            config: sample_config("impostazioni"),
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
    // Stessa revisione una seconda volta: conflitto, nessun effetto.
    let m = ui
        .request(Request::UpdateVm {
            vm_id: vm.id,
            expected_revision: 1,
            config: vm.config.clone(),
        })
        .await
        .unwrap();
    assert_eq!(error_code(&m), Some(ErrorCode::StateConflict));
    // Il ruolo di pairing non modifica le VM.
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

    // Dopo il riavvio la configurazione viene da SQLite.
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

/// Processi `qemu-system-x86_64.exe` figli di questo processo (il backend
/// gira nel servizio in-process): PID.
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

/// Criterio di uscita F1 attraverso il servizio (non solo le API del
/// backend): IPC → core → SQLite → `qemu-backend` → QEMU reale, almeno 20
/// cicli start/stop sulla stessa VM (disco principale creato al primo avvio),
/// arresto ordinario alternato con `quit` diretto, Emergency Stop nell'ultimo
/// ciclo, nessun processo QEMU residuo dopo ogni ciclo, audit completo.
/// `LIGHTVM_E2E_QEMU_DIR` obbligatoria; `LIGHTVM_F1_CYCLES` facoltativa (20).
#[cfg(windows)]
#[test]
#[ignore = "richiede QEMU approvato e un acceleratore (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_cicli_attraverso_il_servizio() {
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR non impostata"),
    );
    let cycles: usize = std::env::var("LIGHTVM_F1_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let dir = temp_dir("e2e-cicli");
    let private_root = std::env::temp_dir().join(format!("lvm-cic-{}", std::process::id()));
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir, store.root(), private_root);
    // Il firmware non risponde all'ACPI: attesa breve, poi `quit`.
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(1),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let (host, vm_backend, capacity) = probe(setup).expect("probe di QEMU");
    assert!(
        capacity.accelerator_available,
        "acceleratore non disponibile"
    );
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
        };
        let mut c = connect_ui(&svc).await;
        let mut cfg = sample_config("e2e-cicli");
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
        let mut avvii = Vec::new();
        let mut arresti = Vec::new();
        for i in 1..=cycles {
            let t = std::time::Instant::now();
            expect_succeeded(&submit(&mut c, vm_id, VmAction::Start).await);
            let t_avvio = t.elapsed();
            assert_eq!(state_of(&mut c, vm_id).await, VmState::Running, "ciclo {i}");
            let figli = qemu_children();
            assert_eq!(figli.len(), 1, "ciclo {i}: processi QEMU {figli:?}");
            tokio::time::sleep(Duration::from_millis(500)).await;
            let t = std::time::Instant::now();
            let modo = if i == cycles {
                match c.request(Request::EmergencyStop).await.unwrap() {
                    ServerMessage::Response {
                        result: ResponseBody::EmergencyStop(r),
                        ..
                    } => {
                        assert!(r.completed, "ciclo {i}: {r:?}");
                        assert_eq!(r.processes_terminated, 1, "ciclo {i}");
                    }
                    other => panic!("{other:?}"),
                }
                "emergency_stop"
            } else if i % 2 == 0 {
                expect_succeeded(&submit(&mut c, vm_id, VmAction::Stop).await);
                "stop"
            } else {
                expect_succeeded(&submit(&mut c, vm_id, VmAction::ForceStop).await);
                "force_stop"
            };
            let mut fermo = false;
            for _ in 0..200 {
                if state_of(&mut c, vm_id).await == VmState::Stopped {
                    fermo = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let t_arresto = t.elapsed();
            assert!(fermo, "ciclo {i}: VM non arrestata ({modo})");
            let residui = qemu_children();
            assert!(residui.is_empty(), "ciclo {i}: QEMU residui {residui:?}");
            eprintln!(
                "ciclo {i}: avvio {:.1} ms, {modo} {:.1} ms, pid {:?}, residui 0",
                t_avvio.as_secs_f64() * 1000.0,
                t_arresto.as_secs_f64() * 1000.0,
                figli
            );
            avvii.push(t_avvio.as_secs_f64() * 1000.0);
            arresti.push((modo, t_arresto.as_secs_f64() * 1000.0));
        }
        let disk = dir2.join("vms").join(vm_id.to_string()).join("primary.qcow2");
        assert!(disk.exists(), "disco principale assente");
        svc.handle.shutdown().await;
        let events = read_audit(&dir2);
        let n = |a: AuditAction| {
            events
                .iter()
                .filter(|e| e.action == a && e.outcome == AuditOutcome::Succeeded)
                .count()
        };
        avvii.sort_by(f64::total_cmp);
        eprintln!(
            "RIEPILOGO {cycles} cicli attraverso il servizio: avvio ms min {:.1} mediana {:.1} max {:.1}; \
             audit succeeded: vm_start {}, vm_stop (anche force_stop) {}, emergency_stop {}; arresti {arresti:?}",
            avvii[0],
            avvii[avvii.len() / 2],
            avvii[avvii.len() - 1],
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
        "QEMU residui dopo l'arresto del servizio"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ADR-0017 attraverso il servizio con QEMU reale: modifica via IPC di una VM
/// spenta (disco 8 → 10 GiB, tetto CPU, margine di memoria), riavvio con il
/// disco ingrandito da `qemu-img resize`; budget di storage superato → avvio
/// rifiutato con `resource_limit` senza processi QEMU.
#[cfg(windows)]
#[test]
#[ignore = "richiede QEMU approvato e un acceleratore (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_impostazioni_vm() {
    use lightvm_proto::VmLimits;
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR non impostata"),
    );
    let dir = temp_dir("e2e-impostazioni");
    let private_root = std::env::temp_dir().join(format!("lvm-imp-{}", std::process::id()));
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir.clone(), store.root(), private_root);
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(1),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let (host, vm_backend, capacity) = probe(setup).expect("probe di QEMU");
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
        };
        let mut c = connect_ui(&svc).await;
        let mut cfg = sample_config("e2e-impostazioni");
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
        // Primo avvio: disco creato a 8 GiB.
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

        // Impostazioni: disco 10 GiB, tetto CPU 25 %, margine 768 MiB.
        let mut nuova = vm.config.clone();
        nuova.disk_gib = 10;
        nuova.limits = VmLimits {
            cpu_max_percent: 25,
            memory_margin_mib: 768,
            storage_budget_gib: None,
        };
        let vm2 = match c
            .request(Request::UpdateVm {
                vm_id: vm.id,
                expected_revision: 1,
                config: nuova.clone(),
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
        eprintln!("avvio con limiti {:?}: in esecuzione", nuova.limits);
        expect_succeeded(&submit(&mut c, vm.id, VmAction::ForceStop).await);
        assert_eq!(size(&disk_mp), 10 << 30);
        eprintln!("disco ingrandito: {} byte", size(&disk_mp));

        // Budget di storage di 1 GiB superato da un file di 2 GiB nella VM.
        let mut con_budget = nuova.clone();
        con_budget.limits.storage_budget_gib = Some(1);
        match c
            .request(Request::UpdateVm {
                vm_id: vm.id,
                expected_revision: vm2.config_revision,
                config: con_budget,
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
        let f = std::fs::File::create(extra.join("riempimento.bin")).unwrap();
        f.set_len(2 << 30).unwrap();
        drop(f);
        let m = submit(&mut c, vm.id, VmAction::Start).await;
        eprintln!("avvio con budget superato: {m:?}");
        let rifiutato = match &m {
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
        assert!(rifiutato, "{m:?}");
        assert!(qemu_children().is_empty());
        std::fs::remove_file(extra.join("riempimento.bin")).unwrap();
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
