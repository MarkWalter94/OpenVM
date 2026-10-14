//! F3 requests through the real IPC: baselines (create, list, restore,
//! delete), current state at start, cancellation by the Emergency Stop,
//! refusals. Real core and SQLite catalog (storage journal, guards); fake VM
//! backend with a fake image tool (no QEMU, no `qemu-img`): the disks are
//! small files with a QCOW2 header (`fakes::fake_qcow2`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lightvm_proto::ipc::{
    BaselineSummary, Request, ResponseBody, Role, ServerMessage, ServiceEvent, StorageOperation,
    VmSummary,
};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, BaselineId, ErrorCode, Label, OperationId,
    OperationStatus, ReplyError, RunId, VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FakeHost, FakeImageTool, FakeVmBackend, fake_content, fake_qcow2, host_capacity,
    sample_config,
};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(20);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    /// F6: `qemu-img convert` fails as on a full volume while set.
    disk_full: Arc<std::sync::atomic::AtomicBool>,
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

async fn start_svc(name: &str, convert_delay: Duration) -> Svc {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).unwrap();
    let audit = FileAuditSink::open(&audit_path(&dir)).unwrap();
    let calls = Calls::default();
    let disk_full = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let backend = FakeVmBackend {
        calls: calls.clone(),
        image_tool: Some(Arc::new(FakeImageTool {
            convert_delay,
            calls: calls.clone(),
            disk_full: disk_full.clone(),
            ..FakeImageTool::default()
        })),
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
    let mut cfg = ServiceConfig::new(&dir);
    cfg.runtime_dir = Some(dir.join("run"));
    cfg.display_dir = Some(short_dir("lvd"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(host))
        .await
        .expect("start");
    Svc {
        handle,
        ui,
        calls,
        disk_full,
        dir,
    }
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

async fn req(c: &mut Client, r: Request) -> ServerMessage {
    tokio::time::timeout(T, c.request(r))
        .await
        .expect("timeout")
        .unwrap()
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

async fn submit(c: &mut Client, vm_id: VmId, action: VmAction) -> ServerMessage {
    req(
        c,
        Request::SubmitVm {
            request: VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id,
                expected_run_id: None,
                action,
            },
        },
    )
    .await
}

fn op_status(m: &ServerMessage) -> (OperationStatus, Option<ErrorCode>) {
    match m {
        ServerMessage::Response {
            result:
                ResponseBody::Operation {
                    operation: Some(op),
                    ..
                },
            ..
        } => (op.status, op.error),
        other => panic!("{other:?}"),
    }
}

async fn status(c: &mut Client, vm_id: VmId) -> VmSummary {
    match req(c, Request::VmStatus { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    }
}

/// New VM with its standalone disk and UEFI variables written as the QEMU
/// backend would after a first start (the fake backend writes no files).
async fn vm_with_disk(c: &mut Client, svc: &Svc, name: &str, content: &[u8]) -> VmId {
    let vm = match req(
        c,
        Request::CreateVm {
            config: sample_config(name),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    };
    let d = svc.dir.join("vms").join(vm.id.to_string());
    std::fs::create_dir_all(d.join("firmware")).unwrap();
    std::fs::write(d.join("primary.qcow2"), fake_qcow2(None, content)).unwrap();
    std::fs::write(d.join("firmware").join("primary.vars.fd"), b"VARS").unwrap();
    vm.id
}

fn label(s: &str) -> Label {
    Label::parse(s).unwrap()
}

async fn create_baseline(
    c: &mut Client,
    vm_id: VmId,
    l: &str,
    allow_unclean: bool,
) -> ServerMessage {
    req(
        c,
        Request::CreateBaseline {
            vm_id,
            label: label(l),
            allow_unclean,
        },
    )
    .await
}

fn created(m: &ServerMessage) -> BaselineSummary {
    match m {
        ServerMessage::Response {
            result: ResponseBody::BaselineCreated { baseline },
            ..
        } => baseline.clone(),
        other => panic!("{other:?}"),
    }
}

async fn restore(c: &mut Client, vm_id: VmId, baseline_id: BaselineId) -> ServerMessage {
    req(c, Request::RestoreBaseline { vm_id, baseline_id }).await
}

async fn list(c: &mut Client, vm_id: VmId) -> Vec<BaselineSummary> {
    match req(c, Request::ListBaselines { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Baselines { baselines, .. },
            ..
        } => baselines,
        other => panic!("{other:?}"),
    }
}

fn read_audit(dir: &Path) -> Vec<AuditEvent> {
    std::fs::read_to_string(audit_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn count(events: &[AuditEvent], a: AuditAction, o: AuditOutcome) -> usize {
    events
        .iter()
        .filter(|e| e.action == a && e.outcome == o)
        .count()
}

/// Guest-visible content of the VM's current overlay (fake images).
fn overlay_content(dir: &Path, vm: VmId) -> Vec<u8> {
    let ov = dir.join("vms").join(vm.to_string()).join("overlays");
    let entries: Vec<_> = std::fs::read_dir(&ov).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "one overlay");
    fake_content(&entries[0].path()).unwrap()
}

/// Removes a test catalog (baseline files are read-only).
fn remove_catalog(dir: &Path) {
    fn writable(p: &Path) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    writable(&path);
                } else if let Ok(m) = e.metadata() {
                    let mut perm = m.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perm.set_readonly(false);
                    let _ = std::fs::set_permissions(&path, perm);
                }
            }
        }
    }
    for _ in 0..20 {
        writable(dir);
        if std::fs::remove_dir_all(dir).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn tmp_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("tmp")).map_or(true, |mut r| r.next().is_none())
}

#[tokio::test(flavor = "multi_thread")]
async fn create_restore_start_on_the_overlay_and_delete() {
    let svc = start_svc("f3-flow", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let mut ev = connect(&svc, &svc.ui).await;
    assert!(matches!(
        req(&mut ev, Request::Subscribe).await,
        ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }
    ));
    let vm = vm_with_disk(&mut c, &svc, "f3-vm", b"installed system").await;
    assert_eq!(list(&mut c, vm).await, vec![]);

    // Never ran: the baseline is verified.
    let b1 = created(&create_baseline(&mut c, vm, "clean install", false).await);
    assert!(b1.clean_shutdown && !b1.current && !b1.in_use);
    assert_eq!(b1.vm_id, vm);
    assert!(b1.size_bytes > 0);
    let st = status(&mut c, vm).await;
    assert_eq!(st.baselines.count, 1);
    assert_eq!(st.baselines.current, None);
    assert_eq!(st.baselines.operation, None);
    // Event for the UI.
    loop {
        let e = tokio::time::timeout(T, ev.next_event())
            .await
            .unwrap()
            .unwrap();
        if matches!(e, Ok(ServiceEvent::BaselinesChanged { vm_id }) if vm_id == vm) {
            break;
        }
    }

    // Start on the standalone disk, clean stop.
    assert_eq!(
        op_status(&submit(&mut c, vm, VmAction::Start).await).0,
        OperationStatus::Succeeded
    );
    assert_eq!(
        op_status(&submit(&mut c, vm, VmAction::Stop).await).0,
        OperationStatus::Succeeded
    );

    // Restore: the standalone disk is discarded, overlay of b1.
    match restore(&mut c, vm, b1.baseline_id).await {
        ServerMessage::Response {
            result: ResponseBody::BaselineRestored { vm_id, baseline_id },
            ..
        } => assert_eq!((vm_id, baseline_id), (vm, b1.baseline_id)),
        other => panic!("{other:?}"),
    }
    let vm_dir = svc.dir.join("vms").join(vm.to_string());
    assert!(!vm_dir.join("primary.qcow2").exists());
    assert_eq!(overlay_content(&svc.dir, vm), b"installed system");
    let st = status(&mut c, vm).await;
    assert_eq!(st.baselines.current, Some(b1.baseline_id));
    assert!(st.baselines.overlay_bytes > 0);

    // Start: the backend receives the verified files of the overlay.
    assert_eq!(
        op_status(&submit(&mut c, vm, VmAction::Start).await).0,
        OperationStatus::Succeeded
    );
    let calls = svc.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|c| c == "vm_start_disks"), "{calls:?}");
    assert!(calls.iter().any(|c| c.ends_with(":overlay")), "{calls:?}");
    // Not while running.
    let m = create_baseline(&mut c, vm, "while running", false).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    let m = restore(&mut c, vm, b1.baseline_id).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    assert_eq!(
        op_status(&submit(&mut c, vm, VmAction::Stop).await).0,
        OperationStatus::Succeeded
    );

    // Guest writes, baseline from the overlay: standalone, derived from b1.
    let ov_dir = vm_dir.join("overlays");
    let ov = std::fs::read_dir(&ov_dir)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let backing = vm_storage::qcow2_backing(&ov).unwrap().unwrap().0;
    std::fs::write(&ov, fake_qcow2(Some(&backing), b"app installed")).unwrap();
    let b2 = created(&create_baseline(&mut c, vm, "app installed", false).await);
    assert_eq!(b2.derived_from, Some(b1.baseline_id));

    // b1 in use: deletion refused; b2 deletable.
    let m = req(
        &mut c,
        Request::DeleteBaseline {
            vm_id: vm,
            baseline_id: b1.baseline_id,
        },
    )
    .await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    assert_eq!(detail(&m, "reason").as_deref(), Some("baseline_referenced"));
    let l = list(&mut c, vm).await;
    assert!(l[0].current && l[0].in_use && !l[1].in_use);
    // Restore b2, then delete b1.
    assert!(matches!(
        restore(&mut c, vm, b2.baseline_id).await,
        ServerMessage::Response { .. }
    ));
    assert_eq!(overlay_content(&svc.dir, vm), b"app installed");
    match req(
        &mut c,
        Request::DeleteBaseline {
            vm_id: vm,
            baseline_id: b1.baseline_id,
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::BaselineDeleted { baseline_id, .. },
            ..
        } => assert_eq!(baseline_id, b1.baseline_id),
        other => panic!("{other:?}"),
    }
    assert_eq!(list(&mut c, vm).await.len(), 1);
    assert!(tmp_empty(&svc.dir));

    // Audit: every storage operation with its outcome.
    let events = read_audit(&svc.dir);
    assert_eq!(
        count(
            &events,
            AuditAction::BaselineCreate,
            AuditOutcome::Succeeded
        ),
        2
    );
    assert_eq!(
        count(
            &events,
            AuditAction::BaselineRestore,
            AuditOutcome::Succeeded
        ),
        2
    );
    assert_eq!(
        count(
            &events,
            AuditAction::BaselineDelete,
            AuditOutcome::Succeeded
        ),
        1
    );
    assert_eq!(
        count(&events, AuditAction::BaselineDelete, AuditOutcome::Failed),
        1
    );
    assert!(count(&events, AuditAction::BaselineCreate, AuditOutcome::Denied) >= 1);
    // The VM can be deleted with its baselines and overlay.
    let rev = status(&mut c, vm).await.config_revision;
    assert!(matches!(
        req(
            &mut c,
            Request::DeleteVm {
                vm_id: vm,
                expected_revision: rev
            }
        )
        .await,
        ServerMessage::Response {
            result: ResponseBody::VmDeleted { .. },
            ..
        }
    ));
    assert!(!vm_dir.exists());
    svc.handle.shutdown().await;
    remove_catalog(&svc.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn unclean_stop_requires_explicit_consent() {
    let svc = start_svc("f3-unclean", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "f3-unclean", b"disk").await;
    op_status(&submit(&mut c, vm, VmAction::Start).await);
    assert_eq!(
        op_status(&submit(&mut c, vm, VmAction::ForceStop).await).0,
        OperationStatus::Succeeded
    );
    let m = create_baseline(&mut c, vm, "after a forced stop", false).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    assert_eq!(detail(&m, "reason").as_deref(), Some("unclean_shutdown"));
    let b = created(&create_baseline(&mut c, vm, "not verified", true).await);
    assert!(!b.clean_shutdown, "recorded as not verified");
    // A clean stop afterwards makes the next one verified.
    op_status(&submit(&mut c, vm, VmAction::Start).await);
    op_status(&submit(&mut c, vm, VmAction::Stop).await);
    let b2 = created(&create_baseline(&mut c, vm, "verified", false).await);
    assert!(b2.clean_shutdown);
    // The first one stays not verified.
    let l = list(&mut c, vm).await;
    assert!(
        !l.iter()
            .find(|x| x.baseline_id == b.baseline_id)
            .unwrap()
            .clean_shutdown
    );
    svc.handle.shutdown().await;
    remove_catalog(&svc.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn vm_is_busy_during_a_long_operation_and_the_emergency_stop_cancels_it() {
    let svc = start_svc("f3-cancel", Duration::from_secs(8)).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "f3-cancel", b"disk").await;
    let mut a = connect(&svc, &svc.ui).await;
    let long = tokio::spawn(async move {
        let m = create_baseline(&mut a, vm, "long", false).await;
        (a, m)
    });
    // The operation is visible and the VM busy.
    let t0 = std::time::Instant::now();
    loop {
        let st = status(&mut c, vm).await;
        if st.baselines.operation == Some(StorageOperation::CreateBaseline) {
            break;
        }
        assert!(t0.elapsed() < T, "operation not visible");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let m = submit(&mut c, vm, VmAction::Start).await;
    assert_eq!(code(&m), Some(ErrorCode::VmBusy), "{m:?}");
    let m = create_baseline(&mut c, vm, "second", false).await;
    assert_eq!(code(&m), Some(ErrorCode::VmBusy), "{m:?}");
    // Other requests are still served (the supervisor thread is free).
    assert!(matches!(
        req(&mut c, Request::ListVms).await,
        ServerMessage::Response { .. }
    ));
    let t_stop = std::time::Instant::now();
    assert!(matches!(
        req(&mut c, Request::EmergencyStop).await,
        ServerMessage::Response {
            result: ResponseBody::EmergencyStop(_),
            ..
        }
    ));
    let (_a, m) = tokio::time::timeout(T, long).await.unwrap().unwrap();
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    assert!(
        t_stop.elapsed() < Duration::from_secs(5),
        "cancelled well before the end of the convert"
    );
    // Nothing published, VM free again.
    assert_eq!(list(&mut c, vm).await, vec![]);
    let st = status(&mut c, vm).await;
    assert_eq!(st.baselines.operation, None);
    assert_eq!(st.state, VmState::Stopped);
    assert!(tmp_empty(&svc.dir));
    let events = read_audit(&svc.dir);
    assert_eq!(
        count(&events, AuditAction::BaselineCreate, AuditOutcome::Failed),
        1
    );
    svc.handle.shutdown().await;
    remove_catalog(&svc.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_baseline_refuses_the_start_and_the_vm_stays_stopped() {
    let svc = start_svc("f3-tamper", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "f3-tamper", b"disk").await;
    let b = created(&create_baseline(&mut c, vm, "one", false).await);
    assert!(matches!(
        restore(&mut c, vm, b.baseline_id).await,
        ServerMessage::Response { .. }
    ));
    let base = svc
        .dir
        .join("vms")
        .join(vm.to_string())
        .join("base")
        .join(b.baseline_id.to_string())
        .join("disk.qcow2");
    let mut p = std::fs::metadata(&base).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    p.set_readonly(false);
    std::fs::set_permissions(&base, p).unwrap();
    std::fs::write(&base, fake_qcow2(None, b"tampered")).unwrap();
    let (s, e) = op_status(&submit(&mut c, vm, VmAction::Start).await);
    assert_eq!(
        (s, e),
        (OperationStatus::Failed, Some(ErrorCode::StateConflict))
    );
    let st = status(&mut c, vm).await;
    assert_eq!(
        st.state,
        VmState::Stopped,
        "not failed: nothing was started"
    );
    let calls = svc.calls.lock().unwrap().clone();
    assert!(
        !calls.iter().any(|c| c.starts_with("vm_start")),
        "{calls:?}"
    );
    // Restore of the tampered baseline is refused too, nothing changes.
    let m = restore(&mut c, vm, b.baseline_id).await;
    assert_eq!(code(&m), Some(ErrorCode::StateConflict), "{m:?}");
    assert_eq!(detail(&m, "reason").as_deref(), Some("integrity"));
    assert!(tmp_empty(&svc.dir));
    svc.handle.shutdown().await;
    remove_catalog(&svc.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_requests_are_refused_for_the_pairing_role() {
    let svc = start_svc("f3-pairing", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "f3-pairing", b"disk").await;
    let cred = match req(&mut c, Request::CreatePairingCredential).await {
        ServerMessage::Response {
            result: ResponseBody::PairingCredential { credential, .. },
            ..
        } => Credential::from_hex(&credential).unwrap(),
        other => panic!("{other:?}"),
    };
    let mut p = tokio::time::timeout(T, Client::connect(svc.handle.endpoint()))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        p.hello(&cred).await.unwrap(),
        ServerMessage::HelloOk {
            role: Role::Pairing,
            ..
        }
    ));
    let b = BaselineId::new_v7();
    for r in [
        Request::ListBaselines { vm_id: vm },
        Request::CreateBaseline {
            vm_id: vm,
            label: label("x"),
            allow_unclean: true,
        },
        Request::RestoreBaseline {
            vm_id: vm,
            baseline_id: b,
        },
        Request::DeleteBaseline {
            vm_id: vm,
            baseline_id: b,
        },
    ] {
        let m = req(&mut p, r).await;
        assert_eq!(code(&m), Some(ErrorCode::PermissionDenied), "{m:?}");
    }
    // Unknown baseline for the UI: not found.
    let m = restore(&mut c, vm, b).await;
    assert_eq!(code(&m), Some(ErrorCode::NotFound), "{m:?}");
    svc.handle.shutdown().await;
    remove_catalog(&svc.dir);
    let _ = RunId::new_v7();
}

/// F6, disk full while taking a baseline (injected: `qemu-img convert`
/// writes part of its output and fails as on a full volume): clear
/// `resource_limit` naming the free space, no baseline recorded, no file
/// left behind (catalog `tmp` empty, nothing under `base`), the VM's disk
/// untouched and the VM still usable; with space back the same request
/// succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn baseline_on_a_full_volume_fails_cleanly() {
    let svc = start_svc("f6-full", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "f6-full", b"installed system").await;
    let vm_dir = svc.dir.join("vms").join(vm.to_string());
    svc.disk_full
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let m = create_baseline(&mut c, vm, "on a full volume", false).await;
    assert_eq!(code(&m), Some(ErrorCode::ResourceLimit), "{m:?}");
    match &m {
        ServerMessage::Error {
            error: ReplyError::Operational(e),
            ..
        } => assert!(e.message.contains("free space"), "{}", e.message),
        other => panic!("{other:?}"),
    }
    assert!(list(&mut c, vm).await.is_empty(), "no baseline recorded");
    assert!(tmp_empty(&svc.dir), "temporary output removed");
    let base_files = std::fs::read_dir(vm_dir.join("base"))
        .map(|r| r.flatten().count())
        .unwrap_or(0);
    assert_eq!(base_files, 0, "nothing published under base/");
    assert_eq!(
        fake_content(&vm_dir.join("primary.qcow2")).unwrap(),
        b"installed system"
    );
    let st = status(&mut c, vm).await;
    assert_eq!(st.state, VmState::Stopped);
    assert_eq!(st.baselines.operation, None);
    assert_eq!(
        count(
            &read_audit(&svc.dir),
            AuditAction::BaselineCreate,
            AuditOutcome::Failed
        ),
        1
    );
    // Space is back: the same request works.
    svc.disk_full
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let b = created(&create_baseline(&mut c, vm, "space back", false).await);
    assert_eq!(list(&mut c, vm).await.len(), 1);
    assert!(b.size_bytes > 0);
    svc.handle.shutdown().await;
    remove_catalog(&svc.dir);
}
