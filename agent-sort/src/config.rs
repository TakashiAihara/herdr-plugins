use serde_json::Value;
use std::fs;
use std::io::ErrorKind;

// The built-in sort fields agent.view.set accepts; `{"token": "<name>"}` sorts by a pane metadata
// token instead.
pub const FIELDS: [&str; 8] = [
    "workspace_order",
    "tab_order",
    "pane_order",
    "attention",
    "status",
    "agent",
    "seen",
    "state_change_seq",
];

#[derive(Clone, PartialEq, Debug)]
pub enum SortKey {
    Token(String),
    Field(String),
}

#[derive(Clone, PartialEq, Debug)]
pub struct SortEntry {
    pub key: SortKey,
    pub desc: bool,
}

impl SortEntry {
    fn field(name: &str, desc: bool) -> SortEntry {
        SortEntry {
            key: SortKey::Field(name.to_string()),
            desc,
        }
    }

    pub fn order(&self) -> &'static str {
        if self.desc {
            "desc"
        } else {
            "asc"
        }
    }

    // `prio desc`, `attention asc`: what the popup shows for one sort entry.
    pub fn describe(&self) -> String {
        let name = match &self.key {
            SortKey::Token(t) => t,
            SortKey::Field(f) => f,
        };
        format!("{name} {}", self.order())
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Preset {
    pub name: String,
    pub sort: Vec<SortEntry>,
    pub filter: Option<Value>,
}

impl Preset {
    // `prio desc, attention desc`: the popup's preview line for a preset.
    pub fn describe(&self) -> String {
        self.sort
            .iter()
            .map(SortEntry::describe)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub struct Config {
    pub default: Option<String>,
    pub presets: Vec<Preset>,
    // The [keys] table is kept raw: only the popup resolves it, so a bad key never stops the
    // other subcommands.
    keys: Option<toml::Value>,
}

impl Config {
    pub fn keys_raw(&self) -> Option<&toml::Value> {
        self.keys.as_ref()
    }

    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.presets.iter().position(|p| p.name == name)
    }

    #[cfg(test)]
    pub fn test(names: &[&str], default: Option<&str>) -> Config {
        Config {
            default: default.map(String::from),
            presets: names
                .iter()
                .map(|n| Preset {
                    name: n.to_string(),
                    sort: vec![],
                    filter: None,
                })
                .collect(),
            keys: None,
        }
    }
}

fn builtins() -> Vec<Preset> {
    let field = SortEntry::field;
    let preset = |name: &str, sort: Vec<SortEntry>| Preset {
        name: name.to_string(),
        sort,
        filter: None,
    };
    vec![
        preset(
            "attention",
            vec![field("attention", true), field("state_change_seq", true)],
        ),
        preset("recent", vec![field("state_change_seq", true)]),
        preset(
            "workspace",
            vec![
                field("workspace_order", false),
                field("tab_order", false),
                field("pane_order", false),
            ],
        ),
    ]
}

fn builtin() -> Config {
    Config {
        default: None,
        presets: builtins(),
        keys: None,
    }
}

// The offending value the way the config wrote it: TOML repr quotes strings ("y", "a b").
pub fn repr(v: &toml::Value) -> String {
    v.to_string()
}

pub fn load(path: &str) -> Result<Config, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(builtin());
        }
        Err(_) => return Err(format!("cannot read {path}")),
    };
    parse(path, &text)
}

fn parse(path: &str, text: &str) -> Result<Config, String> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| format!("{path}: {e}"))?;
    for key in doc.keys() {
        if !["default", "keys", "sort_presets"].contains(&key.as_str()) {
            return Err(format!("{path}: unknown key {key:?}"));
        }
    }

    let default = match doc.get("default") {
        None => None,
        Some(toml::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(v) => {
            return Err(format!(
                "{path}: default must be a preset name, got {}",
                repr(v)
            ))
        }
    };

    let presets = match doc.get("sort_presets") {
        None => builtins(),
        Some(toml::Value::Array(items)) => {
            if items.is_empty() {
                return Err(format!("{path}: sort_presets must not be empty"));
            }
            let presets = items
                .iter()
                .enumerate()
                .map(|(i, v)| preset(path, i + 1, v))
                .collect::<Result<Vec<_>, _>>()?;
            for (i, p) in presets.iter().enumerate() {
                if presets[..i].iter().any(|o| o.name == p.name) {
                    return Err(format!("{path}: duplicate preset name {:?}", p.name));
                }
            }
            presets
        }
        Some(v) => {
            return Err(format!(
                "{path}: sort_presets must be a list of presets, got {}",
                repr(v)
            ))
        }
    };

    if let Some(d) = &default {
        if !presets.iter().any(|p| &p.name == d) {
            return Err(format!("{path}: default {d:?} is not a sort preset"));
        }
    }

    Ok(Config {
        default,
        presets,
        keys: doc.get("keys").cloned(),
    })
}

fn preset(path: &str, i: usize, v: &toml::Value) -> Result<Preset, String> {
    let t = v
        .as_table()
        .ok_or_else(|| format!("{path}: sort preset #{i} is not a table: {}", repr(v)))?;
    for key in t.keys() {
        if !["name", "sort", "filter"].contains(&key.as_str()) {
            return Err(format!("{path}: unknown key {key:?} in sort preset #{i}"));
        }
    }

    let name = match t.get("name") {
        Some(toml::Value::String(s)) if !s.is_empty() => s.clone(),
        Some(v) => {
            return Err(format!(
                "{path}: the preset name must be a non-empty string, got {}",
                repr(v)
            ))
        }
        None => return Err(format!("{path}: sort preset #{i} has no name")),
    };

    let sort = match t.get("sort") {
        Some(toml::Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|e| sort_entry(path, &name, e))
            .collect::<Result<Vec<_>, _>>()?,
        Some(v) => {
            return Err(format!(
                "{path}: preset {name:?} needs a non-empty sort, got {}",
                repr(v)
            ))
        }
        None => return Err(format!("{path}: preset {name:?} has no sort")),
    };

    Ok(Preset {
        name,
        sort,
        filter: t.get("filter").map(to_json),
    })
}

fn sort_entry(path: &str, preset: &str, v: &toml::Value) -> Result<SortEntry, String> {
    let t = v.as_table().ok_or_else(|| {
        format!(
            "{path}: a sort entry of preset {preset:?} is not a table: {}",
            repr(v)
        )
    })?;
    for key in t.keys() {
        if !["token", "field", "order"].contains(&key.as_str()) {
            return Err(format!(
                "{path}: unknown key {key:?} in a sort entry of preset {preset:?}"
            ));
        }
    }

    let key = match (t.get("token"), t.get("field")) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "{path}: a sort entry of preset {preset:?} has both \"token\" and \"field\""
            ))
        }
        (Some(v), None) => SortKey::Token(token(path, preset, v)?),
        (None, Some(v)) => SortKey::Field(field(path, preset, v)?),
        (None, None) => {
            return Err(format!(
                "{path}: a sort entry of preset {preset:?} needs \"token\" or \"field\""
            ))
        }
    };

    let desc = match t.get("order") {
        None => false,
        Some(toml::Value::String(s)) if s == "asc" => false,
        Some(toml::Value::String(s)) if s == "desc" => true,
        Some(v) => {
            return Err(format!(
            "{path}: invalid order {} in a sort entry of preset {preset:?} (\"asc\" or \"desc\")",
            repr(v)
        ))
        }
    };

    Ok(SortEntry { key, desc })
}

// A Herdr token name: 1-32 ASCII letters, digits, `_` or `-`.
fn token(path: &str, preset: &str, v: &toml::Value) -> Result<String, String> {
    match v.as_str() {
        Some(s)
            if (1..=32).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
        {
            Ok(s.to_string())
        }
        _ => Err(format!(
            "{path}: invalid token {} in a sort entry of preset {preset:?} (1-32 letters, digits, _ or -)",
            repr(v)
        )),
    }
}

fn field(path: &str, preset: &str, v: &toml::Value) -> Result<String, String> {
    match v.as_str() {
        Some(s) if FIELDS.contains(&s) => Ok(s.to_string()),
        _ => Err(format!(
            "{path}: unknown field {} in a sort entry of preset {preset:?} (one of: {})",
            repr(v),
            FIELDS.join(" ")
        )),
    }
}

// TOML -> JSON for `filter`, which Herdr takes as-is. A datetime has no JSON form, so it goes as
// its TOML string.
fn to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
        }
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(to_json).collect()),
        toml::Value::Table(t) => {
            Value::Object(t.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}

// Written by `edit` when there is no config file: parses to exactly the built-in presets.
pub const TEMPLATE: &str = r#"# agent-sort configuration.
#
# default = "<name>": the preset `restore` applies on startup when the state
# file does not name one. Uncomment to use.
#
# default = "attention"
#
# [keys]: keys of the preset picker popup. Each action takes a key name or a
# list of key names (fzf-style: ctrl-c, alt-x, shift-tab, f2, or a plain
# character). The values below are the defaults.
#
# [keys]
# up = ["up", "k"]
# down = ["down", "j"]
# select = ["enter"]
# edit = ["e"]
# clear = ["0"]
# quit = ["esc", "q", "ctrl-c"]
#
# A [[sort_presets]] entry has a unique name and a non-empty sort list. Every
# sort entry takes exactly one of:
#
#   token = "prio"   a pane metadata token (reported with
#                    `herdr pane report-metadata <pane> --token prio=3`)
#   field = "agent"  a built-in field: workspace_order, tab_order, pane_order,
#                    attention, status, agent, seen, state_change_seq
#
# and an optional order = "asc" | "desc" (default "asc"). Token values compare
# as strings, so zero-pad numbers: "03" sorts before "10".
#
# An optional filter table is sent to Herdr as-is, for example:
# filter = { op = "in", field = "status", values = ["blocked", "done"] }

[[sort_presets]]
name = "attention"
sort = [
  { field = "attention", order = "desc" },
  { field = "state_change_seq", order = "desc" },
]

[[sort_presets]]
name = "recent"
sort = [{ field = "state_change_seq", order = "desc" }]

[[sort_presets]]
name = "workspace"
sort = [
  { field = "workspace_order" },
  { field = "tab_order" },
  { field = "pane_order" },
]
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err(text: &str) -> String {
        parse("config.toml", text).err().unwrap_or_default()
    }

    #[test]
    fn a_missing_config_file_gives_the_builtins() {
        let dir = crate::testutil::unique_dir("missing");
        let cfg =
            load(&dir.join("config.toml").to_string_lossy()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.presets, builtins());
        assert_eq!(cfg.default, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_builtin_presets_match_the_documented_order() {
        let names: Vec<String> = builtins().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["attention", "recent", "workspace"]);
        assert_eq!(
            builtins()[0].describe(),
            "attention desc, state_change_seq desc"
        );
        assert_eq!(builtins()[1].describe(), "state_change_seq desc");
        assert_eq!(
            builtins()[2].describe(),
            "workspace_order asc, tab_order asc, pane_order asc"
        );
    }

    #[test]
    fn the_template_parses_to_exactly_the_builtins() {
        let cfg = parse("config.toml", TEMPLATE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.presets, builtins());
        assert_eq!(cfg.default, None);
        assert!(TEMPLATE.lines().any(|l| l.starts_with('#')));
    }

    #[test]
    fn a_config_replaces_the_builtins_and_mixes_tokens_fields_and_filter() {
        let cfg = parse(
            "config.toml",
            r#"
default = "prio"

[[sort_presets]]
name = "prio"
sort = [
  { token = "prio", order = "desc" },
  { field = "attention" },
]
filter = { op = "in", field = "status", values = ["blocked", "done"] }

[[sort_presets]]
name = "repo"
sort = [{ token = "repo" }]
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.default.as_deref(), Some("prio"));
        let names: Vec<&str> = cfg.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["prio", "repo"]);
        assert_eq!(cfg.presets[0].describe(), "prio desc, attention asc");
        assert_eq!(
            cfg.presets[0].filter,
            Some(json!({"op": "in", "field": "status", "values": ["blocked", "done"]}))
        );
        assert_eq!(cfg.presets[1].filter, None);
    }

    #[test]
    fn every_validation_error_names_the_file_and_the_value() {
        for (text, needle) in [
            (
                "this is = = not toml",
                "config.toml",
            ),
            (
                "default = \"y\"\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]",
                "default \"y\"",
            ),
            (
                "default = 5\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]",
                "default must be a preset name",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"nope\" }]",
                "unknown field \"nope\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = []",
                "preset \"x\" needs a non-empty sort",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"seen\" }]",
                "duplicate preset name \"x\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ token = \"a b\" }]",
                "invalid token \"a b\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\", order = \"down\" }]",
                "invalid order \"down\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\", token = \"prio\" }]",
                "both \"token\" and \"field\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{}]",
                "needs \"token\" or \"field\"",
            ),
            (
                "[[sort_presets]]\nsort = [{ field = \"agent\" }]",
                "has no name",
            ),
            (
                "[[sort_presets]]\nname = \"\"\nsort = [{ field = \"agent\" }]",
                "non-empty string, got \"\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\", bogus = 1 }]",
                "unknown key \"bogus\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]\nbogus = 1",
                "unknown key \"bogus\"",
            ),
            ("sort_presets = []", "sort_presets must not be empty"),
            ("bogus = 1", "unknown key \"bogus\""),
        ] {
            let e = err(text);
            assert!(
                e.starts_with("config.toml") && e.contains(needle),
                "{text:?}: {e}"
            );
        }
    }

    #[test]
    fn a_valid_config_loads() {
        for text in [
            "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]",
            "[[sort_presets]]\nname = \"x\"\nsort = [{ token = \"a0_-Z\" }]",
            "[keys]\nedit = \"ctrl-nonsense\"\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]",
        ] {
            parse("config.toml", text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        }
    }

    #[test]
    fn a_config_without_sort_presets_keeps_the_builtins() {
        let cfg = parse(
            "config.toml",
            "default = \"recent\"\n[keys]\nup = \"ctrl-p\"",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.presets, builtins());
        assert_eq!(cfg.default.as_deref(), Some("recent"));
    }
}
