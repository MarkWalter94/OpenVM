//! F5 end-to-end with a real LLM client (ignored test, run by hand):
//! service + real QEMU (live Ubuntu ISO + ADR-0019 fixture ISO), the real
//! `lightvm-mcp` adapter launched by the client over stdio, the client asked
//! to run the fixture (`fx.sh scrivi`) and declare its result.
//!
//! The harness plays the user in the app: it approves the session request
//! through the UI-role IPC (stand-in for the click in "Agents", documented
//! as such) and hands input control to the session; once the agent has
//! declared its result it revokes the session, runs the ADR-0019 pixel
//! verifier on the session's last stored screenshot, exports the report
//! (JSON + Markdown + screenshots), stops the VM, restores the baseline and
//! checks on a fresh boot that the marker is gone (V6).
//!
//! The harness itself uses a short agent session to wait for the desktop
//! before the client starts (A0, ADR-0019), so the client's budget is not
//! spent on the boot; that session has its own report.
//!
//! Environment: `LIGHTVM_E2E_QEMU_DIR` (required), `LIGHTVM_E2E_ISO`,
//! `LIGHTVM_E2E_CPU` (`Skylake-Client-v4`), `LIGHTVM_F5_EVIDENCE_DIR`,
//! `LIGHTVM_F5_CLIENT` (`claude`), `LIGHTVM_F5_CLIENT_EXE` (path of the
//! client executable), `LIGHTVM_F5_MODEL` (optional), `LIGHTVM_F5_RUN`
//! (label of the run), `LIGHTVM_F5_BUDGET_USD` (default 4).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lightvm_fixture::verify::{Check, DEFAULT_CALIBRATION, Frame, Marker};
use lightvm_proto::TestOutcome;
use lightvm_proto::ipc::{
    AgentCall, ClientMessage, Request, ResponseBody, Role, ServerMessage, SessionApproval,
    SessionAuth, base64_decode,
};
use lightvm_proto::report::{FixtureMarker, ReportFormat, SessionVerifier};
use lightvm_proto::{
    Capability, CapabilitySet, InputBatch, InputEvent, KeyAction, Label, MediaKind, OperationId,
    QCode, ReplyError, RunId, SessionId, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::sample_config;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, start};

const T: Duration = Duration::from_secs(120);

/// Removes the test catalog (read-only managed copies included).
struct RemoveCatalog(PathBuf);

impl Drop for RemoveCatalog {
    fn drop(&mut self) {
        fn clear(p: &Path) {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        clear(&path);
                    } else if let Ok(m) = e.metadata() {
                        let mut perm = m.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        perm.set_readonly(false);
                        let _ = std::fs::set_permissions(&path, perm);
                    }
                }
            }
        }
        for _ in 0..60 {
            clear(&self.0);
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        eprintln!("test catalog not removed: {}", self.0.display());
    }
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn log(evidence: &Path, line: &str) {
    use std::io::Write as _;
    eprintln!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(evidence.join("harness.log"))
    {
        let _ = writeln!(f, "{} {line}", lightvm_proto::UtcTimestamp::now());
    }
}

fn short_dir(prefix: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("{prefix}{:x}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn req(c: &mut Client, r: Request) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(1800), c.request(r))
        .await
        .expect("timeout")
        .unwrap()
}

async fn ok(c: &mut Client, r: Request) -> ResponseBody {
    let name = r.name();
    match req(c, r).await {
        ServerMessage::Response { result, .. } => result,
        other => panic!("{name}: {other:?}"),
    }
}

async fn status(c: &mut Client, vm: VmId) -> (VmState, Option<RunId>) {
    match ok(c, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => (vm.state, vm.run_id),
        other => panic!("{other:?}"),
    }
}

async fn wait_state(c: &mut Client, vm: VmId, want: VmState) -> Option<RunId> {
    let t0 = Instant::now();
    loop {
        let (s, run) = status(c, vm).await;
        if s == want {
            return run;
        }
        assert!(t0.elapsed() < T, "VM not {want:?} ({s:?})");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn submit(c: &mut Client, vm: VmId, run: Option<RunId>, action: VmAction) -> ResponseBody {
    ok(
        c,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: run,
                action,
            },
        },
    )
    .await
}

fn key(q: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(q).unwrap(),
        down,
    })
}

fn decode(png_bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().expect("PNG");
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).expect("PNG frame");
    buf.truncate(info.buffer_size());
    (info.width as usize, info.height as usize, buf)
}

fn lit_permille(rgb: &[u8]) -> u64 {
    let px = rgb.as_chunks::<3>().0;
    let n = px.len().max(1) as u64;
    px.iter().filter(|p| p.iter().any(|&c| c > 16)).count() as u64 * 1000 / n
}

/// Harness agent session (lease renewed every 3 s).
struct Harness {
    c: Client,
    auth: SessionAuth,
    renewed: Instant,
}

impl Harness {
    async fn new(endpoint: &str, u: &mut Client, vm: VmId) -> Self {
        let cred = match ok(u, Request::CreatePairingCredential).await {
            ResponseBody::PairingCredential { credential, .. } => {
                Credential::from_hex(&credential).unwrap()
            }
            other => panic!("{other:?}"),
        };
        let mut c = Client::connect(endpoint).await.unwrap();
        c.hello(&cred).await.unwrap();
        let request_id = match ok(
            &mut c,
            Request::RequestSession {
                client_name: "f5-harness".into(),
                purpose: "harness: wait for the desktop / check the restore".into(),
                requested_vms: vec![vm],
                requested_capabilities: CapabilitySet::new(),
                requested_endpoints: Vec::new(),
            },
        )
        .await
        {
            ResponseBody::SessionRequested { request_id, .. } => request_id,
            other => panic!("{other:?}"),
        };
        c.send(&ClientMessage::Request {
            request_id: 9_000,
            body: Request::AwaitSession { request_id },
        })
        .await
        .unwrap();
        ok(
            u,
            Request::ApproveSession {
                request_id,
                approval: SessionApproval {
                    vm_id: vm,
                    capabilities: [
                        Capability::VmStatus,
                        Capability::VmScreenshot,
                        Capability::VmInput,
                    ]
                    .into_iter()
                    .collect(),
                    duration_s: 3600,
                    quotas: None,
                    lease_ms: None,
                    give_input: true,
                    network_endpoints: Vec::new(),
                },
            },
        )
        .await;
        let auth = match tokio::time::timeout(T, c.recv()).await.unwrap().unwrap() {
            ServerMessage::Response {
                result:
                    ResponseBody::SessionGranted {
                        session_id, token, ..
                    },
                ..
            } => SessionAuth { session_id, token },
            other => panic!("{other:?}"),
        };
        Self {
            c,
            auth,
            renewed: Instant::now(),
        }
    }

    async fn call(&mut self, call: AgentCall) -> ServerMessage {
        if self.renewed.elapsed() >= Duration::from_secs(3) {
            let _ = self
                .c
                .request(Request::Agent {
                    session: self.auth.clone(),
                    call: AgentCall::RenewLease,
                })
                .await;
            self.renewed = Instant::now();
        }
        tokio::time::timeout(
            T,
            self.c.request(Request::Agent {
                session: self.auth.clone(),
                call,
            }),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn sleep(&mut self, d: Duration) {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            tokio::time::sleep(d.saturating_sub(t0.elapsed()).min(Duration::from_secs(2))).await;
            let _ = self.call(AgentCall::RenewLease).await;
        }
    }

    async fn shot(&mut self, vm: VmId, run: RunId) -> Vec<u8> {
        let op = OperationId::new_v7();
        let chunks = loop {
            match self
                .call(AgentCall::Screenshot {
                    operation_id: op,
                    vm_id: vm,
                    expected_run_id: run,
                })
                .await
            {
                ServerMessage::Response {
                    result: ResponseBody::AgentScreenshot { chunks, .. },
                    ..
                } => break chunks,
                ServerMessage::Error {
                    error: ReplyError::Operational(e),
                    ..
                } if e.message.contains("rate") => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                other => panic!("{other:?}"),
            }
        };
        let mut png_bytes = Vec::new();
        let mut index = 0;
        while index < chunks {
            match self
                .call(AgentCall::ScreenshotChunk {
                    operation_id: op,
                    index,
                })
                .await
            {
                ServerMessage::Response {
                    result: ResponseBody::ScreenshotChunk { data, .. },
                    ..
                } => {
                    png_bytes.extend(base64_decode(&data).unwrap());
                    index += 1;
                }
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
        png_bytes
    }

    /// A0: two screenshots 5 s apart identical, ≥ 99 % lit.
    async fn wait_desktop(&mut self, vm: VmId, run: RunId) -> Vec<u8> {
        let t0 = Instant::now();
        let mut prev: Option<Vec<u8>> = None;
        loop {
            assert!(
                t0.elapsed() < Duration::from_secs(400),
                "desktop (A0) timeout"
            );
            self.sleep(Duration::from_secs(5)).await;
            let p = self.shot(vm, run).await;
            let (_, _, rgb) = decode(&p);
            if lit_permille(&rgb) < 990 {
                prev = None;
                continue;
            }
            if prev.as_deref() == Some(rgb.as_slice()) {
                return p;
            }
            prev = Some(rgb);
        }
    }

    async fn keys(&mut self, vm: VmId, run: RunId, events: Vec<InputEvent>) {
        let m = self
            .call(AgentCall::Submit {
                request: VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: vm,
                    expected_run_id: Some(run),
                    action: VmAction::Input(InputBatch {
                        vm_id: vm,
                        run_id: run,
                        geometry: lightvm_proto::DisplayGeometry::new(1280, 800).unwrap(),
                        events,
                    }),
                },
            })
            .await;
        assert!(matches!(m, ServerMessage::Response { .. }), "{m:?}");
    }

    /// Ctrl+Alt+T and `fx.sh mostra` typed by the harness (V6 check).
    async fn mostra(&mut self, vm: VmId, run: RunId) -> (Check, Vec<u8>) {
        self.keys(
            vm,
            run,
            vec![
                key("ctrl", true),
                key("alt", true),
                key("t", true),
                key("t", false),
                key("alt", false),
                key("ctrl", false),
            ],
        )
        .await;
        self.sleep(Duration::from_secs(6)).await;
        self.type_text(
            vm,
            run,
            "test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh mostra\n",
        )
        .await;
        self.sleep(Duration::from_secs(6)).await;
        let p = self.shot(vm, run).await;
        let (w, h, rgb) = decode(&p);
        (
            Frame {
                width: w,
                height: h,
                rgb: &rgb,
            }
            .check(DEFAULT_CALIBRATION),
            p,
        )
    }

    /// Types `text` (US layout) as the harness.
    async fn type_text(&mut self, vm: VmId, run: RunId, text: &str) {
        for c in text.chars() {
            let (shift, q) = lightvm_mcp::keys::us_key(c).unwrap();
            let mut ev = Vec::new();
            if shift {
                ev.push(key("shift", true));
            }
            ev.push(key(q, true));
            ev.push(key(q, false));
            if shift {
                ev.push(key("shift", false));
            }
            self.keys(vm, run, ev).await;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    async fn end(mut self) {
        let _ = self.call(AgentCall::EndSession).await;
    }
}

/// Deterministic MCP client over the adapter's stdio (not an LLM); every
/// message is written to the transcript (images elided).
struct Scripted {
    stdin: Option<std::process::ChildStdin>,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
    id: u64,
    transcript: std::fs::File,
    /// Tool calls in order with the error code of the `isError` results.
    log: Vec<(String, Option<String>)>,
}

/// Error code of an `isError` tool result (`{"error": {"code": …}}` text).
fn tool_error_code(result: &serde_json::Value) -> Option<String> {
    if result["isError"] != serde_json::json!(true) {
        return None;
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    Some(
        v["error"]["code"]
            .as_str()
            .map_or_else(|| "unparsed".to_owned(), str::to_owned),
    )
}

impl Scripted {
    fn send(&mut self, msg: &serde_json::Value) {
        use std::io::Write as _;
        let _ = writeln!(self.transcript, ">> {msg}");
        let s = self.stdin.as_mut().unwrap();
        writeln!(s, "{msg}").unwrap();
        s.flush().unwrap();
    }

    fn rpc(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use std::io::Write as _;
        self.id += 1;
        let id = self.id;
        self.send(
            &serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        loop {
            let line = self.lines.next().expect("adapter stdout").unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&line).expect("stdout carries only JSON-RPC");
            let mut shown = v.clone();
            if let Some(c) = shown["result"]["content"].as_array_mut() {
                for b in c {
                    if b["type"] == "image" {
                        b["data"] = serde_json::json!("<image elided>");
                    }
                }
            }
            let _ = writeln!(self.transcript, "<< {shown}");
            if v["id"] == serde_json::json!(id) {
                return v;
            }
        }
    }

    fn call(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        std::thread::sleep(Duration::from_millis(150));
        let r = self.rpc(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        )["result"]
            .clone();
        self.log.push((name.to_owned(), tool_error_code(&r)));
        r
    }
}

/// What the scripted client does (F5-03 sample: the fixture script and one
/// script per outcome state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    /// The fixture steps (`fx.sh scrivi`), declaration, and one call after
    /// the harness revoked the session (must be refused).
    Fixture,
    /// Wrong action: `fx.sh mostra` instead of `scrivi` (no marker
    /// written), then declares `passed`.
    Wrong,
    /// A screenshot, then `session_end` (no declaration).
    EndEarly,
    /// A screenshot, then `vm_status` every 2 s until a call is refused
    /// (revocation, grant expiry, Emergency Stop, QEMU killed).
    UntilClosed,
}

/// Opening of every script: handshake, session request, wait for the
/// approval, `vm_list`; returns the VM id.
fn open_session(c: &mut Scripted) -> String {
    use serde_json::json;
    c.rpc(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "f5-scripted-client", "version": "1.0"}}),
    );
    c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    c.rpc("tools/list", json!({}));
    c.call(
        "session_request",
        json!({"purpose": "Run the LightVM fixture: open a terminal and run fx.sh scrivi"}),
    );
    loop {
        let st = c.call("session_status", json!({"wait_s": 30}));
        if st["structuredContent"]["state"] != "pending" {
            break;
        }
    }
    let l = c.call("vm_list", json!({}));
    l["structuredContent"]["vms"][0]["vm_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Runs `script` over the adapter's stdio; closes stdin at the end and
/// returns the calls with their error codes.
fn run_script(
    script: Script,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    transcript: std::fs::File,
) -> Vec<(String, Option<String>)> {
    use serde_json::json;
    if script == Script::Fixture {
        return scripted_client(stdin, stdout, transcript);
    }
    let mut c = Scripted {
        stdin: Some(stdin),
        lines: BufReader::new(stdout).lines(),
        id: 0,
        transcript,
        log: Vec::new(),
    };
    let vm = open_session(&mut c);
    c.call("vm_screenshot", json!({"vm_id": vm, "scale": "half"}));
    match script {
        Script::Fixture => {}
        Script::Wrong => {
            c.call(
                "vm_keyboard_press",
                json!({"vm_id": vm, "keys": "ctrl+alt+t"}),
            );
            std::thread::sleep(Duration::from_secs(6));
            c.call(
                "vm_keyboard_type",
                json!({"vm_id": vm, "text": "test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh mostra\n"}),
            );
            std::thread::sleep(Duration::from_secs(6));
            c.call("vm_screenshot", json!({"vm_id": vm}));
            c.call(
                "session_declare",
                json!({"claimed_outcome": "passed", "summary": "scripted client (wrong action on purpose): typed fx.sh mostra instead of scrivi"}),
            );
            std::thread::sleep(Duration::from_secs(5));
            c.call("vm_list", json!({}));
        }
        Script::EndEarly => {
            c.call("session_end", json!({}));
        }
        Script::UntilClosed => {
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(300) {
                std::thread::sleep(Duration::from_secs(2));
                let r = c.call("vm_status", json!({"vm_id": vm}));
                if r["isError"] == json!(true) {
                    break;
                }
            }
        }
    }
    c.stdin.take();
    c.log
}

/// The fixture steps of [`PROMPT`] as tool calls; closes stdin at the end.
fn scripted_client(
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    transcript: std::fs::File,
) -> Vec<(String, Option<String>)> {
    use serde_json::json;
    let mut c = Scripted {
        stdin: Some(stdin),
        lines: BufReader::new(stdout).lines(),
        id: 0,
        transcript,
        log: Vec::new(),
    };
    let vm = open_session(&mut c);
    c.call("vm_screenshot", json!({"vm_id": vm, "scale": "half"}));
    c.call(
        "vm_keyboard_press",
        json!({"vm_id": vm, "keys": "ctrl+alt+t"}),
    );
    std::thread::sleep(Duration::from_secs(6));
    c.call("vm_screenshot", json!({"vm_id": vm, "scale": "half"}));
    c.call(
        "vm_keyboard_type",
        json!({"vm_id": vm, "text": "test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh scrivi
"}),
    );
    std::thread::sleep(Duration::from_secs(6));
    c.call("vm_screenshot", json!({"vm_id": vm}));
    c.call("vm_logs", json!({"vm_id": vm, "limit": 20}));
    c.call("report_get", json!({}));
    c.call(
        "session_declare",
        json!({"claimed_outcome": "inconclusive", "summary": "scripted client: steps sent, the screen was not interpreted (no model)"}),
    );
    std::thread::sleep(Duration::from_secs(5));
    // After the revocation by the harness: refused.
    c.call("vm_list", json!({}));
    c.stdin.take();
    c.log
}

const PROMPT: &str = "You control a virtual machine only through the `lightvm` MCP tools. \
Task (ADR-0019 fixture): 1) call session_request with purpose \"Run the LightVM fixture: open a terminal and run fx.sh scrivi\", tell me the verification code, then call session_status until the state is active (I approve it in the app). \
2) vm_list to get the VM (it is already running the Ubuntu live desktop) and take a vm_screenshot with scale \"half\". \
3) Press ctrl+alt+t to open a terminal, wait about 5 seconds and take a screenshot to check it opened. \
4) Type exactly this line followed by a newline: test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh scrivi \
5) Wait about 5 seconds and take a screenshot: success shows four coloured blocks in a row with a long yellow bar below them (a cyan bar means the marker was not written). \
6) Call session_declare with the outcome you believe (passed/failed/inconclusive) and a short summary, then stop. Do not do anything else in the VM.";

#[test]
#[ignore = "real QEMU, Ubuntu ISO and a real LLM client (uses the user's account)"]
fn e2e_real_client_runs_the_fixture() {
    let lab = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab");
    let qemu = env_path("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR");
    let iso = env_path("LIGHTVM_E2E_ISO")
        .unwrap_or_else(|| lab.join("media").join("ubuntu-26.04.1-desktop-amd64.iso"));
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let run_label = std::env::var("LIGHTVM_F5_RUN").unwrap_or_else(|_| "run".into());
    let client = std::env::var("LIGHTVM_F5_CLIENT").unwrap_or_else(|_| "claude".into());
    let client_exe = env_path("LIGHTVM_F5_CLIENT_EXE").unwrap_or_else(|| PathBuf::from(&client));
    let budget = std::env::var("LIGHTVM_F5_BUDGET_USD").unwrap_or_else(|_| "4".into());
    let evidence = env_path("LIGHTVM_F5_EVIDENCE_DIR")
        .unwrap_or_else(|| std::env::temp_dir().join("lightvm-f5-evidence"))
        .join(&run_label);
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("f5-real");
    let _cleanup = RemoveCatalog(dir.clone());
    let fx_iso = dir.with_extension("fx.iso");
    std::fs::write(&fx_iso, lightvm_fixture::fixture_iso()).unwrap();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu, store.root(), short_dir("lvf5"));
    setup.cpu_model = cpu;
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(capacity.accelerator_available);
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut cfg = ServiceConfig::new(&dir);
        cfg.runtime_dir = Some(dir.join("run"));
        cfg.display_dir = Some(short_dir("lvd5"));
        let (handle, ui_cred) = start(cfg, Box::new(engine), Box::new(host)).await.unwrap();
        let ep = handle.endpoint().to_owned();
        let mut u = Client::connect(&ep).await.unwrap();
        assert!(matches!(
            u.hello(&ui_cred).await.unwrap(),
            ServerMessage::HelloOk { role: Role::Ui, .. }
        ));
        let import = |p: PathBuf, kind| (p, kind);
        let mut media = Vec::new();
        for (p, kind) in [import(iso.clone(), MediaKind::InstallIso), import(fx_iso.clone(), MediaKind::DataIso)] {
            let p = std::fs::canonicalize(&p).unwrap();
            let p = p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
            match ok(&mut u, Request::ImportMedia { source_path: p, media_kind: kind, declared_origin: "lab media".into() }).await {
                ResponseBody::MediaImported { media: m } => media.push(m.id),
                other => panic!("{other:?}"),
            }
        }
        let mut vmc = sample_config("f5-fixture");
        vmc.vcpus = 4;
        vmc.memory_mib = 4096;
        vmc.disk_gib = 8;
        vmc.install_media = Some(media[0]);
        vmc.data_media = Some(media[1]);
        let vm = match ok(&mut u, Request::CreateVm { config: vmc }).await {
            ResponseBody::VmCreated { vm } => vm.id,
            other => panic!("{other:?}"),
        };
        // Empty data disk as the baseline (first start creates it; forced
        // stop in the firmware: baseline recorded as not verified).
        submit(&mut u, vm, None, VmAction::Start).await;
        let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        submit(&mut u, vm, Some(run), VmAction::ForceStop).await;
        wait_state(&mut u, vm, VmState::Stopped).await;
        let base = match ok(&mut u, Request::CreateBaseline { vm_id: vm, label: Label::parse("fixture empty disk").unwrap(), allow_unclean: true }).await {
            ResponseBody::BaselineCreated { baseline } => baseline,
            other => panic!("{other:?}"),
        };
        ok(&mut u, Request::RestoreBaseline { vm_id: vm, baseline_id: base.baseline_id }).await;
        log(&evidence, &format!("baseline {} disk sha256 {}", base.baseline_id, base.disk_sha256.to_hex()));

        // Boot to the desktop (harness session, A0).
        ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
        submit(&mut u, vm, None, VmAction::Start).await;
        let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
        let mut h = Harness::new(&ep, &mut u, vm).await;
        let t0 = Instant::now();
        let reference = h.wait_desktop(vm, run).await;
        std::fs::write(evidence.join("reference.png"), &reference).unwrap();
        let (w, hh, rgb) = decode(&reference);
        assert!(!Frame { width: w, height: hh, rgb: &rgb }.has_fixture_colour(), "inconclusive: fixture colours in the reference");
        log(&evidence, &format!("desktop after {:?}; reference has no fixture colour", t0.elapsed()));
        h.end().await;

        // Pairing file as the app writes it, MCP config for the client.
        let pdir = dir.join("pairing");
        std::fs::create_dir_all(&pdir).unwrap();
        lightvm_mcp::pairing::restrict_folder(&pdir).unwrap();
        let cred = match ok(&mut u, Request::CreatePairingCredential).await {
            ResponseBody::PairingCredential { credential, .. } => credential,
            other => panic!("{other:?}"),
        };
        let pfile = pdir.join("pairing.json");
        let expires = lightvm_proto::UtcTimestamp::now().checked_add_millis(600_000).unwrap();
        std::fs::write(&pfile, serde_json::json!({"format": "lightvm-pairing-v1", "endpoint": ep, "credential": cred, "expires_at": expires.to_string()}).to_string()).unwrap();
        drop(cred);
        let mcp_cfg = dir.join("mcp-config.json");
        std::fs::write(&mcp_cfg, serde_json::json!({"mcpServers": {"lightvm": {
            "type": "stdio",
            "command": env!("CARGO_BIN_EXE_lightvm-mcp"),
            "args": [],
            "env": {"LIGHTVM_PAIRING_FILE": pfile.to_str().unwrap()}
        }}}).to_string()).unwrap();

        // The real client.
        let mut cmd = Command::new(&client_exe);
        if client == "claude" {
            cmd.args(["-p", PROMPT, "--mcp-config"]).arg(&mcp_cfg).args([
                "--strict-mcp-config",
                "--tools", "",
                "--allowedTools", "mcp__lightvm",
                "--permission-mode", "dontAsk",
                "--no-session-persistence",
                "--output-format", "stream-json",
                "--verbose",
                "--max-budget-usd", &budget,
            ]);
            if let Ok(m) = std::env::var("LIGHTVM_F5_MODEL") {
                cmd.args(["--model", &m]);
            }
        } else if client == "scripted" {
            // Deterministic MCP client (not an LLM): the real adapter is
            // driven over stdio with the fixed fixture steps.
            cmd = Command::new(env!("CARGO_BIN_EXE_lightvm-mcp"));
            cmd.env("LIGHTVM_PAIRING_FILE", &pfile);
        } else {
            panic!("client {client} not scripted");
        }
        // Started from a terminal of its own, not nested in another
        // agent session that may be running the harness.
        // (variables of a host agent session are not inherited; the client
        // uses its own stored login).
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().to_ascii_uppercase().starts_with("CLAUDE") {
                cmd.env_remove(&k);
            }
        }
        cmd.current_dir(&dir)
            .stdin(if client == "scripted" { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if client == "scripted" {
            log(&evidence, "scripted MCP client (no model) started on the adapter's stdio");
        } else {
            log(&evidence, &format!("client {client} started ({}); prompt: {PROMPT}", client_exe.display()));
        }
        let started = Instant::now();
        let mut child = cmd.spawn().expect("client start");
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let ev2 = evidence.clone();
        let stdin = child.stdin.take();
        let out_t = std::thread::spawn(move || {
            let mut f = std::fs::File::create(ev2.join("client-stdout.jsonl")).unwrap();
            match stdin {
                Some(stdin) => {
                    scripted_client(stdin, out, f);
                }
                None => {
                    for l in BufReader::new(out).lines().map_while(Result::ok) {
                        use std::io::Write as _;
                        let _ = writeln!(f, "{l}");
                    }
                }
            }
        });
        let ev3 = evidence.clone();
        let err_t = std::thread::spawn(move || {
            let mut f = std::fs::File::create(ev3.join("client-stderr.txt")).unwrap();
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                use std::io::Write as _;
                let _ = writeln!(f, "{l}");
            }
        });

        // The user in the app: approve the request, hand input over; revoke
        // once the agent declared its result.
        let mut session: Option<SessionId> = None;
        let mut revoked = false;
        let limit = Duration::from_secs(1500);
        loop {
            if let Ok(Some(st)) = child.try_wait() {
                log(&evidence, &format!("client exited: {st} after {:?}", started.elapsed()));
                break;
            }
            if started.elapsed() > limit {
                log(&evidence, "client timeout: killed");
                let _ = child.kill();
                break;
            }
            if session.is_none()
                && let ResponseBody::SessionRequests { requests } = ok(&mut u, Request::ListSessionRequests).await
                && let Some(r) = requests.into_iter().next()
            {
                log(&evidence, &format!("session request from {:?}, purpose {:?}, code {} — approved by the harness (stand-in for the user's click)", r.client_name, r.purpose, r.verification_code));
                let caps: CapabilitySet = [Capability::VmList, Capability::VmStatus, Capability::VmScreenshot, Capability::VmInput, Capability::VmLogs].into_iter().collect();
                if let ResponseBody::Session { session: s } = ok(&mut u, Request::ApproveSession { request_id: r.request_id, approval: SessionApproval { vm_id: vm, capabilities: caps, duration_s: 1800, quotas: None, lease_ms: None, give_input: true, network_endpoints: Vec::new() } }).await {
                    session = Some(s.session_id);
                }
            }
            if let Some(sid) = session
                && !revoked
                && let ResponseBody::SessionReport { report } = ok(&mut u, Request::GetSessionReport { session_id: sid }).await
                && report.agent_declaration.is_some()
            {
                tokio::time::sleep(Duration::from_secs(2)).await;
                ok(&mut u, Request::RevokeSession { session_id: sid }).await;
                revoked = true;
                log(&evidence, &format!("agent declared {:?}; session revoked by the harness", report.agent_declaration.map(|d| (d.claimed_outcome, d.summary))));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let _ = child.wait();
        let _ = out_t.join();
        let _ = err_t.join();

        if let Some(sid) = session {
            let r = match ok(&mut u, Request::VerifySession { session_id: sid, verifier: SessionVerifier::FixtureMarker { expected: FixtureMarker::Present } }).await {
                ResponseBody::SessionReport { report } => report,
                other => panic!("{other:?}"),
            };
            log(&evidence, &format!("verifier: outcome {:?}, source {:?}, rationale {:?}; termination {:?}; screenshots {}; events {}", r.outcome, r.outcome_source, r.verdicts.last().map(|v| v.rationale.clone()), r.termination, r.screenshots_retained, r.events_total));
            for (fmt, name) in [(ReportFormat::Json, "report.json"), (ReportFormat::Markdown, "report.md")] {
                ok(&mut u, Request::ExportSessionReport { session_id: sid, format: fmt, destination: evidence.join(name).to_str().unwrap().into(), overwrite: true, include_screenshots: fmt == ReportFormat::Json }).await;
            }
        } else {
            log(&evidence, "no session request from the client");
        }

        // Stop, restore, check that the marker is gone (V6).
        if let (VmState::Running, Some(run)) = status(&mut u, vm).await {
            submit(&mut u, vm, Some(run), VmAction::ForceStop).await;
        }
        wait_state(&mut u, vm, VmState::Stopped).await;
        let t = Instant::now();
        ok(&mut u, Request::RestoreBaseline { vm_id: vm, baseline_id: base.baseline_id }).await;
        log(&evidence, &format!("baseline restored in {:?}", t.elapsed()));
        submit(&mut u, vm, None, VmAction::Start).await;
        let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
        let mut h = Harness::new(&ep, &mut u, vm).await;
        h.wait_desktop(vm, run).await;
        let (check, p) = h.mostra(vm, run).await;
        std::fs::write(evidence.join("after-restore-mostra.png"), &p).unwrap();
        log(&evidence, &format!("after restore: fx.sh mostra -> {check:?} (V6 expects Absent)"));
        h.end().await;
        let audit = std::fs::read_to_string(dir.join("audit").join("audit.jsonl")).unwrap_or_default();
        log(&evidence, &format!("audit: {} events; typed command present in audit: {}", audit.lines().count(), audit.contains("fx.sh")));
        if let (VmState::Running, Some(run)) = status(&mut u, vm).await {
            submit(&mut u, vm, Some(run), VmAction::ForceStop).await;
        }
        handle.shutdown().await;
        assert_eq!(check, Check::Ok(Marker::Absent), "V6 after restore");
    });
    let _ = std::fs::remove_file(&fx_iso);
}

// ===== F5-03: ten fixture runs and the outcome states (scripted client) =====

/// What the harness (standing in for the user in the app) does during a
/// scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    /// Nothing: the session ends by itself (agent end, grant expiry).
    None,
    /// Revoke once the agent declared its result.
    RevokeAfterDeclaration,
    /// Revoke after the first stored screenshot.
    RevokeAfterShot,
    /// Emergency Stop (UI-role IPC) after the first stored screenshot.
    EmergencyAfterShot,
    /// QEMU killed from outside the service after the first stored
    /// screenshot (crash stand-in).
    KillAfterShot,
}

#[derive(Debug)]
struct ScenarioOut {
    session: Option<SessionId>,
    calls: Vec<(String, Option<String>)>,
    client_panicked: bool,
    adapter_exit: Option<std::process::ExitStatus>,
    action: String,
    duration: Duration,
}

/// Kills, from outside the service, the QEMU process whose command line
/// names `marker` (this test's catalog). Returns the PIDs killed.
fn kill_qemu(marker: &str) -> String {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"Name='qemu-system-x86_64.exe'\" | Where-Object {{ $_.CommandLine -like '*{marker}*' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force; $_.ProcessId }}"
    );
    let o = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("powershell");
    String::from_utf8_lossy(&o.stdout)
        .trim()
        .replace("\r\n", ",")
}

/// One session through the real adapter driven by `script`; the harness
/// approves it through the UI-role IPC (stand-in for the click) and applies
/// `trigger`.
#[allow(clippy::too_many_arguments)]
async fn scenario(
    u: &mut Client,
    ep: &str,
    dir: &Path,
    vm: VmId,
    script: Script,
    trigger: Trigger,
    duration_s: u32,
    ev: &Path,
) -> ScenarioOut {
    std::fs::create_dir_all(ev).unwrap();
    let pdir = dir.join("pairing");
    std::fs::create_dir_all(&pdir).unwrap();
    lightvm_mcp::pairing::restrict_folder(&pdir).unwrap();
    let cred = match ok(u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => credential,
        other => panic!("{other:?}"),
    };
    let pfile = pdir.join("pairing.json");
    let expires = lightvm_proto::UtcTimestamp::now()
        .checked_add_millis(600_000)
        .unwrap();
    std::fs::write(&pfile, serde_json::json!({"format": "lightvm-pairing-v1", "endpoint": ep, "credential": cred, "expires_at": expires.to_string()}).to_string()).unwrap();
    drop(cred);
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightvm-mcp"))
        .env("LIGHTVM_PAIRING_FILE", &pfile)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("adapter start");
    let stdin = child.stdin.take().unwrap();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let ev2 = ev.to_path_buf();
    let client_t = std::thread::spawn(move || {
        let f = std::fs::File::create(ev2.join("client-transcript.jsonl")).unwrap();
        run_script(script, stdin, out, f)
    });
    let ev3 = ev.to_path_buf();
    let err_t = std::thread::spawn(move || {
        let mut f = std::fs::File::create(ev3.join("adapter-stderr.txt")).unwrap();
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            use std::io::Write as _;
            let _ = writeln!(f, "{l}");
        }
    });
    let marker = dir.file_name().unwrap().to_string_lossy().into_owned();
    log(
        ev,
        &format!(
            "scripted MCP client {script:?} (no model) on the real adapter; harness trigger {trigger:?}"
        ),
    );
    let started = Instant::now();
    let mut session: Option<SessionId> = None;
    let mut action = String::from("none");
    let mut acted = trigger == Trigger::None;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if started.elapsed() > Duration::from_secs(900) {
            log(ev, "client timeout: adapter killed");
            let _ = child.kill();
            break;
        }
        if session.is_none()
            && let ResponseBody::SessionRequests { requests } =
                ok(u, Request::ListSessionRequests).await
            && let Some(r) = requests.into_iter().next()
        {
            log(
                ev,
                &format!(
                    "session request from {:?}, code {}: approved by the harness through the UI-role IPC (stand-in for the user's click), {duration_s} s",
                    r.client_name, r.verification_code
                ),
            );
            let caps: CapabilitySet = [
                Capability::VmList,
                Capability::VmStatus,
                Capability::VmScreenshot,
                Capability::VmInput,
                Capability::VmLogs,
            ]
            .into_iter()
            .collect();
            if let ResponseBody::Session { session: s } = ok(
                u,
                Request::ApproveSession {
                    request_id: r.request_id,
                    approval: SessionApproval {
                        vm_id: vm,
                        capabilities: caps,
                        duration_s,
                        quotas: None,
                        lease_ms: None,
                        give_input: true,
                        network_endpoints: Vec::new(),
                    },
                },
            )
            .await
            {
                session = Some(s.session_id);
            }
        }
        if let Some(sid) = session
            && !acted
            && let ResponseBody::SessionReport { report } =
                ok(u, Request::GetSessionReport { session_id: sid }).await
        {
            let ready = if trigger == Trigger::RevokeAfterDeclaration {
                report.agent_declaration.is_some()
            } else {
                report.screenshots_retained >= 1
            };
            if ready {
                acted = true;
                tokio::time::sleep(Duration::from_secs(2)).await;
                action = match trigger {
                    Trigger::RevokeAfterDeclaration | Trigger::RevokeAfterShot => {
                        ok(u, Request::RevokeSession { session_id: sid }).await;
                        "session revoked by the harness (user's Revoke)".to_owned()
                    }
                    Trigger::EmergencyAfterShot => {
                        let r = ok(u, Request::EmergencyStop).await;
                        format!("Emergency Stop through the UI-role IPC: {r:?}")
                    }
                    Trigger::KillAfterShot => format!(
                        "QEMU killed from outside the service (Stop-Process -Force), pid {}",
                        kill_qemu(&marker)
                    ),
                    Trigger::None => String::new(),
                };
                log(ev, &action);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let adapter_exit = child.wait().ok();
    let joined = client_t.join();
    let client_panicked = joined.is_err();
    let calls = joined.unwrap_or_default();
    let _ = err_t.join();
    ScenarioOut {
        session,
        calls,
        client_panicked,
        adapter_exit,
        action,
        duration: started.elapsed(),
    }
}

/// Client errors that are infrastructure errors: every `isError` result,
/// except the final `session_closed` refusal the script expects.
fn unexpected_errors(calls: &[(String, Option<String>)], final_refusal: bool) -> Vec<String> {
    let n = calls.len();
    calls
        .iter()
        .enumerate()
        .filter_map(|(i, (name, code))| {
            let last = i + 1 == n;
            match code {
                None if last && final_refusal => Some(format!("{name}: last call not refused")),
                None => None,
                Some(c) if last && final_refusal && c == "session_closed" => None,
                Some(c) => Some(format!("{name}: {c}")),
            }
        })
        .collect()
}

async fn report_of(u: &mut Client, sid: SessionId) -> lightvm_proto::report::SessionReport {
    match ok(u, Request::GetSessionReport { session_id: sid }).await {
        ResponseBody::SessionReport { report } => *report,
        other => panic!("{other:?}"),
    }
}

async fn export(u: &mut Client, sid: SessionId, ev: &Path) {
    for (fmt, name) in [
        (ReportFormat::Json, "report.json"),
        (ReportFormat::Markdown, "report.md"),
    ] {
        ok(
            u,
            Request::ExportSessionReport {
                session_id: sid,
                format: fmt,
                destination: ev.join(name).to_str().unwrap().into(),
                overwrite: true,
                include_screenshots: fmt == ReportFormat::Json,
            },
        )
        .await;
    }
}

/// Force-stops the VM if it has a process and waits for `stopped` (a
/// `failed` VM is brought back to `stopped` by the supervisor).
async fn stop_vm(u: &mut Client, vm: VmId) {
    if let (VmState::Starting | VmState::Running | VmState::Stopping, Some(run)) =
        status(u, vm).await
    {
        let _ = req(
            u,
            Request::SubmitVm {
                request: VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: vm,
                    expected_run_id: Some(run),
                    action: VmAction::ForceStop,
                },
            },
        )
        .await;
    }
    wait_state(u, vm, VmState::Stopped).await;
}

fn has_fixture_colour(png_bytes: &[u8]) -> bool {
    let (w, h, rgb) = decode(png_bytes);
    Frame {
        width: w,
        height: h,
        rgb: &rgb,
    }
    .has_fixture_colour()
}

/// Starts the VM and waits for the desktop (A0) with a harness session.
async fn boot(u: &mut Client, ep: &str, vm: VmId, ev: &Path) -> RunId {
    submit(u, vm, None, VmAction::Start).await;
    let run = wait_state(u, vm, VmState::Running).await.unwrap();
    let mut h = Harness::new(ep, u, vm).await;
    let t0 = Instant::now();
    let p = h.wait_desktop(vm, run).await;
    h.end().await;
    std::fs::create_dir_all(ev).unwrap();
    std::fs::write(ev.join("boot-desktop.png"), &p).unwrap();
    log(
        ev,
        &format!(
            "boot: desktop after {:?}; fixture colours on screen: {}",
            t0.elapsed(),
            has_fixture_colour(&p)
        ),
    );
    run
}

fn tsv(path: &Path, cols: &[String]) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let _ = writeln!(f, "{}", cols.join("\t"));
}

#[test]
#[ignore = "real QEMU and the Ubuntu ISO; scripted MCP client (no model); about 1 h in release"]
fn f5_fixture_sample_and_outcome_states() {
    let lab = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab");
    let qemu = env_path("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR");
    let iso = env_path("LIGHTVM_E2E_ISO")
        .unwrap_or_else(|| lab.join("media").join("ubuntu-26.04.1-desktop-amd64.iso"));
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let runs: u32 = std::env::var("LIGHTVM_F5_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let outcomes = std::env::var("LIGHTVM_F5_OUTCOMES").map_or(true, |v| v != "0");
    let evidence = env_path("LIGHTVM_F5_EVIDENCE_DIR")
        .unwrap_or_else(|| std::env::temp_dir().join("lightvm-f5-evidence"))
        .join(std::env::var("LIGHTVM_F5_RUN").unwrap_or_else(|_| "f5-03".into()));
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("f5-sample");
    let _cleanup = RemoveCatalog(dir.clone());
    let fx_iso = dir.with_extension("fx.iso");
    std::fs::write(&fx_iso, lightvm_fixture::fixture_iso()).unwrap();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu, store.root(), short_dir("lvs5"));
    setup.cpu_model = cpu;
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(capacity.accelerator_available);
    let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
    let inspector = ManagedMediaInspector::new(store.root());
    let engine =
        CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let infra_total = rt.block_on(async {
        let mut cfg = ServiceConfig::new(&dir);
        cfg.runtime_dir = Some(dir.join("run"));
        cfg.display_dir = Some(short_dir("lvd6"));
        let (handle, ui_cred) = start(cfg, Box::new(engine), Box::new(host)).await.unwrap();
        let ep = handle.endpoint().to_owned();
        let mut u = Client::connect(&ep).await.unwrap();
        assert!(matches!(
            u.hello(&ui_cred).await.unwrap(),
            ServerMessage::HelloOk { role: Role::Ui, .. }
        ));
        let mut media = Vec::new();
        for (p, kind) in [
            (iso.clone(), MediaKind::InstallIso),
            (fx_iso.clone(), MediaKind::DataIso),
        ] {
            let p = std::fs::canonicalize(&p).unwrap();
            let p = p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
            match ok(
                &mut u,
                Request::ImportMedia {
                    source_path: p,
                    media_kind: kind,
                    declared_origin: "lab media".into(),
                },
            )
            .await
            {
                ResponseBody::MediaImported { media: m } => media.push(m.id),
                other => panic!("{other:?}"),
            }
        }
        let mut vmc = sample_config("f5-sample");
        vmc.vcpus = 4;
        vmc.memory_mib = 4096;
        vmc.disk_gib = 8;
        vmc.install_media = Some(media[0]);
        vmc.data_media = Some(media[1]);
        let vm = match ok(&mut u, Request::CreateVm { config: vmc }).await {
            ResponseBody::VmCreated { vm } => vm.id,
            other => panic!("{other:?}"),
        };
        submit(&mut u, vm, None, VmAction::Start).await;
        let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        submit(&mut u, vm, Some(run), VmAction::ForceStop).await;
        wait_state(&mut u, vm, VmState::Stopped).await;
        let base = match ok(
            &mut u,
            Request::CreateBaseline {
                vm_id: vm,
                label: Label::parse("fixture empty disk").unwrap(),
                allow_unclean: true,
            },
        )
        .await
        {
            ResponseBody::BaselineCreated { baseline } => baseline,
            other => panic!("{other:?}"),
        };
        ok(
            &mut u,
            Request::RestoreBaseline {
                vm_id: vm,
                baseline_id: base.baseline_id,
            },
        )
        .await;
        log(
            &evidence,
            &format!(
                "baseline {} disk sha256 {}",
                base.baseline_id,
                base.disk_sha256.to_hex()
            ),
        );
        ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
        let sample = evidence.join("sample.tsv");
        tsv(
            &sample,
            &[
                "attempt",
                "session",
                "outcome",
                "source",
                "termination",
                "verifier",
                "screenshots",
                "events",
                "calls",
                "infrastructure_errors",
                "v6_after_restore",
                "restore_ms",
                "duration_s",
            ]
            .map(str::to_owned),
        );
        boot(&mut u, &ep, vm, &evidence).await;
        let mut infra_total = 0usize;
        for i in 1..=runs {
            let label = format!("run-{i:02}");
            let ev = evidence.join(&label);
            let t_iter = Instant::now();
            let out = scenario(
                &mut u,
                &ep,
                &dir,
                vm,
                Script::Fixture,
                Trigger::RevokeAfterDeclaration,
                1800,
                &ev,
            )
            .await;
            let mut infra = unexpected_errors(&out.calls, true);
            if out.client_panicked {
                infra.push("client: adapter output ended early".into());
            }
            if !out.adapter_exit.is_some_and(|s| s.success()) {
                infra.push(format!("adapter exit {:?}", out.adapter_exit));
            }
            let mut cols = vec![label.clone()];
            if let Some(sid) = out.session {
                let r = match ok(
                    &mut u,
                    Request::VerifySession {
                        session_id: sid,
                        verifier: SessionVerifier::FixtureMarker {
                            expected: FixtureMarker::Present,
                        },
                    },
                )
                .await
                {
                    ResponseBody::SessionReport { report } => report,
                    other => panic!("{other:?}"),
                };
                export(&mut u, sid, &ev).await;
                if r.screenshots_retained == 0 {
                    infra.push("no stored screenshot".into());
                }
                cols.extend([
                    sid.to_string(),
                    format!("{:?}", r.outcome),
                    format!("{:?}", r.outcome_source),
                    format!("{:?}", r.termination),
                    r.verdicts
                        .last()
                        .map(|v| v.rationale.clone())
                        .unwrap_or_default(),
                    r.screenshots_retained.to_string(),
                    r.events_total.to_string(),
                ]);
            } else {
                infra.push("no session".into());
                cols.extend(std::iter::repeat_n(String::new(), 7));
            }
            stop_vm(&mut u, vm).await;
            let t = Instant::now();
            ok(
                &mut u,
                Request::RestoreBaseline {
                    vm_id: vm,
                    baseline_id: base.baseline_id,
                },
            )
            .await;
            let restore_ms = t.elapsed().as_millis();
            submit(&mut u, vm, None, VmAction::Start).await;
            let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
            let mut h = Harness::new(&ep, &mut u, vm).await;
            h.wait_desktop(vm, run).await;
            let (check, p) = h.mostra(vm, run).await;
            std::fs::write(ev.join("after-restore-mostra.png"), &p).unwrap();
            if check != Check::Ok(Marker::Absent) {
                infra.push(format!("V6 after restore: {check:?}"));
            }
            // Close the terminal: the next attempt starts from a clean desktop.
            h.type_text(vm, run, "exit\n").await;
            let p = h.wait_desktop(vm, run).await;
            h.end().await;
            if has_fixture_colour(&p) {
                log(
                    &ev,
                    "fixture colours still on the desktop after exit: fresh restore and boot",
                );
                stop_vm(&mut u, vm).await;
                ok(
                    &mut u,
                    Request::RestoreBaseline {
                        vm_id: vm,
                        baseline_id: base.baseline_id,
                    },
                )
                .await;
                boot(&mut u, &ep, vm, &ev).await;
            }
            cols.extend([
                out.calls.len().to_string(),
                if infra.is_empty() {
                    "0".to_owned()
                } else {
                    format!("{}: {}", infra.len(), infra.join("; "))
                },
                format!("{check:?}"),
                restore_ms.to_string(),
                t_iter.elapsed().as_secs().to_string(),
            ]);
            infra_total += infra.len();
            log(&evidence, &cols.join(" | "));
            tsv(&sample, &cols);
        }

        if outcomes {
            let outcomes_tsv = evidence.join("outcomes.tsv");
            tsv(
                &outcomes_tsv,
                &[
                    "case",
                    "session",
                    "expected",
                    "outcome",
                    "source",
                    "termination",
                    "verdict_rationale",
                    "harness_action",
                    "client_calls",
                    "unexpected_client_errors",
                    "vm_state_after",
                    "duration_s",
                ]
                .map(str::to_owned),
            );
            let cases = [
                (
                    "outcome-failed",
                    Script::Wrong,
                    Trigger::RevokeAfterDeclaration,
                    1800,
                    Some(FixtureMarker::Present),
                    TestOutcome::Failed,
                ),
                (
                    "outcome-inconclusive",
                    Script::EndEarly,
                    Trigger::None,
                    1800,
                    None,
                    TestOutcome::Inconclusive,
                ),
                (
                    "outcome-cancelled-revoke",
                    Script::UntilClosed,
                    Trigger::RevokeAfterShot,
                    1800,
                    None,
                    TestOutcome::Cancelled,
                ),
                (
                    "outcome-timed-out",
                    Script::UntilClosed,
                    Trigger::None,
                    60,
                    None,
                    TestOutcome::TimedOut,
                ),
                (
                    "outcome-cancelled-emergency-stop",
                    Script::UntilClosed,
                    Trigger::EmergencyAfterShot,
                    1800,
                    None,
                    TestOutcome::Cancelled,
                ),
                (
                    "outcome-infrastructure-error",
                    Script::UntilClosed,
                    Trigger::KillAfterShot,
                    1800,
                    None,
                    TestOutcome::InfrastructureError,
                ),
            ];
            for (label, script, trigger, duration_s, verify, expected) in cases {
                let ev = evidence.join(label);
                if status(&mut u, vm).await.0 != VmState::Running {
                    stop_vm(&mut u, vm).await;
                    boot(&mut u, &ep, vm, &ev).await;
                }
                let out = scenario(&mut u, &ep, &dir, vm, script, trigger, duration_s, &ev).await;
                let unexpected = unexpected_errors(&out.calls, script != Script::EndEarly);
                let Some(sid) = out.session else {
                    log(&evidence, &format!("{label}: no session"));
                    infra_total += 1;
                    continue;
                };
                let r = match verify {
                    Some(m) => match ok(
                        &mut u,
                        Request::VerifySession {
                            session_id: sid,
                            verifier: SessionVerifier::FixtureMarker { expected: m },
                        },
                    )
                    .await
                    {
                        ResponseBody::SessionReport { report } => *report,
                        other => panic!("{other:?}"),
                    },
                    None => report_of(&mut u, sid).await,
                };
                export(&mut u, sid, &ev).await;
                // Let the supervisor settle a failed VM before reading it.
                tokio::time::sleep(Duration::from_secs(3)).await;
                let after = status(&mut u, vm).await.0;
                let cols = vec![
                    label.to_owned(),
                    sid.to_string(),
                    format!("{expected:?}"),
                    format!("{:?}", r.outcome),
                    format!("{:?}", r.outcome_source),
                    format!("{:?}", r.termination),
                    r.verdicts
                        .last()
                        .map(|v| v.rationale.clone())
                        .unwrap_or_default(),
                    out.action.clone(),
                    out.calls
                        .iter()
                        .map(|(n, c)| c.as_ref().map_or_else(|| n.clone(), |c| format!("{n}!{c}")))
                        .collect::<Vec<_>>()
                        .join(","),
                    if unexpected.is_empty() {
                        "0".to_owned()
                    } else {
                        unexpected.join("; ")
                    },
                    format!("{after:?}"),
                    out.duration.as_secs().to_string(),
                ];
                if r.outcome != expected {
                    log(
                        &evidence,
                        &format!("{label}: outcome {:?}, expected {expected:?}", r.outcome),
                    );
                    infra_total += 1;
                }
                log(&evidence, &cols.join(" | "));
                tsv(&outcomes_tsv, &cols);
            }
        }
        stop_vm(&mut u, vm).await;
        ok(
            &mut u,
            Request::RestoreBaseline {
                vm_id: vm,
                baseline_id: base.baseline_id,
            },
        )
        .await;
        let audit =
            std::fs::read_to_string(dir.join("audit").join("audit.jsonl")).unwrap_or_default();
        log(
            &evidence,
            &format!(
                "audit: {} events; typed command present in audit: {}; infrastructure errors {infra_total}",
                audit.lines().count(),
                audit.contains("fx.sh")
            ),
        );
        handle.shutdown().await;
        infra_total
    });
    let _ = std::fs::remove_file(&fx_iso);
    assert_eq!(infra_total, 0, "infrastructure errors in the sample");
}
