//! Digitazione di testo ASCII per il layout guest **US** (spike): carattere →
//! qcode più eventuale Shift. Le combinazioni (`ctrl-alt-t`) usano i nomi qcode.

/// Qcode e necessità di Shift per un carattere ASCII stampabile nel layout US.
pub fn us_char(c: char) -> Option<(&'static str, bool)> {
    const LOWER: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    Some(match c {
        'a'..='z' => (LOWER[(c as u8 - b'a') as usize], false),
        'A'..='Z' => (LOWER[(c as u8 - b'A') as usize], true),
        '0'..='9' => (DIGITS[(c as u8 - b'0') as usize], false),
        ' ' => ("spc", false),
        '\n' => ("ret", false),
        '\t' => ("tab", false),
        '-' => ("minus", false),
        '_' => ("minus", true),
        '=' => ("equal", false),
        '+' => ("equal", true),
        '[' => ("bracket_left", false),
        '{' => ("bracket_left", true),
        ']' => ("bracket_right", false),
        '}' => ("bracket_right", true),
        '\\' => ("backslash", false),
        '|' => ("backslash", true),
        ';' => ("semicolon", false),
        ':' => ("semicolon", true),
        '\'' => ("apostrophe", false),
        '"' => ("apostrophe", true),
        '`' => ("grave_accent", false),
        '~' => ("grave_accent", true),
        ',' => ("comma", false),
        '<' => ("comma", true),
        '.' => ("dot", false),
        '>' => ("dot", true),
        '/' => ("slash", false),
        '?' => ("slash", true),
        '!' => ("1", true),
        '@' => ("2", true),
        '#' => ("3", true),
        '$' => ("4", true),
        '%' => ("5", true),
        '^' => ("6", true),
        '&' => ("7", true),
        '*' => ("8", true),
        '(' => ("9", true),
        ')' => ("0", true),
        _ => return None,
    })
}

/// Converte un nome di tasto della riga di comando in qcode (`ctrl`, `alt`,
/// `shift`, `meta`, `ret`, `esc`, lettere, cifre, `f1`..`f12`, ...).
pub fn combo_key(name: &str) -> Option<&'static str> {
    const NAMED: [&str; 37] = [
        "ctrl",
        "ctrl_r",
        "alt",
        "alt_r",
        "shift",
        "shift_r",
        "meta_l",
        "meta_r",
        "ret",
        "esc",
        "tab",
        "spc",
        "backspace",
        "delete",
        "insert",
        "home",
        "end",
        "pgup",
        "pgdn",
        "up",
        "down",
        "left",
        "right",
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
        "caps_lock",
        "menu",
    ];
    if let Some(n) = NAMED.iter().find(|n| **n == name) {
        return Some(n);
    }
    let mut it = name.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_lowercase() || c.is_ascii_digit() => us_char(c).map(|k| k.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caratteri_us() {
        assert_eq!(us_char('a'), Some(("a", false)));
        assert_eq!(us_char('Q'), Some(("q", true)));
        assert_eq!(us_char('_'), Some(("minus", true)));
        assert_eq!(us_char('è'), None);
        assert_eq!(combo_key("ctrl"), Some("ctrl"));
        assert_eq!(combo_key("t"), Some("t"));
        assert_eq!(combo_key("xx"), None);
    }
}
