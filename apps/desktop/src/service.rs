//! Supervisor as a child process of the UI (F1-11, ADR-0002; prerequisite of
//! P-2 in `docs/STATUS.md`).
//!
//! The UI starts `lightvm-service` with structured arguments and keeps three
//! anonymous pipes (contract in `crates/lightvm-service/src/main.rs`):
//! - stdout: a single JSON `ready` line with IPC endpoint and UI credential;
//! - stdin: life pipe (EOF = UI terminated) and `shutdown` line for the
//!   controlled stop;
//! - stderr: diagnostics without secrets; inherited, or appended to
//!   `lightvm-service.log` when the app writes its diagnostic log
//!   ([`crate::diag`]).
//!
//! After the `ready` line the UI talks to the service only via authenticated
//! IPC, with six connections: requests (reply by `request_id`), events
//! (`subscribe`), a fast lane for console tickets and keyboard/mouse input
//! (never queued behind a start or a stop), a slow lane for ISO imports
//! (copy and SHA-256 of several GiB), a storage lane for baseline creation
//! and restore (F3: full copy and hash of the disk, minutes, no timeout)
//! and a start lane (the reply to a start arrives after the verification of
//! media and baseline and the launch of QEMU, seconds). Each lane matches
//! its replies by `request_id` on its own connection. Reads of a frame
//! cannot be interrupted safely, so the lanes do not share a connection.
//!
//! The UI opens neither QMP nor VM files: start, stop, input and Emergency
//! Stop go through the service core (ROADMAP 6.1). The console display is
//! reached only with the per-run ticket of `open_console` (ADR-0013).

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    BaselineSummary, ConsoleTicket, ParentLossPolicy, Request, ResponseBody, ServerMessage,
    ServiceEvent, VmSummary,
};
use lightvm_proto::{
    BaselineId, ErrorCode, HostCapacity, InputBatch, InputEvent, KeyAction, Label, MediaKind,
    MediaRecord, MouseButton, OperationId, OperationStatus, PointerButton, ReplyError, RunId,
    VmAction, VmConfig, VmId, VmRequest,
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
/// Wait before sending again an input batch refused by the service rate
/// limit (`resource_limit`).
pub const INPUT_RETRY_DELAY: Duration = Duration::from_millis(60);
/// Commands waiting on the fast lane beyond which pointer-only input
/// batches are dropped (keys, buttons and console tickets never are).
pub const FAST_LANE_CAPACITY: usize = 32;

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
            .stderr(crate::diag::service_stderr().unwrap_or_else(Stdio::inherit));
        c
    }
}

/// Initial state of the connection, before the handshake.
pub const CONNECTING: &str = "connecting";

/// Failure of a request, readable, with the "not provided by this
/// supervisor" case recognisable (`unsupported`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The supervisor does not implement the operation.
    pub unsupported: bool,
    /// Readable description.
    pub message: String,
    /// Error code of the supervisor, if the failure came from its reply
    /// (`None`: connection or protocol problem).
    pub code: Option<ErrorCode>,
    /// Structured details of the reply (for example `reason`), without
    /// secrets.
    pub details: BTreeMap<String, String>,
}

impl Failure {
    /// Failure from a description.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// Failure with the code of a supervisor reply (stand-ins build them
    /// too).
    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            unsupported: code == ErrorCode::Unsupported,
            message: message.into(),
            code: Some(code),
            details: BTreeMap::new(),
        }
    }

    /// Adds a detail (`reason`, `needed_bytes`, ...).
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: impl Into<String>) -> Self {
        self.details.insert(key.to_owned(), value.into());
        self
    }

    /// Value of the `reason` detail, if any.
    pub fn reason(&self) -> Option<&str> {
        self.details.get("reason").map(String::as_str)
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

/// Baselines of a VM known to the UI (F3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaselineList {
    /// The list was received at least once.
    pub loaded: bool,
    /// Baselines, oldest first.
    pub items: Vec<BaselineSummary>,
    /// Last read error, if any.
    pub error: Option<Failure>,
}

/// Baseline operation requested by the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaselineOp {
    /// Creation with the given label.
    Create {
        /// Label of the new baseline.
        label: Label,
        /// Consent to a baseline after an unclean stop ("not verified").
        allow_unclean: bool,
    },
    /// Restore of a baseline.
    Restore(BaselineId),
    /// Deletion of a baseline.
    Delete(BaselineId),
}

/// Last baseline operation requested by the UI and its outcome (on
/// success, the baseline concerned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineRequest {
    /// VM.
    pub vm_id: VmId,
    /// Operation.
    pub op: BaselineOp,
    /// Outcome.
    pub status: RequestStatus<BaselineId>,
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
    /// Baselines per VM, as last read (F3).
    pub baselines: BTreeMap<VmId, BaselineList>,
    /// Last baseline operation (creation, restore, deletion).
    pub baseline: Option<BaselineRequest>,
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
    /// Re-reads the baselines of a VM.
    ListBaselines(VmId),
    /// Creates a baseline of a stopped VM (long: storage lane).
    CreateBaseline {
        /// VM.
        vm_id: VmId,
        /// Label of the new baseline.
        label: Label,
        /// Consent to a baseline after an unclean stop, recorded as not
        /// verified.
        allow_unclean: bool,
    },
    /// Restores a baseline: the current state is discarded (long: storage
    /// lane).
    RestoreBaseline {
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
    /// Deletes a baseline that is not in use.
    DeleteBaseline {
        /// VM.
        vm_id: VmId,
        /// Baseline.
        baseline_id: BaselineId,
    },
}

impl ServiceCommand {
    /// Baseline operation of the command, if it is one (list excluded).
    pub fn baseline_op(&self) -> Option<(VmId, BaselineOp)> {
        match self {
            Self::CreateBaseline {
                vm_id,
                label,
                allow_unclean,
            } => Some((
                *vm_id,
                BaselineOp::Create {
                    label: label.clone(),
                    allow_unclean: *allow_unclean,
                },
            )),
            Self::RestoreBaseline { vm_id, baseline_id } => {
                Some((*vm_id, BaselineOp::Restore(*baseline_id)))
            }
            Self::DeleteBaseline { vm_id, baseline_id } => {
                Some((*vm_id, BaselineOp::Delete(*baseline_id)))
            }
            _ => None,
        }
    }
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
    /// Drops a console ticket received and not handed over (the console
    /// was closed or its page left): the token does not stay in memory.
    fn discard_console_ticket(&self);
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
pub struct Service {
    child: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    commands: mpsc::UnboundedSender<ServiceCommand>,
    state: Arc<Mutex<ServiceState>>,
    ticket: TicketSlot,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The ticket slot holds the display token: only whether it is full.
        let ticket = if lock(&self.ticket).is_some() {
            "<redacted>"
        } else {
            "none"
        };
        f.debug_struct("Service")
            .field("pid", &self.pid)
            .field("state", &self.state)
            .field("console_ticket", &ticket)
            .finish_non_exhaustive()
    }
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
        // The IPC tasks keep only a weak sender: when the `Service` is
        // dropped the command channel closes and the IPC thread ends.
        let lanes = Lanes {
            state: state.clone(),
            ticket: ticket.clone(),
            repaint,
            tx: commands.downgrade(),
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

    /// Queues an action. Returns `false` if the IPC channel is closed (the
    /// connection to the supervisor ended). The requests whose outcome a
    /// view waits for become `Pending` right away, so a stale outcome is
    /// never mistaken for the new one.
    pub fn send(&self, c: ServiceCommand) -> bool {
        send_command(&self.commands, &self.state, &self.ticket, c)
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
    fn discard_console_ticket(&self) {
        *lock(&self.ticket) = None;
    }
}

/// Body of [`Service::send`].
fn send_command(
    commands: &mpsc::UnboundedSender<ServiceCommand>,
    state: &Mutex<ServiceState>,
    ticket: &Mutex<Option<ConsoleTicket>>,
    c: ServiceCommand,
) -> bool {
    if commands.is_closed() {
        mark_failed(
            &mut lock(state),
            &c,
            Failure::new("supervisor connection closed"),
        );
        return false;
    }
    mark_pending(&mut lock(state), ticket, &c);
    let ok = commands.send(c.clone()).is_ok();
    if !ok {
        mark_failed(
            &mut lock(state),
            &c,
            Failure::new("supervisor connection closed"),
        );
    }
    ok
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
        c => {
            if let Some((vm_id, op)) = c.baseline_op() {
                s.baseline = Some(BaselineRequest {
                    vm_id,
                    op,
                    status: RequestStatus::Pending,
                });
            }
        }
    }
}

/// Requests still waiting for an outcome when the connection ends fail:
/// no view keeps waiting for a supervisor that is gone.
fn fail_pending(s: &mut ServiceState, why: &str) {
    fn fail<T>(r: &mut RequestStatus<T>, why: &str) {
        if r.is_pending() {
            *r = RequestStatus::Failed(Failure::new(why));
        }
    }
    fail(&mut s.import, why);
    fail(&mut s.create, why);
    fail(&mut s.delete, why);
    if let Some(r) = &mut s.console {
        fail(&mut r.status, why);
    }
    if let Some(r) = &mut s.baseline {
        fail(&mut r.status, why);
    }
    s.connected = false;
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
        ServiceCommand::ListBaselines(vm_id) => {
            s.baselines.entry(*vm_id).or_default().error = Some(f);
        }
        c => match c.baseline_op() {
            Some((vm_id, op)) => {
                s.baseline = Some(BaselineRequest {
                    vm_id,
                    op,
                    status: RequestStatus::Failed(f),
                });
            }
            None => s.last_outcome = Some(f.message),
        },
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
                details: e.details.clone().unwrap_or_default(),
                ..Failure::coded(e.code, format!("{}: {}", e.code.as_str(), e.message))
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
    /// Weak: the IPC tasks must not keep the command channel open.
    tx: mpsc::WeakUnboundedSender<ServiceCommand>,
}

impl Lanes {
    fn update(&self, f: impl FnOnce(&mut ServiceState)) {
        f(&mut lock(&self.state));
        (self.repaint)();
    }

    /// Queues a follow-up command, if the service is still in use.
    fn queue(&self, c: ServiceCommand) {
        if let Some(tx) = self.tx.upgrade() {
            let _ = tx.send(c);
        }
    }
}

/// Bounded queue of the fast lane (console tickets and input).
#[derive(Default)]
struct FastQueue {
    items: Mutex<VecDeque<ServiceCommand>>,
    wake: tokio::sync::Notify,
    closed: AtomicBool,
}

/// Input batch made only of pointer moves: superseded by any later one
/// (absolute positions), so it can be dropped when the lane is congested.
fn pointer_only(c: &ServiceCommand) -> bool {
    matches!(c, ServiceCommand::Input(b)
        if b.events.iter().all(|e| matches!(e, InputEvent::PointerMove(_))))
}

/// Adds a command to the fast lane queue. Beyond [`FAST_LANE_CAPACITY`]
/// the waiting pointer-only batches are dropped; key and button events
/// and ticket requests are always kept.
fn push_fast(q: &mut VecDeque<ServiceCommand>, c: ServiceCommand) {
    if q.len() >= FAST_LANE_CAPACITY {
        q.retain(|w| !pointer_only(w));
        if q.len() >= FAST_LANE_CAPACITY && pointer_only(&c) {
            return;
        }
    }
    q.push_back(c);
}

impl FastQueue {
    fn push(&self, c: ServiceCommand) {
        push_fast(&mut lock(&self.items), c);
        self.wake.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Next command; `None` once closed and empty.
    async fn next(&self) -> Option<ServiceCommand> {
        loop {
            if let Some(c) = lock(&self.items).pop_front() {
                return Some(c);
            }
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            self.wake.notified().await;
        }
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
        let b = connect(&endpoint, &cred).await?;
        let st = connect(&endpoint, &cred).await?;
        Ok::<_, String>((r, e, f, s, b, st))
    }
    .await;
    drop(cred);
    let (mut requests, mut events, mut fast, mut slow, mut storage, mut starts) = match connections
    {
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
    lanes.queue(ServiceCommand::Refresh);
    lanes.queue(ServiceCommand::ListMedia);

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
                    ev_lanes.queue(ServiceCommand::Refresh);
                }
                Ok(Ok(ServiceEvent::BaselinesChanged { vm_id })) => {
                    // Operation started or ended, baseline created, restored
                    // or deleted: VM summary (operation, current state) and
                    // list re-read.
                    ev_lanes.queue(ServiceCommand::Refresh);
                    ev_lanes.queue(ServiceCommand::ListBaselines(vm_id));
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

    let fast_queue = Arc::new(FastQueue::default());
    let (slow_tx, mut slow_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let (storage_tx, mut storage_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let (start_tx, mut start_rx) = mpsc::unbounded_channel::<ServiceCommand>();
    let main_lanes = lanes.clone();
    let fast_in = fast_queue.clone();
    // Ends when every strong sender is gone (the `Service` was dropped).
    let executor = async move {
        while let Some(c) = rx.recv().await {
            match c {
                ServiceCommand::Input(_) | ServiceCommand::OpenConsole(_) => fast_in.push(c),
                ServiceCommand::ImportMedia { .. } => {
                    let _ = slow_tx.send(c);
                }
                ServiceCommand::CreateBaseline { .. } | ServiceCommand::RestoreBaseline { .. } => {
                    let _ = storage_tx.send(c);
                }
                ServiceCommand::Start(_) | ServiceCommand::OpenVm { .. } => {
                    let _ = start_tx.send(c);
                }
                c => run(&mut requests, &main_lanes, c).await,
            }
        }
        fast_in.close();
        drop(slow_tx);
        drop(storage_tx);
        drop(start_tx);
    };
    let fast_lanes = lanes.clone();
    let fast_lane = async move {
        while let Some(c) = fast_queue.next().await {
            run(&mut fast, &fast_lanes, c).await;
        }
    };
    let slow_lanes = lanes.clone();
    let slow_lane = async move {
        while let Some(c) = slow_rx.recv().await {
            run(&mut slow, &slow_lanes, c).await;
        }
    };
    // Baseline creation and restore: the reply arrives when the operation
    // ends (minutes for a large disk), without timeout; only the Emergency
    // Stop, the closing of the connection or the service shutdown cancel it.
    // Starts: the reply arrives after the verification of the media and of
    // the baseline and the launch of QEMU (seconds); the VM list is polled
    // meanwhile on the request lane for the progress.
    let start_lanes = lanes.clone();
    let start_lane = async move {
        while let Some(c) = start_rx.recv().await {
            run(&mut starts, &start_lanes, c).await;
        }
    };
    let storage_lanes = lanes.clone();
    let storage_lane = async move {
        while let Some(c) = storage_rx.recv().await {
            run(&mut storage, &storage_lanes, c).await;
        }
    };
    // The loop ends when the service is no longer used (executor and lanes
    // done) or when the event connection closes (the supervisor is gone):
    // the command channel is then dropped, so `Service::send` returns
    // `false`, and the requests still waiting are marked as failed.
    tokio::select! {
        () = event_reader => {}
        _ = async { tokio::join!(executor, fast_lane, slow_lane, storage_lane, start_lane) } => {}
    }
    lanes.update(|s| fail_pending(s, "supervisor connection closed"));
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
        message: format!("{what}: {}", f.message),
        ..f
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
                    // Baselines not read yet, or changed since: re-read, so
                    // the Dashboard can name the baseline a VM is on.
                    let stale: Vec<VmId> = {
                        let mut s = lock(state);
                        s.vms = vms;
                        s.vms
                            .iter()
                            .filter(|v| {
                                let count = v.baselines.count as usize;
                                (count > 0 || v.baselines.current.is_some())
                                    && s.baselines
                                        .get(&v.vm_id)
                                        .is_none_or(|l| !l.loaded || l.items.len() != count)
                            })
                            .map(|v| v.vm_id)
                            .collect()
                    };
                    for vm_id in stale {
                        lanes.queue(ServiceCommand::ListBaselines(vm_id));
                    }
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
                    lanes.queue(ServiceCommand::ListMedia);
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
                    refresh_after(c, lanes).await;
                    Ok(())
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
                    refresh_after(c, lanes).await;
                    Ok(())
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
            // Refused by the rate limit: the same batch once more after a
            // short wait, then (if still refused) only its key and button
            // releases, so the guest is not left with a key held down.
            let releases = releases_of(&batch);
            let mut attempts = vec![batch.clone(), batch];
            if let Some(r) = releases {
                attempts.push(r);
            }
            let mut last = None;
            for (i, b) in attempts.into_iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(INPUT_RETRY_DELAY).await;
                }
                let req = VmRequest {
                    operation_id: OperationId::new_v7(),
                    vm_id: b.vm_id,
                    expected_run_id: Some(b.run_id),
                    action: VmAction::Input(b),
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
                        return Ok(());
                    }
                    m if rate_limited(&m) => last = Some(m),
                    m => return Err(context("input", failure(&m))),
                }
            }
            Err(context(
                "input",
                last.map_or_else(|| Failure::new("not sent"), |m| failure(&m)),
            ))
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
            refresh_after(c, lanes).await;
            Ok(())
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
            // stay visible. A failed re-read does not hide the outcome of
            // the start.
            refresh_after(c, lanes).await;
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
        ServiceCommand::ListBaselines(vm_id) => {
            match request(c, Request::ListBaselines { vm_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::Baselines { baselines, .. },
                    ..
                } => {
                    lock(state).baselines.insert(
                        vm_id,
                        BaselineList {
                            loaded: true,
                            items: baselines,
                            error: None,
                        },
                    );
                    Ok(())
                }
                m => Err(context("baselines", failure(&m))),
            }
        }
        ServiceCommand::CreateBaseline {
            vm_id,
            label,
            allow_unclean,
        } => {
            let op = BaselineOp::Create {
                label: label.clone(),
                allow_unclean,
            };
            let body = Request::CreateBaseline {
                vm_id,
                label,
                allow_unclean,
            };
            match request(c, body).await? {
                ServerMessage::Response {
                    result: ResponseBody::BaselineCreated { baseline },
                    ..
                } => {
                    baseline_done(
                        lanes,
                        vm_id,
                        op,
                        baseline.baseline_id,
                        format!("baseline created: {}", baseline.label.as_str()),
                    );
                    Ok(())
                }
                m => Err(context("baseline", failure(&m))),
            }
        }
        ServiceCommand::RestoreBaseline { vm_id, baseline_id } => {
            match request(c, Request::RestoreBaseline { vm_id, baseline_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::BaselineRestored { .. },
                    ..
                } => {
                    let label = baseline_label(state, vm_id, baseline_id);
                    baseline_done(
                        lanes,
                        vm_id,
                        BaselineOp::Restore(baseline_id),
                        baseline_id,
                        format!("baseline restored: {label}"),
                    );
                    Ok(())
                }
                m => Err(context("restore", failure(&m))),
            }
        }
        ServiceCommand::DeleteBaseline { vm_id, baseline_id } => {
            let label = baseline_label(state, vm_id, baseline_id);
            match request(c, Request::DeleteBaseline { vm_id, baseline_id }).await? {
                ServerMessage::Response {
                    result: ResponseBody::BaselineDeleted { .. },
                    ..
                } => {
                    if let Some(l) = lock(state).baselines.get_mut(&vm_id) {
                        l.items.retain(|b| b.baseline_id != baseline_id);
                    }
                    baseline_done(
                        lanes,
                        vm_id,
                        BaselineOp::Delete(baseline_id),
                        baseline_id,
                        format!("baseline deleted: {label}"),
                    );
                    Ok(())
                }
                m => Err(context("baseline deletion", failure(&m))),
            }
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

/// Label of a known baseline, for the outcome line.
fn baseline_label(state: &Mutex<ServiceState>, vm_id: VmId, id: BaselineId) -> String {
    lock(state)
        .baselines
        .get(&vm_id)
        .and_then(|l| l.items.iter().find(|b| b.baseline_id == id))
        .map_or_else(|| "baseline".into(), |b| b.label.as_str().to_owned())
}

/// Records a successful baseline operation and re-reads the VM list and the
/// baselines (queued: the storage lane does not wait for them).
fn baseline_done(lanes: &Lanes, vm_id: VmId, op: BaselineOp, id: BaselineId, outcome: String) {
    lanes.update(|s| {
        s.baseline = Some(BaselineRequest {
            vm_id,
            op,
            status: RequestStatus::Done(id),
        });
        s.last_outcome = Some(outcome);
    });
    lanes.queue(ServiceCommand::Refresh);
    lanes.queue(ServiceCommand::ListBaselines(vm_id));
}

/// Re-reads the list after a successful change: a failure is reported in
/// `last_outcome` only, the change itself stays successful.
async fn refresh_after(c: &mut Client, lanes: &Lanes) {
    if let Err(f) = Box::pin(execute(c, lanes, ServiceCommand::Refresh)).await {
        lock(&lanes.state).last_outcome = Some(format!("VM list not refreshed: {f}"));
    }
}

/// The service refused the request because of its rate limit.
fn rate_limited(m: &ServerMessage) -> bool {
    matches!(m, ServerMessage::Error {
        error: ReplyError::Operational(e),
        ..
    } if e.code == ErrorCode::ResourceLimit)
}

/// Key and button releases of a batch, as a batch of their own (`None` if
/// there are none, or if the batch has nothing else).
fn releases_of(b: &InputBatch) -> Option<InputBatch> {
    let events: Vec<InputEvent> = b
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                InputEvent::Key(KeyAction { down: false, .. })
                    | InputEvent::PointerButton(PointerButton { down: false, .. })
            )
        })
        .cloned()
        .collect();
    // Wheel "releases" are part of a click, not a held button.
    let events: Vec<InputEvent> = events
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                InputEvent::PointerButton(PointerButton {
                    button: MouseButton::WheelUp | MouseButton::WheelDown,
                    ..
                })
            )
        })
        .collect();
    (!events.is_empty() && events.len() < b.events.len()).then(|| InputBatch {
        events,
        ..b.clone()
    })
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

    /// In-process IPC server speaking the real framing on the real local
    /// transport, with scripted replies: drives `ipc_loop` without a
    /// supervisor process.
    mod fake {
        use super::*;
        use lightvm_proto::ipc::{ClientMessage, Role};
        use lightvm_service::frame;
        use lightvm_service::transport::{Accepted, BoxedStream, EndpointOptions, Listener};

        pub(super) type Script =
            Arc<dyn Fn(&Request) -> Result<ResponseBody, ReplyError> + Send + Sync>;

        pub(super) struct Server {
            pub(super) endpoint: String,
            pub(super) requests: Arc<Mutex<Vec<Request>>>,
            /// Set: the event connection is closed (supervisor gone).
            pub(super) close_events: Arc<AtomicBool>,
            /// Events to push on the event connection.
            pub(super) events: Arc<Mutex<Vec<ServiceEvent>>>,
            /// Set: the reply to `create_baseline` or to a start is held back (a long
            /// operation) until it is cleared.
            pub(super) hold_baseline: Arc<AtomicBool>,
        }

        pub(super) fn start(script: Script) -> Server {
            let dir = std::env::temp_dir().join(format!(
                "lightvm-fake-ipc-{}-{}",
                std::process::id(),
                OperationId::new_v7()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let opts = EndpointOptions {
                runtime_dir: Some(dir.clone()),
                catalog_dir: dir,
            };
            let requests = Arc::new(Mutex::new(Vec::new()));
            let close_events = Arc::new(AtomicBool::new(false));
            let events = Arc::new(Mutex::new(Vec::new()));
            let hold_baseline = Arc::new(AtomicBool::new(false));
            let (r2, c2) = (requests.clone(), close_events.clone());
            let (e2, h2) = (events.clone(), hold_baseline.clone());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = Listener::bind(&opts).unwrap();
                    tx.send(listener.endpoint().to_owned()).unwrap();
                    loop {
                        if let Ok(Accepted::Ok(stream, _)) = listener.accept().await {
                            tokio::spawn(serve(
                                stream,
                                script.clone(),
                                r2.clone(),
                                c2.clone(),
                                e2.clone(),
                                h2.clone(),
                            ));
                        }
                    }
                });
            });
            Server {
                endpoint: rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                requests,
                close_events,
                events,
                hold_baseline,
            }
        }

        async fn serve(
            mut s: BoxedStream,
            script: Script,
            requests: Arc<Mutex<Vec<Request>>>,
            close: Arc<AtomicBool>,
            events: Arc<Mutex<Vec<ServiceEvent>>>,
            hold: Arc<AtomicBool>,
        ) {
            let max = frame::DEFAULT_MAX_FRAME;
            loop {
                let Ok(m) = frame::read_frame::<_, ClientMessage>(&mut s, max).await else {
                    return;
                };
                let reply = match m {
                    ClientMessage::Hello { request_id, .. } => ServerMessage::HelloOk {
                        request_id,
                        protocol_version: 1,
                        connection_id: 1,
                        role: Role::Ui,
                    },
                    ClientMessage::Request { request_id, body } => {
                        lock(&requests).push(body.clone());
                        if body == Request::Subscribe {
                            let ok = ServerMessage::Response {
                                request_id,
                                result: ResponseBody::Subscribed,
                            };
                            if frame::write_frame(&mut s, &ok, max).await.is_err() {
                                return;
                            }
                            while !close.load(Ordering::SeqCst) {
                                let pending: Vec<ServiceEvent> =
                                    std::mem::take(&mut *lock(&events));
                                for event in pending {
                                    let m = ServerMessage::Event { event };
                                    if frame::write_frame(&mut s, &m, max).await.is_err() {
                                        return;
                                    }
                                }
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            return;
                        }
                        let long = matches!(body, Request::CreateBaseline { .. })
                            || matches!(&body, Request::SubmitVm { request }
                                if request.action == VmAction::Start);
                        if long {
                            while hold.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                        match script(&body) {
                            Ok(result) => ServerMessage::Response { request_id, result },
                            Err(error) => ServerMessage::Error {
                                request_id: Some(request_id),
                                error,
                            },
                        }
                    }
                };
                if frame::write_frame(&mut s, &reply, max).await.is_err() {
                    return;
                }
            }
        }
    }

    struct Loop {
        commands: Option<mpsc::UnboundedSender<ServiceCommand>>,
        state: Arc<Mutex<ServiceState>>,
        ticket: TicketSlot,
        thread: std::thread::JoinHandle<()>,
    }

    impl Loop {
        fn start(endpoint: String) -> Self {
            let state = Arc::new(Mutex::new(ServiceState {
                connection: CONNECTING.into(),
                ..ServiceState::default()
            }));
            let ticket: TicketSlot = Arc::new(Mutex::new(None));
            let (commands, rx) = mpsc::unbounded_channel();
            let lanes = Lanes {
                state: state.clone(),
                ticket: ticket.clone(),
                repaint: Arc::new(|| {}),
                tx: commands.downgrade(),
            };
            let cred = Credential::generate().unwrap();
            let thread = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(ipc_loop(endpoint, cred, rx, lanes));
            });
            let l = Self {
                commands: Some(commands),
                state,
                ticket,
                thread,
            };
            l.wait("connection", |s| s.connected);
            l
        }

        fn send(&self, c: ServiceCommand) -> bool {
            send_command(
                self.commands.as_ref().unwrap(),
                &self.state,
                &self.ticket,
                c,
            )
        }

        fn state(&self) -> ServiceState {
            lock(&self.state).clone()
        }

        fn wait(&self, what: &str, ok: impl Fn(&ServiceState) -> bool) {
            let t0 = Instant::now();
            while !ok(&self.state()) {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "{what}: {:?}",
                    self.state()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn wait_thread_end(&self) {
            let t0 = Instant::now();
            while !self.thread.is_finished() {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "IPC thread still running"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn base_reply(r: &Request) -> Result<ResponseBody, ReplyError> {
        match r {
            Request::ListMedia => Ok(ResponseBody::MediaList { media: Vec::new() }),
            // Everything else fails, the VM list included: every refresh fails.
            _ => Err(ReplyError::Operational(lightvm_proto::PublicError::new(
                ErrorCode::InfrastructureError,
                "scripted failure",
            ))),
        }
    }

    fn operation_ok() -> ResponseBody {
        ResponseBody::Operation {
            admission: lightvm_proto::Admission {
                operation_id: OperationId::new_v7(),
                status: OperationStatus::Succeeded,
                duplicate: false,
            },
            operation: None,
        }
    }

    fn rate_limit() -> ReplyError {
        ReplyError::Operational(lightvm_proto::PublicError::resource_limit(
            "request rate over the limit",
        ))
    }

    fn submitted_inputs(server: &fake::Server) -> Vec<InputBatch> {
        lock(&server.requests)
            .iter()
            .filter_map(|r| match r {
                Request::SubmitVm { request } => match &request.action {
                    VmAction::Input(b) => Some(b.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn batch(events: Vec<InputEvent>) -> InputBatch {
        InputBatch {
            vm_id: VmId::new_v7(),
            run_id: RunId::new_v7(),
            geometry: lightvm_proto::DisplayGeometry::new(640, 480).unwrap(),
            events,
        }
    }

    fn key(q: &str, down: bool) -> InputEvent {
        InputEvent::Key(KeyAction {
            qcode: lightvm_proto::QCode::parse(q).unwrap(),
            down,
        })
    }

    /// Finding 7: the deletion succeeded; the re-read of the list that
    /// follows fails: the deletion is reported as done, the refresh failure
    /// only in the last outcome.
    #[test]
    fn successful_delete_is_not_reported_failed_when_the_refresh_fails() {
        let server = fake::start(Arc::new(|r: &Request| match r {
            Request::DeleteVm { vm_id, .. } => Ok(ResponseBody::VmDeleted { vm_id: *vm_id }),
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        let vm = VmId::new_v7();
        assert!(l.send(ServiceCommand::Delete {
            vm_id: vm,
            revision: 1
        }));
        l.wait("deletion outcome", |s| !s.delete.is_pending());
        assert_eq!(l.state().delete, RequestStatus::Done(vm));
        // The refresh that follows fails: recorded in the last outcome only.
        l.wait("refresh failure recorded", |s| {
            s.last_outcome
                .as_deref()
                .is_some_and(|o| o.starts_with("VM list not refreshed"))
        });
        let s = l.state();
        assert_eq!(s.delete, RequestStatus::Done(vm), "{s:?}");
    }

    /// Finding 1: an input batch refused by the rate limit is sent again
    /// once after a short wait.
    #[test]
    fn rate_limited_input_is_retried() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = calls.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::SubmitVm { .. } => {
                if c2.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(rate_limit())
                } else {
                    Ok(operation_ok())
                }
            }
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        let b = batch(vec![key("a", true), key("a", false)]);
        assert!(l.send(ServiceCommand::Input(b.clone())));
        let t0 = Instant::now();
        while submitted_inputs(&server).len() < 2 {
            assert!(t0.elapsed() < Duration::from_secs(10), "no retry");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        let sent = submitted_inputs(&server);
        assert_eq!(sent, [b.clone(), b]);
        assert_eq!(l.state().input_error, None);
    }

    /// Finding 1: still refused after the retry: the key and button
    /// releases are sent on their own, so nothing stays held in the guest.
    #[test]
    fn releases_are_sent_alone_when_input_stays_refused() {
        let server = fake::start(Arc::new(|r: &Request| match r {
            Request::SubmitVm { request } => match &request.action {
                VmAction::Input(b) if b.events.len() == 1 => Ok(operation_ok()),
                _ => Err(rate_limit()),
            },
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        let b = batch(vec![
            InputEvent::PointerMove(lightvm_proto::PointerMove { x: 5, y: 5 }),
            key("ctrl", false),
        ]);
        assert!(l.send(ServiceCommand::Input(b.clone())));
        let t0 = Instant::now();
        while submitted_inputs(&server).len() < 3 {
            assert!(t0.elapsed() < Duration::from_secs(10), "releases not sent");
            std::thread::sleep(Duration::from_millis(10));
        }
        let sent = submitted_inputs(&server);
        assert_eq!(sent[2].events, [key("ctrl", false)]);
    }

    /// Finding 8: when the `Service` (the only strong sender) is dropped,
    /// the IPC executor, its lanes and the thread end, even though the
    /// event connection is still open.
    #[test]
    fn ipc_thread_ends_when_the_service_is_dropped() {
        let server = fake::start(Arc::new(base_reply));
        let mut l = Loop::start(server.endpoint.clone());
        // The loop is idle, the event connection open.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!l.thread.is_finished());
        drop(l.commands.take());
        l.wait_thread_end();
    }

    /// Finding 8: once the supervisor is gone (event connection closed) the
    /// loop ends, `send` returns `false` and waiting requests fail.
    #[test]
    fn send_returns_false_once_the_supervisor_is_gone() {
        let server = fake::start(Arc::new(base_reply));
        let l = Loop::start(server.endpoint.clone());
        // A console ticket request that never gets an answer in time.
        lock(&l.state).console = Some(ConsoleRequest {
            vm_id: VmId::new_v7(),
            status: RequestStatus::Pending,
        });
        server.close_events.store(true, Ordering::SeqCst);
        l.wait_thread_end();
        let s = l.state();
        assert!(!s.connected);
        assert!(matches!(
            s.console.map(|c| c.status),
            Some(RequestStatus::Failed(_))
        ));
        assert!(!l.send(ServiceCommand::Delete {
            vm_id: VmId::new_v7(),
            revision: 1
        }));
        assert!(matches!(l.state().delete, RequestStatus::Failed(_)));
    }

    fn baseline(vm_id: VmId, label: &str) -> BaselineSummary {
        BaselineSummary {
            baseline_id: BaselineId::new_v7(),
            vm_id,
            label: Label::parse(label).unwrap(),
            created_at: lightvm_proto::UtcTimestamp::from_unix_millis(1_791_710_000_000).unwrap(),
            clean_shutdown: true,
            protected: false,
            derived_from: None,
            config_revision: 1,
            virtual_size_bytes: 8 << 30,
            size_bytes: 3 << 30,
            disk_sha256: lightvm_proto::Sha256Digest::from_bytes([7; 32]),
            firmware_sha256: None,
            current: false,
            in_use: false,
        }
    }

    fn requested(server: &fake::Server, f: impl Fn(&Request) -> bool) -> bool {
        lock(&server.requests).iter().any(f)
    }

    /// F3: a baseline creation is long (the reply arrives when it ends,
    /// without timeout): it runs on its own lane, so the VM list, the
    /// baselines list and a deletion are answered meanwhile; its outcome
    /// then re-reads the list.
    #[test]
    fn baseline_creation_does_not_block_other_requests() {
        let vm = VmId::new_v7();
        let created = baseline(vm, "Baseline 1");
        let c2 = created.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::ListBaselines { vm_id } => Ok(ResponseBody::Baselines {
                vm_id: *vm_id,
                baselines: Vec::new(),
            }),
            Request::CreateBaseline { .. } => Ok(ResponseBody::BaselineCreated {
                baseline: c2.clone(),
            }),
            Request::DeleteBaseline { .. } => Err(ReplyError::Operational(
                lightvm_proto::PublicError::state_conflict("baseline in use")
                    .with_detail("reason", "baseline_referenced"),
            )),
            r => base_reply(r),
        }));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        let label = Label::parse("Baseline 1").unwrap();
        assert!(l.send(ServiceCommand::CreateBaseline {
            vm_id: vm,
            label: label.clone(),
            allow_unclean: false,
        }));
        assert!(matches!(
            l.state().baseline,
            Some(BaselineRequest {
                status: RequestStatus::Pending,
                ..
            })
        ));
        l.wait("create_baseline received", |_| {
            requested(&server, |r| matches!(r, Request::CreateBaseline { .. }))
        });
        // Held: the other lanes keep working.
        assert!(l.send(ServiceCommand::ListBaselines(vm)));
        l.wait("list answered during the creation", |s| {
            s.baselines.get(&vm).is_some_and(|b| b.loaded)
        });
        assert!(l.state().baseline.unwrap().status.is_pending());
        server.hold_baseline.store(false, Ordering::SeqCst);
        l.wait("creation done", |s| {
            s.baseline.as_ref().is_some_and(|b| !b.status.is_pending())
        });
        let s = l.state();
        assert_eq!(
            s.baseline,
            Some(BaselineRequest {
                vm_id: vm,
                op: BaselineOp::Create {
                    label,
                    allow_unclean: false
                },
                status: RequestStatus::Done(created.baseline_id),
            })
        );
        // The refused deletion keeps the code and the reason of the reply.
        let id = BaselineId::new_v7();
        assert!(l.send(ServiceCommand::DeleteBaseline {
            vm_id: vm,
            baseline_id: id
        }));
        l.wait("deletion refused", |s| {
            matches!(&s.baseline, Some(b) if matches!(b.op, BaselineOp::Delete(_)) && !b.status.is_pending())
        });
        let Some(BaselineRequest {
            status: RequestStatus::Failed(f),
            ..
        }) = l.state().baseline
        else {
            panic!("not failed")
        };
        assert_eq!(f.code, Some(ErrorCode::StateConflict));
        assert_eq!(f.reason(), Some("baseline_referenced"));
    }

    /// A start replies only after the verification of media and baseline
    /// and the launch: it has its own lane, the VM list is read meanwhile.
    #[test]
    fn start_does_not_block_other_requests() {
        let server = fake::start(Arc::new(|r: &Request| match r {
            Request::SubmitVm { .. } => Ok(operation_ok()),
            Request::ListBaselines { vm_id } => Ok(ResponseBody::Baselines {
                vm_id: *vm_id,
                baselines: Vec::new(),
            }),
            r => base_reply(r),
        }));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        let vm = VmId::new_v7();
        assert!(l.send(ServiceCommand::Start(vm)));
        l.wait("start received", |_| {
            requested(&server, |r| matches!(r, Request::SubmitVm { .. }))
        });
        assert!(l.send(ServiceCommand::ListBaselines(vm)));
        l.wait("list answered during the start", |s| {
            s.baselines.get(&vm).is_some_and(|b| b.loaded)
        });
        assert_ne!(
            l.state().last_outcome.as_deref(),
            Some("start: accepted"),
            "start still waiting"
        );
        server.hold_baseline.store(false, Ordering::SeqCst);
        l.wait("start outcome", |s| {
            s.last_outcome.as_deref() == Some("start: accepted")
        });
    }

    /// F3: `baselines_changed` re-reads the VM list and the baselines of
    /// the VM.
    #[test]
    fn baselines_changed_rereads_the_list() {
        let vm = VmId::new_v7();
        let b = baseline(vm, "Clean install");
        let b2 = b.clone();
        let server = fake::start(Arc::new(move |r: &Request| match r {
            Request::ListBaselines { vm_id } => Ok(ResponseBody::Baselines {
                vm_id: *vm_id,
                baselines: vec![b2.clone()],
            }),
            r => base_reply(r),
        }));
        let l = Loop::start(server.endpoint.clone());
        lock(&server.events).push(ServiceEvent::BaselinesChanged { vm_id: vm });
        l.wait("list re-read after the event", |s| {
            s.baselines.get(&vm).is_some_and(|l| l.items == [b.clone()])
        });
        assert!(requested(&server, |r| *r == Request::ListBaselines { vm_id: vm }));
    }

    /// A baseline operation still waiting when the supervisor goes away
    /// fails (no view keeps waiting).
    #[test]
    fn pending_baseline_operation_fails_when_the_supervisor_is_gone() {
        let server = fake::start(Arc::new(base_reply));
        server.hold_baseline.store(true, Ordering::SeqCst);
        let l = Loop::start(server.endpoint.clone());
        assert!(l.send(ServiceCommand::CreateBaseline {
            vm_id: VmId::new_v7(),
            label: Label::parse("x").unwrap(),
            allow_unclean: true,
        }));
        server.close_events.store(true, Ordering::SeqCst);
        l.wait_thread_end();
        assert!(matches!(
            l.state().baseline.map(|b| b.status),
            Some(RequestStatus::Failed(_))
        ));
    }

    /// The fast lane stays bounded: pointer-only batches are dropped when
    /// it is congested, keys and ticket requests never.
    #[test]
    fn fast_lane_is_bounded_and_keeps_keys() {
        let mut q = VecDeque::new();
        let mv = || {
            ServiceCommand::Input(batch(vec![InputEvent::PointerMove(
                lightvm_proto::PointerMove { x: 1, y: 1 },
            )]))
        };
        for i in 0..200 {
            push_fast(&mut q, mv());
            if i % 10 == 0 {
                push_fast(&mut q, ServiceCommand::Input(batch(vec![key("a", false)])));
            }
            assert!(q.len() <= FAST_LANE_CAPACITY + 20, "{}", q.len());
        }
        let keys = q.iter().filter(|c| !pointer_only(c)).count();
        assert_eq!(keys, 20, "key batches dropped");
        push_fast(&mut q, ServiceCommand::OpenConsole(VmId::new_v7()));
        assert!(matches!(q.back(), Some(ServiceCommand::OpenConsole(_))));
    }

    /// Finding 14: `Debug` of the service does not show the display token,
    /// and the ticket can be discarded.
    #[test]
    fn debug_redacts_the_console_ticket() {
        let token = "feedface".repeat(8);
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (commands, _rx) = mpsc::unbounded_channel();
        let s = Service {
            child,
            stdin: None,
            pid: 1,
            commands,
            state: Arc::new(Mutex::new(ServiceState::default())),
            ticket: Arc::new(Mutex::new(Some(ConsoleTicket {
                vm_id: VmId::new_v7(),
                run_id: RunId::new_v7(),
                socket_path: "display".into(),
                token: token.clone(),
                geometry: None,
            }))),
        };
        let shown = format!("{s:?}");
        assert!(!shown.contains(&token), "{shown}");
        assert!(shown.contains("<redacted>"));
        s.discard_console_ticket();
        assert!(lock(&s.ticket).is_none());
    }
}
