//! Service: IPC endpoint, handshake, connection registry with revocation,
//! request dispatcher, controlled shutdown.
//!
//! F4: agent sessions. A `pairing` connection asks for a session
//! (`request_session`), the user approves it in the UI (`approve_session`),
//! the session is bound to that connection and its calls (`agent`) are
//! checked on receipt by the gate of [`crate::agents::AgentRegistry`], then
//! by the supervisor thread and twice by the core. Pause, revocation and
//! Emergency Stop flip the gate here, before the supervisor queue (priority
//! point). A second endpoint accepts only the emergency verb
//! ([`EmergencyCommand`]) for `lightvm-service --emergency-stop`, usable when
//! the UI is frozen; it bypasses the supervisor thread like the UI's
//! Emergency Stop.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use tokio::io::{AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::{Semaphore, broadcast, oneshot, watch};

use lightvm_proto::ipc::{
    AGENT_DEFAULT_QUOTAS, AGENT_MAX_SCREENSHOT_BYTES, AgentCall, ClientMessage, CloseReason,
    ConnectionInfo, EMERGENCY_ENDPOINT_FILE, EmergencyCommand, EmergencyStopReport,
    MAX_SESSION_DURATION_S, MIN_LEASE_MS, MIN_SESSION_DURATION_S, Request, RequestId, ResponseBody,
    Role, SUPPORTED_PROTOCOL_VERSIONS, ServerMessage, ServiceEvent, ServiceState, SessionApproval,
    SessionAuth, SessionInfo, SessionRequestId, SessionRequestOutcome, SessionUsage, StopReason,
};
use lightvm_proto::{
    Actor, AuditAction, AuditOutcome, Caller, Capability, ErrorCode, LEASE_MAX_MILLIS,
    ProtocolError, ProtocolErrorCode, PublicError, ReplyError, SessionId, SessionState,
    SessionToken, TerminationCause, UtcTimestamp,
};

use crate::agents::{AgentRegistry, AwaitDecision, Closed};
use crate::credential::{Credential, CredentialId, CredentialStore};
use crate::display::DisplayRegistry;
use crate::engine::{NewSession, VmEngine};
use crate::frame::{self, FrameError};
use crate::lock::{InstanceLock, LockError};
use crate::ratelimit::{RateLimit, TokenBucket};
use crate::reports::CallRecord;
use crate::supervisor::{
    Backend, Command, EmergencyStop, ParentLossPolicy, ReportRequest, SupervisorRequest,
    SupervisorThread, unauthenticated,
};
use crate::transport::{Accepted, BoxedStream, EndpointOptions, Listener, PeerInfo};

/// Service configuration.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Catalog directory (contains the single-instance lock).
    pub catalog_dir: PathBuf,
    /// Private directory for the socket (Unix only).
    pub runtime_dir: Option<PathBuf>,
    /// Byte limit per frame.
    pub max_frame: u32,
    /// Per-connection rate limit.
    pub rate_limit: RateLimit,
    /// Rate limit of an agent (`pairing`) connection, i.e. per agent
    /// session (ROADMAP 9: per identity/session).
    pub agent_rate_limit: RateLimit,
    /// Lifetime of a pending session request (F4).
    pub session_request_ttl: Duration,
    /// Slots in the queue towards the supervisor (shared between connections).
    pub queue_capacity: usize,
    /// Maximum simultaneous connections (including those in handshake).
    pub max_connections: usize,
    /// Maximum time to complete the handshake.
    pub handshake_timeout: Duration,
    /// Lifetime of pairing credentials.
    pub pairing_ttl: Duration,
    /// Maximum wait for the supervisor during shutdown.
    pub shutdown_timeout: Duration,
    /// Fate of the VMs when the UI is lost (changeable via IPC).
    pub parent_loss_policy: ParentLossPolicy,
    /// Root of the per-run display sockets (console tickets, ADR-0013),
    /// made accessible to the current user only. It must be short (AF_UNIX
    /// paths: 107 bytes, the proxy adds 21 characters). Default:
    /// `<runtime_dir or catalog/p>/d`.
    pub display_dir: Option<PathBuf>,
    /// Wall-clock source of the host-resume detector (F6, ROADMAP 7.3);
    /// the system clock outside tests.
    pub wall_clock: crate::resume::WallClock,
    /// Gap of the supervisor loop that counts as a host resume (or forward
    /// clock jump): the agent sessions end with `host_resumed` and their
    /// VMs are stopped.
    pub resume_gap: Duration,
}

impl ServiceConfig {
    /// Default values for a catalog.
    pub fn new(catalog_dir: impl Into<PathBuf>) -> Self {
        Self {
            catalog_dir: catalog_dir.into(),
            runtime_dir: None,
            max_frame: frame::DEFAULT_MAX_FRAME,
            rate_limit: RateLimit::default(),
            agent_rate_limit: RateLimit {
                burst: 20,
                per_second: 50.0,
            },
            session_request_ttl: Duration::from_secs(lightvm_proto::ipc::SESSION_REQUEST_TTL_S),
            queue_capacity: 64,
            max_connections: 16,
            handshake_timeout: Duration::from_secs(5),
            pairing_ttl: Duration::from_secs(600),
            shutdown_timeout: Duration::from_secs(30),
            parent_loss_policy: ParentLossPolicy::default(),
            display_dir: None,
            wall_clock: crate::resume::WallClock::default(),
            resume_gap: crate::resume::DEFAULT_RESUME_GAP,
        }
    }
}

/// Startup error.
#[derive(Debug)]
pub enum StartError {
    /// Catalog lock not acquired.
    Lock(LockError),
    /// IPC endpoint not created.
    Endpoint(io::Error),
    /// Other I/O error (credential generation, thread).
    Io(io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lock(e) => write!(f, "{e}"),
            Self::Endpoint(e) => write!(f, "cannot create the IPC endpoint: {e}"),
            Self::Io(e) => write!(f, "startup error: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

struct ConnEntry {
    role: Role,
    credential_id: CredentialId,
    peer_pid: Option<u32>,
    client_name: Option<String>,
    revoke: watch::Sender<bool>,
}

struct Shared {
    cfg: ServiceConfig,
    creds: Mutex<CredentialStore>,
    conns: Mutex<HashMap<u64, ConnEntry>>,
    next_conn: AtomicU64,
    cmd_tx: Mutex<Option<mpsc::Sender<Command>>>,
    queue: Arc<Semaphore>,
    conn_slots: Arc<Semaphore>,
    events: broadcast::Sender<ServiceEvent>,
    emergency: Arc<dyn EmergencyStop>,
    shutdown_trigger: watch::Sender<bool>,
    closing: watch::Sender<bool>,
    stopping: AtomicBool,
    /// Emergency Stop generation: mutations queued before an Emergency Stop are
    /// discarded by the supervisor.
    emergency_epoch: Arc<AtomicU64>,
    /// Parent-loss policy in force (shared with the supervisor).
    parent_loss_policy: Arc<Mutex<ParentLossPolicy>>,
    /// VMs with a process according to the supervisor (shutdown budget).
    vms_with_process: Arc<AtomicUsize>,
    /// Console tickets (revoked here on connection loss and Emergency Stop).
    display: Arc<DisplayRegistry>,
    /// Agent sessions: switch, requests, gate (F4).
    agents: Arc<AgentRegistry>,
    /// File publishing the emergency endpoint, removed at shutdown.
    emergency_file: Option<PathBuf>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Public operational error with a stable code.
fn op_error(code: ErrorCode, message: impl Into<String>) -> ReplyError {
    ReplyError::Operational(PublicError::new(code, message))
}

/// Protocol error: no effect attempted.
fn protocol_error(code: ProtocolErrorCode, message: impl Into<String>) -> ReplyError {
    ReplyError::Protocol(ProtocolError {
        code,
        message: message.into(),
    })
}

impl Shared {
    fn send_cmd(&self, cmd: Command) -> bool {
        match lock(&self.cmd_tx).as_ref() {
            Some(tx) => tx.send(cmd).is_ok(),
            None => false,
        }
    }

    /// Revokes a connection; for roles other than `Ui` it also revokes the
    /// credential (the session is not reusable).
    fn revoke_connection(&self, id: u64, actor: Actor) -> bool {
        let entry = lock(&self.conns).remove(&id);
        match entry {
            Some(e) => {
                if e.role != Role::Ui {
                    lock(&self.creds).revoke(e.credential_id);
                }
                // Before anything else: requests of this connection still
                // queued towards the supervisor are no longer executed.
                let _ = e.revoke.send(true);
                self.revoke_consoles_of(id, actor, TerminationCause::Revoked);
                let _ = self
                    .events
                    .send(ServiceEvent::ConnectionRevoked { connection_id: id });
                true
            }
            None => false,
        }
    }

    /// Revokes all non-UI connections and credentials (agent sessions and pairing).
    fn revoke_non_ui(&self) -> u32 {
        lock(&self.creds).revoke_all_except(Role::Ui);
        let ids: Vec<u64> = lock(&self.conns)
            .iter()
            .filter(|(_, e)| e.role != Role::Ui)
            .map(|(id, _)| *id)
            .collect();
        let mut n = 0;
        for id in ids {
            if self.revoke_connection(id, Actor::System) {
                n += 1;
            }
        }
        n
    }

    /// Revokes the console tickets obtained by a connection (revoked or
    /// closed): audit and release of the input still pressed on those runs.
    fn revoke_consoles_of(&self, connection_id: u64, actor: Actor, cause: TerminationCause) {
        let runs = self.display.revoke_owner(connection_id);
        if !runs.is_empty() {
            self.send_cmd(Command::ConsolesRevoked {
                runs,
                actor,
                cause,
                release_input: true,
            });
        }
    }

    /// Audit of a session lifecycle step decided here (no reply awaited).
    fn audit(
        &self,
        actor: Actor,
        action: AuditAction,
        outcome: AuditOutcome,
        session_id: Option<SessionId>,
        error: Option<ErrorCode>,
    ) {
        self.send_cmd(Command::Record {
            actor,
            action,
            outcome,
            session_id,
            error,
        });
    }

    /// Requests closed without a user decision (expired, withdrawn) or
    /// denied: event and audit.
    fn report_closed(&self, closed: Closed) {
        for (request_id, outcome) in closed {
            let error = match outcome {
                SessionRequestOutcome::Expired => Some(ErrorCode::SessionExpired),
                SessionRequestOutcome::Denied => Some(ErrorCode::PermissionDenied),
                _ => None,
            };
            let actor = if outcome == SessionRequestOutcome::Denied {
                Actor::User
            } else {
                Actor::System
            };
            self.audit(
                actor,
                AuditAction::SessionDeny,
                AuditOutcome::Succeeded,
                None,
                error,
            );
            let _ = self.events.send(ServiceEvent::SessionRequestClosed {
                request_id,
                outcome,
            });
        }
    }

    /// Priority path of the Emergency Stop, of the UI loss and of the agent
    /// access switch turned off: every session gate closes (their
    /// connections close) and every pending request is denied. The core
    /// terminates the sessions afterwards on the supervisor thread.
    /// Closes an agent connection whose session was terminated here: its
    /// request in progress and its queued requests are abandoned at once
    /// (the pairing credential is kept: a new session needs a new approval).
    fn close_agent_connection(&self, connection_id: u64) {
        if let Some(e) = lock(&self.conns).get(&connection_id) {
            let _ = e.revoke.send(true);
        }
    }

    fn close_all_agent_sessions(&self, reason: &str, cause: TerminationCause, stop: bool) {
        for conn in self.agents.terminate_all(cause, stop) {
            self.close_agent_connection(conn);
        }
        let mut closed = Vec::new();
        for id in self.agents.pending_ids() {
            if self.agents.deny(id, reason) {
                closed.push((id, SessionRequestOutcome::Denied));
            }
        }
        self.report_closed(closed);
    }

    fn connections(&self) -> Vec<ConnectionInfo> {
        let mut v: Vec<ConnectionInfo> = lock(&self.conns)
            .iter()
            .map(|(id, e)| ConnectionInfo {
                connection_id: *id,
                role: e.role,
                peer_pid: e.peer_pid,
                client_name: e.client_name.clone(),
            })
            .collect();
        v.sort_by_key(|c| c.connection_id);
        v
    }

    fn trigger_shutdown(&self) {
        // Immediately, not only when the shutdown driver starts: an EOF on the
        // lifeline pipe that follows the UI's shutdown request is not a parent
        // loss (no Emergency Stop).
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown_trigger.send(true);
    }

    async fn emergency_stop(&self, actor: Actor) -> EmergencyStopReport {
        // Revoke first: after this point no new agent mutation is accepted
        // (session gates closed, pending requests denied, non-UI connections
        // and credentials revoked). Then termination through the backend
        // handler, outside the queue and outside the supervisor thread.
        // The Emergency Stop kills every VM itself: no stop per session.
        self.close_all_agent_sessions("emergency_stop", TerminationCause::EmergencyStop, false);
        let revoked = self.revoke_non_ui();
        // Console tickets: no new display connection, live ones closed.
        let consoles = self.display.revoke_all();
        // Mutations already queued towards the supervisor are not executed.
        self.emergency_epoch.fetch_add(1, Ordering::SeqCst);
        let handle = self.emergency.clone();
        let outcome = tokio::task::spawn_blocking(move || handle.emergency_stop_all())
            .await
            .unwrap_or_default();
        // Recording in the core (audit, VM state) without waiting for it: the
        // supervisor may be busy and the priority path does not wait.
        if !consoles.is_empty() {
            self.send_cmd(Command::ConsolesRevoked {
                runs: consoles,
                actor,
                cause: TerminationCause::EmergencyStop,
                release_input: false,
            });
        }
        self.send_cmd(Command::EmergencyReconcile(actor));
        let _ = self.events.send(ServiceEvent::AgentSessionsStopped {
            reason: StopReason::EmergencyStop,
        });
        EmergencyStopReport {
            revoked_connections: revoked,
            processes_terminated: outcome.processes_terminated,
            completed: outcome.completed,
        }
    }

    async fn parent_lost(self: &Arc<Self>) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let policy = *lock(&self.parent_loss_policy);
        // ROADMAP 7.3: UI lost → revocation and stop of the sessions' VMs.
        self.close_all_agent_sessions("ui_lost", TerminationCause::UiClosed, true);
        self.revoke_non_ui();
        let (done_tx, done_rx) = oneshot::channel();
        if self.send_cmd(Command::ParentLost {
            policy,
            done: done_tx,
        }) {
            let _ = tokio::time::timeout(self.cfg.shutdown_timeout, done_rx).await;
        }
        match policy {
            ParentLossPolicy::KillVms => {
                // Priority path: the VMs terminate immediately with OS primitives
                // and the core records the Emergency Stop; then the service shuts down.
                let r = self.emergency_stop(Actor::System).await;
                crate::diag!(
                    "lightvm-service: UI lost, Emergency Stop: {} processes terminated, completed {}",
                    r.processes_terminated,
                    r.completed
                );
                self.trigger_shutdown();
            }
            ParentLossPolicy::ShutdownVms { .. } => self.trigger_shutdown(),
            ParentLossPolicy::KeepVms => {}
        }
    }

    /// Wait for the supervisor during shutdown. The VMs stop one after the
    /// other, so the budget applies to each VM with a process: the service
    /// timeout, or with `shutdown_vms` at least the ACPI wait plus `quit` and
    /// termination (20 s).
    fn shutdown_budget(&self) -> Duration {
        let per_vm = match *lock(&self.parent_loss_policy) {
            ParentLossPolicy::ShutdownVms { powerdown_s } => self
                .cfg
                .shutdown_timeout
                .max(Duration::from_secs(u64::from(powerdown_s) + 20)),
            _ => self.cfg.shutdown_timeout,
        };
        let vms = self.vms_with_process.load(Ordering::SeqCst).max(1);
        per_vm.saturating_mul(u32::try_from(vms).unwrap_or(u32::MAX))
    }
}

/// Handle of the running service.
pub struct ServiceHandle {
    shared: Arc<Shared>,
    endpoint: String,
    emergency_endpoint: String,
    stopped: watch::Receiver<bool>,
}

impl std::fmt::Debug for ServiceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceHandle")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl ServiceHandle {
    /// IPC endpoint (not secret).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Emergency endpoint (not secret): accepts only the emergency verb.
    pub fn emergency_endpoint(&self) -> &str {
        &self.emergency_endpoint
    }

    /// Agent access switch (F4), for diagnostics and tests.
    pub fn agent_access(&self) -> bool {
        self.shared.agents.enabled()
    }

    /// Issues a pairing credential (no VM permissions).
    pub fn issue_pairing_credential(&self) -> io::Result<Credential> {
        let ttl = self.shared.cfg.pairing_ttl;
        lock(&self.shared.creds)
            .issue(Role::Pairing, Some(ttl))
            .map(|(_, c)| c)
    }

    /// Active connections.
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.shared.connections()
    }

    /// Revokes a connection.
    pub fn revoke_connection(&self, id: u64) -> bool {
        self.shared.revoke_connection(id, Actor::User)
    }

    /// Parent process lost: stop of the agent sessions, backend hook and then
    /// [`ParentLossPolicy`].
    pub async fn parent_lost(&self) {
        self.shared.parent_lost().await;
    }

    /// Emergency Stop from the local process (priority path).
    pub async fn emergency_stop(&self) -> EmergencyStopReport {
        self.shared.emergency_stop(Actor::User).await
    }

    /// Starts the controlled shutdown without waiting for it to finish.
    pub fn trigger_shutdown(&self) {
        self.shared.trigger_shutdown();
    }

    /// Waits for the service to end (by request, parent lost or shutdown).
    pub async fn wait(&self) {
        let mut rx = self.stopped.clone();
        flag_set(&mut rx).await;
    }

    /// Controlled shutdown: closes the connections, waits for the operation in
    /// progress, rejects the queue, stops the backend and releases the lock.
    pub async fn shutdown(self) {
        self.shared.trigger_shutdown();
        self.wait().await;
    }
}

/// Starts the service. Returns the handle and the initial UI credential, to be
/// handed to the parent over a private channel (never in the arguments).
pub async fn start(
    cfg: ServiceConfig,
    engine: Box<dyn VmEngine>,
    backend: Box<dyn Backend>,
) -> Result<(ServiceHandle, Credential), StartError> {
    let instance_lock = InstanceLock::acquire(&cfg.catalog_dir).map_err(StartError::Lock)?;
    let listener = Listener::bind(&EndpointOptions {
        runtime_dir: cfg.runtime_dir.clone(),
        catalog_dir: cfg.catalog_dir.clone(),
    })
    .map_err(StartError::Endpoint)?;
    let endpoint = listener.endpoint().to_owned();
    // Emergency endpoint (F4): same per-user ACL, one verb only, published
    // in the catalog for `lightvm-service --emergency-stop`.
    let emergency_listener = Listener::bind(&EndpointOptions {
        runtime_dir: cfg.runtime_dir.clone(),
        catalog_dir: cfg.catalog_dir.clone(),
    })
    .map_err(StartError::Endpoint)?;
    let emergency_endpoint = emergency_listener.endpoint().to_owned();
    let emergency_file = cfg.catalog_dir.join(EMERGENCY_ENDPOINT_FILE);
    let emergency_file = match std::fs::write(&emergency_file, &emergency_endpoint) {
        Ok(()) => Some(emergency_file),
        Err(e) => {
            crate::diag!("lightvm-service: emergency endpoint not published: {e}");
            None
        }
    };
    let agents = Arc::new(AgentRegistry::new(cfg.session_request_ttl));

    let mut creds = CredentialStore::new();
    let (_, ui_credential) = creds.issue(Role::Ui, None).map_err(StartError::Io)?;

    let (events, _) = broadcast::channel(256);
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let emergency = backend.emergency_handle();
    let emergency_epoch = Arc::new(AtomicU64::new(0));
    let parent_loss_policy = Arc::new(Mutex::new(cfg.parent_loss_policy));
    let vms_with_process = Arc::new(AtomicUsize::new(0));
    let display_root = cfg.display_dir.clone().unwrap_or_else(|| {
        cfg.runtime_dir
            .clone()
            .unwrap_or_else(|| cfg.catalog_dir.join("p"))
            .join("d")
    });
    let display = Arc::new(DisplayRegistry::new(display_root));
    // The instance lock is held: per-proxy directories left by a previous
    // service of this catalog can go.
    let stale = display.remove_stale();
    if stale > 0 {
        crate::diag!("lightvm-service: {stale} stale display directories removed");
    }
    let sup = SupervisorThread {
        engine,
        backend,
        events: events.clone(),
        max_frame: cfg.max_frame,
        emergency_epoch: emergency_epoch.clone(),
        parent_loss_policy: parent_loss_policy.clone(),
        vms_with_process: vms_with_process.clone(),
        display: display.clone(),
        self_tx: cmd_tx.clone(),
        imports: 0,
        storage_jobs: 0,
        storage_cancel: Arc::default(),
        start_jobs: 0,
        agents: agents.clone(),
        resume: crate::resume::ResumeDetector::new(cfg.wall_clock.clone(), cfg.resume_gap),
        reports: crate::reports::ReportStore::new(Some(
            cfg.catalog_dir.join(crate::reports::SESSIONS_DIR),
        )),
    };
    // Published before any possible subscription: no client receives it late,
    // after later events (race with `subscribe`).
    let _ = events.send(ServiceEvent::ServiceState {
        state: ServiceState::Running,
    });
    let sup_thread = std::thread::Builder::new()
        .name("lightvm-supervisor".into())
        .spawn(move || sup.run(cmd_rx))
        .map_err(StartError::Io)?;

    let (shutdown_trigger, mut shutdown_rx) = watch::channel(false);
    let (closing, _) = watch::channel(false);
    let (stopped_tx, stopped_rx) = watch::channel(false);

    let shared = Arc::new(Shared {
        queue: Arc::new(Semaphore::new(cfg.queue_capacity)),
        conn_slots: Arc::new(Semaphore::new(cfg.max_connections)),
        cfg,
        creds: Mutex::new(creds),
        conns: Mutex::new(HashMap::new()),
        next_conn: AtomicU64::new(1),
        cmd_tx: Mutex::new(Some(cmd_tx)),
        events,
        emergency,
        shutdown_trigger,
        closing,
        stopping: AtomicBool::new(false),
        emergency_epoch,
        parent_loss_policy,
        vms_with_process,
        display,
        agents,
        emergency_file,
    });

    // Accept loop.
    let accept_shared = shared.clone();
    let mut accept_closing = shared.closing.subscribe();
    let accept_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = flag_set(&mut accept_closing) => break,
                r = listener.accept() => match r {
                    Ok(Accepted::Ok(stream, peer)) => {
                        let sh = accept_shared.clone();
                        match sh.conn_slots.clone().try_acquire_owned() {
                            Ok(permit) => {
                                tokio::spawn(async move {
                                    handle_connection(sh, stream, peer).await;
                                    drop(permit);
                                });
                            }
                            Err(_) => {
                                tokio::spawn(reject_busy(stream, accept_shared.cfg.max_frame));
                            }
                        }
                    }
                    Ok(Accepted::Rejected(reason)) => {
                        crate::diag!("lightvm-service: peer rejected by the transport: {reason}");
                    }
                    Err(e) => {
                        crate::diag!("lightvm-service: accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
        // `listener` is dropped here: on Unix the socket is removed.
    });

    // Emergency accept loop: one verb, no credential (fail-safe effect),
    // bounded concurrency, short timeout.
    let em_shared = shared.clone();
    let mut em_closing = shared.closing.subscribe();
    let em_slots = Arc::new(Semaphore::new(4));
    let emergency_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = flag_set(&mut em_closing) => break,
                r = emergency_listener.accept() => match r {
                    Ok(Accepted::Ok(stream, _peer)) => {
                        if let Ok(permit) = em_slots.clone().try_acquire_owned() {
                            let sh = em_shared.clone();
                            tokio::spawn(async move {
                                handle_emergency(sh, stream).await;
                                drop(permit);
                            });
                        }
                    }
                    Ok(Accepted::Rejected(reason)) => {
                        crate::diag!("lightvm-service: emergency peer rejected: {reason}");
                    }
                    Err(e) => {
                        crate::diag!("lightvm-service: emergency accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
    });

    // Shutdown driver.
    let drv = shared.clone();
    tokio::spawn(async move {
        flag_set(&mut shutdown_rx).await;
        drv.stopping.store(true, Ordering::SeqCst);
        // 1. No new connections; the connections receive `closing`.
        let _ = drv.closing.send(true);
        let _ = accept_task.await;
        let _ = emergency_task.await;
        if let Some(f) = &drv.emergency_file {
            let _ = std::fs::remove_file(f);
        }
        // 2. The supervisor finishes the current operation, rejects the queue and
        //    stops the backend.
        let (done_tx, done_rx) = oneshot::channel();
        let finished = drv.send_cmd(Command::Shutdown { done: done_tx })
            && tokio::time::timeout(drv.shutdown_budget(), done_rx)
                .await
                .is_ok();
        lock(&drv.cmd_tx).take();
        if finished {
            let _ = tokio::task::spawn_blocking(move || sup_thread.join()).await;
        } else {
            // Supervisor stuck: the thread is not awaited; the VMs still alive
            // terminate through the priority path (not only through the Job
            // Object closing at exit), then the lock is released.
            crate::diag!("lightvm-service: timeout while stopping the supervisor");
            let handle = drv.emergency.clone();
            let outcome = tokio::task::spawn_blocking(move || handle.emergency_stop_all())
                .await
                .unwrap_or_default();
            crate::diag!(
                "lightvm-service: Emergency Stop after the timeout: {} processes terminated",
                outcome.processes_terminated
            );
        }
        // 3. Short wait for the connections to close.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while drv.conn_slots.available_permits() < drv.cfg.max_connections
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // 4. Release of the catalog lock.
        drop(instance_lock);
        let _ = stopped_tx.send(true);
    });

    Ok((
        ServiceHandle {
            shared,
            endpoint,
            emergency_endpoint,
            stopped: stopped_rx,
        },
        ui_credential,
    ))
}

/// Emergency endpoint: reads one [`EmergencyCommand`] (small frame, short
/// timeout) and runs the priority path; anything else closes the
/// connection with a protocol error.
async fn handle_emergency(shared: Arc<Shared>, stream: BoxedStream) {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let max = shared.cfg.max_frame;
    let first = tokio::time::timeout(
        shared.cfg.handshake_timeout,
        frame::read_frame::<_, EmergencyCommand>(&mut rd, 256),
    )
    .await;
    let m = match first {
        Ok(Ok(EmergencyCommand::EmergencyStop)) => {
            crate::diag!("lightvm-service: Emergency Stop from the local emergency command");
            let report = shared.emergency_stop(Actor::User).await;
            ServerMessage::Response {
                request_id: 0,
                result: ResponseBody::EmergencyStop(report),
            }
        }
        Ok(Err(e)) => frame_error_message(&e).unwrap_or(ServerMessage::Error {
            request_id: None,
            error: protocol_error(ProtocolErrorCode::MalformedMessage, "invalid message"),
        }),
        Err(_) => return,
    };
    let _ = send(&mut wr, &m, max).await;
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut wr).await;
}

async fn reject_busy(stream: BoxedStream, max: u32) {
    let (_, mut wr) = tokio::io::split(stream);
    let _ = send(
        &mut wr,
        &ServerMessage::Error {
            request_id: None,
            error: op_error(ErrorCode::ResourceLimit, "too many connections"),
        },
        max,
    )
    .await;
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, m: &ServerMessage, max: u32) -> io::Result<()> {
    let payload = serde_json::to_vec(m).map_err(io::Error::other)?;
    // A response that is too large (for example a catalog with many VMs)
    // becomes an error for that request, not a silent close of the
    // connection.
    if payload.len() > max as usize
        && let ServerMessage::Response { request_id, .. } = m
    {
        let e = err(
            Some(*request_id),
            ErrorCode::ResourceLimit,
            "response over the maximum frame size",
        );
        return frame::write_frame(w, &e, max).await;
    }
    frame::write_frame_bytes(w, &payload, max).await
}

fn err(request_id: Option<RequestId>, e: ErrorCode, msg: &str) -> ServerMessage {
    ServerMessage::Error {
        request_id,
        error: op_error(e, msg),
    }
}

/// Dedicated reader: reading a frame cannot be cancelled halfway without
/// corrupting the stream, so it happens in a separate task that delivers
/// complete frames over a bounded channel (per-connection backpressure).
fn spawn_reader(
    mut rd: ReadHalf<BoxedStream>,
    max: u32,
) -> (
    tokio::sync::mpsc::Receiver<Result<ClientMessage, FrameError>>,
    AbortOnDrop,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let task = tokio::spawn(async move {
        loop {
            let r = frame::read_frame::<_, ClientMessage>(&mut rd, max).await;
            let fatal = matches!(
                r,
                Err(FrameError::Closed
                    | FrameError::Io(_)
                    | FrameError::TooLarge { .. }
                    | FrameError::Empty)
            );
            if tx.send(r).await.is_err() || fatal {
                break;
            }
        }
    });
    (rx, AbortOnDrop(task))
}

/// Aborts the reader when the connection ends: the read half is dropped and
/// the transport really closes.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn frame_error_message(e: &FrameError) -> Option<ServerMessage> {
    match e {
        FrameError::TooLarge { declared, max } => Some(ServerMessage::Error {
            request_id: None,
            error: protocol_error(
                ProtocolErrorCode::MessageTooLarge,
                format!("frame of {declared} bytes over the limit of {max}"),
            ),
        }),
        FrameError::Empty => Some(ServerMessage::Error {
            request_id: None,
            error: protocol_error(ProtocolErrorCode::MalformedMessage, "empty frame"),
        }),
        FrameError::Malformed(_) => Some(ServerMessage::Error {
            request_id: None,
            error: protocol_error(ProtocolErrorCode::MalformedMessage, "invalid message"),
        }),
        FrameError::Closed | FrameError::Io(_) => None,
    }
}

struct Authenticated {
    connection_id: u64,
    role: Role,
    revoke_rx: watch::Receiver<bool>,
}

/// Handshake: version, then credential. Every rejection closes the connection
/// without effects and without telling the client the reason for the denial.
async fn handshake(
    shared: &Arc<Shared>,
    frames: &mut tokio::sync::mpsc::Receiver<Result<ClientMessage, FrameError>>,
    wr: &mut WriteHalf<BoxedStream>,
    peer: &PeerInfo,
) -> Option<Authenticated> {
    let max = shared.cfg.max_frame;
    let first = match tokio::time::timeout(shared.cfg.handshake_timeout, frames.recv()).await {
        Err(_) | Ok(None) => return None,
        Ok(Some(r)) => r,
    };
    let (request_id, versions, credential, client_name) = match first {
        Err(e) => {
            if let Some(m) = frame_error_message(&e) {
                let _ = send(wr, &m, max).await;
            }
            return None;
        }
        Ok(ClientMessage::Request { request_id, .. }) => {
            let _ = send(
                wr,
                &ServerMessage::Error {
                    request_id: Some(request_id),
                    error: unauthenticated("handshake with credential required"),
                },
                max,
            )
            .await;
            return None;
        }
        Ok(ClientMessage::Hello {
            request_id,
            protocol_versions,
            credential,
            client_name,
        }) => (request_id, protocol_versions, credential, client_name),
    };
    let Some(version) = SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|v| versions.contains(v))
    else {
        let _ = send(
            wr,
            &ServerMessage::Error {
                request_id: Some(request_id),
                error: protocol_error(
                    ProtocolErrorCode::UnsupportedVersion,
                    format!(
                        "no common protocol version; supported: {SUPPORTED_PROTOCOL_VERSIONS:?}"
                    ),
                ),
            },
            max,
        )
        .await;
        return None;
    };
    let verified = credential
        .as_deref()
        .and_then(Credential::from_hex)
        .and_then(|c| lock(&shared.creds).verify(&c));
    let Some((credential_id, role)) = verified else {
        let _ = send(
            wr,
            &ServerMessage::Error {
                request_id: Some(request_id),
                error: unauthenticated("credential missing or invalid"),
            },
            max,
        )
        .await;
        return None;
    };
    if shared.stopping.load(Ordering::SeqCst) {
        let _ = send(
            wr,
            &err(
                Some(request_id),
                ErrorCode::StateConflict,
                "service shutting down",
            ),
            max,
        )
        .await;
        return None;
    }
    let connection_id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
    let (revoke, revoke_rx) = watch::channel(false);
    lock(&shared.conns).insert(
        connection_id,
        ConnEntry {
            role,
            credential_id,
            peer_pid: peer.pid,
            client_name: client_name.map(|n| n.chars().take(64).collect()),
            revoke,
        },
    );
    let ok = ServerMessage::HelloOk {
        request_id,
        protocol_version: version,
        connection_id,
        role,
    };
    if send(wr, &ok, max).await.is_err() {
        lock(&shared.conns).remove(&connection_id);
        return None;
    }
    Some(Authenticated {
        connection_id,
        role,
        revoke_rx,
    })
}

enum Flow {
    Continue,
    Close(Option<CloseReason>),
}

async fn handle_connection(shared: Arc<Shared>, stream: BoxedStream, peer: PeerInfo) {
    let max = shared.cfg.max_frame;
    let (rd, mut wr) = tokio::io::split(stream);
    let (mut frames, _reader) = spawn_reader(rd, max);
    let Some(auth) = handshake(&shared, &mut frames, &mut wr, &peer).await else {
        return;
    };
    let Authenticated {
        connection_id,
        role,
        mut revoke_rx,
    } = auth;
    let mut closing_rx = shared.closing.subscribe();
    // Agent connections: raised when the bound session is terminated.
    let mut session_end =
        (role == Role::Pairing).then(|| shared.agents.register_conn(connection_id));
    let mut bucket = TokenBucket::new(if role == Role::Pairing {
        shared.cfg.agent_rate_limit
    } else {
        shared.cfg.rate_limit
    });
    let mut events: Option<broadcast::Receiver<ServiceEvent>> = None;
    // Replies of long requests served without blocking the connection (media
    // import: the reply arrives after the copy, events keep flowing).
    let (late_tx, mut late_rx) = tokio::sync::mpsc::channel::<ServerMessage>(4);
    // Agent calls waiting for their reply, recorded in the session report
    // with their outcome (F5).
    let mut pending_calls: HashMap<RequestId, PendingCall> = HashMap::new();

    let close_reason: Option<CloseReason> = loop {
        let flow = tokio::select! {
            biased;
            _ = flag_set(&mut revoke_rx) => Flow::Close(Some(CloseReason::Revoked)),
            _ = session_ended(&mut session_end) => Flow::Close(Some(CloseReason::Revoked)),
            _ = flag_set(&mut closing_rx) => Flow::Close(Some(CloseReason::ServiceStopping)),
            Some(m) = late_rx.recv() => {
                note_reply(&shared, &mut pending_calls, &m);
                if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
            }
            ev = recv_event(&mut events) => {
                let msg = match ev {
                    Ok(event) => ServerMessage::Event { event },
                    Err(broadcast::error::RecvError::Lagged(n)) => ServerMessage::Event {
                        event: ServiceEvent::EventsLost { count: n },
                    },
                    Err(broadcast::error::RecvError::Closed) => {
                        events = None;
                        continue;
                    }
                };
                if send(&mut wr, &msg, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
            }
            f = frames.recv() => match f {
                None | Some(Err(FrameError::Closed | FrameError::Io(_))) => Flow::Close(None),
                Some(Err(e)) => {
                    let fatal = matches!(e, FrameError::TooLarge { .. } | FrameError::Empty);
                    if let Some(m) = frame_error_message(&e) {
                        let _ = send(&mut wr, &m, max).await;
                    }
                    if fatal { Flow::Close(None) } else { Flow::Continue }
                }
                Some(Ok(ClientMessage::Hello { request_id, .. })) => {
                    let m = ServerMessage::Error {
                        request_id: Some(request_id),
                        error: protocol_error(ProtocolErrorCode::MalformedMessage, "handshake already completed"),
                    };
                    if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                }
                Some(Ok(ClientMessage::Request { request_id, body })) => {
                    if role == Role::Pairing && let Some(p) = pending_call(&body) {
                        pending_calls.insert(request_id, p);
                    }
                    if !bucket.try_acquire() {
                        let m = err(Some(request_id), ErrorCode::ResourceLimit, &format!(
                            "request rate over the limit; retry in {} ms",
                            bucket.retry_after().as_millis()
                        ));
                        note_reply(&shared, &mut pending_calls, &m);
                        if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                    } else if matches!(
                        body,
                        Request::ImportMedia { .. }
                            | Request::CreateBaseline { .. }
                            | Request::RestoreBaseline { .. }
                            | Request::AwaitSession { .. }
                            | Request::Agent { call: AgentCall::Snapshot { .. } | AgentCall::Restore { .. }, .. }
                    ) {
                        spawn_detached(&shared, role, connection_id, peer.pid, &revoke_rx, &late_tx, request_id, body);
                        Flow::Continue
                    } else if let Some((request, caller, agent)) = start_request(&shared, role, connection_id, &body) {
                        match submit_start_deferred(&shared, caller, agent, connection_id, &revoke_rx, &late_tx, request_id, request) {
                            Some(m) => {
                                note_reply(&shared, &mut pending_calls, &m);
                                if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                            }
                            None => Flow::Continue,
                        }
                    } else {
                        let ctx = ReqCtx { shared: &shared, role, connection_id, peer_pid: peer.pid, revoke_rx: &mut revoke_rx };
                        match dispatch(ctx, &mut events, body).await {
                            Dispatch::Reply(res) => {
                                let m = match res {
                                    Ok(result) => ServerMessage::Response { request_id, result },
                                    Err(error) => ServerMessage::Error { request_id: Some(request_id), error },
                                };
                                note_reply(&shared, &mut pending_calls, &m);
                                if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                            }
                            Dispatch::Interrupted(reason) => Flow::Close(Some(reason)),
                            Dispatch::ReplyThenShutdown(result) => {
                                let _ = send(&mut wr, &ServerMessage::Response { request_id, result }, max).await;
                                shared.trigger_shutdown();
                                Flow::Continue
                            }
                        }
                    }
                }
            },
        };
        match flow {
            Flow::Continue => {}
            Flow::Close(reason) => break reason,
        }
    };
    // Events already published (for example the stop of the agent sessions)
    // are delivered before the closing notification.
    if let Some(rx) = events.as_mut() {
        while let Ok(event) = rx.try_recv() {
            if send(&mut wr, &ServerMessage::Event { event }, max)
                .await
                .is_err()
            {
                break;
            }
        }
    }
    if let Some(reason) = close_reason {
        let _ = send(&mut wr, &ServerMessage::Closing { reason }, max).await;
    }
    // Requests of this connection still queued (or a media import still
    // copying) are not executed: the revocation flag is raised before the
    // entry (and its sender) goes away.
    if let Some(entry) = lock(&shared.conns).remove(&connection_id) {
        let _ = entry.revoke.send(true);
    }
    // The console tickets do not outlive the connection that obtained them.
    shared.revoke_consoles_of(connection_id, Actor::System, TerminationCause::UiClosed);
    // Agent connection lost: its pending request is withdrawn, its live
    // session ends and its VM is stopped (ROADMAP 7.3, client loss).
    if role == Role::Pairing {
        let (withdrawn, live) = shared.agents.connection_closed(connection_id);
        if let Some(id) = withdrawn {
            shared.report_closed(vec![(id, SessionRequestOutcome::Withdrawn)]);
        }
        if let Some(session_id) = live {
            shared.send_cmd(Command::EndSessions {
                ids: vec![session_id],
                cause: TerminationCause::ClientLost,
                stop: true,
            });
        }
    }
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut wr).await;
}

/// Agent call waiting for its reply (session report, F5).
struct PendingCall {
    session_id: SessionId,
    call: &'static str,
    vm_id: Option<lightvm_proto::VmId>,
    operation_id: Option<lightvm_proto::OperationId>,
    run_id: Option<lightvm_proto::RunId>,
}

/// Agent calls recorded in the report: lease renewals, screenshot chunks
/// and the report calls themselves (recorded by the supervisor) are not.
fn pending_call(body: &Request) -> Option<PendingCall> {
    let Request::Agent { session, call } = body else {
        return None;
    };
    if matches!(
        call,
        AgentCall::RenewLease
            | AgentCall::ScreenshotChunk { .. }
            | AgentCall::ToolNote { .. }
            | AgentCall::Declare { .. }
            | AgentCall::Report { .. }
    ) {
        return None;
    }
    let run_id = match call {
        AgentCall::Screenshot {
            expected_run_id, ..
        } => Some(*expected_run_id),
        AgentCall::Submit { request } => request.expected_run_id,
        _ => None,
    };
    let name = match call {
        AgentCall::Submit { request } => match request.action {
            lightvm_proto::VmAction::Start => "submit.start",
            lightvm_proto::VmAction::Stop => "submit.stop",
            lightvm_proto::VmAction::ForceStop => "submit.force_stop",
            lightvm_proto::VmAction::Reset => "submit.reset",
            lightvm_proto::VmAction::Screenshot => "submit.screenshot",
            lightvm_proto::VmAction::Input(_) => "submit.input",
        },
        other => other.name(),
    };
    Some(PendingCall {
        session_id: session.session_id,
        call: name,
        vm_id: call.vm_id(),
        operation_id: call.operation_id(),
        run_id,
    })
}

/// Records the outcome of an agent call before its reply is written: the
/// record reaches the supervisor before any later request of the agent.
fn note_reply(
    shared: &Arc<Shared>,
    pending: &mut HashMap<RequestId, PendingCall>,
    m: &ServerMessage,
) {
    let (request_id, result) = match m {
        ServerMessage::Response { request_id, result } => (*request_id, Ok(result)),
        ServerMessage::Error {
            request_id: Some(request_id),
            error,
        } => (*request_id, Err(error)),
        _ => return,
    };
    let Some(p) = pending.remove(&request_id) else {
        return;
    };
    let mut rec = CallRecord {
        session_id: p.session_id,
        call: p.call,
        vm_id: p.vm_id,
        run_id: p.run_id,
        operation_id: p.operation_id,
        ok: true,
        error: None,
        screenshot: false,
    };
    match result {
        Ok(ResponseBody::Operation {
            operation: Some(op),
            ..
        })
        | Ok(ResponseBody::OperationStatus { operation: op }) => {
            rec.run_id = op.run_id.or(rec.run_id);
            if op.status == lightvm_proto::OperationStatus::Failed
                || op.status == lightvm_proto::OperationStatus::Discarded
            {
                rec.ok = false;
                rec.error = op.error;
            }
        }
        Ok(ResponseBody::AgentScreenshot { screenshot, .. }) => {
            rec.run_id = Some(screenshot.run_id);
            rec.screenshot = true;
        }
        Ok(ResponseBody::AgentVm { vm }) => rec.run_id = vm.run_id.or(rec.run_id),
        Ok(_) => {}
        Err(ReplyError::Operational(e)) => {
            rec.ok = false;
            rec.error = Some(e.code);
        }
        Err(ReplyError::Protocol(_)) => rec.ok = false,
    }
    shared.send_cmd(Command::AgentCallDone(Box::new(rec)));
}

/// Waits for the end of the session bound to an agent connection (never for
/// other connections).
async fn session_ended(rx: &mut Option<watch::Receiver<bool>>) {
    match rx {
        Some(rx) => flag_set(rx).await,
        None => std::future::pending().await,
    }
}

async fn recv_event(
    events: &mut Option<broadcast::Receiver<ServiceEvent>>,
) -> Result<ServiceEvent, broadcast::error::RecvError> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

struct ReqCtx<'a> {
    shared: &'a Arc<Shared>,
    role: Role,
    connection_id: u64,
    peer_pid: Option<u32>,
    revoke_rx: &'a mut watch::Receiver<bool>,
}

/// Serves a request in its own task; the reply goes through `late`.
#[allow(clippy::too_many_arguments)]
fn spawn_detached(
    shared: &Arc<Shared>,
    role: Role,
    connection_id: u64,
    peer_pid: Option<u32>,
    revoke_rx: &watch::Receiver<bool>,
    late: &tokio::sync::mpsc::Sender<ServerMessage>,
    request_id: RequestId,
    body: Request,
) {
    let shared = shared.clone();
    let mut revoke_rx = revoke_rx.clone();
    let late = late.clone();
    tokio::spawn(async move {
        let mut no_events = None;
        let ctx = ReqCtx {
            shared: &shared,
            role,
            connection_id,
            peer_pid,
            revoke_rx: &mut revoke_rx,
        };
        let m = match dispatch(ctx, &mut no_events, body).await {
            Dispatch::Reply(Ok(result)) => ServerMessage::Response { request_id, result },
            Dispatch::Reply(Err(error)) => ServerMessage::Error {
                request_id: Some(request_id),
                error,
            },
            // The connection is closing: nobody to answer.
            Dispatch::Interrupted(_) | Dispatch::ReplyThenShutdown(_) => return,
        };
        let _ = late.send(m).await;
    });
}

enum Dispatch {
    Reply(Result<ResponseBody, ReplyError>),
    ReplyThenShutdown(ResponseBody),
    Interrupted(CloseReason),
}

fn requires_ui(req: &Request) -> bool {
    !matches!(
        req,
        Request::Ping
            | Request::GetCapabilities
            | Request::RequestSession { .. }
            | Request::AwaitSession { .. }
            | Request::Agent { .. }
    )
}

/// Requests of agents only (`pairing` role).
fn requires_pairing(req: &Request) -> bool {
    matches!(
        req,
        Request::RequestSession { .. } | Request::AwaitSession { .. } | Request::Agent { .. }
    )
}

/// Caller towards the core: the UI is the local user; pairing has no VM
/// permissions at all (the agent session grant arrives in F4).
fn caller_for(role: Role) -> Option<Caller> {
    match role {
        Role::Ui => Some(Caller::User),
        Role::Pairing => None,
    }
}

async fn dispatch(
    ctx: ReqCtx<'_>,
    events: &mut Option<broadcast::Receiver<ServiceEvent>>,
    req: Request,
) -> Dispatch {
    let shared = ctx.shared;
    // First check: on receipt.
    if (requires_ui(&req) && ctx.role != Role::Ui)
        || (requires_pairing(&req) && ctx.role != Role::Pairing)
    {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::PermissionDenied,
            format!("operation {} not allowed for the role", req.name()),
        )));
    }
    let mut caller = caller_for(ctx.role);
    let mut agent_gate = None;
    let sup_req = match req {
        Request::Ping => SupervisorRequest::Ping,
        Request::GetCapabilities => SupervisorRequest::GetCapabilities,
        Request::ListVms => SupervisorRequest::ListVms,
        Request::VmStatus { vm_id } => SupervisorRequest::VmStatus(vm_id),
        Request::CreateVm { config } => SupervisorRequest::CreateVm(Box::new(config)),
        Request::UpdateVm {
            vm_id,
            expected_revision,
            config,
        } => SupervisorRequest::UpdateVm {
            vm_id,
            expected_revision,
            config: Box::new(config),
        },
        Request::SubmitVm { request } => SupervisorRequest::SubmitVm(Box::new(request)),
        Request::ListMedia => SupervisorRequest::ListMedia,
        Request::ImportMedia {
            source_path,
            media_kind,
            declared_origin,
        } => SupervisorRequest::ImportMedia {
            source_path,
            media_kind,
            declared_origin,
        },
        Request::DeleteVm {
            vm_id,
            expected_revision,
        } => SupervisorRequest::DeleteVm {
            vm_id,
            expected_revision,
        },
        Request::OpenConsole { vm_id } => SupervisorRequest::OpenConsole(vm_id),
        Request::ListBaselines { vm_id } => SupervisorRequest::ListBaselines(vm_id),
        Request::CreateBaseline {
            vm_id,
            label,
            allow_unclean,
        } => SupervisorRequest::CreateBaseline {
            vm_id,
            label,
            allow_unclean,
        },
        Request::RestoreBaseline { vm_id, baseline_id } => {
            SupervisorRequest::RestoreBaseline { vm_id, baseline_id }
        }
        Request::DeleteBaseline { vm_id, baseline_id } => {
            SupervisorRequest::DeleteBaseline { vm_id, baseline_id }
        }
        Request::Subscribe => {
            if events.is_none() {
                *events = Some(shared.events.subscribe());
            }
            return Dispatch::Reply(Ok(ResponseBody::Subscribed));
        }
        Request::ListConnections => {
            return Dispatch::Reply(Ok(ResponseBody::Connections {
                connections: shared.connections(),
            }));
        }
        Request::RevokeConnection { connection_id } => {
            return Dispatch::Reply(if shared.revoke_connection(connection_id, Actor::User) {
                Ok(ResponseBody::Revoked { connection_id })
            } else {
                Err(op_error(ErrorCode::StateConflict, "no such connection"))
            });
        }
        Request::CreatePairingCredential => {
            let ttl = shared.cfg.pairing_ttl;
            let r = lock(&shared.creds).issue(Role::Pairing, Some(ttl));
            return Dispatch::Reply(match r {
                Ok((_, c)) => Ok(ResponseBody::PairingCredential {
                    credential: c.to_hex(),
                    expires_in_s: ttl.as_secs(),
                }),
                Err(_) => Err(op_error(
                    ErrorCode::ResourceLimit,
                    "random generator not available",
                )),
            });
        }
        Request::EmergencyStop => {
            // Priority path: no queue slot, no passage through the supervisor
            // thread.
            let report = shared.emergency_stop(Actor::User).await;
            return Dispatch::Reply(Ok(ResponseBody::EmergencyStop(report)));
        }
        Request::SetParentLossPolicy { policy } => {
            return Dispatch::Reply(match policy.validate() {
                Ok(policy) => {
                    *lock(&shared.parent_loss_policy) = policy;
                    Ok(ResponseBody::ParentLossPolicy { policy })
                }
                Err(e) => Err(ReplyError::Operational(e)),
            });
        }
        Request::Shutdown => return Dispatch::ReplyThenShutdown(ResponseBody::ShuttingDown),
        Request::Agent { session, call } => {
            let SessionAuth { session_id, token } = session;
            let mutation = call.is_mutation();
            // Gate on receipt: bound to this connection, live, not paused
            // for a mutation.
            if let Err(e) = shared.agents.check(ctx.connection_id, session_id, mutation) {
                return Dispatch::Reply(Err(ReplyError::Operational(e)));
            }
            caller = Some(Caller::Agent {
                session_id,
                token: Some(token),
            });
            agent_gate = Some((session_id, mutation));
            SupervisorRequest::Agent(Box::new(call))
        }
        Request::RequestSession {
            client_name,
            purpose,
            requested_vms,
            requested_capabilities,
        } => {
            return Dispatch::Reply(request_session(
                shared,
                ctx.connection_id,
                ctx.peer_pid,
                &client_name,
                &purpose,
                requested_vms,
                requested_capabilities,
            ));
        }
        Request::AwaitSession { request_id } => {
            return Dispatch::Reply(await_session(shared, ctx.connection_id, request_id).await);
        }
        Request::GetAgentAccess => {
            return Dispatch::Reply(Ok(ResponseBody::AgentAccess {
                enabled: shared.agents.enabled(),
            }));
        }
        Request::SetAgentAccess { enabled } => {
            return Dispatch::Reply(set_agent_access(shared, ctx.connection_id, enabled).await);
        }
        Request::ListSessionRequests => {
            let (requests, closed) = shared.agents.list();
            shared.report_closed(closed);
            return Dispatch::Reply(Ok(ResponseBody::SessionRequests { requests }));
        }
        Request::ApproveSession {
            request_id,
            approval,
        } => {
            return Dispatch::Reply(
                approve_session(shared, ctx.connection_id, request_id, approval).await,
            );
        }
        Request::DenySession { request_id } => {
            return Dispatch::Reply(if shared.agents.deny(request_id, "denied") {
                shared.report_closed(vec![(request_id, SessionRequestOutcome::Denied)]);
                Ok(ResponseBody::SessionDenied { request_id })
            } else {
                Err(op_error(ErrorCode::NotFound, "session request not found"))
            });
        }
        Request::ListSessions => SupervisorRequest::ListSessions,
        Request::PauseSession { session_id } => {
            // Gate first (priority): agent mutations refused from now on;
            // the core follows (sync before its next admission).
            if !shared.agents.pause(session_id) {
                return Dispatch::Reply(Err(op_error(
                    ErrorCode::StateConflict,
                    "session not active",
                )));
            }
            return Dispatch::Reply(
                supervisor_call(
                    shared,
                    ctx.connection_id,
                    SupervisorRequest::PauseSession(session_id),
                )
                .await,
            );
        }
        Request::ResumeSession { session_id } => {
            let r = supervisor_call(
                shared,
                ctx.connection_id,
                SupervisorRequest::ResumeSession(session_id),
            )
            .await;
            if r.is_ok() {
                shared.agents.resume(session_id);
            }
            return Dispatch::Reply(r);
        }
        Request::RevokeSession { session_id } => {
            // Point of revocation: the gate closes and the agent connection
            // closes before the core records the revocation.
            if let Some(conn) = shared.agents.revoke(session_id) {
                shared.close_agent_connection(conn);
            }
            return Dispatch::Reply(
                supervisor_call(
                    shared,
                    ctx.connection_id,
                    SupervisorRequest::EndSession {
                        session_id,
                        cause: TerminationCause::Revoked,
                        stop: false,
                    },
                )
                .await,
            );
        }
        Request::SetInputOwner { vm_id, owner } => {
            return Dispatch::Reply(
                supervisor_call(
                    shared,
                    ctx.connection_id,
                    SupervisorRequest::SetInputOwner { vm_id, owner },
                )
                .await,
            );
        }
        Request::ListSessionReports => SupervisorRequest::Report(Box::new(ReportRequest::List)),
        Request::GetSessionReport { session_id } => {
            SupervisorRequest::Report(Box::new(ReportRequest::Header(session_id)))
        }
        Request::GetSessionReportEvents {
            session_id,
            offset,
            limit,
        } => SupervisorRequest::Report(Box::new(ReportRequest::Events {
            session_id,
            offset,
            limit,
        })),
        Request::GetReportScreenshotChunk {
            session_id,
            shot,
            index,
        } => SupervisorRequest::Report(Box::new(ReportRequest::ShotChunk {
            session_id,
            shot,
            index,
        })),
        Request::SetSessionVerdict {
            session_id,
            outcome,
            rationale,
        } => SupervisorRequest::Report(Box::new(ReportRequest::Verdict {
            session_id,
            outcome,
            rationale,
        })),
        Request::VerifySession {
            session_id,
            verifier,
        } => SupervisorRequest::Report(Box::new(ReportRequest::Verify {
            session_id,
            verifier,
        })),
        Request::ExportSessionReport {
            session_id,
            format,
            destination,
            overwrite,
            include_screenshots,
        } => SupervisorRequest::Report(Box::new(ReportRequest::Export {
            session_id,
            format,
            destination,
            overwrite,
            include_screenshots,
        })),
        Request::DeleteSessionReport { session_id } => {
            SupervisorRequest::Report(Box::new(ReportRequest::Delete(session_id)))
        }
    };
    let reply_rx = match enqueue(
        shared,
        caller,
        agent_gate,
        ctx.connection_id,
        ctx.revoke_rx,
        sup_req,
    ) {
        Ok(rx) => rx,
        Err(e) => return Dispatch::Reply(Err(e)),
    };
    tokio::select! {
        biased;
        _ = flag_set(ctx.revoke_rx) => Dispatch::Interrupted(CloseReason::Revoked),
        r = reply_rx => Dispatch::Reply(r.unwrap_or_else(|_| Err(op_error(
            ErrorCode::StateConflict,
            "service shutting down",
        )))),
    }
}

type ReplyRx = oneshot::Receiver<Result<ResponseBody, ReplyError>>;

/// Agent session of a request and whether it mutates a VM (gate check).
type AgentGate = Option<(SessionId, bool)>;

/// Caller, agent gate and VM request of a start (`submit_vm(start)` of the
/// UI, or `agent { submit(start) }` of an agent session whose gate is
/// open), served with a deferred reply.
fn start_request(
    shared: &Arc<Shared>,
    role: Role,
    connection_id: u64,
    body: &Request,
) -> Option<(lightvm_proto::VmRequest, Option<Caller>, AgentGate)> {
    match body {
        Request::SubmitVm { request } if request.action == lightvm_proto::VmAction::Start => {
            Some((request.clone(), caller_for(role), None))
        }
        Request::Agent {
            session,
            call: AgentCall::Submit { request },
        } if request.action == lightvm_proto::VmAction::Start
            && role == Role::Pairing
            && shared
                .agents
                .check(connection_id, session.session_id, true)
                .is_ok() =>
        {
            Some((
                request.clone(),
                Some(Caller::Agent {
                    session_id: session.session_id,
                    token: Some(session.token.clone()),
                }),
                Some((session.session_id, true)),
            ))
        }
        _ => None,
    }
}

/// Session control of the UI (approval, pause, resume, revocation, input
/// owner, agent access off) sent to the supervisor **without a queue slot**:
/// a full queue (agents flooding) never prevents the user from pausing,
/// revoking or taking the input back. The supervisor serves commands in
/// order; each agent connection has at most one request ahead of it.
async fn supervisor_call(
    shared: &Arc<Shared>,
    _connection_id: u64,
    req: SupervisorRequest,
) -> Result<ResponseBody, ReplyError> {
    let (reply, rx) = oneshot::channel();
    let sent = shared.send_cmd(Command::Control {
        req,
        epoch: shared.emergency_epoch.load(Ordering::SeqCst),
        reply,
    });
    if !sent {
        return Err(op_error(ErrorCode::StateConflict, "service shutting down"));
    }
    rx.await
        .unwrap_or_else(|_| Err(op_error(ErrorCode::StateConflict, "service shutting down")))
}

/// `request_session` (agent): new pending request with its verification code.
fn request_session(
    shared: &Arc<Shared>,
    connection_id: u64,
    peer_pid: Option<u32>,
    client_name: &str,
    purpose: &str,
    requested_vms: Vec<lightvm_proto::VmId>,
    requested_capabilities: lightvm_proto::CapabilitySet,
) -> Result<ResponseBody, ReplyError> {
    match shared.agents.open_request(
        connection_id,
        peer_pid,
        client_name,
        purpose,
        requested_vms,
        requested_capabilities,
    ) {
        Ok((info, closed)) => {
            shared.report_closed(closed);
            shared.audit(
                Actor::System,
                AuditAction::SessionRequest,
                AuditOutcome::Accepted,
                None,
                None,
            );
            let reply = ResponseBody::SessionRequested {
                request_id: info.request_id,
                verification_code: info.verification_code.clone(),
                expires_in_s: shared.cfg.session_request_ttl.as_secs(),
            };
            let _ = shared
                .events
                .send(ServiceEvent::SessionRequest { request: info });
            Ok(reply)
        }
        Err(e) => {
            shared.audit(
                Actor::System,
                AuditAction::SessionRequest,
                AuditOutcome::Denied,
                None,
                Some(e.code),
            );
            Err(ReplyError::Operational(e))
        }
    }
}

/// `await_session` (agent): the user's decision, or the expiry.
async fn await_session(
    shared: &Arc<Shared>,
    connection_id: u64,
    request_id: SessionRequestId,
) -> Result<ResponseBody, ReplyError> {
    match shared
        .agents
        .await_decision(connection_id, request_id)
        .map_err(ReplyError::Operational)?
    {
        AwaitDecision::Ready(d) => *d,
        AwaitDecision::Wait(rx, left) => match tokio::time::timeout(left, rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => Err(op_error(ErrorCode::StateConflict, "service shutting down")),
            Err(_) => {
                // Expired: purged (event and audit) by the listing.
                let (_, closed) = shared.agents.list();
                shared.report_closed(closed);
                Err(crate::agents::expired())
            }
        },
    }
}

/// Validation of an approval (ROADMAP 7.3: the user authorizes a bounded set
/// of operations for a defined duration).
fn validate_approval(a: &SessionApproval) -> Result<(lightvm_proto::Quotas, i64), PublicError> {
    if !(MIN_SESSION_DURATION_S..=MAX_SESSION_DURATION_S).contains(&a.duration_s) {
        return Err(PublicError::invalid_argument(format!(
            "duration_s outside {MIN_SESSION_DURATION_S}..={MAX_SESSION_DURATION_S}"
        )));
    }
    if a.capabilities.is_empty() {
        return Err(PublicError::invalid_argument("no capability granted"));
    }
    let lease = a.lease_ms.map_or(LEASE_MAX_MILLIS, i64::from);
    if !(i64::from(MIN_LEASE_MS)..=LEASE_MAX_MILLIS).contains(&lease) {
        return Err(PublicError::invalid_argument(format!(
            "lease_ms outside {MIN_LEASE_MS}..={LEASE_MAX_MILLIS}"
        )));
    }
    let mut quotas = a.quotas.unwrap_or(AGENT_DEFAULT_QUOTAS);
    quotas.max_screenshot_bytes = quotas.max_screenshot_bytes.min(AGENT_MAX_SCREENSHOT_BYTES);
    Ok((quotas, lease))
}

/// `approve_session` (UI): the core creates the session, the service binds it
/// to the agent connection and delivers the grant to the waiting agent.
async fn approve_session(
    shared: &Arc<Shared>,
    connection_id: u64,
    request_id: SessionRequestId,
    approval: SessionApproval,
) -> Result<ResponseBody, ReplyError> {
    let (quotas, lease_ms) = validate_approval(&approval).map_err(ReplyError::Operational)?;
    let (info, closed) = shared
        .agents
        .begin_approval(request_id)
        .map_err(ReplyError::Operational)?;
    shared.report_closed(closed);
    let mut b = [0u8; 32];
    if getrandom::fill(&mut b).is_err() {
        shared.agents.abort_approval(request_id);
        return Err(op_error(
            ErrorCode::ResourceLimit,
            "random generator not available",
        ));
    }
    let now = UtcTimestamp::now();
    let Some(expires_at) = now.checked_add_millis(i64::from(approval.duration_s) * 1000) else {
        shared.agents.abort_approval(request_id);
        return Err(op_error(ErrorCode::InvalidArgument, "expiry out of range"));
    };
    let new = NewSession {
        vm_id: approval.vm_id,
        capabilities: approval.capabilities.clone(),
        quotas,
        expires_at,
        lease_ms,
        give_input: approval.give_input,
        token: SessionToken::from_bytes(b),
        connection_id: info.connection_id,
        client_name: info.client_name.clone(),
        purpose: info.purpose.clone(),
    };
    b.fill(0);
    let r = supervisor_call(
        shared,
        connection_id,
        SupervisorRequest::CreateSession(Box::new(new)),
    )
    .await;
    let granted = match r {
        Ok(g @ ResponseBody::SessionGranted { .. }) => g,
        Ok(_) => {
            shared.agents.abort_approval(request_id);
            return Err(op_error(ErrorCode::InfrastructureError, "unexpected reply"));
        }
        Err(e) => {
            shared.agents.abort_approval(request_id);
            return Err(e);
        }
    };
    let ResponseBody::SessionGranted {
        session_id,
        ref grant,
        ref lease,
        ..
    } = granted
    else {
        return Err(op_error(ErrorCode::InfrastructureError, "unexpected reply"));
    };
    let session = SessionInfo {
        session_id,
        vm_id: approval.vm_id,
        connection_id: Some(info.connection_id),
        client_name: info.client_name.clone(),
        purpose: info.purpose.clone(),
        state: SessionState::Active,
        termination: None,
        capabilities: grant.capabilities.clone(),
        quotas: grant.quotas,
        usage: SessionUsage::default(),
        created_at: lease.renewed_at,
        expires_at: grant.expires_at,
        lease_expires_at: lease.expires_at,
        input_owner: approval.give_input && grant.capabilities.contains(&Capability::VmInput),
    };
    if !shared
        .agents
        .complete_approval(request_id, session_id, granted.clone())
    {
        // The agent connection is gone: the session it never received ends.
        shared.send_cmd(Command::EndSessions {
            ids: vec![session_id],
            cause: TerminationCause::ClientLost,
            stop: false,
        });
        return Err(op_error(
            ErrorCode::StateConflict,
            "the agent connection closed before the approval",
        ));
    }
    shared.audit(
        Actor::User,
        AuditAction::SessionRequest,
        AuditOutcome::Succeeded,
        Some(session_id),
        None,
    );
    let _ = shared.events.send(ServiceEvent::SessionRequestClosed {
        request_id,
        outcome: SessionRequestOutcome::Approved,
    });
    Ok(ResponseBody::Session { session })
}

/// `set_agent_access` (UI). Turning it off denies the pending requests and
/// revokes every live session (their connections close; VMs keep running).
async fn set_agent_access(
    shared: &Arc<Shared>,
    connection_id: u64,
    enabled: bool,
) -> Result<ResponseBody, ReplyError> {
    let was = shared.agents.set_enabled(enabled);
    if was != enabled {
        shared.audit(
            Actor::User,
            if enabled {
                AuditAction::AgentAccessEnable
            } else {
                AuditAction::AgentAccessDisable
            },
            AuditOutcome::Succeeded,
            None,
            None,
        );
        let _ = shared.events.send(ServiceEvent::AgentAccess { enabled });
    }
    if !enabled {
        shared.close_all_agent_sessions("agent_access_disabled", TerminationCause::Revoked, false);
        supervisor_call(
            shared,
            connection_id,
            SupervisorRequest::EndAllSessions {
                cause: TerminationCause::Revoked,
                stop: false,
            },
        )
        .await?;
    }
    Ok(ResponseBody::AgentAccess { enabled })
}

/// Queues a request for the supervisor thread (queue slot, revocation and
/// Emergency Stop generation captured now): synchronous, so requests of a
/// connection reach the supervisor in the order they were received.
fn enqueue(
    shared: &Arc<Shared>,
    caller: Option<Caller>,
    agent: Option<(SessionId, bool)>,
    connection_id: u64,
    revoke_rx: &watch::Receiver<bool>,
    req: SupervisorRequest,
) -> Result<ReplyRx, ReplyError> {
    let Ok(permit) = shared.queue.clone().try_acquire_owned() else {
        return Err(op_error(ErrorCode::ResourceLimit, "supervisor queue full"));
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let sent = shared.send_cmd(Command::Request {
        req,
        caller,
        connection_id,
        revoked: revoke_rx.clone(),
        epoch: shared.emergency_epoch.load(Ordering::SeqCst),
        agent,
        reply: reply_tx,
        _permit: permit,
    });
    if !sent {
        return Err(op_error(ErrorCode::StateConflict, "service shutting down"));
    }
    Ok(reply_rx)
}

/// `submit_vm` of a start: queued now (order kept), the reply awaited in its
/// own task and sent through `late`, because it arrives only after the
/// start's file checks (a minute for a large ISO) and the connection must
/// keep serving the other requests and the events meanwhile. Returns the
/// message to send at once if the request was not queued.
#[allow(clippy::too_many_arguments)]
fn submit_start_deferred(
    shared: &Arc<Shared>,
    caller: Option<Caller>,
    agent: Option<(SessionId, bool)>,
    connection_id: u64,
    revoke_rx: &watch::Receiver<bool>,
    late: &tokio::sync::mpsc::Sender<ServerMessage>,
    request_id: RequestId,
    request: lightvm_proto::VmRequest,
) -> Option<ServerMessage> {
    let Some(caller) = caller else {
        return Some(ServerMessage::Error {
            request_id: Some(request_id),
            error: op_error(
                ErrorCode::PermissionDenied,
                "operation submit_vm not allowed for the role",
            ),
        });
    };
    let req = if agent.is_some() {
        SupervisorRequest::Agent(Box::new(AgentCall::Submit { request }))
    } else {
        SupervisorRequest::SubmitVm(Box::new(request))
    };
    let rx = match enqueue(shared, Some(caller), agent, connection_id, revoke_rx, req) {
        Ok(rx) => rx,
        Err(error) => {
            return Some(ServerMessage::Error {
                request_id: Some(request_id),
                error,
            });
        }
    };
    let mut revoke_rx = revoke_rx.clone();
    let late = late.clone();
    tokio::spawn(async move {
        let r = tokio::select! {
            biased;
            // The connection is closing: nobody to answer.
            _ = flag_set(&mut revoke_rx) => return,
            r = rx => r,
        };
        let m = match r {
            Ok(Ok(result)) => ServerMessage::Response { request_id, result },
            Ok(Err(error)) => ServerMessage::Error {
                request_id: Some(request_id),
                error,
            },
            Err(_) => ServerMessage::Error {
                request_id: Some(request_id),
                error: op_error(ErrorCode::StateConflict, "service shutting down"),
            },
        };
        let _ = late.send(m).await;
    });
    None
}

/// Waits for the flag to become true (or for the sender to go away) without
/// holding the reference to the value beyond the wait.
async fn flag_set(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}
