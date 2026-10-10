//! Mapping of physical keys (egui `physical_key`, derived from the winit
//! scancode `KeyCode`) to QEMU qcodes (`QKeyCode`).
//!
//! QEMU qcodes are positional: they indicate the physical key according to
//! the US layout, not the character. The physical key → qcode translation is
//! therefore the same for the declared **US** and **IT** layouts; the declared
//! layout determines which character the guest produces, and the guest must
//! be configured with the same layout. The host keyboard layout has no
//! effect, because the physical position is used and not the character.
//!
//! Limits inherited from egui-winit 0.36.2 (see F0-04): numeric keypad merged
//! with the main keys (Numpad0 → Num0, NumpadEnter → Enter, NumpadSubtract →
//! Minus, NumpadDivide → Slash); Caps Lock, Num Lock, Scroll Lock, Print
//! Screen, Pause and Menu have no `egui::Key` and are not forwarded.

use egui::Key;

/// Keyboard layouts declared for the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardLayout {
    /// US English (ANSI/ISO).
    Us,
    /// Italian (ISO, with the `<>` key to the left of Z).
    It,
}

impl KeyboardLayout {
    /// Short name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Us => "US",
            Self::It => "IT",
        }
    }
}

/// QEMU qcode for an egui physical key, or `None` if not covered.
pub fn qcode(key: Key) -> Option<&'static str> {
    use Key::*;
    Some(match key {
        A => "a",
        B => "b",
        C => "c",
        D => "d",
        E => "e",
        F => "f",
        G => "g",
        H => "h",
        I => "i",
        J => "j",
        K => "k",
        L => "l",
        M => "m",
        N => "n",
        O => "o",
        P => "p",
        Q => "q",
        R => "r",
        S => "s",
        T => "t",
        U => "u",
        V => "v",
        W => "w",
        X => "x",
        Y => "y",
        Z => "z",
        Num0 => "0",
        Num1 => "1",
        Num2 => "2",
        Num3 => "3",
        Num4 => "4",
        Num5 => "5",
        Num6 => "6",
        Num7 => "7",
        Num8 => "8",
        Num9 => "9",
        Escape => "esc",
        Tab => "tab",
        Backspace => "backspace",
        Enter => "ret",
        Space => "spc",
        Insert => "insert",
        Delete => "delete",
        Home => "home",
        End => "end",
        PageUp => "pgup",
        PageDown => "pgdn",
        ArrowUp => "up",
        ArrowDown => "down",
        ArrowLeft => "left",
        ArrowRight => "right",
        Minus => "minus",
        Equals => "equal",
        OpenBracket => "bracket_left",
        CloseBracket => "bracket_right",
        Backslash => "backslash",
        Semicolon => "semicolon",
        Quote => "apostrophe",
        Backtick => "grave_accent",
        Comma => "comma",
        Period => "dot",
        Slash => "slash",
        // Produced by egui-winit only for NumpadAdd.
        Plus => "kp_add",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        ShiftLeft => "shift",
        ShiftRight => "shift_r",
        ControlLeft => "ctrl",
        ControlRight => "ctrl_r",
        AltLeft => "alt",
        AltRight => "alt_r",
        SuperLeft => "meta_l",
        SuperRight => "meta_r",
        IntlBackslash => "less",
        _ => return None,
    })
}

/// Character produced (without modifiers) by the key in the declared layout,
/// for the documentation of the covered keys. `None` for non-printable keys.
pub fn base_char(key: Key, layout: KeyboardLayout) -> Option<char> {
    use Key::*;
    let us = match key {
        Minus => '-',
        Equals => '=',
        OpenBracket => '[',
        CloseBracket => ']',
        Backslash => '\\',
        Semicolon => ';',
        Quote => '\'',
        Backtick => '`',
        Comma => ',',
        Period => '.',
        Slash => '/',
        IntlBackslash => '<',
        Space => ' ',
        _ => {
            let n = key.name();
            let mut it = n.chars();
            return match (it.next(), it.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
                _ => None,
            };
        }
    };
    Some(match layout {
        KeyboardLayout::Us => us,
        KeyboardLayout::It => match key {
            Minus => '\'',
            Equals => 'ì',
            OpenBracket => 'è',
            CloseBracket => '+',
            Backslash => 'ù',
            Semicolon => 'ò',
            Quote => 'à',
            Backtick => '\\',
            Slash => '-',
            _ => us,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys of `egui::Key::ALL` declared not covered in F0-04.
    const NOT_COVERED: &[Key] = &[
        Key::Copy,
        Key::Cut,
        Key::Paste,
        Key::Colon,
        Key::Pipe,
        Key::Questionmark,
        Key::Exclamationmark,
        Key::OpenCurlyBracket,
        Key::CloseCurlyBracket,
        Key::BrowserBack,
    ];

    #[test]
    fn every_egui_key_is_mapped_or_declared_not_covered() {
        for k in Key::ALL {
            let fkey = k.name().starts_with('F')
                && k.name().len() > 1
                && k.name()[1..].parse::<u8>().is_ok_and(|n| n > 12);
            if NOT_COVERED.contains(k) || fkey {
                assert!(
                    qcode(*k).is_none(),
                    "{k:?} is mapped but declared not covered"
                );
            } else {
                assert!(qcode(*k).is_some(), "{k:?} neither mapped nor declared");
            }
        }
    }

    #[test]
    fn it_layout_differs_only_in_characters() {
        assert_eq!(qcode(Key::OpenBracket), Some("bracket_left"));
        assert_eq!(base_char(Key::OpenBracket, KeyboardLayout::Us), Some('['));
        assert_eq!(base_char(Key::OpenBracket, KeyboardLayout::It), Some('è'));
        assert_eq!(base_char(Key::A, KeyboardLayout::It), Some('a'));
        assert_eq!(qcode(Key::IntlBackslash), Some("less"));
    }
}
