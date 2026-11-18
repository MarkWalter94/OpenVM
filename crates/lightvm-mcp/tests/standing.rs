//! ADR-0039 §4.9 — the adapter under a standing grant: a session obtained
//! without any approval in the app, renewed by itself when it ends (lease,
//! restart of the service), never renewed after a revocation or an explicit
//! `session_end`. Real service (real IPC, real core, SQLite catalog, fake
//! VM backend); the adapter is driven in process through `Adapter::call`;
//! the test writes the pairing file as the app does.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightvm_mcp::{Adapter, AdapterConfig};
use lightvm_proto::ipc::{Request, ResponseBody, Role, ServerMessage, SessionInfo};
use lightvm_proto::{
    Capability, CapabilitySet, OperationId, ReplyError, StandingGrantSpec, UtcTimestamp, VmAction,
    VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FakeHost, FakeImageTool, FakeVmBackend, fake_qcow2, host_capacity, sample_config,
};
use lightvm_service::lock::LockError;
use lightvm_service::server::StartError;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(20);

// ----- service ------------------------------------------------------------------

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    dir: PathBuf,
}

fn short_dir(prefix: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("{prefix}{:x}{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_in(dir: &Path) -> Result<Svc, StartError> {
    let store = SqliteStore::open(dir).unwrap();
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let calls = Calls::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        image_tool: Some(Arc::new(FakeImageTool {
            convert_delay: Duration::ZERO,
            calls: calls.clone(),
            ..FakeImageTool::default()
        })),
        ..FakeVmBackend::default()
    };
    let inspector = ManagedMediaInspector::new(store.root());
    let mut host = host_capacity();
    host.baseline_available = true;
    let engine = CoreEngine::new(store, audit, backend, inspector, core_config(host)).unwrap();
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.display_dir = Some(short_dir("lvsg"));
    cfg.standing_min_duration = Duration::from_secs(1);
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(FakeHost { calls })).await?;
    Ok(Svc {
        handle,
        ui,
        dir: dir.to_path_buf(),
    })
}

async fn start_svc(name: &str) -> Svc {
    start_in(&temp_dir(name)).await.expect("service start")
}

/// Restart on the same catalog (the previous instance releases its lock).
async fn restart(svc: Svc) -> Svc {
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    let mut last = None;
    for _ in 0..100 {
        match start_in(&dir).await {
            Ok(s) => return s,
            Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => last = Some(e),
        }
    }
    panic!("restart: {last:?}");
}

async fn ui(svc: &Svc) -> Client {
    let mut c = Client::connect(svc.handle.endpoint()).await.unwrap();
    match c.hello(&svc.ui).await.unwrap() {
        ServerMessage::HelloOk { role: Role::Ui, .. } => c,
        other => panic!("{other:?}"),
    }
}

async fn ok(c: &mut Client, body: Request) -> ResponseBody {
    let name = body.name();
    match tokio::time::timeout(T, c.request(body))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Response { result, .. } => result,
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => panic!("{name}: {e:?}"),
        other => panic!("{name}: {other:?}"),
    }
}

async fn vm_with_disk(u: &mut Client, svc: &Svc) -> VmId {
    let vm = match ok(
        u,
        Request::CreateVm {
            config: sample_config("standing-mcp"),
        },
    )
    .await
    {
        ResponseBody::VmCreated { vm } => vm.id,
        other => panic!("{other:?}"),
    };
    let d = svc.dir.join("vms").join(vm.to_string());
    std::fs::create_dir_all(d.join("firmware")).unwrap();
    std::fs::write(d.join("primary.qcow2"), fake_qcow2(None, b"installed")).unwrap();
    std::fs::write(d.join("firmware").join("primary.vars.fd"), b"VARS").unwrap();
    vm
}

async fn vm_state(u: &mut Client, vm: VmId) -> VmState {
    match ok(u, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => vm.state,
        other => panic!("{other:?}"),
    }
}

async fn ui_start(u: &mut Client, vm: VmId) {
    ok(
        u,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        },
    )
    .await;
    let deadline = Instant::now() + T;
    while vm_state(u, vm).await != VmState::Running {
        assert!(Instant::now() < deadline, "VM not running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn sessions(u: &mut Client) -> Vec<SessionInfo> {
    match ok(u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    }
}

async fn live_sessions(u: &mut Client) -> usize {
    sessions(u)
        .await
        .iter()
        .filter(|s| s.termination.is_none())
        .count()
}

async fn pending_requests(u: &mut Client) -> usize {
    match ok(u, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => requests.len(),
        other => panic!("{other:?}"),
    }
}

const GRANT_CAPS: [Capability; 4] = [
    Capability::VmList,
    Capability::VmStatus,
    Capability::VmStart,
    Capability::VmStop,
];

/// Standing grant created by the user in the app: its credential and
/// expiry.
async fn create_grant(
    u: &mut Client,
    vm: VmId,
    lease_ms: Option<u32>,
) -> (lightvm_proto::StandingGrantId, String, UtcTimestamp) {
    let spec = StandingGrantSpec {
        vm_id: vm,
        capabilities: GRANT_CAPS.into_iter().collect::<CapabilitySet>(),
        tools: Default::default(),
        network_endpoints: Vec::new(),
        quotas: None,
        lease_ms,
        give_input: false,
        duration_s: 3600,
        client_name: None,
    };
    match ok(u, Request::CreateStandingGrant { spec }).await {
        ResponseBody::StandingGrantCreated { grant, credential } => {
            (grant.grant_id, credential, grant.expires_at)
        }
        other => panic!("{other:?}"),
    }
}

/// Pairing file as the app writes it for a standing grant (same format;
/// expiry = the grant's), rewritten with the new endpoint after a restart.
fn write_pairing(dir: &Path, endpoint: &str, credential: &str, expires: UtcTimestamp) -> PathBuf {
    let dir = dir.join("pairing");
    std::fs::create_dir_all(&dir).unwrap();
    lightvm_mcp::pairing::restrict_folder(&dir).unwrap();
    let p = dir.join("pairing.json");
    let tmp = dir.join("pairing.json.tmp");
    std::fs::write(
        &tmp,
        json!({
            "format": "lightvm-pairing-v1",
            "endpoint": endpoint,
            "credential": credential,
            "expires_at": expires.to_string(),
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::rename(&tmp, &p).unwrap();
    p
}

fn adapter(pairing: PathBuf, renew_every: Duration) -> Adapter {
    Adapter::new(AdapterConfig {
        pairing_file: Some(pairing),
        rate_burst: 1000,
        rate_per_second: 1000.0,
        renew_every,
        standing_backoff_min: Duration::from_millis(100),
        standing_backoff_max: Duration::from_millis(400),
        ..AdapterConfig::default()
    })
}

// ----- adapter ------------------------------------------------------------------

async fn call(a: &Adapter, name: &str, args: Value) -> Value {
    let Value::Object(args) = args else {
        panic!("arguments must be an object")
    };
    let r = a
        .call(name, Some(args))
        .await
        .expect("tool in the catalogue");
    serde_json::to_value(r).unwrap()
}

fn error(r: &Value) -> Option<Value> {
    if r["isError"] != json!(true) {
        return None;
    }
    let text = r["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    Some(v["error"].clone())
}

async fn status(a: &Adapter) -> Value {
    let r = call(a, "session_status", json!({"wait_s": 0})).await;
    assert_eq!(error(&r), None, "{r}");
    r["structuredContent"].clone()
}

/// Polls `session_status` until `f` holds (bounded).
async fn wait_status(a: &Adapter, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + T;
    loop {
        let s = status(a).await;
        if f(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "{what}: last status {s}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn session_id(s: &Value) -> String {
    s["session_id"].as_str().unwrap().to_owned()
}

/// `session_request` under the grant: active at once, no UI involved.
async fn standing_session(a: &Adapter, u: &mut Client, vm: VmId) -> String {
    let r = call(a, "session_request", json!({"purpose": "unattended test"})).await;
    assert_eq!(error(&r), None, "{r}");
    let s = &r["structuredContent"];
    assert_eq!(s["state"], "active", "{s}");
    assert_eq!(s["standing"], true, "{s}");
    assert!(s.get("verification_code").is_none(), "{s}");
    assert_eq!(s["vm_ids"], json!([vm.to_string()]), "{s}");
    let caps: Vec<&str> = s["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(caps.len(), GRANT_CAPS.len(), "grant ∩ requested: {s}");
    assert_eq!(pending_requests(u).await, 0, "nothing queued for the user");
    session_id(s)
}

// ----- tests ----------------------------------------------------------------------

/// (a) A session obtained without any approval in the app, usable at once;
/// `help` documents the mode.
#[tokio::test(flavor = "multi_thread")]
async fn standing_session_without_ui_approval() {
    let svc = start_svc("mcp-sg-auto").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc).await;
    let (_, cred, expires) = create_grant(&mut u, vm, None).await;
    let p = write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    // The agent access switch stays off: the grant alone decides.
    assert!(!svc.handle.agent_access());
    let a = adapter(p, Duration::from_millis(200));
    standing_session(&a, &mut u, vm).await;
    let l = call(&a, "vm_list", json!({})).await;
    assert_eq!(error(&l), None, "{l}");
    assert_eq!(
        l["structuredContent"]["vms"][0]["vm_id"],
        json!(vm.to_string())
    );
    // A second request while active returns the same session.
    let s1 = status(&a).await;
    let r = call(&a, "session_request", json!({"purpose": "again"})).await;
    assert_eq!(r["structuredContent"]["session_id"], s1["session_id"]);
    // help: topic and session state.
    let h = call(&a, "help", json!({"topic": "standing"})).await;
    let g = h["structuredContent"]["guide"].as_str().unwrap();
    for needle in ["7 days", "run_id", "keeps", "session_end", "renewing"] {
        assert!(g.contains(needle), "{needle}");
    }
    assert_eq!(h["structuredContent"]["session_state"], "active");
    let e = call(&a, "session_end", json!({})).await;
    assert_eq!(error(&e), None, "{e}");
    svc.handle.shutdown().await;
}

/// (b) A grant with a 1 s lease: the adapter renews at a third of the
/// granted lease (never lets it expire although its configured period is
/// longer); when the session ends (here revoked by the user, the same
/// `closing` the agent sees for an expiry) a new session is obtained by
/// itself and the VM keeps running. Stopping the agent needs the revocation
/// of the grant (test d).
#[tokio::test(flavor = "multi_thread")]
async fn ended_session_is_renewed_automatically() {
    let svc = start_svc("mcp-sg-lease").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc).await;
    ui_start(&mut u, vm).await;
    let (_, cred, expires) = create_grant(&mut u, vm, Some(1000)).await;
    let p = write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    let a = adapter(p, Duration::from_millis(2500));
    let first = standing_session(&a, &mut u, vm).await;
    // The 1 s lease outlives several of its own periods.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let s = call(&a, "session_status", json!({"wait_s": 0})).await;
    let s = &s["structuredContent"];
    assert_eq!(s["session_id"].as_str(), Some(first.as_str()), "{s}");
    let sid = first.parse().unwrap();
    assert!(matches!(
        ok(&mut u, Request::RevokeSession { session_id: sid }).await,
        ResponseBody::SessionRevoked { .. }
    ));
    let s = wait_status(&a, "new session after the end", |s| {
        s["state"] == "active" && s["session_id"].as_str() != Some(first.as_str())
    })
    .await;
    assert_eq!(s["standing"], true, "{s}");
    let terminated = sessions(&mut u)
        .await
        .into_iter()
        .find(|x| x.session_id.to_string() == first)
        .and_then(|x| x.termination);
    assert_eq!(terminated, Some(lightvm_proto::TerminationCause::Revoked));
    assert_eq!(vm_state(&mut u, vm).await, VmState::Running);
    assert_eq!(pending_requests(&mut u).await, 0);
    svc.handle.shutdown().await;
}

/// (c) A restart of the service: the adapter re-reads the pairing file
/// (rewritten with the new endpoint, as the app does), reconnects and gets
/// a new session; meanwhile VM tools report `session_renewing`.
#[tokio::test(flavor = "multi_thread")]
async fn reconnects_after_a_service_restart() {
    let svc = start_svc("mcp-sg-restart").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc).await;
    let (_, cred, expires) = create_grant(&mut u, vm, None).await;
    let p = write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    let a = adapter(p, Duration::from_millis(200));
    let first = standing_session(&a, &mut u, vm).await;
    drop(u);
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    let s = wait_status(&a, "renewing after the shutdown", |s| {
        s["state"] == "renewing"
    })
    .await;
    assert_eq!(s["standing"], true, "{s}");
    let l = call(&a, "vm_list", json!({})).await;
    let e = error(&l).expect("vm_list refused while renewing");
    assert_eq!(e["code"], "session_renewing", "{e}");
    assert_eq!(e["retryable"], true, "{e}");
    // Some attempts fail while the service is down (backoff, no tight loop).
    let s = wait_status(&a, "failed attempts", |s| {
        s["renewal"]["attempt"].as_u64().unwrap_or(0) >= 2
    })
    .await;
    assert!(s["renewal"]["last_error"].is_string(), "{s}");
    assert!(
        s["renewal"]["attempt"].as_u64().unwrap() < 20,
        "no tight loop: {s}"
    );
    let svc = {
        let mut out = None;
        for _ in 0..100 {
            match start_in(&dir).await {
                Ok(s) => {
                    out = Some(s);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        out.expect("restart")
    };
    write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    let s = wait_status(&a, "new session after the restart", |s| {
        s["state"] == "active"
    })
    .await;
    assert_ne!(session_id(&s), first);
    assert_eq!(s["standing"], true, "{s}");
    let l = call(&a, "vm_list", json!({})).await;
    assert_eq!(error(&l), None, "{l}");
    let mut u = ui(&svc).await;
    assert_eq!(live_sessions(&mut u).await, 1);
    // A second restart with the helper, the pairing rewritten again.
    drop(u);
    let svc = restart(svc).await;
    write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    let s = wait_status(&a, "new session after the second restart", |s| {
        s["state"] == "active"
    })
    .await;
    assert_eq!(s["standing"], true, "{s}");
    svc.handle.shutdown().await;
}

/// (d) Revocation of the grant: the credential is refused, the adapter
/// stops renewing and reports a closed state.
#[tokio::test(flavor = "multi_thread")]
async fn revocation_stops_the_renewal() {
    let svc = start_svc("mcp-sg-revoke").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc).await;
    ui_start(&mut u, vm).await;
    let (grant_id, cred, expires) = create_grant(&mut u, vm, None).await;
    let p = write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    let a = adapter(p, Duration::from_millis(200));
    standing_session(&a, &mut u, vm).await;
    assert!(matches!(
        ok(&mut u, Request::RevokeStandingGrant { grant_id }).await,
        ResponseBody::StandingGrantRevoked { .. }
    ));
    let s = wait_status(&a, "closed after the revocation", |s| {
        s["state"] == "closed"
    })
    .await;
    assert!(s.get("standing").is_none(), "{s}");
    assert!(
        s["reason"]
            .as_str()
            .unwrap()
            .contains("pairing_unavailable"),
        "{s}"
    );
    // No further attempt: still closed, nothing live, nothing pending.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let s = status(&a).await;
    assert_eq!(s["state"], "closed", "{s}");
    assert_eq!(live_sessions(&mut u).await, 0);
    assert_eq!(pending_requests(&mut u).await, 0);
    let l = call(&a, "vm_list", json!({})).await;
    assert_eq!(error(&l).unwrap()["code"], "session_closed");
    // Revocation leaves the VM running.
    assert_eq!(vm_state(&mut u, vm).await, VmState::Running);
    svc.handle.shutdown().await;
}

/// (e) An explicit `session_end` ends the session without any renewal; a
/// later `session_request` obtains a new one under the live grant.
#[tokio::test(flavor = "multi_thread")]
async fn explicit_session_end_does_not_renew() {
    let svc = start_svc("mcp-sg-end").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc).await;
    let (_, cred, expires) = create_grant(&mut u, vm, None).await;
    let p = write_pairing(&svc.dir, svc.handle.endpoint(), &cred, expires);
    let a = adapter(p, Duration::from_millis(200));
    let first = standing_session(&a, &mut u, vm).await;
    let e = call(&a, "session_end", json!({})).await;
    assert_eq!(error(&e), None, "{e}");
    assert_eq!(e["structuredContent"]["session_id"], json!(first));
    let s = status(&a).await;
    assert_eq!(s["state"], "closed", "{s}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let s = status(&a).await;
    assert_eq!(s["state"], "closed", "no renewal after session_end: {s}");
    assert_eq!(live_sessions(&mut u).await, 0);
    assert_eq!(sessions(&mut u).await.len(), 1, "no new session");
    // A new explicit request under the live grant.
    let second = standing_session(&a, &mut u, vm).await;
    assert_ne!(second, first);
    let e = call(&a, "session_end", json!({})).await;
    assert_eq!(error(&e), None, "{e}");
    svc.handle.shutdown().await;
}
