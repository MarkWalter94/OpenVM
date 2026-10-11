//! Keyboard: declared US layout (ADR-0019), ASCII only.
//!
//! `vm_keyboard_type` maps printable ASCII (0x20..=0x7E), `\n` (Enter) and
//! `\t` (Tab) to QEMU key codes with the US layout; any other character
//! is refused before anything is sent (the guest layout must be US for
//! the text to come out as written). `vm_keyboard_press` accepts key names
//! and chords (`ctrl+alt+t`) from an allowlist of QEMU key codes.

use lightvm_proto::{InputEvent, KeyAction, QCode};

/// Layouts the adapter can type with.
pub const LAYOUTS: [&str; 1] = ["us"];
/// Longest text of one `vm_keyboard_type` call, in characters.
pub const MAX_TYPE_CHARS: usize = 1024;
/// Longest chord list of one `vm_keyboard_press` call.
pub const MAX_PRESS_KEYS: usize = 8;

const LOWER: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];
const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// `(shift, qcode)` of a character with the US layout.
#[must_use]
pub fn us_key(c: char) -> Option<(bool, &'static str)> {
    Some(match c {
        'a'..='z' => (false, LOWER[c as usize - 'a' as usize]),
        'A'..='Z' => (true, LOWER[c as usize - 'A' as usize]),
        '0'..='9' => (false, DIGITS[c as usize - '0' as usize]),
        ' ' => (false, "spc"),
        '\n' => (false, "ret"),
        '\t' => (false, "tab"),
        '`' => (false, "grave_accent"),
        '~' => (true, "grave_accent"),
        '-' => (false, "minus"),
        '_' => (true, "minus"),
        '=' => (false, "equal"),
        '+' => (true, "equal"),
        '[' => (false, "bracket_left"),
        '{' => (true, "bracket_left"),
        ']' => (false, "bracket_right"),
        '}' => (true, "bracket_right"),
        '\\' => (false, "backslash"),
        '|' => (true, "backslash"),
        ';' => (false, "semicolon"),
        ':' => (true, "semicolon"),
        '\'' => (false, "apostrophe"),
        '"' => (true, "apostrophe"),
        ',' => (false, "comma"),
        '<' => (true, "comma"),
        '.' => (false, "dot"),
        '>' => (true, "dot"),
        '/' => (false, "slash"),
        '?' => (true, "slash"),
        '!' => (true, "1"),
        '@' => (true, "2"),
        '#' => (true, "3"),
        '$' => (true, "4"),
        '%' => (true, "5"),
        '^' => (true, "6"),
        '&' => (true, "7"),
        '*' => (true, "8"),
        '(' => (true, "9"),
        ')' => (true, "0"),
        _ => return None,
    })
}

fn key(q: &str, down: bool) -> InputEvent {
    InputEvent::Key(KeyAction {
        qcode: QCode::parse(q).unwrap_or_else(|_| unreachable_qcode()),
        down,
    })
}

#[cold]
fn unreachable_qcode() -> QCode {
    // Every name used here matches the QCode pattern; kept total anyway.
    QCode::parse("spc").unwrap_or_else(|_| std::process::abort())
}

/// Events of one character (press/release, with shift if needed).
#[must_use]
pub fn char_events(c: char) -> Option<Vec<InputEvent>> {
    let (shift, q) = us_key(c)?;
    let mut ev = Vec::with_capacity(4);
    if shift {
        ev.push(key("shift", true));
    }
    ev.push(key(q, true));
    ev.push(key(q, false));
    if shift {
        ev.push(key("shift", false));
    }
    Some(ev)
}

/// First character (position, 0-based) the layout cannot type.
#[must_use]
pub fn first_untypable(text: &str) -> Option<usize> {
    text.chars().position(|c| us_key(c).is_none())
}

/// QEMU key codes accepted by `vm_keyboard_press`.
const ALLOWED: &[&str] = &[
    "shift",
    "shift_r",
    "alt",
    "alt_r",
    "ctrl",
    "ctrl_r",
    "meta_l",
    "meta_r",
    "menu",
    "esc",
    "backspace",
    "tab",
    "ret",
    "spc",
    "caps_lock",
    "num_lock",
    "scroll_lock",
    "print",
    "sysrq",
    "pause",
    "home",
    "end",
    "pgup",
    "pgdn",
    "insert",
    "delete",
    "left",
    "right",
    "up",
    "down",
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
    "minus",
    "equal",
    "bracket_left",
    "bracket_right",
    "backslash",
    "semicolon",
    "apostrophe",
    "grave_accent",
    "comma",
    "dot",
    "slash",
    "asterisk",
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
    "kp_add",
    "kp_subtract",
    "kp_multiply",
    "kp_divide",
    "kp_enter",
    "kp_decimal",
];

/// Canonical QEMU key code of a key name (aliases accepted).
#[must_use]
pub fn qcode_of(name: &str) -> Option<&'static str> {
    let n = name.trim().to_ascii_lowercase();
    let alias = match n.as_str() {
        "enter" | "return" => "ret",
        "space" => "spc",
        "escape" => "esc",
        "control" => "ctrl",
        "super" | "win" | "windows" | "meta" => "meta_l",
        "pageup" | "page_up" => "pgup",
        "pagedown" | "page_down" => "pgdn",
        "del" => "delete",
        "ins" => "insert",
        "arrowleft" => "left",
        "arrowright" => "right",
        "arrowup" => "up",
        "arrowdown" => "down",
        other => other,
    };
    if let Some(i) = LOWER.iter().position(|l| *l == alias) {
        return Some(LOWER[i]);
    }
    if let Some(i) = DIGITS.iter().position(|d| *d == alias) {
        return Some(DIGITS[i]);
    }
    ALLOWED.iter().copied().find(|k| *k == alias)
}

/// Parses a chord such as `ctrl+alt+t` (or `ctrl-alt-t`): key codes in
/// press order.
pub fn parse_chord(chord: &str) -> Result<Vec<&'static str>, String> {
    let parts: Vec<&str> = chord
        .split(['+', '-'])
        .filter(|p| !p.trim().is_empty())
        .collect();
    // A lone "-" or "+" key: the name is the character itself.
    let parts = if parts.is_empty() && !chord.trim().is_empty() {
        vec![chord.trim()]
    } else {
        parts
    };
    if parts.is_empty() || parts.len() > MAX_PRESS_KEYS {
        return Err(format!("a chord has 1..={MAX_PRESS_KEYS} keys"));
    }
    parts
        .iter()
        .map(|p| match *p {
            "-" => Some("minus"),
            "+" => Some("equal"),
            _ => qcode_of(p),
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "unknown key name (see the tool description)".to_owned())
}

/// Events of a chord: press in order, release in reverse order.
#[must_use]
pub fn chord_events(keys: &[&'static str]) -> Vec<InputEvent> {
    let mut ev: Vec<InputEvent> = keys.iter().map(|k| key(k, true)).collect();
    ev.extend(keys.iter().rev().map(|k| key(k, false)));
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_printable_ascii_is_typable_and_nothing_else() {
        for b in 0x20u8..=0x7e {
            assert!(us_key(b as char).is_some(), "{:?}", b as char);
        }
        assert!(us_key('\n').is_some() && us_key('\t').is_some());
        for c in ['é', 'ß', '€', '\u{1b}', '\r', '\u{7f}'] {
            assert!(us_key(c).is_none(), "{c:?}");
        }
        assert_eq!(first_untypable("ls -la\n"), None);
        assert_eq!(first_untypable("abc é"), Some(4));
        // Every key code used is an allowed QCode.
        for b in 0x20u8..=0x7e {
            let (_, q) = us_key(b as char).unwrap();
            assert!(QCode::parse(q).is_ok());
        }
    }

    #[test]
    fn chords() {
        assert_eq!(parse_chord("ctrl+alt+t").unwrap(), ["ctrl", "alt", "t"]);
        assert_eq!(parse_chord("Ctrl-Alt-T").unwrap(), ["ctrl", "alt", "t"]);
        assert_eq!(parse_chord("Enter").unwrap(), ["ret"]);
        assert!(parse_chord("ctrl+qmp_execute").is_err());
        assert!(parse_chord("").is_err());
        let ev = chord_events(&["ctrl", "c"]);
        assert_eq!(ev.len(), 4);
    }
}
