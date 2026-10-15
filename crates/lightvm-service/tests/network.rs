//! ADR-0033 / ADR-0034 through the real IPC with the real core, in-memory
//! ports and a fake backend: the network of each run (NAT in manual use
//! only, the endpoint allowlist or no network under an agent session), the
//! `network.endpoints` capability requested, approved and enforced, the
//! allowed endpoints following the session (revocation), and the network
//! records in the audit and in the session report.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    AgentCall, AgentEndpointView, ClientMessage, Request, ResponseBody, Role, ServerMessage,
    SessionApproval, SessionAuth, SessionInfo,
};
use lightvm_proto::report::ReportEventBody;
use lightvm_proto::{
    AuditAction, AuditOutcome, Capability, CapabilitySet, ErrorCode, NetworkAudit, NetworkEndpoint,
    NetworkProfile, OperationId, ReplyError, RunId, SessionId, TerminationCause, VmAction, VmId,
    VmRequest, VmState,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::NetworkRecord;
use lightvm_service::fakes::{
    Calls, FakeHost, FakeVmBackend, MemoryAudit, NetAllowed, sample_config, sample_engine,
};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(10);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    audit: MemoryAudit,
    allowed: NetAllowed,
    records: Arc<Mutex<Vec<NetworkRecord>>>,
}

async fn start_svc(name: &str) -> Svc {
    let dir = temp_dir(name);
    start_svc_in(&dir).await
}

async fn start_svc_in(dir: &Path) -> Svc {
    let calls = Calls::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        ..FakeVmBackend::default()
    };
    let allowed = backend.net_allowed.clone();
    let records = backend.net_records.clone();
    let audit = MemoryAudit::default();
    let engine = sample_engine(backend, audit.clone());
    let host = FakeHost {
        calls: calls.clone(),
    };
    let mut cfg = ServiceConfig::new(dir);
    cfg.runtime_dir = Some(dir.join("run"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(host))
        .await
        .expect("service start");
    Svc {
        handle,
        ui,
        calls,
        audit,
        allowed,
        records,
    }
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

async fn agent_client(svc: &Svc, ui: &mut Client) -> Client {
    let cred = match ok(ui, Request::CreatePairingCredential).await {
        ResponseBody::PairingCredential { credential, .. } => {
            Credential::from_hex(&credential).unwrap()
        }
        other => panic!("{other:?}"),
    };
    let mut c = connect(svc.handle.endpoint()).await;
    match c.hello(&cred).await.unwrap() {
        ServerMessage::HelloOk {
            role: Role::Pairing,
            ..
        } => c,
        other => panic!("{other:?}"),
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

async fn wait_state(ui: &mut Client, vm: VmId, want: VmState) -> Option<RunId> {
    let deadline = Instant::now() + T;
    loop {
        let (s, run) = match ok(ui, Request::VmStatus { vm_id: vm }).await {
            ResponseBody::Vm { vm } => (vm.state, vm.run_id),
            other => panic!("{other:?}"),
        };
        if s == want {
            return run;
        }
        assert!(Instant::now() < deadline, "VM not {want:?} (now {s:?})");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn ui_action(ui: &mut Client, vm: VmId, action: VmAction, want: VmState) -> Option<RunId> {
    let r = ok(
        ui,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: op_id(),
                vm_id: vm,
                expected_run_id: None,
                action,
            },
        },
    )
    .await;
    assert!(matches!(r, ResponseBody::Operation { .. }), "{r:?}");
    wait_state(ui, vm, want).await
}

fn endpoint(name: &str, port: u16, last: u8) -> NetworkEndpoint {
    NetworkEndpoint {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        virtual_ip: Ipv4Addr::new(10, 0, 2, last),
    }
}

/// The sample VM reconfigured with `mode` and two endpoints.
async fn network_vm(ui: &mut Client, mode: NetworkProfile) -> VmId {
    let (vm, rev) = match ok(ui, Request::ListVms).await {
        ResponseBody::VmList { vms } => (vms[0].vm_id, vms[0].config_revision),
        other => panic!("{other:?}"),
    };
    let mut config = sample_config("sample");
    config.network = mode;
    config.endpoints = vec![
        endpoint("sql.lan", 1433, 101),
        endpoint("WcfHost", 8733, 102),
    ];
    match ok(
        ui,
        Request::UpdateVm {
            vm_id: vm,
            expected_revision: rev,
            config,
        },
    )
    .await
    {
        ResponseBody::VmUpdated { .. } => {}
        other => panic!("{other:?}"),
    }
    vm
}

fn approval(vm: VmId, caps: &[Capability], endpoints: &[&str]) -> SessionApproval {
    SessionApproval {
        vm_id: vm,
        capabilities: caps.iter().copied().collect::<CapabilitySet>(),
        duration_s: 3600,
        quotas: None,
        lease_ms: None,
        give_input: false,
        network_endpoints: endpoints.iter().map(|s| (*s).to_owned()).collect(),
    }
}

const CAPS: [Capability; 5] = [
    Capability::VmList,
    Capability::VmStatus,
    Capability::VmStart,
    Capability::VmStop,
    Capability::NetworkEndpoints,
];

/// Request (with the endpoints the agent asks for) and approval.
async fn request_and_approve(
    ui: &mut Client,
    agent: &mut Client,
    a: SessionApproval,
    requested: &[&str],
) -> Result<(SessionAuth, SessionInfo), lightvm_proto::PublicError> {
    let request_id = match ok(
        agent,
        Request::RequestSession {
            client_name: "net-agent".into(),
            purpose: "ADR-0033 test".into(),
            requested_vms: vec![a.vm_id],
            requested_capabilities: a.capabilities.clone(),
            requested_endpoints: requested.iter().map(|s| (*s).to_owned()).collect(),
        },
    )
    .await
    {
        ResponseBody::SessionRequested { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    match ok(ui, Request::ListSessionRequests).await {
        ResponseBody::SessionRequests { requests } => {
            let r = requests
                .iter()
                .find(|r| r.request_id == request_id)
                .unwrap();
            assert_eq!(
                r.requested_endpoints,
                requested
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
            );
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
    let info = match tokio::time::timeout(
        T,
        ui.request(Request::ApproveSession {
            request_id,
            approval: a,
        }),
    )
    .await
    .unwrap()
    .unwrap()
    {
        ServerMessage::Response {
            result: ResponseBody::Session { session },
            ..
        } => session,
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => {
            // The waiting agent hears about it later; deny to close it.
            let _ = ok(ui, Request::DenySession { request_id }).await;
            return Err(e);
        }
        other => panic!("{other:?}"),
    };
    let auth = match tokio::time::timeout(T, agent.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ServerMessage::Response {
            result:
                ResponseBody::SessionGranted {
                    session_id, token, ..
                },
            ..
        } => SessionAuth { session_id, token },
        other => panic!("{other:?}"),
    };
    Ok((auth, info))
}

async fn agent_ok(agent: &mut Client, auth: &SessionAuth, call: AgentCall) -> ResponseBody {
    match tokio::time::timeout(
        T,
        agent.request(Request::Agent {
            session: auth.clone(),
            call,
        }),
    )
    .await
    .unwrap()
    .unwrap()
    {
        ServerMessage::Response { result, .. } => result,
        other => panic!("{other:?}"),
    }
}

fn applied(svc: &Svc, vm: VmId) -> Option<BTreeSet<String>> {
    svc.allowed
        .lock()
        .unwrap()
        .get(&vm)
        .and_then(|(_, a)| a.clone())
}

async fn wait_applied(svc: &Svc, ui: &mut Client, vm: VmId, want: &[&str]) {
    let want: BTreeSet<String> = want.iter().map(|s| (*s).to_owned()).collect();
    let deadline = Instant::now() + T;
    while applied(svc, vm).as_ref() != Some(&want) {
        assert!(
            Instant::now() < deadline,
            "allowed {:?}, want {want:?}",
            applied(svc, vm)
        );
        // Any request makes the supervisor publish (and sync the network).
        let _ = ok(ui, Request::Ping).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn network_calls(svc: &Svc) -> Vec<String> {
    svc.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.starts_with("vm_network:"))
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn allowlist_follows_the_session_and_is_audited() {
    let svc = start_svc("net-allow").await;
    let mut u = ui(&svc).await;
    let vm = network_vm(&mut u, NetworkProfile::Endpoints).await;
    ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
    // Manual use: every endpoint of the VM.
    ui_action(&mut u, vm, VmAction::Start, VmState::Running).await;
    assert_eq!(network_calls(&svc), ["vm_network:endpoints"]);
    wait_applied(&svc, &mut u, vm, &["sql.lan", "wcfhost"]).await;

    // A session granted one endpoint: only that one, at once.
    let mut a = agent_client(&svc, &mut u).await;
    let (auth, info) = request_and_approve(
        &mut u,
        &mut a,
        approval(vm, &CAPS, &["SQL.lan"]),
        &["sql.lan"],
    )
    .await
    .unwrap();
    assert_eq!(info.network_endpoints, ["sql.lan"]);
    assert!(info.capabilities.contains(&Capability::NetworkEndpoints));
    wait_applied(&svc, &mut u, vm, &["sql.lan"]).await;
    // The agent sees the granted endpoint as it uses it inside the guest.
    match agent_ok(&mut a, &auth, AgentCall::VmStatus { vm_id: vm }).await {
        ResponseBody::AgentVm { vm } => assert_eq!(
            vm.network_endpoints,
            [AgentEndpointView {
                name: "sql.lan".into(),
                virtual_ip: Ipv4Addr::new(10, 0, 2, 101),
                port: 1433,
            }]
        ),
        other => panic!("{other:?}"),
    }
    // A connection record of the stack: audited with the session, and in
    // the session report.
    let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
    svc.records.lock().unwrap().push(NetworkRecord {
        vm_id: vm,
        run_id: run,
        action: AuditAction::NetworkConnection,
        outcome: AuditOutcome::Succeeded,
        detail: NetworkAudit {
            endpoint: Some("sql.lan".into()),
            destination: Some("10.0.2.101:1433".into()),
            target: Some("127.0.0.1:1433".into()),
            bytes_from_guest: Some(11),
            bytes_to_guest: Some(18),
            reason: Some("closed".into()),
            ..NetworkAudit::default()
        },
    });
    let _ = ok(&mut u, Request::Ping).await;
    let deadline = Instant::now() + T;
    let conn = loop {
        let ev = svc.audit.events();
        if let Some(e) = ev
            .iter()
            .find(|e| e.action == AuditAction::NetworkConnection)
        {
            break e.clone();
        }
        assert!(Instant::now() < deadline, "connection not audited");
        let _ = ok(&mut u, Request::Ping).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(conn.session_id, Some(auth.session_id));
    assert_eq!(conn.run_id, Some(run));
    assert_eq!(conn.network.as_ref().unwrap().bytes_to_guest, Some(18));

    // Revocation: nothing allowed any more on this run.
    ok(
        &mut u,
        Request::RevokeSession {
            session_id: auth.session_id,
        },
    )
    .await;
    wait_applied(&svc, &mut u, vm, &[]).await;
    let policies: Vec<String> = svc
        .audit
        .events()
        .iter()
        .filter(|e| e.action == AuditAction::NetworkPolicy)
        .filter_map(|e| e.network.as_ref().and_then(|n| n.reason.clone()))
        .collect();
    assert_eq!(
        policies,
        [
            "allowed: sql.lan, wcfhost",
            "allowed: sql.lan",
            "allowed: none"
        ]
    );
    // Report: the granted endpoints and the network events.
    report_has_network(&mut u, auth.session_id).await;
    // The next start is manual use again: every endpoint.
    ui_action(&mut u, vm, VmAction::Stop, VmState::Stopped).await;
    ui_action(&mut u, vm, VmAction::Start, VmState::Running).await;
    wait_applied(&svc, &mut u, vm, &["sql.lan", "wcfhost"]).await;
}

async fn report_has_network(u: &mut Client, session_id: SessionId) {
    match ok(u, Request::GetSessionReport { session_id }).await {
        ResponseBody::SessionReport { report } => {
            assert_eq!(report.network_endpoints, ["sql.lan"]);
            assert!(report.capabilities.contains(&Capability::NetworkEndpoints));
            assert_eq!(report.termination, Some(TerminationCause::Revoked));
        }
        other => panic!("{other:?}"),
    }
    match ok(
        u,
        Request::GetSessionReportEvents {
            session_id,
            offset: 0,
            limit: 50,
        },
    )
    .await
    {
        ResponseBody::SessionReportEvents { events, .. } => {
            let net: Vec<&str> = events
                .iter()
                .filter_map(|e| match &e.body {
                    ReportEventBody::Network { action, .. } => Some(action.as_str()),
                    _ => None,
                })
                .collect();
            assert!(net.contains(&"network_policy"), "{net:?}");
            assert!(net.contains(&"network_connection"), "{net:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nat_is_for_manual_use_only() {
    let svc = start_svc("net-nat").await;
    let mut u = ui(&svc).await;
    let vm = network_vm(&mut u, NetworkProfile::Nat).await;
    ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
    ui_action(&mut u, vm, VmAction::Start, VmState::Running).await;
    // A session on a VM running with NAT is refused (stop it first).
    let mut a = agent_client(&svc, &mut u).await;
    let e = request_and_approve(&mut u, &mut a, approval(vm, &CAPS, &["sql.lan"]), &[])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::StateConflict);
    assert_eq!(
        e.details
            .as_ref()
            .and_then(|d| d.get("reason"))
            .map(String::as_str),
        Some("nat_running")
    );
    ui_action(&mut u, vm, VmAction::Stop, VmState::Stopped).await;
    // Approved while stopped: the next run (started by the user) gets the
    // session's endpoint allowlist, never NAT.
    let mut a = agent_client(&svc, &mut u).await;
    let (auth, _) = request_and_approve(&mut u, &mut a, approval(vm, &CAPS, &["wcfhost"]), &[])
        .await
        .unwrap();
    ui_action(&mut u, vm, VmAction::Start, VmState::Running).await;
    wait_applied(&svc, &mut u, vm, &["wcfhost"]).await;
    ui_action(&mut u, vm, VmAction::Stop, VmState::Stopped).await;
    ok(
        &mut u,
        Request::RevokeSession {
            session_id: auth.session_id,
        },
    )
    .await;
    // Without the capability: no network at all.
    let mut a = agent_client(&svc, &mut u).await;
    let caps = &CAPS[..4];
    let (auth, info) = request_and_approve(&mut u, &mut a, approval(vm, caps, &[]), &[])
        .await
        .unwrap();
    assert!(info.network_endpoints.is_empty());
    ui_action(&mut u, vm, VmAction::Start, VmState::Running).await;
    match agent_ok(&mut a, &auth, AgentCall::VmStatus { vm_id: vm }).await {
        ResponseBody::AgentVm { vm } => assert!(vm.network_endpoints.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        network_calls(&svc),
        [
            "vm_network:nat",
            "vm_network:endpoints",
            "vm_network:offline"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_are_checked_against_the_vm() {
    let svc = start_svc("net-grant").await;
    let mut u = ui(&svc).await;
    let vm = network_vm(&mut u, NetworkProfile::Endpoints).await;
    ok(&mut u, Request::SetAgentAccess { enabled: true }).await;
    let mut a = agent_client(&svc, &mut u).await;
    // An endpoint the VM does not have.
    let e = request_and_approve(
        &mut u,
        &mut a,
        approval(vm, &CAPS, &["mail.lan"]),
        &["mail.lan"],
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    // Endpoints without the capability.
    let mut a = agent_client(&svc, &mut u).await;
    let e = request_and_approve(&mut u, &mut a, approval(vm, &CAPS[..4], &["sql.lan"]), &[])
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    // The network settings are UI-only (no agent call changes them) and a
    // running VM cannot change its mode.
    ui_action(&mut u, vm, VmAction::Start, VmState::Running).await;
    let rev = match ok(&mut u, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => vm.config_revision,
        other => panic!("{other:?}"),
    };
    let mut c = sample_config("sample");
    c.network = NetworkProfile::Nat;
    let e = err(
        &mut u,
        Request::UpdateVm {
            vm_id: vm,
            expected_revision: rev,
            config: c,
        },
    )
    .await;
    assert_ne!(e.code, ErrorCode::InvalidArgument, "{e:?}");
}
