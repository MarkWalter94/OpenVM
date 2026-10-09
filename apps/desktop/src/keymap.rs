//! Mappatura dei tasti fisici (egui `physical_key`, derivato dallo scancode
//! winit `KeyCode`) sui qcode QEMU (`QKeyCode`).
//!
//! I qcode QEMU sono posizionali: indicano il tasto fisico secondo la
//! disposizione US, non il carattere. La traduzione tasto fisico → qcode è
//! quindi la stessa per i layout dichiarati **US** e **IT**; il layout
//! dichiarato stabilisce quale carattere produce il guest, che deve essere
//! configurato con lo stesso layout. Il layout della tastiera host non
//! influisce, perché si usa la posizione fisica e non il carattere.
//!
//! Limiti ereditati da egui-winit 0.36.2 (vedi F0-04): tastierino numerico
//! fuso con i tasti principali (Numpad0 → Num0, NumpadEnter → Enter,
//! NumpadSubtract → Minus, NumpadDivide → Slash); Caps Lock, Num Lock, Scroll
//! Lock, Stamp, Pausa e Menu non hanno un `egui::Key` e non sono inoltrati.

use egui::Key;

/// Layout tastiera dichiarati per il guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardLayout {
    /// Inglese USA (ANSI/ISO).
    Us,
    /// Italiano (ISO, con il tasto `<>` a sinistra di Z).
    It,
}

impl KeyboardLayout {
    /// Nome breve.
    pub fn label(self) -> &'static str {
        match self {
            Self::Us => "US",
            Self::It => "IT",
        }
    }
}

/// Qcode QEMU per un tasto fisico egui, oppure `None` se non coperto.
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
        // Prodotto da egui-winit solo per NumpadAdd.
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

/// Carattere prodotto (senza modificatori) dal tasto nel layout dichiarato,
/// per la documentazione dei tasti coperti. `None` per tasti non stampabili.
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

    /// Tasti di `egui::Key::ALL` dichiarati non coperti in F0-04.
    const NON_COPERTI: &[Key] = &[
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
    fn ogni_tasto_egui_e_mappato_o_dichiarato_non_coperto() {
        for k in Key::ALL {
            let fkey = k.name().starts_with('F')
                && k.name().len() > 1
                && k.name()[1..].parse::<u8>().is_ok_and(|n| n > 12);
            if NON_COPERTI.contains(k) || fkey {
                assert!(
                    qcode(*k).is_none(),
                    "{k:?} è mappato ma dichiarato non coperto"
                );
            } else {
                assert!(qcode(*k).is_some(), "{k:?} non mappato e non dichiarato");
            }
        }
    }

    #[test]
    fn layout_it_differisce_solo_nei_caratteri() {
        assert_eq!(qcode(Key::OpenBracket), Some("bracket_left"));
        assert_eq!(base_char(Key::OpenBracket, KeyboardLayout::Us), Some('['));
        assert_eq!(base_char(Key::OpenBracket, KeyboardLayout::It), Some('è'));
        assert_eq!(base_char(Key::A, KeyboardLayout::It), Some('a'));
        assert_eq!(qcode(Key::IntlBackslash), Some("less"));
    }
}
