use ratatui::style::Color;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

// The statuses `herdr pane list` reports as agent_status, snake_case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Working,
    Blocked,
    Done,
    Idle,
    Unknown,
}

impl Status {
    pub fn from_name(name: &str) -> Option<Status> {
        match name {
            "working" => Some(Status::Working),
            "blocked" => Some(Status::Blocked),
            "done" => Some(Status::Done),
            "idle" => Some(Status::Idle),
            "unknown" => Some(Status::Unknown),
            _ => None,
        }
    }

    // The one-cell glyph Herdr's Agents view draws for the status, in its ui.status_indicators
    // style.
    pub fn glyph(self, style: Indicator) -> &'static str {
        use Status::*;
        match (style, self) {
            (Indicator::Dots, Working | Blocked | Done) => "●",
            (Indicator::Dots, Idle) => "○",
            (Indicator::Dots, Unknown) => "·",
            (Indicator::Symbols, Blocked) => "×",
            (Indicator::Symbols, Working) => "◐",
            (Indicator::Symbols, Done) => "✓",
            (Indicator::Symbols, Idle) => "○",
            (Indicator::Symbols, Unknown) => "·",
        }
    }

    // The color the status glyph gets when status_colors does not override it: the terminal's
    // basic colors, since Herdr's theme palette is not available to a plugin.
    pub fn color(self) -> Color {
        match self {
            Status::Working => Color::Yellow,
            Status::Blocked => Color::Red,
            Status::Done => Color::Cyan,
            Status::Idle => Color::Green,
            Status::Unknown => Color::DarkGray,
        }
    }
}

// Herdr's ui.status_indicators: "dots" (the default) or "symbols".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Indicator {
    Dots,
    Symbols,
}

// The indicator style in effect: dots unless Herdr's config file says symbols. Every failure —
// no config path, a missing, unreadable or unparsable file, no ui table, no status_indicators
// key, or a value that is not "dots"/"symbols" — falls back to dots without an error.
pub fn indicators() -> Indicator {
    indicators_from(herdr_config_path(&|k| std::env::var(k).ok()).as_deref())
}

// Herdr's config.toml: $HERDR_CONFIG_PATH when set, else <config dir>/herdr/config.toml where
// the config dir is $XDG_CONFIG_HOME/herdr or ~/.config/herdr (Herdr's config_dir() at v0.9.5).
fn herdr_config_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env("HERDR_CONFIG_PATH") {
        return Some(PathBuf::from(p));
    }
    let dir = env("XDG_CONFIG_HOME")
        .map(|d| PathBuf::from(d).join("herdr"))
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config/herdr")))?;
    Some(dir.join("config.toml"))
}

fn indicators_from(path: Option<&Path>) -> Indicator {
    let Some(path) = path else {
        return Indicator::Dots;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Indicator::Dots;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Indicator::Dots;
    };
    match table
        .get("ui")
        .and_then(|u| u.get("status_indicators"))
        .and_then(toml::Value::as_str)
    {
        Some("symbols") => Indicator::Symbols,
        _ => Indicator::Dots,
    }
}

// A status_colors value: "#rrggbb" or a basic color name — the spellings ratatui's Color
// accepts, including the light variants ("light red", "light-red", "bright red", ...). Color
// also parses "reset" and indexed numbers like "10"; neither is a basic color, so both are
// rejected here.
pub fn parse_color(s: &str) -> Option<Color> {
    Color::from_str(s)
        .ok()
        .filter(|c| !matches!(c, Color::Reset | Color::Indexed(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_glyphs_match_herdrs_agents_view() {
        for (style, expected) in [
            (Indicator::Dots, ["●", "●", "●", "○", "·"]),
            (Indicator::Symbols, ["✓", "×", "◐", "○", "·"]),
        ] {
            let got: Vec<&str> = [
                Status::Done,
                Status::Blocked,
                Status::Working,
                Status::Idle,
                Status::Unknown,
            ]
            .iter()
            .map(|s| s.glyph(style))
            .collect();
            assert_eq!(got, expected, "{style:?}");
        }
    }

    #[test]
    fn the_default_status_colors_are_the_basic_terminal_colors() {
        assert_eq!(Status::Working.color(), Color::Yellow);
        assert_eq!(Status::Blocked.color(), Color::Red);
        assert_eq!(Status::Done.color(), Color::Cyan);
        assert_eq!(Status::Idle.color(), Color::Green);
        assert_eq!(Status::Unknown.color(), Color::DarkGray);
    }

    #[test]
    fn only_the_five_status_names_are_statuses() {
        for (name, status) in [
            ("working", Status::Working),
            ("blocked", Status::Blocked),
            ("done", Status::Done),
            ("idle", Status::Idle),
            ("unknown", Status::Unknown),
        ] {
            assert_eq!(Status::from_name(name), Some(status));
        }
        for name in ["", "Working", "WORKING", "paused", "working "] {
            assert_eq!(Status::from_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn a_status_colors_value_is_a_hex_color_or_a_basic_color_name() {
        assert_eq!(parse_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_color("#ABCDEF"), Some(Color::Rgb(171, 205, 239)));
        for (name, color) in [
            ("black", Color::Black),
            ("red", Color::Red),
            ("green", Color::Green),
            ("yellow", Color::Yellow),
            ("blue", Color::Blue),
            ("magenta", Color::Magenta),
            ("cyan", Color::Cyan),
            ("gray", Color::Gray),
            ("darkgray", Color::DarkGray),
            ("white", Color::White),
            ("lightred", Color::LightRed),
            ("light green", Color::LightGreen),
            ("light-blue", Color::LightBlue),
            ("LIGHTYELLOW", Color::LightYellow),
            ("light_magenta", Color::LightMagenta),
            ("bright cyan", Color::LightCyan),
        ] {
            assert_eq!(parse_color(name), Some(color), "{name}");
        }
        for bad in [
            "",
            "not-a-color",
            "#fff",
            "#gg0000",
            "#ff00000",
            "rgb(1,2,3)",
            "10",
            "reset",
        ] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn herdrs_config_path_comes_from_the_env_like_herdr_resolves_it() {
        let get = |k: &str| match k {
            "HERDR_CONFIG_PATH" => Some("/x/herdr.toml".to_string()),
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            herdr_config_path(&get),
            Some(PathBuf::from("/x/herdr.toml")),
            "HERDR_CONFIG_PATH wins"
        );
        let no_override = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            herdr_config_path(&no_override),
            Some(PathBuf::from("/xdg/herdr/config.toml"))
        );
        let no_xdg = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            herdr_config_path(&no_xdg),
            Some(PathBuf::from("/home/u/.config/herdr/config.toml"))
        );
        assert_eq!(herdr_config_path(&|_| None), None);
    }

    #[test]
    fn the_indicator_style_is_dots_unless_herdrs_config_says_symbols() {
        let dir = crate::testutil::unique_dir("ind");
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            fs::write(&path, text).unwrap_or_else(|e| panic!("{e}"));
            path
        };
        assert_eq!(indicators_from(None), Indicator::Dots);
        assert_eq!(
            indicators_from(Some(&dir.join("missing.toml"))),
            Indicator::Dots
        );
        let symbols = write("symbols.toml", "[ui]\nstatus_indicators = \"symbols\"\n");
        assert_eq!(indicators_from(Some(&symbols)), Indicator::Symbols);
        let dots = write("dots.toml", "[ui]\nstatus_indicators = \"dots\"\n");
        assert_eq!(indicators_from(Some(&dots)), Indicator::Dots);
        // Every other outcome is dots: no ui table, no key, a non-string or unknown value,
        // and a file that is not TOML at all.
        for (name, text) in [
            ("nokey.toml", "[ui]\n"),
            ("other.toml", "status_indicators = \"symbols\"\n"),
            ("num.toml", "[ui]\nstatus_indicators = 5\n"),
            ("bogus.toml", "[ui]\nstatus_indicators = \"emoji\"\n"),
            ("broken.toml", "this is = = not toml"),
            ("empty.toml", ""),
        ] {
            let path = write(name, text);
            assert_eq!(indicators_from(Some(&path)), Indicator::Dots, "{name}");
        }
        // A [ui] table nested deeper does not count, and unrelated sections are ignored.
        let nested = write(
            "nested.toml",
            "[ui.status]\nstatus_indicators = \"symbols\"\n[server]\nstatus_indicators = \"symbols\"\n",
        );
        assert_eq!(indicators_from(Some(&nested)), Indicator::Dots);
        let _ = fs::remove_dir_all(&dir);
    }
}
