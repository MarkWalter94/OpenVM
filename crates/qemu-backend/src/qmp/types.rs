//! Messaggi QMP tipizzati e analisi delle risposte.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

/// Errore del canale o di un comando QMP.
#[derive(Debug)]
#[non_exhaustive]
pub enum QmpError {
    /// Errore di I/O sul socket.
    Io(std::io::Error),
    /// Messaggio non conforme al protocollo.
    Protocol(String),
    /// Errore restituito da QEMU.
    Command {
        /// Comando eseguito.
        command: &'static str,
        /// Classe QAPI dell'errore.
        class: String,
        /// Descrizione di QEMU.
        desc: String,
    },
    /// Nessuna risposta entro il timeout del comando.
    Timeout {
        /// Comando eseguito.
        command: &'static str,
    },
    /// Connessione chiusa da QEMU o persa.
    Disconnected,
    /// Client chiuso localmente.
    Closed,
}

impl fmt::Display for QmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O sul canale QMP: {e}"),
            Self::Protocol(m) => write!(f, "protocollo QMP: {m}"),
            Self::Command {
                command,
                class,
                desc,
            } => write!(f, "QEMU ha rifiutato `{command}`: {class}: {desc}"),
            Self::Timeout { command } => write!(f, "nessuna risposta QMP a `{command}` entro il timeout"),
            Self::Disconnected => write!(f, "canale QMP chiuso da QEMU"),
            Self::Closed => write!(f, "client QMP chiuso"),
        }
    }
}

impl std::error::Error for QmpError {}

impl From<std::io::Error> for QmpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Versione QEMU riportata da QMP.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QmpVersion {
    /// Maggiore.
    pub major: u32,
    /// Minore.
    pub minor: u32,
    /// Micro.
    pub micro: u32,
}

/// Greeting ricevuto alla connessione.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Greeting {
    /// Versione.
    pub version: QmpVersion,
    /// Pacchetto (descrizione della build).
    pub package: String,
    /// Capacità offerte (per esempio `oob`).
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

/// Analizza il greeting QMP.
pub(crate) fn parse_greeting(v: &Value) -> Result<Greeting, QmpError> {
    let w: GreetingWire = serde_json::from_value(v.clone())
        .map_err(|e| QmpError::Protocol(format!("greeting non valido: {e}")))?;
    Ok(Greeting {
        version: w.qmp.version.qemu,
        package: w.qmp.version.package,
        capabilities: w.qmp.capabilities,
    })
}

/// Stato di esecuzione (`RunState` QAPI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Prima dell'avvio (`-S`).
    Prelaunch,
    /// In esecuzione.
    Running,
    /// In pausa.
    Paused,
    /// Spento (guest arrestato, processo ancora vivo).
    Shutdown,
    /// Sospeso (S3).
    Suspended,
    /// Errore interno.
    InternalError,
    /// Errore di I/O sui dischi.
    IoError,
    /// Panico del guest.
    GuestPanicked,
    /// Watchdog scattato.
    Watchdog,
    /// Altro stato non usato da LightVM.
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

/// Risposta a `query-status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusInfo {
    /// `true` se le vCPU eseguono.
    pub running: bool,
    /// Stato.
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

/// Risposta a `query-accelerators`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AcceleratorsInfo {
    /// Acceleratore attivo.
    pub enabled: String,
    /// Acceleratori presenti nel binario.
    #[serde(default)]
    pub present: Vec<String>,
}

pub(crate) fn parse_accelerators(v: Value) -> Result<AcceleratorsInfo, QmpError> {
    from_value("query-accelerators", v)
}

/// Risposta a `query-version`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    /// Versione.
    pub qemu: QmpVersion,
    /// Pacchetto.
    pub package: String,
}

pub(crate) fn parse_version(v: Value) -> Result<VersionInfo, QmpError> {
    let w: VersionWire = from_value("query-version", v)?;
    Ok(VersionInfo {
        qemu: w.qemu,
        package: w.package,
    })
}

/// Macchina corrente: nome ricavato da `qom-get /machine type` e voce
/// corrispondente di `query-machines`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentMachine {
    /// Nome della macchina (per esempio `pc-q35-11.1`).
    pub name: String,
    /// Alias, se esiste.
    pub alias: Option<String>,
    /// Deprecata.
    pub deprecated: bool,
    /// Numero massimo di CPU.
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

/// Converte il tipo QOM (`pc-q35-11.1-machine`) nel nome della macchina.
pub(crate) fn machine_name_from_qom_type(v: &Value) -> Result<String, QmpError> {
    let s = v
        .as_str()
        .ok_or_else(|| QmpError::Protocol(format!("tipo QOM non testuale: {v}")))?;
    Ok(s.strip_suffix("-machine").unwrap_or(s).to_owned())
}

pub(crate) fn find_machine(v: Value, name: &str) -> Result<CurrentMachine, QmpError> {
    let list: Vec<MachineWire> = from_value("query-machines", v)?;
    let m = list
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| QmpError::Protocol(format!("macchina `{name}` assente da query-machines")))?;
    Ok(CurrentMachine {
        name: m.name,
        alias: m.alias,
        deprecated: m.deprecated,
        cpu_max: m.cpu_max,
    })
}

/// Risposta a `query-name`.
pub(crate) fn parse_name(v: Value) -> Result<Option<String>, QmpError> {
    #[derive(Deserialize)]
    struct W {
        #[serde(default)]
        name: Option<String>,
    }
    let w: W = from_value("query-name", v)?;
    Ok(w.name)
}

/// Dispositivo PCI di classe rete trovato da `query-pci`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PciNetworkDevice {
    /// Bus.
    pub bus: u32,
    /// Slot.
    pub slot: u32,
    /// Descrizione QEMU.
    pub description: String,
}

/// Cerca dispositivi di classe 0x02 (controller di rete) nella risposta di
/// `query-pci`, compresi quelli dietro bridge.
pub(crate) fn pci_network_devices(v: &Value) -> Result<Vec<PciNetworkDevice>, QmpError> {
    let buses = v
        .as_array()
        .ok_or_else(|| QmpError::Protocol("query-pci: attesa una lista".into()))?;
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
        .ok_or_else(|| QmpError::Protocol("query-pci: `devices` non è una lista".into()))?;
    for d in devices {
        let class = d
            .get("class_info")
            .and_then(|c| c.get("class"))
            .and_then(Value::as_u64)
            .ok_or_else(|| QmpError::Protocol("query-pci: classe assente".into()))?;
        if class >> 8 == 0x02 {
            out.push(PciNetworkDevice {
                bus: d.get("bus").and_then(Value::as_u64).unwrap_or(0) as u32,
                slot: d.get("slot").and_then(Value::as_u64).unwrap_or(0) as u32,
                description: d
                    .get("class_info")
                    .and_then(|c| c.get("desc"))
                    .and_then(Value::as_str)
                    .unwrap_or("rete")
                    .to_owned(),
            });
        }
        if let Some(bridge) = d.get("pci_bridge") {
            collect_pci(bridge.get("devices"), out)?;
        }
    }
    Ok(())
}

/// Evento QMP asincrono.
#[derive(Clone, Debug, PartialEq)]
pub struct QmpEvent {
    /// Nome dell'evento (per esempio `SHUTDOWN`).
    pub name: String,
    /// Dati dell'evento.
    pub data: Value,
    /// Marca temporale di QEMU in microsecondi dall'epoca Unix.
    pub timestamp_us: u64,
}

impl QmpEvent {
    /// Interpretazione tipizzata degli eventi usati dal backend.
    pub fn kind(&self) -> EventKind {
        match self.name.as_str() {
            "SHUTDOWN" => EventKind::Shutdown {
                guest: self.data.get("guest").and_then(Value::as_bool).unwrap_or(false),
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

/// Eventi riconosciuti.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// Arresto (`guest` = richiesto dal guest; `reason` QAPI `ShutdownCause`).
    Shutdown {
        /// Richiesto dal guest.
        guest: bool,
        /// Causa QAPI.
        reason: String,
    },
    /// Richiesta ACPI di spegnimento consegnata.
    Powerdown,
    /// Reset.
    Reset,
    /// vCPU fermate.
    Stop,
    /// vCPU riprese.
    Resume,
    /// Panico del guest.
    GuestPanicked,
    /// Altro evento.
    Other,
}

/// Messaggio in arrivo dal server.
#[derive(Debug)]
pub(crate) enum Incoming {
    Return { id: Option<u64>, value: Value },
    Error { id: Option<u64>, class: String, desc: String },
    Event(QmpEvent),
}

pub(crate) fn classify(v: Value) -> Result<Incoming, QmpError> {
    let id = v.get("id").and_then(Value::as_u64);
    if let Some(name) = v.get("event").and_then(Value::as_str) {
        let ts = v.get("timestamp");
        let secs = ts.and_then(|t| t.get("seconds")).and_then(Value::as_u64).unwrap_or(0);
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
            class: e.get("class").and_then(Value::as_str).unwrap_or("?").to_owned(),
            desc: e.get("desc").and_then(Value::as_str).unwrap_or("").to_owned(),
        });
    }
    Err(QmpError::Protocol(format!("messaggio non riconosciuto: {v}")))
}

fn from_value<T: serde::de::DeserializeOwned>(cmd: &str, v: Value) -> Result<T, QmpError> {
    serde_json::from_value(v).map_err(|e| QmpError::Protocol(format!("risposta a {cmd}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Greeting reale di QEMU 11.1.0 su socket Unix (F0-02 4.3, F0-04).
    const GREETING: &str = include_str!("../../tests/fixtures/qmp-greeting.json");

    #[test]
    fn greeting_reale() {
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
    fn stato_e_acceleratori() {
        let s = parse_status(json!({"status": "prelaunch", "running": false})).unwrap();
        assert_eq!(s.status, RunState::Prelaunch);
        let a = parse_accelerators(json!({"enabled": "whpx", "present": ["qtest","tcg","whpx"]}))
            .unwrap();
        assert_eq!(a.enabled, "whpx");
        assert!(parse_status(json!({"status": 3})).is_err());
    }

    #[test]
    fn classificazione() {
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
    fn pci_senza_e_con_nic() {
        let no_nic: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/qmp-query-pci-nodefaults.json"))
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
    fn macchina_corrente() {
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
