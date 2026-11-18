//! ADR-0039 with real QEMU through the service (ignored test, lab): a
//! standing grant on a real VM, at least three accelerated session
//! renewals (lease loss, client loss, explicit end), a restart of LightVM
//! (service and supervisor) in the middle, the VM still running between the
//! sessions of the grant, revocation
//! (`docs/verification/R6-10-standing-grant.md`).
//!
//! Requires `LIGHTVM_E2E_QEMU_DIR` and `LIGHTVM_E2E_ISO` (an installation ISO
//! the VM boots from; nothing is installed). Optional `LIGHTVM_E2E_CPU`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{AgentCall, Request, ResponseBody, ServerMessage, SessionAuth};
use lightvm_proto::{
    Capability, CapabilitySet, ErrorCode, MediaKind, OperationId, ReplyError, RunId,
    StandingGrantSpec, TerminationCause, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::backend::{QemuSetup, probe};
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::sample_config;
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(120);

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

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
}

fn start_real(rt: &tokio::runtime::Runtime, dir: &Path) -> Svc {
    let qemu =
        PathBuf::from(std::env::var_os("LIGHTVM_E2E_QEMU_DIR").expect("LIGHTVM_E2E_QEMU_DIR"));
    let cpu = std::env::var("LIGHTVM_E2E_CPU").unwrap_or_else(|_| "Skylake-Client-v4".into());
    let mut last = None;
    for _ in 0..100 {
        let store = match SqliteStore::open(dir) {
            Ok(s) => s,
            Err(e) => {
                last = Some(format!("{e}"));
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let mut setup = QemuSetup::new(qemu.clone(), store.root(), short_dir("lsr"));
        setup.cpu_model = cpu.clone();
        let (host, vm_backend, capacity) = probe(setup).expect("QEMU probe");
        assert!(capacity.accelerator_available, "accelerator required");
        let audit = FileAuditSink::open(&dir.join("audit").join("audit.jsonl")).unwrap();
        let inspector = ManagedMediaInspector::new(store.root());
        let engine =
            CoreEngine::new(store, audit, vm_backend, inspector, core_config(capacity)).unwrap();
        let mut cfg = ServiceConfig::new(dir);
        cfg.runtime_dir = Some(dir.join("run"));
        cfg.display_dir = Some(short_dir("lsd"));
        match rt.block_on(start(cfg, Box::new(engine), Box::new(host))) {
            Ok((handle, ui)) => return Svc { handle, ui },
            Err(e) => {
                last = Some(format!("{e:?}"));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    panic!("service not started: {last:?}");
}

async fn connect(svc: &Svc, cred: &Credential) -> Client {
    let mut c = Client::connect(svc.handle.endpoint()).await.unwrap();
    match c.hello(cred).await.unwrap() {
        ServerMessage::HelloOk { .. } => c,
        other => panic!("{other:?}"),
    }
}

async fn ok(c: &mut Client, r: Request) -> ResponseBody {
    let name = r.name();
    match tokio::time::timeout(Duration::from_secs(1800), c.request(r))
        .await
        .expect("timeout")
        .unwrap()
    {
        ServerMessage::Response { result, .. } => result,
        other => panic!("{name}: {other:?}"),
    }
}

async fn vm_state(c: &mut Client, vm: VmId) -> (VmState, Option<RunId>) {
    match ok(c, Request::VmStatus { vm_id: vm }).await {
        ResponseBody::Vm { vm } => (vm.state, vm.run_id),
        other => panic!("{other:?}"),
    }
}

async fn wait_state(c: &mut Client, vm: VmId, want: VmState) -> Option<RunId> {
    let t0 = Instant::now();
    loop {
        let (s, run) = vm_state(c, vm).await;
        if s == want {
            return run;
        }
        assert!(t0.elapsed() < T, "VM not {want:?} (now {s:?})");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn auto_session(svc: &Svc, cred: &Credential) -> (Client, SessionAuth) {
    let mut a = connect(svc, cred).await;
    let all: CapabilitySet = Capability::ALL.into_iter().collect();
    let request_id = match ok(
        &mut a,
        Request::RequestSession {
            client_name: "r6-10 agent".into(),
            purpose: "ADR-0039 hardware cycle".into(),
            requested_vms: Vec::new(),
            requested_capabilities: all,
            requested_endpoints: Vec::new(),
        },
    )
    .await
    {
        ResponseBody::SessionRequested {
            request_id,
            standing: true,
            ..
        } => request_id,
        other => panic!("{other:?}"),
    };
    match ok(&mut a, Request::AwaitSession { request_id }).await {
        ResponseBody::SessionGranted {
            session_id, token, ..
        } => (a, SessionAuth { session_id, token }),
        other => panic!("{other:?}"),
    }
}

async fn agent(a: &mut Client, auth: &SessionAuth, call: AgentCall) -> ResponseBody {
    ok(
        a,
        Request::Agent {
            session: auth.clone(),
            call,
        },
    )
    .await
}

async fn screenshot_bytes(a: &mut Client, auth: &SessionAuth, vm: VmId, run: RunId) -> u64 {
    match agent(
        a,
        auth,
        AgentCall::Screenshot {
            operation_id: OperationId::new_v7(),
            vm_id: vm,
            expected_run_id: run,
        },
    )
    .await
    {
        ResponseBody::AgentScreenshot { screenshot, .. } => screenshot.byte_len,
        other => panic!("{other:?}"),
    }
}

async fn termination(u: &mut Client, sid: lightvm_proto::SessionId) -> TerminationCause {
    let t0 = Instant::now();
    loop {
        if let ResponseBody::Sessions { sessions } = ok(u, Request::ListSessions).await
            && let Some(c) = sessions
                .iter()
                .find(|s| s.session_id == sid)
                .and_then(|s| s.termination)
        {
            return c;
        }
        assert!(t0.elapsed() < T, "session still live");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[test]
#[ignore = "requires approved QEMU, an accelerator and an ISO (LIGHTVM_E2E_QEMU_DIR, LIGHTVM_E2E_ISO)"]
fn e2e_real_qemu_standing_grant_cycle() {
    let iso = PathBuf::from(std::env::var_os("LIGHTVM_E2E_ISO").expect("LIGHTVM_E2E_ISO"));
    let dir = temp_dir("sg-real");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let t_all = Instant::now();
    let svc = start_real(&rt, &dir);
    let (vm, grant, cred, run) = rt.block_on(async {
        let mut u = connect(&svc, &svc.ui.clone()).await;
        let p = std::fs::canonicalize(&iso).unwrap();
        let p = p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
        let media = match ok(
            &mut u,
            Request::ImportMedia {
                source_path: p,
                media_kind: MediaKind::InstallIso,
                declared_origin: "lab media".into(),
            },
        )
        .await
        {
            ResponseBody::MediaImported { media } => media,
            other => panic!("{other:?}"),
        };
        let mut cfg = sample_config("r6-10");
        cfg.vcpus = 2;
        cfg.memory_mib = 2048;
        cfg.disk_gib = 8;
        cfg.install_media = Some(media.id);
        let vm = match ok(&mut u, Request::CreateVm { config: cfg }).await {
            ResponseBody::VmCreated { vm } => vm.id,
            other => panic!("{other:?}"),
        };
        let start_vm = |vm| Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: vm,
                expected_run_id: None,
                action: VmAction::Start,
            },
        };
        let _ = ok(&mut u, start_vm(vm)).await;
        let run = wait_state(&mut u, vm, VmState::Running).await.unwrap();
        eprintln!("[r6-10] VM running, run {run}");

        // Grant: lease 1 s, so that a silent agent loses its session fast.
        let caps: CapabilitySet = [
            Capability::VmList,
            Capability::VmStatus,
            Capability::VmStart,
            Capability::VmStop,
            Capability::VmScreenshot,
        ]
        .into_iter()
        .collect();
        let spec = StandingGrantSpec {
            vm_id: vm,
            capabilities: caps,
            tools: Default::default(),
            network_endpoints: Vec::new(),
            quotas: None,
            lease_ms: Some(1000),
            give_input: false,
            duration_s: 3600,
            client_name: None,
        };
        let (grant, cred) = match ok(&mut u, Request::CreateStandingGrant { spec }).await {
            ResponseBody::StandingGrantCreated { grant, credential } => {
                (grant, Credential::from_hex(&credential).unwrap())
            }
            other => panic!("{other:?}"),
        };
        eprintln!(
            "[r6-10] grant {} until {}",
            grant.grant_id, grant.expires_at
        );

        // Session 1, ends by lease loss: the VM keeps running (same run).
        let (mut a, auth) = auto_session(&svc, &cred).await;
        let n = screenshot_bytes(&mut a, &auth, vm, run).await;
        eprintln!("[r6-10] session 1 {} screenshot {n} B", auth.session_id);
        let c = termination(&mut u, auth.session_id).await;
        assert_eq!(c, TerminationCause::LeaseExpired);
        assert_eq!(vm_state(&mut u, vm).await, (VmState::Running, Some(run)));
        eprintln!("[r6-10] renewal 1 after lease loss: VM running, same run");
        drop(a);

        // Session 2, ends by client loss: the VM keeps running.
        let (mut a, auth) = auto_session(&svc, &cred).await;
        let n = screenshot_bytes(&mut a, &auth, vm, run).await;
        eprintln!("[r6-10] session 2 {} screenshot {n} B", auth.session_id);
        drop(a);
        assert_eq!(
            termination(&mut u, auth.session_id).await,
            TerminationCause::ClientLost
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(vm_state(&mut u, vm).await, (VmState::Running, Some(run)));
        eprintln!("[r6-10] renewal 2 after client loss: VM running, same run");

        // Restart of LightVM (service and supervisor) in the middle. Closing
        // LightVM stops its VMs in an orderly way (ADR-0039: UI close keeps
        // stopping); the grant survives.
        drop(u);
        svc.handle.shutdown().await;
        eprintln!("[r6-10] LightVM stopped");
        (vm, grant, cred, run)
    });
    let _ = run;
    let svc = start_real(&rt, &dir);
    rt.block_on(async {
        let mut u = connect(&svc, &svc.ui.clone()).await;
        match ok(&mut u, Request::GetStandingGrant).await {
            ResponseBody::StandingGrant { grant: Some(g), .. } => {
                assert_eq!(g.grant_id, grant.grant_id);
                eprintln!(
                    "[r6-10] grant kept after the restart ({} sessions so far)",
                    g.sessions_approved
                );
            }
            other => panic!("grant lost: {other:?}"),
        }
        assert!(!svc.handle.agent_access(), "switch off after the restart");
        assert_eq!(vm_state(&mut u, vm).await.0, VmState::Stopped);

        // Session 3 after the restart (no click). The user starts the VM
        // again (this test client cannot renew the 1 s lease while it waits
        // for the deferred reply of a start that re-hashes the ISO); the
        // agent then ends the session explicitly; the VM keeps running.
        let _ = ok(
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
        let run2 = wait_state(&mut u, vm, VmState::Running).await.unwrap();
        let (mut a, auth) = auto_session(&svc, &cred).await;
        for _ in 0..5 {
            let _ = agent(&mut a, &auth, AgentCall::RenewLease).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let n = screenshot_bytes(&mut a, &auth, vm, run2).await;
        eprintln!(
            "[r6-10] session 3 {} on run {run2}, screenshot {n} B",
            auth.session_id
        );
        assert!(matches!(
            agent(&mut a, &auth, AgentCall::EndSession).await,
            ResponseBody::SessionEnded { .. }
        ));
        assert_eq!(vm_state(&mut u, vm).await, (VmState::Running, Some(run2)));
        drop(a);
        eprintln!("[r6-10] renewal 3 after the explicit end: VM running");

        // Session 4, then revocation: session ended, credential refused,
        // VM still running.
        let (mut a, auth) = auto_session(&svc, &cred).await;
        let n = screenshot_bytes(&mut a, &auth, vm, run2).await;
        eprintln!("[r6-10] session 4 {} screenshot {n} B", auth.session_id);
        let t = Instant::now();
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
        assert_eq!(
            termination(&mut u, auth.session_id).await,
            TerminationCause::Revoked
        );
        eprintln!(
            "[r6-10] revocation: session ended {:?} after the request",
            t.elapsed()
        );
        let mut c = Client::connect(svc.handle.endpoint()).await.unwrap();
        match c.hello(&cred).await {
            Ok(ServerMessage::Error {
                error: ReplyError::Operational(e),
                ..
            }) => assert_eq!(e.code, ErrorCode::Unauthenticated),
            other => panic!("credential still accepted: {other:?}"),
        }
        assert_eq!(vm_state(&mut u, vm).await, (VmState::Running, Some(run2)));

        drop(a);
        eprintln!("[r6-10] after revocation: credential refused, VM running");

        // Clean up: the user stops the VM, LightVM closes.
        let _ = ok(
            &mut u,
            Request::SubmitVm {
                request: VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: vm,
                    expected_run_id: Some(run2),
                    action: VmAction::ForceStop,
                },
            },
        )
        .await;
        wait_state(&mut u, vm, VmState::Stopped).await;
        svc.handle.shutdown().await;
        eprintln!("[r6-10] total {:?}", t_all.elapsed());
    });
}
