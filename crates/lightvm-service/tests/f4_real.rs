//! F4 exit criterion with real QEMU through the service (ignored test):
//! an **agent session** drives a real VM — start, desktop wait, keyboard
//! input, fixture `scrivi`/`mostra` of ADR-0019 verified pixel-exact on the
//! agent's own screenshots (QMP `screendump`, PNG read back in chunks) —
//! under input ownership, pause and revocation; then the stop latencies
//! (revocation and Emergency Stop, UI request and local emergency command)
//! with the agent flooding input requests at the rate limit
//! (`docs/verification/F4-01-sessions-bridge.md`).
//!
//! Requires `LIGHTVM_E2E_QEMU_DIR`; optional `LIGHTVM_E2E_ISO`,
//! `LIGHTVM_F4_EVIDENCE_DIR` (PNG), `LIGHTVM_F4_SAMPLES` (stop samples per
//! kind, default 10), `LIGHTVM_E2E_CPU` (`Skylake-Client-v4`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightvm_fixture::verify::{Check, DEFAULT_CALIBRATION, Frame, Marker};
use lightvm_proto::ipc::{
    AgentCall, ClientMessage, InputOwner, Request, ResponseBody, Role, ServerMessage,
    SessionApproval, SessionAuth, base64_decode,
};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, Capability, CapabilitySet, ErrorCode, InputBatch,
    InputEvent, KeyAction, MediaKind, OperationId, QCode, ReplyError, RunId, SessionId,
    UtcTimestamp, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::sample_config;
use lightvm_service::frame::{self, FrameError};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, start};

const T: Duration = Duration::from_secs(120);

fn qemu_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR not set"))
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

/// Removes a test catalog even if the test fails (read-only managed copies).
struct RemoveCatalog(PathBuf);

impl Drop for RemoveCatalog {
    fn drop(&mut self) {
        for _ in 0..60 {
            clear_readonly(&self.0);
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        eprintln!("test catalog not removed: {}", self.0.display());
    }
}

fn clear_readonly(p: &Path) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                clear_readonly(&path);
            } else if let Ok(m) = e.metadata() {
                let mut perm = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(&path, perm);
            }
        }
    }
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

async fn import(c: &mut Client, path: &Path, kind: MediaKind) -> lightvm_proto::MediaRecord {
    let p = std::fs::canonicalize(path).unwrap();
    let p = p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
    match ok(
        c,
        Request::ImportMedia {
            source_path: p,
            media_kind: kind,
            declared_origin: "lab media".into(),
        },
    )
    .await
    {
        ResponseBody::MediaImported { media } => media,
        other => panic!("{other:?}"),
    }
}

async fn vm_state(c: &mut Client, vm: VmId) -> (VmState, Option<RunId>) {
    match ok(c, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => (vm.state, vm.run_id),
        other => panic!("{other:?}"),
    }
}

async fn wait_state(c: &mut Client, vm: VmId, want: VmState, limit: Duration) -> Option<RunId> {
    let t0 = Instant::now();
    loop {
        let (s, run) = vm_state(c, vm).await;
        if s == want {
            return run;
        }
        assert!(t0.elapsed() < limit, "VM not {want:?} (now {s:?})");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn reason(e: &lightvm_proto::PublicError) -> Option<&str> {
    e.details
        .as_ref()
        .and_then(|d| d.get("reason"))
        .map(String::as_str)
}

// ----- keyboard (US layout, ADR-0019) --------------------------------------

fn qcode_for(c: char) -> (bool, &'static str) {
    const LOWER: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match c {
        'a'..='z' => (false, LOWER[c as usize - 'a' as usize]),
        'A'..='Z' => (true, LOWER[c as usize - 'A' as usize]),
        '0'..='9' => (false, DIGITS[c as usize - '0' as usize]),
        ' ' => (false, "spc"),
        '-' => (false, "minus"),
        '_' => (true, "minus"),
        '/' => (false, "slash"),
        '.' => (false, "dot"),
        ';' => (false, "semicolon"),
        '|' => (true, "backslash"),
        '=' => (false, "equal"),
        '\n' => (false, "ret"),
        other => panic!("character not mapped: {other:?}"),
    }
}

fn key(q: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(q).unwrap(),
        down,
    })
}

fn fx_command(verb: &str) -> String {
    format!("test -f /mnt/fx.sh || sudo mount -r -L LIGHTVM_FX /mnt; sh /mnt/fx.sh {verb}\n")
}

fn input_request(vm: VmId, run: RunId, events: Vec<InputEvent>) -> VmRequest {
    VmRequest {
        operation_id: OperationId::new_v7(),
        vm_id: vm,
        expected_run_id: Some(run),
        action: VmAction::Input(InputBatch {
            vm_id: vm,
            run_id: run,
            // Keyboard batches do not depend on the geometry.
            geometry: lightvm_proto::DisplayGeometry::new(1280, 800).unwrap(),
            events,
        }),
    }
}

// ----- agent ----------------------------------------------------------------

/// Agent session client: renews the lease every 3 s while it works and
/// retries reads refused by the rate limit (never mutations).
struct Agent {
    c: Client,
    auth: SessionAuth,
    renewed: Instant,
    shots: u32,
}

impl Agent {
    async fn raw(&mut self, call: AgentCall) -> ServerMessage {
        tokio::time::timeout(
            T,
            self.c.request(Request::Agent {
                session: self.auth.clone(),
                call,
            }),
        )
        .await
        .expect("timeout")
        .unwrap()
    }

    async fn renew_if_due(&mut self) {
        if self.renewed.elapsed() >= Duration::from_secs(3) {
            match self.raw(AgentCall::RenewLease).await {
                ServerMessage::Response {
                    result: ResponseBody::Lease { .. },
                    ..
                } => self.renewed = Instant::now(),
                other => panic!("lease: {other:?}"),
            }
        }
    }

    async fn call(&mut self, call: AgentCall) -> ServerMessage {
        self.renew_if_due().await;
        self.raw(call).await
    }

    /// Read call, retried while refused by the rate limit.
    async fn read(&mut self, call: AgentCall) -> ResponseBody {
        loop {
            match self.call(call.clone()).await {
                ServerMessage::Response { result, .. } => return result,
                ServerMessage::Error {
                    error: ReplyError::Operational(e),
                    ..
                } if e.code == ErrorCode::ResourceLimit && e.message.contains("rate") => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => panic!("{}: {other:?}", call.name()),
            }
        }
    }

    async fn sleep(&mut self, d: Duration) {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            tokio::time::sleep(d.saturating_sub(t0.elapsed()).min(Duration::from_secs(2))).await;
            self.renew_if_due().await;
        }
    }

    async fn submit(&mut self, request: VmRequest) -> ServerMessage {
        // Mutations are not retried: a rate-limit refusal is waited out
        // before sending (40 requests/s at most from `type_text`).
        self.call(AgentCall::Submit { request }).await
    }

    async fn keys(&mut self, vm: VmId, run: RunId, events: Vec<InputEvent>) {
        let m = self.submit(input_request(vm, run, events)).await;
        assert!(
            matches!(
                m,
                ServerMessage::Response {
                    result: ResponseBody::Operation { .. },
                    ..
                }
            ),
            "agent input refused: {m:?}"
        );
    }

    async fn type_text(&mut self, vm: VmId, run: RunId, text: &str) {
        for ch in text.chars() {
            let (shift, q) = qcode_for(ch);
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
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Agent screenshot: metadata, chunks, SHA-256 check, PNG decoded to RGB.
    async fn screenshot(&mut self, vm: VmId, run: RunId) -> (usize, usize, Vec<u8>, Vec<u8>) {
        let op = OperationId::new_v7();
        let (meta, chunks) = match self
            .read(AgentCall::Screenshot {
                operation_id: op,
                vm_id: vm,
                expected_run_id: run,
            })
            .await
        {
            ResponseBody::AgentScreenshot {
                screenshot, chunks, ..
            } => (screenshot, chunks),
            other => panic!("{other:?}"),
        };
        self.shots += 1;
        let mut png = Vec::with_capacity(usize::try_from(meta.byte_len).unwrap());
        for index in 0..chunks {
            match self
                .read(AgentCall::ScreenshotChunk {
                    operation_id: op,
                    index,
                })
                .await
            {
                ResponseBody::ScreenshotChunk { data, .. } => {
                    png.extend(base64_decode(&data).expect("base64"));
                }
                other => panic!("{other:?}"),
            }
        }
        {
            use sha2::Digest as _;
            let d: [u8; 32] = sha2::Sha256::digest(&png).into();
            assert_eq!(&d, meta.sha256.as_bytes(), "screenshot SHA-256");
        }
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().expect("PNG");
        let mut buf = vec![0; reader.output_buffer_size().expect("PNG size")];
        let info = reader.next_frame(&mut buf).expect("PNG frame");
        assert_eq!(info.color_type, png::ColorType::Rgb);
        buf.truncate(info.buffer_size());
        (info.width as usize, info.height as usize, buf, png)
    }

    /// A0 (ADR-0019): two agent screenshots 5 s apart identical with ≥ 99 %
    /// lit pixels.
    async fn wait_desktop(&mut self, vm: VmId, run: RunId, limit: Duration) -> Option<Duration> {
        let t0 = Instant::now();
        let mut prev: Option<Vec<u8>> = None;
        while t0.elapsed() < limit {
            self.sleep(Duration::from_secs(5)).await;
            let (_, _, rgb, _) = self.screenshot(vm, run).await;
            if non_black_permille(&rgb) < 990 {
                prev = None;
                continue;
            }
            if prev.as_deref() == Some(rgb.as_slice()) {
                return Some(t0.elapsed());
            }
            prev = Some(rgb);
        }
        None
    }

    /// Two agent screenshots 1 s apart identical (20 s at most); returns the
    /// last one.
    async fn stable(&mut self, vm: VmId, run: RunId) -> (usize, usize, Vec<u8>, Vec<u8>) {
        let t0 = Instant::now();
        let mut prev = self.screenshot(vm, run).await;
        while t0.elapsed() < Duration::from_secs(20) {
            self.sleep(Duration::from_secs(1)).await;
            let s = self.screenshot(vm, run).await;
            if s.2 == prev.2 {
                return s;
            }
            prev = s;
        }
        prev
    }

    /// Ctrl+Alt+T, terminal open once the screen changes (A3), then the
    /// fixture verb typed by the agent; outcome of the pixel verifier on the
    /// agent's screenshot (one retry, as in F3).
    async fn fixture(&mut self, vm: VmId, run: RunId, verb: &str, evidence: &Path) -> Check {
        let mut out = Check::NotFound("not run".into());
        for attempt in 0..2 {
            let (_, _, before, _) = self.screenshot(vm, run).await;
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
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(15) {
                self.sleep(Duration::from_millis(500)).await;
                if self.screenshot(vm, run).await.2 != before {
                    break;
                }
            }
            self.stable(vm, run).await;
            self.type_text(vm, run, &fx_command(verb)).await;
            self.sleep(Duration::from_secs(2)).await;
            let (w, h, rgb, png) = self.stable(vm, run).await;
            let _ = std::fs::write(evidence.join(format!("f4-{verb}-{attempt}.png")), &png);
            out = Frame {
                width: w,
                height: h,
                rgb: &rgb,
            }
            .check(DEFAULT_CALIBRATION);
            if matches!(out, Check::Ok(_)) {
                return out;
            }
        }
        out
    }
}

fn non_black_permille(rgb: &[u8]) -> u64 {
    let px = rgb.as_chunks::<3>().0;
    let n = px.len().max(1) as u64;
    let lit = px.iter().filter(|p| p.iter().any(|&c| c > 16)).count() as u64;
    lit * 1000 / n
}

async fn pairing(u: &mut Client) -> Credential {
    match ok(u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    }
}

fn approval(vm: VmId, give_input: bool) -> SessionApproval {
    SessionApproval {
        vm_id: vm,
        capabilities: [
            Capability::VmList,
            Capability::VmStatus,
            Capability::VmStart,
            Capability::VmStop,
            Capability::VmScreenshot,
            Capability::VmInput,
        ]
        .into_iter()
        .collect(),
        duration_s: 3600,
        quotas: None,
        lease_ms: None,
        give_input,
        network_endpoints: Vec::new(),
    }
}

/// New pairing connection, session request, approval by the UI, grant.
async fn new_agent(endpoint: &str, u: &mut Client, a: SessionApproval) -> Agent {
    let cred = pairing(u).await;
    let mut c = Client::connect(endpoint).await.unwrap();
    assert!(matches!(
        c.hello(&cred).await.unwrap(),
        ServerMessage::HelloOk {
            role: Role::Pairing,
            ..
        }
    ));
    let request_id = match ok(
        &mut c,
        Request::RequestSession {
            client_name: "f4-real-agent".into(),
            purpose: "F4 exit criterion".into(),
            requested_vms: vec![a.vm_id],
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
    assert!(matches!(
        ok(
            u,
            Request::ApproveSession {
                request_id,
                approval: a
            }
        )
        .await,
        ResponseBody::Session { .. }
    ));
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
    Agent {
        c,
        auth,
        renewed: Instant::now(),
        shots: 0,
    }
}

// ----- flood ------------------------------------------------------------------

#[derive(Debug, Default)]
struct FloodStats {
    replies: u64,
    rate_limited: u64,
    closed_at: Option<Instant>,
}

/// The agent writes input requests without waiting for replies (harmless
/// shift press/release) until its connection is closed.
fn flood(
    agent: Agent,
    vm: VmId,
    run: RunId,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<FloodStats>,
) {
    let auth = agent.auth.clone();
    let (mut rd, mut wr) = agent.c.into_parts();
    let writer = tokio::spawn(async move {
        let mut id = 100_000u64;
        loop {
            id += 1;
            let m = ClientMessage::Request {
                request_id: id,
                body: Request::Agent {
                    session: auth.clone(),
                    call: AgentCall::Submit {
                        request: input_request(
                            vm,
                            run,
                            vec![key("shift", true), key("shift", false)],
                        ),
                    },
                },
            };
            if frame::write_frame(&mut wr, &m, u32::MAX).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let reader = tokio::spawn(async move {
        let mut st = FloodStats::default();
        loop {
            match frame::read_frame::<_, ServerMessage>(&mut rd, frame::DEFAULT_MAX_FRAME).await {
                Ok(ServerMessage::Closing { .. }) => {
                    st.closed_at.get_or_insert_with(Instant::now);
                }
                Ok(ServerMessage::Error {
                    error: ReplyError::Operational(e),
                    ..
                }) if e.code == ErrorCode::ResourceLimit => {
                    st.rate_limited += 1;
                    st.replies += 1;
                }
                Ok(_) => st.replies += 1,
                Err(FrameError::Closed | FrameError::Io(_)) => {
                    st.closed_at.get_or_insert_with(Instant::now);
                    break;
                }
                Err(_) => {}
            }
        }
        st
    });
    (writer, reader)
}

fn audit_events(path: &Path) -> Vec<AuditEvent> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Agent operations forwarded after the session's termination (must be 0)
/// and after the instant of the command (≤ 1: already inside the backend).
fn forwarded(ev: &[AuditEvent], s: SessionId, t: UtcTimestamp) -> (usize, usize) {
    let end = ev
        .iter()
        .find(|e| e.action == AuditAction::SessionTerminate && e.session_id == Some(s))
        .map_or(u64::MAX, |e| e.seq);
    let fw = |e: &&AuditEvent| e.session_id == Some(s) && e.outcome == AuditOutcome::Forwarded;
    (
        ev.iter().filter(fw).filter(|e| e.seq > end).count(),
        ev.iter().filter(fw).filter(|e| e.at > t).count(),
    )
}

fn stats(name: &str, v: &mut [f64]) -> String {
    v.sort_by(f64::total_cmp);
    let q = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
    format!(
        "{name}: n={} p50={:.1} ms p95={:.1} ms max={:.1} ms",
        v.len(),
        q(0.5),
        q(0.95),
        v.last().copied().unwrap_or(0.0)
    )
}

#[test]
#[ignore = "requires approved QEMU, an accelerator and the Ubuntu ISO (LIGHTVM_E2E_QEMU_DIR)"]
fn e2e_real_qemu_f4_agent_session() {
    let lab = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab");
    let iso = std::env::var_os("LIGHTVM_E2E_ISO").map_or_else(
        || lab.join("media").join("ubuntu-26.04.1-desktop-amd64.iso"),
        PathBuf::from,
    );
    let samples: usize = std::env::var("LIGHTVM_F4_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let evidence = std::env::var_os("LIGHTVM_F4_EVIDENCE_DIR").map_or_else(
        || std::env::temp_dir().join("lightvm-f4-evidence"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&evidence).unwrap();
    let dir = temp_dir("f4-real");
    let _cleanup = RemoveCatalog(dir.clone());
    let fx_iso = dir.with_extension("fx.iso");
    std::fs::write(&fx_iso, lightvm_fixture::fixture_iso()).unwrap();

    let store = SqliteStore::open(&dir).unwrap();
    let mut setup = QemuSetup::new(qemu_dir(), store.root(), short_dir("lvf"));
    setup.cpu_model = cpu;
    let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
    assert!(
        capacity.accelerator_available,
        "AI sessions need the accelerator (ADR-0007)"
    );
    let audit_file = dir.join("audit").join("audit.jsonl");
    let audit = FileAuditSink::open(&audit_file).unwrap();
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
        cfg.display_dir = Some(short_dir("lvd"));
        let (handle, ui_cred) = start(cfg, Box::new(engine), Box::new(host)).await.unwrap();
        let ep = handle.endpoint().to_owned();
        let mut u = Client::connect(&ep).await.unwrap();
        assert!(matches!(
            u.hello(&ui_cred).await.unwrap(),
            ServerMessage::HelloOk { role: Role::Ui, .. }
        ));
        let install = import(&mut u, &iso, MediaKind::InstallIso).await;
        let data = import(&mut u, &fx_iso, MediaKind::DataIso).await;
        let mut vmc = sample_config("f4-agent");
        vmc.vcpus = 4;
        vmc.memory_mib = 4096;
        vmc.disk_gib = 8;
        vmc.install_media = Some(install.id);
        vmc.data_media = Some(data.id);
        let vm = match ok(&mut u, Request::CreateVm { config: vmc }).await {
            ResponseBody::VmCreated { vm } => vm.id,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            ok(&mut u, Request::SetAgentAccess { enabled: true }).await,
            ResponseBody::AgentAccess { enabled: true }
        ));

        // ----- the agent starts the VM and waits for the desktop ------------
        let mut a = new_agent(&ep, &mut u, approval(vm, false)).await;
        let t = Instant::now();
        let m = a
            .submit(VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            })
            .await;
        assert!(matches!(m, ServerMessage::Response { result: ResponseBody::Operation { .. }, .. }), "agent start: {m:?}");
        let run = wait_state(&mut u, vm, VmState::Running, T).await.expect("run");
        eprintln!("agent start: running after {:?}", t.elapsed());
        let boot = a
            .wait_desktop(vm, run, Duration::from_secs(300))
            .await
            .expect("desktop (A0) on agent screenshots");
        eprintln!("desktop after {boot:?} ({} agent screenshots)", a.shots);
        let (w, h, rgb, png) = a.screenshot(vm, run).await;
        let _ = std::fs::write(evidence.join("f4-reference.png"), &png);
        assert!(
            !Frame { width: w, height: h, rgb: &rgb }.has_fixture_colour(),
            "fixture colours in the reference: inconclusive"
        );
        eprintln!("agent screenshot {w}x{h}, PNG {} bytes", png.len());

        // ----- input ownership: the user owns it until handed over -----------
        let e = match a.submit(input_request(vm, run, vec![key("shift", true), key("shift", false)])).await {
            ServerMessage::Error { error: ReplyError::Operational(e), .. } => e,
            other => panic!("{other:?}"),
        };
        assert_eq!((e.code, reason(&e)), (ErrorCode::PermissionDenied, Some("input_not_owned")));
        let owner = InputOwner::Agent { session_id: a.auth.session_id };
        ok(&mut u, Request::SetInputOwner { vm_id: vm, owner }).await;

        // ----- fixture driven by the agent ------------------------------------
        let v3 = a.fixture(vm, run, "mostra", &evidence).await;
        let v4 = a.fixture(vm, run, "scrivi", &evidence).await;
        let v5 = a.fixture(vm, run, "mostra", &evidence).await;
        eprintln!("fixture by the agent: mostra {v3:?}, scrivi {v4:?}, mostra {v5:?}");
        assert_eq!(v3, Check::Ok(Marker::Absent), "V3");
        assert_eq!(v4, Check::Ok(Marker::Present), "V4");
        assert_eq!(v5, Check::Ok(Marker::Present), "V5");

        // ----- user and agent never both inject -------------------------------
        let user_input = Request::SubmitVm {
            request: input_request(vm, run, vec![key("shift", true), key("shift", false)]),
        };
        match req(&mut u, user_input.clone()).await {
            ServerMessage::Error { error: ReplyError::Operational(e), .. } => {
                assert_eq!(reason(&e), Some("input_owned_by_agent"));
            }
            other => panic!("user input while the agent owns it: {other:?}"),
        }
        // Agent presses shift and keeps it; the user takes control back:
        // the pressed key is released by the service on the handover.
        a.keys(vm, run, vec![key("shift", true)]).await;
        ok(&mut u, Request::SetInputOwner { vm_id: vm, owner: InputOwner::User }).await;
        match a.submit(input_request(vm, run, vec![key("shift", false)])).await {
            ServerMessage::Error { error: ReplyError::Operational(e), .. } => {
                assert_eq!(reason(&e), Some("input_not_owned"));
            }
            other => panic!("agent input after the takeback: {other:?}"),
        }
        assert!(matches!(req(&mut u, user_input).await, ServerMessage::Response { .. }));
        ok(&mut u, Request::SetInputOwner { vm_id: vm, owner }).await;

        // ----- pause: no mutation, observation allowed ------------------------
        ok(&mut u, Request::PauseSession { session_id: a.auth.session_id }).await;
        match a.submit(input_request(vm, run, vec![key("shift", true), key("shift", false)])).await {
            ServerMessage::Error { error: ReplyError::Operational(e), .. } => {
                assert_eq!(e.message, "session paused");
            }
            other => panic!("{other:?}"),
        }
        a.screenshot(vm, run).await;
        ok(&mut u, Request::ResumeSession { session_id: a.auth.session_id }).await;
        a.keys(vm, run, vec![key("shift", true), key("shift", false)]).await;
        // End of the first session by the agent itself (VM not stopped).
        assert!(matches!(
            a.call(AgentCall::EndSession).await,
            ServerMessage::Response { result: ResponseBody::SessionEnded { .. }, .. }
        ));
        assert_eq!(vm_state(&mut u, vm).await.0, VmState::Running);

        // ----- revocation latency with a flooding agent (VM keeps running) ---
        let mut rev_closed = Vec::new();
        let mut rev_reply = Vec::new();
        let mut inflight_max = 0;
        let mut rate_limited = 0;
        for i in 0..samples {
            let agent = new_agent(&ep, &mut u, approval(vm, true)).await;
            let sid = agent.auth.session_id;
            let (writer, reader) = flood(agent, vm, run);
            tokio::time::sleep(Duration::from_millis(1000)).await;
            let utc0 = UtcTimestamp::now();
            let t0 = Instant::now();
            assert!(matches!(
                ok(&mut u, Request::RevokeSession { session_id: sid }).await,
                ResponseBody::SessionRevoked { .. }
            ));
            rev_reply.push(t0.elapsed().as_secs_f64() * 1e3);
            let st = tokio::time::timeout(T, reader).await.unwrap().unwrap();
            writer.abort();
            rev_closed.push(st.closed_at.unwrap().duration_since(t0).as_secs_f64() * 1e3);
            rate_limited += st.rate_limited;
            tokio::time::sleep(Duration::from_millis(300)).await;
            let (after_end, after_cmd) = forwarded(&audit_events(&audit_file), sid, utc0);
            assert_eq!(after_end, 0, "revoke sample {i}");
            assert!(after_cmd <= 1, "revoke sample {i}: {after_cmd}");
            inflight_max = inflight_max.max(after_cmd);
            assert_eq!(vm_state(&mut u, vm).await.0, VmState::Running, "revoke does not stop");
        }

        // ----- Emergency Stop latency with a flooding agent -------------------
        let mut es_closed = Vec::new();
        let mut es_done = Vec::new();
        let mut run = run;
        for i in 0..samples {
            if i > 0 {
                let m = ok(
                    &mut u,
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
                assert!(matches!(m, ResponseBody::Operation { .. }));
                run = wait_state(&mut u, vm, VmState::Running, T).await.unwrap();
                ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            let agent = new_agent(&ep, &mut u, approval(vm, true)).await;
            let sid = agent.auth.session_id;
            let (writer, reader) = flood(agent, vm, run);
            tokio::time::sleep(Duration::from_millis(1000)).await;
            let utc0 = UtcTimestamp::now();
            let t0 = Instant::now();
            let report = if i % 2 == 0 {
                match ok(&mut u, Request::EmergencyStop).await {
                    ResponseBody::EmergencyStop(r) => r,
                    other => panic!("{other:?}"),
                }
            } else {
                lightvm_service::client::emergency_stop(handle.emergency_endpoint())
                    .await
                    .unwrap()
            };
            es_done.push(t0.elapsed().as_secs_f64() * 1e3);
            assert!(report.completed, "end of the processes observed");
            assert_eq!(report.processes_terminated, 1);
            let st = tokio::time::timeout(T, reader).await.unwrap().unwrap();
            writer.abort();
            es_closed.push(st.closed_at.unwrap().duration_since(t0).as_secs_f64() * 1e3);
            rate_limited += st.rate_limited;
            wait_state(&mut u, vm, VmState::Stopped, T).await;
            let (after_end, after_cmd) = forwarded(&audit_events(&audit_file), sid, utc0);
            assert_eq!(after_end, 0, "emergency sample {i}");
            assert!(after_cmd <= 1, "emergency sample {i}: {after_cmd}");
            inflight_max = inflight_max.max(after_cmd);
        }
        eprintln!(
            "STOP LATENCY (real QEMU, agent flooding input at the rate limit: {rate_limited} rate-limited replies; at most {inflight_max} agent operation forwarded after a command)"
        );
        eprintln!("{}", stats("revoke -> agent connection closed (no new forwarding)", &mut rev_closed));
        eprintln!("{}", stats("revoke -> UI reply (core revocation recorded)", &mut rev_reply));
        eprintln!("{}", stats("emergency stop -> agent connection closed", &mut es_closed));
        eprintln!("{}", stats("emergency stop -> reply, end of the QEMU process observed", &mut es_done));

        // ----- audit: complete and without secrets --------------------------
        let events = audit_events(&audit_file);
        let n = |a: AuditAction, o: AuditOutcome| events.iter().filter(|e| e.action == a && e.outcome == o).count();
        eprintln!(
            "audit: {} events; session_create {}, session_terminate {}, input_control {}, vm_input forwarded {}, screenshot_read {}, session_lease {}, emergency_stop succeeded {}",
            events.len(),
            n(AuditAction::SessionCreate, AuditOutcome::Succeeded),
            n(AuditAction::SessionTerminate, AuditOutcome::Succeeded),
            n(AuditAction::InputControl, AuditOutcome::Succeeded),
            n(AuditAction::VmInput, AuditOutcome::Forwarded),
            n(AuditAction::ScreenshotRead, AuditOutcome::Succeeded),
            n(AuditAction::SessionLease, AuditOutcome::Succeeded),
            n(AuditAction::EmergencyStop, AuditOutcome::Succeeded),
        );
        let raw = std::fs::read_to_string(&audit_file).unwrap();
        assert!(!raw.contains("fx.sh"), "typed text in the audit");
        handle.shutdown().await;
    });
    let _ = std::fs::remove_file(&fx_iso);
}
