use serde_json::{Map, Value};
use std::fs;
use std::io::ErrorKind;

// Each scope searches everything the one before it does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scope {
    Meta,
    User,
    Assistant,
    Tools,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::Meta, Scope::User, Scope::Assistant, Scope::Tools];

    pub fn label(self) -> &'static str {
        match self {
            Scope::Meta => "meta",
            Scope::User => "+user",
            Scope::Assistant => "+assistant",
            Scope::Tools => "+tools",
        }
    }

    // How many body files (user, assistant, tools, in that order) the scope reads.
    pub fn tiers(self) -> usize {
        self as usize
    }

    pub fn step(self, delta: isize) -> Scope {
        let n = Scope::ALL.len();
        Scope::ALL[(self as usize + n).wrapping_add_signed(delta) % n]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Show {
    All,
    Unmarked,
    Marked,
}

impl Show {
    pub fn label(self) -> &'static str {
        match self {
            Show::All => "all",
            Show::Unmarked => "unmarked",
            Show::Marked => "marked",
        }
    }

    pub fn next(self) -> Show {
        match self {
            Show::All => Show::Unmarked,
            Show::Unmarked => Show::Marked,
            Show::Marked => Show::All,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Config {
    pub resume: String,
    pub projects: String,
    // A path template; `{id}` becomes the session id.
    pub marker: Option<String>,
    pub show: Show,
    pub scope: Scope,
}

// What the defaults and `~/` need from the environment, passed in so a test never reads it.
pub struct Defaults<'a> {
    pub home: &'a str,
    pub claude_config_dir: Option<&'a str>,
}

fn default_config(d: &Defaults) -> Config {
    Config {
        resume: "claude --resume {id}".into(),
        projects: match d.claude_config_dir.filter(|c| !c.is_empty()) {
            Some(dir) => format!("{dir}/projects"),
            None => format!("{}/.claude/projects", d.home),
        },
        marker: None,
        show: Show::All,
        scope: Scope::Meta,
    }
}

// A missing file is the default; a file that exists but is unusable is an error, never a fallback.
pub fn load(path: &str, d: &Defaults) -> Result<Config, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(default_config(d)),
        Err(e) => return Err(format!("cannot read {path}: {e}")),
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))?;
    parse(&value, d).map_err(|e| format!("{path}: {e}"))
}

fn text<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s)),
        Some(_) => Err(format!("{key} must be a non-empty string")),
    }
}

fn expand(s: &str, key: &str, home: &str) -> Result<String, String> {
    match s.strip_prefix("~/") {
        None => Ok(s.to_string()),
        Some(_) if home.is_empty() => Err(format!("{key} starts with ~/ but HOME is not set")),
        Some(rest) => Ok(format!("{home}/{rest}")),
    }
}

fn choice<T: Copy>(
    obj: &Map<String, Value>,
    key: &str,
    options: &[(&str, T)],
) -> Result<Option<T>, String> {
    let Some(s) = text(obj, key)? else {
        return Ok(None);
    };
    match options.iter().find(|(name, _)| *name == s) {
        Some((_, v)) => Ok(Some(*v)),
        None => {
            let names: Vec<_> = options.iter().map(|(n, _)| *n).collect();
            Err(format!(
                "{key} must be one of {}, not {s:?}",
                names.join(", ")
            ))
        }
    }
}

fn parse(value: &Value, d: &Defaults) -> Result<Config, String> {
    let obj = value.as_object().ok_or("not a JSON object")?;
    if let Some(k) = obj
        .keys()
        .find(|k| !["resume", "projects", "marker", "show", "scope"].contains(&k.as_str()))
    {
        return Err(format!("unknown key {k:?} in the top level"));
    }
    let mut cfg = default_config(d);
    if let Some(s) = text(obj, "resume")? {
        cfg.resume = s.to_string();
    }
    if let Some(s) = text(obj, "projects")? {
        cfg.projects = expand(s, "projects", d.home)?;
    }
    if let Some(s) = text(obj, "marker")? {
        if !s.contains("{id}") {
            return Err("marker must contain {id}".into());
        }
        cfg.marker = Some(expand(s, "marker", d.home)?);
    }
    let show = choice(
        obj,
        "show",
        &[
            ("all", Show::All),
            ("unmarked", Show::Unmarked),
            ("marked", Show::Marked),
        ],
    )?;
    if let Some(s) = show {
        if s != Show::All && cfg.marker.is_none() {
            return Err(format!("show {:?} needs a marker", s.label()));
        }
        cfg.show = s;
    }
    let scopes = Scope::ALL.map(|s| (s.label().trim_start_matches('+'), s));
    if let Some(s) = choice(obj, "scope", &scopes)? {
        cfg.scope = s;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::unique_dir;

    const D: Defaults = Defaults {
        home: "/home/u",
        claude_config_dir: None,
    };

    fn load_text(name: &str, body: &str, d: &Defaults) -> Result<Config, String> {
        let dir = unique_dir(name);
        let path = dir.join("config.json");
        fs::write(&path, body).unwrap();
        let r = load(path.to_str().unwrap(), d);
        let _ = fs::remove_dir_all(dir);
        r
    }

    #[test]
    fn a_missing_file_is_the_claude_resume_default_under_home() {
        let dir = unique_dir("missing");
        let cfg = load(dir.join("config.json").to_str().unwrap(), &D).unwrap();
        let _ = fs::remove_dir_all(dir);
        assert_eq!(cfg.resume, "claude --resume {id}");
        assert_eq!(cfg.projects, "/home/u/.claude/projects");
        assert_eq!(cfg.marker, None);
        assert_eq!((cfg.show, cfg.scope), (Show::All, Scope::Meta));
    }

    #[test]
    fn claude_config_dir_moves_the_default_projects_unless_it_is_empty() {
        let d = Defaults {
            home: "/home/u",
            claude_config_dir: Some("/cc/acct"),
        };
        assert_eq!(default_config(&d).projects, "/cc/acct/projects");
        let d = Defaults {
            home: "/home/u",
            claude_config_dir: Some(""),
        };
        assert_eq!(default_config(&d).projects, "/home/u/.claude/projects");
    }

    #[test]
    fn every_key_is_read_and_a_leading_tilde_slash_becomes_home() {
        let cfg = load_text(
            "ok",
            r#"{"resume":"my-resume {id}","projects":"~/p/projects",
                "marker":"~/.claude/sessions/{id}/archived","show":"unmarked","scope":"tools"}"#,
            &D,
        )
        .unwrap();
        assert_eq!(cfg.resume, "my-resume {id}");
        assert_eq!(cfg.projects, "/home/u/p/projects");
        assert_eq!(
            cfg.marker.as_deref(),
            Some("/home/u/.claude/sessions/{id}/archived")
        );
        assert_eq!((cfg.show, cfg.scope), (Show::Unmarked, Scope::Tools));
    }

    #[test]
    fn a_tilde_that_is_not_a_leading_tilde_slash_is_kept() {
        let cfg = load_text("tilde", r#"{"projects":"/a/~/b"}"#, &D).unwrap();
        assert_eq!(cfg.projects, "/a/~/b");
        let cfg = load_text("tilde2", r#"{"projects":"~x/b"}"#, &D).unwrap();
        assert_eq!(cfg.projects, "~x/b");
    }

    #[test]
    fn every_kind_of_invalid_file_is_an_error_naming_the_problem() {
        let cases = [
            ("{nope", "is not valid JSON"),
            ("[]", "not a JSON object"),
            (
                r#"{"agents":[]}"#,
                r#"unknown key "agents" in the top level"#,
            ),
            (r#"{"resume":5}"#, "resume must be a non-empty string"),
            (r#"{"resume":" "}"#, "resume must be a non-empty string"),
            (r#"{"projects":[]}"#, "projects must be a non-empty string"),
            (r#"{"marker":"/m/archived"}"#, "marker must contain {id}"),
            (r#"{"marker":""}"#, "marker must be a non-empty string"),
            (r#"{"show":"marked"}"#, r#"show "marked" needs a marker"#),
            (
                r#"{"show":"unmarked"}"#,
                r#"show "unmarked" needs a marker"#,
            ),
            (
                r#"{"marker":"/m/{id}","show":"some"}"#,
                r#"show must be one of all, unmarked, marked, not "some""#,
            ),
            (
                r#"{"scope":"+user"}"#,
                r#"scope must be one of meta, user, assistant, tools, not "+user""#,
            ),
            (r#"{"scope":3}"#, "scope must be a non-empty string"),
        ];
        for (body, want) in cases {
            let err = load_text("bad", body, &D).unwrap_err();
            assert!(err.contains(want), "{body}: {err}");
            assert!(err.contains("config.json"), "{body}: {err}");
        }
    }

    #[test]
    fn show_all_is_allowed_without_a_marker() {
        let cfg = load_text("all", r#"{"show":"all"}"#, &D).unwrap();
        assert_eq!(cfg.show, Show::All);
    }

    #[test]
    fn a_tilde_path_without_home_is_an_error() {
        let d = Defaults {
            home: "",
            claude_config_dir: None,
        };
        let err = load_text("nohome", r#"{"projects":"~/p"}"#, &d).unwrap_err();
        assert!(
            err.contains("projects starts with ~/ but HOME is not set"),
            "{err}"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_error_not_the_default() {
        let dir = unique_dir("unreadable");
        let err = load(dir.to_str().unwrap(), &D).unwrap_err();
        let _ = fs::remove_dir_all(dir);
        assert!(err.contains("cannot read"), "{err}");
    }

    #[test]
    fn scopes_and_filters_cycle_in_both_directions() {
        assert_eq!(Scope::Meta.step(1), Scope::User);
        assert_eq!(Scope::Tools.step(1), Scope::Meta);
        assert_eq!(Scope::Meta.step(-1), Scope::Tools);
        assert_eq!(Scope::Assistant.step(-1), Scope::User);
        assert_eq!(
            [Scope::Meta, Scope::User, Scope::Assistant, Scope::Tools].map(Scope::tiers),
            [0, 1, 2, 3]
        );
        assert_eq!(Show::All.next(), Show::Unmarked);
        assert_eq!(Show::Unmarked.next(), Show::Marked);
        assert_eq!(Show::Marked.next(), Show::All);
    }
}
