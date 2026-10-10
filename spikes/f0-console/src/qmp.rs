//! Minimal QMP client over a Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

/// Maximum value of QEMU's absolute axes (`INPUT_EVENT_ABS_MAX`).
pub const ABS_MAX: u32 = 0x7fff;

/// QMP error.
#[derive(Debug)]
pub enum QmpError {
    /// I/O.
    Io(std::io::Error),
    /// Invalid JSON or unexpected response.
    Protocol(String),
    /// Error returned by QEMU (`class`, `desc`).
    Command(String),
}

impl std::fmt::Display for QmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O QMP: {e}"),
            Self::Protocol(e) => write!(f, "QMP protocol: {e}"),
            Self::Command(e) => write!(f, "QEMU error: {e}"),
        }
    }
}

impl std::error::Error for QmpError {}

impl From<std::io::Error> for QmpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Pointer buttons (QAPI `InputButton` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Left.
    Left,
    /// Middle.
    Middle,
    /// Right.
    Right,
    /// Wheel up.
    WheelUp,
    /// Wheel down.
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

/// Typed input event for `input-send-event`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Absolute position already scaled to 0..=32767.
    Abs {
        /// Scaled x axis.
        x: u32,
        /// Scaled y axis.
        y: u32,
    },
    /// Button pressed or released.
    Btn {
        /// Button.
        button: Button,
        /// Pressed.
        down: bool,
    },
    /// Key pressed or released, by QEMU qcode.
    Key {
        /// qcode name (`QKeyCode`).
        qcode: &'static str,
        /// Pressed.
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

/// Scales a pixel coordinate (0..size-1) to the range 0..=32767,
/// with the same formula as QEMU's VNC server (`qemu_input_scale_axis`).
pub fn scale_abs(px: u32, size: u32) -> u32 {
    if size <= 1 {
        return 0;
    }
    let px = px.min(size - 1);
    (u64::from(px) * u64::from(ABS_MAX) / u64::from(size - 1)) as u32
}

/// QMP client connected to a Unix socket.
pub struct Qmp {
    reader: BufReader<socket2::Socket>,
    writer: socket2::Socket,
    /// Greeting received on connection.
    pub greeting: Value,
    /// Asynchronous events received and not yet read.
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
    /// Connects, reads the greeting and negotiates `qmp_capabilities`.
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
                "unexpected greeting: {}",
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
                return Err(QmpError::Protocol("QMP connection closed".into()));
            }
            if line.len() > 16 * 1024 * 1024 {
                return Err(QmpError::Protocol("QMP line too long".into()));
            }
            if !line.trim().is_empty() {
                return serde_json::from_str(&line).map_err(|e| QmpError::Protocol(e.to_string()));
            }
        }
    }

    /// Executes a command and returns the `return` field. Private: the UI uses
    /// only the typed methods.
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
            return Err(QmpError::Protocol(format!("unexpected response: {v}")));
        }
    }

    /// `input-send-event` with a sequence of events in a single call.
    pub fn send_input(&mut self, events: &[InputEvent]) -> Result<(), QmpError> {
        if events.is_empty() {
            return Ok(());
        }
        let list: Vec<Value> = events.iter().flat_map(InputEvent::to_json).collect();
        self.execute_raw("input-send-event", Some(json!({ "events": list })))?;
        Ok(())
    }

    /// `screendump` in PPM format to a file chosen by the caller (in the
    /// spike: fixed directory passed on the command line, never from the UI).
    pub fn screendump_ppm(&mut self, path: &Path) -> Result<(), QmpError> {
        let p = path
            .to_str()
            .ok_or_else(|| QmpError::Protocol("non-UTF-8 path".into()))?
            .replace('\\', "/");
        self.execute_raw(
            "screendump",
            Some(json!({ "filename": p, "format": "ppm" })),
        )?;
        Ok(())
    }

    /// `query-status`: returns the status (`running`, `paused`, ...).
    pub fn query_status(&mut self) -> Result<String, QmpError> {
        let r = self.execute_raw("query-status", None)?;
        Ok(r.get("status")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned())
    }

    /// `query-mice`: pointing devices and the current one.
    pub fn query_mice(&mut self) -> Result<Value, QmpError> {
        self.execute_raw("query-mice", None)
    }

    /// `query-accelerators` (checks WHPX without TCG).
    pub fn query_accelerators(&mut self) -> Result<Value, QmpError> {
        self.execute_raw("query-accelerators", None)
    }

    /// `quit`: shuts QEMU down.
    pub fn quit(&mut self) -> Result<(), QmpError> {
        self.execute_raw("quit", None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_axes_like_qemu() {
        assert_eq!(scale_abs(0, 1280), 0);
        assert_eq!(scale_abs(1279, 1280), 32767);
        assert_eq!(scale_abs(5000, 1280), 32767);
        assert_eq!(scale_abs(640, 1281), 16383);
        assert_eq!(scale_abs(10, 1), 0);
    }

    #[test]
    fn event_json() {
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
