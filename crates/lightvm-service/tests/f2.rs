//! F2 requests through the real IPC: media registry, VM deletion, console
//! ticket with the per-run display proxy, input from the console, roles.
//! Real core; in-memory or SQLite catalog; fake VM backend whose display is a
//! fake RFB server on a Unix socket (no QEMU).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{ConsoleTicket, Request, ResponseBody, Role, ServerMessage};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, DisplayGeometry, ErrorCode, InputBatch, InputEvent,
    KeyAction, MediaKind, OperationId, OperationStatus, PointerMove, QCode, ReplyError, RunId,
    VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FAKE_GEOMETRY, FakeHost, FakeVmBackend, MemoryAudit, StopResults, host_capacity,
    sample_config, sample_engine,
};
use lightvm_service::lock::LockError;
use lightvm_service::server::StartError;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};
use socket2::{Domain, SockAddr, Socket, Type};

const T: Duration = Duration::from_secs(10);
const GREETING: &[u8; 12] = b"RFB 003.008\n";

// ----- fake RFB server -------------------------------------------------------

/// Fake QEMU display: sends the RFB greeting, then echoes. Counts the
/// connections it accepted.
struct FakeVnc {
    path: PathBuf,
    accepted: Arc<AtomicUsize>,
}

fn short_dir(prefix: &str) -> PathBuf {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    let d = std::env::temp_dir().join(format!(
        "{prefix}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3]
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fake_vnc() -> FakeVnc {
    let path = short_dir("lvv").join("v.sock");
    let l = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
    l.bind(&SockAddr::unix(&path).unwrap()).unwrap();
    l.listen(8).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let acc = accepted.clone();
    std::thread::spawn(move || {
        while let Ok((s, _)) = l.accept() {
            acc.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                if (&s).write_all(GREETING).is_err() {
                    return;
                }
                let mut buf = [0u8; 4096];
                loop {
                    match (&s).read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if (&s).write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = s.shutdown(std::net::Shutdown::Write);
            });
        }
    });
    FakeVnc { path, accepted }
}

/// Viewer side: connects to the ticket socket and sends a line.
fn viewer(path: &str, line: &[u8]) -> std::io::Result<Socket> {
    let s = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    s.connect(&SockAddr::unix(path)?)?;
    (&s).write_all(line)?;
    Ok(s)
}

fn token_line(t: &ConsoleTicket) -> Vec<u8> {
    format!("{}\n", t.token).into_bytes()
}

/// Reads up to `n` bytes until EOF or `n`; returns what was read.
fn read_some(s: &Socket, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; n];
    while out.len() < n {
        match (&*s).read(&mut buf[..n - out.len()]) {
            Ok(0) | Err(_) => break,
            Ok(k) => out.extend_from_slice(&buf[..k]),
        }
    }
    out
}

/// `true` if the stream reaches EOF (or an error) within `d`.
fn closed_within(s: &Socket, d: Duration) -> bool {
    let start = Instant::now();
    let mut buf = [0u8; 256];
    // Data already queued (greeting, echoes) is drained first.
    while start.elapsed() < d {
        match (&*s).read(&mut buf) {
            Ok(0) | Err(_) => return true,
            Ok(_) => {}
        }
    }
    false
}

async fn blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    tokio::time::timeout(T, tokio::task::spawn_blocking(f))
        .await
        .expect("timeout")
        .unwrap()
}

// ----- service ----------------------------------------------------------------

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
}

fn config(dir: &Path) -> ServiceConfig {
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.display_dir = Some(short_dir("lvd"));
    cfg
}

async fn start_memory(name: &str, display: Option<PathBuf>) -> Svc {
    start_memory_with(name, display, None).await
}

/// With `stops`, graceful stops run in the background (as with QEMU) and
/// their outcome is injected by the test.
async fn start_memory_with(
    name: &str,
    display: Option<PathBuf>,
    stops: Option<StopResults>,
) -> Svc {
    let dir = temp_dir(name);
    let calls = Calls::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        display_socket: display,
        async_stop: stops.is_some(),
        stop_results: stops.unwrap_or_default(),
        ..FakeVmBackend::default()
    };
    let engine = sample_engine(backend, MemoryAudit::default());
    let host = FakeHost {
        calls: calls.clone(),
    };
    let (handle, ui) = start(config(&dir), Box::new(engine), Box::new(host))
        .await
        .expect("start");
    Svc { handle, ui, calls }
}

fn audit_path(dir: &Path) -> PathBuf {
    dir.join("audit").join("audit.jsonl")
}

async fn start_sqlite(dir: &Path) -> Svc {
    for _ in 0..40 {
        let store = match SqliteStore::open(dir) {
            Ok(s) => s,
            Err(vm_storage::StorageError::CatalogLocked(_)) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("catalog: {e}"),
        };
        let audit = FileAuditSink::open(&audit_path(dir)).unwrap();
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
        .unwrap();
        let host = FakeHost {
            calls: calls.clone(),
        };
        match start(config(dir), Box::new(engine), Box::new(host)).await {
            Ok((handle, ui)) => return Svc { handle, ui, calls },
            Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("start: {e}"),
        }
    }
    panic!("catalog not released");
}

async fn connect_ui(svc: &Svc) -> (Client, u64) {
    let mut c = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .unwrap()
        .unwrap();
    match c.hello(&svc.ui).await.unwrap() {
        ServerMessage::HelloOk {
            role: Role::Ui,
            connection_id,
            ..
        } => (c, connection_id),
        other => panic!("{other:?}"),
    }
}

fn code(m: &ServerMessage) -> Option<ErrorCode> {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => Some(p.code),
        _ => None,
    }
}

fn detail(m: &ServerMessage, key: &str) -> Option<String> {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => p.details.as_ref().and_then(|d| d.get(key)).cloned(),
        _ => None,
    }
}

async fn req(c: &mut Client, r: Request) -> ServerMessage {
    tokio::time::timeout(T, c.request(r))
        .await
        .expect("timeout")
        .unwrap()
}

async fn submit(
    c: &mut Client,
    vm_id: VmId,
    run: Option<RunId>,
    action: VmAction,
) -> ServerMessage {
    req(
        c,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id,
                expected_run_id: run,
                action,
            },
        },
    )
    .await
}

fn succeeded(m: &ServerMessage) {
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

async fn sample_vm(c: &mut Client) -> (VmId, u64) {
    match req(c, Request::ListVms).await {
        ServerMessage::Response {
            result: ResponseBody::VmList { vms },
            ..
        } => (vms[0].vm_id, vms[0].config_revision),
        other => panic!("{other:?}"),
    }
}

async fn status(c: &mut Client, vm_id: VmId) -> lightvm_proto::ipc::VmSummary {
    match req(c, Request::VmStatus { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    }
}

async fn open_console(c: &mut Client, vm_id: VmId) -> ConsoleTicket {
    match req(c, Request::OpenConsole { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Console { ticket },
            ..
        } => ticket,
        other => panic!("{other:?}"),
    }
}

async fn start_vm(c: &mut Client, vm_id: VmId) -> RunId {
    succeeded(&submit(c, vm_id, None, VmAction::Start).await);
    status(c, vm_id).await.run_id.expect("run")
}

fn calls(svc: &Svc) -> Vec<String> {
    svc.calls.lock().unwrap().clone()
}

/// Waits until the ticket socket no longer accepts the token.
async fn ticket_refused(t: &ConsoleTicket) -> bool {
    let path = t.socket_path.clone();
    let line = token_line(t);
    blocking(move || match viewer(&path, &line) {
        Err(_) => true,
        Ok(s) => read_some(&s, GREETING.len()).is_empty(),
    })
    .await
}

// ----- tests ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn f2_requests_are_refused_for_the_pairing_role() {
    let svc = start_memory("f2-pairing", None).await;
    let (mut ui, _) = connect_ui(&svc).await;
    let (vm_id, rev) = sample_vm(&mut ui).await;
    let pairing = svc.handle.issue_pairing_credential().unwrap();
    let mut p = Client::connect(svc.handle.endpoint()).await.unwrap();
    assert!(matches!(
        p.hello(&pairing).await.unwrap(),
        ServerMessage::HelloOk {
            role: Role::Pairing,
            ..
        }
    ));
    for r in [
        Request::ListMedia,
        Request::ImportMedia {
            source_path: "C:\\x.iso".into(),
            media_kind: MediaKind::InstallIso,
            declared_origin: "x".into(),
        },
        Request::DeleteVm {
            vm_id,
            expected_revision: rev,
        },
        Request::OpenConsole { vm_id },
    ] {
        let name = r.name();
        let m = req(&mut p, r).await;
        assert_eq!(code(&m), Some(ErrorCode::PermissionDenied), "{name}: {m:?}");
    }
    // Nothing happened: the VM still exists.
    assert_eq!(status(&mut ui, vm_id).await.state, VmState::Stopped);
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn console_ticket_is_checked_and_revoked_with_the_run() {
    let vnc = fake_vnc();
    let svc = start_memory("f2-console", Some(vnc.path.clone())).await;
    let (mut ui, _) = connect_ui(&svc).await;
    let (vm_id, _) = sample_vm(&mut ui).await;
    // Capabilities report the host capacity.
    match req(&mut ui, Request::GetCapabilities).await {
        ServerMessage::Response {
            result: ResponseBody::Capabilities(c),
            ..
        } => {
            let h = c.host.expect("host capacity");
            assert!(h.input_available && h.screenshot_available);
        }
        other => panic!("{other:?}"),
    }
    // Stopped VM: no console.
    let m = req(&mut ui, Request::OpenConsole { vm_id }).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict));

    let run = start_vm(&mut ui, vm_id).await;
    let t = open_console(&mut ui, vm_id).await;
    assert_eq!((t.vm_id, t.run_id), (vm_id, run));
    assert_eq!(t.geometry, Some(FAKE_GEOMETRY));
    assert_eq!(t.token.len(), 64);
    assert!(t.token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(Path::new(&t.socket_path).exists());
    assert!(
        !t.socket_path.contains("v.sock"),
        "QEMU socket never exposed"
    );

    // Wrong token, uppercase token, truncated line: closed without RFB and
    // without contacting the display.
    let before = vnc.accepted.load(Ordering::SeqCst);
    let mut wrong = t.token.clone().into_bytes();
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    for line in [
        [wrong.as_slice(), b"\n"].concat(),
        format!("{}\n", t.token.to_uppercase()).into_bytes(),
        b"0123\n".to_vec(),
        [t.token.as_bytes(), b"X"].concat(),
    ] {
        let path = t.socket_path.clone();
        let got = blocking(move || {
            let s = viewer(&path, &line).unwrap();
            let _ = s.shutdown(std::net::Shutdown::Write);
            read_some(&s, GREETING.len())
        })
        .await;
        assert!(got.is_empty(), "no RFB without the token: {got:?}");
    }
    assert_eq!(vnc.accepted.load(Ordering::SeqCst), before);

    // Handshake not completed in time: closed after about 5 s.
    let path = t.socket_path.clone();
    let elapsed = blocking(move || {
        let s = viewer(&path, b"").unwrap();
        let t0 = Instant::now();
        assert!(read_some(&s, 1).is_empty());
        t0.elapsed()
    })
    .await;
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(9),
        "{elapsed:?}"
    );

    // Right token: RFB greeting from the display, then the client's RFB
    // stream, filtered (the fake display echoes what reaches it): the
    // version is forwarded, a KeyEvent closes the connection unforwarded.
    let (path, line) = (t.socket_path.clone(), token_line(&t));
    let first = blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        (&s).write_all(GREETING).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        s
    })
    .await;
    let (path, line) = (t.socket_path.clone(), token_line(&t));
    blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        let key = [4u8, 1, 0, 0, 0, 0, 0, 0x61];
        (&s).write_all(&[GREETING.as_slice(), b"\x01\x01", &key].concat())
            .unwrap();
        // At most the echo of the handshake, then EOF: the key never
        // reached the display.
        let hs = [GREETING.as_slice(), b"\x01\x01"].concat();
        let got = read_some(&s, 64);
        assert!(hs.starts_with(&got), "{got:?}");
    })
    .await;
    // That connection replaced the first viewer: reconnect it.
    drop(first);
    let (path, line) = (t.socket_path.clone(), token_line(&t));
    let first = blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        s
    })
    .await;
    // A second viewer replaces the first ("last wins").
    let (path, line) = (t.socket_path.clone(), token_line(&t));
    let (first, second) = blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        assert!(closed_within(&first, Duration::from_secs(5)));
        (first, s)
    })
    .await;
    drop(first);

    // A new ticket for the same run revokes the previous one.
    let t2 = open_console(&mut ui, vm_id).await;
    assert_ne!(t2.token, t.token);
    let second = blocking(move || {
        assert!(closed_within(&second, Duration::from_secs(5)));
        second
    })
    .await;
    drop(second);
    assert!(ticket_refused(&t).await);

    // Stop: the live connection is closed and the ticket refused.
    let (path, line) = (t2.socket_path.clone(), token_line(&t2));
    let live = blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        s
    })
    .await;
    succeeded(&submit(&mut ui, vm_id, None, VmAction::Stop).await);
    assert!(blocking(move || closed_within(&live, Duration::from_secs(5))).await);
    assert!(ticket_refused(&t2).await);
    assert!(!Path::new(&t2.socket_path).exists() || ticket_refused(&t2).await);

    // Emergency Stop revokes the ticket of the new run.
    let run2 = start_vm(&mut ui, vm_id).await;
    assert_ne!(run2, run);
    let t3 = open_console(&mut ui, vm_id).await;
    assert_eq!(t3.run_id, run2);
    let (path, line) = (t3.socket_path.clone(), token_line(&t3));
    let live = blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        s
    })
    .await;
    assert!(matches!(
        req(&mut ui, Request::EmergencyStop).await,
        ServerMessage::Response {
            result: ResponseBody::EmergencyStop(_),
            ..
        }
    ));
    assert!(blocking(move || closed_within(&live, Duration::from_secs(5))).await);
    assert!(ticket_refused(&t3).await);
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn console_ticket_dies_with_the_ui_connection() {
    let vnc = fake_vnc();
    let svc = start_memory("f2-console-conn", Some(vnc.path.clone())).await;
    let (mut ui, _) = connect_ui(&svc).await;
    let (vm_id, _) = sample_vm(&mut ui).await;
    start_vm(&mut ui, vm_id).await;

    // Revocation of the connection that obtained the ticket.
    let (mut b, b_id) = connect_ui(&svc).await;
    let t = open_console(&mut b, vm_id).await;
    let (path, line) = (t.socket_path.clone(), token_line(&t));
    let live = blocking(move || {
        let s = viewer(&path, &line).unwrap();
        assert_eq!(read_some(&s, GREETING.len()), GREETING);
        s
    })
    .await;
    assert!(matches!(
        req(
            &mut ui,
            Request::RevokeConnection {
                connection_id: b_id
            }
        )
        .await,
        ServerMessage::Response {
            result: ResponseBody::Revoked { .. },
            ..
        }
    ));
    assert!(blocking(move || closed_within(&live, Duration::from_secs(5))).await);
    assert!(ticket_refused(&t).await);

    // Normal close of the connection that obtained the ticket.
    let (mut c, _) = connect_ui(&svc).await;
    let t = open_console(&mut c, vm_id).await;
    drop(c);
    let mut refused = false;
    for _ in 0..100 {
        if ticket_refused(&t).await {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(refused, "ticket still valid after the UI connection closed");
    // Keys pressed on the lost console are released.
    let mut released = false;
    for _ in 0..100 {
        if calls(&svc).iter().any(|c| c == "vm_release_input") {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(released, "{:?}", calls(&svc));
    svc.handle.shutdown().await;
}

fn key(q: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(q).unwrap(),
        down,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn input_from_the_console_without_a_screenshot_and_lifecycle_actions() {
    let svc = start_memory("f2-input", None).await;
    let (mut ui, _) = connect_ui(&svc).await;
    let (vm_id, _) = sample_vm(&mut ui).await;
    let run = start_vm(&mut ui, vm_id).await;
    let batch = |geometry: DisplayGeometry, events: Vec<InputEvent>| {
        VmAction::Input(InputBatch {
            vm_id,
            run_id: run,
            geometry,
            events,
        })
    };
    // Pointer on the display geometry, never captured before: accepted
    // (the geometry is read from the backend).
    let pointer = vec![
        InputEvent::PointerMove(PointerMove::new(16000, 16000).unwrap()),
        key("a", true),
        key("a", false),
    ];
    succeeded(
        &submit(
            &mut ui,
            vm_id,
            Some(run),
            batch(FAKE_GEOMETRY, pointer.clone()),
        )
        .await,
    );
    // Stale geometry: refused (stale coordinates).
    let stale = DisplayGeometry {
        width: 1024,
        height: 768,
    };
    let m = submit(&mut ui, vm_id, Some(run), batch(stale, pointer)).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    assert_eq!(detail(&m, "reason").as_deref(), Some("stale_geometry"));
    // Keyboard only: independent of the geometry.
    succeeded(
        &submit(
            &mut ui,
            vm_id,
            Some(run),
            batch(stale, vec![key("ret", true), key("ret", false)]),
        )
        .await,
    );
    // Wrong run: refused.
    let other = RunId::new_v7();
    let m = submit(
        &mut ui,
        vm_id,
        Some(other),
        VmAction::Input(InputBatch {
            vm_id,
            run_id: other,
            geometry: FAKE_GEOMETRY,
            events: vec![key("a", true)],
        }),
    )
    .await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict));
    // Screenshot and reset (explicit), then graceful stop and forced stop.
    succeeded(&submit(&mut ui, vm_id, Some(run), VmAction::Screenshot).await);
    succeeded(&submit(&mut ui, vm_id, Some(run), VmAction::Reset).await);
    succeeded(&submit(&mut ui, vm_id, None, VmAction::Stop).await);
    start_vm(&mut ui, vm_id).await;
    succeeded(&submit(&mut ui, vm_id, None, VmAction::ForceStop).await);
    let c = calls(&svc);
    let inputs: Vec<&String> = c.iter().filter(|c| c.starts_with("vm_input")).collect();
    assert_eq!(inputs.len(), 2, "{c:?}");
    for want in ["vm_screenshot", "vm_reset", "vm_shutdown", "vm_force_stop"] {
        assert!(c.iter().any(|x| x == want), "{want}: {c:?}");
    }
    assert!(c.iter().any(|x| x == "vm_refresh_geometry"));
    svc.handle.shutdown().await;
}

fn iso_file(dir: &Path, name: &str, signature: &[u8]) -> PathBuf {
    let mut b = vec![0u8; 48 * 1024];
    b[16 * 2048 + 1..16 * 2048 + 1 + signature.len()].copy_from_slice(signature);
    let p = dir.join(name);
    std::fs::write(&p, b).unwrap();
    p
}

fn read_audit(dir: &Path) -> Vec<AuditEvent> {
    std::fs::read_to_string(audit_path(dir))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn import_list_create_and_delete_with_the_sqlite_catalog() {
    let dir = temp_dir("f2-sqlite");
    let src = temp_dir("f2-sources");
    let svc = start_sqlite(&dir).await;
    let (mut ui, _) = connect_ui(&svc).await;

    // Valid import as installation media.
    let iso = iso_file(&src, "Ubuntu.ISO", b"CD001");
    let media = match req(
        &mut ui,
        Request::ImportMedia {
            source_path: iso.to_str().unwrap().into(),
            media_kind: MediaKind::InstallIso,
            declared_origin: "ubuntu.com".into(),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::MediaImported { media },
            ..
        } => media,
        other => panic!("{other:?}"),
    };
    assert_eq!(media.kind, MediaKind::InstallIso);
    assert_eq!(media.size_bytes, 48 * 1024);
    let copy = dir.join("media").join(format!("{}.iso", media.id));
    assert!(std::fs::metadata(&copy).unwrap().permissions().readonly());
    match req(&mut ui, Request::ListMedia).await {
        ServerMessage::Response {
            result: ResponseBody::MediaList { media: list },
            ..
        } => assert_eq!(list, vec![media.clone()]),
        other => panic!("{other:?}"),
    }

    // Refused sources: nothing imported.
    std::fs::create_dir_all(src.join("folder.iso")).unwrap();
    let no_sig = iso_file(&src, "zip.iso", b"PK\x03\x04\x00");
    let txt = iso_file(&src, "notes.txt", b"CD001");
    let cases: Vec<(String, ErrorCode)> = vec![
        ("relative.iso".into(), ErrorCode::InvalidArgument),
        (r"\\server\share\x.iso".into(), ErrorCode::PermissionDenied),
        (r"\\?\C:\x.iso".into(), ErrorCode::PermissionDenied),
        (r"\\.\PhysicalDrive0".into(), ErrorCode::PermissionDenied),
        (r"//?/C:/x.iso".into(), ErrorCode::PermissionDenied),
        (
            format!("{}:ads", iso.display()),
            ErrorCode::PermissionDenied,
        ),
        (
            src.join("NUL.iso").display().to_string(),
            ErrorCode::PermissionDenied,
        ),
        (
            format!("{}\\..\\{}", src.display(), "Ubuntu.ISO"),
            ErrorCode::InvalidArgument,
        ),
        (txt.display().to_string(), ErrorCode::InvalidArgument),
        (
            src.join("folder.iso").display().to_string(),
            ErrorCode::InvalidArgument,
        ),
        (
            src.join("missing.iso").display().to_string(),
            ErrorCode::NotFound,
        ),
        (no_sig.display().to_string(), ErrorCode::InvalidArgument),
        (String::new(), ErrorCode::InvalidArgument),
    ];
    for (path, want) in cases {
        if !cfg!(windows) && path.contains('\\') {
            continue;
        }
        let m = req(
            &mut ui,
            Request::ImportMedia {
                source_path: path.clone(),
                media_kind: MediaKind::DataIso,
                declared_origin: "test".into(),
            },
        )
        .await;
        assert_eq!(code(&m), Some(want), "{path}: {m:?}");
    }
    // Junction named like an ISO (no privileges needed on Windows).
    if cfg!(windows) {
        let link = src.join("link.iso");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&src)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let m = req(
                &mut ui,
                Request::ImportMedia {
                    source_path: link.display().to_string(),
                    media_kind: MediaKind::DataIso,
                    declared_origin: "test".into(),
                },
            )
            .await;
            assert_eq!(code(&m), Some(ErrorCode::PermissionDenied), "{m:?}");
        }
    }
    assert_eq!(std::fs::read_dir(dir.join("media")).unwrap().count(), 1);

    // The imported medium is usable as installation media.
    let mut cfg = sample_config("f2-vm");
    cfg.install_media = Some(media.id);
    let vm = match req(&mut ui, Request::CreateVm { config: cfg }).await {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    };
    // The managed copy is verified at start (real inspector).
    let run = start_vm(&mut ui, vm.id).await;
    assert!(
        calls(&svc).iter().any(|c| c == "vm_start:f2-vm:media=1"),
        "{:?}",
        calls(&svc)
    );
    // Files of the VM written by the supervisor (disk, run directory).
    let vm_dir = dir.join("vms").join(vm.id.to_string());
    std::fs::create_dir_all(&vm_dir).unwrap();
    std::fs::write(vm_dir.join("primary.qcow2"), vec![1u8; 3000]).unwrap();
    let run_dir = dir.join("runs").join(run.to_string());
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("vars.fd"), vec![2u8; 1000]).unwrap();
    assert_eq!(status(&mut ui, vm.id).await.disk_used_bytes, Some(4000));

    // Delete refused while running, with a stale revision, then accepted.
    let m = req(
        &mut ui,
        Request::DeleteVm {
            vm_id: vm.id,
            expected_revision: 1,
        },
    )
    .await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict));
    succeeded(&submit(&mut ui, vm.id, None, VmAction::Stop).await);
    let m = req(
        &mut ui,
        Request::DeleteVm {
            vm_id: vm.id,
            expected_revision: 5,
        },
    )
    .await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict));
    match req(
        &mut ui,
        Request::DeleteVm {
            vm_id: vm.id,
            expected_revision: 1,
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::VmDeleted { vm_id },
            ..
        } => assert_eq!(vm_id, vm.id),
        other => panic!("{other:?}"),
    }
    assert!(!vm_dir.exists());
    assert!(!run_dir.exists());
    let m = req(&mut ui, Request::VmStatus { vm_id: vm.id }).await;
    assert_eq!(code(&m), Some(ErrorCode::NotFound));
    // The medium stays in the registry.
    assert!(copy.exists());
    svc.handle.shutdown().await;

    let events = read_audit(&dir);
    let has = |a, o| events.iter().any(|e| e.action == a && e.outcome == o);
    assert!(has(AuditAction::MediaImport, AuditOutcome::Succeeded));
    assert!(has(AuditAction::MediaImport, AuditOutcome::Failed));
    assert!(has(AuditAction::VmDelete, AuditOutcome::Denied));
    assert!(has(AuditAction::VmDelete, AuditOutcome::Succeeded));
    let text = std::fs::read_to_string(audit_path(&dir)).unwrap();
    assert!(!text.contains("Ubuntu.ISO"), "no host path in the audit");

    // After a restart the VM is still gone and the medium still there.
    let svc = start_sqlite(&dir).await;
    let (mut ui, _) = connect_ui(&svc).await;
    match req(&mut ui, Request::ListVms).await {
        ServerMessage::Response {
            result: ResponseBody::VmList { vms },
            ..
        } => assert!(vms.iter().all(|v| v.vm_id != vm.id)),
        other => panic!("{other:?}"),
    }
    match req(&mut ui, Request::ListMedia).await {
        ServerMessage::Response {
            result: ResponseBody::MediaList { media: list },
            ..
        } => assert_eq!(list.len(), 1),
        other => panic!("{other:?}"),
    }
    svc.handle.shutdown().await;
}

// ----- real QEMU -------------------------------------------------------------

/// Display seen by the RFB client through the ticket proxy.
#[derive(Debug, Default, Clone)]
struct RfbSeen {
    connected: bool,
    init: Option<(u16, u16)>,
    updates: u64,
    resizes: u64,
    size: Option<(u16, u16)>,
    non_black_permille: u64,
    ended: Option<String>,
    /// Last RGB copy of the framebuffer (refreshed every 2 s).
    snap: Option<Snap>,
}

/// RGB framebuffer copy (not printed).
#[derive(Clone)]
struct Snap(Arc<(u16, u16, Vec<u8>)>);

impl std::fmt::Debug for Snap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.0.0, self.0.1)
    }
}

/// Saves the last framebuffer copy as PNG (evidence of the real tests).
fn save_png(seen: &RfbSeen, path: &Path) {
    let Some(Snap(s)) = &seen.snap else {
        return;
    };
    let img = qemu_backend::screenshot::RgbImage {
        width: u32::from(s.0),
        height: u32::from(s.1),
        data: s.2.clone(),
    };
    if let Ok(png) = qemu_backend::screenshot::encode_png(&img) {
        let _ = std::fs::write(path, png);
        eprintln!("framebuffer saved: {}", path.display());
    }
}

fn rfb_watch(ticket: &ConsoleTicket) -> Arc<std::sync::Mutex<RfbSeen>> {
    let seen = Arc::new(std::sync::Mutex::new(RfbSeen::default()));
    let s2 = seen.clone();
    let transport = rfb_client::Transport::Ticket {
        path: PathBuf::from(&ticket.socket_path),
        imp: rfb_client::UnixImpl::Socket2,
        token: ticket.token.clone(),
    };
    std::thread::spawn(move || {
        let mut conn = match rfb_client::Connection::connect(&transport) {
            Ok(c) => c,
            Err(e) => {
                s2.lock().unwrap().ended = Some(format!("connect: {e}"));
                return;
            }
        };
        {
            let mut s = s2.lock().unwrap();
            s.connected = true;
            let i = conn.server_init();
            s.init = Some((i.width, i.height));
            s.size = s.init;
        }
        let mut last_snap: Option<Instant> = None;
        loop {
            let r = conn.request_update().and_then(|()| conn.read_update());
            let mut s = s2.lock().unwrap();
            match r {
                Ok(u) => {
                    s.updates += 1;
                    if u.resized {
                        s.resizes += 1;
                    }
                    s.size = Some((u.width, u.height));
                    let fb = conn.framebuffer();
                    let px = fb.data().as_chunks::<4>().0;
                    let n = px.len().max(1) as u64;
                    let lit = px.iter().filter(|p| p[..3].iter().any(|&c| c > 16)).count() as u64;
                    s.non_black_permille = lit * 1000 / n;
                    if last_snap.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                        last_snap = Some(Instant::now());
                        s.snap = Some(Snap(Arc::new((fb.width(), fb.height(), fb.to_rgb()))));
                    }
                }
                Err(e) => {
                    s.ended = Some(e.to_string());
                    return;
                }
            }
            drop(s);
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    seen
}

/// Removes a test catalog even if the test fails: the managed copy of the
/// ISO is large and read-only.
struct RemoveCatalog(PathBuf);

impl Drop for RemoveCatalog {
    fn drop(&mut self) {
        if let Ok(rd) = std::fs::read_dir(self.0.join("media")) {
            for e in rd.flatten() {
                if let Ok(m) = e.metadata() {
                    let mut p = m.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    p.set_readonly(false);
                    let _ = std::fs::set_permissions(e.path(), p);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn qemu_dir_for_test() -> PathBuf {
    PathBuf::from(std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"))
}

fn percentile(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i.min(v.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Real end-to-end through the service (F2-01): import of the Ubuntu ISO as
/// installation media, VM creation, start (WHPX), console ticket with an RFB
/// handshake through the proxy and framebuffer updates, keyboard and pointer
/// input through IPC with latency measurements, reset, graceful stop, forced
/// stop, deletion. Requires `LIGHTVM_E2E_QEMU_DIR`; optional
/// `LIGHTVM_E2E_ISO` (default `lab/media/ubuntu-26.04.1-desktop-amd64.iso`)
/// and `LIGHTVM_E2E_BOOT_S` (wait for the live system, default 150). The
/// catalog (with the 6 GiB managed copy) is removed at the end. Run in
/// release: `cargo test --release -p lightvm-service --test f2 --
/// --ignored --nocapture`.
#[test]
#[ignore = "requires approved QEMU, an accelerator and the Ubuntu ISO (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_import_console_input_stop_delete() {
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    use qemu_backend::ShutdownPolicy;

    let qemu_dir = PathBuf::from(
        std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"),
    );
    let iso = std::env::var_os("LIGHTVM_E2E_ISO").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../lab/media/ubuntu-26.04.1-desktop-amd64.iso")
        },
        PathBuf::from,
    );
    let iso = std::fs::canonicalize(&iso).expect("ISO");
    // `canonicalize` gives a `\\?\` path, refused by the service on purpose:
    // the UI passes the path from the native picker (drive letter form).
    let iso_text = iso.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
    let boot_s: u64 = std::env::var("LIGHTVM_E2E_BOOT_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    // Catalogs left by an interrupted run (QEMU kept the copy open).
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir().join("lightvm-service-tests")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("f2-e2e-") {
                drop(RemoveCatalog(e.path()));
            }
        }
    }
    let dir = temp_dir("f2-e2e");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lve");
    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir, store.root(), private_root);
    setup.stop_policy = ShutdownPolicy {
        powerdown_timeout: Duration::from_secs(60),
        quit_timeout: Duration::from_secs(10),
        kill_timeout: Duration::from_secs(10),
    };
    let (host, vm_backend, mut capacity) = probe(setup).expect("QEMU probe");
    assert!(capacity.accelerator_available, "accelerator not available");
    assert!(capacity.input_available && capacity.screenshot_available);
    // The lab host may have less free space than the 16 GiB reserve plus the
    // VM disk after the 6 GiB managed copy: the qcow2 disk is sparse and the
    // live session does not write to it, so the test lowers the reserve
    // (declared in F2-01) instead of failing on host capacity.
    if capacity.free_storage_gib < capacity.reserved_storage_gib + 25 {
        eprintln!(
            "free space {} GiB below reserve {} + 25 GiB: reserve set to 0 for this test",
            capacity.free_storage_gib, capacity.reserved_storage_gib
        );
        capacity.reserved_storage_gib = 0;
        capacity.free_storage_gib = capacity.free_storage_gib.max(25);
    }
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
        let dir = dir2;
        let (handle, ui) = start(config(&dir), Box::new(engine), Box::new(host))
            .await
            .unwrap();
        let svc = Svc {
            handle,
            ui,
            calls: Calls::default(),
        };
        let (mut c, _) = connect_ui(&svc).await;
        match req(&mut c, Request::GetCapabilities).await {
            ServerMessage::Response {
                result: ResponseBody::Capabilities(caps),
                ..
            } => eprintln!(
                "capabilities: backend {} accelerator {:?} host {:?}",
                caps.backend, caps.accelerator, caps.host
            ),
            other => panic!("{other:?}"),
        }

        // 1. Import: the supervisor keeps answering during the copy.
        let size = std::fs::metadata(&iso).unwrap().len();
        let (mut other, _) = connect_ui(&svc).await;
        let t0 = Instant::now();
        let import = {
            let mut c2 = connect_ui(&svc).await.0;
            let path = iso_text.clone();
            tokio::spawn(async move {
                let m = c2
                    .request(Request::ImportMedia {
                        source_path: path,
                        media_kind: MediaKind::InstallIso,
                        declared_origin: "releases.ubuntu.com (lab copy)".into(),
                    })
                    .await
                    .unwrap();
                (m, t0.elapsed())
            })
        };
        let mut during = Vec::new();
        while !import.is_finished() {
            let t = Instant::now();
            assert!(matches!(
                req(&mut other, Request::ListVms).await,
                ServerMessage::Response { .. }
            ));
            during.push(t.elapsed());
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let (m, import_time) = import.await.unwrap();
        let media = match m {
            ServerMessage::Response {
                result: ResponseBody::MediaImported { media },
                ..
            } => media,
            other => panic!("{other:?}"),
        };
        assert_eq!(media.size_bytes, size);
        let n = during.len();
        let max_during = during.iter().max().copied().unwrap_or_default();
        eprintln!(
            "import: {size} bytes in {import_time:?} ({:.0} MiB/s), sha256 {}; {n} list_vms during the copy, max {:.1} ms",
            size as f64 / 1048576.0 / import_time.as_secs_f64(),
            media.sha256.to_hex(),
            ms(max_during)
        );

        // 2. VM with the imported medium.
        let mut cfg = sample_config("f2-e2e-ubuntu");
        cfg.vcpus = 2;
        cfg.memory_mib = 4096;
        cfg.disk_gib = 25;
        cfg.install_media = Some(media.id);
        let vm = match req(&mut c, Request::CreateVm { config: cfg }).await {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm,
            other => panic!("{other:?}"),
        };
        let t = Instant::now();
        let run = start_vm(&mut c, vm.id).await;
        eprintln!("start (media verification included): {:?}", t.elapsed());

        // 3. Console ticket and RFB through the proxy.
        let t = Instant::now();
        let ticket = open_console(&mut c, vm.id).await;
        eprintln!(
            "open_console: {:?}, geometry {:?}, socket {} (token not shown)",
            t.elapsed(),
            ticket.geometry,
            ticket.socket_path
        );
        assert_eq!(ticket.run_id, run);
        let seen = rfb_watch(&ticket);
        let mut ok = false;
        for _ in 0..200 {
            if seen.lock().unwrap().updates >= 1 {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let s = seen.lock().unwrap().clone();
        eprintln!("RFB via ticket: {s:?}");
        assert!(ok && s.connected, "no framebuffer update: {s:?}");
        // A wrong token never reaches QEMU.
        let mut bad = ticket.clone();
        bad.token = "0".repeat(64);
        assert!(ticket_refused(&bad).await);

        // 4. Keyboard: Enter on the boot menu, then latency of 100 batches.
        tokio::time::sleep(Duration::from_secs(8)).await;
        let geom = |s: &RfbSeen| {
            let (w, h) = s.size.unwrap();
            DisplayGeometry {
                width: u32::from(w),
                height: u32::from(h),
            }
        };
        let g = geom(&seen.lock().unwrap());
        let batch = |g: DisplayGeometry, events: Vec<InputEvent>| {
            VmAction::Input(InputBatch {
                vm_id: vm.id,
                run_id: run,
                geometry: g,
                events,
            })
        };
        succeeded(
            &submit(
                &mut c,
                vm.id,
                Some(run),
                batch(g, vec![key("ret", true), key("ret", false)]),
            )
            .await,
        );
        // Paced at 50 batches/s (a console sends at most one batch per frame;
        // the per-connection rate limit is 100 requests/s).
        let mut lat = Vec::new();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let t = Instant::now();
            succeeded(
                &submit(
                    &mut c,
                    vm.id,
                    Some(run),
                    batch(g, vec![key("shift", true), key("shift", false)]),
                )
                .await,
            );
            lat.push(t.elapsed());
        }
        let (p50, p95) = (percentile(&mut lat, 0.5), percentile(&mut lat, 0.95));
        let max = *lat.iter().max().unwrap();
        eprintln!(
            "keyboard batch IPC→QMP round trip, 100 samples: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
            ms(p50),
            ms(p95),
            ms(max)
        );

        // 5. Pointer on the current geometry (no screenshot taken), stale
        // geometry refused.
        let g = geom(&seen.lock().unwrap());
        let mut plat = Vec::new();
        for i in 0..100u16 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let t = Instant::now();
            succeeded(
                &submit(
                    &mut c,
                    vm.id,
                    Some(run),
                    batch(
                        g,
                        vec![InputEvent::PointerMove(
                            PointerMove::new(1000 + i * 100, 16000).unwrap(),
                        )],
                    ),
                )
                .await,
            );
            plat.push(t.elapsed());
        }
        eprintln!(
            "pointer batch round trip on {}x{}, 100 samples: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
            g.width,
            g.height,
            ms(percentile(&mut plat, 0.5)),
            ms(percentile(&mut plat, 0.95)),
            ms(*plat.iter().max().unwrap())
        );
        let stale = DisplayGeometry {
            width: g.width + 16,
            height: g.height,
        };
        let m = submit(
            &mut c,
            vm.id,
            Some(run),
            batch(
                stale,
                vec![InputEvent::PointerMove(PointerMove::new(10, 10).unwrap())],
            ),
        )
        .await;
        assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
        assert_eq!(detail(&m, "reason").as_deref(), Some("stale_geometry"));

        // 6. Boot of the live system; after a resolution change the new
        // geometry is accepted without a screenshot.
        let deadline = Instant::now() + Duration::from_secs(boot_s);
        let mut last = seen.lock().unwrap().clone();
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let s = seen.lock().unwrap().clone();
            if s.size != last.size {
                eprintln!(
                    "display resized {:?} → {:?} after {:?}",
                    last.size,
                    s.size,
                    t0.elapsed()
                );
            }
            last = s;
        }
        let s = seen.lock().unwrap().clone();
        eprintln!("RFB after the boot wait: {s:?}");
        assert!(s.ended.is_none(), "RFB session ended: {s:?}");
        let g2 = geom(&s);
        succeeded(
            &submit(
                &mut c,
                vm.id,
                Some(run),
                batch(
                    g2,
                    vec![InputEvent::PointerMove(PointerMove::new(16383, 16383).unwrap())],
                ),
            )
            .await,
        );
        succeeded(&submit(&mut c, vm.id, Some(run), VmAction::Screenshot).await);

        // 7. Graceful stop (ACPI with a 60 s wait, then `quit`) in the
        // background: the reply comes at once, the supervisor keeps serving
        // and the console stays open until the process exits.
        let t = Instant::now();
        let m = submit(&mut c, vm.id, None, VmAction::Stop).await;
        let reply = t.elapsed();
        assert_eq!(op_status(&m), OperationStatus::Forwarded, "{m:?}");
        assert_eq!(status(&mut c, vm.id).await.state, VmState::Stopping);
        // The user can still answer the guest from the console while stopping.
        let g = geom(&seen.lock().unwrap());
        let t_in = Instant::now();
        succeeded(
            &submit(
                &mut c,
                vm.id,
                Some(run),
                batch(
                    g,
                    vec![
                        InputEvent::PointerMove(PointerMove::new(16000, 16000).unwrap()),
                        key("shift", true),
                        key("shift", false),
                    ],
                ),
            )
            .await,
        );
        eprintln!("input while stopping: succeeded in {:?}", t_in.elapsed());
        let mut during = Vec::new();
        let mut console_open_while_stopping = true;
        let total = loop {
            let tq = Instant::now();
            let st = status(&mut c, vm.id).await;
            during.push(tq.elapsed());
            if st.state == VmState::Stopped {
                break t.elapsed();
            }
            if seen.lock().unwrap().ended.is_some() {
                console_open_while_stopping = false;
            }
            assert!(t.elapsed() < Duration::from_secs(150), "stop not completed");
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        eprintln!(
            "graceful stop: reply {reply:?} (forwarded), stopped after {total:?} ({}); {} vm_status during the stop, max {:.1} ms; console open while stopping: {console_open_while_stopping}",
            if total < Duration::from_secs(60) {
                "guest powered off on ACPI"
            } else {
                "ACPI wait expired, then quit"
            },
            during.len(),
            ms(*during.iter().max().unwrap())
        );
        assert!(reply < Duration::from_secs(2));
        let mut ended = false;
        for _ in 0..100 {
            if seen.lock().unwrap().ended.is_some() {
                ended = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        eprintln!("RFB at the end of the run: {:?}", seen.lock().unwrap());
        assert!(ended, "RFB session not closed at the end of the run");
        assert!(ticket_refused(&ticket).await);

        // Persistent firmware state: per-VM variables, not per run.
        let vars = dir
            .join("vms")
            .join(vm.id.to_string())
            .join("firmware")
            .join("primary.vars.fd");
        let template = qemu_dir_for_test().join("share").join("edk2-i386-vars.fd");
        let sha = |p: &Path| qemu_backend::probe::sha256_file(p).unwrap();
        let vars_after_run1 = sha(&vars);
        let created1 = std::fs::metadata(&vars).unwrap().created().ok();
        eprintln!(
            "UEFI variables of the VM after run 1: sha256 {} (template {}), per-run copy present: {}",
            &vars_after_run1[..16],
            &sha(&template)[..16],
            dir.join("runs").join(run.to_string()).join("vars.fd").exists()
        );
        assert!(!dir.join("runs").join(run.to_string()).join("vars.fd").exists());

        // 8. Second run: same variables file, new ticket, explicit reset
        // (same run, the console stays open), then graceful stop overtaken
        // by a forced stop.
        let run2 = start_vm(&mut c, vm.id).await;
        assert_ne!(run2, run);
        assert_eq!(std::fs::metadata(&vars).unwrap().created().ok(), created1);
        assert!(!dir.join("runs").join(run2.to_string()).join("vars.fd").exists());
        let ticket2 = open_console(&mut c, vm.id).await;
        assert_eq!(ticket2.run_id, run2);
        assert_ne!(ticket2.token, ticket.token);
        let seen = rfb_watch(&ticket2);
        tokio::time::sleep(Duration::from_secs(5)).await;
        let t = Instant::now();
        succeeded(&submit(&mut c, vm.id, Some(run2), VmAction::Reset).await);
        eprintln!("reset: {:?}", t.elapsed());
        let before = seen.lock().unwrap().updates;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let s = seen.lock().unwrap().clone();
        eprintln!("RFB after the reset: {s:?}");
        assert!(s.ended.is_none() && s.updates > before, "{s:?}");
        assert_eq!(status(&mut c, vm.id).await.run_id, Some(run2));
        assert_eq!(
            op_status(&submit(&mut c, vm.id, None, VmAction::Stop).await),
            OperationStatus::Forwarded
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        let t = Instant::now();
        succeeded(&submit(&mut c, vm.id, None, VmAction::ForceStop).await);
        eprintln!(
            "forced stop during the graceful stop: {:?}, state {:?}",
            t.elapsed(),
            status(&mut c, vm.id).await.state
        );
        assert_eq!(status(&mut c, vm.id).await.state, VmState::Stopped);
        assert!(ticket_refused(&ticket2).await);

        // 9. Deletion: rows and files.
        let used = status(&mut c, vm.id).await.disk_used_bytes;
        eprintln!("disk_used_bytes before deletion: {used:?}");
        assert!(used.unwrap_or(0) > 0);
        let vm_dir = dir.join("vms").join(vm.id.to_string());
        let run_dirs = [run, run2].map(|r| dir.join("runs").join(r.to_string()));
        assert!(vm_dir.exists() && run_dirs.iter().all(|d| d.exists()));
        let t = Instant::now();
        match req(
            &mut c,
            Request::DeleteVm {
                vm_id: vm.id,
                expected_revision: vm.config_revision,
            },
        )
        .await
        {
            ServerMessage::Response {
                result: ResponseBody::VmDeleted { .. },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        eprintln!("delete_vm: {:?}", t.elapsed());
        assert!(!vm_dir.exists());
        assert!(run_dirs.iter().all(|d| !d.exists()));
        svc.handle.shutdown().await;
        let events = read_audit(&dir);
        let count = |a: AuditAction| events.iter().filter(|e| e.action == a).count();
        eprintln!(
            "audit: {} lines, vm_input {}, media_import {}, console_open {}, console_revoke {}, vm_delete {}",
            events.len(),
            count(AuditAction::VmInput),
            count(AuditAction::MediaImport),
            count(AuditAction::ConsoleOpen),
            count(AuditAction::ConsoleRevoke),
            count(AuditAction::VmDelete)
        );
        // Tickets revoked with their runs (and the replaced one) are audited.
        assert!(count(AuditAction::ConsoleRevoke) >= 2);
        let text = std::fs::read_to_string(audit_path(&dir)).unwrap();
        assert!(!text.contains(&ticket.token), "token never audited");
    });
}

fn op_status(m: &ServerMessage) -> OperationStatus {
    match m {
        ServerMessage::Response {
            result:
                ResponseBody::Operation {
                    operation: Some(op),
                    ..
                },
            ..
        } => op.status,
        other => panic!("{other:?}"),
    }
}

async fn wait_state(c: &mut Client, vm_id: VmId, want: VmState) -> bool {
    for _ in 0..100 {
        if status(c, vm_id).await.state == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_stop_runs_in_the_background_and_a_forced_stop_wins() {
    let vnc = fake_vnc();
    let stops = StopResults::default();
    let svc = start_memory_with("f2-async-stop", Some(vnc.path.clone()), Some(stops.clone())).await;
    let (mut ui, _) = connect_ui(&svc).await;
    let (vm_id, _) = sample_vm(&mut ui).await;
    start_vm(&mut ui, vm_id).await;
    let ticket = open_console(&mut ui, vm_id).await;

    // Stop: the reply comes at once with the operation forwarded, the VM is
    // `stopping`, the console stays open during the graceful stop.
    let t = Instant::now();
    let m = submit(&mut ui, vm_id, None, VmAction::Stop).await;
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(op_status(&m), OperationStatus::Forwarded);
    assert_eq!(status(&mut ui, vm_id).await.state, VmState::Stopping);
    assert!(
        !ticket_refused(&ticket).await,
        "console open while stopping"
    );
    let c = calls(&svc);
    assert!(c.iter().any(|x| x == "vm_shutdown_begin"));
    assert!(!c.iter().any(|x| x == "vm_shutdown"));

    // Other requests are served meanwhile (MVP limit: one running VM).
    let t = Instant::now();
    let _other = match req(
        &mut ui,
        Request::CreateVm {
            config: sample_config("other"),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm.id,
        m => panic!("{m:?}"),
    };
    assert!(matches!(
        req(&mut ui, Request::ListVms).await,
        ServerMessage::Response { .. }
    ));
    assert!(t.elapsed() < Duration::from_secs(2));
    // A second graceful stop of the same VM is refused (busy), input too.
    let m = submit(&mut ui, vm_id, None, VmAction::Stop).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    // The user can answer the guest's power-off confirmation while stopping.
    let run = status(&mut ui, vm_id).await.run_id.expect("run");
    succeeded(
        &submit(
            &mut ui,
            vm_id,
            Some(run),
            VmAction::Input(InputBatch {
                vm_id,
                run_id: run,
                geometry: FAKE_GEOMETRY,
                events: vec![
                    InputEvent::PointerMove(PointerMove::new(16000, 16000).unwrap()),
                    key("ret", true),
                    key("ret", false),
                ],
            }),
        )
        .await,
    );
    assert!(calls(&svc).iter().any(|c| c.starts_with("vm_input")));

    // Forced stop: forwarded at once and wins.
    let t = Instant::now();
    succeeded(&submit(&mut ui, vm_id, None, VmAction::ForceStop).await);
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(status(&mut ui, vm_id).await.state, VmState::Stopped);
    assert!(ticket_refused(&ticket).await);
    // A late outcome of the superseded stop changes nothing.
    stops.lock().unwrap().push((vm_id, Ok(())));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(status(&mut ui, vm_id).await.state, VmState::Stopped);

    // Exit observed in the background: `stopped` published without requests.
    let run = start_vm(&mut ui, vm_id).await;
    assert!(matches!(
        req(&mut ui, Request::Subscribe).await,
        ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }
    ));
    assert_eq!(
        op_status(&submit(&mut ui, vm_id, None, VmAction::Stop).await),
        OperationStatus::Forwarded
    );
    stops.lock().unwrap().push((vm_id, Ok(())));
    let mut stopped = false;
    for _ in 0..100 {
        match tokio::time::timeout(T, ui.next_event())
            .await
            .unwrap()
            .unwrap()
        {
            Ok(lightvm_proto::ipc::ServiceEvent::VmState {
                vm_id: v,
                state: VmState::Stopped,
            }) if v == vm_id => {
                stopped = true;
                break;
            }
            _ => {}
        }
    }
    assert!(stopped, "no `stopped` event after the exit");
    let st = status(&mut ui, vm_id).await;
    assert_eq!((st.state, st.run_id), (VmState::Stopped, None));
    let _ = run;

    // Escalation that fails (process not terminated): the VM stays
    // `stopping` and a forced stop resolves it.
    start_vm(&mut ui, vm_id).await;
    let _ = submit(&mut ui, vm_id, None, VmAction::Stop).await;
    stops.lock().unwrap().push((
        vm_id,
        Err(lightvm_proto::PublicError::infrastructure("kill failed")),
    ));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(status(&mut ui, vm_id).await.state, VmState::Stopping);
    succeeded(&submit(&mut ui, vm_id, None, VmAction::ForceStop).await);
    assert!(wait_state(&mut ui, vm_id, VmState::Stopped).await);

    // Service shutdown with a graceful stop in progress: it waits for it
    // (outcome after 1.5 s) instead of killing the VM.
    start_vm(&mut ui, vm_id).await;
    let _ = submit(&mut ui, vm_id, None, VmAction::Stop).await;
    let before = calls(&svc).iter().filter(|c| *c == "vm_force_stop").count();
    let s2 = stops.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        s2.lock().unwrap().push((vm_id, Ok(())));
    });
    let t = Instant::now();
    svc.handle.shutdown().await;
    assert!(
        t.elapsed() >= Duration::from_millis(1400),
        "{:?}",
        t.elapsed()
    );
    let after = svc
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == "vm_force_stop")
        .count();
    assert_eq!(after, before, "no forced stop at shutdown");
}

/// Engine and host on the real QEMU backend for a catalog (outside any tokio
/// runtime: the probe uses the backend's own runtime).
fn real_service_parts(
    dir: &Path,
    private_root: PathBuf,
    cpu: &str,
) -> (
    Box<dyn lightvm_service::engine::VmEngine>,
    Box<dyn lightvm_service::supervisor::Backend>,
) {
    use lightvm_service::adapters::backend::{QemuSetup, probe};
    let store = SqliteStore::open(dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir_for_test(), store.root(), private_root);
    setup.cpu_model = cpu.to_owned();
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(capacity.accelerator_available, "accelerator not available");
    let audit = FileAuditSink::open(&audit_path(dir)).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    (Box::new(engine), Box::new(host))
}

/// Waits for the guest display to show the installed system: framebuffer at
/// the desktop resolution and mostly lit (GDM login screen), as in F1-17;
/// the OVMF "no bootable option" screen is mostly black text.
async fn wait_installed_desktop(seen: &Arc<std::sync::Mutex<RfbSeen>>, limit: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let s = seen.lock().unwrap().clone();
        if s.size == Some((1280, 800)) && s.non_black_permille > 500 {
            // Stable for 10 s.
            tokio::time::sleep(Duration::from_secs(10)).await;
            let s = seen.lock().unwrap().clone();
            if s.size == Some((1280, 800)) && s.non_black_permille > 500 {
                eprintln!("installed system on screen after {:?}: {s:?}", t0.elapsed());
                return true;
            }
        }
    }
    eprintln!("installed system not seen: {:?}", seen.lock().unwrap());
    false
}

async fn stop_gracefully(c: &mut Client, vm_id: VmId, limit: Duration) -> Duration {
    let t = Instant::now();
    let m = submit(c, vm_id, None, VmAction::Stop).await;
    assert!(
        matches!(
            op_status(&m),
            OperationStatus::Forwarded | OperationStatus::Succeeded
        ),
        "{m:?}"
    );
    while status(c, vm_id).await.state != VmState::Stopped {
        assert!(t.elapsed() < limit, "VM not stopped");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    t.elapsed()
}

async fn import_media(c: &mut Client, path: String, kind: MediaKind) -> lightvm_proto::MediaRecord {
    let t = Instant::now();
    match c
        .request(Request::ImportMedia {
            source_path: path,
            media_kind: kind,
            declared_origin: "lab media".into(),
        })
        .await
        .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::MediaImported { media },
            ..
        } => {
            eprintln!(
                "import {kind:?}: {} bytes in {:?}, sha256 {}",
                media.size_bytes,
                t.elapsed(),
                media.sha256.to_hex()
            );
            media
        }
        other => panic!("{other:?}"),
    }
}

/// F2 exit criterion path through the service with real QEMU: import of the
/// Ubuntu ISO (installation media) and of `lab/media/f1-17-cidata.iso`
/// (data media with the autoinstall of F1-17), VM creation, start, click on
/// "Install" through the service's input path (as F1-17), unattended
/// installation until the guest powers off, removal of the media with
/// `update_vm`, start from the disk (login screen), graceful stop; then
/// "reopen LightVM": new service on the same catalog, VM listed, start from
/// the disk again (disk and UEFI variables persistent), graceful stop,
/// deletion. CPU `Skylake-Client-v4` (`LIGHTVM_E2E_CPU`), 4 vCPUs, 6144 MiB,
/// 25 GiB. Requires `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_ISO`,
/// `LIGHTVM_E2E_CIDATA`, `LIGHTVM_E2E_INSTALL_MIN` (45),
/// `LIGHTVM_E2E_EVIDENCE_DIR` (framebuffer PNGs). Never reads
/// `lab/vms/f1-17/`.
#[test]
#[ignore = "requires approved QEMU, an accelerator, the Ubuntu ISO and the F1-17 CIDATA volume (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_f2_exit_install_reboot_reopen() {
    let lab_media = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab/media");
    let path_env = |var: &str, default: &str| {
        let p = std::env::var_os(var).map_or_else(|| lab_media.join(default), PathBuf::from);
        let p = std::fs::canonicalize(p).expect("media file");
        p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned()
    };
    let iso = path_env("LIGHTVM_E2E_ISO", "ubuntu-26.04.1-desktop-amd64.iso");
    let cidata = path_env("LIGHTVM_E2E_CIDATA", "f1-17-cidata.iso");
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let install_min: u64 = std::env::var("LIGHTVM_E2E_INSTALL_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);
    let evidence = std::env::var_os("LIGHTVM_E2E_EVIDENCE_DIR").map_or_else(
        || std::env::temp_dir().join("lightvm-f2-evidence"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir().join("lightvm-service-tests")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("f2-exit-") {
                drop(RemoveCatalog(e.path()));
            }
        }
    }
    let dir = temp_dir("f2-exit");
    let _cleanup = RemoveCatalog(dir.clone());
    let private_root = short_dir("lvx");

    // ----- first service: install, reboot from the disk -----------------------
    let (engine, host) = real_service_parts(&dir, private_root.clone(), &cpu);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let vm_id = rt.block_on(async {
        let (handle, ui) = start(config(&dir), engine, host).await.unwrap();
        let svc = Svc {
            handle,
            ui,
            calls: Calls::default(),
        };
        let (mut c, _) = connect_ui(&svc).await;
        let install = import_media(&mut c, iso.clone(), MediaKind::InstallIso).await;
        let data = import_media(&mut c, cidata.clone(), MediaKind::DataIso).await;

        let mut cfg = sample_config("f2-exit-ubuntu");
        cfg.vcpus = 4;
        cfg.memory_mib = 6144;
        cfg.disk_gib = 25;
        cfg.install_media = Some(install.id);
        cfg.data_media = Some(data.id);
        let vm = match req(&mut c, Request::CreateVm { config: cfg }).await {
            ServerMessage::Response {
                result: ResponseBody::VmCreated { vm },
                ..
            } => vm,
            other => panic!("{other:?}"),
        };
        let t0 = Instant::now();
        let run = start_vm(&mut c, vm.id).await;
        let ticket = open_console(&mut c, vm.id).await;
        let seen = rfb_watch(&ticket);

        // The desktop installer reads the autoinstall and waits for
        // "Install" in its summary about 2 minutes after start (F1-17):
        // click through the service's input path at 140 s.
        while t0.elapsed() < Duration::from_secs(140) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let s = seen.lock().unwrap().clone();
        save_png(&s, &evidence.join("f2-exit-1-installer-summary.png"));
        let (w, h) = s.size.expect("display size");
        let g = DisplayGeometry {
            width: u32::from(w),
            height: u32::from(h),
        };
        let (x, y) = (g.width * 1035 / 1280, g.height * 716 / 800);
        let click = vec![
            InputEvent::PointerMove(PointerMove::from_pixels(x, y, g).unwrap()),
            InputEvent::PointerButton(lightvm_proto::PointerButton {
                button: lightvm_proto::MouseButton::Left,
                down: true,
            }),
            InputEvent::PointerButton(lightvm_proto::PointerButton {
                button: lightvm_proto::MouseButton::Left,
                down: false,
            }),
        ];
        succeeded(
            &submit(
                &mut c,
                vm.id,
                Some(run),
                VmAction::Input(InputBatch {
                    vm_id: vm.id,
                    run_id: run,
                    geometry: g,
                    events: click,
                }),
            )
            .await,
        );
        eprintln!("click on Install ({x},{y}) on {w}x{h} at {:?}", t0.elapsed());

        // Unattended installation until the guest powers off (`shutdown: poweroff`).
        let mut next_log = Duration::from_secs(0);
        loop {
            let st = status(&mut c, vm.id).await;
            if st.state == VmState::Stopped {
                break;
            }
            assert_ne!(st.state, VmState::Failed, "VM failed during the installation");
            if t0.elapsed() > Duration::from_secs(install_min * 60) {
                let _ = req(&mut c, Request::EmergencyStop).await;
                panic!("installation not completed in {install_min} minutes");
            }
            if t0.elapsed() >= next_log {
                next_log += Duration::from_secs(120);
                let s = seen.lock().unwrap().clone();
                eprintln!(
                    "install t={}s state {:?} display {:?} non-black {} per mille",
                    t0.elapsed().as_secs(),
                    st.state,
                    s.size,
                    s.non_black_permille
                );
                save_png(
                    &s,
                    &evidence.join(format!(
                        "f2-exit-2-install-{:03}min.png",
                        t0.elapsed().as_secs() / 60
                    )),
                );
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let vm_dir = dir.join("vms").join(vm.id.to_string());
        let disk_bytes = std::fs::metadata(vm_dir.join("primary.qcow2"))
            .unwrap()
            .len();
        let vars = vm_dir.join("firmware").join("primary.vars.fd");
        let vars_sha = qemu_backend::probe::sha256_file(&vars).unwrap();
        let template_sha = qemu_backend::probe::sha256_file(
            &qemu_dir_for_test().join("share").join("edk2-i386-vars.fd"),
        )
        .unwrap();
        eprintln!(
            "installation finished, guest powered off after {:?}; disk {disk_bytes} bytes; UEFI variables {} (template {})",
            t0.elapsed(),
            &vars_sha[..16],
            &template_sha[..16]
        );
        assert!(disk_bytes > 2 << 30, "disk almost empty: nothing installed");
        assert_ne!(vars_sha, template_sha, "boot entry not written in the variables");

        // Media removed as the UI does (VM off), then boot from the disk.
        let st = status(&mut c, vm.id).await;
        let mut cfg = st.config.clone();
        cfg.install_media = None;
        cfg.data_media = None;
        match req(
            &mut c,
            Request::UpdateVm {
                vm_id: vm.id,
                expected_revision: st.config_revision,
                config: cfg,
            },
        )
        .await
        {
            ServerMessage::Response {
                result: ResponseBody::VmUpdated { .. },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        start_vm(&mut c, vm.id).await;
        let ticket = open_console(&mut c, vm.id).await;
        let seen = rfb_watch(&ticket);
        let ok = wait_installed_desktop(&seen, Duration::from_secs(180)).await;
        save_png(
            &seen.lock().unwrap().clone(),
            &evidence.join("f2-exit-3-first-boot-from-disk.png"),
        );
        assert!(ok, "the installed system did not appear");
        let took = stop_gracefully(&mut c, vm.id, Duration::from_secs(120)).await;
        eprintln!("graceful stop of the installed system: {took:?}");
        svc.handle.shutdown().await;
        vm.id
    });
    drop(rt);

    // ----- "reopen LightVM": new service on the same catalog ------------------
    let (engine, host) = real_service_parts(&dir, private_root, &cpu);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (handle, ui) = start(config(&dir), engine, host).await.unwrap();
        let svc = Svc {
            handle,
            ui,
            calls: Calls::default(),
        };
        let (mut c, _) = connect_ui(&svc).await;
        let vms = match req(&mut c, Request::ListVms).await {
            ServerMessage::Response {
                result: ResponseBody::VmList { vms },
                ..
            } => vms,
            other => panic!("{other:?}"),
        };
        let vm = vms
            .iter()
            .find(|v| v.vm_id == vm_id)
            .expect("VM after reopening");
        eprintln!(
            "after reopening: {} {:?}, install media {:?}, disk_used_bytes {:?}",
            vm.name, vm.state, vm.config.install_media, vm.disk_used_bytes
        );
        assert_eq!(vm.state, VmState::Stopped);
        assert_eq!(vm.config.install_media, None);
        start_vm(&mut c, vm_id).await;
        let ticket = open_console(&mut c, vm_id).await;
        let seen = rfb_watch(&ticket);
        let ok = wait_installed_desktop(&seen, Duration::from_secs(180)).await;
        save_png(
            &seen.lock().unwrap().clone(),
            &evidence.join("f2-exit-4-boot-after-reopen.png"),
        );
        assert!(ok, "the installed system did not appear after reopening");
        let took = stop_gracefully(&mut c, vm_id, Duration::from_secs(120)).await;
        eprintln!("graceful stop after reopening: {took:?}");
        let rev = status(&mut c, vm_id).await.config_revision;
        match req(
            &mut c,
            Request::DeleteVm {
                vm_id,
                expected_revision: rev,
            },
        )
        .await
        {
            ServerMessage::Response {
                result: ResponseBody::VmDeleted { .. },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        svc.handle.shutdown().await;
        let events = read_audit(&dir);
        let n = |a: AuditAction, o: AuditOutcome| {
            events
                .iter()
                .filter(|e| e.action == a && e.outcome == o)
                .count()
        };
        eprintln!(
            "audit: {} lines; vm_start succeeded {}, vm_stop succeeded {}, vm_configure succeeded {}",
            events.len(),
            n(AuditAction::VmStart, AuditOutcome::Succeeded),
            n(AuditAction::VmStop, AuditOutcome::Succeeded),
            n(AuditAction::VmConfigure, AuditOutcome::Succeeded)
        );
    });
}
