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
//! IPC, with two connections: one for requests (reply by `request_id`) and one
//! for events (`subscribe`). Reads of a frame cannot be interrupted safely, so
//! requests and events do not share the connection.
//!
//! The UI opens neither QMP nor VM files: start, stop and Emergency Stop go
//! through the service core (ROADMAP 6.1).

use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{
    ParentLossPolicy, Request, ResponseBody, ServerMessage, ServiceEvent, VmSummary,
};
use lightvm_proto::{
    OperationId, OperationStatus, ReplyError, VmAction, VmConfig, VmId, VmRequest,
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
    /// VMs of the catalog.
    pub vms: Vec<VmSummary>,
    /// Outcome of the last requested action, readable.
    pub last_outcome: Option<String>,
    /// UI loss policy reported by the service.
    pub policy: Option<ParentLossPolicy>,
}

/// Actions requested by the UI from the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceCommand {
    /// Re-reads the VM list and the capabilities.
    Refresh,
    /// Creates a VM in the service's catalog. Not exposed in the widgets: the
    /// guided creation arrives in F2; it serves the tests (F1-12, P-2).
    Create(Box<VmConfig>),
    /// Starts a VM.
    Start(VmId),
    /// Ordinary stop of a VM.
    Stop(VmId),
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
        /// the changes (without installation media: importing media arrives
        /// with the guided creation, F2).
        create: bool,
    },
    /// Opens the VM settings panel: handled by the UI, never sent to the
    /// supervisor.
    OpenSettings(VmId),
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

/// Supervisor started by the UI. On drop it requests the controlled stop
/// and, once [`STOP_TIMEOUT`] has expired, kills the process.
#[derive(Debug)]
pub struct Service {
    child: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    commands: mpsc::UnboundedSender<ServiceCommand>,
    state: Arc<Mutex<ServiceState>>,
}

fn lock(m: &Mutex<ServiceState>) -> MutexGuard<'_, ServiceState> {
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
        let (commands, rx) = mpsc::unbounded_channel();
        let Ready {
            pid,
            endpoint,
            credential,
        } = ready;
        let s2 = state.clone();
        let tx2 = commands.clone();
        std::thread::Builder::new()
            .name("service-ipc".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        lock(&s2).connection = format!("IPC runtime not started: {e}");
                        repaint();
                        return;
                    }
                };
                rt.block_on(ipc_loop(endpoint, credential, rx, tx2, s2, repaint));
            })
            .map_err(StartError::Spawn)?;
        Ok(Self {
            child,
            stdin,
            pid,
            commands,
            state,
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

    /// Queues an action. Returns `false` if the IPC channel is closed.
    pub fn send(&self, c: ServiceCommand) -> bool {
        self.commands.send(c).is_ok()
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

impl Drop for Service {
    fn drop(&mut self) {
        if self.is_alive() {
            let _ = self.stop(STOP_TIMEOUT);
        } else {
            let _ = self.child.wait();
        }
    }
}

/// Readable label of the state of an operation.
pub fn operation_status_label(s: OperationStatus) -> &'static str {
    match s {
        OperationStatus::Queued => "queued",
        OperationStatus::Forwarded => "forwarded",
        OperationStatus::Succeeded => "succeeded",
        OperationStatus::Failed => "failed",
        OperationStatus::Discarded => "discarded",
    }
}

fn describe_error(m: &ServerMessage) -> String {
    match m {
        ServerMessage::Error { error, .. } => match error {
            ReplyError::Operational(e) => format!("{}: {}", e.code.as_str(), e.message),
            ReplyError::Protocol(e) => format!("protocol: {e}"),
        },
        ServerMessage::Closing { reason } => {
            format!("connection closed by the service: {reason:?}")
        }
        other => format!("unexpected reply: {other:?}"),
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

async fn ipc_loop(
    endpoint: String,
    cred: Credential,
    mut rx: mpsc::UnboundedReceiver<ServiceCommand>,
    tx: mpsc::UnboundedSender<ServiceCommand>,
    state: Arc<Mutex<ServiceState>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) {
    let update = |f: &dyn Fn(&mut ServiceState)| {
        f(&mut lock(&state));
        repaint();
    };
    let connections = async {
        let r = connect(&endpoint, &cred).await?;
        let e = connect(&endpoint, &cred).await?;
        Ok::<_, String>((r, e))
    }
    .await;
    drop(cred);
    let (mut requests, mut events) = match connections {
        Ok(x) => x,
        Err(m) => {
            update(&|s| s.connection = m.clone());
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
            update(&|s| s.connection = format!("event subscription refused: {d}"));
            return;
        }
        Err(e) => {
            update(&|s| s.connection = format!("event subscription failed: {e}"));
            return;
        }
    }
    update(&|s| {
        s.connection = "connected".into();
        s.connected = true;
    });
    let _ = tx.send(ServiceCommand::Refresh);

    let state_ev = state.clone();
    let repaint_ev = repaint.clone();
    let event_reader = async move {
        loop {
            match events.next_event().await {
                Ok(Ok(ServiceEvent::VmState { vm_id, state })) => {
                    {
                        let mut s = lock(&state_ev);
                        if let Some(v) = s.vms.iter_mut().find(|v| v.vm_id == vm_id) {
                            v.state = state;
                        }
                    }
                    repaint_ev();
                    // The run_id changes on start: the list is re-read.
                    let _ = tx.send(ServiceCommand::Refresh);
                }
                Ok(Ok(_)) => {}
                Ok(Err(m)) => {
                    let d = describe_error(&m);
                    let mut s = lock(&state_ev);
                    s.connection = d;
                    s.connected = false;
                    drop(s);
                    repaint_ev();
                    return;
                }
                Err(e) => {
                    let mut s = lock(&state_ev);
                    s.connection = format!("event channel closed: {e}");
                    s.connected = false;
                    drop(s);
                    repaint_ev();
                    return;
                }
            }
        }
    };

    let executor = async {
        while let Some(c) = rx.recv().await {
            let outcome = execute(&mut requests, &state, c).await;
            if let Err(m) = outcome {
                update(&|s| s.last_outcome = Some(m.clone()));
            } else {
                repaint();
            }
        }
    };
    tokio::join!(event_reader, executor);
}

async fn execute(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    command: ServiceCommand,
) -> Result<(), String> {
    match command {
        ServiceCommand::Refresh => {
            let caps = c
                .request(Request::GetCapabilities)
                .await
                .map_err(|e| e.to_string())?;
            if let ServerMessage::Response {
                result: ResponseBody::Capabilities(k),
                ..
            } = caps
            {
                let mut s = lock(state);
                s.backend = Some(k.backend);
                s.accelerator = k.accelerator;
                s.policy = Some(k.parent_loss_policy);
            }
            match c
                .request(Request::ListVms)
                .await
                .map_err(|e| e.to_string())?
            {
                ServerMessage::Response {
                    result: ResponseBody::VmList { vms },
                    ..
                } => {
                    lock(state).vms = vms;
                    Ok(())
                }
                m => Err(describe_error(&m)),
            }
        }
        ServiceCommand::Create(config) => {
            match c
                .request(Request::CreateVm { config: *config })
                .await
                .map_err(|e| e.to_string())?
            {
                ServerMessage::Response {
                    result: ResponseBody::VmCreated { vm },
                    ..
                } => {
                    lock(state).last_outcome =
                        Some(format!("created: {}", vm.config.name.as_str()));
                    Box::pin(execute(c, state, ServiceCommand::Refresh)).await
                }
                m => Err(format!("creation: {}", describe_error(&m))),
            }
        }
        ServiceCommand::SetPolicy(policy) => {
            match c
                .request(Request::SetParentLossPolicy { policy })
                .await
                .map_err(|e| e.to_string())?
            {
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
                m => Err(format!("setting: {}", describe_error(&m))),
            }
        }
        ServiceCommand::OpenSettings(_) => Ok(()),
        ServiceCommand::UpdateVm {
            vm_id,
            revision,
            config,
        } => {
            update_vm(c, state, vm_id, revision, *config).await?;
            Box::pin(execute(c, state, ServiceCommand::Refresh)).await
        }
        ServiceCommand::OpenVm {
            name,
            changes,
            create,
        } => {
            Box::pin(execute(c, state, ServiceCommand::Refresh)).await?;
            let found = lock(state)
                .vms
                .iter()
                .find(|v| v.name.as_str() == name)
                .cloned();
            let vm = match found {
                Some(v) => v,
                None if create => {
                    let mut config = crate::vm_settings::default_config(&name)?;
                    changes.apply(&mut config);
                    Box::pin(execute(c, state, ServiceCommand::Create(Box::new(config)))).await?;
                    lock(state)
                        .vms
                        .iter()
                        .find(|v| v.name.as_str() == name)
                        .cloned()
                        .ok_or_else(|| format!("VM \"{name}\" not found after creation"))?
                }
                None => return Err(format!("VM \"{name}\" not found in the catalog")),
            };
            let stopped = vm.state == lightvm_proto::VmState::Stopped;
            if !changes.is_empty() {
                if !stopped {
                    return Err(format!(
                        "VM \"{name}\" running: resources and limits can be changed only while the VM is stopped"
                    ));
                }
                let mut config = vm.config.clone();
                changes.apply(&mut config);
                if config != vm.config {
                    update_vm(c, state, vm.vm_id, vm.config_revision, config).await?;
                }
            }
            let start = if stopped {
                vm_action(c, state, vm.vm_id, VmAction::Start, "start").await
            } else {
                Ok(())
            };
            // The list is re-read even if the start fails: the saved changes
            // stay visible.
            Box::pin(execute(c, state, ServiceCommand::Refresh)).await?;
            start
        }
        ServiceCommand::Start(vm_id) => vm_action(c, state, vm_id, VmAction::Start, "start").await,
        ServiceCommand::Stop(vm_id) => vm_action(c, state, vm_id, VmAction::Stop, "stop").await,
        ServiceCommand::EmergencyStop => {
            match c
                .request(Request::EmergencyStop)
                .await
                .map_err(|e| e.to_string())?
            {
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
                m => Err(describe_error(&m)),
            }
        }
    }
}

async fn update_vm(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    vm_id: VmId,
    revision: u64,
    config: VmConfig,
) -> Result<(), String> {
    match c
        .request(Request::UpdateVm {
            vm_id,
            expected_revision: revision,
            config,
        })
        .await
        .map_err(|e| e.to_string())?
    {
        ServerMessage::Response {
            result: ResponseBody::VmUpdated { vm },
            ..
        } => {
            lock(state).last_outcome = Some(format!("settings saved: {}", vm.config.name.as_str()));
            Ok(())
        }
        m => Err(format!("settings: {}", describe_error(&m))),
    }
}

async fn vm_action(
    c: &mut Client,
    state: &Mutex<ServiceState>,
    vm_id: VmId,
    action: VmAction,
    name: &str,
) -> Result<(), String> {
    let req = VmRequest {
        operation_id: OperationId::new_v7(),
        vm_id,
        expected_run_id: None,
        action,
    };
    match c
        .request(Request::SubmitVm { request: req })
        .await
        .map_err(|e| e.to_string())?
    {
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
        m => Err(format!("{name}: {}", describe_error(&m))),
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
}
