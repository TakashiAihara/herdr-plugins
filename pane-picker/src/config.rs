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
    pub priority_source: String,
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
        "priority": {"source": "manual"},
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

fn extract(v: &Value) -> Option<Config> {
    let o = v.as_object()?;
    let keys = o.get("keys")?.as_object()?;
    Some(Config {
        columns: non_empty_strs(o.get("columns")?)?,
        search: non_empty_strs(o.get("search")?)?,
        sorts: named(o.get("sorts")?)?,
        groups: named(o.get("groups")?)?,
        priority_source: o
            .get("priority")?
            .as_object()?
            .get("source")?
            .as_str()?
            .to_string(),
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
    let mut merged = defaults();
    if Path::new(path).is_file() {
        let user = fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(Value::is_object)
            .ok_or_else(|| format!("{path} is not a JSON object"))?;
        merged = merge(merged, user);
    }
    extract(&merged).ok_or_else(|| {
        format!("{path}: columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {{name, by}} with unique names, priority.source a string, and keys sort/group/up/down/clear non-empty strings")
    })
}

// Index of the state's chosen name in the list, 0 when it is missing or unknown.
pub fn pick(names: &[Named], chosen: Option<&Value>) -> usize {
    chosen
        .and_then(Value::as_str)
        .and_then(|s| names.iter().position(|n| n.name == s))
        .unwrap_or(0)
}
