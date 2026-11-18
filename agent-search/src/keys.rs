use ratatui::crossterm::event::{KeyCode, KeyModifiers};

// A key as written in the config (fzf-style names: ctrl-s, alt-up, alt-0, shift-a).
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

// Modifiers and key names are case-insensitive; a single character keeps its case, and an uppercase
// letter is the shifted key, as crossterm reports it (Char('A') with SHIFT), so `a` and `A` differ.
pub fn parse_key(name: &str) -> Option<Bind> {
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
    let code = match lower.as_str() {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" | "bspace" => KeyCode::Backspace,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" | "page-up" => KeyCode::PageUp,
        "pgdn" | "page-down" => KeyCode::PageDown,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        s => {
            let fkey = s
                .strip_prefix('f')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=12).contains(n));
            let mut chars = rest.chars();
            match (fkey, chars.next(), chars.next()) {
                (Some(n), _, _) => KeyCode::F(n),
                (None, Some(c), None)
                    if c.is_ascii_alphabetic() && mods.contains(KeyModifiers::SHIFT) =>
                {
                    KeyCode::Char(c.to_ascii_uppercase())
                }
                (None, Some(c), None) => {
                    if c.is_ascii_uppercase() {
                        mods |= KeyModifiers::SHIFT;
                    }
                    KeyCode::Char(c)
                }
                _ => return None,
            }
        }
    };
    Some(Bind { code, mods })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_parse_into_code_and_modifiers() {
        assert_eq!(
            parse_key("ctrl-s"),
            Some(Bind {
                code: KeyCode::Char('s'),
                mods: KeyModifiers::CONTROL
            })
        );
        assert_eq!(
            parse_key("alt-up"),
            Some(Bind {
                code: KeyCode::Up,
                mods: KeyModifiers::ALT
            })
        );
        assert_eq!(
            parse_key("Alt-0"),
            Some(Bind {
                code: KeyCode::Char('0'),
                mods: KeyModifiers::ALT
            })
        );
        assert_eq!(
            parse_key("ctrl-alt-f5"),
            Some(Bind {
                code: KeyCode::F(5),
                mods: KeyModifiers::CONTROL | KeyModifiers::ALT
            })
        );
        assert_eq!(parse_key("tab").map(|b| b.code), Some(KeyCode::Tab));
        assert_eq!(parse_key("ctrl-nonsense"), None);
        assert_eq!(parse_key("ctrl-"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn letter_case_is_kept_so_a_and_shift_a_differ() {
        let lower = parse_key("alt-a");
        let upper = parse_key("alt-A");
        assert_ne!(lower, upper);
        assert_eq!(upper, parse_key("alt-shift-a"));
        assert_eq!(
            parse_key("A"),
            Some(Bind {
                code: KeyCode::Char('A'),
                mods: KeyModifiers::SHIFT
            })
        );
        assert_eq!(
            parse_key("a"),
            Some(Bind {
                code: KeyCode::Char('a'),
                mods: KeyModifiers::NONE
            })
        );
    }
}
