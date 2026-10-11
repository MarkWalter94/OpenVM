//! Supervisor loop with the real core and the fake backend: panic
//! containment, revocation of queued requests, console tickets bound to the
//! run, observation deadline, cancellation of media imports, geometry
//! refresh, graceful stop timeout and Emergency Stop during a graceful stop.

use std::sync::atomic::AtomicU64;
use std::time::Duration;

use std::path::PathBuf;

use lightvm_proto::ipc::{ResponseBody, ServiceEvent};
use lightvm_proto::{
    Actor, AuditAction, AuditOutcome, BackendObservation, Caller, DisplayGeometry, ErrorCode,
    HostCapacity, InputBatch, InputEvent, MediaKind, MediaRecord, OperationId, PointerMove,
    PublicError, ReplyError, RunId, SessionId, TerminationCause, VmAction, VmConfig, VmId,
    VmRecord, VmRequest, VmState,
};
use tokio::sync::{Semaphore, broadcast, oneshot, watch};

use super::*;
use crate::engine::{ConsoleTarget, ImportCopied, ImportJob, SubmitOutcome, VmEngine};
use crate::fakes::{
    Calls, FAKE_GEOMETRY, FakeHost, FakeVmBackend, MemoryAudit, MemoryEngine, StopResults,
    sample_engine,
};

type Observations = Arc<Mutex<Vec<(VmId, BackendObservation)>>>;
type ReplyRx = oneshot::Receiver<Result<ResponseBody, ReplyError>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Parts {
    calls: Calls,
    audit: MemoryAudit,
    observations: Observations,
    stops: StopResults,
    killed: Arc<Mutex<std::collections::BTreeSet<VmId>>>,
}

fn engine(async_stop: bool) -> (MemoryEngine, Parts) {
    let calls = Calls::default();
    let audit = MemoryAudit::default();
    let backend = FakeVmBackend {
        calls: calls.clone(),
        display_socket: Some(std::env::temp_dir().join("lightvm-no-display.sock")),
        async_stop,
        ..FakeVmBackend::default()
    };
    let parts = Parts {
        calls,
        audit: audit.clone(),
        observations: backend.observations.clone(),
        stops: backend.stop_results.clone(),
        killed: backend.emergency_killed.clone(),
    };
    (sample_engine(backend, audit), parts)
}

/// Short display root (AF_UNIX paths are limited to 107 bytes).
fn short_root() -> PathBuf {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    std::env::temp_dir().join(format!("lvt{}", crate::credential::hex_encode(&b)))
}

fn supervisor(engine: Box<dyn VmEngine>) -> (SupervisorThread, mpsc::Receiver<Command>) {
    let (tx, rx) = mpsc::channel();
    let (events, _) = broadcast::channel(256);
    let display = Arc::new(DisplayRegistry::new(short_root()));

    let sup = SupervisorThread {
        engine,
        backend: Box::new(FakeHost::default()),
        events,
        max_frame: 1 << 20,
        emergency_epoch: Arc::new(AtomicU64::new(0)),
        parent_loss_policy: Arc::new(Mutex::new(ParentLossPolicy::default())),
        vms_with_process: Arc::new(AtomicUsize::new(0)),
        display,
        self_tx: tx,
        imports: 0,
        storage_jobs: 0,
        storage_cancel: Arc::default(),
    };
    (sup, rx)
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

/// Serves one command on the calling thread (no supervisor thread).
fn serve(sup: &mut SupervisorThread, rx: &mpsc::Receiver<Command>, cmd: Command) {
    let mut slot = None;
    let _ = sup.serve(cmd, rx, &mut slot);
    sup.publish_state_changes();
}

fn reply(mut r: ReplyRx) -> Result<ResponseBody, ReplyError> {
    for _ in 0..500 {
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

fn code(r: &Result<ResponseBody, ReplyError>) -> Option<ErrorCode> {
    match r {
        Err(ReplyError::Operational(e)) => Some(e.code),
        _ => None,
    }
}

fn live() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

fn vm_of(e: &mut dyn VmEngine) -> VmId {
    e.list_vms(&Caller::User).unwrap()[0].vm_id
}

fn submit(
    e: &mut dyn VmEngine,
    caller: &Caller,
    vm_id: VmId,
    run: Option<RunId>,
    action: VmAction,
) -> Result<SubmitOutcome, PublicError> {
    e.submit(
        caller,
        VmRequest {
            operation_id: OperationId::new_v7(),
            vm_id,
            expected_run_id: run,
            action,
        },
    )
}

fn start(e: &mut dyn VmEngine, vm_id: VmId) -> RunId {
    submit(e, &Caller::User, vm_id, None, VmAction::Start).unwrap();
    e.vm_status(&Caller::User, vm_id).unwrap().run_id.unwrap()
}

fn state(e: &mut dyn VmEngine, vm_id: VmId) -> VmState {
    e.vm_status(&Caller::User, vm_id).unwrap().state
}

fn count(audit: &MemoryAudit, action: AuditAction, outcome: AuditOutcome) -> usize {
    audit
        .events()
        .iter()
        .filter(|e| e.action == action && e.outcome == outcome)
        .count()
}

// ----- finding 1: panic containment ------------------------------------------

/// Engine that panics on `vm_status` and delegates everything else.
struct Panicky(MemoryEngine);

impl VmEngine for Panicky {
    fn list_vms(&mut self, c: &Caller) -> Result<Vec<lightvm_proto::ipc::VmSummary>, PublicError> {
        self.0.list_vms(c)
    }
    fn vm_status(
        &mut self,
        _: &Caller,
        _: VmId,
    ) -> Result<lightvm_proto::ipc::VmSummary, PublicError> {
        panic!("bug while serving vm_status");
    }
    fn create_vm(&mut self, c: &Caller, config: VmConfig) -> Result<VmRecord, PublicError> {
        self.0.create_vm(c, config)
    }
    fn update_vm(
        &mut self,
        c: &Caller,
        v: VmId,
        r: u64,
        config: VmConfig,
    ) -> Result<VmRecord, PublicError> {
        self.0.update_vm(c, v, r, config)
    }
    fn submit(&mut self, c: &Caller, req: VmRequest) -> Result<SubmitOutcome, PublicError> {
        self.0.submit(c, req)
    }
    fn after_emergency_stop(&mut self, a: Actor) {
        self.0.after_emergency_stop(a);
    }
    fn observe(&mut self) {
        self.0.observe();
    }
    fn vms_with_process(&self) -> usize {
        self.0.vms_with_process()
    }
    fn stop_all(&mut self) {
        self.0.stop_all();
    }
    fn state_changes(&mut self) -> Vec<(VmId, VmState)> {
        self.0.state_changes()
    }
    fn current_runs(&self) -> Vec<(VmId, Option<RunId>)> {
        self.0.current_runs()
    }
    fn list_media(&mut self, c: &Caller) -> Result<Vec<MediaRecord>, PublicError> {
        self.0.list_media(c)
    }
    fn begin_import(
        &mut self,
        c: &Caller,
        s: &str,
        k: MediaKind,
        o: &str,
    ) -> Result<ImportJob, PublicError> {
        self.0.begin_import(c, s, k, o)
    }
    fn finish_import(
        &mut self,
        c: &Caller,
        copied: ImportCopied,
    ) -> Result<MediaRecord, PublicError> {
        self.0.finish_import(c, copied)
    }
    fn abort_import(
        &mut self,
        c: &Caller,
        copied: ImportCopied,
        reason: PublicError,
    ) -> PublicError {
        self.0.abort_import(c, copied, reason)
    }
    fn delete_vm(&mut self, c: &Caller, v: VmId, r: u64) -> Result<(), PublicError> {
        self.0.delete_vm(c, v, r)
    }
    fn open_console(&mut self, c: &Caller, v: VmId) -> Result<ConsoleTarget, PublicError> {
        self.0.open_console(c, v)
    }
    fn console_opened(&mut self, c: &Caller, v: VmId, r: RunId, res: Result<(), ErrorCode>) {
        self.0.console_opened(c, v, r, res);
    }
    fn console_revoked(&mut self, a: Actor, runs: &[(VmId, RunId)], cause: TerminationCause) {
        self.0.console_revoked(a, runs, cause);
    }
    fn release_input(&mut self, v: VmId, r: RunId) {
        self.0.release_input(v, r);
    }
    fn host_capacity(&self) -> Option<HostCapacity> {
        self.0.host_capacity()
    }
}

#[test]
fn a_panic_while_serving_a_request_does_not_stop_the_supervisor() {
    let (mut e, parts) = engine(false);
    let vm = vm_of(&mut e);
    let run = start(&mut e, vm);
    let (sup, rx) = supervisor(Box::new(Panicky(e)));
    let tx = sup.self_tx.clone();
    let t = std::thread::spawn(move || sup.run(rx));
    let (_alive, rev) = live();

    let (cmd, r) = request(SupervisorRequest::VmStatus(vm), rev.clone(), 0);
    tx.send(cmd).unwrap();
    assert_eq!(code(&reply(r)), Some(ErrorCode::InfrastructureError));
    // Still serving, with the same engine and runs.
    let (cmd, r) = request(SupervisorRequest::Ping, rev.clone(), 0);
    tx.send(cmd).unwrap();
    assert!(matches!(reply(r), Ok(ResponseBody::Pong)));
    let (cmd, r) = request(SupervisorRequest::ListVms, rev.clone(), 0);
    tx.send(cmd).unwrap();
    match reply(r) {
        Ok(ResponseBody::VmList { vms }) => {
            assert_eq!((vms[0].state, vms[0].run_id), (VmState::Running, Some(run)));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !parts
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "vm_force_stop")
    );
    let (done, d) = oneshot::channel();
    tx.send(Command::Shutdown { done }).unwrap();
    let _ = d.blocking_recv();
    t.join().unwrap();
}

// ----- finding 5: closed connection = revoked ---------------------------------

#[test]
fn requests_of_a_closed_connection_are_not_executed() {
    let (mut e, parts) = engine(false);
    let vm = vm_of(&mut e);
    start(&mut e, vm);
    let (mut sup, rx) = supervisor(Box::new(e));
    // Queued, then the connection closes (its sender goes away).
    let (alive, rev) = live();
    let (cmd, r) = request(SupervisorRequest::OpenConsole(vm), rev, 0);
    drop(alive);
    serve(&mut sup, &rx, cmd);
    assert_eq!(code(&reply(r)), Some(ErrorCode::Unauthenticated));
    assert!(sup.display.active().is_empty(), "no ticket issued");
    assert_eq!(
        count(
            &parts.audit,
            AuditAction::ConsoleOpen,
            AuditOutcome::Succeeded
        ),
        0
    );
    // Raised flag: same.
    let (alive, rev) = live();
    let (cmd, r) = request(SupervisorRequest::OpenConsole(vm), rev, 0);
    alive.send(true).unwrap();
    serve(&mut sup, &rx, cmd);
    assert_eq!(code(&reply(r)), Some(ErrorCode::Unauthenticated));
    // Live connection: issued and audited once the proxy is up.
    let (_alive, rev) = live();
    let (cmd, r) = request(SupervisorRequest::OpenConsole(vm), rev, 0);
    serve(&mut sup, &rx, cmd);
    assert!(matches!(reply(r), Ok(ResponseBody::Console { .. })));
    assert_eq!(sup.display.active().len(), 1);
    assert_eq!(
        count(
            &parts.audit,
            AuditAction::ConsoleOpen,
            AuditOutcome::Succeeded
        ),
        1
    );
    sup.display.revoke_all();
}

// ----- findings 6 and 10: ticket bound to the run, audit ---------------------

#[test]
fn a_run_replaced_within_one_command_revokes_the_console_ticket() {
    let (mut e, parts) = engine(false);
    let vm = vm_of(&mut e);
    let (mut sup, rx) = supervisor(Box::new(e));
    let (_alive, rev) = live();
    let start_req = || {
        SupervisorRequest::SubmitVm(Box::new(VmRequest {
            operation_id: OperationId::new_v7(),
            vm_id: vm,
            expected_run_id: None,
            action: VmAction::Start,
        }))
    };
    let (cmd, r) = request(start_req(), rev.clone(), 0);
    serve(&mut sup, &rx, cmd);
    assert!(reply(r).is_ok());
    let (cmd, r) = request(SupervisorRequest::OpenConsole(vm), rev.clone(), 0);
    serve(&mut sup, &rx, cmd);
    let run1 = match reply(r) {
        Ok(ResponseBody::Console { ticket }) => ticket.run_id,
        other => panic!("{other:?}"),
    };
    assert_eq!(sup.display.active(), vec![(vm, run1)]);
    // The guest powered off and the next command starts the VM again:
    // `running → stopped → running` within one command.
    lock(&parts.observations).push((
        vm,
        BackendObservation::ProcessExited {
            run_id: run1,
            expected: true,
        },
    ));
    let (cmd, r) = request(start_req(), rev, 0);
    serve(&mut sup, &rx, cmd);
    assert!(reply(r).is_ok());
    let run2 = sup
        .engine
        .vm_status(&Caller::User, vm)
        .unwrap()
        .run_id
        .unwrap();
    assert_ne!(run1, run2);
    assert!(
        sup.display.active().is_empty(),
        "ticket of the old run revoked"
    );
    let revoke = parts
        .audit
        .events()
        .into_iter()
        .find(|e| e.action == AuditAction::ConsoleRevoke)
        .expect("console_revoke audited");
    assert_eq!(revoke.run_id, Some(run1));
    assert_eq!(revoke.termination, Some(TerminationCause::VmTerminated));
}

// ----- finding 13: observation deadline ---------------------------------------

#[test]
fn exits_are_observed_within_the_deadline_under_steady_traffic() {
    let (mut e, parts) = engine(false);
    let vm = vm_of(&mut e);
    let run = start(&mut e, vm);
    let (sup, rx) = supervisor(Box::new(e));
    let mut events = sup.events.subscribe();
    let tx = sup.self_tx.clone();
    let t = std::thread::spawn(move || sup.run(rx));
    let stop = Arc::new(AtomicBool::new(false));
    let pinger = {
        let (tx, stop) = (tx.clone(), stop.clone());
        std::thread::spawn(move || {
            let (_alive, rev) = live();
            while !stop.load(Ordering::SeqCst) {
                let (cmd, _r) = request(SupervisorRequest::Ping, rev.clone(), 0);
                if tx.send(cmd).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    while events.try_recv().is_ok() {}
    let t0 = Instant::now();
    lock(&parts.observations).push((
        vm,
        BackendObservation::ProcessExited {
            run_id: run,
            expected: false,
        },
    ));
    let mut seen = None;
    while t0.elapsed() < Duration::from_secs(3) && seen.is_none() {
        match events.try_recv() {
            Ok(ServiceEvent::VmState { vm_id, state }) if vm_id == vm && !state.has_process() => {
                seen = Some(t0.elapsed());
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    stop.store(true, Ordering::SeqCst);
    pinger.join().unwrap();
    let elapsed = seen.expect("exit not observed under steady traffic");
    assert!(elapsed < Duration::from_millis(2200), "{elapsed:?}");
    let (done, d) = oneshot::channel();
    tx.send(Command::Shutdown { done }).unwrap();
    let _ = d.blocking_recv();
    t.join().unwrap();
}

// ----- finding 15: imports cancelled by revocation and Emergency Stop --------

fn iso(name: &str) -> String {
    let d = crate::test_support::temp_dir("sup-import");
    let p = d.join(name);
    let mut b = vec![0u8; 40 * 1024];
    b[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
    std::fs::write(&p, b).unwrap();
    p.to_str().unwrap().to_owned()
}

fn import_req(path: &str) -> SupervisorRequest {
    SupervisorRequest::ImportMedia {
        source_path: path.to_owned(),
        media_kind: MediaKind::DataIso,
        declared_origin: "test".into(),
    }
}

/// Starts an import and returns the `FinishImport` sent by its worker.
fn import_until_finish(
    sup: &mut SupervisorThread,
    rx: &mpsc::Receiver<Command>,
    path: &str,
    rev: watch::Receiver<bool>,
) -> (Command, ReplyRx) {
    let epoch = sup.emergency_epoch.load(Ordering::SeqCst);
    let (cmd, r) = request(import_req(path), rev, epoch);
    let mut slot = None;
    let _ = sup.serve(cmd, rx, &mut slot);
    let finish = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("copy ended");
    assert!(matches!(finish, Command::FinishImport { .. }));
    (finish, r)
}

#[test]
fn imports_are_cancelled_by_revocation_and_emergency_stop() {
    let (e, parts) = engine(false);
    let (mut sup, rx) = supervisor(Box::new(e));
    let path = iso("a.iso");
    let media = |sup: &mut SupervisorThread| sup.engine.list_media(&Caller::User).unwrap().len();

    // Queued before an Emergency Stop: discarded like any mutation.
    let (_alive, rev) = live();
    let (cmd, r) = request(import_req(&path), rev.clone(), 0);
    sup.emergency_epoch.store(1, Ordering::SeqCst);
    serve(&mut sup, &rx, cmd);
    assert_eq!(code(&reply(r)), Some(ErrorCode::StateConflict));

    // Emergency Stop while copying: the copy is discarded.
    let (finish, r) = import_until_finish(&mut sup, &rx, &path, rev.clone());
    sup.emergency_epoch.store(2, Ordering::SeqCst);
    serve(&mut sup, &rx, finish);
    assert_eq!(code(&reply(r)), Some(ErrorCode::StateConflict));
    assert_eq!(media(&mut sup), 0);

    // Connection closed while copying: discarded too.
    let (alive2, rev2) = live();
    let (finish, r) = import_until_finish(&mut sup, &rx, &path, rev2);
    drop(alive2);
    serve(&mut sup, &rx, finish);
    assert_eq!(code(&reply(r)), Some(ErrorCode::Unauthenticated));
    assert_eq!(media(&mut sup), 0);
    assert_eq!(
        count(&parts.audit, AuditAction::MediaImport, AuditOutcome::Failed),
        2
    );
    assert_eq!(sup.imports, 0);

    // Nothing happened meanwhile: published.
    let (finish, r) = import_until_finish(&mut sup, &rx, &path, rev);
    serve(&mut sup, &rx, finish);
    assert!(matches!(reply(r), Ok(ResponseBody::MediaImported { .. })));
    assert_eq!(media(&mut sup), 1);
}

// ----- finding 14: geometry refresh after authorization, debounced ------------

#[test]
fn geometry_refresh_is_authorized_and_debounced() {
    let (mut e, parts) = engine(false);
    let vm = vm_of(&mut e);
    let run = start(&mut e, vm);
    let refreshes = || {
        parts
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "vm_refresh_geometry")
            .count()
    };
    let batch = |g: DisplayGeometry| {
        VmAction::Input(InputBatch {
            vm_id: vm,
            run_id: run,
            geometry: g,
            events: vec![InputEvent::PointerMove(PointerMove::new(100, 100).unwrap())],
        })
    };
    let stale = DisplayGeometry {
        width: 1024,
        height: 768,
    };
    // Geometry never read: one refresh, then the batch is admitted.
    assert!(submit(&mut e, &Caller::User, vm, Some(run), batch(FAKE_GEOMETRY)).is_ok());
    assert_eq!(refreshes(), 1);
    // A burst of stale batches within the interval: no further capture.
    for _ in 0..5 {
        assert!(submit(&mut e, &Caller::User, vm, Some(run), batch(stale)).is_err());
    }
    assert_eq!(refreshes(), 1);
    std::thread::sleep(crate::engine::GEOMETRY_REFRESH_INTERVAL + Duration::from_millis(100));
    // Unauthorized caller (no session): refused without any capture.
    let agent = Caller::Agent {
        session_id: SessionId::new_v7(),
        token: None,
    };
    assert!(submit(&mut e, &agent, vm, Some(run), batch(stale)).is_err());
    assert_eq!(refreshes(), 1);
    // After the interval the user's stale batch triggers one capture again.
    assert!(submit(&mut e, &Caller::User, vm, Some(run), batch(stale)).is_err());
    assert_eq!(refreshes(), 2);
}

// ----- findings 4 and 7: graceful stop timeout, Emergency Stop during it -----

#[test]
fn graceful_stop_timeout_waits_for_the_exit() {
    let (mut e, parts) = engine(true);
    let vm = vm_of(&mut e);
    let run = start(&mut e, vm);
    submit(&mut e, &Caller::User, vm, None, VmAction::Stop).unwrap();
    assert_eq!(state(&mut e, vm), VmState::Stopping);
    // Escalation timed out with the process alive.
    lock(&parts.stops).push((
        vm,
        Err(PublicError::infrastructure("QEMU shutdown timeout")),
    ));
    e.observe();
    let st = e.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.state, st.run_id), (VmState::Stopping, Some(run)));
    // The exit arrives later (escalation kill): stopped.
    lock(&parts.observations).push((
        vm,
        BackendObservation::ProcessExited {
            run_id: run,
            expected: false,
        },
    ));
    e.observe();
    let st = e.vm_status(&Caller::User, vm).unwrap();
    assert_eq!((st.state, st.run_id), (VmState::Stopped, None));
    // A new start works.
    start(&mut e, vm);
}

#[test]
fn emergency_stop_during_a_graceful_stop_is_recorded_as_emergency_stop() {
    let (mut e, parts) = engine(true);
    let vm = vm_of(&mut e);
    let run = start(&mut e, vm);
    submit(&mut e, &Caller::User, vm, None, VmAction::Stop).unwrap();
    assert_eq!(state(&mut e, vm), VmState::Stopping);
    // Priority path: the process is terminated; the background stop task
    // then ends (the process is gone) before the supervisor records the
    // Emergency Stop, and a periodic observation runs in between.
    lock(&parts.killed).insert(vm);
    lock(&parts.stops).push((vm, Ok(())));
    lock(&parts.observations).push((
        vm,
        BackendObservation::ProcessExited {
            run_id: run,
            expected: true,
        },
    ));
    e.observe();
    assert_eq!(state(&mut e, vm), VmState::Stopping, "not an ordinary stop");
    e.after_emergency_stop(Actor::User);
    assert_eq!(state(&mut e, vm), VmState::Stopped);
    let events = parts.audit.events();
    assert!(
        events
            .iter()
            .any(|ev| ev.action == AuditAction::EmergencyStop
                && ev.outcome == AuditOutcome::Succeeded
                && ev.run_id == Some(run))
    );
    assert!(
        !events
            .iter()
            .any(|ev| ev.action == AuditAction::VmStop && ev.outcome == AuditOutcome::Succeeded),
        "the graceful stop is not recorded as completed"
    );
}
