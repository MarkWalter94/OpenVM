//! Input tipizzato verso il guest (ROADMAP 6.4; F0-04: QMP `input-send-event`
//! con eventi `key` qcode, `abs` 0–32767 e `btn`; tablet virtio assoluto).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DisplayGeometry, PublicError, QCode, RunId, VmId};

/// Massimo valore di un asse assoluto (come `INPUT_EVENT_ABS_MAX` di QEMU).
pub const ABS_AXIS_MAX: u16 = 32767;

/// Numero massimo di eventi in una singola richiesta di input.
pub const MAX_INPUT_EVENTS_PER_REQUEST: usize = 256;

/// Pressione o rilascio di un tasto.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct KeyAction {
    /// Tasto QEMU.
    pub qcode: QCode,
    /// `true` = premuto, `false` = rilasciato.
    pub down: bool,
}

/// Movimento assoluto del puntatore in coordinate QEMU 0..=32767 su ciascun asse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PointerMove {
    /// Asse X (0..=32767).
    #[schemars(range(max = 32767))]
    pub x: u16,
    /// Asse Y (0..=32767).
    #[schemars(range(max = 32767))]
    pub y: u16,
}

impl PointerMove {
    /// Costruzione validata.
    pub fn new(x: u16, y: u16) -> Result<Self, PublicError> {
        let p = Self { x, y };
        p.validate()?;
        Ok(p)
    }

    /// Verifica l'intervallo (utile dopo la deserializzazione).
    pub fn validate(&self) -> Result<(), PublicError> {
        if self.x <= ABS_AXIS_MAX && self.y <= ABS_AXIS_MAX {
            Ok(())
        } else {
            Err(PublicError::invalid_argument(
                "coordinate assolute fuori da 0..=32767",
            ))
        }
    }

    /// Converte pixel del display in coordinate assolute con la formula di QEMU
    /// (`qemu_input_scale_axis`): `px * 32767 / (size - 1)`. Rifiuta pixel fuori
    /// dalla geometria dichiarata.
    pub fn from_pixels(px: u32, py: u32, geometry: DisplayGeometry) -> Result<Self, PublicError> {
        geometry.validate()?;
        if px >= geometry.width || py >= geometry.height {
            return Err(PublicError::invalid_argument(
                "pixel fuori dalla geometria del display",
            ));
        }
        let scale = |p: u32, size: u32| -> u16 {
            if size <= 1 {
                return 0;
            }
            let v = u64::from(p) * u64::from(ABS_AXIS_MAX) / u64::from(size - 1);
            u16::try_from(v.min(u64::from(ABS_AXIS_MAX))).unwrap_or(ABS_AXIS_MAX)
        };
        Ok(Self {
            x: scale(px, geometry.width),
            y: scale(py, geometry.height),
        })
    }
}

/// Pulsante del puntatore (nomi QMP `InputButton` ammessi).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
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

/// Pressione o rilascio di un pulsante del puntatore.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PointerButton {
    /// Pulsante.
    pub button: MouseButton,
    /// `true` = premuto, `false` = rilasciato.
    pub down: bool,
}

/// Evento di input elementare.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    /// Tasto.
    Key(KeyAction),
    /// Movimento assoluto.
    PointerMove(PointerMove),
    /// Pulsante.
    PointerButton(PointerButton),
}

/// Lotto di eventi di input riferito a un'esecuzione e a una geometria: il core
/// rifiuta lotti riferiti a un `run_id` superato o a una geometria non corrente
/// (ROADMAP 6.4). Gli input non vengono mai ripetuti automaticamente.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InputBatch {
    /// VM destinataria.
    pub vm_id: VmId,
    /// Esecuzione attesa.
    pub run_id: RunId,
    /// Geometria su cui il chiamante ha calcolato le coordinate.
    pub geometry: DisplayGeometry,
    /// Eventi, al massimo [`MAX_INPUT_EVENTS_PER_REQUEST`].
    pub events: Vec<InputEvent>,
}

impl InputBatch {
    /// Validazione strutturale (numero di eventi, intervalli, geometria).
    pub fn validate(&self) -> Result<(), PublicError> {
        self.geometry.validate()?;
        if self.events.is_empty() {
            return Err(PublicError::invalid_argument("lotto di input vuoto"));
        }
        if self.events.len() > MAX_INPUT_EVENTS_PER_REQUEST {
            return Err(PublicError::resource_limit(
                "troppi eventi di input in una richiesta",
            ));
        }
        for e in &self.events {
            if let InputEvent::PointerMove(p) = e {
                p.validate()?;
            }
        }
        Ok(())
    }
}
