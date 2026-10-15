//! F4 — bridge and sessions through the real IPC (named pipe on Windows,
//! Unix socket on Linux), real core with in-memory ports, fake backend and
//! host (no QEMU): session lifecycle, gate, input ownership, quotas,
//! lifecycle rules, emergency endpoint, request × role matrix and the
//! stop-latency measurements with a flooding agent
//! (`docs/verification/F4-01-sessions-bridge.md`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    AgentCall, AgentVmView, ClientMessage, CloseReason, InputOwner, Request, ResponseBody, Role,
    ServerMessage, ServiceEvent, SessionApproval, SessionAuth, SessionInfo, SessionRequestOutcome,
    base64_decode,
};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, Capability, CapabilitySet, ErrorCode, InputBatch,
    InputEvent, KeyAction, OperationId, OperationStatus, QCode, Quotas, ReplyError, RunId,
    SessionId, SessionState, TerminationCause, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::fakes::{
    Calls, FAKE_GEOMETRY, FakeHost, FakeVmBackend, Gate, MemoryAudit, sample_config, sample_engine,
};
use lightvm_service::frame::{self, FrameError};
use lightvm_service::lock::LockError;
use lightvm_service::server::StartError;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(10);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    audit: MemoryAudit,
    gate: Arc<Gate>,
    dir: PathBuf,
}

#[derive(Clone)]
struct Opts {
    png_bytes: usize,
    input_delay: Option<Duration>,
    queue_capacity: usize,
    keep_vms: bool,
    /// Forward jump (ms) of the wall clock seen by the host-resume detector.
    clock_jump_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            png_bytes: 100_000,
            input_delay: None,
            queue_capacity: 64,
            keep_vms: false,
            clock_jump_ms: Arc::default(),
        }
    }
}

fn png(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 + 7) as u8).collect()
}

async fn start_svc_in(dir: &Path, o: &Opts) -> Result<Svc, StartError> {
    let calls = Calls::default();
    let gate = Gate::closed();
    gate.open();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        gate: Some(gate.clone()),
        screenshot_png: png(o.png_bytes),
        input_delay: o.input_delay,
        ..FakeVmBackend::default()
    };
    let audit = MemoryAudit::default();
    let engine = sample_engine(backend, audit.clone());
    let host = FakeHost {
        calls: calls.clone(),
    };
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.queue_capacity = o.queue_capacity;
    let jump = o.clock_jump_ms.clone();
    cfg.wall_clock = lightvm_service::resume::WallClock::new(move || {
        std::time::SystemTime::now()
            + Duration::from_millis(jump.load(std::sync::atomic::Ordering::SeqCst))
    });
    if o.keep_vms {
        cfg.parent_loss_policy = lightvm_proto::ipc::ParentLossPolicy::KeepVms;
    }
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(host)).await?;
    Ok(Svc {
        handle,
        ui,
        calls,
        audit,
        gate,
        dir: dir.to_path_buf(),
    })
}

async fn start_svc(name: &str, o: Opts) -> Svc {
    let dir = temp_dir(name);
    start_svc_in(&dir, &o).await.expect("service start")
}

async fn connect(endpoint: &str) -> Client {
    tokio::time::timeout(T, Client::connect(endpoint))
        .await
        .expect("connection timeout")
        .expect("connection")
}

async fn ui(svc: &Svc) -> Client {
    let mut c = connect(svc.handle.endpoint()).await;
    match c.hello(&svc.ui).await.unwrap() {
        ServerMessage::HelloOk { role: Role::Ui, .. } => c,
        other => panic!("{other:?}"),
    }
}

async fn ok(c: &mut Client, body: Request) -> ResponseBody {
    let name = body.name();
    match tokio::time::timeout(T, c.request(body))
        .await
        .expect("timeout")
        .unwrap()
    {
        ServerMessage::Response { result, .. } => result,
        other => panic!("{name}: {other:?}"),
    }
}

async fn err(c: &mut Client, body: Request) -> lightvm_proto::PublicError {
    let name = body.name();
    match tokio::time::timeout(T, c.request(body))
        .await
        .expect("timeout")
        .unwrap()
    {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => e,
        other => panic!("{name}: expected an error, got {other:?}"),
    }
}

fn reason(e: &lightvm_proto::PublicError) -> Option<&str> {
    e.details
        .as_ref()
        .and_then(|d| d.get("reason"))
        .map(String::as_str)
}

async fn pairing_credential(ui: &mut Client) -> Credential {
    match ok(ui, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    }
}

async fn agent_client(svc: &Svc, ui: &mut Client) -> Client {
    let cred = pairing_credential(ui).await;
    let mut c = connect(svc.handle.endpoint()).await;
    match c.hello(&cred).await.unwrap() {
        ServerMessage::HelloOk {
            role: Role::Pairing,
            ..
        } => c,
        other => panic!("{other:?}"),
    }
}

async fn set_access(ui: &mut Client, enabled: bool) {
    assert_eq!(
        ok(ui, Request::SetAgentAccess { enabled }).await,
        ResponseBody::AgentAccess { enabled }
    );
}

fn caps(c: &[Capability]) -> CapabilitySet {
    c.iter().copied().collect()
}

const AGENT_CAPS: [Capability; 7] = [
    Capability::VmList,
    Capability::VmStatus,
    Capability::VmStart,
    Capability::VmStop,
    Capability::VmReset,
    Capability::VmScreenshot,
    Capability::VmInput,
];

fn approval(vm: VmId) -> SessionApproval {
    SessionApproval {
        vm_id: vm,
        capabilities: caps(&AGENT_CAPS),
        duration_s: 3600,
        quotas: None,
        lease_ms: None,
        give_input: false,
        network_endpoints: Vec::new(),
    }
}

/// Request, approval by the UI, grant delivered to the agent.
async fn session(
    ui: &mut Client,
    agent: &mut Client,
    a: SessionApproval,
) -> (SessionAuth, SessionInfo) {
    let (request_id, code) = match ok(
        agent,
        Request::RequestSession {
            client_name: "test-agent".into(),
            purpose: "F4 test".into(),
            requested_vms: vec![a.vm_id],
            requested_capabilities: a.capabilities.clone(),
            requested_endpoints: Vec::new(),
        },
    )
    .await
    {
        ResponseBody::SessionRequested {
            request_id,
            verification_code,
            ..
        } => (request_id, verification_code),
        other => panic!("{other:?}"),
    };
    match ok(ui, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => {
            let r = requests
                .iter()
                .find(|r| r.request_id == request_id)
                .expect("listed");
            assert_eq!(r.verification_code, code);
            assert_eq!(r.client_name, "test-agent");
        }
        other => panic!("{other:?}"),
    }
    agent
        .send(&ClientMessage::Request {
            request_id: 9_000,
            body: Request::AwaitSession { request_id },
        })
        .await
        .unwrap();
    let info = match ok(
        ui,
        Request::ApproveSession {
            request_id,
            approval: a,
        },
    )
    .await
    {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    };
    let auth = match tokio::time::timeout(T, agent.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Response {
            request_id: 9_000,
            result:
                ResponseBody::SessionGranted {
                    session_id,
                    token,
                    grant,
                    lease,
                    ..
                },
        } => {
            assert_eq!(session_id, info.session_id);
            assert_eq!(grant.session_id, session_id);
            assert!(grant.vm_allowlist.contains(&info.vm_id));
            assert!(lease.expires_at > lease.renewed_at);
            SessionAuth { session_id, token }
        }
        other => panic!("{other:?}"),
    };
    (auth, info)
}

async fn call(agent: &mut Client, auth: &SessionAuth, call: AgentCall) -> ServerMessage {
    tokio::time::timeout(
        T,
        agent.request(Request::Agent {
            session: auth.clone(),
            call,
        }),
    )
    .await
    .expect("timeout")
    .unwrap()
}

async fn call_ok(agent: &mut Client, auth: &SessionAuth, c: AgentCall) -> ResponseBody {
    let name = c.name();
    match call(agent, auth, c).await {
        ServerMessage::Response { result, .. } => result,
        other => panic!("{name}: {other:?}"),
    }
}

async fn call_err(
    agent: &mut Client,
    auth: &SessionAuth,
    c: AgentCall,
) -> lightvm_proto::PublicError {
    let name = c.name();
    match call(agent, auth, c).await {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => e,
        other => panic!("{name}: expected an error, got {other:?}"),
    }
}

fn op_id() -> OperationId {
    let mut b = [0u8; 10];
    getrandom::fill(&mut b).unwrap();
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    OperationId::from_unix_millis(ms, b)
}

async fn sample_vm(ui: &mut Client) -> VmId {
    match ok(ui, Request::ListVms).await {
        ResponseBody::VmList { vms } => vms[0].vm_id,
        other => panic!("{other:?}"),
    }
}

async fn vm_state(ui: &mut Client, vm: VmId) -> (VmState, Option<RunId>) {
    match ok(ui, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => (vm.state, vm.run_id),
        other => panic!("{other:?}"),
    }
}

async fn wait_state(ui: &mut Client, vm: VmId, want: VmState) -> Option<RunId> {
    let deadline = Instant::now() + T;
    loop {
        let (s, run) = vm_state(ui, vm).await;
        if s == want {
            return run;
        }
        assert!(Instant::now() < deadline, "VM not {want:?} (now {s:?})");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn ui_start(ui: &mut Client, vm: VmId) -> RunId {
    let r = ok(
        ui,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: op_id(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        },
    )
    .await;
    assert!(matches!(r, ResponseBody::Operation { .. }), "{r:?}");
    wait_state(ui, vm, VmState::Running).await.unwrap()
}

fn keys(vm: VmId, run: RunId, n: usize) -> VmAction {
    let a = QCode::parse("a").unwrap();
    VmAction::Input(InputBatch {
        vm_id: vm,
        run_id: run,
        geometry: FAKE_GEOMETRY,
        events: (0..n)
            .map(|i| {
                InputEvent::Key(KeyAction {
                    qcode: a.clone(),
                    down: i % 2 == 0,
                })
            })
            .collect(),
    })
}

fn input_req(vm: VmId, run: RunId, n: usize) -> VmRequest {
    VmRequest {
        operation_id: op_id(),
        vm_id: vm,
        expected_run_id: Some(run),
        action: keys(vm, run, n),
    }
}

fn count_calls(svc: &Svc, prefix: &str) -> usize {
    svc.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with(prefix))
        .count()
}

async fn expect_closing(agent: &mut Client) {
    loop {
        match tokio::time::timeout(T, agent.recv())
            .await
            .expect("timeout")
        {
            Ok(ServerMessage::Closing {
                reason: CloseReason::Revoked,
            }) => return,
            Ok(ServerMessage::Response { .. } | ServerMessage::Error { .. }) => {}
            other => panic!("expected closing, got {other:?}"),
        }
    }
}

async fn wait_event(ui: &mut Client, f: impl Fn(&ServiceEvent) -> bool) -> ServiceEvent {
    let deadline = Instant::now() + T;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, ui.next_event()).await {
            Ok(Ok(Ok(e))) if f(&e) => return e,
            Ok(Ok(Ok(_) | Err(_))) => {}
            Ok(Err(e)) => panic!("{e}"),
            Err(_) => panic!("event not received"),
        }
    }
}

fn audit(svc: &Svc) -> Vec<AuditEvent> {
    svc.audit.events()
}

// ----- tests ----------------------------------------------------------------

/// MCP/agent access is off at every service start; session requests are
/// refused until the user turns it on; turning it off denies the pending
/// requests (ROADMAP 9, ADR-0006).
#[tokio::test(flavor = "multi_thread")]
async fn agent_access_is_off_at_start_and_gates_session_requests() {
    let svc = start_svc("f4-access", Opts::default()).await;
    assert!(!svc.handle.agent_access());
    let mut u = ui(&svc).await;
    assert_eq!(
        ok(&mut u, Request::GetAgentAccess).await,
        ResponseBody::AgentAccess { enabled: false }
    );
    let mut a = agent_client(&svc, &mut u).await;
    let req = Request::RequestSession {
        client_name: "x".into(),
        purpose: "y".into(),
        requested_vms: Vec::new(),
        requested_capabilities: CapabilitySet::new(),
        requested_endpoints: Vec::new(),
    };
    let e = err(&mut a, req.clone()).await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(reason(&e), Some("agent_access_disabled"));
    assert!(matches!(
        ok(&mut u, Request::Subscribe).await,
        ResponseBody::Subscribed
    ));
    set_access(&mut u, true).await;
    wait_event(&mut u, |e| {
        matches!(e, ServiceEvent::AgentAccess { enabled: true })
    })
    .await;
    let request_id = match ok(&mut a, req).await {
        ResponseBody::SessionRequested {
            request_id,
            verification_code,
            expires_in_s,
        } => {
            assert_eq!(expires_in_s, 300);
            assert!(verification_code.len() == 7 && verification_code.as_bytes()[3] == b'-');
            request_id
        }
        other => panic!("{other:?}"),
    };
    let ev = wait_event(&mut u, |e| matches!(e, ServiceEvent::SessionRequest { .. })).await;
    let ServiceEvent::SessionRequest { request } = ev else {
        unreachable!()
    };
    assert_eq!(request.request_id, request_id);
    // A second request on the same connection is refused.
    let e = err(
        &mut a,
        Request::RequestSession {
            client_name: "x".into(),
            purpose: "y".into(),
            requested_vms: Vec::new(),
            requested_capabilities: CapabilitySet::new(),
            requested_endpoints: Vec::new(),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::StateConflict);
    // Turning the switch off denies the pending request.
    set_access(&mut u, false).await;
    let e = err(&mut a, Request::AwaitSession { request_id }).await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(reason(&e), Some("agent_access_disabled"));
    wait_event(&mut u, |e| {
        matches!(
            e,
            ServiceEvent::SessionRequestClosed {
                outcome: SessionRequestOutcome::Denied,
                ..
            }
        )
    })
    .await;
    // Audit: refused request, switch on, accepted request, switch off, denial.
    let actions: Vec<(AuditAction, AuditOutcome)> = audit(&svc)
        .iter()
        .filter(|e| {
            matches!(
                e.action,
                AuditAction::SessionRequest
                    | AuditAction::AgentAccessEnable
                    | AuditAction::AgentAccessDisable
                    | AuditAction::SessionDeny
            )
        })
        .map(|e| (e.action, e.outcome))
        .collect();
    assert_eq!(
        actions,
        vec![
            (AuditAction::SessionRequest, AuditOutcome::Denied),
            (AuditAction::AgentAccessEnable, AuditOutcome::Succeeded),
            (AuditAction::SessionRequest, AuditOutcome::Accepted),
            // Second request on the same connection.
            (AuditAction::SessionRequest, AuditOutcome::Denied),
            (AuditAction::AgentAccessDisable, AuditOutcome::Succeeded),
            (AuditAction::SessionDeny, AuditOutcome::Succeeded),
        ]
    );
    svc.handle.shutdown().await;
}

/// Denial by the user and expiry of a request; the decision reaches an
/// agent that awaits it later too.
#[tokio::test(flavor = "multi_thread")]
async fn denied_and_expired_requests() {
    let dir = temp_dir("f4-deny");
    let calls = Calls::default();
    let engine = sample_engine(
        FakeVmBackend {
            calls: calls.clone(),
            ..FakeVmBackend::default()
        },
        MemoryAudit::default(),
    );
    let mut cfg = ServiceConfig::new(&dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.session_request_ttl = Duration::from_millis(600);
    let (handle, ui_cred) = start(cfg, Box::new(engine), Box::new(FakeHost { calls }))
        .await
        .unwrap();
    let mut u = connect(handle.endpoint()).await;
    u.hello(&ui_cred).await.unwrap();
    set_access(&mut u, true).await;
    let cred = pairing_credential(&mut u).await;
    let mut a = connect(handle.endpoint()).await;
    a.hello(&cred).await.unwrap();
    let rq = || Request::RequestSession {
        client_name: "x".into(),
        purpose: "y".into(),
        requested_vms: Vec::new(),
        requested_capabilities: CapabilitySet::new(),
        requested_endpoints: Vec::new(),
    };
    let id = match ok(&mut a, rq()).await {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        ok(&mut u, Request::DenySession { request_id: id }).await,
        ResponseBody::SessionDenied { request_id: id }
    );
    assert_eq!(
        err(&mut u, Request::DenySession { request_id: id })
            .await
            .code,
        ErrorCode::NotFound
    );
    let e = err(&mut a, Request::AwaitSession { request_id: id }).await;
    assert_eq!(
        (e.code, reason(&e)),
        (ErrorCode::PermissionDenied, Some("denied"))
    );
    // Expiry while awaiting.
    let id = match ok(&mut a, rq()).await {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    let t0 = Instant::now();
    let e = err(&mut a, Request::AwaitSession { request_id: id }).await;
    assert_eq!(
        (e.code, reason(&e)),
        (ErrorCode::SessionExpired, Some("request_expired"))
    );
    assert!(t0.elapsed() >= Duration::from_millis(400));
    // An expired request cannot be approved.
    let vm = sample_vm(&mut u).await;
    let e = err(
        &mut u,
        Request::ApproveSession {
            request_id: id,
            approval: approval(vm),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::NotFound);
    handle.shutdown().await;
}

/// The whole path: approval, filtered list, capabilities, screenshot in
/// chunks with its SHA-256, input ownership with handover and key release,
/// pause, revocation that closes the agent connection.
#[tokio::test(flavor = "multi_thread")]
async fn session_lifecycle_through_the_bridge() {
    let svc = start_svc("f4-life", Opts::default()).await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let other = match ok(
        &mut u,
        Request::CreateVm {
            config: sample_config("other"),
        },
    )
    .await
    {
        ResponseBody::VmCreated { vm } => vm.id,
        r => panic!("{r:?}"),
    };
    let mut a = agent_client(&svc, &mut u).await;
    let (auth, info) = session(&mut u, &mut a, approval(vm)).await;
    assert_eq!(info.state, SessionState::Active);
    assert!(!info.input_owner);

    // Grant-filtered list; the other VM is invisible and refused.
    match call_ok(&mut a, &auth, AgentCall::ListVms).await {
        ResponseBody::AgentVms { vms } => {
            assert_eq!(vms.len(), 1);
            assert_eq!(vms[0].vm_id, vm);
        }
        r => panic!("{r:?}"),
    }
    let e = call_err(&mut a, &auth, AgentCall::VmStatus { vm_id: other }).await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    let e = call_err(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: VmRequest {
                operation_id: op_id(),
                vm_id: other,
                expected_run_id: None,
                action: VmAction::Start,
            },
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);

    // Start by the agent (capability `vm.start`).
    match call_ok(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: VmRequest {
                operation_id: op_id(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        },
    )
    .await
    {
        ResponseBody::Operation { operation, .. } => {
            assert_eq!(operation.unwrap().status, OperationStatus::Succeeded);
        }
        r => panic!("{r:?}"),
    }
    let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();

    // Screenshot: metadata, then the bytes in chunks, SHA-256 verified.
    let shot_op = op_id();
    let (meta, chunks) = match call_ok(
        &mut a,
        &auth,
        AgentCall::Screenshot {
            operation_id: shot_op,
            vm_id: vm,
            expected_run_id: run,
        },
    )
    .await
    {
        ResponseBody::AgentScreenshot {
            screenshot,
            chunks,
            chunk_bytes,
            operation,
        } => {
            assert_eq!(operation.id, shot_op);
            assert_eq!(chunk_bytes, 32 * 1024);
            assert_eq!(screenshot.byte_len, 100_000);
            (screenshot, chunks)
        }
        r => panic!("{r:?}"),
    };
    assert_eq!(chunks, 4);
    let mut bytes = Vec::new();
    for index in 0..chunks {
        match call_ok(
            &mut a,
            &auth,
            AgentCall::ScreenshotChunk {
                operation_id: shot_op,
                index,
            },
        )
        .await
        {
            ResponseBody::ScreenshotChunk { data, .. } => {
                bytes.extend(base64_decode(&data).unwrap());
            }
            r => panic!("{r:?}"),
        }
    }
    {
        use sha2::Digest as _;
        let d: [u8; 32] = sha2::Sha256::digest(&bytes).into();
        assert_eq!(d, *meta.sha256.as_bytes());
    }
    assert_eq!(bytes, png(100_000));
    let e = call_err(
        &mut a,
        &auth,
        AgentCall::ScreenshotChunk {
            operation_id: shot_op,
            index: 4,
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::InvalidArgument);

    // Input: the user owns it by default.
    let e = call_err(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 2),
        },
    )
    .await;
    assert_eq!(
        (e.code, reason(&e)),
        (ErrorCode::PermissionDenied, Some("input_not_owned"))
    );
    assert!(matches!(
        ok(&mut u, Request::Subscribe).await,
        ResponseBody::Subscribed
    ));
    let releases = count_calls(&svc, "vm_release_input");
    let owner = InputOwner::Agent {
        session_id: auth.session_id,
    };
    assert_eq!(
        ok(&mut u, Request::SetInputOwner { vm_id: vm, owner }).await,
        ResponseBody::InputOwner { vm_id: vm, owner }
    );
    wait_event(
        &mut u,
        |e| matches!(e, ServiceEvent::InputOwner { owner: o, .. } if *o == owner),
    )
    .await;
    assert_eq!(count_calls(&svc, "vm_release_input"), releases + 1);
    match call_ok(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 2),
        },
    )
    .await
    {
        ResponseBody::Operation { operation, .. } => {
            assert_eq!(operation.unwrap().status, OperationStatus::Succeeded);
        }
        r => panic!("{r:?}"),
    }
    // The user cannot inject meanwhile.
    let e = err(
        &mut u,
        Request::SubmitVm {
            request: input_req(vm, run, 1),
        },
    )
    .await;
    assert_eq!(
        (e.code, reason(&e)),
        (ErrorCode::PermissionDenied, Some("input_owned_by_agent"))
    );

    // Pause: mutations refused, observation allowed.
    match ok(
        &mut u,
        Request::PauseSession {
            session_id: auth.session_id,
        },
    )
    .await
    {
        ResponseBody::Session { session } => assert_eq!(session.state, SessionState::Paused),
        r => panic!("{r:?}"),
    }
    let e = call_err(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 1),
        },
    )
    .await;
    assert_eq!(
        (e.code, e.message.as_str()),
        (ErrorCode::PermissionDenied, "session paused")
    );
    assert!(matches!(
        call_ok(&mut a, &auth, AgentCall::VmStatus { vm_id: vm }).await,
        ResponseBody::AgentVm {
            vm: AgentVmView {
                input_owner: true,
                ..
            }
        }
    ));
    assert_eq!(
        err(
            &mut u,
            Request::PauseSession {
                session_id: auth.session_id
            }
        )
        .await
        .code,
        ErrorCode::StateConflict
    );
    match ok(
        &mut u,
        Request::ResumeSession {
            session_id: auth.session_id,
        },
    )
    .await
    {
        ResponseBody::Session { session } => assert_eq!(session.state, SessionState::Active),
        r => panic!("{r:?}"),
    }
    assert!(matches!(
        call_ok(
            &mut a,
            &auth,
            AgentCall::Submit {
                request: input_req(vm, run, 1)
            }
        )
        .await,
        ResponseBody::Operation { .. }
    ));
    // Take control back: keys released, agent input refused, user input ok.
    let releases = count_calls(&svc, "vm_release_input");
    ok(
        &mut u,
        Request::SetInputOwner {
            vm_id: vm,
            owner: InputOwner::User,
        },
    )
    .await;
    assert_eq!(count_calls(&svc, "vm_release_input"), releases + 1);
    let e = call_err(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 1),
        },
    )
    .await;
    assert_eq!(reason(&e), Some("input_not_owned"));
    assert!(matches!(
        ok(
            &mut u,
            Request::SubmitVm {
                request: input_req(vm, run, 1)
            }
        )
        .await,
        ResponseBody::Operation { .. }
    ));
    // Lease renewal.
    match call_ok(&mut a, &auth, AgentCall::RenewLease).await {
        ResponseBody::Lease { lease } => assert_eq!(lease.session_id, auth.session_id),
        r => panic!("{r:?}"),
    }
    // The UI sees the session and its usage.
    match ok(&mut u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => {
            assert_eq!(sessions.len(), 1);
            let s = &sessions[0];
            assert_eq!(s.session_id, auth.session_id);
            assert_eq!(s.client_name, "test-agent");
            // start, screenshot, two inputs (the paused one never reached
            // the core).
            assert_eq!(s.usage.operations, 4, "{:?}", s.usage);
            assert_eq!(s.usage.screenshots, 1);
        }
        r => panic!("{r:?}"),
    }

    // Revocation: the agent connection is closed; the VM keeps running.
    let force_stops = count_calls(&svc, "vm_force_stop");
    match ok(
        &mut u,
        Request::RevokeSession {
            session_id: auth.session_id,
        },
    )
    .await
    {
        ResponseBody::SessionRevoked { session_id, .. } => assert_eq!(session_id, auth.session_id),
        r => panic!("{r:?}"),
    }
    expect_closing(&mut a).await;
    assert_eq!(count_calls(&svc, "vm_force_stop"), force_stops);
    assert_eq!(vm_state(&mut u, vm).await.0, VmState::Running);
    wait_event(&mut u, |e| {
        matches!(
            e,
            ServiceEvent::SessionChanged {
                state: SessionState::Terminated,
                termination: Some(TerminationCause::Revoked),
                ..
            }
        )
    })
    .await;
    // The token is useless on any other connection.
    let mut a2 = agent_client(&svc, &mut u).await;
    let e = call_err(&mut a2, &auth, AgentCall::ListVms).await;
    assert_eq!(e.code, ErrorCode::Unauthenticated);
    // Audit of the agent calls: never the token, never typed text.
    let ev = audit(&svc);
    let json = serde_json::to_string(&ev).unwrap();
    assert!(!json.contains(&serde_json::to_string(&auth.token).unwrap().replace('"', "")));
    assert!(ev.iter().any(|e| e.action == AuditAction::ScreenshotRead));
    assert!(ev.iter().any(|e| e.action == AuditAction::VmList));
    svc.handle.shutdown().await;
}

/// The session is bound to the connection that requested it: its token is
/// refused on another connection; the UI role cannot make agent calls.
#[tokio::test(flavor = "multi_thread")]
async fn session_is_bound_to_its_connection_and_role() {
    let svc = start_svc("f4-bound", Opts::default()).await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let mut a = agent_client(&svc, &mut u).await;
    let (auth, _) = session(&mut u, &mut a, approval(vm)).await;
    let mut thief = agent_client(&svc, &mut u).await;
    let e = call_err(&mut thief, &auth, AgentCall::ListVms).await;
    assert_eq!(e.code, ErrorCode::Unauthenticated);
    let e = err(
        &mut u,
        Request::Agent {
            session: auth.clone(),
            call: AgentCall::ListVms,
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    // Wrong token on the right connection.
    let bad = SessionAuth {
        session_id: auth.session_id,
        token: lightvm_proto::SessionToken::from_bytes([7; 32]),
    };
    let e = call_err(&mut a, &bad, AgentCall::ListVms).await;
    assert_eq!(e.code, ErrorCode::Unauthenticated);
    // The genuine one still works.
    assert!(matches!(
        call_ok(&mut a, &auth, AgentCall::ListVms).await,
        ResponseBody::AgentVms { .. }
    ));
    // One live session (MVP): a second approval is refused.
    let mut b = agent_client(&svc, &mut u).await;
    let id = match ok(
        &mut b,
        Request::RequestSession {
            client_name: "b".into(),
            purpose: "b".into(),
            requested_vms: Vec::new(),
            requested_capabilities: CapabilitySet::new(),
            requested_endpoints: Vec::new(),
        },
    )
    .await
    {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        r => panic!("{r:?}"),
    };
    let e = err(
        &mut u,
        Request::ApproveSession {
            request_id: id,
            approval: approval(vm),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::ResourceLimit);
    // The request stays pending and can be denied.
    ok(&mut u, Request::DenySession { request_id: id }).await;
    svc.handle.shutdown().await;
}

/// Lease without renewal: the session ends (`lease_expired`) and its VM is
/// stopped with the emergency path; renewals keep it alive.
#[tokio::test(flavor = "multi_thread")]
async fn lease_expiry_terminates_the_session_and_stops_the_vm() {
    let svc = start_svc("f4-lease", Opts::default()).await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    ui_start(&mut u, vm).await;
    let mut a = agent_client(&svc, &mut u).await;
    let mut ap = approval(vm);
    ap.lease_ms = Some(1_000);
    let (auth, _) = session(&mut u, &mut a, ap).await;
    // Renewed every 300 ms for 2.5 s: alive.
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(matches!(
            call_ok(&mut a, &auth, AgentCall::RenewLease).await,
            ResponseBody::Lease { .. }
        ));
    }
    assert_eq!(vm_state(&mut u, vm).await.0, VmState::Running);
    // No renewal: terminated within lease + one observation interval.
    let t0 = Instant::now();
    expect_closing(&mut a).await;
    let waited = t0.elapsed();
    assert!(waited < Duration::from_secs(4), "{waited:?}");
    wait_state(&mut u, vm, VmState::Stopped).await;
    assert!(count_calls(&svc, "vm_force_stop") >= 1);
    match ok(&mut u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => {
            assert_eq!(
                sessions[0].termination,
                Some(TerminationCause::LeaseExpired)
            );
        }
        r => panic!("{r:?}"),
    }
    eprintln!("lease 1000 ms: connection closed {waited:?} after the last renewal");
    svc.handle.shutdown().await;
}

/// Loss of the agent connection: the session ends (`client_lost`) and its
/// VM is stopped; a revoked session's later close stops nothing.
#[tokio::test(flavor = "multi_thread")]
async fn client_loss_terminates_the_session_and_stops_the_vm() {
    let svc = start_svc("f4-lost", Opts::default()).await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    ui_start(&mut u, vm).await;
    let mut a = agent_client(&svc, &mut u).await;
    let (_auth, _) = session(&mut u, &mut a, approval(vm)).await;
    drop(a);
    wait_state(&mut u, vm, VmState::Stopped).await;
    match ok(&mut u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => {
            assert_eq!(sessions[0].termination, Some(TerminationCause::ClientLost));
        }
        r => panic!("{r:?}"),
    }
    svc.handle.shutdown().await;
}

/// F6 / ROADMAP 7.3, host suspend/resume with an injected forward jump of
/// the wall clock seen by the supervisor (a 10-minute sleep): the agent
/// session ends with `host_resumed` before any further agent request is
/// served (no input forwarded after the resume), its VM is stopped on the
/// emergency path, the report says `cancelled`, the old token is refused;
/// continuing needs a new approval, which works. Small jumps (within the
/// detector's gap) change nothing.
#[tokio::test(flavor = "multi_thread")]
async fn host_resume_terminates_the_sessions_and_stops_their_vms() {
    use std::sync::atomic::Ordering::SeqCst;
    let o = Opts::default();
    let jump = o.clock_jump_ms.clone();
    let svc = start_svc("f6-resume", o).await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let run = ui_start(&mut u, vm).await;
    let mut a = agent_client(&svc, &mut u).await;
    let mut ap = approval(vm);
    ap.give_input = true;
    let (auth, info) = session(&mut u, &mut a, ap).await;
    call_ok(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 2),
        },
    )
    .await;
    // A 2 s jump is within the gap: nothing happens.
    jump.store(2_000, SeqCst);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    call_ok(&mut a, &auth, AgentCall::RenewLease).await;
    let inputs_before = count_calls(&svc, "vm_input");
    assert!(inputs_before >= 1);
    // The host sleeps for 10 minutes (wall clock + 600 s), then the agent
    // sends input right away.
    jump.store(602_000, SeqCst);
    let r = tokio::time::timeout(
        T,
        a.request(Request::Agent {
            session: auth.clone(),
            call: AgentCall::Submit {
                request: input_req(vm, run, 2),
            },
        }),
    )
    .await
    .expect("timeout");
    match r {
        Ok(ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        }) => assert!(
            matches!(
                e.code,
                ErrorCode::Unauthenticated
                    | ErrorCode::PermissionDenied
                    | ErrorCode::SessionExpired
            ),
            "{e:?}"
        ),
        // The termination closed the agent connection first.
        Ok(ServerMessage::Closing { .. }) | Err(_) => {}
        Ok(other) => panic!("agent request served after the resume: {other:?}"),
    }
    wait_state(&mut u, vm, VmState::Stopped).await;
    assert_eq!(
        count_calls(&svc, "vm_input"),
        inputs_before,
        "input forwarded after the resume"
    );
    match ok(&mut u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => {
            let s = sessions
                .iter()
                .find(|s| s.session_id == info.session_id)
                .unwrap();
            assert_eq!(s.state, SessionState::Terminated);
            assert_eq!(s.termination, Some(TerminationCause::HostResumed));
        }
        r => panic!("{r:?}"),
    }
    match ok(
        &mut u,
        Request::GetSessionReport {
            session_id: info.session_id,
        },
    )
    .await
    {
        ResponseBody::SessionReport { report } => {
            assert_eq!(report.termination, Some(TerminationCause::HostResumed));
            assert_eq!(report.outcome, lightvm_proto::TestOutcome::Cancelled);
        }
        r => panic!("{r:?}"),
    }
    assert!(
        audit(&svc)
            .iter()
            .any(|e| e.action == AuditAction::SessionTerminate
                && e.termination == Some(TerminationCause::HostResumed)),
        "termination audited with its cause"
    );
    // The old token is refused; a new approval gives a working session.
    let mut a2 = agent_client(&svc, &mut u).await;
    let run2 = ui_start(&mut u, vm).await;
    let mut ap = approval(vm);
    ap.give_input = true;
    let (auth2, _) = session(&mut u, &mut a2, ap).await;
    call_ok(
        &mut a2,
        &auth2,
        AgentCall::Submit {
            request: input_req(vm, run2, 2),
        },
    )
    .await;
    assert_eq!(count_calls(&svc, "vm_input"), inputs_before + 1);
    svc.handle.shutdown().await;
}

/// Loss of the UI (even with the `keep_vms` policy, which leaves the user's
/// VMs running): the agent sessions end (`ui_closed`) and their VMs are
/// stopped (ROADMAP 7.3).
#[tokio::test(flavor = "multi_thread")]
async fn ui_loss_terminates_the_sessions_and_stops_their_vms() {
    let svc = start_svc(
        "f4-uiloss",
        Opts {
            keep_vms: true,
            ..Opts::default()
        },
    )
    .await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    ui_start(&mut u, vm).await;
    let mut a = agent_client(&svc, &mut u).await;
    let (_auth, _) = session(&mut u, &mut a, approval(vm)).await;
    svc.handle.parent_lost().await;
    expect_closing(&mut a).await;
    wait_state(&mut u, vm, VmState::Stopped).await;
    match ok(&mut u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => {
            assert_eq!(sessions[0].termination, Some(TerminationCause::UiClosed));
        }
        r => panic!("{r:?}"),
    }
    svc.handle.shutdown().await;
}

/// Quotas of the grant (screenshots, operations, input events) and the
/// byte cap of a screenshot.
#[tokio::test(flavor = "multi_thread")]
async fn quotas_and_screenshot_byte_cap_are_enforced() {
    let svc = start_svc(
        "f4-quota",
        Opts {
            png_bytes: 60_000,
            ..Opts::default()
        },
    )
    .await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let run = ui_start(&mut u, vm).await;
    let mut a = agent_client(&svc, &mut u).await;
    let mut ap = approval(vm);
    ap.give_input = true;
    ap.quotas = Some(Quotas {
        max_operations: 6,
        max_input_events: 10,
        max_screenshots: 2,
        max_screenshot_bytes: 50_000,
        max_queued_operations: 4,
    });
    let (auth, info) = session(&mut u, &mut a, ap).await;
    assert!(info.input_owner);
    let shot = |id| AgentCall::Screenshot {
        operation_id: id,
        vm_id: vm,
        expected_run_id: run,
    };
    // 60 000 bytes over the cap of 50 000.
    let e = call_err(&mut a, &auth, shot(op_id())).await;
    assert_eq!(e.code, ErrorCode::ResourceLimit);
    call_err(&mut a, &auth, shot(op_id())).await;
    // Screenshot quota (2) exhausted.
    let e = call_err(&mut a, &auth, shot(op_id())).await;
    assert_eq!(
        (e.code, e.message.as_str()),
        (ErrorCode::ResourceLimit, "screenshot quota exhausted")
    );
    // Input events: 8 of 10.
    call_ok(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 8),
        },
    )
    .await;
    let e = call_err(
        &mut a,
        &auth,
        AgentCall::Submit {
            request: input_req(vm, run, 4),
        },
    )
    .await;
    assert_eq!(e.message, "input quota exhausted");
    // Operations: 3 used (2 screenshots, 1 input); 3 resets, then exhausted.
    let reset = || AgentCall::Submit {
        request: VmRequest {
            operation_id: op_id(),
            vm_id: vm,
            expected_run_id: Some(run),
            action: VmAction::Reset,
        },
    };
    for _ in 0..3 {
        call_ok(&mut a, &auth, reset()).await;
    }
    let e = call_err(&mut a, &auth, reset()).await;
    assert_eq!(e.message, "operation quota exhausted");
    svc.handle.shutdown().await;
}

/// The local emergency command: endpoint published in the catalog, one verb
/// only, works without the UI connection, revokes every session.
#[tokio::test(flavor = "multi_thread")]
async fn emergency_endpoint_stops_everything() {
    let svc = start_svc("f4-emerg", Opts::default()).await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    ui_start(&mut u, vm).await;
    let mut a = agent_client(&svc, &mut u).await;
    let (_auth, _) = session(&mut u, &mut a, approval(vm)).await;
    let published = lightvm_service::client::read_emergency_endpoint(&svc.dir).unwrap();
    assert_eq!(published, svc.handle.emergency_endpoint());
    assert_ne!(published, svc.handle.endpoint());
    // Anything but the verb is refused (a hello with the UI credential too).
    {
        let mut c = connect(&published).await;
        let m = c.hello(&svc.ui).await.unwrap();
        assert!(
            matches!(
                m,
                ServerMessage::Error {
                    error: ReplyError::Protocol(_),
                    ..
                }
            ),
            "{m:?}"
        );
    }
    assert_eq!(count_calls(&svc, "emergency_stop_all"), 0);
    let t0 = Instant::now();
    let r = lightvm_service::client::emergency_stop(&published)
        .await
        .unwrap();
    let dt = t0.elapsed();
    assert!(r.completed);
    assert_eq!(r.revoked_connections, 1);
    assert_eq!(count_calls(&svc, "emergency_stop_all"), 1);
    expect_closing(&mut a).await;
    wait_state(&mut u, vm, VmState::Stopped).await;
    match ok(&mut u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => {
            assert_eq!(
                sessions[0].termination,
                Some(TerminationCause::EmergencyStop)
            );
        }
        r => panic!("{r:?}"),
    }
    eprintln!("emergency command round trip: {dt:?}");
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    assert!(!dir.join("emergency.endpoint").exists());
}

/// A session never survives a service restart: same catalog, token refused.
#[tokio::test(flavor = "multi_thread")]
async fn restart_invalidates_sessions_and_turns_agent_access_off() {
    let dir = temp_dir("f4-restart");
    let svc = start_svc_in(&dir, &Opts::default()).await.unwrap();
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let mut a = agent_client(&svc, &mut u).await;
    let (auth, _) = session(&mut u, &mut a, approval(vm)).await;
    drop((a, u));
    svc.handle.shutdown().await;
    let svc = {
        let mut last = None;
        let mut out = None;
        for _ in 0..40 {
            match start_svc_in(&dir, &Opts::default()).await {
                Ok(s) => {
                    out = Some(s);
                    break;
                }
                Err(StartError::Lock(LockError::AlreadyRunning { .. })) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => last = Some(e),
            }
        }
        out.unwrap_or_else(|| panic!("restart: {last:?}"))
    };
    assert!(!svc.handle.agent_access());
    let mut u = ui(&svc).await;
    let mut a = agent_client(&svc, &mut u).await;
    let e = call_err(&mut a, &auth, AgentCall::ListVms).await;
    assert_eq!(e.code, ErrorCode::Unauthenticated);
    svc.handle.shutdown().await;
}

/// Request × role: every UI-only request is refused to a pairing
/// connection, every agent-only request to the UI, with `permission_denied`
/// and without effect.
#[tokio::test(flavor = "multi_thread")]
async fn request_role_matrix() {
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum Who {
        Any,
        Ui,
        Pairing,
    }
    fn who(r: &Request) -> Who {
        match r {
            Request::Ping | Request::GetCapabilities => Who::Any,
            Request::RequestSession { .. }
            | Request::AwaitSession { .. }
            | Request::Agent { .. } => Who::Pairing,
            Request::ListVms
            | Request::VmStatus { .. }
            | Request::CreateVm { .. }
            | Request::UpdateVm { .. }
            | Request::SubmitVm { .. }
            | Request::Subscribe
            | Request::ListConnections
            | Request::RevokeConnection { .. }
            | Request::CreatePairingCredential
            | Request::EmergencyStop
            | Request::SetParentLossPolicy { .. }
            | Request::Shutdown
            | Request::ListMedia
            | Request::ImportMedia { .. }
            | Request::DeleteMedia { .. }
            | Request::DeleteVm { .. }
            | Request::OpenConsole { .. }
            | Request::ListBaselines { .. }
            | Request::CreateBaseline { .. }
            | Request::RestoreBaseline { .. }
            | Request::DeleteBaseline { .. }
            | Request::ListSnapshots { .. }
            | Request::TakeSnapshot { .. }
            | Request::RevertSnapshot { .. }
            | Request::DeleteSnapshot { .. }
            | Request::UpdateSnapshot { .. }
            | Request::GetSnapshotThumbnail { .. }
            | Request::PromoteSnapshot { .. }
            | Request::GetAgentAccess
            | Request::SetAgentAccess { .. }
            | Request::ListSessionRequests
            | Request::ApproveSession { .. }
            | Request::DenySession { .. }
            | Request::ListSessions
            | Request::PauseSession { .. }
            | Request::ResumeSession { .. }
            | Request::RevokeSession { .. }
            | Request::SetInputOwner { .. }
            | Request::ListSessionReports
            | Request::GetSessionReport { .. }
            | Request::GetSessionReportEvents { .. }
            | Request::GetReportScreenshotChunk { .. }
            | Request::SetSessionVerdict { .. }
            | Request::VerifySession { .. }
            | Request::ExportSessionReport { .. }
            | Request::DeleteSessionReport { .. }
            | Request::PreviewVmwareImport { .. }
            | Request::ImportVmware { .. }
            | Request::CancelVmwareImport => Who::Ui,
        }
    }
    let svc = start_svc("f4-roles", Opts::default()).await;
    let mut u = ui(&svc).await;
    let vm = sample_vm(&mut u).await;
    let sid = SessionId::from_unix_millis(1_760_000_000_000, [1; 10]);
    let auth = SessionAuth {
        session_id: sid,
        token: lightvm_proto::SessionToken::from_bytes([1; 32]),
    };
    let baseline = lightvm_proto::BaselineId::from_unix_millis(1_760_000_000_000, [2; 10]);
    let snapshot = lightvm_proto::SnapshotId::from_unix_millis(1_760_000_000_000, [4; 10]);
    let label = lightvm_proto::Label::parse("x").unwrap();
    let all = vec![
        Request::Ping,
        Request::GetCapabilities,
        Request::ListVms,
        Request::VmStatus { vm_id: vm },
        Request::CreateVm {
            config: sample_config("m"),
        },
        Request::UpdateVm {
            vm_id: vm,
            expected_revision: 1,
            config: sample_config("m"),
        },
        Request::SubmitVm {
            request: VmRequest {
                operation_id: op_id(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        },
        Request::Subscribe,
        Request::ListConnections,
        Request::RevokeConnection { connection_id: 999 },
        Request::CreatePairingCredential,
        Request::EmergencyStop,
        Request::SetParentLossPolicy {
            policy: lightvm_proto::ipc::ParentLossPolicy::KillVms,
        },
        Request::Shutdown,
        Request::ListMedia,
        Request::ImportMedia {
            source_path: "x".into(),
            media_kind: lightvm_proto::MediaKind::InstallIso,
            declared_origin: "x".into(),
        },
        // Deleting a medium is the user's decision, never an agent's.
        Request::DeleteMedia {
            media_id: lightvm_proto::MediaId::from_unix_millis(1_760_000_000_000, [3; 10]),
        },
        Request::DeleteVm {
            vm_id: vm,
            expected_revision: 1,
        },
        Request::OpenConsole { vm_id: vm },
        Request::ListBaselines { vm_id: vm },
        Request::CreateBaseline {
            vm_id: vm,
            label: label.clone(),
            allow_unclean: false,
        },
        Request::RestoreBaseline {
            vm_id: vm,
            baseline_id: baseline,
        },
        Request::DeleteBaseline {
            vm_id: vm,
            baseline_id: baseline,
        },
        // ADR-0032: user snapshots are UI-only.
        Request::ListSnapshots { vm_id: vm },
        Request::TakeSnapshot {
            vm_id: vm,
            name: label.clone(),
            description: String::new(),
        },
        Request::RevertSnapshot {
            vm_id: vm,
            snapshot_id: snapshot,
        },
        Request::DeleteSnapshot {
            vm_id: vm,
            snapshot_id: snapshot,
        },
        Request::UpdateSnapshot {
            vm_id: vm,
            snapshot_id: snapshot,
            name: label.clone(),
            description: String::new(),
        },
        Request::GetSnapshotThumbnail {
            vm_id: vm,
            snapshot_id: snapshot,
        },
        Request::PromoteSnapshot {
            vm_id: vm,
            snapshot_id: snapshot,
            label: label.clone(),
        },
        Request::RequestSession {
            client_name: "x".into(),
            purpose: "x".into(),
            requested_vms: Vec::new(),
            requested_capabilities: CapabilitySet::new(),
            requested_endpoints: Vec::new(),
        },
        Request::AwaitSession { request_id: 1 },
        Request::Agent {
            session: auth.clone(),
            call: AgentCall::ListVms,
        },
        Request::GetAgentAccess,
        Request::SetAgentAccess { enabled: true },
        Request::ListSessionRequests,
        Request::ApproveSession {
            request_id: 1,
            approval: approval(vm),
        },
        Request::DenySession { request_id: 1 },
        Request::ListSessions,
        Request::PauseSession { session_id: sid },
        Request::ResumeSession { session_id: sid },
        Request::RevokeSession { session_id: sid },
        Request::SetInputOwner {
            vm_id: vm,
            owner: InputOwner::User,
        },
        // F5: session reports are UI-only (an agent never chooses an
        // export destination nor sets a verdict).
        Request::ListSessionReports,
        Request::GetSessionReport { session_id: sid },
        Request::GetSessionReportEvents {
            session_id: sid,
            offset: 0,
            limit: 10,
        },
        Request::GetReportScreenshotChunk {
            session_id: sid,
            shot: 0,
            index: 0,
        },
        Request::SetSessionVerdict {
            session_id: sid,
            outcome: lightvm_proto::TestOutcome::Passed,
            rationale: "x".into(),
        },
        Request::VerifySession {
            session_id: sid,
            verifier: lightvm_proto::report::SessionVerifier::FixtureMarker {
                expected: lightvm_proto::report::FixtureMarker::Present,
            },
        },
        Request::ExportSessionReport {
            session_id: sid,
            format: lightvm_proto::report::ReportFormat::Json,
            destination: "C:/x.json".into(),
            overwrite: true,
            include_screenshots: false,
        },
        Request::DeleteSessionReport { session_id: sid },
        // ADR-0030: a VMware VM is examined and imported for the user only;
        // an agent never names a host file.
        Request::PreviewVmwareImport {
            vmx_path: "C:/vm/x.vmx".into(),
        },
        Request::ImportVmware {
            vmx_path: "C:/vm/x.vmx".into(),
            name: lightvm_proto::VmName::parse("x").unwrap(),
            display: lightvm_proto::DisplayAdapter::Standard,
        },
        Request::CancelVmwareImport,
    ];
    let mut a = agent_client(&svc, &mut u).await;
    let mut checked = 0;
    for r in all {
        let w = who(&r);
        let name = r.name();
        // Below the agent rate limit (20 burst, 50/s).
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Pairing connection.
        if w == Who::Ui {
            let e = err(&mut a, r.clone()).await;
            assert_eq!(e.code, ErrorCode::PermissionDenied, "pairing × {name}");
            checked += 1;
        }
        // UI connection: agent-only requests refused; the others are not
        // executed here when destructive (shutdown, Emergency Stop).
        if w == Who::Pairing {
            let e = err(&mut u, r.clone()).await;
            assert_eq!(e.code, ErrorCode::PermissionDenied, "ui × {name}");
            checked += 1;
        }
    }
    assert!(checked >= 33, "{checked}");
    assert_eq!(count_calls(&svc, "vm_start"), 0, "no effect");
    assert_eq!(count_calls(&svc, "emergency_stop_all"), 0, "no effect");
    svc.handle.shutdown().await;
}

// ----- stop latency with a flooding agent -------------------------------------

/// Raw agent connection (frames written without waiting for replies).
struct RawAgent {
    rd: tokio::io::ReadHalf<lightvm_service::transport::BoxedStream>,
    wr: tokio::io::WriteHalf<lightvm_service::transport::BoxedStream>,
}

/// Agent connection with a session, as raw halves for flooding.
async fn raw_agent(svc: &Svc, u: &mut Client, a: SessionApproval) -> (RawAgent, SessionAuth) {
    let mut agent = agent_client(svc, u).await;
    let (auth, _) = session(u, &mut agent, a).await;
    let (rd, wr) = agent.into_parts();
    (RawAgent { rd, wr }, auth)
}

#[derive(Debug, Default, Clone, Copy)]
struct FloodStats {
    sent: u64,
    ok: u64,
    rate_limited: u64,
    refused_after: u64,
    closed_at: Option<Instant>,
    first_refusal_at: Option<Instant>,
}

/// Floods input requests (pipelined) until the connection closes or `stop`.
/// Records when the first refusal of the gate (unauthenticated / paused)
/// and the closing arrive.
fn flood(
    agent: RawAgent,
    auth: SessionAuth,
    vm: VmId,
    run: RunId,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<FloodStats>,
) {
    let RawAgent { mut rd, mut wr } = agent;
    let writer = tokio::spawn(async move {
        let mut id = 100_000u64;
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            id += 1;
            let m = ClientMessage::Request {
                request_id: id,
                body: Request::Agent {
                    session: auth.clone(),
                    call: AgentCall::Submit {
                        request: input_req(vm, run, 2),
                    },
                },
            };
            if frame::write_frame(&mut wr, &m, u32::MAX).await.is_err() {
                break;
            }
            // Pipelined: a short pause keeps the stream above the rate limit
            // without spinning.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let reader = tokio::spawn(async move {
        let mut st = FloodStats::default();
        loop {
            match frame::read_frame::<_, ServerMessage>(&mut rd, frame::DEFAULT_MAX_FRAME).await {
                Ok(ServerMessage::Response { .. }) => st.ok += 1,
                Ok(ServerMessage::Error {
                    error: ReplyError::Operational(e),
                    ..
                }) => {
                    if e.code == ErrorCode::ResourceLimit {
                        st.rate_limited += 1;
                    } else if matches!(
                        e.code,
                        ErrorCode::Unauthenticated | ErrorCode::PermissionDenied
                    ) {
                        st.refused_after += 1;
                        st.first_refusal_at.get_or_insert_with(Instant::now);
                    }
                }
                Ok(ServerMessage::Closing { .. }) => {
                    st.closed_at = Some(Instant::now());
                }
                Ok(_) => {}
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

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

fn summary(name: &str, v: &mut [f64]) -> String {
    let n = v.len();
    let p50 = pct(v, 0.5);
    let p95 = pct(v, 0.95);
    let max = v.iter().copied().fold(0.0, f64::max);
    format!("{name}: n={n} p50={p50:.2} ms p95={p95:.2} ms max={max:.2} ms")
}

/// Agent operations of `session` forwarded after the instant `t` (UTC,
/// millisecond resolution: one at the boundary may be counted).
fn forwarded_after(ev: &[AuditEvent], session: SessionId, t: lightvm_proto::UtcTimestamp) -> usize {
    ev.iter()
        .filter(|e| {
            e.session_id == Some(session) && e.outcome == AuditOutcome::Forwarded && e.at > t
        })
        .count()
}

/// Forwarded agent operations of `session` after its termination in the
/// audit (must be zero).
fn forwarded_after_termination(ev: &[AuditEvent], session: SessionId) -> usize {
    let end = ev
        .iter()
        .find(|e| e.action == AuditAction::SessionTerminate && e.session_id == Some(session))
        .map(|e| e.seq)
        .expect("session terminated in the audit");
    ev.iter()
        .filter(|e| {
            e.seq > end && e.session_id == Some(session) && e.outcome == AuditOutcome::Forwarded
        })
        .count()
}

fn samples() -> usize {
    std::env::var("LIGHTVM_F4_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

/// Revocation, pause and Emergency Stop with an uncooperative agent flooding
/// input requests at (and above) the rate limit, a slow backend (5 ms per
/// input) keeping the supervisor busy: latency from the UI command to (a)
/// the first refusal / closing seen by the agent, (b) the end of the VM
/// process observed (Emergency Stop); zero agent operations forwarded after
/// the point of revocation.
#[tokio::test(flavor = "multi_thread")]
async fn stop_latency_with_a_flooding_agent() {
    let svc = start_svc(
        "f4-flood",
        Opts {
            input_delay: Some(Duration::from_millis(5)),
            ..Opts::default()
        },
    )
    .await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let n = samples();
    let mut revoke_close = Vec::new();
    let mut revoke_reply = Vec::new();
    let mut pause_refusal = Vec::new();
    let mut es_close = Vec::new();
    let mut es_done = Vec::new();
    let mut flooded = 0u64;
    let mut limited = 0u64;
    let mut inflight_max = 0usize;
    for i in 0..n {
        let kind = i % 3;
        let run = match vm_state(&mut u, vm).await {
            (VmState::Running, Some(r)) => r,
            _ => ui_start(&mut u, vm).await,
        };
        let mut ap = approval(vm);
        ap.give_input = true;
        let (agent, auth) = raw_agent(&svc, &mut u, ap).await;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (writer, reader) = flood(agent, auth.clone(), vm, run, stop.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
        let utc0 = lightvm_proto::UtcTimestamp::now();
        let t0 = Instant::now();
        match kind {
            0 => {
                ok(
                    &mut u,
                    Request::RevokeSession {
                        session_id: auth.session_id,
                    },
                )
                .await;
                revoke_reply.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            1 => {
                ok(
                    &mut u,
                    Request::PauseSession {
                        session_id: auth.session_id,
                    },
                )
                .await;
                // Let the refusals come, then revoke to close.
                tokio::time::sleep(Duration::from_millis(100)).await;
                ok(
                    &mut u,
                    Request::RevokeSession {
                        session_id: auth.session_id,
                    },
                )
                .await;
            }
            _ => {
                let r = match ok(&mut u, Request::EmergencyStop).await {
                    ResponseBody::EmergencyStop(r) => r,
                    r => panic!("{r:?}"),
                };
                assert!(r.completed);
                es_done.push(t0.elapsed().as_secs_f64() * 1e3);
            }
        }
        let st = tokio::time::timeout(T, reader).await.unwrap().unwrap();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = writer.await;
        flooded += st.sent + st.ok + st.rate_limited;
        limited += st.rate_limited;
        let closed = st.closed_at.expect("agent connection closed");
        match kind {
            0 => revoke_close.push(closed.duration_since(t0).as_secs_f64() * 1e3),
            1 => pause_refusal.push(
                st.first_refusal_at
                    .expect("paused mutations refused")
                    .duration_since(t0)
                    .as_secs_f64()
                    * 1e3,
            ),
            _ => es_close.push(closed.duration_since(t0).as_secs_f64() * 1e3),
        }
        // Zero agent operations forwarded after the termination; at most
        // the one already inside the backend after the command.
        let ev = audit(&svc);
        assert_eq!(
            forwarded_after_termination(&ev, auth.session_id),
            0,
            "sample {i}"
        );
        let after = forwarded_after(&ev, auth.session_id, utc0);
        assert!(
            after <= 1,
            "sample {i}: {after} forwarded after the command"
        );
        inflight_max = inflight_max.max(after);
    }
    assert!(limited > 0, "the flood must hit the rate limit");
    eprintln!(
        "F4 stop latency, fake backend (input 5 ms), agent flooding: {flooded} replies, {limited} rate-limited, at most {inflight_max} operation forwarded after a command (already in the backend)"
    );
    for (name, v) in [
        ("revoke -> agent connection closed", &mut revoke_close),
        (
            "revoke -> UI reply (core revocation recorded)",
            &mut revoke_reply,
        ),
        ("pause -> first paused refusal", &mut pause_refusal),
        ("emergency stop -> agent connection closed", &mut es_close),
        (
            "emergency stop -> UI reply (process end observed)",
            &mut es_done,
        ),
    ] {
        if !v.is_empty() {
            eprintln!("{}", summary(name, v));
        }
    }
    svc.handle.shutdown().await;
}

/// Revocation and Emergency Stop while the supervisor thread is **stuck**
/// (backend blocked) and the supervisor queue is **full**: the gate closes
/// and the agent connection is closed at once; nothing queued is forwarded
/// once the supervisor resumes.
#[tokio::test(flavor = "multi_thread")]
async fn revoke_and_emergency_stop_with_a_stuck_supervisor_and_a_full_queue() {
    let svc = start_svc(
        "f4-stuck",
        Opts {
            queue_capacity: 4,
            ..Opts::default()
        },
    )
    .await;
    let mut u = ui(&svc).await;
    set_access(&mut u, true).await;
    let vm = sample_vm(&mut u).await;
    let run = ui_start(&mut u, vm).await;
    let mut ap = approval(vm);
    ap.give_input = true;
    let mut revoke = Vec::new();
    let mut es = Vec::new();
    for round in 0..2 {
        let (agent, auth) = raw_agent(&svc, &mut u, ap.clone()).await;
        // Pairing connections that will fill the queue (created while the
        // supervisor still runs).
        let mut fillers = Vec::new();
        for _ in 0..4 {
            fillers.push(agent_client(&svc, &mut u).await);
        }
        let inputs_before = count_calls(&svc, "vm_input");
        // Stuck supervisor: the next engine call (the agent's first input)
        // blocks inside it.
        svc.gate.close();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (writer, reader) = flood(agent, auth.clone(), vm, run, stop.clone());
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Full queue: each filler queues a request behind the stuck one;
        // the last ones find no slot.
        let mut full = 0;
        for c in &mut fillers {
            match tokio::time::timeout(Duration::from_millis(300), c.request(Request::Ping)).await {
                Ok(Ok(ServerMessage::Error {
                    error: ReplyError::Operational(e),
                    ..
                })) if e.code == ErrorCode::ResourceLimit => full += 1,
                Ok(other) => panic!("{other:?}"),
                Err(_) => {} // queued behind the stuck supervisor
            }
        }
        assert!(full >= 1, "supervisor queue full");
        let t0 = Instant::now();
        let ui_task = if round == 0 {
            // The UI reply needs the supervisor: awaited after the gate opens.
            let ep = svc.handle.endpoint().to_owned();
            let cred = svc.ui.clone();
            let sid = auth.session_id;
            tokio::spawn(async move {
                let mut c = Client::connect(&ep).await.unwrap();
                c.hello(&cred).await.unwrap();
                c.request(Request::RevokeSession { session_id: sid })
                    .await
                    .unwrap()
            })
        } else {
            let ep = svc.handle.emergency_endpoint().to_owned();
            tokio::spawn(async move {
                let r = lightvm_service::client::emergency_stop(&ep).await.unwrap();
                ServerMessage::Response {
                    request_id: 0,
                    result: ResponseBody::EmergencyStop(r),
                }
            })
        };
        let st = tokio::time::timeout(T, reader).await.unwrap().unwrap();
        let closed = st.closed_at.unwrap().duration_since(t0).as_secs_f64() * 1e3;
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = writer.await;
        if round == 0 {
            revoke.push(closed);
        } else {
            es.push(closed);
        }
        // The supervisor resumes: nothing of the agent is forwarded (the
        // request stuck inside it meets the core's checks after the gate).
        svc.gate.open();
        let m = tokio::time::timeout(T, ui_task).await.unwrap().unwrap();
        assert!(matches!(m, ServerMessage::Response { .. }), "{m:?}");
        drop(fillers);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ev = audit(&svc);
        assert_eq!(forwarded_after_termination(&ev, auth.session_id), 0);
        assert_eq!(
            count_calls(&svc, "vm_input"),
            inputs_before,
            "no agent input forwarded"
        );
        eprintln!(
            "stuck supervisor, full queue ({}): agent connection closed after {closed:.2} ms",
            if round == 0 {
                "revoke"
            } else {
                "emergency stop"
            }
        );
    }
    eprintln!("{}", summary("stuck: revoke -> agent closed", &mut revoke));
    eprintln!(
        "{}",
        summary("stuck: emergency stop -> agent closed", &mut es)
    );
    svc.handle.shutdown().await;
}
