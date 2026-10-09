//! Tipi condivisi definiti localmente in attesa di `lightvm-proto`.
//!
//! TODO(F1): `crates/lightvm-proto` (in definizione da un altro flusso) non
//! esportava ancora `RunId`, `DisplayGeometry`, input tipizzato e `Screenshot`
//! quando questo modulo è stato scritto. Appena disponibili, questi tipi vanno
//! sostituiti da quelli del proto (o convertiti con `From`), lasciando qui
//! soltanto ciò che è interno al backend (per esempio l'allowlist dei qcode).

use std::fmt;
use std::time::SystemTime;

use crate::error::{BackendError, Result};
use crate::validate::{TOKEN_MAX_LEN, check_token};

/// Identificativo di un'esecuzione (`run_id`, ROADMAP 6.3): cambia a ogni avvio.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RunId(String);

impl RunId {
    /// Valida un identificativo: alfabeto `[A-Za-z0-9._-]`, al massimo
    /// [`TOKEN_MAX_LEN`] caratteri, non inizia con `-`.
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        check_token("run_id", &s)?;
        Ok(Self(s))
    }

    /// Testo dell'identificativo.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identificativo di un supporto del registro media approvato (ISO di
/// installazione o supporto dati). Il backend riceve ID, mai percorsi: il
/// percorso lo restituisce [`crate::ManagedPaths::media`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MediaId(String);

impl MediaId {
    /// Valida un identificativo con le stesse regole di [`RunId`].
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        check_token("media_id", &s)?;
        Ok(Self(s))
    }

    /// Testo dell'identificativo.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Geometria del display del guest in pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DisplayGeometry {
    /// Larghezza.
    pub width: u32,
    /// Altezza.
    pub height: u32,
}

/// Screenshot prodotto dal backend (PNG in memoria).
#[derive(Clone, Debug)]
pub struct Screenshot {
    /// Esecuzione a cui appartiene.
    pub run_id: RunId,
    /// Geometria del display al momento della cattura.
    pub geometry: DisplayGeometry,
    /// Immagine PNG RGB a 8 bit.
    pub png: Vec<u8>,
    /// Istante della cattura.
    pub captured_at: SystemTime,
}

/// Pulsanti del puntatore (nomi QAPI `InputButton`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputButton {
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

impl InputButton {
    pub(crate) fn qapi_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Middle => "middle",
            Self::Right => "right",
            Self::WheelUp => "wheel-up",
            Self::WheelDown => "wheel-down",
        }
    }
}

/// Codici tasto QEMU ammessi (`QKeyCode`, QAPI `ui.json`).
///
/// Esclusi di proposito: `unmapped` e i tasti di alimentazione `power`,
/// `sleep`, `wake` (il controllo dell'alimentazione ha comandi dedicati e non
/// passa dall'input).
const QKEYCODES: &[&str] = &[
    "shift", "shift_r", "alt", "alt_r", "ctrl", "ctrl_r", "menu", "esc", "1", "2", "3", "4", "5",
    "6", "7", "8", "9", "0", "minus", "equal", "backspace", "tab", "q", "w", "e", "r", "t", "y",
    "u", "i", "o", "p", "bracket_left", "bracket_right", "ret", "a", "s", "d", "f", "g", "h",
    "j", "k", "l", "semicolon", "apostrophe", "grave_accent", "backslash", "z", "x", "c", "v",
    "b", "n", "m", "comma", "dot", "slash", "asterisk", "spc", "caps_lock", "f1", "f2", "f3",
    "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "num_lock", "scroll_lock",
    "kp_divide", "kp_multiply", "kp_subtract", "kp_add", "kp_enter", "kp_decimal", "sysrq",
    "kp_0", "kp_1", "kp_2", "kp_3", "kp_4", "kp_5", "kp_6", "kp_7", "kp_8", "kp_9", "less",
    "home", "pgup", "pgdn", "end", "left", "up", "down", "right", "insert", "delete", "stop",
    "again", "props", "undo", "front", "copy", "open", "paste", "find", "cut", "lf", "help",
    "meta_l", "meta_r", "compose", "pause", "ro", "hiragana", "henkan", "yen", "muhenkan",
    "katakanahiragana", "kp_comma", "kp_equals", "audionext", "audioprev", "audiostop",
    "audioplay", "audiomute", "volumeup", "volumedown", "mediaselect", "mail", "calculator",
    "computer", "ac_home", "ac_back", "ac_forward", "ac_refresh", "ac_bookmarks", "lang1",
    "lang2", "f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24",
];

/// Codice tasto QEMU appartenente all'allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QKeyCode(&'static str);

impl QKeyCode {
    /// Restituisce il qcode se appartiene all'allowlist.
    pub fn from_name(name: &str) -> Option<Self> {
        QKEYCODES.iter().find(|&&k| k == name).map(|&k| Self(k))
    }

    /// Nome QAPI.
    pub fn name(self) -> &'static str {
        self.0
    }

    /// Tutti i qcode ammessi.
    pub fn all() -> impl Iterator<Item = Self> {
        QKEYCODES.iter().map(|&k| Self(k))
    }
}

/// Evento di input tipizzato (inoltrato con QMP `input-send-event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Tasto premuto o rilasciato.
    Key {
        /// Tasto.
        key: QKeyCode,
        /// `true` se premuto.
        down: bool,
    },
    /// Puntatore assoluto in pixel, riferito alla geometria dichiarata.
    PointerAbs {
        /// Coordinata x (0..width-1).
        x: u32,
        /// Coordinata y (0..height-1).
        y: u32,
    },
    /// Pulsante premuto o rilasciato.
    Button {
        /// Pulsante.
        button: InputButton,
        /// `true` se premuto.
        down: bool,
    },
}

/// Numero massimo di eventi in una singola richiesta di input.
pub const MAX_INPUT_EVENTS: usize = 256;

pub(crate) fn validate_input(geometry: DisplayGeometry, events: &[InputEvent]) -> Result<()> {
    if events.len() > MAX_INPUT_EVENTS {
        return Err(BackendError::ResourceLimit(format!(
            "{} eventi di input in una richiesta (massimo {MAX_INPUT_EVENTS})",
            events.len()
        )));
    }
    if geometry.width == 0 || geometry.height == 0 {
        return Err(BackendError::invalid("geometry", "geometria nulla"));
    }
    for e in events {
        if let InputEvent::PointerAbs { x, y } = *e
            && (x >= geometry.width || y >= geometry.height)
        {
            return Err(BackendError::invalid(
                "pointer",
                format!(
                    "coordinate {x},{y} fuori dalla geometria {}x{}",
                    geometry.width, geometry.height
                ),
            ));
        }
    }
    Ok(())
}

const _: () = assert!(TOKEN_MAX_LEN >= 36, "un UUID testuale deve essere un token valido");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qcode_allowlist() {
        assert!(QKeyCode::from_name("a").is_some());
        assert!(QKeyCode::from_name("ctrl_r").is_some());
        assert!(QKeyCode::from_name("power").is_none());
        assert!(QKeyCode::from_name("unmapped").is_none());
        assert!(QKeyCode::from_name("a\"}").is_none());
        let n = QKeyCode::all().count();
        let mut v: Vec<_> = QKeyCode::all().map(QKeyCode::name).collect();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), n, "qcode duplicati");
    }

    #[test]
    fn input_fuori_geometria() {
        let g = DisplayGeometry {
            width: 10,
            height: 5,
        };
        assert!(validate_input(g, &[InputEvent::PointerAbs { x: 9, y: 4 }]).is_ok());
        assert!(validate_input(g, &[InputEvent::PointerAbs { x: 10, y: 0 }]).is_err());
        let many = vec![InputEvent::PointerAbs { x: 0, y: 0 }; MAX_INPUT_EVENTS + 1];
        assert!(validate_input(g, &many).is_err());
    }

    #[test]
    fn identificativi() {
        assert!(RunId::new("7f1c2a9e-0000-4000-8000-000000000001").is_ok());
        assert!(RunId::new("a,b").is_err());
        assert!(MediaId::new("-x").is_err());
    }
}
