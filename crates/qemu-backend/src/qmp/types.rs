//! Typed QMP messages and response parsing.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

/// Error of the QMP channel or of a QMP command.
#[derive(Debug)]
#[non_exhaustive]
pub enum QmpError {
    /// I/O error on the socket.
    Io(std::io::Error),
    /// Message not conforming to the protocol.
    Protocol(String),
    /// Error returned by QEMU.
    Command {
        /// Command executed.
        command: &'static str,
        /// QAPI class of the error.
        class: String,
        /// QEMU description.
        desc: String,
    },
    /// No response within the command timeout.
    Timeout {
        /// Command executed.
        command: &'static str,
    },
    /// Connection closed by QEMU or lost.
    Disconnected,
    /// Client closed locally.
    Closed,
}

impl fmt::Display for QmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O on the QMP channel: {e}"),
            Self::Protocol(m) => write!(f, "QMP protocol: {m}"),
            Self::Command {
                command,
                class,
                desc,
            } => write!(f, "QEMU rejected `{command}`: {class}: {desc}"),
            Self::Timeout { command } => {
                write!(f, "no QMP response to `{command}` within the timeout")
            }
            Self::Disconnected => write!(f, "QMP channel closed by QEMU"),
            Self::Closed => write!(f, "QMP client closed"),
        }
    }
}

impl QmpError {
    /// Summary without text coming from QEMU (which may contain paths).
    pub fn public_summary(&self) -> String {
        match self {
            Self::Io(_) => "I/O error".to_owned(),
            Self::Protocol(_) => "message not conforming to the protocol".to_owned(),
            Self::Command { command, class, .. } => {
                format!("command `{command}` rejected ({class})")
            }
            Self::Timeout { command } => format!("no response to `{command}` within the timeout"),
            Self::Disconnected => "channel closed by QEMU".to_owned(),
            Self::Closed => "channel closed locally".to_owned(),
        }
    }
}

impl std::error::Error for QmpError {}

impl From<std::io::Error> for QmpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// QEMU version reported by QMP.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QmpVersion {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Micro.
    pub micro: u32,
}

/// Greeting received on connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Greeting {
    /// Version.
    pub version: QmpVersion,
    /// Package (build description).
    pub package: String,
    /// Offered capabilities (for example `oob`).
    pub capabilities: Vec<String>,
}

#[derive(Deserialize)]
struct GreetingWire {
    #[serde(rename = "QMP")]
    qmp: GreetingInner,
}

#[derive(Deserialize)]
struct GreetingInner {
    version: VersionWire,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Deserialize)]
struct VersionWire {
    qemu: QmpVersion,
    #[serde(default)]
    package: String,
}

/// Parses the QMP greeting.
pub(crate) fn parse_greeting(v: &Value) -> Result<Greeting, QmpError> {
    let w: GreetingWire = serde_json::from_value(v.clone())
        .map_err(|e| QmpError::Protocol(format!("invalid greeting: {e}")))?;
    Ok(Greeting {
        version: w.qmp.version.qemu,
        package: w.qmp.version.package,
        capabilities: w.qmp.capabilities,
    })
}

/// Run state (QAPI `RunState`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Before start (`-S`).
    Prelaunch,
    /// Running.
    Running,
    /// Paused.
    Paused,
    /// Shut down (guest stopped, process still alive).
    Shutdown,
    /// Suspended (S3).
    Suspended,
    /// Internal error.
    InternalError,
    /// I/O error on the disks.
    IoError,
    /// Guest panic.
    GuestPanicked,
    /// Watchdog fired.
    Watchdog,
    /// Other state not used by LightVM.
    Other(String),
}

impl RunState {
    fn from_str(s: &str) -> Self {
        match s {
            "prelaunch" => Self::Prelaunch,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "shutdown" => Self::Shutdown,
            "suspended" => Self::Suspended,
            "internal-error" => Self::InternalError,
            "io-error" => Self::IoError,
            "guest-panicked" => Self::GuestPanicked,
            "watchdog" => Self::Watchdog,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Response to `query-status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusInfo {
    /// `true` if the vCPUs are running.
    pub running: bool,
    /// State.
    pub status: RunState,
}

#[derive(Deserialize)]
struct StatusWire {
    running: bool,
    status: String,
}

pub(crate) fn parse_status(v: Value) -> Result<StatusInfo, QmpError> {
    let w: StatusWire = from_value("query-status", v)?;
    Ok(StatusInfo {
        running: w.running,
        status: RunState::from_str(&w.status),
    })
}

/// Response to `query-accelerators`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AcceleratorsInfo {
    /// Active accelerator.
    pub enabled: String,
    /// Accelerators present in the binary.
    #[serde(default)]
    pub present: Vec<String>,
}

pub(crate) fn parse_accelerators(v: Value) -> Result<AcceleratorsInfo, QmpError> {
    from_value("query-accelerators", v)
}

/// Response to `query-version`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    /// Version.
    pub qemu: QmpVersion,
    /// Package.
    pub package: String,
}

pub(crate) fn parse_version(v: Value) -> Result<VersionInfo, QmpError> {
    let w: VersionWire = from_value("query-version", v)?;
    Ok(VersionInfo {
        qemu: w.qemu,
        package: w.package,
    })
}

/// Current machine: name derived from `qom-get /machine type` and the
/// matching entry of `query-machines`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentMachine {
    /// Machine name (for example `pc-q35-11.1`).
    pub name: String,
    /// Alias, if any.
    pub alias: Option<String>,
    /// Deprecated.
    pub deprecated: bool,
    /// Maximum number of CPUs.
    pub cpu_max: Option<u32>,
}

#[derive(Deserialize)]
struct MachineWire {
    name: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    deprecated: bool,
    #[serde(rename = "cpu-max", default)]
    cpu_max: Option<u32>,
}

/// Converts the QOM type (`pc-q35-11.1-machine`) to the machine name.
pub(crate) fn machine_name_from_qom_type(v: &Value) -> Result<String, QmpError> {
    let s = v
        .as_str()
        .ok_or_else(|| QmpError::Protocol(format!("non-text QOM type: {v}")))?;
    Ok(s.strip_suffix("-machine").unwrap_or(s).to_owned())
}

pub(crate) fn find_machine(v: Value, name: &str) -> Result<CurrentMachine, QmpError> {
    let list: Vec<MachineWire> = from_value("query-machines", v)?;
    let m = list.into_iter().find(|m| m.name == name).ok_or_else(|| {
        QmpError::Protocol(format!("machine `{name}` missing from query-machines"))
    })?;
    Ok(CurrentMachine {
        name: m.name,
        alias: m.alias,
        deprecated: m.deprecated,
        cpu_max: m.cpu_max,
    })
}

/// Response to `query-name`.
pub(crate) fn parse_name(v: Value) -> Result<Option<String>, QmpError> {
    #[derive(Deserialize)]
    struct W {
        #[serde(default)]
        name: Option<String>,
    }
    let w: W = from_value("query-name", v)?;
    Ok(w.name)
}

/// PCI device of the network class found by `query-pci`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PciNetworkDevice {
    /// Bus.
    pub bus: u32,
    /// Slot.
    pub slot: u32,
    /// QEMU description.
    pub description: String,
}

/// Looks for class 0x02 devices (network controllers) in the response of
/// `query-pci`, including those behind bridges.
pub(crate) fn pci_network_devices(v: &Value) -> Result<Vec<PciNetworkDevice>, QmpError> {
    let buses = v
        .as_array()
        .ok_or_else(|| QmpError::Protocol("query-pci: expected a list".into()))?;
    let mut out = Vec::new();
    for bus in buses {
        collect_pci(bus.get("devices"), &mut out)?;
    }
    Ok(out)
}

fn collect_pci(devices: Option<&Value>, out: &mut Vec<PciNetworkDevice>) -> Result<(), QmpError> {
    let Some(devices) = devices else {
        return Ok(());
    };
    let devices = devices
        .as_array()
        .ok_or_else(|| QmpError::Protocol("query-pci: `devices` is not a list".into()))?;
    for d in devices {
        let class = d
            .get("class_info")
            .and_then(|c| c.get("class"))
            .and_then(Value::as_u64)
            .ok_or_else(|| QmpError::Protocol("query-pci: class missing".into()))?;
        if class >> 8 == 0x02 {
            out.push(PciNetworkDevice {
                bus: d.get("bus").and_then(Value::as_u64).unwrap_or(0) as u32,
                slot: d.get("slot").and_then(Value::as_u64).unwrap_or(0) as u32,
                description: d
                    .get("class_info")
                    .and_then(|c| c.get("desc"))
                    .and_then(Value::as_str)
                    .unwrap_or("network")
                    .to_owned(),
            });
        }
        if let Some(bridge) = d.get("pci_bridge") {
            collect_pci(bridge.get("devices"), out)?;
        }
    }
    Ok(())
}

/// Asynchronous QMP event.
#[derive(Clone, Debug, PartialEq)]
pub struct QmpEvent {
    /// Event name (for example `SHUTDOWN`).
    pub name: String,
    /// Event data.
    pub data: Value,
    /// QEMU timestamp in microseconds since the Unix epoch.
    pub timestamp_us: u64,
}

impl QmpEvent {
    /// Typed interpretation of the events used by the backend.
    pub fn kind(&self) -> EventKind {
        match self.name.as_str() {
            "SHUTDOWN" => EventKind::Shutdown {
                guest: self
                    .data
                    .get("guest")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                reason: self
                    .data
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            },
            "POWERDOWN" => EventKind::Powerdown,
            "RESET" => EventKind::Reset,
            "STOP" => EventKind::Stop,
            "RESUME" => EventKind::Resume,
            "GUEST_PANICKED" => EventKind::GuestPanicked,
            _ => EventKind::Other,
        }
    }
}

/// Recognised events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// Shutdown (`guest` = requested by the guest; `reason` QAPI `ShutdownCause`).
    Shutdown {
        /// Requested by the guest.
        guest: bool,
        /// QAPI cause.
        reason: String,
    },
    /// ACPI power-off request delivered.
    Powerdown,
    /// Reset.
    Reset,
    /// vCPUs stopped.
    Stop,
    /// vCPUs resumed.
    Resume,
    /// Guest panic.
    GuestPanicked,
    /// Other event.
    Other,
}

/// Message incoming from the server.
#[derive(Debug)]
pub(crate) enum Incoming {
    Return {
        id: Option<u64>,
        value: Value,
    },
    Error {
        id: Option<u64>,
        class: String,
        desc: String,
    },
    Event(QmpEvent),
}

pub(crate) fn classify(v: Value) -> Result<Incoming, QmpError> {
    let id = v.get("id").and_then(Value::as_u64);
    if let Some(name) = v.get("event").and_then(Value::as_str) {
        let ts = v.get("timestamp");
        let secs = ts
            .and_then(|t| t.get("seconds"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let us = ts
            .and_then(|t| t.get("microseconds"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Ok(Incoming::Event(QmpEvent {
            name: name.to_owned(),
            data: v.get("data").cloned().unwrap_or(Value::Null),
            timestamp_us: secs.saturating_mul(1_000_000).saturating_add(us),
        }));
    }
    if let Some(r) = v.get("return") {
        return Ok(Incoming::Return {
            id,
            value: r.clone(),
        });
    }
    if let Some(e) = v.get("error") {
        return Ok(Incoming::Error {
            id,
            class: e
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
            desc: e
                .get("desc")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        });
    }
    Err(QmpError::Protocol(format!("unrecognised message: {v}")))
}

fn from_value<T: serde::de::DeserializeOwned>(cmd: &str, v: Value) -> Result<T, QmpError> {
    serde_json::from_value(v).map_err(|e| QmpError::Protocol(format!("response to {cmd}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real QEMU 11.1.0 greeting on a Unix socket (F0-02 4.3, F0-04).
    const GREETING: &str = include_str!("../../tests/fixtures/qmp-greeting.json");

    #[test]
    fn real_greeting() {
        let v: Value = serde_json::from_str(GREETING).unwrap();
        let g = parse_greeting(&v).unwrap();
        assert_eq!(
            g.version,
            QmpVersion {
                major: 11,
                minor: 1,
                micro: 0
            }
        );
        assert_eq!(g.capabilities, vec!["oob".to_owned()]);
        assert!(parse_greeting(&json!({"x": 1})).is_err());
    }

    #[test]
    fn status_and_accelerators() {
        let s = parse_status(json!({"status": "prelaunch", "running": false})).unwrap();
        assert_eq!(s.status, RunState::Prelaunch);
        let a = parse_accelerators(json!({"enabled": "whpx", "present": ["qtest","tcg","whpx"]}))
            .unwrap();
        assert_eq!(a.enabled, "whpx");
        assert!(parse_status(json!({"status": 3})).is_err());
    }

    #[test]
    fn classification() {
        match classify(json!({"return": {}, "id": 7})).unwrap() {
            Incoming::Return { id, .. } => assert_eq!(id, Some(7)),
            other => panic!("{other:?}"),
        }
        match classify(json!({"error": {"class": "GenericError", "desc": "x"}, "id": 2})).unwrap() {
            Incoming::Error { id, class, .. } => {
                assert_eq!(id, Some(2));
                assert_eq!(class, "GenericError");
            }
            other => panic!("{other:?}"),
        }
        let ev = json!({"timestamp": {"seconds": 2, "microseconds": 5}, "event": "SHUTDOWN",
                        "data": {"guest": false, "reason": "host-qmp-quit"}});
        match classify(ev).unwrap() {
            Incoming::Event(e) => {
                assert_eq!(e.timestamp_us, 2_000_005);
                assert_eq!(
                    e.kind(),
                    EventKind::Shutdown {
                        guest: false,
                        reason: "host-qmp-quit".into()
                    }
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(classify(json!({"foo": 1})).is_err());
    }

    #[test]
    fn pci_without_and_with_nic() {
        let no_nic: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/qmp-query-pci-nodefaults.json"
        ))
        .unwrap();
        assert!(pci_network_devices(&no_nic).unwrap().is_empty());
        let with_nic = json!([{"bus": 0, "devices": [
            {"bus": 0, "slot": 2, "class_info": {"class": 512, "desc": "Ethernet controller"}},
            {"bus": 0, "slot": 3, "class_info": {"class": 1540}, "pci_bridge": {"bus": {}, "devices": [
                {"bus": 1, "slot": 0, "class_info": {"class": 640}}
            ]}}
        ]}]);
        let found = pci_network_devices(&with_nic).unwrap();
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn current_machine() {
        assert_eq!(
            machine_name_from_qom_type(&json!("pc-q35-11.1-machine")).unwrap(),
            "pc-q35-11.1"
        );
        let list = json!([{"name": "pc-q35-11.1", "alias": "q35", "cpu-max": 4096, "deprecated": false},
                          {"name": "pc-q35-5.2", "deprecated": true}]);
        let m = find_machine(list, "pc-q35-11.1").unwrap();
        assert_eq!(m.alias.as_deref(), Some("q35"));
        assert_eq!(m.cpu_max, Some(4096));
    }
}
