//! Test headless dell'IPC e del ciclo di vita del servizio, senza UI.
//! Trasporto reale (named pipe su Windows, socket Unix su Linux); backend e
//! catalogo finti (nessun QEMU).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lightvm_service::client::Client;
use lightvm_service::compat::{
    CloseReason, ErrorCode, ProtocolErrorCode, Request, ResponseBody, Role, ServerMessage,
    ServiceEvent, StopReason, operational_code, protocol_code,
};
use lightvm_service::credential::Credential;
use lightvm_service::frame::FrameError;
use lightvm_service::lock::{InstanceLock, LockError};
use lightvm_service::ratelimit::RateLimit;
use lightvm_service::server::StartError;
use lightvm_service::supervisor::fake::{Calls, FakeBackend, FakeCatalog, Gate};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(10);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    dir: PathBuf,
}

fn config(dir: &std::path::Path) -> ServiceConfig {
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg
}

async fn start_with(
    name: &str,
    tweak: impl FnOnce(&mut ServiceConfig),
    catalog: FakeCatalog,
) -> Svc {
    let dir = temp_dir(name);
    let mut cfg = config(&dir);
    tweak(&mut cfg);
    let backend = FakeBackend::default();
    let calls = backend.calls.clone();
    let (handle, ui) = start(cfg, Box::new(catalog), Box::new(backend))
        .await
        .expect("avvio del servizio");
    Svc {
        handle,
        ui,
        calls,
        dir,
    }
}

async fn start_default(name: &str) -> Svc {
    start_with(name, |_| {}, FakeCatalog::sample()).await
}

async fn connect(svc: &Svc) -> Client {
    tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .expect("timeout di connessione")
        .expect("connessione")
}

async fn connect_ui(svc: &Svc) -> (Client, u64) {
    let mut c = connect(svc).await;
    match c.hello(&svc.ui).await.expect("hello") {
        ServerMessage::HelloOk {
            role: Role::Ui,
            connection_id,
            protocol_version: 1,
            ..
        } => (c, connection_id),
        other => panic!("handshake UI rifiutato: {other:?}"),
    }
}

fn public_error(m: &ServerMessage) -> Option<ErrorCode> {
    match m {
        ServerMessage::Error { error, .. } => operational_code(error),
        _ => None,
    }
}

fn protocol_err(m: &ServerMessage) -> Option<ProtocolErrorCode> {
    match m {
        ServerMessage::Error { error, .. } => protocol_code(error),
        _ => None,
    }
}

async fn expect_closed(c: &mut Client) {
    match tokio::time::timeout(T, c.recv()).await.expect("timeout") {
        Err(FrameError::Closed | FrameError::Io(_)) => {}
        other => panic!("attesa chiusura, ricevuto {other:?}"),
    }
}

/// Acquisisce il lock con un breve margine. Su Linux un `fork` concorrente di
/// un altro test (che avvia processi figli) duplica per un istante il
/// descrittore del lock fino all'`exec` (`O_CLOEXEC`): il lock `flock` resta
/// visibile come occupato per quella finestra.
async fn acquire_eventually(dir: &std::path::Path) {
    let mut last = None;
    for _ in 0..40 {
        match InstanceLock::acquire(dir) {
            Ok(_) => return,
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("lock non rilasciato: {last:?}");
}

/// Riavvio sullo stesso catalogo, con lo stesso margine di `acquire_eventually`.
async fn restart(dir: &std::path::Path) -> (ServiceHandle, Credential) {
    for _ in 0..40 {
        match start(
            config(dir),
            Box::new(FakeCatalog::sample()),
            Box::<FakeBackend>::default(),
        )
        .await
        {
            Ok(x) => return x,
            Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("riavvio: {e}"),
        }
    }
    panic!("lock non rilasciato");
}

fn calls(svc: &Svc) -> Vec<String> {
    svc.calls.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_handshake_and_typed_requests() {
    let svc = start_default("valid").await;
    let (mut c, _) = connect_ui(&svc).await;
    let m = c.request(Request::Ping).await.unwrap();
    assert!(matches!(
        m,
        ServerMessage::Response {
            result: ResponseBody::Pong,
            ..
        }
    ));
    match c.request(Request::GetCapabilities).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::Capabilities(caps),
            ..
        } => {
            assert_eq!(caps.protocol_versions, vec![1]);
            assert_eq!(caps.backend, "fake");
            assert_eq!(caps.accelerator, None);
        }
        other => panic!("{other:?}"),
    }
    match c.request(Request::ListVms).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::VmList { vms },
            ..
        } => assert_eq!(vms.len(), 1),
        other => panic!("{other:?}"),
    }
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_credential_is_denied_and_closed() {
    let svc = start_default("invalid").await;
    let mut c = connect(&svc).await;
    let wrong = Credential::generate().unwrap();
    let m = c.hello(&wrong).await.unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::PermissionDenied));
    expect_closed(&mut c).await;
    // Formato non valido: stesso esito.
    let mut c = connect(&svc).await;
    let m = c.hello_with(vec![1], Some("zz".repeat(32))).await.unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::PermissionDenied));
    expect_closed(&mut c).await;
    assert!(svc.handle.connections().is_empty());
    assert!(calls(&svc).is_empty(), "nessun effetto sul backend");
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_credential_and_request_before_hello_are_denied() {
    let svc = start_default("missing").await;
    let mut c = connect(&svc).await;
    let m = c.hello_with(vec![1], None).await.unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::PermissionDenied));
    expect_closed(&mut c).await;

    let mut c = connect(&svc).await;
    let m = c.request(Request::EmergencyStop).await.unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::PermissionDenied));
    expect_closed(&mut c).await;
    assert!(calls(&svc).is_empty(), "nessun Emergency Stop eseguito");
    assert!(svc.handle.connections().is_empty());
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_protocol_version_is_rejected() {
    let svc = start_default("version").await;
    let mut c = connect(&svc).await;
    let m = c
        .hello_with(vec![0, 2, 99], Some(svc.ui.to_hex()))
        .await
        .unwrap();
    assert_eq!(
        protocol_err(&m),
        Some(ProtocolErrorCode::UnsupportedVersion)
    );
    expect_closed(&mut c).await;
    // Offerta multipla con una versione comune: negoziata la 1.
    let mut c = connect(&svc).await;
    let m = c
        .hello_with(vec![7, 1], Some(svc.ui.to_hex()))
        .await
        .unwrap();
    assert!(matches!(
        m,
        ServerMessage::HelloOk {
            protocol_version: 1,
            ..
        }
    ));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_frame_is_rejected_before_and_after_handshake() {
    let svc = start_with("frame", |c| c.max_frame = 4096, FakeCatalog::sample()).await;
    // Prima dell'handshake: solo l'intestazione, il payload non viene letto.
    let mut c = connect(&svc).await;
    c.send_raw(&(1u32 << 20).to_be_bytes()).await.unwrap();
    let m = tokio::time::timeout(T, c.recv()).await.unwrap().unwrap();
    assert_eq!(protocol_err(&m), Some(ProtocolErrorCode::MessageTooLarge));
    expect_closed(&mut c).await;
    // Dopo l'handshake.
    let (mut c, _) = connect_ui(&svc).await;
    let mut big = (8192u32).to_be_bytes().to_vec();
    big.extend(std::iter::repeat_n(b' ', 8192));
    c.send_raw(&big).await.unwrap();
    let m = tokio::time::timeout(T, c.recv()).await.unwrap().unwrap();
    assert_eq!(protocol_err(&m), Some(ProtocolErrorCode::MessageTooLarge));
    expect_closed(&mut c).await;
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_message_is_a_protocol_error_and_connection_survives() {
    let svc = start_default("malformed").await;
    let (mut c, _) = connect_ui(&svc).await;
    let payload = br#"{"type":"request","request_id":5,"body":{"op":"qmp_execute"}}"#;
    let mut f = (payload.len() as u32).to_be_bytes().to_vec();
    f.extend_from_slice(payload);
    c.send_raw(&f).await.unwrap();
    match c.recv().await.unwrap() {
        ServerMessage::Error { error, .. } => assert_eq!(
            protocol_code(&error),
            Some(ProtocolErrorCode::MalformedMessage)
        ),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        c.request(Request::Ping).await.unwrap(),
        ServerMessage::Response {
            result: ResponseBody::Pong,
            ..
        }
    ));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_with_different_roles() {
    let svc = start_default("two").await;
    let (mut ui, ui_id) = connect_ui(&svc).await;
    let pairing = match ui.request(Request::CreatePairingCredential).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::PairingCredential { credential, .. },
            ..
        } => Credential::from_hex(&credential).unwrap(),
        other => panic!("{other:?}"),
    };
    let mut p = connect(&svc).await;
    let p_id = match p.hello(&pairing).await.unwrap() {
        ServerMessage::HelloOk {
            role: Role::Pairing,
            connection_id,
            ..
        } => connection_id,
        other => panic!("{other:?}"),
    };
    assert_ne!(ui_id, p_id);
    // Il pairing non conferisce permessi VM (ADR-0006).
    assert!(matches!(
        p.request(Request::Ping).await.unwrap(),
        ServerMessage::Response {
            result: ResponseBody::Pong,
            ..
        }
    ));
    for req in [
        Request::ListVms,
        Request::Subscribe,
        Request::EmergencyStop,
        Request::Shutdown,
        Request::CreatePairingCredential,
        Request::RevokeConnection {
            connection_id: ui_id,
        },
    ] {
        let m = p.request(req.clone()).await.unwrap();
        assert_eq!(
            public_error(&m),
            Some(ErrorCode::PermissionDenied),
            "{req:?}"
        );
    }
    match ui.request(Request::ListConnections).await.unwrap() {
        ServerMessage::Response {
            result: ResponseBody::Connections { connections },
            ..
        } => {
            assert_eq!(connections.len(), 2);
            assert!(connections.iter().all(|c| c.peer_pid.is_some()));
        }
        other => panic!("{other:?}"),
    }
    assert!(calls(&svc).is_empty());
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_connection_closes_it_and_invalidates_the_credential() {
    let svc = start_default("revoke").await;
    let (mut ui, _) = connect_ui(&svc).await;
    assert!(matches!(
        ui.request(Request::Subscribe).await.unwrap(),
        ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }
    ));
    let pairing = svc.handle.issue_pairing_credential().unwrap();
    let mut p = connect(&svc).await;
    let p_id = match p.hello(&pairing).await.unwrap() {
        ServerMessage::HelloOk { connection_id, .. } => connection_id,
        other => panic!("{other:?}"),
    };
    match ui
        .request(Request::RevokeConnection {
            connection_id: p_id,
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::Revoked { connection_id },
            ..
        } => assert_eq!(connection_id, p_id),
        other => panic!("{other:?}"),
    }
    // Il client revocato riceve `closing` e la chiusura.
    match tokio::time::timeout(T, p.recv()).await.unwrap().unwrap() {
        ServerMessage::Closing {
            reason: CloseReason::Revoked,
        } => {}
        other => panic!("{other:?}"),
    }
    expect_closed(&mut p).await;
    // L'iscritto riceve l'evento.
    match tokio::time::timeout(T, ui.next_event())
        .await
        .unwrap()
        .unwrap()
    {
        Ok(ServiceEvent::ConnectionRevoked { connection_id }) => assert_eq!(connection_id, p_id),
        other => panic!("{other:?}"),
    }
    // La credenziale revocata non è più accettata.
    let mut p2 = connect(&svc).await;
    let m = p2.hello(&pairing).await.unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::PermissionDenied));
    // Revoca di una connessione inesistente.
    let m = ui
        .request(Request::RevokeConnection {
            connection_id: 9999,
        })
        .await
        .unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::StateConflict));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_instance_on_same_catalog_is_rejected() {
    let svc = start_default("single").await;
    let backend = FakeBackend::default();
    let r = start(
        config(&svc.dir),
        Box::new(FakeCatalog::sample()),
        Box::new(backend),
    )
    .await;
    match r {
        Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {}
        other => panic!("attesa AlreadyRunning: {other:?}"),
    }
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    // Dopo l'arresto il lock è libero.
    let (h, _) = restart(&dir).await;
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_is_not_valid_after_restart() {
    let svc = start_default("restart").await;
    let old = svc.ui.clone();
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    let (h, _new) = restart(&dir).await;
    let mut c = Client::connect(h.endpoint()).await.unwrap();
    let m = c.hello(&old).await.unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::PermissionDenied));
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_per_connection() {
    let svc = start_with(
        "rate",
        |c| {
            c.rate_limit = RateLimit {
                burst: 3,
                per_second: 0.5,
            }
        },
        FakeCatalog::sample(),
    )
    .await;
    let (mut c, _) = connect_ui(&svc).await;
    let mut ok = 0;
    let mut limited = 0;
    for _ in 0..6 {
        match c.request(Request::Ping).await.unwrap() {
            ServerMessage::Response { .. } => ok += 1,
            m => {
                assert_eq!(public_error(&m), Some(ErrorCode::ResourceLimit));
                limited += 1;
            }
        }
    }
    assert_eq!(ok, 3);
    assert_eq!(limited, 3);
    // Un'altra connessione ha il proprio bucket.
    let (mut c2, _) = connect_ui(&svc).await;
    assert!(matches!(
        c2.request(Request::Ping).await.unwrap(),
        ServerMessage::Response { .. }
    ));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_queue_and_emergency_stop_does_not_wait_for_it() {
    let gate = Gate::closed();
    let catalog = FakeCatalog {
        gate: Some(gate.clone()),
        ..FakeCatalog::sample()
    };
    let svc = Arc::new(start_with("queue", |c| c.queue_capacity = 1, catalog).await);
    // A blocca il supervisor dentro `list_vms` e occupa l'unico posto in coda.
    let (mut a, _) = connect_ui(&svc).await;
    let a_task = tokio::spawn(async move { a.request(Request::ListVms).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    // B: coda piena → resource_limit, senza attendere.
    let (mut b, _) = connect_ui(&svc).await;
    let m = tokio::time::timeout(Duration::from_secs(2), b.request(Request::Ping))
        .await
        .expect("la coda piena non deve bloccare")
        .unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::ResourceLimit));
    // Un client di pairing attivo verrà revocato dall'Emergency Stop.
    let pairing = svc.handle.issue_pairing_credential().unwrap();
    let mut p = connect(&svc).await;
    assert!(matches!(
        p.hello(&pairing).await.unwrap(),
        ServerMessage::HelloOk { .. }
    ));
    // C: Emergency Stop con il supervisor bloccato → completato subito.
    let (mut c, _) = connect_ui(&svc).await;
    let started = std::time::Instant::now();
    let m = tokio::time::timeout(Duration::from_secs(2), c.request(Request::EmergencyStop))
        .await
        .expect("l'Emergency Stop non deve attendere la coda")
        .unwrap();
    let elapsed = started.elapsed();
    match m {
        ServerMessage::Response {
            result: ResponseBody::EmergencyStop(r),
            ..
        } => {
            assert_eq!(r.revoked_connections, 1);
            assert!(r.completed);
        }
        other => panic!("{other:?}"),
    }
    eprintln!("emergency_stop con supervisor bloccato: {elapsed:?}");
    assert!(calls(&svc).contains(&"emergency_stop_all".to_string()));
    match tokio::time::timeout(T, p.recv()).await.unwrap().unwrap() {
        ServerMessage::Closing {
            reason: CloseReason::Revoked,
        } => {}
        other => panic!("{other:?}"),
    }
    // Sblocco: A riceve la risposta.
    gate.open();
    match tokio::time::timeout(T, a_task).await.unwrap().unwrap() {
        ServerMessage::Response {
            result: ResponseBody::VmList { .. },
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    let svc = Arc::try_unwrap(svc).ok().expect("unico riferimento");
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_limit_and_handshake_timeout() {
    let svc = start_with(
        "limits",
        |c| {
            c.max_connections = 1;
            c.handshake_timeout = Duration::from_millis(300);
        },
        FakeCatalog::sample(),
    )
    .await;
    let (_a, _) = connect_ui(&svc).await;
    let mut b = connect(&svc).await;
    let m = tokio::time::timeout(T, b.recv()).await.unwrap().unwrap();
    assert_eq!(public_error(&m), Some(ErrorCode::ResourceLimit));
    drop(_a);
    // Attesa del rilascio del posto, poi un client muto viene chiuso al timeout.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut mute = connect(&svc).await;
    expect_closed(&mut mute).await;
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn controlled_shutdown_via_ipc_closes_connections_and_releases_lock() {
    let svc = start_default("shutdown").await;
    let (mut other, _) = connect_ui(&svc).await;
    let (mut c, _) = connect_ui(&svc).await;
    assert!(matches!(
        c.request(Request::Shutdown).await.unwrap(),
        ServerMessage::Response {
            result: ResponseBody::ShuttingDown,
            ..
        }
    ));
    match tokio::time::timeout(T, other.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Closing {
            reason: CloseReason::ServiceStopping,
        } => {}
        other => panic!("{other:?}"),
    }
    tokio::time::timeout(T, svc.handle.wait())
        .await
        .expect("arresto entro il timeout");
    let calls = calls(&svc);
    assert!(calls.contains(&"stop_agent_sessions:Shutdown".to_string()));
    assert_eq!(calls.last().map(String::as_str), Some("shutdown"));
    acquire_eventually(&svc.dir).await;
    // Nuove connessioni rifiutate.
    assert!(Client::connect(svc.handle.endpoint()).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn parent_lost_in_process_stops_agent_sessions_then_shuts_down() {
    let svc = start_default("parent-inproc").await;
    let (mut ui, _) = connect_ui(&svc).await;
    ui.request(Request::Subscribe).await.unwrap();
    let pairing = svc.handle.issue_pairing_credential().unwrap();
    let mut p = connect(&svc).await;
    p.hello(&pairing).await.unwrap();
    svc.handle.parent_lost().await;
    tokio::time::timeout(T, svc.handle.wait()).await.unwrap();
    let calls = calls(&svc);
    assert_eq!(calls[0], "stop_agent_sessions:ParentLost");
    assert_eq!(calls[1], "on_parent_lost:ShutdownService");
    // Il client di pairing è stato revocato prima dell'arresto.
    match tokio::time::timeout(T, p.recv()).await.unwrap().unwrap() {
        ServerMessage::Closing {
            reason: CloseReason::Revoked,
        } => {}
        other => panic!("{other:?}"),
    }
    let mut saw_agents_stopped = false;
    while let Ok(Ok(Ok(ev))) = tokio::time::timeout(T, ui.next_event()).await {
        if ev
            == (ServiceEvent::AgentSessionsStopped {
                reason: StopReason::ParentLost,
            })
        {
            saw_agents_stopped = true;
        }
    }
    assert!(saw_agents_stopped);
}

// --- Test sul binario reale -------------------------------------------------

mod binary {
    use super::*;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::{Child, Command};

    struct Spawned {
        child: Child,
        endpoint: String,
        credential: Credential,
    }

    async fn spawn_service(dir: &std::path::Path) -> Spawned {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lightvm-service"))
            .arg("--catalog")
            .arg(dir)
            .arg("--runtime-dir")
            .arg(dir.join("run"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("avvio del binario");
        let out = child.stdout.take().unwrap();
        let mut lines = BufReader::new(out).lines();
        let line = tokio::time::timeout(T, lines.next_line())
            .await
            .expect("timeout della riga ready")
            .unwrap()
            .expect("riga ready");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "ready");
        Spawned {
            child,
            endpoint: v["endpoint"].as_str().unwrap().to_owned(),
            credential: Credential::from_hex(v["credential"].as_str().unwrap()).unwrap(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn second_binary_instance_exits_with_clear_error() {
        let dir = temp_dir("bin-single");
        let mut first = spawn_service(&dir).await;
        let out = Command::new(env!("CARGO_BIN_EXE_lightvm-service"))
            .arg("--catalog")
            .arg(&dir)
            .arg("--runtime-dir")
            .arg(dir.join("run"))
            .stdin(Stdio::piped())
            .output()
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(3));
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("seconda istanza: {}", stderr.trim());
        assert!(stderr.contains("un'altra istanza"), "{stderr}");
        assert!(out.stdout.is_empty(), "nessuna credenziale emessa");
        drop(first.child.stdin.take());
        let st = tokio::time::timeout(T, first.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(st.success());
    }

    /// Morte del genitore simulata: la chiusura dello stdin equivale, per il
    /// servizio, alla chiusura dell'handle da parte del sistema operativo alla
    /// morte della UI.
    #[tokio::test(flavor = "multi_thread")]
    async fn parent_death_closes_sessions_and_releases_lock() {
        let dir = temp_dir("bin-parent");
        let mut s = spawn_service(&dir).await;
        let mut ui = Client::connect(&s.endpoint).await.unwrap();
        assert!(matches!(
            ui.hello(&s.credential).await.unwrap(),
            ServerMessage::HelloOk { .. }
        ));
        ui.request(Request::Subscribe).await.unwrap();
        let started = std::time::Instant::now();
        drop(s.child.stdin.take());
        let mut events = Vec::new();
        let mut closing = None;
        loop {
            match tokio::time::timeout(T, ui.recv()).await.unwrap() {
                Ok(ServerMessage::Event { event }) => events.push(event),
                Ok(ServerMessage::Closing { reason }) => closing = Some(reason),
                Ok(other) => panic!("{other:?}"),
                Err(_) => break,
            }
        }
        let st = tokio::time::timeout(T, s.child.wait())
            .await
            .unwrap()
            .unwrap();
        eprintln!(
            "genitore perso → uscita del servizio in {:?}, eventi {events:?}",
            started.elapsed()
        );
        assert!(st.success());
        assert!(events.contains(&ServiceEvent::AgentSessionsStopped {
            reason: StopReason::ParentLost
        }));
        assert_eq!(closing, Some(CloseReason::ServiceStopping));
        acquire_eventually(&dir).await;
    }

    /// Kill del servizio (crash): il lock del catalogo si rilascia comunque.
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_released_when_service_is_killed() {
        let dir = temp_dir("bin-kill");
        let mut s = spawn_service(&dir).await;
        assert!(matches!(
            InstanceLock::acquire(&dir),
            Err(LockError::AlreadyRunning { .. })
        ));
        s.child.kill().await.unwrap();
        acquire_eventually(&dir).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_command_on_stdin() {
        let dir = temp_dir("bin-stdin-shutdown");
        let mut s = spawn_service(&dir).await;
        use tokio::io::AsyncWriteExt;
        let stdin = s.child.stdin.as_mut().unwrap();
        stdin.write_all(b"shutdown\n").await.unwrap();
        stdin.flush().await.unwrap();
        let st = tokio::time::timeout(T, s.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(st.success());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_arguments_exit_2_without_secrets() {
        let out = Command::new(env!("CARGO_BIN_EXE_lightvm-service"))
            .arg("--token")
            .arg("abc")
            .output()
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
}
