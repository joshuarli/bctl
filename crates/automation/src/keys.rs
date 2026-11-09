use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub(crate) key: String,
    pub(crate) code: String,
    pub(crate) virtual_code: u32,
    pub(crate) text: String,
    pub(crate) shifted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Alt,
    Control,
    Meta,
    Shift,
}

impl Modifier {
    pub(crate) fn bit(self) -> u32 {
        match self {
            Modifier::Alt => 1,
            Modifier::Control => 2,
            Modifier::Meta => 4,
            Modifier::Shift => 8,
        }
    }

    pub(crate) fn key(self) -> Key {
        let (key, code, virtual_code) = match self {
            Modifier::Alt => ("Alt", "AltLeft", 18),
            Modifier::Control => ("Control", "ControlLeft", 17),
            Modifier::Meta => ("Meta", "MetaLeft", 91),
            Modifier::Shift => ("Shift", "ShiftLeft", 16),
        };
        Key { key: key.into(), code: code.into(), virtual_code, text: String::new(), shifted: false }
    }
}

/// A key press with modifiers held, parsed from `Enter`, `a`, or `Control+Shift+K`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub(crate) modifiers: Vec<Modifier>,
    pub(crate) key: Key,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownKey(pub String);

impl fmt::Display for UnknownKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a key bctl knows; use a name like Enter, Tab, ArrowDown, F5, or one character, joined to Control, Shift, Alt, or Meta with +", self.0)
    }
}

impl std::error::Error for UnknownKey {}

impl Chord {
    pub fn parse(text: &str) -> Result<Chord, UnknownKey> {
        let unknown = || UnknownKey(text.to_string());
        let (held, last) = match text.rsplit_once('+') {
            Some(("", "")) => ("", "+"),
            Some((held, "")) => (held.strip_suffix('+').ok_or_else(unknown)?, "+"),
            Some((held, last)) => (held, last),
            None => ("", text),
        };
        let mut modifiers = Vec::new();
        for name in held.split('+').filter(|_| !held.is_empty()) {
            let modifier = match name {
                "Alt" => Modifier::Alt,
                "Control" | "Ctrl" => Modifier::Control,
                "Meta" | "Command" | "Cmd" => Modifier::Meta,
                "Shift" => Modifier::Shift,
                _ => return Err(unknown()),
            };
            if modifiers.contains(&modifier) {
                return Err(unknown());
            }
            modifiers.push(modifier);
        }
        let key = named(last).or_else(|| single(last).and_then(of_char)).ok_or_else(unknown)?;
        Ok(Chord { modifiers, key })
    }

    pub(crate) fn modifier_bits(&self) -> u32 {
        self.modifiers.iter().map(|m| m.bit()).sum::<u32>() | if self.key.shifted { Modifier::Shift.bit() } else { 0 }
    }

    pub(crate) fn typed_text(&self) -> &str {
        if self.modifiers.iter().all(|m| *m == Modifier::Shift) { &self.key.text } else { "" }
    }
}

fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

fn key(key: &str, code: &str, virtual_code: u32, text: &str, shifted: bool) -> Key {
    Key { key: key.into(), code: code.into(), virtual_code, text: text.into(), shifted }
}

fn named(name: &str) -> Option<Key> {
    let plain = |code: &str, virtual_code| Some(key(name, code, virtual_code, "", false));
    match name {
        "Enter" => Some(key("Enter", "Enter", 13, "\r", false)),
        "Tab" => plain("Tab", 9),
        "Backspace" => plain("Backspace", 8),
        "Delete" => plain("Delete", 46),
        "Escape" => plain("Escape", 27),
        "Space" => Some(key(" ", "Space", 32, " ", false)),
        "ArrowUp" => plain("ArrowUp", 38),
        "ArrowDown" => plain("ArrowDown", 40),
        "ArrowLeft" => plain("ArrowLeft", 37),
        "ArrowRight" => plain("ArrowRight", 39),
        "Home" => plain("Home", 36),
        "End" => plain("End", 35),
        "PageUp" => plain("PageUp", 33),
        "PageDown" => plain("PageDown", 34),
        "Insert" => plain("Insert", 45),
        function if function.len() >= 2 && function.starts_with('F') => {
            let n: u32 = function[1..].parse().ok().filter(|n| (1..=12).contains(n))?;
            plain(function, 111 + n)
        }
        _ => None,
    }
}

const PUNCTUATION: &[(char, char, &str, u32)] = &[
    (';', ':', "Semicolon", 186),
    ('=', '+', "Equal", 187),
    (',', '<', "Comma", 188),
    ('-', '_', "Minus", 189),
    ('.', '>', "Period", 190),
    ('/', '?', "Slash", 191),
    ('`', '~', "Backquote", 192),
    ('[', '{', "BracketLeft", 219),
    ('\\', '|', "Backslash", 220),
    (']', '}', "BracketRight", 221),
    ('\'', '"', "Quote", 222),
];

const SHIFTED_DIGITS: &str = ")!@#$%^&*(";

pub(crate) fn of_char(c: char) -> Option<Key> {
    let text = c.to_string();
    match c {
        'a'..='z' => Some(key(&text, &format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as u32, &text, false)),
        'A'..='Z' => Some(key(&text, &format!("Key{c}"), c as u32, &text, true)),
        '0'..='9' => Some(key(&text, &format!("Digit{c}"), c as u32, &text, false)),
        ' ' => Some(key(" ", "Space", 32, " ", false)),
        '\n' | '\r' => named("Enter"),
        '\t' => named("Tab"),
        _ => {
            if let Some(digit) = SHIFTED_DIGITS.find(c) {
                return Some(key(&text, &format!("Digit{digit}"), 48 + digit as u32, &text, true));
            }
            let (plain, _, code, virtual_code) = PUNCTUATION.iter().find(|(plain, shifted, _, _)| *plain == c || *shifted == c)?;
            Some(key(&text, code, *virtual_code, &text, *plain != c))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_name_their_key_and_held_modifiers() {
        let chord = Chord::parse("Control+Shift+K").unwrap();
        assert_eq!(chord.modifiers, [Modifier::Control, Modifier::Shift]);
        assert_eq!((chord.key.code.as_str(), chord.key.virtual_code, chord.modifier_bits(), chord.typed_text()), ("KeyK", 75, 10, ""));
        let enter = Chord::parse("Enter").unwrap();
        assert_eq!((enter.key.virtual_code, enter.typed_text()), (13, "\r"));
        let plus = Chord::parse("Shift++").unwrap();
        assert_eq!((plus.key.code.as_str(), plus.typed_text(), plus.modifier_bits()), ("Equal", "+", 8));
        assert_eq!(Chord::parse("?").unwrap().modifier_bits(), 8, "a shifted symbol holds Shift");
        assert_eq!(Chord::parse("F12").unwrap().key.virtual_code, 123);
        assert_eq!(Chord::parse("+").unwrap().key.code, "Equal");
        for bad in ["", "Hyper+a", "Control+", "Control+Control+a", "F13", "ab", "é"] {
            assert!(Chord::parse(bad).is_err(), "{bad}");
        }
    }
}
