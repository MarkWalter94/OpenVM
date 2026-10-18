//! Concurrent `session_request` calls of one adapter (code review fix): the
//! second call must not replace the first request's connection and state,
//! otherwise the first request's background task closes the approved
//! session, the lease loop stops and the supervisor's lease expires.
//! Real service (real IPC, real core, SQLite catalog, fake backend); the
//! adapter is driven in process through `Adapter::call`.

use std::sync::Arc;
use std::time::Duration;

use lightvm_mcp::{Adapter, AdapterConfig};
use lightvm_proto::ipc::{Request, ResponseBody, Role, ServerMessage, SessionApproval};
use lightvm_proto::{Capability, Label, ReplyError, VmId};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FakeHost, FakeImageTool, FakeVmBackend, fake_qcow2, host_capacity, sample_config,
};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(20);

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

/// Tool result as JSON (`isError`, `structuredContent`, `content`).
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

fn error_code(r: &Value) -> Option<String> {
    if r["isError"] != json!(true) {
        return None;
    }
    let text = r["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    Some(v["error"]["code"].as_str().unwrap().to_owned())
}

async fn start_svc(name: &str) -> (ServiceHandle, Client, std::path::PathBuf) {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).unwrap();
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
    let mut cfg = ServiceConfig::new(&dir);
    cfg.runtime_dir = Some(dir.join("run"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(FakeHost { calls }))
        .await
        .expect("service start");
    let mut c = Client::connect(handle.endpoint()).await.unwrap();
    match c.hello(&ui).await.unwrap() {
        ServerMessage::HelloOk { role: Role::Ui, .. } => {}
        other => panic!("{other:?}"),
    }
    (handle, c, dir)
}

async fn vm_with_baseline(u: &mut Client, dir: &std::path::Path) -> VmId {
    let vm = match ok(
        u,
        Request::CreateVm {
            config: sample_config("race"),
        },
    )
    .await
    {
        ResponseBody::VmCreated { vm } => vm.id,
        other => panic!("{other:?}"),
    };
    let d = dir.join("vms").join(vm.to_string());
    std::fs::create_dir_all(d.join("firmware")).unwrap();
    std::fs::write(d.join("primary.qcow2"), fake_qcow2(None, b"clean")).unwrap();
    std::fs::write(d.join("firmware").join("primary.vars.fd"), b"VARS").unwrap();
    let b = match ok(
        u,
        Request::CreateBaseline {
            vm_id: vm,
            label: Label::parse("initial").unwrap(),
            allow_unclean: false,
        },
    )
    .await
    {
        ResponseBody::BaselineCreated { baseline } => baseline.baseline_id,
        other => panic!("{other:?}"),
    };
    ok(
        u,
        Request::RestoreBaseline {
            vm_id: vm,
            baseline_id: b,
        },
    )
    .await;
    vm
}

async fn pairing_file(
    u: &mut Client,
    handle: &ServiceHandle,
    dir: &std::path::Path,
) -> std::path::PathBuf {
    let cred = match ok(u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => credential,
        other => panic!("{other:?}"),
    };
    let dir = dir.join("pairing");
    std::fs::create_dir_all(&dir).unwrap();
    lightvm_mcp::pairing::restrict_folder(&dir).unwrap();
    let p = dir.join("pairing.json");
    let expires = lightvm_proto::UtcTimestamp::now()
        .checked_add_millis(600_000)
        .unwrap();
    std::fs::write(
        &p,
        json!({
            "format": "lightvm-pairing-v1",
            "endpoint": handle.endpoint(),
            "credential": cred,
            "expires_at": expires.to_string(),
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_session_requests_do_not_close_the_approved_session() {
    let (handle, mut u, dir) = start_svc("mcp-session-race").await;
    let vm = vm_with_baseline(&mut u, &dir).await;
    ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
    let p = pairing_file(&mut u, &handle, &dir).await;
    let a = Adapter::new(AdapterConfig {
        pairing_file: Some(p),
        renew_every: Duration::from_millis(200),
        ..AdapterConfig::default()
    });

    // Both calls in flight together: one request reaches the service, the
    // other is refused (or sees the pending request), never a second one.
    let args = json!({"purpose": "race test"});
    let (r1, r2) = tokio::join!(
        call(&a, "session_request", args.clone()),
        call(&a, "session_request", args.clone())
    );
    let codes = [error_code(&r1), error_code(&r2)];
    assert!(
        codes.iter().any(Option::is_none),
        "one request succeeds: {r1} {r2}"
    );
    for c in codes.iter().flatten() {
        assert_eq!(c, "session_pending", "{r1} {r2}");
    }
    let requests = match ok(&mut u, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => requests,
        other => panic!("{other:?}"),
    };
    assert_eq!(requests.len(), 1, "{requests:?}");
    let req = requests.into_iter().next().unwrap();
    ok(
        &mut u,
        Request::ApproveSession {
            request_id: req.request_id,
            approval: SessionApproval {
                tools: Default::default(),
                vm_id: vm,
                capabilities: Capability::VM_CONTROLS.into_iter().collect(),
                duration_s: 3600,
                quotas: None,
                lease_ms: None,
                give_input: false,
                network_endpoints: Vec::new(),
            },
        },
    )
    .await;
    let s = call(&a, "session_status", json!({"wait_s": 10})).await;
    assert_eq!(s["structuredContent"]["state"], "active", "{s}");

    // Several lease periods later the session is still active and usable.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let s = call(&a, "session_status", json!({"wait_s": 0})).await;
    assert_eq!(s["structuredContent"]["state"], "active", "{s}");
    let l = call(&a, "vm_list", json!({})).await;
    assert_eq!(error_code(&l), None, "{l}");
    let e = call(&a, "session_end", json!({})).await;
    assert_eq!(error_code(&e), None, "{e}");
    handle.shutdown().await;
}
