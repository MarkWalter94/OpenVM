//! Client QMP minimo su socket Unix (AF_UNIX) per Windows.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uds_windows::UnixStream;

/// Connessione QMP sincrona con raccolta degli eventi.
#[derive(Debug)]
pub struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// Eventi ricevuti con l'istante di ricezione.
    pub events: Vec<(Instant, Value)>,
}

impl Qmp {
    /// Si collega ritentando fino a `timeout` (QEMU crea il socket dopo l'avvio)
    /// ed esegue la negoziazione `qmp_capabilities`.
    pub fn connect(path: &Path, timeout: Duration) -> io::Result<Qmp> {
        let start = Instant::now();
        let stream = loop {
            match UnixStream::connect(path) {
                Ok(s) => break s,
                Err(e) if start.elapsed() < timeout => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let writer = stream.try_clone()?;
        let mut q = Qmp {
            reader: BufReader::new(stream),
            writer,
            events: Vec::new(),
        };
        let greeting = q.read_value()?;
        if greeting.get("QMP").is_none() {
            return Err(io::Error::other(format!("greeting inatteso: {greeting}")));
        }
        q.execute("qmp_capabilities", None)?;
        Ok(q)
    }

    fn read_value(&mut self) -> io::Result<Value> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "QMP chiuso"));
        }
        serde_json::from_str(&line).map_err(io::Error::other)
    }

    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.reader.get_ref().set_read_timeout(Some(t))
    }

    /// Esegue un comando e restituisce `return` (o errore con `error`).
    pub fn execute(&mut self, cmd: &str, args: Option<Value>) -> io::Result<Value> {
        let msg = match args {
            Some(a) => json!({"execute": cmd, "arguments": a}),
            None => json!({"execute": cmd}),
        };
        let mut s = msg.to_string();
        s.push('\n');
        self.writer.write_all(s.as_bytes())?;
        loop {
            let v = self.read_value()?;
            if v.get("event").is_some() {
                self.events.push((Instant::now(), v));
                continue;
            }
            if let Some(r) = v.get("return") {
                return Ok(r.clone());
            }
            if let Some(e) = v.get("error") {
                return Err(io::Error::other(format!("errore QMP {cmd}: {e}")));
            }
        }
    }

    /// Attende un evento con nome dato fino a `timeout`.
    pub fn wait_event(&mut self, name: &str, timeout: Duration) -> io::Result<Option<Value>> {
        if let Some((_, v)) = self.events.iter().find(|(_, v)| v["event"] == name) {
            return Ok(Some(v.clone()));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            self.set_timeout((deadline - now).max(Duration::from_millis(1)))?;
            match self.read_value() {
                Ok(v) => {
                    if v.get("event").is_some() {
                        let hit = v["event"] == name;
                        self.events.push((Instant::now(), v.clone()));
                        if hit {
                            return Ok(Some(v));
                        }
                    }
                }
                Err(e)
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Invia un tasto (o combinazione) con `send-key`.
    pub fn send_keys(&mut self, qcodes: &[&str]) -> io::Result<()> {
        let keys: Vec<Value> = qcodes
            .iter()
            .map(|k| json!({"type": "qcode", "data": k}))
            .collect();
        self.execute("send-key", Some(json!({"keys": keys, "hold-time": 40})))?;
        Ok(())
    }

    /// Digita testo ASCII con layout US tramite `send-key`.
    pub fn type_text(&mut self, text: &str) -> io::Result<()> {
        for c in text.chars() {
            let (shift, code) = qcode_for(c)
                .ok_or_else(|| io::Error::other(format!("carattere non mappato: {c:?}")))?;
            if shift {
                self.send_keys(&["shift", code])?;
            } else {
                self.send_keys(&[code])?;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }
}

/// Mappa carattere → (shift, qcode) per layout US.
pub fn qcode_for(c: char) -> Option<(bool, &'static str)> {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    Some(match c {
        'a'..='z' => (false, LETTERS[(c as u8 - b'a') as usize]),
        'A'..='Z' => (true, LETTERS[(c as u8 - b'A') as usize]),
        '0'..='9' => (false, DIGITS[(c as u8 - b'0') as usize]),
        ' ' => (false, "spc"),
        '-' => (false, "minus"),
        '_' => (true, "minus"),
        '=' => (false, "equal"),
        '/' => (false, "slash"),
        '.' => (false, "dot"),
        ',' => (false, "comma"),
        ';' => (false, "semicolon"),
        '|' => (true, "backslash"),
        '>' => (true, "dot"),
        '&' => (true, "7"),
        '\n' => (false, "ret"),
        _ => return None,
    })
}
