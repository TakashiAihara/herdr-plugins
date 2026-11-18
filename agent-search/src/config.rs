use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub struct Named {
    pub name: String,
    pub by: Vec<String>,
}

pub struct Keys {
    pub sort: String,
    pub group: String,
    pub up: String,
    pub down: String,
    pub clear: String,
}

pub struct Config {
    pub columns: Vec<String>,
    pub search: Vec<String>,
    pub sorts: Vec<Named>,
    pub groups: Vec<Named>,
    pub priority_token: String,
    pub keys: Keys,
}

fn defaults() -> Value {
    json!({
        "columns": ["prio", "agent_status", "title"],
        "search": ["title", "cwd", "agent", "agent_status", "terminal_title_stripped", "tokens.*"],
        "sorts": [
            {"name": "priority", "by": ["prio:desc", "manual"]},
            {"name": "manual", "by": ["manual"]},
            {"name": "status", "by": ["agent_status", "manual"]}
        ],
        "groups": [
            {"name": "none", "by": []},
            {"name": "repo", "by": ["tokens.repo", "cwd:basename"]},
            {"name": "workspace", "by": ["workspace_id"]}
        ],
        "priority_token": "prio",
        "keys": {"sort": "ctrl-s", "group": "ctrl-g", "up": "alt-up", "down": "alt-down", "clear": "alt-0"}
    })
}

// Same as jq `*`: objects merge key by key, anything else on the right replaces.
fn merge(d: Value, u: Value) -> Value {
    match (d, u) {
        (Value::Object(mut d), Value::Object(u)) => {
            for (k, v) in u {
                match d.get_mut(&k) {
                    Some(dv) => {
                        let old = std::mem::take(dv);
                        *dv = merge(old, v);
                    }
                    None => {
                        d.insert(k, v);
                    }
                }
            }
            Value::Object(d)
        }
        (_, u) => u,
    }
}

fn strs(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
        .iter()
        .map(|x| x.as_str().filter(|s| !s.is_empty()).map(String::from))
        .collect()
}

fn non_empty_strs(v: &Value) -> Option<Vec<String>> {
    strs(v).filter(|a| !a.is_empty())
}

fn named(v: &Value) -> Option<Vec<Named>> {
    let items = v.as_array()?;
    if items.is_empty() {
        return None;
    }
    let mut out: Vec<Named> = Vec::new();
    for item in items {
        let o = item.as_object()?;
        let name = o.get("name")?.as_str().filter(|s| !s.is_empty())?;
        let by = strs(o.get("by")?)?;
        if out.iter().any(|n| n.name == name) {
            return None;
        }
        out.push(Named {
            name: name.to_string(),
            by,
        });
    }
    Some(out)
}

fn key(keys: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    keys.get(name)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// A Herdr token name: 1-32 ASCII letters, digits, `_` or `-`.
fn token_name(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| (1..=32).contains(&s.len()))
        .filter(|s| {
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map(String::from)
}

fn extract(v: &Value) -> Option<Config> {
    let o = v.as_object()?;
    let keys = o.get("keys")?.as_object()?;
    Some(Config {
        columns: non_empty_strs(o.get("columns")?)?,
        search: non_empty_strs(o.get("search")?)?,
        sorts: named(o.get("sorts")?)?,
        groups: named(o.get("groups")?)?,
        priority_token: token_name(o.get("priority_token")?)?,
        keys: Keys {
            sort: key(keys, "sort")?,
            group: key(keys, "group")?,
            up: key(keys, "up")?,
            down: key(keys, "down")?,
            clear: key(keys, "clear")?,
        },
    })
}

pub fn load(path: &str) -> Result<Config, String> {
    load_with(path, true)
}

// `restore` never binds keys: it only needs `priority_token` and the settings that build rows, so
// a config whose `keys` are unusable or invalid still restores priorities. Its keys are replaced
// by the defaults before extraction; every other error still fails.
pub fn load_for_restore(path: &str) -> Result<Config, String> {
    load_with(path, false)
}

fn load_with(path: &str, need_keys: bool) -> Result<Config, String> {
    let mut merged = defaults();
    if Path::new(path).is_file() {
        let user = fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(Value::is_object)
            .ok_or_else(|| format!("{path} is not a JSON object"))?;
        merged = merge(merged, user);
    }
    if !need_keys {
        if let Value::Object(m) = &mut merged {
            m.insert("keys".into(), defaults()["keys"].clone());
        }
    }
    let cfg = extract(&merged).ok_or_else(|| {
        let fields = "columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {name, by} with unique names, priority_token a Herdr token name (1-32 letters, digits, _ or -)";
        if need_keys {
            format!("{path}: {fields}, and keys sort/group/up/down/clear non-empty strings")
        } else {
            format!("{path}: {fields}")
        }
    })?;
    validate(&cfg).map_err(|e| format!("{path}: {e}"))?;
    Ok(cfg)
}

// A field path has no empty part, so `title.` or `a..b` is a mistake in the config rather than
// something to read as an empty value on every pane.
fn check_field(spec: &str) -> Result<(), String> {
    let path = spec.split(':').next().unwrap_or("");
    if path.split('.').all(|part| !part.is_empty()) {
        Ok(())
    } else {
        Err(format!(
            "invalid field {spec:?}: its path has an empty part"
        ))
    }
}

fn validate(cfg: &Config) -> Result<(), String> {
    for spec in cfg.columns.iter().chain(&cfg.search) {
        check_field(spec)?;
    }
    for spec in cfg.groups.iter().flat_map(|g| &g.by) {
        check_field(spec)?;
    }
    for spec in cfg.sorts.iter().flat_map(|s| &s.by) {
        check_field(spec.strip_suffix(":desc").unwrap_or(spec))?;
    }
    crate::keys::validate(&cfg.keys)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_json(json: &str) -> Result<Config, String> {
        load_json_with(json, load)
    }

    fn load_json_with(json: &str, f: fn(&str) -> Result<Config, String>) -> Result<Config, String> {
        let dir = crate::testutil::unique_dir("cfg");
        let file = dir.join("config.json");
        fs::write(&file, json).unwrap_or_else(|e| panic!("{e}"));
        let loaded = f(&file.to_string_lossy());
        let _ = fs::remove_dir_all(&dir);
        loaded
    }

    #[test]
    fn an_empty_part_in_a_field_path_is_a_config_error() {
        for bad in [
            r#"{"columns":["title."]}"#,
            r#"{"search":["a..b"]}"#,
            r#"{"groups":[{"name":"g","by":[".repo"]}]}"#,
            r#"{"sorts":[{"name":"s","by":[".:desc"]}]}"#,
        ] {
            let err = load_json(bad).err().unwrap_or_default();
            assert!(err.contains("invalid field"), "{bad}: {err}");
        }
        assert!(load_json(r#"{"columns":["tokens.repo","cwd:basename"]}"#).is_ok());
    }

    #[test]
    fn an_unknown_key_name_is_a_config_error_naming_the_key() {
        let err = load_json(r#"{"keys":{"up":"ctrl-nonsense"}}"#)
            .err()
            .unwrap_or_default();
        assert!(
            err.contains("unknown key name \"ctrl-nonsense\" in keys.up"),
            "{err}"
        );
        assert!(load_json(r#"{"keys":{"sort":"alt-A"}}"#).is_ok());
    }

    #[test]
    fn restore_needs_no_valid_keys_but_the_rest_of_the_config_still_applies() {
        for keys in [
            r#"{"keys":{"sort":"x"}}"#,
            r#"{"keys":{"up":"ctrl-nonsense"}}"#,
            r#"{"keys":{"sort":"shift-1"}}"#,
            r#"{"keys":{"up":5}}"#,
            r#"{"keys":[]}"#,
        ] {
            assert!(load_json_with(keys, load_for_restore).is_ok(), "{keys}");
            assert!(load_json(keys).is_err(), "{keys}");
        }
        for bad in [
            r#"{"columns":"title"}"#,
            r#"{"sorts":[]}"#,
            r#"{"priority_token":"a b"}"#,
            r#"{"columns":["title."]}"#,
        ] {
            assert!(load_json_with(bad, load_for_restore).is_err(), "{bad}");
        }
    }
}

// Index of the state's chosen name in the list, 0 when it is missing or unknown.
pub fn pick(names: &[Named], chosen: Option<&Value>) -> usize {
    chosen
        .and_then(Value::as_str)
        .and_then(|s| names.iter().position(|n| n.name == s))
        .unwrap_or(0)
}
