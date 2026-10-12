//! F5 protocol tests of the real adapter over stdio against a real service
//! (real IPC, real core, SQLite catalog, fake VM backend and image tool; no
//! QEMU): handshake and negotiation, tools/list, each tool's success and
//! denial, wrong VM, host path attempts, pause and revocation, oversized
//! screenshot, rate limit (`docs/verification/F5-01-mcp-reports.md`).
//!
//! The MCP client is hand-written (newline-delimited JSON-RPC 2.0 over the
//! child's stdin/stdout, MCP 2025-11-25) so the test does not depend on the
//! SDK it tests. Every stdout line must be JSON-RPC; stderr is collected
//! and checked for secrets.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightvm_proto::ipc::{Request, ResponseBody, Role, ServerMessage, SessionApproval};
use lightvm_proto::report::{OutcomeSource, ReportEventBody};
use lightvm_proto::{
    BaselineId, Capability, CapabilitySet, Label, OperationId, Quotas, ReplyError, SessionId,
    TestOutcome, VmId,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FakeHost, FakeImageTool, FakeVmBackend, fake_qcow2, host_capacity, sample_config,
};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(20);

// ----- service ------------------------------------------------------------------

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    dir: PathBuf,
    calls: Calls,
}

fn short_dir(prefix: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("{prefix}{:x}{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A real 1280×800 RGB PNG (grey), as the backend would return.
fn real_png() -> Vec<u8> {
    let (w, h) = (1280u32, 800u32);
    let rgb = vec![90u8; (w * h * 3) as usize];
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }
    out
}

async fn start_svc(name: &str) -> Svc {
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
        screenshot_png: real_png(),
        ..FakeVmBackend::default()
    };
    let inspector = ManagedMediaInspector::new(store.root());
    // Baselines available, as with the real QEMU backend (the fake image
    // tool stands in for qemu-img).
    let mut host = host_capacity();
    host.baseline_available = true;
    let engine = CoreEngine::new(store, audit, backend, inspector, core_config(host)).unwrap();
    let mut cfg = ServiceConfig::new(&dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.display_dir = Some(short_dir("lvd"));
    let (handle, ui) = start(
        cfg,
        Box::new(engine),
        Box::new(FakeHost {
            calls: calls.clone(),
        }),
    )
    .await
    .expect("service start");
    Svc {
        handle,
        ui,
        dir,
        calls,
    }
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
        other => panic!("{name}: {other:?}"),
    }
}

async fn ui_err(c: &mut Client, body: Request) -> lightvm_proto::PublicError {
    match tokio::time::timeout(T, c.request(body))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => e,
        other => panic!("{other:?}"),
    }
}

/// A VM with a standalone disk (as after a first start) and an initial
/// baseline restored, so the session starts on an overlay of it.
async fn vm_with_baseline(u: &mut Client, svc: &Svc, name: &str) -> (VmId, BaselineId) {
    let vm = match ok(
        u,
        Request::CreateVm {
            config: sample_config(name),
        },
    )
    .await
    {
        ResponseBody::VmCreated { vm } => vm.id,
        other => panic!("{other:?}"),
    };
    let d = svc.dir.join("vms").join(vm.to_string());
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
    (vm, b)
}

/// Pairing file as the app writes it (format `lightvm-pairing-v1`).
async fn pairing_file(u: &mut Client, svc: &Svc) -> PathBuf {
    let cred = match ok(u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => credential,
        other => panic!("{other:?}"),
    };
    let dir = svc.dir.join("pairing");
    std::fs::create_dir_all(&dir).unwrap();
    // Private to the user, as the app makes it.
    lightvm_mcp::pairing::restrict_folder(&dir).unwrap();
    let p = dir.join("pairing.json");
    let expires = lightvm_proto::UtcTimestamp::now()
        .checked_add_millis(600_000)
        .unwrap();
    std::fs::write(
        &p,
        json!({
            "format": "lightvm-pairing-v1",
            "endpoint": svc.handle.endpoint(),
            "credential": cred,
            "expires_at": expires.to_string(),
        })
        .to_string(),
    )
    .unwrap();
    // The app writes the file with mode 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    p
}

fn all_caps() -> CapabilitySet {
    Capability::ALL.into_iter().collect()
}

fn approval(vm: VmId, caps: CapabilitySet, give_input: bool) -> SessionApproval {
    SessionApproval {
        vm_id: vm,
        capabilities: caps,
        duration_s: 3600,
        quotas: None,
        lease_ms: None,
        give_input,
    }
}

// ----- MCP client -------------------------------------------------------------------

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    next: u64,
}

impl Mcp {
    fn spawn(pairing: Option<&Path>) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lightvm-mcp"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("LIGHTVM_PAIRING_FILE");
        match pairing {
            Some(p) => {
                cmd.env("LIGHTVM_PAIRING_FILE", p);
            }
            None => {
                cmd.env("LIGHTVM_PAIRING_FILE", "Z:\\no-such-dir\\pairing.json");
            }
        }
        let mut child = cmd.spawn().expect("adapter start");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let err_buf = stderr.clone();
        let se = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(se).lines().map_while(Result::ok) {
                err_buf.lock().unwrap().push_str(&line);
                err_buf.lock().unwrap().push('\n');
            }
        });
        Self {
            child,
            stdin,
            lines: rx,
            stderr,
            next: 1,
        }
    }

    fn send(&mut self, msg: &Value) {
        let s = self.stdin.as_mut().unwrap();
        let mut t = msg.to_string();
        t.push('\n');
        s.write_all(t.as_bytes()).unwrap();
        s.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let line = match self.lines.recv_timeout(T) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => panic!("no MCP message within {T:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("adapter stdout closed"),
        };
        let v: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout carries only JSON-RPC: {e}: {line:?}"));
        assert_eq!(v["jsonrpc"], "2.0", "{v}");
        v
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let v = self.recv();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn initialize(&mut self, version: &str) -> Value {
        let r = self.request(
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "f5-test-client", "version": "1.0"}
            }),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    /// `tools/call`: the result object. Calls are spaced below the
    /// adapter's rate limit (10/s); the rate test pipelines on purpose.
    fn call(&mut self, name: &str, args: Value) -> Value {
        std::thread::sleep(Duration::from_millis(110));
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(r.get("error").is_none(), "{name}: protocol error {r}");
        r["result"].clone()
    }

    /// Successful call: the structured content (checked against the text copy).
    fn ok(&mut self, name: &str, args: Value) -> Value {
        let r = self.call(name, args);
        assert_ne!(r["isError"], json!(true), "{name}: {r}");
        r["structuredContent"].clone()
    }

    /// Failed call (`isError: true`): the error object.
    fn err(&mut self, name: &str, args: Value) -> Value {
        let r = self.call(name, args);
        assert_eq!(
            r["isError"],
            json!(true),
            "{name}: expected an error, got {r}"
        );
        assert!(r.get("structuredContent").is_none());
        let text = r["content"][0]["text"].as_str().unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        v["error"].clone()
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Adapter initialized, session requested and approved by the UI (the
/// test plays the user's click in the app).
async fn session(
    svc: &Svc,
    u: &mut Client,
    vm: VmId,
    caps: CapabilitySet,
    give_input: bool,
) -> (Mcp, SessionId) {
    ok(u, Request::SetAgentAccess { enabled: true }).await;
    let p = pairing_file(u, svc).await;
    let mut m = Mcp::spawn(Some(&p));
    m.initialize("2025-11-25");
    let r = m.ok("session_request", json!({"purpose": "F5 protocol test"}));
    assert_eq!(r["state"], "pending");
    let code = r["verification_code"].as_str().unwrap().to_owned();
    let req = match ok(u, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => requests.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        req.verification_code, code,
        "same code in the app and the client"
    );
    assert!(
        req.client_name.contains("f5-test-client"),
        "{}",
        req.client_name
    );
    assert_eq!(req.purpose, "F5 protocol test");
    ok(
        u,
        Request::ApproveSession {
            request_id: req.request_id,
            approval: approval(vm, caps, give_input),
        },
    )
    .await;
    let s = m.ok("session_status", json!({"wait_s": 10}));
    assert_eq!(s["state"], "active", "{s}");
    let sid: SessionId = s["session_id"].as_str().unwrap().parse().unwrap();
    (m, sid)
}

fn audit_text(svc: &Svc) -> String {
    std::fs::read_to_string(svc.dir.join("audit").join("audit.jsonl")).unwrap_or_default()
}

// ----- tests ------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn handshake_tools_list_and_protocol_errors() {
    let mut m = Mcp::spawn(None);
    let r = m.initialize("2025-11-25");
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25", "{r}");
    assert_eq!(r["result"]["serverInfo"]["name"], "lightvm");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    assert!(
        r["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("help tool is the reference"),
        "{r}"
    );
    let list = m.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in [
        "session_request",
        "session_status",
        "vm_list",
        "vm_status",
        "vm_start",
        "vm_stop",
        "vm_reset",
        "vm_screenshot",
        "vm_mouse_move",
        "vm_mouse_click",
        "vm_keyboard_type",
        "vm_keyboard_press",
        "vm_snapshot",
        "vm_restore",
        "vm_logs",
        "operation_get",
        "report_get",
    ] {
        assert!(names.contains(&n), "{n}");
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert_eq!(t["outputSchema"]["type"], "object", "{t}");
        let title = t["title"].as_str().unwrap();
        assert!(
            title.starts_with("LightVM ") && (title.contains('.') || title == "LightVM help"),
            "{title}"
        );
    }
    // The guide is the first tool and works before any session.
    assert_eq!(tools[0]["name"], "help");
    let r = m.request(
        "tools/call",
        json!({"name": "help", "arguments": {"topic": "tools"}}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("## vm_keyboard_type"), "{text}");
    assert_eq!(r["result"]["structuredContent"]["session_state"], "none");
    // Unknown tool: protocol error -32602, not a tool result.
    let r = m.request(
        "tools/call",
        json!({"name": "qmp_execute", "arguments": {}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // Invalid arguments: tool execution error (isError), not protocol.
    let e = m.err("vm_status", json!({"vm_id": "x", "extra": 1}));
    assert_eq!(e["code"], "invalid_argument");
    // No session: nothing works, nothing is connected.
    let e = m.err("vm_list", json!({}));
    assert_eq!(e["code"], "no_session");
    // No pairing file: the session tool explains, without content.
    let e = m.err("session_request", json!({"purpose": "x"}));
    assert_eq!(e["code"], "pairing_unavailable");
    // A client offering an older revision gets the baseline (it must
    // disconnect if it does not support it).
    let mut m2 = Mcp::spawn(None);
    let r = m2.initialize("2024-11-05");
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test(flavor = "multi_thread")]
async fn adapter_starts_nothing_and_respects_the_access_switch() {
    let svc = start_svc("f5-mcp-switch").await;
    let mut u = ui(&svc).await;
    let (vm, _) = vm_with_baseline(&mut u, &svc, "switch").await;
    let p = pairing_file(&mut u, &svc).await;
    let mut m = Mcp::spawn(Some(&p));
    m.initialize("2025-11-25");
    m.request("tools/list", json!({}));
    // The adapter connected to nothing yet (only the UI connection).
    match ok(&mut u, Request::ListConnections).await {
        ResponseBody::Connections { connections } => {
            assert!(
                connections.iter().all(|c| c.role == Role::Ui),
                "{connections:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    // A pairing file in a folder other users can read is not read.
    let open_dir = svc.dir.join("open-pairing");
    std::fs::create_dir_all(&open_dir).unwrap();
    let open_file = open_dir.join("pairing.json");
    std::fs::copy(&p, &open_file).unwrap();
    let mut m2 = Mcp::spawn(Some(&open_file));
    m2.initialize("2025-11-25");
    let e = m2.err("session_request", json!({"purpose": "x"}));
    assert_eq!(e["code"], "pairing_unavailable", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("not private"),
        "{e}"
    );
    assert!(
        e["message"].as_str().unwrap().contains("Agents first"),
        "{e}"
    );
    // Agent access off (default at every start): refused, not turned on.
    let e = m.err("session_request", json!({"purpose": "x"}));
    assert_eq!(e["code"], "permission_denied", "{e}");
    assert_eq!(e["details"]["reason"], "agent_access_disabled");
    assert!(matches!(
        ok(&mut u, Request::GetAgentAccess).await,
        ResponseBody::AgentAccess { enabled: false }
    ));
    assert!(svc.calls.lock().unwrap().iter().all(|c| c != "vm_start"));
    let _ = vm;
    // The pairing credential and the session token never reach stderr.
    let cred = std::fs::read_to_string(&p).unwrap();
    let v: Value = serde_json::from_str(&cred).unwrap();
    assert!(!m.stderr().contains(v["credential"].as_str().unwrap()));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_tool_through_an_approved_session() {
    let svc = start_svc("f5-mcp-tools").await;
    let mut u = ui(&svc).await;
    let (vm, initial) = vm_with_baseline(&mut u, &svc, "tools").await;
    let (mut m, sid) = session(&svc, &mut u, vm, all_caps(), true).await;
    let v = vm.to_string();

    let l = m.ok("vm_list", json!({}));
    assert_eq!(l["vms"].as_array().unwrap().len(), 1);
    assert_eq!(l["vms"][0]["vm_id"], v);
    let st = m.ok("vm_status", json!({"vm_id": v}));
    assert_eq!(st["state"], "stopped");
    // Snapshot and restore refuse a running VM; checkpoint while stopped.
    let snap = m.ok("vm_snapshot", json!({"vm_id": v, "label": "before test"}));
    assert!(snap["label"].as_str().unwrap().starts_with("agent: "));
    let started = m.ok("vm_start", json!({"vm_id": v}));
    assert_eq!(started["kind"], "start");
    let st = m.ok("vm_status", json!({"vm_id": v}));
    assert_eq!(st["state"], "running");
    assert_eq!(st["input_owner"], true);
    let e = m.err("vm_snapshot", json!({"vm_id": v, "label": "x"}));
    assert_eq!(e["code"], "state_conflict", "{e}");
    let e = m.err(
        "vm_restore",
        json!({"vm_id": v, "baseline_id": initial.to_string()}),
    );
    assert_eq!(e["code"], "state_conflict", "{e}");

    // Screenshot: image content + structured metadata, full and half.
    let r = m.call("vm_screenshot", json!({"vm_id": v}));
    assert_ne!(r["isError"], json!(true), "{r}");
    assert_eq!(r["content"][0]["type"], "image");
    assert_eq!(r["content"][0]["mimeType"], "image/png");
    assert_eq!(r["structuredContent"]["image_width"], 1280);
    assert_eq!(r["structuredContent"]["display"]["height"], 800);
    let r = m.call("vm_screenshot", json!({"vm_id": v, "scale": "half"}));
    assert_eq!(r["structuredContent"]["image_width"], 640, "{r}");
    assert_eq!(r["structuredContent"]["image_height"], 400);
    // The screenshot made the display size current.
    let st = m.ok("vm_status", json!({"vm_id": v}));
    assert_eq!(st["display"]["width"], 1280);

    // Input.
    let i = m.ok("vm_mouse_move", json!({"vm_id": v, "x": 662, "y": 326}));
    assert_eq!(i["events"], 1);
    let i = m.ok(
        "vm_mouse_click",
        json!({"vm_id": v, "x": 10, "y": 10, "double": true}),
    );
    assert_eq!(i["operations"], 5);
    let e = m.err("vm_mouse_move", json!({"vm_id": v, "x": 5000, "y": 1}));
    assert_eq!(e["code"], "invalid_argument");
    let secret = "s3cret-Passw0rd!";
    let t = m.ok(
        "vm_keyboard_type",
        json!({"vm_id": v, "text": format!("echo {secret}\n")}),
    );
    assert_eq!(t["chars"], 22);
    assert_eq!(t["layout"], "us");
    let e = m.err("vm_keyboard_type", json!({"vm_id": v, "text": "caffè"}));
    assert_eq!(e["code"], "invalid_argument");
    let e = m.err(
        "vm_keyboard_type",
        json!({"vm_id": v, "text": "x", "layout": "it"}),
    );
    assert_eq!(e["code"], "invalid_argument");
    let k = m.ok(
        "vm_keyboard_press",
        json!({"vm_id": v, "keys": "ctrl+alt+t"}),
    );
    assert_eq!(k["events"], 6);
    let e = m.err("vm_keyboard_press", json!({"vm_id": v, "keys": "ctrl+qmp"}));
    assert_eq!(e["code"], "invalid_argument");

    // Logs: LightVM events and QEMU lines without host paths.
    let lg = m.ok("vm_logs", json!({"vm_id": v, "limit": 200}));
    assert!(!lg["entries"].as_array().unwrap().is_empty());
    let q = lg["qemu"].to_string();
    assert!(q.contains("<path>"), "{q}");
    assert!(!q.contains("Users") && !q.contains("C:\\\\"), "{q}");
    let after = lg["last_seq"].as_u64().unwrap();
    let lg2 = m.ok("vm_logs", json!({"vm_id": v, "after_seq": after}));
    assert!(
        lg2["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["seq"].as_u64().unwrap() > after)
    );

    // Operation lookup and reset.
    let op = started["operation_id"].as_str().unwrap();
    let o = m.ok("operation_get", json!({"operation_id": op}));
    assert_eq!(o["kind"], "start");
    let e = m.err(
        "operation_get",
        json!({"operation_id": OperationId::new_v7().to_string()}),
    );
    assert_eq!(e["code"], "not_found");
    let rs = m.ok("vm_reset", json!({"vm_id": v}));
    assert_eq!(rs["kind"], "reset");

    // Stop (forced), checkpoint, baselines, restore of the own checkpoint
    // and of the initial baseline; a user's other baseline is refused.
    m.ok("vm_stop", json!({"vm_id": v, "force": true}));
    let st = m.ok("vm_status", json!({"vm_id": v}));
    assert_eq!(st["state"], "stopped");
    let bl = m.ok("vm_baselines", json!({"vm_id": v}));
    let rows = bl["baselines"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|b| b["initial"] == true && b["restorable"] == true)
    );
    assert!(
        rows.iter()
            .any(|b| b["created_by_session"] == true && b["restorable"] == true)
    );
    let own = snap["baseline_id"].as_str().unwrap();
    let r = m.ok("vm_restore", json!({"vm_id": v, "baseline_id": own}));
    assert_eq!(r["baseline_id"], own);
    // The session survives its own restore.
    assert_eq!(
        m.ok("session_status", json!({"wait_s": 0}))["state"],
        "active"
    );
    m.ok(
        "vm_restore",
        json!({"vm_id": v, "baseline_id": initial.to_string()}),
    );
    let other = match ok(
        &mut u,
        Request::CreateBaseline {
            vm_id: vm,
            label: Label::parse("user's own").unwrap(),
            allow_unclean: true,
        },
    )
    .await
    {
        ResponseBody::BaselineCreated { baseline } => baseline.baseline_id,
        other => panic!("{other:?}"),
    };
    let e = m.err(
        "vm_restore",
        json!({"vm_id": v, "baseline_id": other.to_string()}),
    );
    assert_eq!(e["code"], "permission_denied", "{e}");
    assert_eq!(e["details"]["reason"], "baseline_not_restorable");

    // Report and declaration: the claim never sets the outcome.
    let d = m.ok(
        "session_declare",
        json!({"claimed_outcome": "passed", "summary": "typed the command"}),
    );
    assert_eq!(d["recorded"], true);
    let rep = m.ok("report_get", json!({}));
    assert_eq!(rep["report"]["outcome_source"], "pending");
    assert_eq!(rep["report"]["outcome"], "inconclusive");
    let e = m.err(
        "report_get",
        json!({"session_id": SessionId::new_v7().to_string()}),
    );
    assert_eq!(e["code"], "not_found");

    // End: the UI reads the report; no typed text anywhere.
    let end = m.ok("session_end", json!({}));
    assert_eq!(end["session_id"], sid.to_string());
    let e = m.err("vm_list", json!({}));
    assert_eq!(e["code"], "session_closed");
    let header = match ok(&mut u, Request::GetSessionReport { session_id: sid }).await {
        ResponseBody::SessionReport { report } => report,
        other => panic!("{other:?}"),
    };
    assert_eq!(header.outcome, TestOutcome::Inconclusive);
    assert_eq!(header.outcome_source, OutcomeSource::Termination);
    assert_eq!(header.baseline_id, Some(initial));
    assert_eq!(header.versions.mcp_protocol.as_deref(), Some("2025-11-25"));
    assert!(header.screenshots_retained >= 2);
    assert_eq!(
        header.agent_declaration.as_ref().unwrap().claimed_outcome,
        TestOutcome::Passed
    );
    let mut events = Vec::new();
    let mut off = 0;
    loop {
        match ok(
            &mut u,
            Request::GetSessionReportEvents {
                session_id: sid,
                offset: off,
                limit: 50,
            },
        )
        .await
        {
            ResponseBody::SessionReportEvents {
                events: e, total, ..
            } => {
                off += u32::try_from(e.len()).unwrap();
                events.extend(e);
                if off >= total {
                    break;
                }
            }
            other => panic!("{other:?}"),
        }
    }
    let typed = events.iter().find_map(|e| match &e.body {
        ReportEventBody::Tool { tool, detail, .. } if tool == "vm_keyboard_type" => Some(detail),
        _ => None,
    });
    let typed = typed.expect("keyboard note");
    assert_eq!(typed["chars"], 22);
    assert_eq!(typed["sha256"].as_str().unwrap().len(), 64);
    let all = serde_json::to_string(&events).unwrap();
    assert!(!all.contains(secret), "typed text in the report");
    assert!(
        !audit_text(&svc).contains(secret),
        "typed text in the audit"
    );
    let report_file = std::fs::read_to_string(
        svc.dir
            .join("sessions")
            .join(sid.to_string())
            .join("report.json"),
    )
    .unwrap();
    assert!(!report_file.contains(secret));
    assert!(
        !report_file.contains(&svc.dir.display().to_string()),
        "host path in the report"
    );
    for call in [
        "submit.start",
        "submit.input",
        "submit.reset",
        "submit.force_stop",
        "screenshot",
        "snapshot",
        "restore",
        "logs",
        "list_vms",
        "vm_status",
        "list_baselines",
        "operation",
    ] {
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.body, ReportEventBody::Call { call: c, .. } if c == call)),
            "{call} not recorded"
        );
    }
    assert!(!m.stderr().contains(secret));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn denials_wrong_vm_host_paths_capabilities_and_ownership() {
    let svc = start_svc("f5-mcp-deny").await;
    let mut u = ui(&svc).await;
    let (vm, _) = vm_with_baseline(&mut u, &svc, "granted").await;
    let (other, _) = vm_with_baseline(&mut u, &svc, "other").await;
    // No vm.input, no vm.reset, no vm.logs; input not handed over.
    let caps: CapabilitySet = [
        Capability::VmList,
        Capability::VmStatus,
        Capability::VmStart,
        Capability::VmStop,
        Capability::VmScreenshot,
    ]
    .into_iter()
    .collect();
    let (mut m, _) = session(&svc, &mut u, vm, caps, false).await;
    let v = vm.to_string();
    // The list shows only the granted VM; the other one is not visible.
    let l = m.ok("vm_list", json!({}));
    assert_eq!(l["vms"].as_array().unwrap().len(), 1);
    for tool in ["vm_status", "vm_start", "vm_screenshot", "vm_baselines"] {
        let e = m.err(tool, json!({"vm_id": other.to_string()}));
        assert_eq!(e["code"], "permission_denied", "{tool}: {e}");
        assert!(!e.to_string().contains("other"), "no data of the other VM");
    }
    // Host paths, UNC, device paths and traversal as identifiers.
    for bad in [
        "C:\\Windows\\System32\\config\\SAM",
        "..\\..\\vms\\x\\primary.qcow2",
        "\\\\server\\share\\disk.qcow2",
        "\\\\.\\PhysicalDrive0",
        "/etc/passwd",
        "-drive file=/etc/passwd",
    ] {
        for tool in ["vm_status", "vm_start", "vm_logs"] {
            let e = m.err(tool, json!({"vm_id": bad}));
            assert_eq!(e["code"], "invalid_argument", "{tool} {bad}: {e}");
        }
        let e = m.err("vm_restore", json!({"vm_id": v, "baseline_id": bad}));
        assert_eq!(e["code"], "invalid_argument");
    }
    // Unknown argument names (a `path` smuggled in) are refused.
    let e = m.err("vm_screenshot", json!({"vm_id": v, "path": "C:\\x.png"}));
    assert_eq!(e["code"], "invalid_argument");
    m.ok("vm_start", json!({"vm_id": v}));
    // Capabilities not granted.
    let e = m.err("vm_reset", json!({"vm_id": v}));
    assert_eq!(e["code"], "permission_denied", "{e}");
    let e = m.err("vm_logs", json!({"vm_id": v}));
    assert_eq!(e["code"], "permission_denied", "{e}");
    let e = m.err("vm_snapshot", json!({"vm_id": v, "label": "x"}));
    assert_eq!(e["code"], "permission_denied", "{e}");
    // Input: capability missing and ownership not handed over.
    let e = m.err("vm_keyboard_press", json!({"vm_id": v, "keys": "ret"}));
    assert_eq!(e["code"], "permission_denied", "{e}");
    let calls_before = svc
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with("vm_input"))
        .count();
    let e = m.err("vm_mouse_click", json!({"vm_id": v}));
    assert_eq!(e["code"], "permission_denied");
    let calls_after = svc
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with("vm_input"))
        .count();
    assert_eq!(calls_before, calls_after, "no input forwarded");
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_revocation_through_the_adapter() {
    let svc = start_svc("f5-mcp-revoke").await;
    let mut u = ui(&svc).await;
    let (vm, _) = vm_with_baseline(&mut u, &svc, "revoke").await;
    let (mut m, sid) = session(&svc, &mut u, vm, all_caps(), true).await;
    let v = vm.to_string();
    m.ok("vm_start", json!({"vm_id": v}));
    ok(&mut u, Request::PauseSession { session_id: sid }).await;
    let e = m.err("vm_keyboard_press", json!({"vm_id": v, "keys": "ret"}));
    assert_eq!(e["code"], "permission_denied", "{e}");
    let e = m.err("vm_stop", json!({"vm_id": v}));
    assert_eq!(e["message"], "session paused", "{e}");
    // Observation stays allowed while paused.
    m.call("vm_screenshot", json!({"vm_id": v}));
    ok(&mut u, Request::ResumeSession { session_id: sid }).await;
    m.ok("vm_keyboard_press", json!({"vm_id": v, "keys": "ret"}));
    // Lease renewals keep the session alive beyond the 10 s lease.
    tokio::time::sleep(Duration::from_secs(12)).await;
    m.ok("vm_status", json!({"vm_id": v}));
    // Revocation: the next call is refused, the session is closed.
    ok(&mut u, Request::RevokeSession { session_id: sid }).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let e = m.err("vm_keyboard_press", json!({"vm_id": v, "keys": "ret"}));
    assert_eq!(e["code"], "session_closed", "{e}");
    let s = m.ok("session_status", json!({"wait_s": 0}));
    assert_eq!(s["state"], "closed");
    // The VM keeps running after a revocation (F4 rule); report: cancelled.
    let header = match ok(&mut u, Request::GetSessionReport { session_id: sid }).await {
        ResponseBody::SessionReport { report } => report,
        other => panic!("{other:?}"),
    };
    assert_eq!(header.outcome, TestOutcome::Cancelled);
    // A new session needs a new approval.
    let r = m.ok("session_request", json!({"purpose": "again"}));
    assert_eq!(r["state"], "pending");
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_screenshot_and_rate_limit() {
    let svc = start_svc("f5-mcp-limits").await;
    let mut u = ui(&svc).await;
    let (vm, _) = vm_with_baseline(&mut u, &svc, "limits").await;
    ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
    let p = pairing_file(&mut u, &svc).await;
    let mut m = Mcp::spawn(Some(&p));
    m.initialize("2025-11-25");
    m.ok("session_request", json!({"purpose": "limits"}));
    let req = match ok(&mut u, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => requests.into_iter().next().unwrap(),
        other => panic!("{other:?}"),
    };
    let mut a = approval(vm, all_caps(), true);
    a.quotas = Some(Quotas {
        max_operations: 10_000,
        max_input_events: 10_000,
        max_screenshots: 100,
        max_screenshot_bytes: 1_000,
        max_queued_operations: 8,
    });
    ok(
        &mut u,
        Request::ApproveSession {
            request_id: req.request_id,
            approval: a,
        },
    )
    .await;
    assert_eq!(
        m.ok("session_status", json!({"wait_s": 10}))["state"],
        "active"
    );
    let v = vm.to_string();
    m.ok("vm_start", json!({"vm_id": v}));
    let e = m.err("vm_screenshot", json!({"vm_id": v}));
    assert_eq!(e["code"], "resource_limit", "{e}");
    // Pipelined calls over the adapter's rate (20 burst, 10/s).
    for i in 0..40 {
        m.send(
            &json!({"jsonrpc": "2.0", "id": 10_000 + i, "method": "tools/call",
            "params": {"name": "session_status", "arguments": {"wait_s": 0}}}),
        );
    }
    let mut limited = 0;
    for _ in 0..40 {
        let r = m.recv();
        if r["result"]["isError"] == json!(true)
            && r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("resource_limit")
        {
            limited += 1;
        }
    }
    assert!(limited >= 10, "rate limit not applied: {limited}");
    // The UI side cannot be reached by the adapter: a UI-only request is
    // impossible from MCP (no such tool), and the IPC refuses it anyway.
    let e = ui_err(
        &mut u,
        Request::DeleteSessionReport {
            session_id: SessionId::new_v7(),
        },
    )
    .await;
    assert_eq!(e.code, lightvm_proto::ErrorCode::NotFound);
    svc.handle.shutdown().await;
}
