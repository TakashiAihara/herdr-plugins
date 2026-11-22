use serde_json::{Map, Value};
use std::fs;
use std::io::ErrorKind;

#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub name: String,
    pub command: String,
}

#[derive(Debug, PartialEq)]
pub struct Config {
    // Never empty; the first agent is the default.
    pub agents: Vec<Agent>,
    // A shell command that prints one repo per line.
    pub repos: Option<String>,
}

impl Config {
    // Only a repo-taking launcher needs the repo step.
    pub fn wants_repo(&self) -> bool {
        self.agents.iter().any(|a| a.command.contains("{repo}"))
    }
}

fn default_config() -> Config {
    Config {
        agents: vec![Agent {
            name: "claude".into(),
            command: "claude -- {prompt}".into(),
        }],
        repos: None,
    }
}

// A missing file is the default; a file that exists but is unusable is an error, never a fallback.
pub fn load(path: &str) -> Result<Config, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(default_config()),
        Err(e) => return Err(format!("cannot read {path}: {e}")),
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))?;
    parse(&value).map_err(|e| format!("{path}: {e}"))
}

fn only_keys(obj: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("unknown key {k:?} in {what}")),
        None => Ok(()),
    }
}

fn text<'a>(obj: &'a Map<String, Value>, key: &str, what: &str) -> Result<&'a str, String> {
    match obj.get(key) {
        None => Err(format!("{what} has no {key}")),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s),
        Some(_) => Err(format!("{what}: {key} must be a non-empty string")),
    }
}

fn parse(value: &Value) -> Result<Config, String> {
    let obj = value.as_object().ok_or("not a JSON object")?;
    only_keys(obj, &["agents", "repos"], "the top level")?;
    let mut cfg = default_config();
    if let Some(agents) = obj.get("agents") {
        let list = agents
            .as_array()
            .filter(|l| !l.is_empty())
            .ok_or("agents must be a non-empty list")?;
        cfg.agents = list
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let what = format!("agents[{i}]");
                let a = a.as_object().ok_or(format!("{what} is not an object"))?;
                only_keys(a, &["name", "command"], &what)?;
                Ok(Agent {
                    name: text(a, "name", &what)?.trim().to_string(),
                    command: text(a, "command", &what)?.to_string(),
                })
            })
            .collect::<Result<_, String>>()?;
    }
    if let Some(repos) = obj.get("repos") {
        match repos {
            Value::String(s) if !s.trim().is_empty() => cfg.repos = Some(s.clone()),
            _ => return Err("repos must be a non-empty string (a shell command)".into()),
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::unique_dir;

    fn load_text(name: &str, body: &str) -> Result<Config, String> {
        let dir = unique_dir(name);
        let path = dir.join("config.json");
        fs::write(&path, body).unwrap();
        let r = load(path.to_str().unwrap());
        let _ = fs::remove_dir_all(dir);
        r
    }

    fn err_of(name: &str, body: &str) -> String {
        load_text(name, body).unwrap_err()
    }

    #[test]
    fn a_missing_file_is_the_claude_default_with_no_repos() {
        let dir = unique_dir("missing");
        let cfg = load(dir.join("config.json").to_str().unwrap()).unwrap();
        let _ = fs::remove_dir_all(dir);
        assert_eq!(cfg, default_config());
        assert_eq!(cfg.agents[0].command, "claude -- {prompt}");
        assert_eq!(cfg.repos, None);
        assert!(!cfg.wants_repo());
    }

    #[test]
    fn agents_and_repos_are_read_in_order() {
        let cfg = load_text(
            "ok",
            r#"{"agents":[{"name":"claude","command":"ccxn {repo} {prompt}"},
                          {"name":"devin","command":"ccxn --agent devin {repo} {prompt}"}],
                "repos":"ghq list"}"#,
        )
        .unwrap();
        let names: Vec<_> = cfg.agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["claude", "devin"]);
        assert_eq!(cfg.agents[1].command, "ccxn --agent devin {repo} {prompt}");
        assert_eq!(cfg.repos.as_deref(), Some("ghq list"));
        assert!(cfg.wants_repo());
    }

    #[test]
    fn a_file_with_only_repos_keeps_the_default_agent() {
        let cfg = load_text("only-repos", r#"{"repos":"ghq list"}"#).unwrap();
        assert_eq!(cfg.agents, default_config().agents);
    }

    #[test]
    fn the_repo_step_is_wanted_when_any_agent_has_the_placeholder() {
        let cfg = load_text(
            "any",
            r#"{"agents":[{"name":"a","command":"a {prompt}"},{"name":"b","command":"b {repo}"}]}"#,
        )
        .unwrap();
        assert!(cfg.wants_repo());
    }

    #[test]
    fn every_kind_of_invalid_file_is_an_error_naming_the_problem() {
        let cases = [
            ("{nope", "is not valid JSON"),
            ("[]", "not a JSON object"),
            (
                r#"{"command":"x"}"#,
                r#"unknown key "command" in the top level"#,
            ),
            (r#"{"agents":[]}"#, "agents must be a non-empty list"),
            (r#"{"agents":{}}"#, "agents must be a non-empty list"),
            (r#"{"agents":["x"]}"#, "agents[0] is not an object"),
            (r#"{"agents":[{"command":"c"}]}"#, "agents[0] has no name"),
            (r#"{"agents":[{"name":"n"}]}"#, "agents[0] has no command"),
            (
                r#"{"agents":[{"name":"  ","command":"c"}]}"#,
                "agents[0]: name must be a non-empty string",
            ),
            (
                r#"{"agents":[{"name":"n","command":5}]}"#,
                "agents[0]: command must be a non-empty string",
            ),
            (
                r#"{"agents":[{"name":"ok","command":"c"},{"name":"n","command":"c","cwd":"/"}]}"#,
                r#"unknown key "cwd" in agents[1]"#,
            ),
            (r#"{"repos":5}"#, "repos must be a non-empty string"),
            (r#"{"repos":" "}"#, "repos must be a non-empty string"),
        ];
        for (body, want) in cases {
            let err = err_of("bad", body);
            assert!(err.contains(want), "{body}: {err}");
            assert!(err.contains("config.json"), "{body}: {err}");
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_error_not_the_default() {
        let dir = unique_dir("unreadable");
        let err = load(dir.to_str().unwrap()).unwrap_err();
        let _ = fs::remove_dir_all(dir);
        assert!(err.contains("cannot read"), "{err}");
    }
}
