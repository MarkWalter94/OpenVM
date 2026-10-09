//! Client QMP minimo su socket Unix.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

/// Valore massimo degli assi assoluti di QEMU (`INPUT_EVENT_ABS_MAX`).
pub const ABS_MAX: u32 = 0x7fff;

/// Errore QMP.
#[derive(Debug)]
pub enum QmpError {
    /// I/O.
    Io(std::io::Error),
    /// JSON non valido o risposta inattesa.
    Protocol(String),
    /// Errore restituito da QEMU (`class`, `desc`).
    Command(String),
}

impl std::fmt::Display for QmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O QMP: {e}"),
            Self::Protocol(e) => write!(f, "protocollo QMP: {e}"),
            Self::Command(e) => write!(f, "errore QEMU: {e}"),
        }
    }
}

impl std::error::Error for QmpError {}

impl From<std::io::Error> for QmpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Pulsanti del puntatore (nomi QAPI `InputButton`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Sinistro.
    Left,
    /// Centrale.
    Middle,
    /// Destro.
    Right,
    /// Rotella su.
    WheelUp,
    /// Rotella giù.
    WheelDown,
}

impl Button {
    fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Middle => "middle",
            Self::Right => "right",
            Self::WheelUp => "wheel-up",
            Self::WheelDown => "wheel-down",
        }
    }
}

/// Evento di input tipizzato per `input-send-event`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Posizione assoluta già scalata in 0..=32767.
    Abs {
        /// Asse x scalato.
        x: u32,
        /// Asse y scalato.
        y: u32,
    },
    /// Pulsante premuto o rilasciato.
    Btn {
        /// Pulsante.
        button: Button,
        /// Premuto.
        down: bool,
    },
    /// Tasto premuto o rilasciato, per qcode QEMU.
    Key {
        /// Nome qcode (`QKeyCode`).
        qcode: &'static str,
        /// Premuto.
        down: bool,
    },
}

impl InputEvent {
    fn to_json(&self) -> Vec<Value> {
        match self {
            Self::Abs { x, y } => vec![
                json!({"type": "abs", "data": {"axis": "x", "value": x}}),
                json!({"type": "abs", "data": {"axis": "y", "value": y}}),
            ],
            Self::Btn { button, down } => {
                vec![json!({"type": "btn", "data": {"down": down, "button": button.name()}})]
            }
            Self::Key { qcode, down } => vec![
                json!({"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}),
            ],
        }
    }
}

/// Scala una coordinata in pixel (0..size-1) sull'intervallo 0..=32767,
/// con la stessa formula del server VNC di QEMU (`qemu_input_scale_axis`).
pub fn scale_abs(px: u32, size: u32) -> u32 {
    if size <= 1 {
        return 0;
    }
    let px = px.min(size - 1);
    (u64::from(px) * u64::from(ABS_MAX) / u64::from(size - 1)) as u32
}

/// Client QMP collegato a un socket Unix.
pub struct Qmp {
    reader: BufReader<socket2::Socket>,
    writer: socket2::Socket,
    /// Greeting ricevuto alla connessione.
    pub greeting: Value,
    /// Eventi asincroni ricevuti e non ancora letti.
    pub events: Vec<Value>,
}

impl std::fmt::Debug for Qmp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Qmp")
            .field("greeting", &self.greeting)
            .finish_non_exhaustive()
    }
}

impl Qmp {
    /// Si collega, legge il greeting e negozia `qmp_capabilities`.
    pub fn connect(path: &Path) -> Result<Self, QmpError> {
        let s = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
        s.connect(&socket2::SockAddr::unix(path)?)?;
        s.set_read_timeout(Some(Duration::from_secs(30)))?;
        let writer = s.try_clone()?;
        let mut q = Self {
            reader: BufReader::new(s),
            writer,
            greeting: Value::Null,
            events: Vec::new(),
        };
        q.greeting = q.read_json()?;
        if q.greeting.get("QMP").is_none() {
            return Err(QmpError::Protocol(format!(
                "greeting inatteso: {}",
                q.greeting
            )));
        }
        q.execute_raw("qmp_capabilities", None)?;
        Ok(q)
    }

    fn read_json(&mut self) -> Result<Value, QmpError> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                return Err(QmpError::Protocol("connessione QMP chiusa".into()));
            }
            if line.len() > 16 * 1024 * 1024 {
                return Err(QmpError::Protocol("riga QMP troppo lunga".into()));
            }
            if !line.trim().is_empty() {
                return serde_json::from_str(&line).map_err(|e| QmpError::Protocol(e.to_string()));
            }
        }
    }

    /// Esegue un comando e restituisce il campo `return`. Privato: la UI usa
    /// soltanto i metodi tipizzati.
    fn execute_raw(&mut self, cmd: &str, args: Option<Value>) -> Result<Value, QmpError> {
        let mut msg = json!({ "execute": cmd });
        if let Some(a) = args {
            msg["arguments"] = a;
        }
        let mut line = msg.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        loop {
            let v = self.read_json()?;
            if v.get("event").is_some() {
                self.events.push(v);
                continue;
            }
            if let Some(r) = v.get("return") {
                return Ok(r.clone());
            }
            if let Some(e) = v.get("error") {
                return Err(QmpError::Command(e.to_string()));
            }
            return Err(QmpError::Protocol(format!("risposta inattesa: {v}")));
        }
    }

    /// `input-send-event` con una sequenza di eventi in un'unica chiamata.
    pub fn send_input(&mut self, events: &[InputEvent]) -> Result<(), QmpError> {
        if events.is_empty() {
            return Ok(());
        }
        let list: Vec<Value> = events.iter().flat_map(InputEvent::to_json).collect();
        self.execute_raw("input-send-event", Some(json!({ "events": list })))?;
        Ok(())
    }

    /// `screendump` in formato PPM verso un file scelto dal chiamante (nello
    /// spike: directory fissa passata da riga di comando, mai dalla UI).
    pub fn screendump_ppm(&mut self, path: &Path) -> Result<(), QmpError> {
        let p = path
            .to_str()
            .ok_or_else(|| QmpError::Protocol("percorso non UTF-8".into()))?
            .replace('\\', "/");
        self.execute_raw(
            "screendump",
            Some(json!({ "filename": p, "format": "ppm" })),
        )?;
        Ok(())
    }

    /// `query-status`: restituisce lo stato (`running`, `paused`, ...).
    pub fn query_status(&mut self) -> Result<String, QmpError> {
        let r = self.execute_raw("query-status", None)?;
        Ok(r.get("status")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned())
    }

    /// `query-mice`: dispositivi di puntamento e quello corrente.
    pub fn query_mice(&mut self) -> Result<Value, QmpError> {
        self.execute_raw("query-mice", None)
    }

    /// `query-accelerators` (verifica WHPX senza TCG).
    pub fn query_accelerators(&mut self) -> Result<Value, QmpError> {
        self.execute_raw("query-accelerators", None)
    }

    /// `quit`: chiude QEMU.
    pub fn quit(&mut self) -> Result<(), QmpError> {
        self.execute_raw("quit", None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scala_assi_come_qemu() {
        assert_eq!(scale_abs(0, 1280), 0);
        assert_eq!(scale_abs(1279, 1280), 32767);
        assert_eq!(scale_abs(5000, 1280), 32767);
        assert_eq!(scale_abs(640, 1281), 16383);
        assert_eq!(scale_abs(10, 1), 0);
    }

    #[test]
    fn json_degli_eventi() {
        let e = InputEvent::Key {
            qcode: "a",
            down: true,
        }
        .to_json();
        assert_eq!(e[0]["data"]["key"]["data"], "a");
        let b = InputEvent::Btn {
            button: Button::WheelUp,
            down: false,
        }
        .to_json();
        assert_eq!(b[0]["data"]["button"], "wheel-up");
        let a = InputEvent::Abs { x: 1, y: 2 }.to_json();
        assert_eq!(a.len(), 2);
        assert_eq!(a[1]["data"]["axis"], "y");
    }
}
