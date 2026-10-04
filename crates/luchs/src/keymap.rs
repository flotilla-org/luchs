//! DOM physical positions to Carbon virtual key codes. No fallback guesses.
pub const PHYSICAL: &[(&str, u16)] = &[
    ("KeyA", 0),
    ("KeyS", 1),
    ("KeyD", 2),
    ("KeyF", 3),
    ("KeyH", 4),
    ("KeyG", 5),
    ("KeyZ", 6),
    ("KeyX", 7),
    ("KeyC", 8),
    ("KeyV", 9),
    ("IntlBackslash", 10),
    ("KeyB", 11),
    ("KeyQ", 12),
    ("KeyW", 13),
    ("KeyE", 14),
    ("KeyR", 15),
    ("KeyY", 16),
    ("KeyT", 17),
    ("Digit1", 18),
    ("Digit2", 19),
    ("Digit3", 20),
    ("Digit4", 21),
    ("Digit6", 22),
    ("Digit5", 23),
    ("Equal", 24),
    ("Digit9", 25),
    ("Digit7", 26),
    ("Minus", 27),
    ("Digit8", 28),
    ("Digit0", 29),
    ("BracketRight", 30),
    ("KeyO", 31),
    ("KeyU", 32),
    ("BracketLeft", 33),
    ("KeyI", 34),
    ("KeyP", 35),
    ("Enter", 36),
    ("KeyL", 37),
    ("KeyJ", 38),
    ("Quote", 39),
    ("KeyK", 40),
    ("Semicolon", 41),
    ("Backslash", 42),
    ("Comma", 43),
    ("Slash", 44),
    ("KeyN", 45),
    ("KeyM", 46),
    ("Period", 47),
    ("Tab", 48),
    ("Space", 49),
    ("Backquote", 50),
    ("Backspace", 51),
    ("Escape", 53),
    ("MetaRight", 54),
    ("MetaLeft", 55),
    ("ShiftLeft", 56),
    ("CapsLock", 57),
    ("AltLeft", 58),
    ("ControlLeft", 59),
    ("ShiftRight", 60),
    ("AltRight", 61),
    ("ControlRight", 62),
    ("NumpadDecimal", 65),
    ("NumpadMultiply", 67),
    ("NumpadAdd", 69),
    ("NumLock", 71),
    ("NumpadDivide", 75),
    ("NumpadEnter", 76),
    ("NumpadSubtract", 78),
    ("NumpadEqual", 81),
    ("Numpad0", 82),
    ("Numpad1", 83),
    ("Numpad2", 84),
    ("Numpad3", 85),
    ("Numpad4", 86),
    ("Numpad5", 87),
    ("Numpad6", 88),
    ("Numpad7", 89),
    ("Numpad8", 91),
    ("Numpad9", 92),
    ("IntlYen", 93),
    ("IntlRo", 94),
    ("F5", 96),
    ("F6", 97),
    ("F7", 98),
    ("F3", 99),
    ("F8", 100),
    ("F9", 101),
    ("F11", 103),
    ("F13", 105),
    ("F16", 106),
    ("F14", 107),
    ("F10", 109),
    ("F12", 111),
    ("F15", 113),
    ("Help", 114),
    ("Home", 115),
    ("PageUp", 116),
    ("Delete", 117),
    ("F4", 118),
    ("End", 119),
    ("F2", 120),
    ("PageDown", 121),
    ("F1", 122),
    ("ArrowLeft", 123),
    ("ArrowRight", 124),
    ("ArrowDown", 125),
    ("ArrowUp", 126),
];
pub fn physical(code: &str) -> Option<u16> {
    PHYSICAL
        .iter()
        .find_map(|&(name, vk)| (name == code).then_some(vk))
}

/// Logical keys carry literal characters, or AppKit's named-key characters.
/// Printable bindings use a known US position when available for shortcuts;
/// arbitrary Unicode has no invented physical position.
pub fn logical(key: &str) -> Option<(u16, String)> {
    let (code, text) = match key {
        "Enter" => ("Enter", "\r"),
        "Tab" => ("Tab", "\t"),
        "Backspace" => ("Backspace", "\u{7f}"),
        "Escape" => ("Escape", "\u{1b}"),
        "Delete" => ("Delete", "\u{f728}"),
        "ArrowUp" => ("ArrowUp", "\u{f700}"),
        "ArrowDown" => ("ArrowDown", "\u{f701}"),
        "ArrowLeft" => ("ArrowLeft", "\u{f702}"),
        "ArrowRight" => ("ArrowRight", "\u{f703}"),
        "Home" => ("Home", "\u{f729}"),
        "End" => ("End", "\u{f72b}"),
        "PageUp" => ("PageUp", "\u{f72c}"),
        "PageDown" => ("PageDown", "\u{f72d}"),
        "Shift" => ("ShiftLeft", ""),
        "Control" => ("ControlLeft", ""),
        "Alt" => ("AltLeft", ""),
        "Meta" => ("MetaLeft", ""),
        "CapsLock" => ("CapsLock", ""),
        _ => {
            if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
                && (1..=16).contains(&n)
            {
                return Some((physical(key)?, char::from_u32(0xf703 + n)?.to_string()));
            }
            let mut chars = key.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || ch.is_control() {
                return None;
            }
            let code = if ch.is_ascii_alphabetic() {
                format!("Key{}", ch.to_ascii_uppercase())
            } else if ch.is_ascii_digit() {
                format!("Digit{ch}")
            } else {
                match ch {
                    ' ' => "Space",
                    '-' | '_' => "Minus",
                    '=' | '+' => "Equal",
                    '[' | '{' => "BracketLeft",
                    ']' | '}' => "BracketRight",
                    '\\' | '|' => "Backslash",
                    ';' | ':' => "Semicolon",
                    '\'' | '"' => "Quote",
                    ',' | '<' => "Comma",
                    '.' | '>' => "Period",
                    '/' | '?' => "Slash",
                    '`' | '~' => "Backquote",
                    _ => "",
                }
                .into()
            };
            return Some((physical(&code).unwrap_or(u16::MAX), key.into()));
        }
    };
    Some((physical(code)?, text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_supported_code_has_a_unique_native_position() {
        let mut codes = std::collections::HashSet::new();
        let mut positions = std::collections::HashSet::new();
        for &(code, expected) in PHYSICAL {
            assert!(codes.insert(code));
            assert!(positions.insert(expected));
            assert_eq!(physical(code), Some(expected));
            assert!(expected <= 126);
        }
        assert_eq!(physical("Unidentified"), None);
        assert_eq!(physical("KeyAA"), None);
        assert_eq!(physical("AudioVolumeUp"), None);
        assert_eq!(logical("é"), Some((u16::MAX, "é".into())));
        assert_eq!(logical("ArrowLeft"), Some((123, "\u{f702}".into())));
    }
}
