//! Keyboard combo parsing for `press_key`.
//!
//! Modifier bitmask matches the monitor protocol and CDP: Alt = 1, Ctrl = 2,
//! Meta = 4, Shift = 8 (README §6.5). Text is only attached when the key really
//! types a character; shortcuts and navigation keys stay `rawKeyDown`.

/// Modifier bitmask values shared with the monitor gateway.
pub const MOD_ALT: u32 = 1;
pub const MOD_CTRL: u32 = 2;
pub const MOD_META: u32 = 4;
pub const MOD_SHIFT: u32 = 8;

/// One parsed keyboard chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    pub key: String,
    pub code: String,
    pub text: Option<String>,
    pub modifiers: u32,
}

impl KeyCombo {
    #[must_use]
    pub fn has_shortcut_modifier(&self) -> bool {
        self.modifiers & (MOD_ALT | MOD_CTRL | MOD_META) != 0
    }
}

/// Parses `Enter`, `a`, `Control+A`, `Shift+Tab` and similar chords.
///
/// # Errors
///
/// Returns a human-readable reason for an unknown key or an empty chord.
pub fn parse_key_combo(input: &str) -> Result<KeyCombo, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("key must not be empty".to_owned());
    }
    if input == "+" {
        return Ok(KeyCombo {
            key: "+".to_owned(),
            code: "Equal".to_owned(),
            text: Some("+".to_owned()),
            modifiers: MOD_SHIFT,
        });
    }

    let mut parts: Vec<&str> = input.split('+').map(str::trim).collect();
    let key_part = parts.pop().unwrap_or_default();
    if key_part.is_empty() {
        return Err(format!("missing key after modifiers in {input:?}"));
    }
    let mut modifiers = 0;
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "alt" | "option" => modifiers |= MOD_ALT,
            "ctrl" | "control" => modifiers |= MOD_CTRL,
            "meta" | "cmd" | "command" | "super" => modifiers |= MOD_META,
            "shift" => modifiers |= MOD_SHIFT,
            other => return Err(format!("unknown modifier {other:?} in {input:?}")),
        }
    }

    let shortcut = modifiers & (MOD_ALT | MOD_CTRL | MOD_META) != 0;
    let shifted = modifiers & MOD_SHIFT != 0;
    let key = named_key(key_part, shifted)
        .or_else(|| printable_key(key_part, shifted, shortcut))
        .ok_or_else(|| format!("unknown key {key_part:?}"))?;
    Ok(KeyCombo {
        key: key.key,
        code: key.code,
        text: if shortcut { None } else { key.text },
        modifiers,
    })
}

struct ResolvedKey {
    key: String,
    code: String,
    text: Option<String>,
}

fn named_key(raw: &str, shifted: bool) -> Option<ResolvedKey> {
    let normalized = raw.to_ascii_lowercase();
    let (key, code, text) = match normalized.as_str() {
        "enter" | "return" => ("Enter", "Enter", Some("\r")),
        "tab" => ("Tab", "Tab", None),
        "backspace" => ("Backspace", "Backspace", None),
        "escape" | "esc" => ("Escape", "Escape", None),
        "space" | "spacebar" => (" ", "Space", Some(" ")),
        "delete" | "del" => ("Delete", "Delete", None),
        "insert" => ("Insert", "Insert", None),
        "home" => ("Home", "Home", None),
        "end" => ("End", "End", None),
        "pageup" | "page_up" => ("PageUp", "PageUp", None),
        "pagedown" | "page_down" => ("PageDown", "PageDown", None),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", None),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", None),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", None),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", None),
        "meta" => ("Meta", "MetaLeft", None),
        "control" | "ctrl" => ("Control", "ControlLeft", None),
        "alt" => ("Alt", "AltLeft", None),
        "shift" => ("Shift", "ShiftLeft", None),
        f if f.len() >= 2 && f.len() <= 3 && f.starts_with('f') => {
            let number = f[1..].parse::<u8>().ok()?;
            if !(1..=12).contains(&number) {
                return None;
            }
            return Some(ResolvedKey {
                key: format!("F{number}"),
                code: format!("F{number}"),
                text: None,
            });
        }
        _ => return None,
    };
    let key = if shifted && key.len() == 1 {
        key.to_uppercase()
    } else {
        key.to_owned()
    };
    Some(ResolvedKey {
        key,
        code: code.to_owned(),
        text: text.map(str::to_owned),
    })
}

fn printable_key(raw: &str, shifted: bool, shortcut: bool) -> Option<ResolvedKey> {
    let mut characters = raw.chars();
    let character = characters.next()?;
    if characters.next().is_some() {
        return None;
    }
    if character.is_ascii_alphabetic() {
        let upper = character.to_ascii_uppercase();
        let key = if shifted {
            upper
        } else {
            character.to_ascii_lowercase()
        };
        return Some(ResolvedKey {
            key: key.to_string(),
            code: format!("Key{upper}"),
            text: (!shortcut).then(|| key.to_string()),
        });
    }
    if character.is_ascii_digit() {
        return Some(ResolvedKey {
            key: character.to_string(),
            code: format!("Digit{character}"),
            text: (!shortcut).then(|| character.to_string()),
        });
    }
    let code = match character {
        '-' => "Minus",
        '=' => "Equal",
        '[' => "BracketLeft",
        ']' => "BracketRight",
        '\\' => "Backslash",
        ';' => "Semicolon",
        '\'' => "Quote",
        ',' => "Comma",
        '.' => "Period",
        '/' => "Slash",
        '`' => "Backquote",
        _ => return None,
    };
    Some(ResolvedKey {
        key: character.to_string(),
        code: code.to_owned(),
        text: (!shortcut).then(|| character.to_string()),
    })
}

#[cfg(test)]
#[path = "keys_test.rs"]
mod tests;
