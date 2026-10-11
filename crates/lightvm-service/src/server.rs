//! Service: IPC endpoint, handshake, connection registry with revocation,
//! request dispatcher, controlled shutdown.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use tokio::io::{AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::{Semaphore, broadcast, oneshot, watch};

use lightvm_proto::ipc::{
    ClientMessage, CloseReason, ConnectionInfo, EmergencyStopReport, Request, RequestId,
    ResponseBody, Role, SUPPORTED_PROTOCOL_VERSIONS, ServerMessage, ServiceEvent, ServiceState,
    StopReason,
};
use lightvm_proto::{
    Actor, Caller, ErrorCode, ProtocolError, ProtocolErrorCode, PublicError, ReplyError,
    TerminationCause,
};

use crate::credential::{Credential, CredentialId, CredentialStore};
use crate::display::DisplayRegistry;
use crate::engine::VmEngine;
use crate::frame::{self, FrameError};
use crate::lock::{InstanceLock, LockError};
use crate::ratelimit::{RateLimit, TokenBucket};
use crate::supervisor::{
    Backend, Command, EmergencyStop, ParentLossPolicy, SupervisorRequest, SupervisorThread,
    unauthenticated,
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
}

impl ServiceConfig {
    /// Default values for a catalog.
    pub fn new(catalog_dir: impl Into<PathBuf>) -> Self {
        Self {
            catalog_dir: catalog_dir.into(),
            runtime_dir: None,
            max_frame: frame::DEFAULT_MAX_FRAME,
            rate_limit: RateLimit::default(),
            queue_capacity: 64,
            max_connections: 16,
            handshake_timeout: Duration::from_secs(5),
            pairing_ttl: Duration::from_secs(600),
            shutdown_timeout: Duration::from_secs(30),
            parent_loss_policy: ParentLossPolicy::default(),
            display_dir: None,
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
        // Revoke first: after this point no new agent mutation is accepted.
        // Then termination through the backend handler, outside the queue and
        // outside the supervisor thread.
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

    // Shutdown driver.
    let drv = shared.clone();
    tokio::spawn(async move {
        flag_set(&mut shutdown_rx).await;
        drv.stopping.store(true, Ordering::SeqCst);
        // 1. No new connections; the connections receive `closing`.
        let _ = drv.closing.send(true);
        let _ = accept_task.await;
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
            stopped: stopped_rx,
        },
        ui_credential,
    ))
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
    let mut bucket = TokenBucket::new(shared.cfg.rate_limit);
    let mut events: Option<broadcast::Receiver<ServiceEvent>> = None;
    // Replies of long requests served without blocking the connection (media
    // import: the reply arrives after the copy, events keep flowing).
    let (late_tx, mut late_rx) = tokio::sync::mpsc::channel::<ServerMessage>(4);

    let close_reason: Option<CloseReason> = loop {
        let flow = tokio::select! {
            biased;
            _ = flag_set(&mut revoke_rx) => Flow::Close(Some(CloseReason::Revoked)),
            _ = flag_set(&mut closing_rx) => Flow::Close(Some(CloseReason::ServiceStopping)),
            Some(m) = late_rx.recv() => {
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
                    if !bucket.try_acquire() {
                        let m = err(Some(request_id), ErrorCode::ResourceLimit, &format!(
                            "request rate over the limit; retry in {} ms",
                            bucket.retry_after().as_millis()
                        ));
                        if send(&mut wr, &m, max).await.is_err() { Flow::Close(None) } else { Flow::Continue }
                    } else if matches!(
                        body,
                        Request::ImportMedia { .. }
                            | Request::CreateBaseline { .. }
                            | Request::RestoreBaseline { .. }
                    ) {
                        spawn_detached(&shared, role, connection_id, &revoke_rx, &late_tx, request_id, body);
                        Flow::Continue
                    } else {
                        let ctx = ReqCtx { shared: &shared, role, connection_id, revoke_rx: &mut revoke_rx };
                        match dispatch(ctx, &mut events, body).await {
                            Dispatch::Reply(res) => {
                                let m = match res {
                                    Ok(result) => ServerMessage::Response { request_id, result },
                                    Err(error) => ServerMessage::Error { request_id: Some(request_id), error },
                                };
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
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut wr).await;
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
    revoke_rx: &'a mut watch::Receiver<bool>,
}

/// Serves a request in its own task; the reply goes through `late`.
fn spawn_detached(
    shared: &Arc<Shared>,
    role: Role,
    connection_id: u64,
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
    !matches!(req, Request::Ping | Request::GetCapabilities)
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
    if requires_ui(&req) && ctx.role != Role::Ui {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::PermissionDenied,
            format!("operation {} not allowed for the role", req.name()),
        )));
    }
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
    };
    let Ok(permit) = shared.queue.clone().try_acquire_owned() else {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::ResourceLimit,
            "supervisor queue full",
        )));
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let sent = shared.send_cmd(Command::Request {
        req: sup_req,
        caller: caller_for(ctx.role),
        connection_id: ctx.connection_id,
        revoked: ctx.revoke_rx.clone(),
        epoch: shared.emergency_epoch.load(Ordering::SeqCst),
        reply: reply_tx,
        _permit: permit,
    });
    if !sent {
        return Dispatch::Reply(Err(op_error(
            ErrorCode::StateConflict,
            "service shutting down",
        )));
    }
    tokio::select! {
        biased;
        _ = flag_set(ctx.revoke_rx) => Dispatch::Interrupted(CloseReason::Revoked),
        r = reply_rx => Dispatch::Reply(r.unwrap_or_else(|_| Err(op_error(
            ErrorCode::StateConflict,
            "service shutting down",
        )))),
    }
}

/// Waits for the flag to become true (or for the sender to go away) without
/// holding the reference to the value beyond the wait.
async fn flag_set(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}
