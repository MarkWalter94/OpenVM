//! User snapshots through the real IPC (ADR-0032): take with the VM off and
//! running, revert (VM off only: disk, UEFI variables and configuration),
//! delete with re-parenting, rename, thumbnail, promotion to a baseline,
//! failures and cancellation settled from the disk, refusals (agent role,
//! baseline restore while snapshots exist). Real core and SQLite catalog;
//! fake VM backend and image tool working on a fake QCOW2 disk with a real
//! snapshot table (`fakes::snapshot_disk`), no QEMU.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lightvm_proto::ipc::{
    Request, ResponseBody, ServerMessage, ServiceEvent, SnapshotSummary, SnapshotTree, VmSummary,
};
use lightvm_proto::{
    AuditAction, AuditEvent, AuditOutcome, ErrorCode, Label, OperationId, ReplyError, SnapshotId,
    VmAction, VmId, VmRequest, VmState,
};
use lightvm_service::adapters::audit::FileAuditSink;
use lightvm_service::adapters::catalog::{ManagedMediaInspector, SqliteStore};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use lightvm_service::engine::{CoreEngine, core_config};
use lightvm_service::fakes::{
    Calls, FakeHost, FakeImageTool, FakeVmBackend, fake_content, host_capacity, sample_config,
    snapshot_disk,
};
use lightvm_service::test_support::temp_dir;
use lightvm_service::{ServiceConfig, ServiceHandle, start};

const T: Duration = Duration::from_secs(20);

struct Svc {
    handle: ServiceHandle,
    ui: Credential,
    calls: Calls,
    fail: Arc<AtomicBool>,
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

async fn start_svc(name: &str, delay: Duration) -> Svc {
    let dir = temp_dir(name);
    let store = SqliteStore::open(&dir).unwrap();
    let audit = FileAuditSink::open(&audit_path(&dir)).unwrap();
    let calls = Calls::default();
    let fail = Arc::new(AtomicBool::new(false));
    let backend = FakeVmBackend {
        calls: calls.clone(),
        image_tool: Some(Arc::new(FakeImageTool {
            calls: calls.clone(),
            snapshot_fail: fail.clone(),
            snapshot_delay: delay,
            ..FakeImageTool::default()
        })),
        snapshot_root: Some(dir.clone()),
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
    cfg.display_dir = Some(short_dir("lvs"));
    let (handle, ui) = start(cfg, Box::new(engine), Box::new(host))
        .await
        .expect("start");
    Svc {
        handle,
        ui,
        calls,
        fail,
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

fn error(m: &ServerMessage) -> (ErrorCode, String, Option<String>) {
    match m {
        ServerMessage::Error {
            error: ReplyError::Operational(p),
            ..
        } => (
            p.code,
            p.message.clone(),
            p.details.as_ref().and_then(|d| d.get("reason")).cloned(),
        ),
        other => panic!("expected an error: {other:?}"),
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

async fn status(c: &mut Client, vm_id: VmId) -> VmSummary {
    match req(c, Request::VmStatus { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Vm { vm },
            ..
        } => vm,
        other => panic!("{other:?}"),
    }
}

async fn wait_state(c: &mut Client, vm: VmId, want: VmState) {
    for _ in 0..200 {
        if status(c, vm).await.state == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("VM not {want:?}");
}

fn disk(dir: &Path, vm: VmId) -> PathBuf {
    dir.join("vms").join(vm.to_string()).join("primary.qcow2")
}

fn vars(dir: &Path, vm: VmId) -> PathBuf {
    dir.join("vms")
        .join(vm.to_string())
        .join("firmware")
        .join("primary.vars.fd")
}

/// New VM with its disk (fake QCOW2 with a snapshot table) and UEFI
/// variables, as after a first start.
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
        } => vm.id,
        other => panic!("{other:?}"),
    };
    snapshot_disk::create(&disk(&svc.dir, vm), content).unwrap();
    std::fs::create_dir_all(vars(&svc.dir, vm).parent().unwrap()).unwrap();
    std::fs::write(vars(&svc.dir, vm), b"VARS 1").unwrap();
    vm
}

fn label(s: &str) -> Label {
    Label::parse(s).unwrap()
}

async fn take(c: &mut Client, vm_id: VmId, name: &str) -> ServerMessage {
    req(
        c,
        Request::TakeSnapshot {
            vm_id,
            name: label(name),
            description: format!("{name} description"),
        },
    )
    .await
}

fn taken(m: &ServerMessage) -> SnapshotSummary {
    match m {
        ServerMessage::Response {
            result: ResponseBody::Snapshot { snapshot },
            ..
        } => snapshot.clone(),
        other => panic!("{other:?}"),
    }
}

async fn tree(c: &mut Client, vm_id: VmId) -> SnapshotTree {
    match req(c, Request::ListSnapshots { vm_id }).await {
        ServerMessage::Response {
            result: ResponseBody::Snapshots { tree, .. },
            ..
        } => tree,
        other => panic!("{other:?}"),
    }
}

async fn revert(c: &mut Client, vm_id: VmId, snapshot_id: SnapshotId) -> ServerMessage {
    req(c, Request::RevertSnapshot { vm_id, snapshot_id }).await
}

async fn delete(c: &mut Client, vm_id: VmId, snapshot_id: SnapshotId) -> ServerMessage {
    req(c, Request::DeleteSnapshot { vm_id, snapshot_id }).await
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

fn tmp_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("tmp")).map_or(true, |mut r| r.next().is_none())
}

#[tokio::test(flavor = "multi_thread")]
async fn take_off_and_running_revert_delete_rename() {
    let svc = start_svc("snap-flow", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let mut ev = connect(&svc, &svc.ui).await;
    assert!(matches!(
        req(&mut ev, Request::Subscribe).await,
        ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }
    ));
    let vm = vm_with_disk(&mut c, &svc, "snap-vm", b"state A").await;
    let t = tree(&mut c, vm).await;
    assert!(t.available, "{t:?}");
    assert!(t.snapshots.is_empty() && t.current.is_none());

    // Off: consistent snapshot `a`.
    let a = taken(&take(&mut c, vm, "a").await);
    assert!(!a.taken_while_running && a.clean_shutdown && a.firmware_saved);
    assert_eq!(a.description, "a description");
    assert_eq!(a.disk_gib, 16);
    assert!(!a.has_thumbnail);
    loop {
        let e = tokio::time::timeout(T, ev.next_event())
            .await
            .unwrap()
            .unwrap();
        if matches!(e, Ok(ServiceEvent::SnapshotsChanged { vm_id }) if vm_id == vm) {
            break;
        }
    }
    let st = status(&mut c, vm).await;
    assert_eq!(st.snapshots.count, 1);
    assert_eq!(st.snapshots.current, Some(a.snapshot_id));
    assert!(
        svc.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "snapshot_create")
    );

    // Running: disk-only snapshot `b` through "QMP", with a thumbnail.
    assert!(matches!(
        submit(&mut c, vm, VmAction::Start).await,
        ServerMessage::Response { .. }
    ));
    wait_state(&mut c, vm, VmState::Running).await;
    snapshot_disk::guest_write(&disk(&svc.dir, vm), b"state B").unwrap();
    std::fs::write(vars(&svc.dir, vm), b"VARS 2").unwrap();
    let b = taken(&take(&mut c, vm, "b").await);
    assert!(b.taken_while_running && !b.clean_shutdown);
    assert_eq!(b.parent, Some(a.snapshot_id));
    assert!(b.has_thumbnail);
    assert!(
        svc.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "vm_snapshot_disk")
    );
    match req(
        &mut c,
        Request::GetSnapshotThumbnail {
            vm_id: vm,
            snapshot_id: b.snapshot_id,
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::SnapshotThumbnail { png_base64, .. },
            ..
        } => assert_eq!(
            lightvm_proto::ipc::base64_decode(&png_base64.unwrap()).unwrap(),
            b"\x89PNG fake thumbnail"
        ),
        other => panic!("{other:?}"),
    }
    // Revert refused while running: never a hidden shutdown.
    let (code, msg, _) = error(&revert(&mut c, vm, a.snapshot_id).await);
    assert_eq!(code, ErrorCode::StateConflict);
    assert!(msg.contains("shut it down"), "{msg}");

    // Stop, change the configuration, then revert to `a`.
    snapshot_disk::guest_write(&disk(&svc.dir, vm), b"state C").unwrap();
    assert!(matches!(
        submit(&mut c, vm, VmAction::Stop).await,
        ServerMessage::Response { .. }
    ));
    wait_state(&mut c, vm, VmState::Stopped).await;
    std::fs::write(vars(&svc.dir, vm), b"VARS 3").unwrap();
    let st = status(&mut c, vm).await;
    let mut cfg = st.config.clone();
    cfg.vcpus = 4;
    let upd = req(
        &mut c,
        Request::UpdateVm {
            vm_id: vm,
            expected_revision: st.config_revision,
            config: cfg,
        },
    )
    .await;
    assert!(matches!(upd, ServerMessage::Response { .. }), "{upd:?}");
    let rev_before = status(&mut c, vm).await.config_revision;
    match revert(&mut c, vm, a.snapshot_id).await {
        ServerMessage::Response {
            result:
                ResponseBody::SnapshotReverted {
                    snapshot_id,
                    config_revision,
                    ..
                },
            ..
        } => {
            assert_eq!(snapshot_id, a.snapshot_id);
            assert_eq!(config_revision, rev_before + 1);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        snapshot_disk::content(&disk(&svc.dir, vm)).unwrap(),
        b"state A"
    );
    assert_eq!(std::fs::read(vars(&svc.dir, vm)).unwrap(), b"VARS 1");
    let st = status(&mut c, vm).await;
    assert_eq!(st.config.vcpus, 2, "the snapshot's configuration is back");
    assert_eq!(st.config_revision, rev_before + 1);
    assert_eq!(st.snapshots.current, Some(a.snapshot_id));
    // The VM starts again from the reverted state.
    assert!(matches!(
        submit(&mut c, vm, VmAction::Start).await,
        ServerMessage::Response { .. }
    ));
    wait_state(&mut c, vm, VmState::Running).await;

    // `c` while running, child of `a`; delete `a`: `b` and `c` become roots.
    let cs = taken(&take(&mut c, vm, "c").await);
    assert_eq!(cs.parent, Some(a.snapshot_id));
    assert!(matches!(
        delete(&mut c, vm, a.snapshot_id).await,
        ServerMessage::Response {
            result: ResponseBody::SnapshotDeleted { .. },
            ..
        }
    ));
    assert!(
        svc.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "vm_delete_disk_snapshot")
    );
    let t = tree(&mut c, vm).await;
    let parents: Vec<_> = t
        .snapshots
        .iter()
        .map(|s| (s.name.as_str().to_owned(), s.parent))
        .collect();
    assert_eq!(parents, vec![("b".into(), None), ("c".into(), None)]);
    assert_eq!(t.current, Some(cs.snapshot_id));
    assert_eq!(
        snapshot_disk::tags(&disk(&svc.dir, vm)).unwrap(),
        vec![b.snapshot_id.to_string(), cs.snapshot_id.to_string()]
    );

    // Rename and description; invalid description refused.
    match req(
        &mut c,
        Request::UpdateSnapshot {
            vm_id: vm,
            snapshot_id: b.snapshot_id,
            name: label("before the update"),
            description: "line 1\nline 2".into(),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::Snapshot { snapshot },
            ..
        } => {
            assert_eq!(snapshot.name.as_str(), "before the update");
            assert_eq!(snapshot.description, "line 1\nline 2");
        }
        other => panic!("{other:?}"),
    }
    let (code, _, _) = error(
        &req(
            &mut c,
            Request::UpdateSnapshot {
                vm_id: vm,
                snapshot_id: b.snapshot_id,
                name: label("x"),
                description: "bell \u{7}".into(),
            },
        )
        .await,
    );
    assert_eq!(code, ErrorCode::InvalidArgument);

    let events = read_audit(&svc.dir);
    assert_eq!(
        count(&events, AuditAction::SnapshotTake, AuditOutcome::Succeeded),
        3
    );
    assert_eq!(
        count(
            &events,
            AuditAction::SnapshotRevert,
            AuditOutcome::Succeeded
        ),
        1
    );
    assert_eq!(
        count(&events, AuditAction::SnapshotRevert, AuditOutcome::Denied),
        1
    );
    assert_eq!(
        count(
            &events,
            AuditAction::SnapshotDelete,
            AuditOutcome::Succeeded
        ),
        1
    );
    assert_eq!(
        count(
            &events,
            AuditAction::SnapshotUpdate,
            AuditOutcome::Succeeded
        ),
        1
    );
    assert!(tmp_empty(&svc.dir));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_tool_and_cancellation_leave_a_consistent_state() {
    let svc = start_svc("snap-fail", Duration::from_millis(600)).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "snap-fail", b"state A").await;
    let a = taken(&take(&mut c, vm, "a").await);
    snapshot_disk::guest_write(&disk(&svc.dir, vm), b"state B").unwrap();

    // `qemu-img` fails: nothing changes, the VM is free again.
    svc.fail.store(true, Ordering::SeqCst);
    let (code, _, _) = error(&take(&mut c, vm, "x").await);
    assert_eq!(code, ErrorCode::InfrastructureError);
    let (code, _, _) = error(&revert(&mut c, vm, a.snapshot_id).await);
    assert_eq!(code, ErrorCode::InfrastructureError);
    assert_eq!(
        snapshot_disk::content(&disk(&svc.dir, vm)).unwrap(),
        b"state B"
    );
    let (code, _, _) = error(&delete(&mut c, vm, a.snapshot_id).await);
    assert_eq!(code, ErrorCode::InfrastructureError);
    svc.fail.store(false, Ordering::SeqCst);
    let t = tree(&mut c, vm).await;
    assert_eq!(t.snapshots.len(), 1);
    assert_eq!(status(&mut c, vm).await.baselines.operation, None);

    // Emergency Stop during the disk operation: cancelled before it ran.
    let mut c2 = connect(&svc, &svc.ui).await;
    let pending = tokio::spawn(async move { revert(&mut c2, vm, a.snapshot_id).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let st = status(&mut c, vm).await;
    assert_eq!(
        st.baselines.operation,
        Some(lightvm_proto::ipc::StorageOperation::RevertSnapshot)
    );
    // Busy meanwhile.
    let (code, _, _) = error(&take(&mut c, vm, "busy").await);
    assert_eq!(code, ErrorCode::VmBusy);
    assert!(matches!(
        req(&mut c, Request::EmergencyStop).await,
        ServerMessage::Response { .. }
    ));
    let r = pending.await.unwrap();
    let (code, _, _) = error(&r);
    assert_eq!(code, ErrorCode::StateConflict);
    assert_eq!(
        snapshot_disk::content(&disk(&svc.dir, vm)).unwrap(),
        b"state B",
        "cancelled before the disk changed"
    );
    assert_eq!(tree(&mut c, vm).await.snapshots.len(), 1);
    assert!(tmp_empty(&svc.dir));
    let events = read_audit(&svc.dir);
    assert_eq!(
        count(&events, AuditAction::SnapshotTake, AuditOutcome::Failed),
        1
    );
    assert_eq!(
        count(&events, AuditAction::SnapshotRevert, AuditOutcome::Failed),
        2
    );
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn promote_to_baseline_and_restore_refused_with_snapshots() {
    let svc = start_svc("snap-promote", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = vm_with_disk(&mut c, &svc, "snap-promote", b"state A").await;
    let a = taken(&take(&mut c, vm, "a").await);
    snapshot_disk::guest_write(&disk(&svc.dir, vm), b"state B").unwrap();
    let baseline = match req(
        &mut c,
        Request::PromoteSnapshot {
            vm_id: vm,
            snapshot_id: a.snapshot_id,
            label: label("from snapshot a"),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::BaselineCreated { baseline },
            ..
        } => baseline,
        other => panic!("{other:?}"),
    };
    assert!(baseline.clean_shutdown);
    assert_eq!(baseline.label.as_str(), "from snapshot a");
    let base_disk = svc
        .dir
        .join("vms")
        .join(vm.to_string())
        .join("base")
        .join(baseline.baseline_id.to_string())
        .join("disk.qcow2");
    assert_eq!(fake_content(&base_disk).unwrap(), b"state A");
    let base_vars = base_disk.with_file_name("vars.fd");
    assert_eq!(std::fs::read(base_vars).unwrap(), b"VARS 1");
    // The VM itself is unchanged.
    assert_eq!(
        snapshot_disk::content(&disk(&svc.dir, vm)).unwrap(),
        b"state B"
    );
    // Restoring a baseline would discard the disk holding the snapshots.
    let (code, msg, _) = error(
        &req(
            &mut c,
            Request::RestoreBaseline {
                vm_id: vm,
                baseline_id: baseline.baseline_id,
            },
        )
        .await,
    );
    assert_eq!(code, ErrorCode::StateConflict, "{msg}");
    assert!(msg.contains("snapshots"), "{msg}");
    // A snapshot taken while running cannot become a baseline.
    assert!(matches!(
        submit(&mut c, vm, VmAction::Start).await,
        ServerMessage::Response { .. }
    ));
    wait_state(&mut c, vm, VmState::Running).await;
    let b = taken(&take(&mut c, vm, "b").await);
    assert!(matches!(
        submit(&mut c, vm, VmAction::Stop).await,
        ServerMessage::Response { .. }
    ));
    wait_state(&mut c, vm, VmState::Stopped).await;
    let (code, msg, _) = error(
        &req(
            &mut c,
            Request::PromoteSnapshot {
                vm_id: vm,
                snapshot_id: b.snapshot_id,
                label: label("no"),
            },
        )
        .await,
    );
    assert_eq!(code, ErrorCode::StateConflict);
    assert!(msg.contains("running"), "{msg}");
    // Deleting every snapshot makes the restore possible again.
    for s in [a.snapshot_id, b.snapshot_id] {
        assert!(matches!(
            delete(&mut c, vm, s).await,
            ServerMessage::Response { .. }
        ));
    }
    let restored = req(
        &mut c,
        Request::RestoreBaseline {
            vm_id: vm,
            baseline_id: baseline.baseline_id,
        },
    )
    .await;
    assert!(
        matches!(restored, ServerMessage::Response { .. }),
        "{restored:?}"
    );
    // On an overlay of a baseline, snapshots are unavailable.
    let t = tree(&mut c, vm).await;
    assert!(!t.available);
    assert!(
        t.unavailable_reason.unwrap().contains("overlay"),
        "reason names the overlay"
    );
    let (code, _, reason) = error(&take(&mut c, vm, "x").await);
    assert_eq!(code, ErrorCode::StateConflict);
    assert_eq!(reason.as_deref(), Some("snapshots_unavailable"));
    svc.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn vm_without_disk_has_no_snapshots_yet() {
    let svc = start_svc("snap-nodisk", Duration::ZERO).await;
    let mut c = connect(&svc, &svc.ui).await;
    let vm = match req(
        &mut c,
        Request::CreateVm {
            config: sample_config("snap-nodisk"),
        },
    )
    .await
    {
        ServerMessage::Response {
            result: ResponseBody::VmCreated { vm },
            ..
        } => vm.id,
        other => panic!("{other:?}"),
    };
    let t = tree(&mut c, vm).await;
    assert!(!t.available);
    let (code, msg, _) = error(&take(&mut c, vm, "x").await);
    assert_eq!(code, ErrorCode::StateConflict);
    assert!(msg.contains("start it once"), "{msg}");
    svc.handle.shutdown().await;
}
