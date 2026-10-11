//! Start checks off the supervisor thread (fakes): the supervisor keeps
//! serving while a slow media verification runs; forced stop, Emergency
//! Stop and revocation cancel it; a tampered copy is refused; the media
//! handles live as long as the run.

use std::sync::atomic::AtomicU64;
use std::time::Duration;

use lightvm_proto::ipc::{ResponseBody, StartPhase};
use lightvm_proto::{
    Actor, AuditAction, AuditOutcome, Caller, ErrorCode, Label, ManagedRelPath, MediaEntry,
    MediaId, MediaKind, MediaRecord, OperationId, OperationRecord, OperationStatus, ReplyError,
    Sha256Digest, UtcTimestamp, VmAction, VmId, VmRequest, VmState,
};
use tokio::sync::{Semaphore, broadcast, oneshot, watch};
use vm_core::ports::CatalogStore as _;

use super::*;
use crate::engine::{CoreEngine, VmEngine, core_config};
use crate::fakes::{
    Calls, FakeHost, FakeInspector, FakeVmBackend, Gate, MemoryAudit, MemoryStore, host_capacity,
    sample_config,
};

type ReplyRx = oneshot::Receiver<Result<ResponseBody, ReplyError>>;

/// Size declared for the fake ISO (the real one of the F2 runs).
const ISO_BYTES: u64 = 6_400_000_000;

struct Fx {
    sup: SupervisorThread,
    rx: mpsc::Receiver<Command>,
    vm: VmId,
    calls: Calls,
    audit: MemoryAudit,
    gate: Arc<Gate>,
    tampered: Arc<AtomicBool>,
}

/// Supervisor with an engine whose VM has an installation ISO; the fake
/// "hash" of the ISO waits for `gate`. Offload on, as in `run`.
fn fixture() -> Fx {
    let calls = Calls::default();
    let audit = MemoryAudit::default();
    let gate = Gate::closed();
    let tampered = Arc::new(AtomicBool::new(false));
    let backend = FakeVmBackend {
        calls: calls.clone(),
        ..FakeVmBackend::default()
    };
    let inspector = FakeInspector {
        gate: Some(gate.clone()),
        tampered: tampered.clone(),
        calls: calls.clone(),
        ..FakeInspector::default()
    };
    let entry = MediaEntry {
        record: MediaRecord {
            id: MediaId::new_v7(),
            kind: MediaKind::InstallIso,
            sha256: Sha256Digest::from_bytes([7; 32]),
            size_bytes: ISO_BYTES,
            declared_origin: Label::parse("test").unwrap(),
            approved_at: UtcTimestamp::now(),
        },
        managed_path: ManagedRelPath::parse("media/x.iso").unwrap(),
        file_identity: None,
    };
    let mut store = MemoryStore::default();
    store.insert_media(&entry).unwrap();
    let mut e = CoreEngine::new(
        store,
        audit.clone(),
        backend,
        inspector,
        core_config(host_capacity()),
    )
    .unwrap();
    let mut cfg = sample_config("iso");
    cfg.install_media = Some(entry.record.id);
    let vm = e.create_vm(&Caller::User, cfg).unwrap().id;
    let _ = e.state_changes();
    let (tx, rx) = mpsc::channel();
    let (events, _) = broadcast::channel(256);
    let mut sup = SupervisorThread {
        engine: Box::new(e),
        backend: Box::new(FakeHost::default()),
        events,
        max_frame: 1 << 20,
        emergency_epoch: Arc::new(AtomicU64::new(0)),
        parent_loss_policy: Arc::new(Mutex::new(ParentLossPolicy::default())),
        vms_with_process: Arc::new(AtomicUsize::new(0)),
        display: Arc::new(DisplayRegistry::new(
            std::env::temp_dir().join("lvt-start-checks"),
        )),
        self_tx: tx,
        imports: 0,
        storage_jobs: 0,
        storage_cancel: Arc::default(),
        start_jobs: 0,
    };
    sup.engine.offload_start_checks(true);
    Fx {
        sup,
        rx,
        vm,
        calls,
        audit,
        gate,
        tampered,
    }
}

fn request(
    req: SupervisorRequest,
    revoked: watch::Receiver<bool>,
    epoch: u64,
) -> (Command, ReplyRx) {
    let (tx, rx) = oneshot::channel();
    let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
    (
        Command::Request {
            req,
            caller: Some(Caller::User),
            connection_id: 1,
            revoked,
            epoch,
            reply: tx,
            _permit: permit,
        },
        rx,
    )
}

fn submit_req(vm: VmId, action: VmAction) -> SupervisorRequest {
    SupervisorRequest::SubmitVm(Box::new(VmRequest {
        operation_id: OperationId::new_v7(),
        vm_id: vm,
        expected_run_id: None,
        action,
    }))
}

fn serve(fx: &mut Fx, cmd: Command) {
    let mut slot = None;
    let _ = fx.sup.serve(cmd, &fx.rx, &mut slot);
    fx.sup.publish_state_changes();
}

fn reply(mut r: ReplyRx) -> Result<ResponseBody, ReplyError> {
    for _ in 0..1000 {
        match r.try_recv() {
            Ok(v) => return v,
            Err(oneshot::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("no reply: {e:?}"),
        }
    }
    panic!("reply timeout")
}

fn pending(r: &mut ReplyRx) -> bool {
    matches!(r.try_recv(), Err(oneshot::error::TryRecvError::Empty))
}

fn record(r: Result<ResponseBody, ReplyError>) -> OperationRecord {
    match r {
        Ok(ResponseBody::Operation {
            operation: Some(op),
            ..
        }) => op,
        other => panic!("{other:?}"),
    }
}

/// Serves the `FinishStart` sent by the start worker.
fn finish(fx: &mut Fx) {
    let cmd = fx
        .rx
        .recv_timeout(Duration::from_secs(10))
        .expect("start checks ended");
    assert!(matches!(cmd, Command::FinishStart { .. }), "{cmd:?}");
    serve(fx, cmd);
}

fn calls(fx: &Fx) -> Vec<String> {
    fx.calls.lock().unwrap().clone()
}

fn launched(fx: &Fx) -> bool {
    calls(fx).iter().any(|c| c.starts_with("vm_start:"))
}

fn state(fx: &mut Fx) -> VmState {
    fx.sup.engine.vm_status(&Caller::User, fx.vm).unwrap().state
}

#[test]
fn the_supervisor_serves_other_requests_while_the_media_is_verified() {
    let mut fx = fixture();
    let (_alive, rev) = watch::channel(false);
    let (cmd, mut r) = request(submit_req(fx.vm, VmAction::Start), rev.clone(), 0);
    serve(&mut fx, cmd);
    assert!(pending(&mut r), "the start reply waits for the checks");
    assert_eq!(calls(&fx), vec!["media_open".to_owned()]);
    // Status, list and ping are served at once, with the sub-status.
    for _ in 0..5 {
        let t0 = Instant::now();
        let (cmd, s) = request(SupervisorRequest::VmStatus(fx.vm), rev.clone(), 0);
        serve(&mut fx, cmd);
        match reply(s) {
            Ok(ResponseBody::Vm { vm }) => {
                assert_eq!(vm.state, VmState::Starting);
                let p = vm.start.expect("start progress");
                assert_eq!(p.phase, StartPhase::VerifyingMedia);
                assert_eq!(p.total_bytes, ISO_BYTES);
                assert_eq!(p.verified_bytes, 0);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "{:?}",
            t0.elapsed()
        );
    }
    let (cmd, l) = request(SupervisorRequest::ListVms, rev.clone(), 0);
    serve(&mut fx, cmd);
    assert!(matches!(reply(l), Ok(ResponseBody::VmList { vms }) if vms[0].start.is_some()));
    assert!(pending(&mut r));
    assert!(!launched(&fx));
    // The hash ends: QEMU is launched with the media handles, the reply
    // goes out with the final record.
    fx.gate.open();
    finish(&mut fx);
    let op = record(reply(r));
    assert_eq!(op.status, OperationStatus::Succeeded, "{op:?}");
    assert_eq!(state(&mut fx), VmState::Running);
    let st = fx.sup.engine.vm_status(&Caller::User, fx.vm).unwrap();
    assert_eq!(st.start, None, "no sub-status once running");
    let c = calls(&fx);
    let media = c.iter().position(|x| x == "vm_start_media:1").unwrap();
    let start = c.iter().position(|x| x == "vm_start:iso:media=1").unwrap();
    assert!(media < start, "{c:?}");
    // The handles are kept for the whole run, dropped when it ends.
    assert!(!c.iter().any(|x| x == "media_guard_dropped"), "{c:?}");
    let (cmd, f) = request(submit_req(fx.vm, VmAction::ForceStop), rev, 0);
    serve(&mut fx, cmd);
    assert_eq!(record(reply(f)).status, OperationStatus::Succeeded);
    assert!(calls(&fx).iter().any(|x| x == "media_guard_dropped"));
    assert_eq!(fx.sup.start_jobs, 0);
}

#[test]
fn the_supervisor_loop_answers_status_quickly_during_a_slow_verification() {
    let fx = fixture();
    let (vm, gate) = (fx.vm, fx.gate.clone());
    let tx = fx.sup.self_tx.clone();
    let sup = fx.sup;
    let rx = fx.rx;
    let t = std::thread::spawn(move || sup.run(rx));
    let (_alive, rev) = watch::channel(false);
    let (cmd, mut r) = request(submit_req(vm, VmAction::Start), rev.clone(), 0);
    tx.send(cmd).unwrap();
    let mut worst = Duration::ZERO;
    for _ in 0..20 {
        let t0 = Instant::now();
        let (cmd, s) = request(SupervisorRequest::VmStatus(vm), rev.clone(), 0);
        tx.send(cmd).unwrap();
        match reply(s) {
            Ok(ResponseBody::Vm { vm }) => assert_eq!(vm.state, VmState::Starting),
            other => panic!("{other:?}"),
        }
        worst = worst.max(t0.elapsed());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(worst < Duration::from_millis(500), "{worst:?}");
    assert!(pending(&mut r));
    gate.open();
    assert_eq!(record(reply(r)).status, OperationStatus::Succeeded);
    let (done, d) = oneshot::channel();
    tx.send(Command::Shutdown { done }).unwrap();
    let _ = d.blocking_recv();
    t.join().unwrap();
}

#[test]
fn a_forced_stop_cancels_the_verification() {
    let mut fx = fixture();
    let (_alive, rev) = watch::channel(false);
    let (cmd, r) = request(submit_req(fx.vm, VmAction::Start), rev.clone(), 0);
    serve(&mut fx, cmd);
    let (cmd, f) = request(submit_req(fx.vm, VmAction::ForceStop), rev, 0);
    serve(&mut fx, cmd);
    assert_eq!(record(reply(f)).status, OperationStatus::Succeeded);
    assert_eq!(state(&mut fx), VmState::Stopped);
    finish(&mut fx);
    let op = record(reply(r));
    assert_eq!(
        (op.status, op.error),
        (OperationStatus::Failed, Some(ErrorCode::StateConflict))
    );
    assert_eq!(state(&mut fx), VmState::Stopped);
    let c = calls(&fx);
    assert!(c.iter().any(|x| x == "media_hash_cancelled"), "{c:?}");
    assert!(c.iter().any(|x| x == "media_guard_dropped"), "{c:?}");
    assert!(!launched(&fx));
    // A new start works afterwards.
    fx.gate.open();
    let (_alive, rev) = watch::channel(false);
    let (cmd, r) = request(submit_req(fx.vm, VmAction::Start), rev, 0);
    serve(&mut fx, cmd);
    finish(&mut fx);
    assert_eq!(record(reply(r)).status, OperationStatus::Succeeded);
    assert_eq!(state(&mut fx), VmState::Running);
}

#[test]
fn an_emergency_stop_cancels_the_verification() {
    let mut fx = fixture();
    let (_alive, rev) = watch::channel(false);
    let (cmd, r) = request(submit_req(fx.vm, VmAction::Start), rev, 0);
    serve(&mut fx, cmd);
    // Priority path done (no process yet), then recorded on the thread.
    fx.sup.emergency_epoch.store(1, Ordering::SeqCst);
    serve(&mut fx, Command::EmergencyReconcile(Actor::User));
    assert_eq!(state(&mut fx), VmState::Stopped);
    finish(&mut fx);
    let op = record(reply(r));
    assert_eq!(op.status, OperationStatus::Failed);
    assert!(!launched(&fx));
    assert!(calls(&fx).iter().any(|x| x == "media_hash_cancelled"));
    assert_eq!(state(&mut fx), VmState::Stopped);
}

#[test]
fn revoking_the_connection_cancels_the_verification() {
    let mut fx = fixture();
    let (alive, rev) = watch::channel(false);
    let (cmd, r) = request(submit_req(fx.vm, VmAction::Start), rev, 0);
    serve(&mut fx, cmd);
    alive.send(true).unwrap();
    finish(&mut fx);
    let op = record(reply(r));
    assert_eq!(
        (op.status, op.error),
        (OperationStatus::Failed, Some(ErrorCode::Unauthenticated))
    );
    assert_eq!(state(&mut fx), VmState::Stopped);
    assert!(!launched(&fx));
    assert_eq!(
        fx.audit
            .events()
            .iter()
            .filter(|e| e.action == AuditAction::VmStart && e.outcome == AuditOutcome::Failed)
            .count(),
        1
    );
}

#[test]
fn a_copy_tampered_during_the_verification_is_refused() {
    let mut fx = fixture();
    let (_alive, rev) = watch::channel(false);
    let (cmd, r) = request(submit_req(fx.vm, VmAction::Start), rev, 0);
    serve(&mut fx, cmd);
    // The bytes read by the hash differ from the registry.
    fx.tampered.store(true, Ordering::SeqCst);
    fx.gate.open();
    finish(&mut fx);
    let op = record(reply(r));
    assert_eq!(
        (op.status, op.error),
        (OperationStatus::Failed, Some(ErrorCode::PermissionDenied))
    );
    assert_eq!(state(&mut fx), VmState::Failed);
    assert!(!launched(&fx));
    assert!(calls(&fx).iter().any(|x| x == "media_guard_dropped"));
}

#[test]
fn the_shutdown_cancels_the_verification() {
    let fx = fixture();
    let (vm, calls_log) = (fx.vm, fx.calls.clone());
    let tx = fx.sup.self_tx.clone();
    let sup = fx.sup;
    let rx = fx.rx;
    let t = std::thread::spawn(move || sup.run(rx));
    let (_alive, rev) = watch::channel(false);
    let (cmd, r) = request(submit_req(vm, VmAction::Start), rev, 0);
    tx.send(cmd).unwrap();
    // Wait until the checks are running.
    for _ in 0..200 {
        if calls_log.lock().unwrap().iter().any(|c| c == "media_open") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let t0 = Instant::now();
    let (done, d) = oneshot::channel();
    tx.send(Command::Shutdown { done }).unwrap();
    let _ = d.blocking_recv();
    t.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    let op = record(reply(r));
    assert_eq!(op.status, OperationStatus::Failed);
    let c = calls_log.lock().unwrap().clone();
    assert!(!c.iter().any(|x| x.starts_with("vm_start:")), "{c:?}");
}
