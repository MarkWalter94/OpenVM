//! Supervisor as a child process of the UI (F1-11, ADR-0002; prerequisite of
//! P-2 in `docs/STATUS.md`).
//!
//! The UI starts `lightvm-service` with structured arguments and keeps three
//! anonymous pipes (contract in `crates/lightvm-service/src/main.rs`):
//! - stdout: a single JSON `ready` line with IPC endpoint and UI credential;
//! - stdin: life pipe (EOF = UI terminated) and `shutdown` line for the
//!   controlled stop;
//! - stderr: inherited, diagnostics without secrets.
//!
//! After the `ready` line the UI talks to the service only via authenticated
//! IPC, with four connections: requests (reply by `request_id`), events
//! (`subscribe`), a fast lane for console tickets and keyboard/mouse input
//! (never queued behind a start or a stop) and a slow lane for ISO imports
//! (copy and SHA-256 of several GiB). Reads of a frame cannot be interrupted
//! safely, so the lanes do not share a connection.
//!
//! The UI opens neither QMP nor VM files: start, stop, input and Emergency
//! Stop go through the service core (ROADMAP 6.1). The console display is
//! reached only with the per-run ticket of `open_console` (ADR-0013).

use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    ConsoleTicket, ParentLossPolicy, Request, ResponseBody, ServerMessage, ServiceEvent, VmSummary,
};
use lightvm_proto::{
    ErrorCode, HostCapacity, InputBatch, MediaKind, MediaRecord, OperationId, OperationStatus,
    ReplyError, RunId, VmAction, VmConfig, VmId, VmRequest,
};
use lightvm_service::client::Client;
use lightvm_service::credential::Credential;
use tokio::sync::mpsc;

/// Maximum wait for the `ready` line.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum wait for the exit after the controlled stop request: beyond the
/// service stop timeout (30 s, then Emergency Stop of the remaining VMs), so
/// the UI does not kill it halfway through the orderly stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(45);

/// Start parameters of the supervisor.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// `lightvm-service` executable.
    pub executable: PathBuf,
    /// Catalog directory.
    pub catalog: PathBuf,
    /// Approved QEMU distribution; without it, the service refuses starts.
    pub qemu_dir: Option<PathBuf>,
    /// Runtime directory (IPC socket on Unix, private QEMU sockets).
    pub runtime_dir: Option<PathBuf>,
    /// Audit file; without it, the service uses the user profile
    /// (`%LOCALAPPDATA%\LightVM\audit.jsonl` on Windows).
    pub audit_log: Option<PathBuf>,
    /// Fate of the VMs if the UI terminates without a controlled stop.
    pub policy: ParentLossPolicy,
}

impl ServiceConfig {
    /// Executable next to the UI one (`lightvm-service[.exe]`).
    pub fn executable_next_to_ui() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.with_file_name(format!("lightvm-service{}", std::env::consts::EXE_SUFFIX)))
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.executable);
        c.arg("--catalog").arg(&self.catalog);
        if let Some(q) = &self.qemu_dir {
            c.arg("--qemu-dir").arg(q);
        }
        if let Some(r) = &self.runtime_dir {
            c.arg("--runtime-dir").arg(r);
        }
        if let Some(a) = &self.audit_log {
            c.arg("--audit-log").arg(a);
        }
        match self.policy {
            ParentLossPolicy::KillVms => {
                c.args(["--parent-loss-policy", "kill-vms"]);
            }
            ParentLossPolicy::ShutdownVms { powerdown_s } => {
                c.args(["--parent-loss-policy", "shutdown-vms"])
                    .arg("--parent-loss-powerdown-s")
                    .arg(powerdown_s.to_string());
            }
            ParentLossPolicy::KeepVms => {
                c.args(["--parent-loss-policy", "keep-vms"]);
            }
        }
        c.args(["--parent-watch", "stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        c
    }
}

/// Initial state of the connection, before the handshake.
pub const CONNECTING: &str = "connecting";

/// Failure of a request, readable, with the "not provided by this
/// supervisor" case recognisable (`unsupported`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The supervisor does not implement the operation.
    pub unsupported: bool,
    /// Readable description.
    pub message: String,
}

impl Failure {
    /// Failure from a description.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            unsupported: false,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// State of a request whose outcome a view waits for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RequestStatus<T> {
    /// Never requested.
    #[default]
    Idle,
    /// Sent, outcome not arrived yet.
    Pending,
    /// Succeeded.
    Done(T),
    /// Failed.
    Failed(Failure),
}

impl<T> RequestStatus<T> {
    /// Waiting for the outcome.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Approved media known to the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    /// The list was received at least once.
    pub loaded: bool,
    /// Approved media.
    pub records: Vec<MediaRecord>,
    /// Last read error, if any.
    pub error: Option<Failure>,
}

impl MediaState {
    /// Media of a kind, most recent first.
    pub fn of_kind(&self, kind: MediaKind) -> Vec<&MediaRecord> {
        let mut v: Vec<&MediaRecord> = self.records.iter().filter(|m| m.kind == kind).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.approved_at));
        v
    }
}

/// Request of a console ticket. The ticket itself (secret) is not part of
/// the state: it is handed over once with [`Supervisor::take_console_ticket`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleRequest {
    /// VM.
    pub vm_id: VmId,
    /// Outcome: on success, the run the ticket is valid for.
    pub status: RequestStatus<RunId>,
}

/// State seen by the UI, updated by the IPC threads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceState {
    /// Connection state, readable.
    pub connection: String,
    /// Connection established and handshake succeeded.
    pub connected: bool,
    /// Backend reported by the service.
    pub backend: Option<String>,
    /// Accelerator reported by the service.
    pub accelerator: Option<String>,
    /// Host capacity reported by the service, if measured.
    pub host: Option<HostCapacity>,
    /// VMs of the catalog.
    pub vms: Vec<VmSummary>,
    /// Outcome of the last requested action, readable.
    pub last_outcome: Option<String>,
    /// UI loss policy reported by the service.
    pub policy: Option<ParentLossPolicy>,
    /// Approved media of the registry.
    pub media: MediaState,
    /// Last ISO import.
    pub import: RequestStatus<MediaRecord>,
    /// Last VM creation.
    pub create: RequestStatus<VmId>,
    /// Last VM deletion.
    pub delete: RequestStatus<VmId>,
    /// Last console ticket request.
    pub console: Option<ConsoleRequest>,
    /// Last refused input batch, readable (cleared by the next accepted one).
    pub input_error: Option<String>,
}

impl ServiceState {
    /// VM by identifier.
    pub fn vm(&self, id: VmId) -> Option<&VmSummary> {
        self.vms.iter().find(|v| v.vm_id == id)
    }

    /// VMs with a QEMU process (starting, running, stopping, recovering).
    pub fn active_vms(&self) -> impl Iterator<Item = &VmSummary> {
        self.vms.iter().filter(|v| v.state.has_process())
    }
}

/// Actions requested by the UI from the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceCommand {
    /// Re-reads the VM list and the capabilities.
    Refresh,
    /// Creates a VM in the service's catalog (wizard).
    Create(Box<VmConfig>),
    /// Starts a VM.
    Start(VmId),
    /// Orderly stop of a VM (ACPI, then termination after the service
    /// timeout).
    Stop(VmId),
    /// Forced stop with OS primitives.
    ForceStop(VmId),
    /// Guest reset of the given run.
    Reset {
        /// VM.
        vm_id: VmId,
        /// Run to reset (a later run is not touched).
        run_id: RunId,
    },
    /// Deletion of a stopped VM.
    Delete {
        /// VM.
        vm_id: VmId,
        /// Revision the deletion is based on.
        revision: u64,
    },
    /// Emergency Stop (priority path of the service).
    EmergencyStop,
    /// Changes the fate of the VMs on UI loss (applies immediately).
    SetPolicy(ParentLossPolicy),
    /// Saves resources and limits of a stopped VM.
    UpdateVm {
        /// VM.
        vm_id: VmId,
        /// Revision the change is based on.
        revision: u64,
        /// New configuration.
        config: Box<VmConfig>,
    },
    /// Opens a VM by name (command line): applies the requested changes if
    /// the VM is stopped, then starts it.
    OpenVm {
        /// VM name.
        name: String,
        /// Changes to resources and limits.
        changes: crate::vm_settings::VmChanges,
        /// If the VM does not exist, create it with the default values plus
        /// the changes (without installation media).
        create: bool,
    },
    /// Re-reads the approved media.
    ListMedia,
    /// Imports an ISO chosen by the user with the native file picker.
    ImportMedia {
        /// File chosen in the picker.
        path: PathBuf,
        /// Kind of media.
        kind: MediaKind,
        /// Origin declared to the registry (text, never interpreted).
        origin: String,
    },
    /// Requests a per-run console ticket.
    OpenConsole(VmId),
    /// Keyboard and mouse towards the guest.
    Input(InputBatch),
}

/// Supervisor seen by the UI: the real child process ([`Service`]) or, in
/// headless tests, a stand-in built by the test.
pub trait Supervisor: Send {
    /// PID of the supervisor.
    fn pid(&self) -> u32;
    /// Copy of the current state.
    fn state(&self) -> ServiceState;
    /// Queues an action. Returns `false` if the channel is closed.
    fn send(&self, c: ServiceCommand) -> bool;
    /// The supervisor is still alive.
    fn is_alive(&mut self) -> bool;
    /// Controlled stop; returns the exit code and whether forcing was needed.
    fn stop(&mut self, timeout: Duration) -> (Option<i32>, bool);
    /// Hands over (once) the console ticket received for `vm_id`.
    fn take_console_ticket(&self, vm_id: VmId) -> Option<ConsoleTicket>;
}

/// Supervisor start errors.
#[derive(Debug)]
pub enum StartError {
    /// Process not started.
    Spawn(std::io::Error),
    /// The service exited before the `ready` line (exit code, if known).
    ExitedBeforeReady(Option<i32>),
    /// No `ready` line within [`READY_TIMEOUT`].
    Timeout,
    /// Invalid `ready` line.
    InvalidLine(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "starting lightvm-service failed: {e}"),
            Self::ExitedBeforeReady(Some(3)) => {
                write!(
                    f,
                    "the catalog is already used by another lightvm-service instance"
                )
            }
            Self::ExitedBeforeReady(c) => {
                write!(f, "lightvm-service exited before being ready (code {c:?})")
            }
            Self::Timeout => write!(f, "lightvm-service did not report being ready"),
            Self::InvalidLine(m) => write!(f, "invalid lightvm-service start line: {m}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Data of the `ready` line (the credential is never logged).
struct Ready {
    pid: u32,
    endpoint: String,
    credential: Credential,
}

fn parse_ready(line: &str) -> Result<Ready, StartError> {
    let v: serde_json::Value =
        serde_json::from_str(line).map_err(|e| StartError::InvalidLine(e.to_string()))?;
    if v.get("event").and_then(|e| e.as_str()) != Some("ready") {
        return Err(StartError::InvalidLine("event other than ready".into()));
    }
    let pid = v
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| StartError::InvalidLine("missing pid".into()))?;
    let endpoint = v
        .get("endpoint")
        .and_then(|e| e.as_str())
        .ok_or_else(|| StartError::InvalidLine("missing endpoint".into()))?
        .to_owned();
    let credential = v
        .get("credential")
        .and_then(|c| c.as_str())
        .and_then(Credential::from_hex)
        .ok_or_else(|| StartError::InvalidLine("missing or malformed credential".into()))?;
    Ok(Ready {
        pid,
        endpoint,
        credential,
    })
}

/// Console ticket received and not yet handed to the console.
type TicketSlot = Arc<Mutex<Option<ConsoleTicket>>>;

/// Supervisor started by the UI. On drop it requests the controlled stop
/// and, once [`STOP_TIMEOUT`] has expired, kills the process.
#[derive(Debug)]
pub struct Service {
    child: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    commands: mpsc::UnboundedSender<ServiceCommand>,
    state: Arc<Mutex<ServiceState>>,
    ticket: TicketSlot,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Service {
    /// Starts the service, waits for the `ready` line and opens the IPC
    /// connections in their own threads. `repaint` is called at every state
    /// change.
    pub fn start(
        cfg: &ServiceConfig,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, StartError> {
        let mut child = cfg.command().spawn().map_err(StartError::Spawn)?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or(StartError::Timeout)?;

        // The `ready` line is read in a thread: `read_line` has no timeout.
        let (tx_line, rx_line) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("service-stdout".into())
            .spawn(move || {
                let mut r = BufReader::new(stdout);
                let mut line = String::new();
                let outcome = r.read_line(&mut line).map(|n| (n, line));
                let _ = tx_line.send(outcome);
                // The rest of stdout is not used: discarded until closed.
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            })
            .map_err(StartError::Spawn)?;
        let ready = match rx_line.recv_timeout(READY_TIMEOUT) {
            Ok(Ok((n, line))) if n > 0 => parse_ready(line.trim()),
            Ok(_) => Err(StartError::ExitedBeforeReady(
                child.wait().ok().and_then(|s| s.code()),
            )),
            Err(_) => Err(StartError::Timeout),
        };
        let ready = match ready {
            Ok(p) => p,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

        let state = Arc::new(Mutex::new(ServiceState {
            connection: CONNECTING.into(),
            ..ServiceState::default()
        }));
        let ticket: TicketSlot = Arc::new(Mutex::new(None));
        let (commands, rx) = mpsc::unbounded_channel();
        let Ready {
            pid,
            endpoint,
            credential,
        } = ready;
        let lanes = Lanes {
            state: state.clone(),
            ticket: ticket.clone(),
            repaint,
            tx: commands.clone(),
        };
        std::thread::Builder::new()
            .name("service-ipc".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        lanes.update(|s| s.connection = format!("IPC runtime not started: {e}"));
                        return;
                    }
                };
                rt.block_on(ipc_loop(endpoint, credential, rx, lanes));
            })
            .map_err(StartError::Spawn)?;
        Ok(Self {
            child,
            stdin,
            pid,
            commands,
            state,
            ticket,
        })
    }

    /// PID of the supervisor reported in the `ready` line.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Copy of the current state.
    pub fn state(&self) -> ServiceState {
        lock(&self.state).clone()
    }

    /// Queues an action. Returns `false` if the IPC channel is closed. The
    /// requests whose outcome a view waits for become `Pending` right away,
    /// so a stale outcome is never mistaken for the new one.
    pub fn send(&self, c: ServiceCommand) -> bool {
        mark_pending(&mut lock(&self.state), &self.ticket, &c);
        let ok = self.commands.send(c.clone()).is_ok();
        if !ok {
            mark_failed(
                &mut lock(&self.state),
                &c,
                Failure::new("supervisor connection closed"),
            );
        }
        ok
    }

    /// The service process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Controlled stop: `shutdown` line on stdin, then closing of the life
    /// pipe; once `timeout` has expired, forced termination. Returns the exit
    /// code and whether forcing was necessary.
    pub fn stop(&mut self, timeout: Duration) -> (Option<i32>, bool) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.write_all(b"shutdown\n").and_then(|()| s.flush());
        }
        let t0 = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(st)) => return (st.code(), false),
                Ok(None) if t0.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let code = self.child.wait().ok().and_then(|s| s.code());
        (code, true)
    }
}

impl Supervisor for Service {
    fn pid(&self) -> u32 {
        Service::pid(self)
    }
    fn state(&self) -> ServiceState {
        Service::state(self)
    }
    fn send(&self, c: ServiceCommand) -> bool {
        Service::send(self, c)
    }
    fn is_alive(&mut self) -> bool {
        Service::is_alive(self)
    }
    fn stop(&mut self, timeout: Duration) -> (Option<i32>, bool) {
        Service::stop(self, timeout)
    }
    fn take_console_ticket(&self, vm_id: VmId) -> Option<ConsoleTicket> {
        let mut slot = lock(&self.ticket);
        if slot.as_ref().is_some_and(|t| t.vm_id == vm_id) {
            slot.take()
        } else {
            None
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.is_alive() {
            let _ = self.stop(STOP_TIMEOUT);
        } else {
            let _ = self.child.wait();
        }
    }
}

/// Marks as `Pending` the request a view waits for. Public for the test
/// stand-ins of [`Supervisor`].
pub fn mark_pending(
    s: &mut ServiceState,
    ticket: &Mutex<Option<ConsoleTicket>>,
    c: &ServiceCommand,
) {
    match c {
        ServiceCommand::Create(_) => s.create = RequestStatus::Pending,
        ServiceCommand::ImportMedia { .. } => s.import = RequestStatus::Pending,
        ServiceCommand::Delete { .. } => s.delete = RequestStatus::Pending,
        ServiceCommand::OpenConsole(vm_id) => {
            *lock(ticket) = None;
            s.console = Some(ConsoleRequest {
                vm_id: *vm_id,
                status: RequestStatus::Pending,
            });
        }
        _ => {}
    }
}

fn mark_failed(s: &mut ServiceState, c: &ServiceCommand, f: Failure) {
    match c {
        ServiceCommand::Create(_) => s.create = RequestStatus::Failed(f),
        ServiceCommand::ImportMedia { .. } => s.import = RequestStatus::Failed(f),
        ServiceCommand::Delete { .. } => s.delete = RequestStatus::Failed(f),
        ServiceCommand::OpenConsole(vm_id) => {
            s.console = Some(ConsoleRequest {
                vm_id: *vm_id,
                status: RequestStatus::Failed(f),
            });
        }
        ServiceCommand::Input(_) => s.input_error = Some(f.message),
        _ => s.last_outcome = Some(f.message),
    }
}

/// Readable label of the state of an operation.
pub fn operation_status_label(s: OperationStatus) -> &'static str {
    match s {
        OperationStatus::Queued => "queued",
        OperationStatus::Forwarded => "in progress",
        OperationStatus::Succeeded => "succeeded",
        OperationStatus::Failed => "failed",
        OperationStatus::Discarded => "discarded",
    }
}

fn describe_error(m: &ServerMessage) -> String {
    failure(m).message
}

fn failure(m: &ServerMessage) -> Failure {
    match m {
        ServerMessage::Error { error, .. } => match error {
            ReplyError::Operational(e) => Failure {
                unsupported: e.code == ErrorCode::Unsupported,
                message: format!("{}: {}", e.code.as_str(), e.message),
            },
            ReplyError::Protocol(e) => Failure::new(format!("protocol: {e}")),
        },
        ServerMessage::Closing { reason } => {
            Failure::new(format!("connection closed by the service: {reason:?}"))
        }
        other => Failure::new(format!("unexpected reply: {other:?}")),
    }
}

async fn connect(endpoint: &str, cred: &Credential) -> Result<Client, String> {
    let mut c = Client::connect(endpoint)
        .await
        .map_err(|e| format!("IPC connection failed: {e}"))?;
    match c.hello(cred).await {
        Ok(ServerMessage::HelloOk { .. }) => Ok(c),
        Ok(m) => Err(describe_error(&m)),
        Err(e) => Err(format!("handshake failed: {e}")),
    }
}

/// Shared parts of the IPC tasks.
#[derive(Clone)]
struct Lanes {
    state: Arc<Mutex<ServiceState>>,
    ticket: TicketSlot,
    repaint: Arc<dyn Fn() + Send + Sync>,
    tx: mpsc::UnboundedSender<ServiceCommand>,
}

impl Lanes {
    fn update(&self, f: impl FnOnce(&mut ServiceState)) {
        f(&mut lock(&self.state));
        (self.repaint)();
    }
}

async fn ipc_loop(
    endpoint: String,
    cred: Credential,
    mut rx: mpsc::UnboundedReceiver<ServiceCommand>,
    lanes: Lanes,
) {
    let connections = async {
        let r = connect(&endpoint, &cred).await?;
        let e = connect(&endpoint, &cred).await?;
        let f = connect(&endpoint, &cred).await?;
        let s = connect(&endpoint, &cred).await?;
        Ok::<_, String>((r, e, f, s))
    }
    .await;
    drop(cred);
    let (mut requests, mut events, mut fast, mut slow) = match connections {
        Ok(x) => x,
        Err(m) => {
            lanes.update(|s| s.connection = m);
            return;
        }
    };
    match events.request(Request::Subscribe).await {
        Ok(ServerMessage::Response {
            result: ResponseBody::Subscribed,
            ..
        }) => {}
        Ok(m) => {
            let d = describe_error(&m);
            lanes.update(|s| s.connection = format!("event subscription refused: {d}"));
            return;
        }
        Err(e) => {
            lanes.update(|s| s.connection = format!("event subscription failed: {e}"));
            return;
        }
    }
    lanes.update(|s| {
        s.connection = "connected".into();
        s.connected = true;
    });
    let _ = lanes.tx.send(ServiceCommand::Refresh);
    let _ = lanes.tx.send(ServiceCommand::ListMedia);

    let ev_lanes = lanes.clone();
    let event_reader = async move {
        loop {
            match events.next_event().await {
                Ok(Ok(ServiceEvent::VmState { vm_id, state })) => {
                    ev_lanes.update(|s| {
                        if let Some(v) = s.vms.iter_mut().find(|v| v.vm_id == vm_id) {
                            v.state = state;
                        }
                    });
                    // The run_id changes on start: the list is re-read.
                    let _ = ev_lanes.tx.send(ServiceCommand::Refresh);
                }
                Ok(Ok(_)) => {}
                Ok(Err(m)) => {
                    let d = describe_error(&m);
                    ev_lanes.update(|s| {
                        s.connection = d;
                        s.connected = false;
                    });
                    return;
                }
                Err(e) => {
                    ev_lanes.update(|s| {
                        s.connection = format!("event channel closed: {e}");
                        s.connected = false;
                    });
                    return;
                }
            }
        }
    };

    let (fast_tx, mut fast_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let (slow_tx, mut slow_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let main_lanes = lanes.clone();
    let executor = async move {
        while let Some(c) = rx.recv().await {
            match c {
                ServiceCommand::Input(_) | ServiceCommand::OpenConsole(_) => {
                    let _ = fast_tx.send(c);
                }
                ServiceCommand::ImportMedia { .. } => {
                    let _ = slow_tx.send(c);
                }
                c => run(&mut requests, &main_lanes, c).await,
            }
        }
    };
    let fast_lanes = lanes.clone();
    let fast_lane = async move {
        while let Some(c) = fast_rx.recv().await {
            run(&mut fast, &fast_lanes, c).await;
        }
    };
    let slow_lanes = lanes.clone();
    let slow_lane = async move {
        while let Some(c) = slow_rx.recv().await {
            run(&mut slow, &slow_lanes, c).await;
        }
    };
    tokio::join!(event_reader, executor, fast_lane, slow_lane);
}

/// Executes a command and records the failure where the views look for it.
async fn run(c: &mut Client, lanes: &Lanes, command: ServiceCommand) {
    let kind = command.clone();
    match execute(c, lanes, command).await {
        Ok(()) => (lanes.repaint)(),
        Err(f) => lanes.update(|s| mark_failed(s, &kind, f)),
    }
}

async fn request(c: &mut Client, body: Request) -> Result<ServerMessage, Failure> {
    c.request(body)
        .await
        .map_err(|e| Failure::new(e.to_string()))
}

fn context(what: &str, f: Failure) -> Failure {
    Failure {
        unsupported: f.unsupported,
        message: format!("{what}: {}", f.message),
    }
}

async fn execute(c: &mut Client, lanes: &Lanes, command: ServiceCommand) -> Result<(), Failure> {
    let state = &*lanes.state;
    match command {
        ServiceCommand::Refresh => {
            let caps = request(c, Request::GetCapabilities).await?;
            if let ServerMessage::Response {
                result: ResponseBody::Capabilities(k),
                ..
            } = caps
            {
                let mut s = lock(state);
                s.backend = Some(k.backend);
                s.accelerator = k.accelerator;
                s.policy = Some(k.parent_loss_policy);
                s.host = k.host;
            }
            match request(c, Request::ListVms).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmList { vms },
                    ..
                } => {
                    lock(state).vms = vms;
                    Ok(())
                }
                m => Err(failure(&m)),
            }
        }
        ServiceCommand::ListMedia => match request(c, Request::ListMedia).await? {
            ServerMessage::Response {
                result: ResponseBody::MediaList { media },
                ..
            } => {
                let mut s = lock(state);
                s.media = MediaState {
                    loaded: true,
                    records: media,
                    error: None,
                };
                Ok(())
            }
            m => {
                let f = context("approved media", failure(&m));
                lock(state).media.error = Some(f);
                Ok(())
            }
        },
        ServiceCommand::ImportMedia { path, kind, origin } => {
            let body = Request::ImportMedia {
                source_path: path.to_string_lossy().into_owned(),
                media_kind: kind,
                declared_origin: origin,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::MediaImported { media },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.media.records.retain(|m| m.id != media.id);
                        s.media.records.push(media.clone());
                        s.import = RequestStatus::Done(media);
                        s.last_outcome = Some("ISO imported".into());
                    }
                    let _ = lanes.tx.send(ServiceCommand::ListMedia);
                    Ok(())
                }
                m => Err(context("import", failure(&m))),
            }
        }
        ServiceCommand::Create(config) => {
            match request(c, Request::CreateVm { config: *config }).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmCreated { vm },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        s.last_outcome = Some(format!("created: {}", vm.config.name.as_str()));
                        s.create = RequestStatus::Done(vm.id);
                    }
                    Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await
                }
                m => Err(context("creation", failure(&m))),
            }
        }
        ServiceCommand::Delete { vm_id, revision } => {
            let body = Request::DeleteVm {
                vm_id,
                expected_revision: revision,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::VmDeleted { vm_id },
                    ..
                } => {
                    {
                        let mut s = lock(state);
                        let name = s.vm(vm_id).map(|v| v.name.as_str().to_owned());
                        s.vms.retain(|v| v.vm_id != vm_id);
                        s.delete = RequestStatus::Done(vm_id);
                        s.last_outcome =
                            Some(format!("deleted: {}", name.unwrap_or_else(|| "VM".into())));
                    }
                    Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await
                }
                m => Err(context("deletion", failure(&m))),
            }
        }
        ServiceCommand::OpenConsole(vm_id) => {
            match request(c, Request::OpenConsole { vm_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::Console { ticket },
                    ..
                } => {
                    let run_id = ticket.run_id;
                    *lock(&lanes.ticket) = Some(ticket);
                    lock(state).console = Some(ConsoleRequest {
                        vm_id,
                        status: RequestStatus::Done(run_id),
                    });
                    Ok(())
                }
                m => Err(context("console", failure(&m))),
            }
        }
        ServiceCommand::Input(batch) => {
            let req = VmRequest {
                operation_id: OperationId::new_v7(),
                vm_id: batch.vm_id,
                expected_run_id: Some(batch.run_id),
                action: VmAction::Input(batch),
            };
            match request(c, Request::SubmitVm { request: req }).await? {
                ServerMessage::Response {
                    result: ResponseBody::Operation { operation, .. },
                    ..
                } => {
                    let error = operation
                        .and_then(|o| o.error)
                        .map(|code| format!("input refused: {}", code.as_str()));
                    let mut s = lock(state);
                    if s.input_error != error {
                        s.input_error = error;
                    }
                    Ok(())
                }
                m => Err(context("input", failure(&m))),
            }
        }
        ServiceCommand::SetPolicy(policy) => {
            match request(c, Request::SetParentLossPolicy { policy }).await? {
                ServerMessage::Response {
                    result: ResponseBody::ParentLossPolicy { policy },
                    ..
                } => {
                    let mut s = lock(state);
                    s.policy = Some(policy);
                    s.last_outcome = Some(format!(
                        "if the UI closes unexpectedly: {}",
                        crate::settings::describe_policy(policy)
                    ));
                    Ok(())
                }
                m => Err(context("setting", failure(&m))),
            }
        }
        ServiceCommand::UpdateVm {
            vm_id,
            revision,
            config,
        } => {
            update_vm(c, state, vm_id, revision, *config).await?;
            Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await
        }
        ServiceCommand::OpenVm {
            name,
            changes,
            create,
        } => {
            Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await?;
            let found = lock(state)
                .vms
                .iter()
                .find(|v| v.name.as_str() == name)
                .cloned();
            let vm = match found {
                Some(v) => v,
                None if create => {
                    let mut config =
                        crate::vm_settings::default_config(&name).map_err(Failure::new)?;
                    changes.apply(&mut config);
                    Box::pin(execute(c, lanes, ServiceCommand::Create(Box::new(config)))).await?;
                    lock(state)
                        .vms
                        .iter()
                        .find(|v| v.name.as_str() == name)
                        .cloned()
                        .ok_or_else(|| {
                            Failure::new(format!("VM \"{name}\" not found after creation"))
                        })?
                }
                None => {
                    return Err(Failure::new(format!(
                        "VM \"{name}\" not found in the catalog"
                    )));
                }
            };
            let stopped = vm.state == lightvm_proto::VmState::Stopped;
            if !changes.is_empty() {
                if !stopped {
                    return Err(Failure::new(format!(
                        "VM \"{name}\" running: resources and limits can be changed only while the VM is stopped"
                    )));
                }
                let mut config = vm.config.clone();
                changes.apply(&mut config);
                if config != vm.config {
                    update_vm(c, state, vm.vm_id, vm.config_revision, config).await?;
                }
            }
            let start = if stopped {
                vm_action(c, state, vm.vm_id, None, VmAction::Start, "start").await
            } else {
                Ok(())
            };
            // The list is re-read even if the start fails: the saved changes
            // stay visible.
            Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await?;
            start
        }
        ServiceCommand::Start(vm_id) => {
            vm_action(c, state, vm_id, None, VmAction::Start, "start").await
        }
        ServiceCommand::Stop(vm_id) => {
            vm_action(c, state, vm_id, None, VmAction::Stop, "stop").await
        }
        ServiceCommand::ForceStop(vm_id) => {
            vm_action(c, state, vm_id, None, VmAction::ForceStop, "force stop").await
        }
        ServiceCommand::Reset { vm_id, run_id } => {
            vm_action(c, state, vm_id, Some(run_id), VmAction::Reset, "reset").await
        }
        ServiceCommand::EmergencyStop => match request(c, Request::EmergencyStop).await? {
            ServerMessage::Response {
                result: ResponseBody::EmergencyStop(r),
                ..
            } => {
                lock(state).last_outcome = Some(format!(
                    "Emergency Stop: {} processes terminated, completed: {}",
                    r.processes_terminated,
                    if r.completed { "yes" } else { "no" }
                ));
                Ok(())
            }
            m => Err(failure(&m)),
        },
    }
}

async fn update_vm(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    vm_id: VmId,
    revision: u64,
    config: VmConfig,
) -> Result<(), Failure> {
    let body = Request::UpdateVm {
        vm_id,
        expected_revision: revision,
        config,
    };
    match request(c, body).await? {
        ServerMessage::Response {
            result: ResponseBody::VmUpdated { vm },
            ..
        } => {
            lock(state).last_outcome = Some(format!("settings saved: {}", vm.config.name.as_str()));
            Ok(())
        }
        m => Err(context("settings", failure(&m))),
    }
}

async fn vm_action(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    vm_id: VmId,
    expected_run_id: Option<RunId>,
    action: VmAction,
    name: &str,
) -> Result<(), Failure> {
    let req = VmRequest {
        operation_id: OperationId::new_v7(),
        vm_id,
        expected_run_id,
        action,
    };
    match request(c, Request::SubmitVm { request: req }).await? {
        ServerMessage::Response {
            result: ResponseBody::Operation { operation, .. },
            ..
        } => {
            let outcome = operation.map_or_else(
                || format!("{name}: accepted"),
                |o| match o.error {
                    Some(code) => {
                        format!(
                            "{name}: {} ({})",
                            operation_status_label(o.status),
                            code.as_str()
                        )
                    }
                    None => format!("{name}: {}", operation_status_label(o.status)),
                },
            );
            lock(state).last_outcome = Some(outcome);
            Ok(())
        }
        m => Err(context(name, failure(&m))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ready_line() {
        let cred = Credential::generate().unwrap();
        let line = format!(
            r#"{{"event":"ready","pid":42,"endpoint":"\\\\.\\pipe\\x","protocol_versions":[1],"credential":"{}"}}"#,
            cred.to_hex()
        );
        let p = parse_ready(&line).unwrap();
        assert_eq!(p.pid, 42);
        assert_eq!(p.endpoint, r"\\.\pipe\x");
        assert!(p.credential.ct_eq(&cred));
    }

    #[test]
    fn invalid_ready_line() {
        for line in [
            "",
            "not json",
            r#"{"event":"other","pid":1,"endpoint":"e","credential":"00"}"#,
            r#"{"event":"ready","endpoint":"e","credential":"00"}"#,
            r#"{"event":"ready","pid":1,"credential":"00"}"#,
            r#"{"event":"ready","pid":1,"endpoint":"e","credential":"zz"}"#,
        ] {
            assert!(
                matches!(parse_ready(line), Err(StartError::InvalidLine(_))),
                "{line}"
            );
        }
    }

    #[test]
    fn unsupported_is_recognised() {
        let m = ServerMessage::Error {
            request_id: Some(1),
            error: ReplyError::Operational(lightvm_proto::PublicError::unsupported("later")),
        };
        let f = failure(&m);
        assert!(f.unsupported);
        assert!(f.message.contains("later"));
    }

    #[test]
    fn pending_is_set_synchronously_and_ticket_cleared() {
        let mut s = ServiceState::default();
        let slot = Mutex::new(None);
        let vm = VmId::new_v7();
        mark_pending(&mut s, &slot, &ServiceCommand::OpenConsole(vm));
        assert_eq!(
            s.console,
            Some(ConsoleRequest {
                vm_id: vm,
                status: RequestStatus::Pending
            })
        );
        mark_failed(
            &mut s,
            &ServiceCommand::Delete {
                vm_id: vm,
                revision: 1,
            },
            Failure::new("x"),
        );
        assert_eq!(s.delete, RequestStatus::Failed(Failure::new("x")));
    }
}
