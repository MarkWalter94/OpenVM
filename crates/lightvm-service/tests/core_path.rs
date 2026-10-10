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
