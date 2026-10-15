//! Asynchronous QMP client over a Unix socket (ROADMAP 6.3, 7.4; ADR-0012).
//!
//! - AF_UNIX connection with `socket2` (on Windows `std::os::unix` does not exist;
//!   F0-04 chose `socket2`). QEMU serves only one QMP client at a time: the
//!   backend is the sole owner of the connection.
//! - Greeting, `qmp_capabilities` with `oob` enabled if offered.
//! - Every command carries a numeric `id`; responses are correlated by `id`
//!   by a dedicated reader thread, events go on a separate channel
//!   (`tokio::sync::broadcast`).
//! - Per-command timeout; a late response is discarded.
//! - A single socket handle shared between reading, writing and closing,
//!   blocking reads without `SO_RCVTIMEO` after the handshake (F0-04, point 8:
//!   after a receive timeout the Winsock stream may remain in an
//!   undefined state).
//! - No public API runs an arbitrary QMP command: there are only
//!   typed methods with arguments built by the backend.

mod types;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use socket2::{Domain, SockAddr, Socket, Type};
use tokio::sync::{broadcast, oneshot, watch};

use crate::paths::ManagedPath;
use lightvm_proto::InputEvent;

pub use types::{
    AcceleratorsInfo, CurrentMachine, EventKind, Greeting, PciNetworkDevice, QmpError, QmpEvent,
    QmpVersion, RunState, StatusInfo, VersionInfo,
};

/// Client configuration.
#[derive(Clone, Debug)]
pub struct QmpConfig {
    /// Default per-command timeout.
    pub command_timeout: Duration,
    /// Timeout for greeting and negotiation.
    pub handshake_timeout: Duration,
    /// Maximum length of a QMP line.
    pub max_line_bytes: usize,
    /// Capacity of the event channel (slow subscribers lose events).
    pub event_capacity: usize,
}

impl Default for QmpConfig {
    fn default() -> Self {
        Self {
            command_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(5),
            max_line_bytes: 16 * 1024 * 1024,
            event_capacity: 256,
        }
    }
}

/// Connection state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// Negotiation completed, commands accepted.
    Ready,
    /// Connection closed (by QEMU, by error or locally).
    Closed {
        /// Reason.
        reason: String,
    },
}

type Pending = HashMap<u64, (&'static str, oneshot::Sender<Result<Value, QmpError>>)>;

#[derive(Debug)]
struct Shared {
    sock: Socket,
    pending: Mutex<Pending>,
    closing: AtomicBool,
    state_tx: watch::Sender<ConnectionState>,
    events_tx: broadcast::Sender<QmpEvent>,
}

/// Identifier reserved for the request that unblocks the reader on close.
const CLOSE_NUDGE_ID: u64 = u64::MAX;

struct SockReader(Arc<Shared>);

impl Read for SockReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&self.0.sock).read(buf)
    }
}

/// QMP client. Sole owner of the connection with a QEMU process.
#[derive(Debug)]
pub struct QmpClient {
    shared: Arc<Shared>,
    writer_tx: Mutex<Option<std_mpsc::Sender<Vec<u8>>>>,
    reader_done: Mutex<Option<std_mpsc::Receiver<()>>>,
    next_id: AtomicU64,
    greeting: Greeting,
    oob: bool,
    cfg: QmpConfig,
}

/// QMP commands used by the backend. The type is private: no external caller
/// can compose a command.
enum Cmd {
    QueryStatus,
    QueryAccelerators,
    QueryVersion,
    QueryMachines,
    QomGetMachineType,
    QueryName,
    QueryPci,
    HmpInfoNetwork,
    SystemPowerdown,
    SystemReset,
    Quit,
    Cont,
    Screendump { filename: String },
    InputSendEvent { events: Vec<Value> },
    BlockdevSnapshotInternal { device: &'static str, name: String },
    BlockdevSnapshotDeleteInternal { device: &'static str, name: String },
}

impl Cmd {
    fn name(&self) -> &'static str {
        match self {
            Self::QueryStatus => "query-status",
            Self::QueryAccelerators => "query-accelerators",
            Self::QueryVersion => "query-version",
            Self::QueryMachines => "query-machines",
            Self::QomGetMachineType => "qom-get",
            Self::QueryName => "query-name",
            Self::QueryPci => "query-pci",
            Self::HmpInfoNetwork => "human-monitor-command",
            Self::SystemPowerdown => "system_powerdown",
            Self::SystemReset => "system_reset",
            Self::Quit => "quit",
            Self::Cont => "cont",
            Self::Screendump { .. } => "screendump",
            Self::InputSendEvent { .. } => "input-send-event",
            Self::BlockdevSnapshotInternal { .. } => "blockdev-snapshot-internal-sync",
            Self::BlockdevSnapshotDeleteInternal { .. } => "blockdev-snapshot-delete-internal-sync",
        }
    }

    fn arguments(self) -> Option<Value> {
        match self {
            Self::QomGetMachineType => Some(json!({"path": "/machine", "property": "type"})),
            Self::QueryMachines => Some(json!({"compat-props": false})),
            Self::HmpInfoNetwork => Some(json!({"command-line": "info network"})),
            Self::Screendump { filename } => Some(json!({"filename": filename, "format": "ppm"})),
            Self::InputSendEvent { events } => Some(json!({ "events": events })),
            Self::BlockdevSnapshotInternal { device, name }
            | Self::BlockdevSnapshotDeleteInternal { device, name } => {
                Some(json!({"device": device, "name": name}))
            }
            _ => None,
        }
    }
}

fn read_json_line<R: BufRead>(r: &mut R, max: usize) -> Result<Option<Value>, QmpError> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = r
            .by_ref()
            .take(max as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() != Some(&b'\n') {
            if buf.len() > max {
                return Err(QmpError::Protocol(format!("QMP line over {max} bytes")));
            }
            return Ok(None);
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return serde_json::from_slice(&buf)
            .map(Some)
            .map_err(|e| QmpError::Protocol(format!("invalid JSON: {e}")));
    }
}

impl QmpClient {
    /// Connects to the QMP socket, reads the greeting and negotiates the capabilities.
    pub async fn connect(socket: &ManagedPath, cfg: QmpConfig) -> Result<Self, QmpError> {
        let path = socket.as_path().to_owned();
        tokio::task::spawn_blocking(move || Self::connect_blocking(&path, cfg))
            .await
            .map_err(|e| QmpError::Protocol(format!("connection task: {e}")))?
    }

    fn connect_blocking(path: &Path, cfg: QmpConfig) -> Result<Self, QmpError> {
        let sock = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        sock.connect(&SockAddr::unix(path)?)?;
        // Receive timeout only during the handshake; if it fires, the
        // connection is abandoned (F0-04, point 8).
        sock.set_read_timeout(Some(cfg.handshake_timeout))?;
        sock.set_write_timeout(Some(cfg.handshake_timeout))?;
        let (state_tx, _) = watch::channel(ConnectionState::Ready);
        let (events_tx, _) = broadcast::channel(cfg.event_capacity.max(1));
        let shared = Arc::new(Shared {
            sock,
            pending: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
            state_tx,
            events_tx,
        });
        let mut reader = BufReader::new(SockReader(shared.clone()));
        let greeting_v =
            read_json_line(&mut reader, cfg.max_line_bytes)?.ok_or(QmpError::Disconnected)?;
        let greeting = types::parse_greeting(&greeting_v)?;
        let oob = greeting.capabilities.iter().any(|c| c == "oob");
        let mut neg = if oob {
            json!({"execute": "qmp_capabilities", "arguments": {"enable": ["oob"]}, "id": 0})
        } else {
            json!({"execute": "qmp_capabilities", "id": 0})
        }
        .to_string();
        neg.push('\n');
        (&shared.sock).write_all(neg.as_bytes())?;
        loop {
            let v =
                read_json_line(&mut reader, cfg.max_line_bytes)?.ok_or(QmpError::Disconnected)?;
            match types::classify(v)? {
                types::Incoming::Return { id: Some(0), .. } => break,
                types::Incoming::Error { class, desc, .. } => {
                    return Err(QmpError::Command {
                        command: "qmp_capabilities",
                        class,
                        desc,
                    });
                }
                types::Incoming::Event(e) => {
                    let _ = shared.events_tx.send(e);
                }
                types::Incoming::Return { .. } => {
                    return Err(QmpError::Protocol(
                        "unexpected response during negotiation".into(),
                    ));
                }
            }
        }
        shared.sock.set_read_timeout(None)?;
        shared.sock.set_write_timeout(None)?;

        let (writer_tx, writer_rx) = std_mpsc::channel::<Vec<u8>>();
        let w_shared = shared.clone();
        std::thread::Builder::new()
            .name("qmp-writer".into())
            .spawn(move || {
                while let Ok(line) = writer_rx.recv() {
                    if (&w_shared.sock).write_all(&line).is_err() {
                        break;
                    }
                }
            })?;
        let (done_tx, done_rx) = std_mpsc::channel();
        let r_shared = shared.clone();
        let max = cfg.max_line_bytes;
        std::thread::Builder::new()
            .name("qmp-reader".into())
            .spawn(move || {
                let reason = reader_loop(&r_shared, &mut reader, max);
                finish(&r_shared, reason);
                let _ = done_tx.send(());
            })?;

        Ok(Self {
            shared,
            writer_tx: Mutex::new(Some(writer_tx)),
            reader_done: Mutex::new(Some(done_rx)),
            next_id: AtomicU64::new(1),
            greeting,
            oob,
            cfg,
        })
    }

    /// Received greeting.
    pub fn greeting(&self) -> &Greeting {
        &self.greeting
    }

    /// `true` if the `oob` capability was enabled.
    pub fn oob_enabled(&self) -> bool {
        self.oob
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        self.shared.state_tx.borrow().clone()
    }

    /// Observer of the connection state.
    pub fn subscribe_state(&self) -> watch::Receiver<ConnectionState> {
        self.shared.state_tx.subscribe()
    }

    /// New subscription to asynchronous events (receives subsequent events).
    pub fn subscribe_events(&self) -> broadcast::Receiver<QmpEvent> {
        self.shared.events_tx.subscribe()
    }

    async fn execute(&self, cmd: Cmd, timeout: Option<Duration>) -> Result<Value, QmpError> {
        let name = cmd.name();
        if self.shared.closing.load(Ordering::SeqCst) {
            return Err(QmpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.shared.pending).insert(id, (name, tx));
        if let ConnectionState::Closed { .. } = *self.shared.state_tx.borrow() {
            lock(&self.shared.pending).remove(&id);
            return Err(QmpError::Disconnected);
        }
        let mut msg = json!({"execute": name, "id": id});
        if let Some(args) = cmd.arguments() {
            msg["arguments"] = args;
        }
        let mut line = msg.to_string().into_bytes();
        line.push(b'\n');
        let sent = lock(&self.writer_tx)
            .as_ref()
            .map(|w| w.send(line).is_ok())
            .unwrap_or(false);
        if !sent {
            lock(&self.shared.pending).remove(&id);
            return Err(QmpError::Closed);
        }
        let t = timeout.unwrap_or(self.cfg.command_timeout);
        match tokio::time::timeout(t, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(QmpError::Disconnected),
            Err(_) => {
                lock(&self.shared.pending).remove(&id);
                Err(QmpError::Timeout { command: name })
            }
        }
    }

    /// `query-status`.
    pub async fn query_status(&self) -> Result<StatusInfo, QmpError> {
        types::parse_status(self.execute(Cmd::QueryStatus, None).await?)
    }

    /// `query-status` with an explicit timeout (state reconciliation).
    pub async fn query_status_within(&self, timeout: Duration) -> Result<StatusInfo, QmpError> {
        types::parse_status(self.execute(Cmd::QueryStatus, Some(timeout)).await?)
    }

    /// `query-accelerators` (verification of the active accelerator, ADR-0007).
    pub async fn query_accelerators(&self) -> Result<AcceleratorsInfo, QmpError> {
        types::parse_accelerators(self.execute(Cmd::QueryAccelerators, None).await?)
    }

    /// `query-version`.
    pub async fn query_version(&self) -> Result<VersionInfo, QmpError> {
        types::parse_version(self.execute(Cmd::QueryVersion, None).await?)
    }

    /// Current machine. `query-current-machine` of QEMU 11.1 does not report the
    /// name (F0-02 4.1): the name is read with `qom-get /machine type` and only
    /// the matching entry of `query-machines` is returned.
    pub async fn query_current_machine(&self) -> Result<CurrentMachine, QmpError> {
        let ty = self.execute(Cmd::QomGetMachineType, None).await?;
        let name = types::machine_name_from_qom_type(&ty)?;
        let list = self.execute(Cmd::QueryMachines, None).await?;
        types::find_machine(list, &name)
    }

    /// `query-name`.
    pub async fn query_name(&self) -> Result<Option<String>, QmpError> {
        types::parse_name(self.execute(Cmd::QueryName, None).await?)
    }

    /// Diagnostics: text of `info network` via `human-monitor-command`.
    /// Reserved for diagnostics and for verifying the absence of NICs; the
    /// HMP command is fixed.
    pub async fn network_diagnostics(&self) -> Result<String, QmpError> {
        let v = self.execute(Cmd::HmpInfoNetwork, None).await?;
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| QmpError::Protocol("info network: non-text response".into()))
    }

    /// PCI devices of the network class (`query-pci`).
    pub async fn pci_network_devices(&self) -> Result<Vec<PciNetworkDevice>, QmpError> {
        let v = self.execute(Cmd::QueryPci, None).await?;
        types::pci_network_devices(&v)
    }

    /// `system_powerdown` (ACPI request; the guest may ignore it).
    pub async fn system_powerdown(&self) -> Result<(), QmpError> {
        self.execute(Cmd::SystemPowerdown, None).await.map(drop)
    }

    /// `system_reset`.
    pub async fn system_reset(&self) -> Result<(), QmpError> {
        self.execute(Cmd::SystemReset, None).await.map(drop)
    }

    /// `quit`. The channel closing right after the command is a success.
    pub async fn quit(&self) -> Result<(), QmpError> {
        match self.execute(Cmd::Quit, None).await {
            Ok(_) | Err(QmpError::Disconnected) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// `system_powerdown` with an explicit timeout (shutdown escalation).
    pub async fn system_powerdown_within(&self, timeout: Duration) -> Result<(), QmpError> {
        self.execute(Cmd::SystemPowerdown, Some(timeout))
            .await
            .map(drop)
    }

    /// `quit` with an explicit timeout (shutdown escalation).
    pub async fn quit_within(&self, timeout: Duration) -> Result<(), QmpError> {
        match self.execute(Cmd::Quit, Some(timeout)).await {
            Ok(_) | Err(QmpError::Disconnected) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// `blockdev-snapshot-internal-sync` on the root node `device` (ADR-0032):
    /// disk-only internal snapshot `name` of a running VM. `name` is chosen
    /// by the supervisor (a snapshot identifier), never by a client.
    pub(crate) async fn snapshot_internal(
        &self,
        device: &'static str,
        name: &str,
        timeout: Duration,
    ) -> Result<(), QmpError> {
        self.execute(
            Cmd::BlockdevSnapshotInternal {
                device,
                name: name.to_owned(),
            },
            Some(timeout),
        )
        .await
        .map(drop)
    }

    /// `blockdev-snapshot-delete-internal-sync` of the internal snapshot
    /// `name` on the root node `device` (ADR-0032).
    pub(crate) async fn snapshot_delete_internal(
        &self,
        device: &'static str,
        name: &str,
        timeout: Duration,
    ) -> Result<(), QmpError> {
        self.execute(
            Cmd::BlockdevSnapshotDeleteInternal {
                device,
                name: name.to_owned(),
            },
            Some(timeout),
        )
        .await
        .map(drop)
    }

    /// `cont`: starts the vCPUs after `-S` (used by the backend after the checks).
    pub(crate) async fn cont(&self) -> Result<(), QmpError> {
        self.execute(Cmd::Cont, None).await.map(drop)
    }

    /// `screendump` as PPM to a file chosen by the backend in the private
    /// directory (never a client path).
    pub(crate) async fn screendump_ppm(
        &self,
        file: &ManagedPath,
        timeout: Duration,
    ) -> Result<(), QmpError> {
        self.execute(
            Cmd::Screendump {
                filename: file.qemu_value().to_owned(),
            },
            Some(timeout),
        )
        .await
        .map(drop)
    }

    /// `input-send-event` with typed events already validated by
    /// [`crate::input::validate_events`] (absolute coordinates 0..=32767).
    /// No automatic retry after a timeout (ROADMAP 6.3).
    pub(crate) async fn input_send_event(&self, events: &[InputEvent]) -> Result<(), QmpError> {
        if events.is_empty() {
            return Ok(());
        }
        let list = crate::input::events_json(events);
        self.execute(Cmd::InputSendEvent { events: list }, None)
            .await
            .map(drop)
    }

    /// Clean close without stopping QEMU: blocks new commands, triggers a
    /// response to unblock the reader, closes the write side and waits
    /// at most `wait` for the reader thread to end.
    pub async fn close(&self, wait: Duration) {
        self.begin_close();
        let done = lock(&self.reader_done).take();
        if let Some(rx) = done {
            let _ = tokio::task::spawn_blocking(move || rx.recv_timeout(wait)).await;
        }
    }

    fn begin_close(&self) {
        if self.shared.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        let nudge = format!("{{\"execute\": \"query-status\", \"id\": {CLOSE_NUDGE_ID}}}\n");
        if let Some(w) = lock(&self.writer_tx).take() {
            let _ = w.send(nudge.into_bytes());
        }
        // The writer thread ends after draining the queue; the
        // shutdown of the write side makes QEMU close the connection.
        let shared = self.shared.clone();
        let _ = std::thread::Builder::new()
            .name("qmp-close".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                let _ = shared.sock.shutdown(std::net::Shutdown::Write);
            });
    }
}

impl Drop for QmpClient {
    fn drop(&mut self) {
        self.begin_close();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn reader_loop<R: BufRead>(shared: &Shared, reader: &mut R, max: usize) -> String {
    loop {
        let v = match read_json_line(reader, max) {
            Ok(Some(v)) => v,
            Ok(None) => return "connection closed by QEMU".into(),
            Err(e) => return e.to_string(),
        };
        match types::classify(v) {
            Ok(types::Incoming::Event(e)) => {
                let _ = shared.events_tx.send(e);
            }
            Ok(types::Incoming::Return {
                id: Some(id),
                value,
            }) => {
                if let Some((_, tx)) = lock(&shared.pending).remove(&id) {
                    let _ = tx.send(Ok(value));
                }
            }
            Ok(types::Incoming::Error {
                id: Some(id),
                class,
                desc,
            }) => {
                if let Some((command, tx)) = lock(&shared.pending).remove(&id) {
                    let _ = tx.send(Err(QmpError::Command {
                        command,
                        class,
                        desc,
                    }));
                }
            }
            // Responses without `id` cannot be correlated: discarded.
            Ok(_) => {}
            Err(e) => return e.to_string(),
        }
    }
}

fn finish(shared: &Shared, reason: String) {
    let closing = shared.closing.load(Ordering::SeqCst);
    shared
        .state_tx
        .send_replace(ConnectionState::Closed { reason });
    let drained: Vec<_> = lock(&shared.pending).drain().collect();
    for (_, (_, tx)) in drained {
        let _ = tx.send(Err(if closing {
            QmpError::Closed
        } else {
            QmpError::Disconnected
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lines() {
        let mut r = BufReader::new(&b"\r\n{\"a\": 1}\r\n{\"b\": 2}\n"[..]);
        assert_eq!(read_json_line(&mut r, 1024).unwrap().unwrap()["a"], 1);
        assert_eq!(read_json_line(&mut r, 1024).unwrap().unwrap()["b"], 2);
        assert!(read_json_line(&mut r, 1024).unwrap().is_none());
        let mut long = BufReader::new(&b"{\"a\": \"xxxxxxxxxxxxxxxxxxxx\"}\n"[..]);
        assert!(read_json_line(&mut long, 8).is_err());
        let mut bad = BufReader::new(&b"{nope\n"[..]);
        assert!(read_json_line(&mut bad, 1024).is_err());
    }
}
