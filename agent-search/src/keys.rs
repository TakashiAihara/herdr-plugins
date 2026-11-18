use crate::config::Keys;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

// A key as written in the config (fzf-style names: ctrl-s, alt-up, alt-0, shift-a). Always
// normalized, so two spellings of one key are equal.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Bind {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

fn strip_mod<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn upper(c: char) -> char {
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

// The one form of a key: a shifted letter is its uppercase with SHIFT (a terminal reports a typed
// capital as Char('A') + SHIFT, or as Char('a') + SHIFT, or without SHIFT), and BackTab never
// carries SHIFT.
pub fn normalize(code: KeyCode, mods: KeyModifiers) -> Bind {
    match code {
        KeyCode::Char(c) if c.is_alphabetic() => {
            let up = upper(c);
            if mods.contains(KeyModifiers::SHIFT) || (c.is_uppercase() && up == c) {
                Bind {
                    code: KeyCode::Char(up),
                    mods: mods | KeyModifiers::SHIFT,
                }
            } else {
                Bind { code, mods }
            }
        }
        KeyCode::BackTab => Bind {
            code,
            mods: mods - KeyModifiers::SHIFT,
        },
        _ => Bind { code, mods },
    }
}

// Modifiers and key names are case-insensitive; a single character keeps its case.
pub fn parse_key_err(name: &str) -> Result<Bind, String> {
    let mut rest = name;
    let mut mods = KeyModifiers::NONE;
    loop {
        if let Some(r) = strip_mod(rest, "ctrl-") {
            mods |= KeyModifiers::CONTROL;
            rest = r;
        } else if let Some(r) = strip_mod(rest, "alt-") {
            mods |= KeyModifiers::ALT;
            rest = r;
        } else if let Some(r) = strip_mod(rest, "shift-") {
            mods |= KeyModifiers::SHIFT;
            rest = r;
        } else {
            break;
        }
    }
    let lower = rest.to_ascii_lowercase();
    let named = match lower.as_str() {
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "tab" if mods.contains(KeyModifiers::SHIFT) => Some(KeyCode::BackTab),
        "tab" => Some(KeyCode::Tab),
        "btab" | "backtab" => Some(KeyCode::BackTab),
        "space" => Some(KeyCode::Char(' ')),
        "backspace" | "bspace" => Some(KeyCode::Backspace),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pgup" | "page-up" => Some(KeyCode::PageUp),
        "pgdn" | "page-down" => Some(KeyCode::PageDown),
        "delete" | "del" => Some(KeyCode::Delete),
        "insert" => Some(KeyCode::Insert),
        s => s
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=12).contains(n))
            .map(KeyCode::F),
    };
    if let Some(code) = named {
        return Ok(normalize(code, mods));
    }
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_alphabetic() => Ok(normalize(KeyCode::Char(c), mods)),
        (Some(_), None) if mods.contains(KeyModifiers::SHIFT) => Err(format!(
            "{name:?}: shift only combines with a letter or a named key; the terminal reports the shifted character itself, so name that character"
        )),
        (Some(c), None) if !c.is_control() => Ok(normalize(KeyCode::Char(c), mods)),
        _ => Err(format!("unknown key name {name:?}")),
    }
}

#[cfg(test)]
pub fn parse_key(name: &str) -> Option<Bind> {
    parse_key_err(name).ok()
}

fn is_builtin(b: Bind) -> bool {
    let m = b.mods;
    let plain = m.is_empty();
    match b.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace | KeyCode::Up | KeyCode::Down => plain,
        KeyCode::Char('c' | 'p' | 'n') if m == KeyModifiers::CONTROL => true,
        KeyCode::Char('1'..='9') if m == KeyModifiers::ALT => true,
        // Printable characters are typed into the search.
        KeyCode::Char(c) => {
            !c.is_control() && !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        }
        _ => false,
    }
}

// Ctrl with one of these chars is the same byte a terminal sends for another key: ^M is Enter's
// byte, ^I is Tab's, ^H is Backspace's, ^J is the byte Enter sends when the line discipline maps
// it, ^[ is Esc's and ^@ is Ctrl-Space's. A bind named for the char either never fires, because
// the terminal reports the other key, or it steals that key's presses.
fn ctrl_byte(b: Bind) -> Option<&'static str> {
    if b.mods != KeyModifiers::CONTROL {
        return None;
    }
    let KeyCode::Char(c) = b.code else {
        return None;
    };
    Some(match c.to_ascii_lowercase() {
        'm' | 'j' => "enter",
        'i' => "tab",
        'h' => "backspace",
        '[' => "esc",
        '@' => "ctrl-space",
        _ => return None,
    })
}

// Every configured key must be a key the popup can see, unique, and not one it already uses. The
// error names the field.
pub fn validate(keys: &Keys) -> Result<Vec<(&'static str, Bind)>, String> {
    let mut seen: Vec<(&'static str, Bind)> = Vec::new();
    for (field, name) in [
        ("sort", &keys.sort),
        ("group", &keys.group),
        ("up", &keys.up),
        ("down", &keys.down),
        ("clear", &keys.clear),
    ] {
        let bind = parse_key_err(name).map_err(|e| format!("{e} in keys.{field}"))?;
        if let Some(other) = ctrl_byte(bind) {
            return Err(format!(
                "keys.{field} {name:?} is the same key as {other} to a terminal"
            ));
        }
        if is_builtin(bind) {
            return Err(format!(
                "keys.{field} {name:?} is already used by the popup (alt-1..alt-9, esc, enter, backspace, up, down, ctrl-p, ctrl-n, ctrl-c, or a character typed into the search)"
            ));
        }
        if let Some((other, _)) = seen.iter().find(|(_, b)| *b == bind) {
            return Err(format!(
                "keys.{field} {name:?} is the same key as keys.{other}"
            ));
        }
        seen.push((field, bind));
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(code: KeyCode, mods: KeyModifiers) -> Option<Bind> {
        Some(Bind { code, mods })
    }

    fn keys(sort: &str, group: &str) -> Keys {
        Keys {
            sort: sort.into(),
            group: group.into(),
            up: "alt-up".into(),
            down: "alt-down".into(),
            clear: "alt-0".into(),
        }
    }

    #[test]
    fn key_names_parse_into_code_and_modifiers() {
        assert_eq!(
            parse_key("ctrl-s"),
            bind(KeyCode::Char('s'), KeyModifiers::CONTROL)
        );
        assert_eq!(parse_key("alt-up"), bind(KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(
            parse_key("Alt-0"),
            bind(KeyCode::Char('0'), KeyModifiers::ALT)
        );
        assert_eq!(
            parse_key("ctrl-alt-f5"),
            bind(KeyCode::F(5), KeyModifiers::CONTROL | KeyModifiers::ALT)
        );
        assert_eq!(parse_key("tab").map(|b| b.code), Some(KeyCode::Tab));
        assert_eq!(parse_key("ctrl-nonsense"), None);
        assert_eq!(parse_key("ctrl-"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(
            parse_key("shift-tab"),
            bind(KeyCode::BackTab, KeyModifiers::NONE)
        );
        assert_eq!(
            parse_key("ctrl-shift-tab"),
            bind(KeyCode::BackTab, KeyModifiers::CONTROL)
        );
        assert_eq!(
            normalize(KeyCode::BackTab, KeyModifiers::SHIFT),
            parse_key("shift-tab").unwrap_or_else(|| panic!("shift-tab"))
        );
    }

    #[test]
    fn an_uppercase_letter_and_shift_letter_are_one_bind_for_any_script() {
        assert_ne!(parse_key("alt-a"), parse_key("alt-A"));
        assert_eq!(parse_key("alt-A"), parse_key("alt-shift-a"));
        assert_eq!(parse_key("alt-É"), parse_key("alt-shift-é"));
        assert_eq!(
            parse_key("A"),
            bind(KeyCode::Char('A'), KeyModifiers::SHIFT)
        );
        assert_eq!(parse_key("a"), bind(KeyCode::Char('a'), KeyModifiers::NONE));
        // What a terminal may report for a typed capital.
        let want = parse_key("alt-A");
        assert_eq!(
            normalize(KeyCode::Char('a'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            want.unwrap_or_else(|| panic!("alt-A"))
        );
        assert_eq!(
            normalize(KeyCode::Char('A'), KeyModifiers::ALT),
            want.unwrap_or_else(|| panic!("alt-A"))
        );
        assert_eq!(
            normalize(KeyCode::Char('A'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            want.unwrap_or_else(|| panic!("alt-A"))
        );
    }

    #[test]
    fn shift_with_a_non_letter_is_rejected_with_the_key_named() {
        for bad in ["shift-1", "alt-shift-/", "shift-;"] {
            let err = parse_key_err(bad).err().unwrap_or_default();
            assert!(
                err.contains(bad) && err.contains("shift only combines"),
                "{err}"
            );
        }
        assert!(parse_key("shift-up").is_some());
        assert!(parse_key("shift-f3").is_some());
    }

    #[test]
    fn validation_rejects_duplicates_and_keys_the_popup_uses() {
        assert!(validate(&keys("ctrl-s", "ctrl-g")).is_ok());
        let dup = validate(&keys("alt-A", "alt-shift-a"))
            .err()
            .unwrap_or_default();
        assert!(
            dup.contains("keys.group \"alt-shift-a\" is the same key as keys.sort"),
            "{dup}"
        );
        for builtin in [
            "alt-1",
            "alt-9",
            "esc",
            "enter",
            "backspace",
            "up",
            "down",
            "ctrl-p",
            "ctrl-n",
            "ctrl-c",
            "x",
            "X",
            "space",
            "5",
        ] {
            let err = validate(&keys(builtin, "ctrl-g")).err().unwrap_or_default();
            assert!(
                err.contains("already used by the popup") && err.contains(&format!("{builtin:?}")),
                "{builtin}: {err}"
            );
        }
        for fine in [
            "ctrl-x",
            "alt-x",
            "alt-A",
            "tab",
            "shift-tab",
            "f2",
            "ctrl-up",
        ] {
            assert!(validate(&keys(fine, "ctrl-g")).is_ok(), "{fine}");
        }
        let bad = validate(&keys("ctrl-nonsense", "ctrl-g"))
            .err()
            .unwrap_or_default();
        assert!(
            bad.contains("unknown key name \"ctrl-nonsense\" in keys.sort"),
            "{bad}"
        );
    }

    #[test]
    fn ctrl_names_that_are_other_keys_on_a_terminal_are_rejected() {
        for (name, other) in [
            ("ctrl-m", "enter"),
            ("ctrl-j", "enter"),
            ("ctrl-i", "tab"),
            ("ctrl-h", "backspace"),
            ("ctrl-[", "esc"),
            ("ctrl-@", "ctrl-space"),
        ] {
            let err = validate(&keys(name, "ctrl-g")).err().unwrap_or_default();
            assert!(
                err.contains(&format!(
                    "keys.sort {name:?} is the same key as {other} to a terminal"
                )),
                "{name}: {err}"
            );
        }
        // Ctrl with more modifiers is not the name of a control byte, and neither are the ctrl
        // letters a terminal does report as Char+CONTROL.
        for fine in ["ctrl-k", "ctrl-4", "ctrl-shift-m", "alt-ctrl-m"] {
            assert!(validate(&keys(fine, "ctrl-g")).is_ok(), "{fine}");
        }
    }
}
