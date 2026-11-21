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

// Ctrl with one of these chars is the same byte a terminal sends for another key: ^M is Enter's
// byte, ^I is Tab's, ^H is Backspace's, ^J is the byte Enter sends when the line discipline maps
// it, ^[ is Esc's and ^@ is Ctrl-Space's. A bind named for the char either never fires, because
// the terminal reports the other key, or it steals that key's presses. The shifted spelling of a
// control letter (`ctrl-M`, parsed as CONTROL|SHIFT|M) sends the same byte, so it is caught too.
fn ctrl_byte(b: Bind) -> Option<&'static str> {
    if b.mods != KeyModifiers::CONTROL && b.mods != (KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
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

// The popup's actions, in help order. A [keys] value is a key name or a list of key names.
pub const ACTIONS: [&str; 6] = ["up", "down", "select", "edit", "clear", "quit"];

fn default_names(action: &str) -> Vec<&'static str> {
    match action {
        "up" => vec!["up", "k"],
        "down" => vec!["down", "j"],
        "select" => vec!["enter"],
        "edit" => vec!["e"],
        "clear" => vec!["0"],
        "quit" => vec!["esc", "q", "ctrl-c"],
        _ => vec![],
    }
}

// One action's keys: the names as configured and the binds they parse to.
pub struct KeySet {
    pub action: &'static str,
    pub names: Vec<String>,
    pub binds: Vec<Bind>,
}

// Resolves the raw [keys] table into a key set for every action, in ACTIONS order. Missing actions
// take the defaults; any error fails the whole resolution, so the popup can fall back to all
// default keys and show the error.
pub fn resolve(raw: Option<&toml::Value>) -> Result<Vec<KeySet>, String> {
    let table = match raw {
        None => None,
        Some(toml::Value::Table(t)) => Some(t),
        Some(v) => {
            return Err(format!(
                "keys must be a table of actions to key names, got {}",
                crate::config::repr(v)
            ))
        }
    };
    if let Some(t) = table {
        for key in t.keys() {
            if !ACTIONS.contains(&key.as_str()) {
                return Err(format!("unknown action {key:?} in keys"));
            }
        }
    }

    let mut sets = Vec::new();
    for action in ACTIONS {
        let names: Vec<String> = match table.and_then(|t| t.get(action)) {
            None => default_names(action)
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Some(toml::Value::String(s)) => vec![s.clone()],
            Some(toml::Value::Array(a)) if !a.is_empty() => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| format!("keys.{action} is not a list of key names"))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => {
                return Err(format!(
                    "keys.{action} must be a key name or a list of key names"
                ))
            }
        };
        let mut binds = Vec::new();
        for name in &names {
            binds.push(parse_key_err(name).map_err(|e| format!("{e} in keys.{action}"))?);
        }
        sets.push(KeySet {
            action,
            names,
            binds,
        });
    }

    // Every bind across all actions must be unique and not another key's byte to a terminal.
    let mut seen: Vec<(&'static str, &str, Bind)> = Vec::new();
    for set in &sets {
        for (name, bind) in set.names.iter().zip(&set.binds) {
            if let Some(other) = ctrl_byte(*bind) {
                return Err(format!(
                    "keys.{} {name:?} is the same key as {other} to a terminal",
                    set.action
                ));
            }
            if let Some((oa, on, _)) = seen.iter().find(|(_, _, b)| b == bind) {
                return Err(format!(
                    "keys.{} {name:?} is the same key as keys.{oa} {on:?}",
                    set.action
                ));
            }
            seen.push((set.action, name, *bind));
        }
    }
    Ok(sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(code: KeyCode, mods: KeyModifiers) -> Option<Bind> {
        Some(Bind { code, mods })
    }

    #[test]
    fn key_names_parse_into_code_and_modifiers() {
        assert_eq!(
            parse_key("ctrl-s"),
            bind(KeyCode::Char('s'), KeyModifiers::CONTROL)
        );
        assert_eq!(parse_key("alt-up"), bind(KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(
            parse_key("ctrl-alt-f5"),
            bind(KeyCode::F(5), KeyModifiers::CONTROL | KeyModifiers::ALT)
        );
        assert_eq!(parse_key("tab").map(|b| b.code), Some(KeyCode::Tab));
        assert_eq!(parse_key("ctrl-nonsense"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn shift_tab_is_backtab_and_an_uppercase_letter_is_a_shift_bind() {
        assert_eq!(
            parse_key("shift-tab"),
            bind(KeyCode::BackTab, KeyModifiers::NONE)
        );
        assert_eq!(parse_key("alt-A"), parse_key("alt-shift-a"));
        let want = parse_key("alt-A");
        assert_eq!(
            normalize(KeyCode::Char('a'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            want.unwrap_or_else(|| panic!("alt-A"))
        );
        for bad in ["shift-1", "alt-shift-/"] {
            let err = parse_key_err(bad).err().unwrap_or_default();
            assert!(err.contains("shift only combines"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_defaults_resolve_for_every_action() {
        let sets = resolve(None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sets.len(), ACTIONS.len());
        assert_eq!(sets[0].action, "up");
        assert_eq!(sets[0].names, ["up", "k"]);
        assert_eq!(sets[5].names, ["esc", "q", "ctrl-c"]);
    }

    #[test]
    fn a_single_name_and_a_list_of_names_are_both_accepted() {
        let keys = toml::from_str::<toml::Table>(
            "[keys]\nup = \"ctrl-p\"\ndown = [\"ctrl-n\", \"alt-j\"]",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let sets = resolve(keys.get("keys")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sets[0].names, ["ctrl-p"]);
        assert_eq!(sets[1].names, ["ctrl-n", "alt-j"]);
        assert_eq!(sets[2].names, ["enter"], "select keeps its default");
    }

    #[test]
    fn a_bad_name_duplicate_or_ctrl_byte_fails_the_whole_resolution() {
        let resolve_keys = |text: &str| {
            let keys = toml::from_str::<toml::Table>(text).unwrap_or_else(|e| panic!("{e}"));
            resolve(keys.get("keys")).err().unwrap_or_default()
        };
        let bad_name = resolve_keys("[keys]\nedit = \"ctrl-nonsense\"");
        assert!(
            bad_name.contains("unknown key name \"ctrl-nonsense\" in keys.edit"),
            "{bad_name}"
        );
        let dup = resolve_keys("[keys]\nselect = \"tab\"\nedit = \"tab\"");
        assert!(
            dup.contains("keys.edit \"tab\" is the same key as keys.select"),
            "{dup}"
        );
        let same = resolve_keys("[keys]\nedit = [\"alt-A\", \"alt-shift-a\"]");
        assert!(
            same.contains("keys.edit \"alt-shift-a\" is the same key as keys.edit \"alt-A\""),
            "{same}"
        );
        for (name, other) in [
            ("ctrl-m", "enter"),
            ("ctrl-j", "enter"),
            ("ctrl-i", "tab"),
            ("ctrl-h", "backspace"),
            ("ctrl-[", "esc"),
            ("ctrl-@", "ctrl-space"),
            ("ctrl-M", "enter"),
            ("ctrl-J", "enter"),
            ("ctrl-I", "tab"),
            ("ctrl-H", "backspace"),
        ] {
            let err = resolve_keys(&format!("[keys]\nedit = \"{name}\""));
            assert!(
                err.contains(&format!("keys.edit \"{name}\" is the same key as {other}")),
                "{name}: {err}"
            );
        }
        let not_table = resolve_keys("keys = \"up\"");
        assert!(not_table.contains("keys must be a table"), "{not_table}");
        let bad_action = resolve_keys("[keys]\nleft = \"h\"");
        assert!(
            bad_action.contains("unknown action \"left\""),
            "{bad_action}"
        );
    }
}
