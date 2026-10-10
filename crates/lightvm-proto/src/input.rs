//! Typed input towards the guest (ROADMAP 6.4; F0-04: QMP `input-send-event`
//! with `key` qcode, `abs` 0–32767 and `btn` events; absolute virtio tablet).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DisplayGeometry, PublicError, QCode, RunId, VmId};

/// Maximum value of an absolute axis (like QEMU's `INPUT_EVENT_ABS_MAX`).
pub const ABS_AXIS_MAX: u16 = 32767;

/// Maximum number of events in a single input request.
pub const MAX_INPUT_EVENTS_PER_REQUEST: usize = 256;

/// Press or release of a key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct KeyAction {
    /// QEMU key.
    pub qcode: QCode,
    /// `true` = pressed, `false` = released.
    pub down: bool,
}

/// Absolute pointer movement in QEMU coordinates 0..=32767 on each axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PointerMove {
    /// X axis (0..=32767).
    #[schemars(range(max = 32767))]
    pub x: u16,
    /// Y axis (0..=32767).
    #[schemars(range(max = 32767))]
    pub y: u16,
}

impl PointerMove {
    /// Validated construction.
    pub fn new(x: u16, y: u16) -> Result<Self, PublicError> {
        let p = Self { x, y };
        p.validate()?;
        Ok(p)
    }

    /// Checks the range (useful after deserialization).
    pub fn validate(&self) -> Result<(), PublicError> {
        if self.x <= ABS_AXIS_MAX && self.y <= ABS_AXIS_MAX {
            Ok(())
        } else {
            Err(PublicError::invalid_argument(
                "absolute coordinates outside 0..=32767",
            ))
        }
    }

    /// Converts display pixels to absolute coordinates with QEMU's formula
    /// (`qemu_input_scale_axis`): `px * 32767 / (size - 1)`. Rejects pixels
    /// outside the declared geometry.
    pub fn from_pixels(px: u32, py: u32, geometry: DisplayGeometry) -> Result<Self, PublicError> {
        geometry.validate()?;
        if px >= geometry.width || py >= geometry.height {
            return Err(PublicError::invalid_argument(
                "pixel outside the display geometry",
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

/// Pointer button (allowed QMP `InputButton` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
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

/// Press or release of a pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PointerButton {
    /// Button.
    pub button: MouseButton,
    /// `true` = pressed, `false` = released.
    pub down: bool,
}

/// Elementary input event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    /// Key.
    Key(KeyAction),
    /// Absolute movement.
    PointerMove(PointerMove),
    /// Button.
    PointerButton(PointerButton),
}

/// Batch of input events tied to a run and a geometry: the core
/// rejects batches tied to a superseded `run_id` or a non-current geometry
/// (ROADMAP 6.4). Inputs are never repeated automatically.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InputBatch {
    /// Target VM.
    pub vm_id: VmId,
    /// Expected run.
    pub run_id: RunId,
    /// Geometry on which the caller computed the coordinates.
    pub geometry: DisplayGeometry,
    /// Events, at most [`MAX_INPUT_EVENTS_PER_REQUEST`].
    pub events: Vec<InputEvent>,
}

impl InputBatch {
    /// Structural validation (number of events, ranges, geometry).
    pub fn validate(&self) -> Result<(), PublicError> {
        self.geometry.validate()?;
        if self.events.is_empty() {
            return Err(PublicError::invalid_argument("empty input batch"));
        }
        if self.events.len() > MAX_INPUT_EVENTS_PER_REQUEST {
            return Err(PublicError::resource_limit(
                "too many input events in one request",
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
