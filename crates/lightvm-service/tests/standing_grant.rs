//! ADR-0039 — standing grant (unattended agent access to one VM) through the
//! real IPC: real core and SQLite catalog (the grant survives a restart),
//! fake VM backend and image tool (no QEMU). The tests follow §5 of the
//! specification: persistence and expiry across restarts, automatic
//! approval and denial without queueing, ordinary pairing unaffected, end
//! of session without stopping the VM (D1), revocation, Emergency Stop,
//! one live grant, deletion of the VM, suspension on a configuration
//! change, baselines across the sessions of a grant, the 7-day bound.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    AgentCall, ClientMessage, CloseReason, Request, ResponseBody, ServerMessage, ServiceEvent,
    SessionApproval, SessionAuth,
};
use lightvm_proto::{
    Actor, AuditAction, AuditEvent, AuditOutcome, Capability, CapabilitySet, ErrorCode,
    OperationId, PublicError, ReplyError, StandingGrant, StandingGrantChange, StandingGrantSpec,
    StandingGrantState, StandingRevokeCause, TerminationCause, VmAction, VmId, VmRequest, VmState,
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

const T: Duration = Duration::from_secs(20);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    dir: PathBuf,
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

fn audit_path(dir: &Path) -> PathBuf {
    dir.join("audit").join("audit.jsonl")
}

async fn start_in(dir: &Path) -> Result<Svc, StartError> {
    let store = SqliteStore::open(dir).unwrap();
    let audit = FileAuditSink::open(&audit_path(dir)).unwrap();
    let calls = Calls::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        image_tool: Some(Arc::new(FakeImageTool {
            calls: calls.clone(),
            ..FakeImageTool::default()
        })),
        ..FakeVmBackend::default()
    };
    let inspector = ManagedMediaInspector::new(store.root());
    let mut host = host_capacity();
    host.baseline_available = true;
    let engine = CoreEngine::new(store, audit, backend, inspector, core_config(host)).unwrap();
    let host = FakeHost {
        calls: calls.clone(),
    };
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.display_dir = Some(short_dir("lvs"));
    // Short grants for the expiry tests.
    cfg.standing_min_duration = Duration::from_secs(1);
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(host)).await?;
    Ok(Svc {
        handle,
        ui,
        calls,
        dir: dir.to_path_buf(),
    })
}

async fn start_svc(name: &str) -> Svc {
    start_in(&temp_dir(name)).await.expect("start")
}

/// Restart on the same catalog (the previous instance releases its lock).
async fn restart(svc: Svc) -> Svc {
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    let mut last = None;
    for _ in 0..60 {
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

async fn connect(svc: &Svc, cred: &Credential) -> Client {
    let mut c = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .unwrap()
        .unwrap();
    match c.hello(cred).await.unwrap() {
        ServerMessage::HelloOk { .. } => c,
        other => panic!("{other:?}"),
    }
}

/// `true` if the credential is refused at the handshake.
async fn refused(svc: &Svc, cred: &Credential) -> bool {
    let mut c = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .unwrap()
        .unwrap();
    match c.hello(cred).await {
        Ok(ServerMessage::HelloOk { .. }) => false,
        Ok(ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        }) => e.code == ErrorCode::Unauthenticated,
        _ => true,
    }
}

async fn ui(svc: &Svc) -> Client {
    connect(svc, &svc.ui).await
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

async fn err(c: &mut Client, body: Request) -> PublicError {
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

fn reason(e: &PublicError) -> &str {
    e.details
        .as_ref()
        .and_then(|d| d.get("reason"))
        .map_or("", String::as_str)
}

fn caps(c: &[Capability]) -> CapabilitySet {
    c.iter().copied().collect()
}

const GRANT_CAPS: [Capability; 7] = [
    Capability::VmList,
    Capability::VmStatus,
    Capability::VmStart,
    Capability::VmStop,
    Capability::VmScreenshot,
    Capability::VmSnapshot,
    Capability::VmRestore,
];

fn spec(vm: VmId, duration_s: u32) -> StandingGrantSpec {
    StandingGrantSpec {
        vm_id: vm,
        capabilities: caps(&GRANT_CAPS),
        tools: Default::default(),
        network_endpoints: Vec::new(),
        quotas: None,
        lease_ms: None,
        give_input: false,
        duration_s,
        client_name: None,
    }
}

/// New VM with its disk files written as the QEMU backend would.
async fn vm_with_disk(c: &mut Client, svc: &Svc, name: &str) -> VmId {
    let vm = match ok(
        c,
        Request::CreateVm {
            config: sample_config(name),
        },
    )
    .await
    {
        ResponseBody::VmCreated { vm } => vm,
        other => panic!("{other:?}"),
    };
    let d = svc.dir.join("vms").join(vm.id.to_string());
    std::fs::create_dir_all(d.join("firmware")).unwrap();
    std::fs::write(d.join("primary.qcow2"), fake_qcow2(None, b"installed")).unwrap();
    std::fs::write(d.join("firmware").join("primary.vars.fd"), b"VARS").unwrap();
    vm.id
}

async fn create_grant(u: &mut Client, s: StandingGrantSpec) -> (StandingGrant, Credential) {
    match ok(u, Request::CreateStandingGrant { spec: s }).await {
        ResponseBody::StandingGrantCreated { grant, credential } => {
            (grant, Credential::from_hex(&credential).unwrap())
        }
        other => panic!("{other:?}"),
    }
}

async fn live_grant(u: &mut Client) -> Option<StandingGrant> {
    match ok(u, Request::GetStandingGrant).await {
        ResponseBody::StandingGrant { grant, .. } => grant,
        other => panic!("{other:?}"),
    }
}

fn request(vms: Vec<VmId>, c: CapabilitySet, endpoints: Vec<String>) -> Request {
    Request::RequestSession {
        client_name: "unattended-agent".into(),
        purpose: "ADR-0039 test".into(),
        requested_vms: vms,
        requested_capabilities: c,
        requested_endpoints: endpoints,
    }
}

fn all_caps() -> CapabilitySet {
    Capability::ALL.into_iter().collect()
}

/// Request with the standing credential and the grant delivered at once
/// (no UI involved).
async fn auto_session(agent: &mut Client) -> (SessionAuth, lightvm_proto::Grant) {
    let request_id = match ok(agent, request(Vec::new(), all_caps(), Vec::new())).await {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    match ok(agent, Request::AwaitSession { request_id }).await {
        ResponseBody::SessionGranted {
            session_id,
            token,
            grant,
            ..
        } => (SessionAuth { session_id, token }, grant),
        other => panic!("{other:?}"),
    }
}

async fn agent_ok(agent: &mut Client, auth: &SessionAuth, call: AgentCall) -> ResponseBody {
    ok(
        agent,
        Request::Agent {
            session: auth.clone(),
            call,
        },
    )
    .await
}

async fn vm_state(u: &mut Client, vm: VmId) -> VmState {
    match ok(u, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => vm.state,
        other => panic!("{other:?}"),
    }
}

async fn wait_state(u: &mut Client, vm: VmId, want: VmState) {
    let deadline = Instant::now() + T;
    loop {
        let s = vm_state(u, vm).await;
        if s == want {
            return;
        }
        assert!(Instant::now() < deadline, "VM not {want:?} (now {s:?})");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn ui_submit(u: &mut Client, vm: VmId, action: VmAction) {
    let r = ok(
        u,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: None,
                action,
            },
        },
    )
    .await;
    assert!(matches!(r, ResponseBody::Operation { .. }), "{r:?}");
}

async fn ui_start(u: &mut Client, vm: VmId) {
    ui_submit(u, vm, VmAction::Start).await;
    wait_state(u, vm, VmState::Running).await;
}

async fn sessions(u: &mut Client) -> Vec<lightvm_proto::ipc::SessionInfo> {
    match ok(u, Request::ListSessions).await {
        ResponseBody::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    }
}

async fn wait_terminated(u: &mut Client, sid: lightvm_proto::SessionId) -> TerminationCause {
    let deadline = Instant::now() + T;
    loop {
        if let Some(s) = sessions(u).await.into_iter().find(|s| s.session_id == sid)
            && let Some(c) = s.termination
        {
            return c;
        }
        assert!(Instant::now() < deadline, "session still live");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn pending_requests(u: &mut Client) -> usize {
    match ok(u, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => requests.len(),
        other => panic!("{other:?}"),
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

fn read_audit(dir: &Path) -> Vec<AuditEvent> {
    std::fs::read_to_string(audit_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

async fn wait_closed(agent: &mut Client) {
    let deadline = Instant::now() + T;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, agent.recv()).await {
            Ok(Ok(ServerMessage::Closing { reason })) => {
                assert_eq!(reason, CloseReason::Revoked);
                return;
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return,
            Err(_) => panic!("connection not closed"),
        }
    }
}

async fn wait_standing_event(
    ev: &mut Client,
    f: impl Fn(&StandingGrant, &StandingGrantChange) -> bool,
) {
    let deadline = Instant::now() + T;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, ev.next_event()).await {
            Ok(Ok(Ok(ServiceEvent::StandingGrant { grant, change }))) if f(&grant, &change) => {
                return;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("{e}"),
            Err(_) => panic!("standing grant event not received"),
        }
    }
}

async fn subscribed(svc: &Svc) -> Client {
    let mut ev = ui(svc).await;
    assert_eq!(
        ok(&mut ev, Request::Subscribe).await,
        ResponseBody::Subscribed
    );
    ev
}

// ----- 1. persistence and expiry across restarts --------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn grant_survives_a_restart_and_an_expired_one_does_not() {
    let svc = start_svc("sg-restart").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let (grant, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    assert_eq!(grant.state, StandingGrantState::Active);
    drop(u);
    let svc = restart(svc).await;
    let mut u = ui(&svc).await;
    // The agent access switch is off after the restart, as always.
    assert!(!svc.handle.agent_access());
    let after = live_grant(&mut u).await.expect("grant kept");
    assert_eq!(after.grant_id, grant.grant_id);
    assert_eq!(after.state, StandingGrantState::Active);
    // Its credential (hash only in the catalog) still works, and a request
    // is approved without the switch and without the UI.
    let mut a = connect(&svc, &cred).await;
    let (auth, g) = auto_session(&mut a).await;
    assert!(g.vm_allowlist.contains(&vm));
    assert_eq!(g.capabilities, caps(&GRANT_CAPS));
    assert!(matches!(
        agent_ok(&mut a, &auth, AgentCall::ListVms).await,
        ResponseBody::AgentVms { .. }
    ));
    // The catalog never holds the credential.
    let db = std::fs::read(svc.dir.join(vm_storage::DB_FILE)).unwrap();
    let hex = cred.to_hex();
    assert!(
        !db.windows(hex.len()).any(|w| w == hex.as_bytes()),
        "credential in the catalog"
    );
    // Revoke this one; a short one expires while the service is down.
    assert!(matches!(
        ok(
            &mut u,
            Request::RevokeStandingGrant {
                grant_id: grant.grant_id
            }
        )
        .await,
        ResponseBody::StandingGrantRevoked { .. }
    ));
    let (short, short_cred) = create_grant(&mut u, spec(vm, 2)).await;
    drop((a, u));
    let dir = svc.dir.clone();
    svc.handle.shutdown().await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let svc = {
        let mut out = None;
        for _ in 0..60 {
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
    let mut u = ui(&svc).await;
    assert!(live_grant(&mut u).await.is_none(), "expired grant not live");
    assert!(refused(&svc, &short_cred).await);
    assert!(refused(&svc, &cred).await, "revoked grant");
    let audit = read_audit(&svc.dir);
    assert!(
        audit
            .iter()
            .any(|e| e.action == AuditAction::StandingGrantExpire),
        "expiry audited"
    );
    let _ = short;
    svc.handle.shutdown().await;
}

// ----- 2. automatic approval and denial, never queued -----------------------------

#[tokio::test(flavor = "multi_thread")]
async fn conforming_requests_are_approved_others_denied_and_never_queued() {
    let svc = start_svc("sg-conform").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let other = vm_with_disk(&mut u, &svc, "sg-other").await;
    let (grant, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    let mut a = connect(&svc, &cred).await;
    // Another VM: denied, nothing pending for the user.
    let e = err(&mut a, request(vec![other], all_caps(), Vec::new())).await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(reason(&e), "standing_vm_mismatch");
    // An endpoint outside the grant: denied.
    let e = err(
        &mut a,
        request(Vec::new(), all_caps(), vec!["db.lan".into()]),
    )
    .await;
    assert_eq!(reason(&e), "standing_endpoint_not_granted");
    // Only capabilities outside the grant: denied.
    let e = err(
        &mut a,
        request(Vec::new(), caps(&[Capability::VmInput]), Vec::new()),
    )
    .await;
    assert_eq!(reason(&e), "standing_no_capability");
    assert_eq!(pending_requests(&mut u).await, 0, "nothing queued");
    // More capabilities than the grant: only the grant's are given.
    let (auth, g) = auto_session(&mut a).await;
    assert_eq!(g.capabilities, caps(&GRANT_CAPS));
    assert!(!g.capabilities.contains(&Capability::VmInput));
    assert_eq!(pending_requests(&mut u).await, 0);
    // The UI sees the session as approved by the grant.
    let s = sessions(&mut u)
        .await
        .into_iter()
        .find(|s| s.session_id == auth.session_id)
        .unwrap();
    assert_eq!(s.standing_grant_id, Some(grant.grant_id));
    // A second request while a session is live: denied.
    let mut b = connect(&svc, &cred).await;
    let e = err(&mut b, request(Vec::new(), all_caps(), Vec::new())).await;
    assert_eq!(reason(&e), "session_live");
    // Counters of the grant.
    let now = live_grant(&mut u).await.unwrap();
    assert_eq!(now.sessions_approved, 1);
    assert!(now.last_session_at.is_some());
    // The audit tells an automatic approval from a click.
    let audit = read_audit(&svc.dir);
    let auto = Actor::StandingGrant {
        grant_id: grant.grant_id,
    };
    assert!(audit.iter().any(|e| e.action == AuditAction::SessionCreate
        && e.outcome == AuditOutcome::Succeeded
        && e.actor == auto));
    assert!(audit.iter().any(|e| e.action == AuditAction::SessionRequest
        && e.outcome == AuditOutcome::Succeeded
        && e.actor == auto));
    assert!(
        !audit
            .iter()
            .any(|e| e.action == AuditAction::SessionCreate && e.actor == Actor::User),
        "no click"
    );
    svc.handle.shutdown().await;
}

// ----- 3. an ordinary pairing credential is unaffected ------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_pairing_gets_no_automatic_approval() {
    let svc = start_svc("sg-ordinary").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let _ = create_grant(&mut u, spec(vm, 3600)).await;
    let cred = match ok(&mut u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    };
    let mut a = connect(&svc, &cred).await;
    // Switch off: refused as before.
    let e = err(&mut a, request(vec![vm], all_caps(), Vec::new())).await;
    assert_eq!(reason(&e), "agent_access_disabled");
    // Switch on: the request waits for the user, nothing is approved.
    assert_eq!(
        ok(&mut u, Request::SetAgentAccess { enabled: true }).await,
        ResponseBody::AgentAccess { enabled: true }
    );
    let request_id = match ok(&mut a, request(vec![vm], all_caps(), Vec::new())).await {
        ResponseBody::SessionRequested {
            request_id,
            verification_code,
            ..
        } => {
            assert_ne!(verification_code, "standing");
            request_id
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(pending_requests(&mut u).await, 1);
    assert!(
        sessions(&mut u).await.is_empty(),
        "no session without a click"
    );
    // The user may still decide it.
    assert!(matches!(
        ok(&mut u, Request::DenySession { request_id }).await,
        ResponseBody::SessionDenied { .. }
    ));
    svc.handle.shutdown().await;
}

// ----- 4. D1: end of a standing session leaves the VM running -------------------------

#[tokio::test(flavor = "multi_thread")]
async fn client_loss_lease_and_grant_expiry_leave_the_vm_running() {
    let svc = start_svc("sg-d1").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    ui_start(&mut u, vm).await;
    let mut s = spec(vm, 6);
    s.lease_ms = Some(1000);
    let (grant, cred) = create_grant(&mut u, s).await;

    // Client lost.
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    drop(a);
    assert_eq!(
        wait_terminated(&mut u, auth.session_id).await,
        TerminationCause::ClientLost
    );
    assert_eq!(vm_state(&mut u, vm).await, VmState::Running);

    // Lease expired (no renewal: lease 1000 ms).
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    assert_eq!(
        wait_terminated(&mut u, auth.session_id).await,
        TerminationCause::LeaseExpired
    );
    wait_closed(&mut a).await;
    assert_eq!(vm_state(&mut u, vm).await, VmState::Running);

    // Grant (and session) expiry, renewing the lease meanwhile.
    let mut a = connect(&svc, &cred).await;
    let (auth, g) = auto_session(&mut a).await;
    assert!(g.expires_at <= grant.expires_at, "session within the grant");
    let deadline = Instant::now() + Duration::from_secs(15);
    let cause = loop {
        let r = tokio::time::timeout(
            T,
            a.request(Request::Agent {
                session: auth.clone(),
                call: AgentCall::RenewLease,
            }),
        )
        .await
        .unwrap();
        if !matches!(r, Ok(ServerMessage::Response { .. })) {
            break wait_terminated(&mut u, auth.session_id).await;
        }
        assert!(Instant::now() < deadline, "grant not expired");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!(cause, TerminationCause::GrantExpired);
    assert_eq!(vm_state(&mut u, vm).await, VmState::Running);
    assert!(live_grant(&mut u).await.is_none(), "grant expired");
    assert!(refused(&svc, &cred).await, "credential expired");
    assert_eq!(count_calls(&svc, "emergency_stop_all"), 0);

    // Same causes on a session approved by the user: the VM is stopped
    // (ROADMAP 7.3, unchanged).
    assert_eq!(
        ok(&mut u, Request::SetAgentAccess { enabled: true }).await,
        ResponseBody::AgentAccess { enabled: true }
    );
    let pc = match ok(&mut u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    };
    let mut m = connect(&svc, &pc).await;
    let request_id = match ok(&mut m, request(vec![vm], all_caps(), Vec::new())).await {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    m.send(&ClientMessage::Request {
        request_id: 9_000,
        body: Request::AwaitSession { request_id },
    })
    .await
    .unwrap();
    let info = match ok(
        &mut u,
        Request::ApproveSession {
            request_id,
            approval: SessionApproval {
                tools: Default::default(),
                vm_id: vm,
                capabilities: caps(&[Capability::VmList, Capability::VmStatus]),
                duration_s: 3600,
                quotas: None,
                lease_ms: None,
                give_input: false,
                network_endpoints: Vec::new(),
            },
        },
    )
    .await
    {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    };
    assert_eq!(info.standing_grant_id, None);
    drop(m);
    assert_eq!(
        wait_terminated(&mut u, info.session_id).await,
        TerminationCause::ClientLost
    );
    wait_state(&mut u, vm, VmState::Stopped).await;
    svc.handle.shutdown().await;
}

// ----- 5. revocation --------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn revocation_ends_the_session_and_the_credential_on_open_connections() {
    let svc = start_svc("sg-revoke").await;
    let mut u = ui(&svc).await;
    let mut ev = subscribed(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    ui_start(&mut u, vm).await;
    let (grant, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    // A second connection with the same credential, without a session.
    let mut idle = connect(&svc, &cred).await;
    assert!(matches!(
        ok(
            &mut u,
            Request::RevokeStandingGrant {
                grant_id: grant.grant_id
            }
        )
        .await,
        ResponseBody::StandingGrantRevoked { .. }
    ));
    wait_standing_event(&mut ev, |g, c| {
        g.grant_id == grant.grant_id
            && *c == StandingGrantChange::Revoked
            && matches!(
                g.state,
                StandingGrantState::Revoked {
                    cause: StandingRevokeCause::User,
                    ..
                }
            )
    })
    .await;
    assert_eq!(
        wait_terminated(&mut u, auth.session_id).await,
        TerminationCause::Revoked
    );
    wait_closed(&mut a).await;
    wait_closed(&mut idle).await;
    assert!(refused(&svc, &cred).await);
    assert!(live_grant(&mut u).await.is_none());
    // Revocation does not stop the VM.
    assert_eq!(vm_state(&mut u, vm).await, VmState::Running);
    // Revoking again: not found.
    let e = err(
        &mut u,
        Request::RevokeStandingGrant {
            grant_id: grant.grant_id,
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::NotFound);
    svc.handle.shutdown().await;
}

// ----- 6. Emergency Stop ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn emergency_stop_revokes_the_grant_and_stops_the_vms() {
    let svc = start_svc("sg-emergency").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    ui_start(&mut u, vm).await;
    let (_, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    assert!(matches!(
        ok(&mut u, Request::EmergencyStop).await,
        ResponseBody::EmergencyStop(_)
    ));
    assert_eq!(count_calls(&svc, "emergency_stop_all"), 1);
    wait_state(&mut u, vm, VmState::Stopped).await;
    assert_eq!(
        wait_terminated(&mut u, auth.session_id).await,
        TerminationCause::EmergencyStop
    );
    assert!(live_grant(&mut u).await.is_none());
    assert!(refused(&svc, &cred).await);
    // Revoked in the catalog too: not back after a restart.
    drop((a, u));
    let svc = restart(svc).await;
    let mut u = ui(&svc).await;
    assert!(live_grant(&mut u).await.is_none());
    assert!(refused(&svc, &cred).await);
    svc.handle.shutdown().await;
}

// ----- 8. one live grant; deleting the VM revokes it ---------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn one_live_grant_and_vm_deletion_revokes_it() {
    let svc = start_svc("sg-one").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let other = vm_with_disk(&mut u, &svc, "sg-other").await;
    let (grant, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    let e = err(
        &mut u,
        Request::CreateStandingGrant {
            spec: spec(other, 3600),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::StateConflict);
    // Unknown VM.
    let _ = ok(
        &mut u,
        Request::RevokeStandingGrant {
            grant_id: grant.grant_id,
        },
    )
    .await;
    let e = err(
        &mut u,
        Request::CreateStandingGrant {
            spec: spec(VmId::new_v7(), 3600),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::NotFound);
    let (grant, cred2) = create_grant(&mut u, spec(vm, 3600)).await;
    assert!(refused(&svc, &cred).await, "the first one stays revoked");
    let rev = match ok(&mut u, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => vm.config_revision,
        other => panic!("{other:?}"),
    };
    let mut ev = subscribed(&svc).await;
    assert!(matches!(
        ok(
            &mut u,
            Request::DeleteVm {
                vm_id: vm,
                expected_revision: rev
            }
        )
        .await,
        ResponseBody::VmDeleted { .. }
    ));
    wait_standing_event(&mut ev, |g, c| {
        g.grant_id == grant.grant_id
            && *c == StandingGrantChange::Revoked
            && matches!(
                g.state,
                StandingGrantState::Revoked {
                    cause: StandingRevokeCause::VmDeleted,
                    ..
                }
            )
    })
    .await;
    assert!(live_grant(&mut u).await.is_none());
    assert!(refused(&svc, &cred2).await);
    drop((u, ev));
    let svc = restart(svc).await;
    let mut u = ui(&svc).await;
    assert!(live_grant(&mut u).await.is_none(), "revoked in the catalog");
    svc.handle.shutdown().await;
}

// ----- 9. a configuration change suspends the grant --------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn configuration_change_suspends_until_confirmed() {
    let svc = start_svc("sg-suspend").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let (grant, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    let (rev, mut config) = match ok(&mut u, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => (vm.config_revision, vm.config),
        other => panic!("{other:?}"),
    };
    config.memory_mib = 1024;
    assert!(matches!(
        ok(
            &mut u,
            Request::UpdateVm {
                vm_id: vm,
                expected_revision: rev,
                config
            }
        )
        .await,
        ResponseBody::VmUpdated { .. }
    ));
    let g = live_grant(&mut u).await.unwrap();
    assert_eq!(g.state, StandingGrantState::Suspended);
    let mut a = connect(&svc, &cred).await;
    let e = err(&mut a, request(Vec::new(), all_caps(), Vec::new())).await;
    assert_eq!(reason(&e), "standing_grant_suspended");
    assert_eq!(pending_requests(&mut u).await, 0);
    // Still suspended after a restart.
    drop((a, u));
    let svc = restart(svc).await;
    let mut u = ui(&svc).await;
    assert_eq!(
        live_grant(&mut u).await.unwrap().state,
        StandingGrantState::Suspended
    );
    match ok(
        &mut u,
        Request::ConfirmStandingGrant {
            grant_id: grant.grant_id,
        },
    )
    .await
    {
        ResponseBody::StandingGrant { grant: Some(g), .. } => {
            assert_eq!(g.state, StandingGrantState::Active);
            assert_ne!(g.vm_config_revision, grant.vm_config_revision);
        }
        other => panic!("{other:?}"),
    }
    let mut a = connect(&svc, &cred).await;
    let _ = auto_session(&mut a).await;
    let audit = read_audit(&svc.dir);
    assert!(
        audit
            .iter()
            .any(|e| e.action == AuditAction::StandingGrantSuspend)
    );
    assert!(
        audit
            .iter()
            .any(|e| e.action == AuditAction::StandingGrantConfirm)
    );
    svc.handle.shutdown().await;
}

// ----- 10. baselines across the sessions of a grant -----------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn baselines_of_a_grant_are_restorable_by_its_later_sessions_only() {
    let svc = start_svc("sg-baselines").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let (_, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    // Session 1: checkpoint, then end.
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    let first = match agent_ok(
        &mut a,
        &auth,
        AgentCall::Snapshot {
            operation_id: OperationId::new_v7(),
            vm_id: vm,
            label: "checkpoint one".into(),
        },
    )
    .await
    {
        ResponseBody::BaselineCreated { baseline } => baseline.baseline_id,
        other => panic!("{other:?}"),
    };
    // A second one, so that the first is not the current baseline.
    let _ = agent_ok(
        &mut a,
        &auth,
        AgentCall::Snapshot {
            operation_id: OperationId::new_v7(),
            vm_id: vm,
            label: "checkpoint two".into(),
        },
    )
    .await;
    assert!(matches!(
        agent_ok(&mut a, &auth, AgentCall::EndSession).await,
        ResponseBody::SessionEnded { .. }
    ));
    drop(a);
    // Session 2 of the same grant (after a restart): restorable.
    let svc = restart(svc).await;
    let mut u = ui(&svc).await;
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    match agent_ok(&mut a, &auth, AgentCall::ListBaselines { vm_id: vm }).await {
        ResponseBody::AgentBaselines { baselines, .. } => {
            let b = baselines.iter().find(|b| b.baseline_id == first).unwrap();
            assert!(b.restorable && b.created_by_session);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        agent_ok(
            &mut a,
            &auth,
            AgentCall::Restore {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                baseline_id: first,
            }
        )
        .await,
        ResponseBody::BaselineRestored { .. }
    ));
    assert!(matches!(
        agent_ok(&mut a, &auth, AgentCall::EndSession).await,
        ResponseBody::SessionEnded { .. }
    ));
    drop(a);
    // A session approved by the user (not of the grant) cannot restore it.
    let gid = live_grant(&mut u).await.unwrap().grant_id;
    let _ = ok(&mut u, Request::RevokeStandingGrant { grant_id: gid }).await;
    assert_eq!(
        ok(&mut u, Request::SetAgentAccess { enabled: true }).await,
        ResponseBody::AgentAccess { enabled: true }
    );
    let pc = match ok(&mut u, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    };
    let mut m = connect(&svc, &pc).await;
    let request_id = match ok(&mut m, request(vec![vm], all_caps(), Vec::new())).await {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    m.send(&ClientMessage::Request {
        request_id: 9_000,
        body: Request::AwaitSession { request_id },
    })
    .await
    .unwrap();
    let _ = ok(
        &mut u,
        Request::ApproveSession {
            request_id,
            approval: SessionApproval {
                tools: Default::default(),
                vm_id: vm,
                capabilities: caps(&[
                    Capability::VmList,
                    Capability::VmStatus,
                    Capability::VmRestore,
                ]),
                duration_s: 3600,
                quotas: None,
                lease_ms: None,
                give_input: false,
                network_endpoints: Vec::new(),
            },
        },
    )
    .await;
    let auth = match tokio::time::timeout(T, m.recv()).await.unwrap().unwrap() {
        ServerMessage::Response {
            result:
                ResponseBody::SessionGranted {
                    session_id, token, ..
                },
            ..
        } => SessionAuth { session_id, token },
        other => panic!("{other:?}"),
    };
    // `first` is no longer the current baseline (the restore made it
    // current: take the other one, created by the grant's first session).
    let target = match agent_ok(&mut m, &auth, AgentCall::ListBaselines { vm_id: vm }).await {
        ResponseBody::AgentBaselines { baselines, .. } => {
            let b = baselines
                .iter()
                .find(|b| !b.initial)
                .expect("a baseline that is not the initial one");
            assert!(!b.restorable && !b.created_by_session);
            b.baseline_id
        }
        other => panic!("{other:?}"),
    };
    let e = err(
        &mut m,
        Request::Agent {
            session: auth,
            call: AgentCall::Restore {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                baseline_id: target,
            },
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(reason(&e), "baseline_not_restorable");
    svc.handle.shutdown().await;
}

// ----- 11. duration bound and roles ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn duration_over_seven_days_is_refused_and_agents_cannot_grant_themselves() {
    let svc = start_svc("sg-bounds").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let e = err(
        &mut u,
        Request::CreateStandingGrant {
            spec: spec(vm, lightvm_proto::MAX_STANDING_GRANT_S + 1),
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let e = err(
        &mut u,
        Request::CreateStandingGrant {
            spec: StandingGrantSpec {
                capabilities: CapabilitySet::new(),
                ..spec(vm, 3600)
            },
        },
    )
    .await;
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let (grant, cred) = create_grant(&mut u, spec(vm, lightvm_proto::MAX_STANDING_GRANT_S)).await;
    assert_eq!(
        grant.expires_at.millis_since(grant.created_at),
        i64::from(lightvm_proto::MAX_STANDING_GRANT_S) * 1000
    );
    // The standing credential is a pairing credential: no request about
    // standing grants, no UI request (it cannot grant itself anything).
    let mut a = connect(&svc, &cred).await;
    for r in [
        Request::GetStandingGrant,
        Request::CreateStandingGrant {
            spec: spec(vm, 3600),
        },
        Request::RevokeStandingGrant {
            grant_id: grant.grant_id,
        },
        Request::ConfirmStandingGrant {
            grant_id: grant.grant_id,
        },
        Request::SetAgentAccess { enabled: true },
        Request::ListSessionRequests,
    ] {
        let name = r.name();
        let e = err(&mut a, r).await;
        assert_eq!(e.code, ErrorCode::PermissionDenied, "{name}");
    }
    match a.hello(&cred).await {
        Ok(ServerMessage::Error { .. }) | Err(_) => {}
        other => panic!("second hello: {other:?}"),
    }

    svc.handle.shutdown().await;
}

// ----- the agent access switch does not touch the standing session ---------------

#[tokio::test(flavor = "multi_thread")]
async fn agent_access_switch_off_leaves_the_standing_session() {
    let svc = start_svc("sg-switch").await;
    let mut u = ui(&svc).await;
    let vm = vm_with_disk(&mut u, &svc, "sg-vm").await;
    let (_, cred) = create_grant(&mut u, spec(vm, 3600)).await;
    let mut a = connect(&svc, &cred).await;
    let (auth, _) = auto_session(&mut a).await;
    assert_eq!(
        ok(&mut u, Request::SetAgentAccess { enabled: true }).await,
        ResponseBody::AgentAccess { enabled: true }
    );
    assert_eq!(
        ok(&mut u, Request::SetAgentAccess { enabled: false }).await,
        ResponseBody::AgentAccess { enabled: false }
    );
    assert!(matches!(
        agent_ok(&mut a, &auth, AgentCall::ListVms).await,
        ResponseBody::AgentVms { .. }
    ));
    let s = sessions(&mut u)
        .await
        .into_iter()
        .find(|s| s.session_id == auth.session_id)
        .unwrap();
    assert_eq!(s.termination, None);
    assert!(live_grant(&mut u).await.is_some());
    svc.handle.shutdown().await;
}
