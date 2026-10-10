//! Typed input to the guest (ROADMAP 6.4; F0-04 §4).
//!
//! The types are those of `lightvm-proto` ([`InputEvent`], [`KeyAction`],
//! [`lightvm_proto::PointerMove`] in absolute coordinates 0..=32767,
//! [`PointerButton`]). Only the backend-specific parts remain here:
//!
//! - the allowlist of accepted QEMU qcodes (the proto only validates the grammar
//!   `[a-z0-9_]{1,32}`);
//! - validation of a batch against the current geometry;
//! - translation to JSON for QMP `input-send-event`;
//! - the register of pressed keys and buttons, for explicit release on
//!   focus loss, disconnection or change of input owner
//!   (ROADMAP 6.4).

use std::collections::BTreeSet;

use lightvm_proto::{
    ABS_AXIS_MAX, DisplayGeometry, InputEvent, KeyAction, MAX_INPUT_EVENTS_PER_REQUEST,
    MouseButton, PointerButton, QCode,
};
use serde_json::{Value, json};

use crate::error::{BackendError, Result};

/// Allowed QEMU key codes (`QKeyCode`, QAPI `ui.json`).
///
/// Deliberately excluded: `unmapped` and the power keys `power`,
/// `sleep`, `wake` (power control has dedicated commands and does not
/// go through input).
const QKEYCODES: &[&str] = &[
    "shift",
    "shift_r",
    "alt",
    "alt_r",
    "ctrl",
    "ctrl_r",
    "menu",
    "esc",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "0",
    "minus",
    "equal",
    "backspace",
    "tab",
    "q",
    "w",
    "e",
    "r",
    "t",
    "y",
    "u",
    "i",
    "o",
    "p",
    "bracket_left",
    "bracket_right",
    "ret",
    "a",
    "s",
    "d",
    "f",
    "g",
    "h",
    "j",
    "k",
    "l",
    "semicolon",
    "apostrophe",
    "grave_accent",
    "backslash",
    "z",
    "x",
    "c",
    "v",
    "b",
    "n",
    "m",
    "comma",
    "dot",
    "slash",
    "asterisk",
    "spc",
    "caps_lock",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "num_lock",
    "scroll_lock",
    "kp_divide",
    "kp_multiply",
    "kp_subtract",
    "kp_add",
    "kp_enter",
    "kp_decimal",
    "sysrq",
    "kp_0",
    "kp_1",
    "kp_2",
    "kp_3",
    "kp_4",
    "kp_5",
    "kp_6",
    "kp_7",
    "kp_8",
    "kp_9",
    "less",
    "home",
    "pgup",
    "pgdn",
    "end",
    "left",
    "up",
    "down",
    "right",
    "insert",
    "delete",
    "stop",
    "again",
    "props",
    "undo",
    "front",
    "copy",
    "open",
    "paste",
    "find",
    "cut",
    "lf",
    "help",
    "meta_l",
    "meta_r",
    "compose",
    "pause",
    "ro",
    "hiragana",
    "henkan",
    "yen",
    "muhenkan",
    "katakanahiragana",
    "kp_comma",
    "kp_equals",
    "audionext",
    "audioprev",
    "audiostop",
    "audioplay",
    "audiomute",
    "volumeup",
    "volumedown",
    "mediaselect",
    "mail",
    "calculator",
    "computer",
    "ac_home",
    "ac_back",
    "ac_forward",
    "ac_refresh",
    "ac_bookmarks",
    "lang1",
    "lang2",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
    "f21",
    "f22",
    "f23",
    "f24",
];

/// `true` if the qcode belongs to the backend allowlist.
pub fn is_supported_qcode(qcode: &QCode) -> bool {
    QKEYCODES.contains(&qcode.as_str())
}

/// All qcodes allowed by the backend.
pub fn supported_qcodes() -> impl Iterator<Item = &'static str> {
    QKEYCODES.iter().copied()
}

/// QAPI name of a pointer button.
pub(crate) fn button_qapi_name(b: MouseButton) -> &'static str {
    match b {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
        MouseButton::WheelUp => "wheel-up",
        MouseButton::WheelDown => "wheel-down",
    }
}

/// Validates a batch of events against the declared geometry: number of
/// events, absolute coordinates within the QEMU range, qcodes in the allowlist.
pub fn validate_events(geometry: DisplayGeometry, events: &[InputEvent]) -> Result<()> {
    geometry.validate()?;
    if events.len() > MAX_INPUT_EVENTS_PER_REQUEST {
        return Err(BackendError::ResourceLimit(format!(
            "{} input events in one request (maximum {MAX_INPUT_EVENTS_PER_REQUEST})",
            events.len()
        )));
    }
    for e in events {
        match e {
            InputEvent::Key(k) => {
                if !is_supported_qcode(&k.qcode) {
                    return Err(BackendError::invalid(
                        "qcode",
                        format!("key `{}` not allowed", k.qcode),
                    ));
                }
            }
            InputEvent::PointerMove(p) => p.validate()?,
            InputEvent::PointerButton(_) => {}
        }
    }
    Ok(())
}

/// QMP `input-send-event` events (coordinates are already absolute).
pub(crate) fn events_json(events: &[InputEvent]) -> Vec<Value> {
    let mut list = Vec::with_capacity(events.len() * 2);
    for e in events {
        match e {
            InputEvent::PointerMove(p) => {
                let (x, y) = (p.x.min(ABS_AXIS_MAX), p.y.min(ABS_AXIS_MAX));
                list.push(json!({"type": "abs", "data": {"axis": "x", "value": x}}));
                list.push(json!({"type": "abs", "data": {"axis": "y", "value": y}}));
            }
            InputEvent::PointerButton(PointerButton { button, down }) => {
                list.push(json!({
                    "type": "btn",
                    "data": {"down": down, "button": button_qapi_name(*button)}
                }));
            }
            InputEvent::Key(KeyAction { qcode, down }) => {
                list.push(json!({
                    "type": "key",
                    "data": {"down": down, "key": {"type": "qcode", "data": qcode.as_str()}}
                }));
            }
        }
    }
    list
}

/// Keys and buttons pressed according to the forwarded events.
///
/// The register is updated **before** sending: if the QMP command times out, the
/// key might be pressed in the guest and must be released anyway. Releasing
/// a key that is not pressed is harmless.
#[derive(Debug, Default)]
pub(crate) struct PressedInput {
    keys: BTreeSet<String>,
    buttons: Vec<MouseButton>,
}

impl PressedInput {
    pub(crate) fn apply(&mut self, events: &[InputEvent]) {
        for e in events {
            match e {
                InputEvent::Key(KeyAction { qcode, down: true }) => {
                    self.keys.insert(qcode.as_str().to_owned());
                }
                InputEvent::Key(KeyAction { qcode, down: false }) => {
                    self.keys.remove(qcode.as_str());
                }
                InputEvent::PointerButton(PointerButton { button, down: true }) => {
                    if !self.buttons.contains(button) {
                        self.buttons.push(*button);
                    }
                }
                InputEvent::PointerButton(PointerButton {
                    button,
                    down: false,
                }) => self.buttons.retain(|b| b != button),
                InputEvent::PointerMove(_) => {}
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// Release events for everything that is pressed; empties the register.
    pub(crate) fn take_release_events(&mut self) -> Vec<InputEvent> {
        let mut out = Vec::with_capacity(self.keys.len() + self.buttons.len());
        for k in std::mem::take(&mut self.keys) {
            if let Ok(qcode) = QCode::parse(k) {
                out.push(InputEvent::Key(KeyAction { qcode, down: false }));
            }
        }
        for button in std::mem::take(&mut self.buttons) {
            out.push(InputEvent::PointerButton(PointerButton {
                button,
                down: false,
            }));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightvm_proto::PointerMove;

    fn key(name: &str, down: bool) -> InputEvent {
        InputEvent::Key(KeyAction {
            qcode: QCode::parse(name).unwrap(),
            down,
        })
    }

    #[test]
    fn qcode_allowlist() {
        assert!(is_supported_qcode(&QCode::parse("a").unwrap()));
        assert!(is_supported_qcode(&QCode::parse("ctrl_r").unwrap()));
        assert!(!is_supported_qcode(&QCode::parse("power").unwrap()));
        assert!(!is_supported_qcode(&QCode::parse("unmapped").unwrap()));
        assert!(QCode::parse("a\"}").is_err());
        let mut v: Vec<_> = supported_qcodes().collect();
        let n = v.len();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), n, "duplicate qcodes");
        for q in supported_qcodes() {
            assert!(QCode::parse(q).is_ok(), "{q} outside the proto grammar");
        }
    }

    #[test]
    fn batch_validation() {
        let g = DisplayGeometry::new(10, 5).unwrap();
        assert!(validate_events(g, &[key("a", true), key("a", false)]).is_ok());
        assert!(validate_events(g, &[key("power", true)]).is_err());
        let outside = InputEvent::PointerMove(PointerMove { x: 40000, y: 0 });
        assert!(validate_events(g, &[outside]).is_err());
        let many = vec![key("a", true); MAX_INPUT_EVENTS_PER_REQUEST + 1];
        assert!(matches!(
            validate_events(g, &many),
            Err(BackendError::ResourceLimit(_))
        ));
        let bad_geometry = DisplayGeometry {
            width: 0,
            height: 5,
        };
        assert!(validate_events(bad_geometry, &[]).is_err());
    }

    #[test]
    fn events_json_shape() {
        let g = DisplayGeometry::new(1280, 800).unwrap();
        let v = events_json(&[
            key("a", true),
            InputEvent::PointerButton(PointerButton {
                button: MouseButton::WheelUp,
                down: false,
            }),
            InputEvent::PointerMove(PointerMove::from_pixels(1279, 0, g).unwrap()),
        ]);
        assert_eq!(v[0]["data"]["key"]["data"], "a");
        assert_eq!(v[0]["data"]["key"]["type"], "qcode");
        assert_eq!(v[1]["data"]["button"], "wheel-up");
        assert_eq!(v[2]["data"]["value"], 32767);
        assert_eq!(v[3]["data"]["axis"], "y");
        assert_eq!(v[3]["data"]["value"], 0);
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn release_pressed_keys() {
        let mut p = PressedInput::default();
        p.apply(&[
            key("shift", true),
            key("a", true),
            key("a", false),
            InputEvent::PointerButton(PointerButton {
                button: MouseButton::Left,
                down: true,
            }),
        ]);
        assert!(!p.is_empty());
        let rel = p.take_release_events();
        assert_eq!(rel.len(), 2);
        assert!(rel.contains(&key("shift", false)));
        assert!(rel.contains(&InputEvent::PointerButton(PointerButton {
            button: MouseButton::Left,
            down: false
        })));
        assert!(p.is_empty());
        assert!(p.take_release_events().is_empty());
    }
}
