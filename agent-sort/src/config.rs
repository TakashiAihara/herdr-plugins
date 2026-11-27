use serde_json::Value;
use std::fs;
use std::io::ErrorKind;

// The parts of agent.view.set's input contract checked at load — preset name length (it is sent
// as the label, MAX_LABEL_CHARS) and the sort entry count (MAX_SORT_FIELDS) — so a too-big config
// is a config error, not a rejected request at apply time. Herdr's filter depth and node limits
// are left to it.
const MAX_NAME_CHARS: usize = 32;
const MAX_SORT_ENTRIES: usize = 8;

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

    // The name to look up for a name from a config default or `set`. "attention" and "workspace"
    // were built-in presets: the first sorted exactly like Herdr's "priority", the second by tab
    // and pane number, the layout order "spaces" shows up to panes split out of creation order.
    // A config could name them as the default without defining them; where no preset carries the
    // name it means Herdr's sort. A saved state is not read this way: an old name there goes the
    // way of any preset gone from the config.
    pub fn resolve<'a>(&self, name: &'a str) -> &'a str {
        let herdr = match name {
            "attention" => "priority",
            "workspace" => "spaces",
            _ => return name,
        };
        if self.preset(name).is_none() {
            herdr
        } else {
            name
        }
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

// Herdr's own two orders (`ui.agent_panel_sort`), always first in the list: a config replaces the
// presets after them, never these. Each is the view that reproduces Herdr's order whatever the
// config says (Herdr 0.9.5 `apply_agent_view`): "priority" is Herdr's own two keys, and "spaces"
// is the order agents are collected in — workspaces, then tabs, then panes by their place in the
// layout — which a stable sort on the workspace alone leaves as it is. Adding `tab_order` and
// `pane_order` would not be the same order: a pane's number is the order it was created in, so a
// pane split off an earlier one sits before later-numbered panes on screen and in "spaces".
fn herdr_sorts() -> Vec<Preset> {
    let field = SortEntry::field;
    let preset = |name: &str, sort: Vec<SortEntry>| Preset {
        name: name.to_string(),
        sort,
        filter: None,
    };
    vec![
        preset("spaces", vec![field("workspace_order", false)]),
        preset(
            "priority",
            vec![field("attention", true), field("state_change_seq", true)],
        ),
    ]
}

// Herdr's sorts, then the given presets.
fn listed(rest: Vec<Preset>) -> Vec<Preset> {
    let mut all = herdr_sorts();
    all.extend(rest);
    all
}

fn builtins() -> Vec<Preset> {
    let field = SortEntry::field;
    let preset = |name: &str, sort: Vec<SortEntry>| Preset {
        name: name.to_string(),
        sort,
        filter: None,
    };
    vec![preset("recent", vec![field("state_change_seq", true)])]
}

fn builtin() -> Config {
    Config {
        default: None,
        presets: listed(builtins()),
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
        None => listed(builtins()),
        Some(toml::Value::Array(items)) => {
            let presets = items
                .iter()
                .enumerate()
                .map(|(i, v)| preset(path, i + 1, v))
                .collect::<Result<Vec<_>, _>>()?;
            for (i, p) in presets.iter().enumerate() {
                if herdr_sorts().iter().any(|h| h.name == p.name) {
                    return Err(format!(
                        "{path}: preset name {:?} is taken by Herdr's own sort, which is always listed; rename the preset",
                        p.name
                    ));
                }
                if presets[..i].iter().any(|o| o.name == p.name) {
                    return Err(format!("{path}: duplicate preset name {:?}", p.name));
                }
            }
            listed(presets)
        }
        Some(v) => {
            return Err(format!(
                "{path}: sort_presets must be a list of presets, got {}",
                repr(v)
            ))
        }
    };

    let mut cfg = Config {
        default: None,
        presets,
        keys: doc.get("keys").cloned(),
    };
    if let Some(d) = default {
        let d = cfg.resolve(&d).to_string();
        if cfg.preset(&d).is_none() {
            return Err(format!("{path}: default {d:?} is not a sort preset"));
        }
        cfg.default = Some(d);
    }
    Ok(cfg)
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
        // Herdr trims the label, so a name with padding or only padding is not the name written.
        Some(toml::Value::String(s)) if s.trim().is_empty() => {
            return Err(format!(
                "{path}: the preset name must be a non-empty string, got {s:?}"
            ))
        }
        Some(toml::Value::String(s)) if s.trim() != s => {
            return Err(format!(
                "{path}: preset name {s:?} has leading or trailing whitespace"
            ))
        }
        // `presets` prints one name per line and `set` takes the name back as one argument.
        Some(toml::Value::String(s)) if s.chars().any(char::is_control) => {
            return Err(format!(
                "{path}: preset name {s:?} contains a control character"
            ))
        }
        Some(toml::Value::String(s)) => s.clone(),
        Some(v) => {
            return Err(format!(
                "{path}: the preset name must be a non-empty string, got {}",
                repr(v)
            ))
        }
        None => return Err(format!("{path}: sort preset #{i} has no name")),
    };
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "{path}: preset name {name:?} is longer than {MAX_NAME_CHARS} characters"
        ));
    }

    let sort = match t.get("sort") {
        Some(toml::Value::Array(a)) if !a.is_empty() => {
            if a.len() > MAX_SORT_ENTRIES {
                return Err(format!(
                    "{path}: preset {name:?} has {} sort entries (at most {MAX_SORT_ENTRIES})",
                    a.len()
                ));
            }
            a.iter()
                .map(|e| sort_entry(path, &name, e))
                .collect::<Result<Vec<_>, _>>()?
        }
        Some(v) => {
            return Err(format!(
                "{path}: preset {name:?} needs a non-empty sort, got {}",
                repr(v)
            ))
        }
        None => return Err(format!("{path}: preset {name:?} has no sort")),
    };

    let filter = match t.get("filter") {
        None => None,
        Some(v @ toml::Value::Table(_)) => Some(to_json(v)),
        Some(v) => {
            return Err(format!(
                "{path}: filter of preset {name:?} must be a table, got {}",
                repr(v)
            ))
        }
    };

    Ok(Preset { name, sort, filter })
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
# default = "<name>": the preset used when no preset was picked yet, or when
# the picked one is no longer in this file. Uncomment to use. Herdr's own two
# orders, "spaces" and "priority", are always listed before the presets below;
# they can be the default too, and no preset can take their names.
#
# default = "recent"
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
#
# Herdr's own two orders are built in, so they are not written here. For
# reference, as presets they would read as below (Herdr 0.9.5; Herdr may change
# how it sorts). Their names are taken and the two stay listed: for a variation
# of one, copy it under another name. To list only these two, replace every
# [[sort_presets]] entry in this file with the line `sort_presets = []`, placed
# above any [keys] table.
#
# [[sort_presets]]
# name = "spaces"
# sort = [{ field = "workspace_order" }]
#
# [[sort_presets]]
# name = "priority"
# sort = [
#   { field = "attention", order = "desc" },
#   { field = "state_change_seq", order = "desc" },
# ]

[[sort_presets]]
name = "recent"
sort = [{ field = "state_change_seq", order = "desc" }]
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
        assert_eq!(cfg.presets, listed(builtins()));
        assert_eq!(cfg.default, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_builtin_presets_match_the_documented_order() {
        let names: Vec<String> = builtins().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["recent"]);
        assert_eq!(builtins()[0].describe(), "state_change_seq desc");
    }

    #[test]
    fn herdrs_own_sorts_come_first_and_reproduce_its_orders() {
        let names: Vec<String> = listed(builtins()).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["spaces", "priority", "recent"]);
        let herdr = herdr_sorts();
        assert_eq!(herdr[0].sort, [SortEntry::field("workspace_order", false)]);
        assert_eq!(
            herdr[1].sort,
            [
                SortEntry::field("attention", true),
                SortEntry::field("state_change_seq", true)
            ]
        );
        assert_eq!(herdr[0].describe(), "workspace_order asc");
        assert_eq!(herdr[1].describe(), "attention desc, state_change_seq desc");
        assert!(herdr.iter().all(|p| p.filter.is_none()));
    }

    #[test]
    fn a_preset_named_like_one_of_herdrs_sorts_is_an_error_saying_the_name_is_taken() {
        for name in ["spaces", "priority"] {
            let e = err(&format!(
                "[[sort_presets]]\nname = \"{name}\"\nsort = [{{ field = \"agent\" }}]"
            ));
            assert!(
                e.contains(&format!(
                    "preset name \"{name}\" is taken by Herdr's own sort"
                )),
                "{name}: {e}"
            );
        }
        let e = err("[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"seen\" }]");
        assert!(e.contains("duplicate preset name \"x\""), "{e}");
    }

    #[test]
    fn the_old_builtin_name_attention_means_priority_unless_a_preset_carries_it() {
        let cfg = parse("config.toml", "default = \"attention\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.default.as_deref(), Some("priority"));
        assert_eq!(cfg.resolve("attention"), "priority");
        assert_eq!(cfg.resolve("recent"), "recent");
        assert_eq!(cfg.resolve("workspace"), "spaces");
        let ws = parse("config.toml", "default = \"workspace\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ws.default.as_deref(), Some("spaces"));
        let own_ws = parse(
            "config.toml",
            "default = \"workspace\"\n[[sort_presets]]\nname = \"workspace\"\nsort = [{ field = \"agent\" }]",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(own_ws.default.as_deref(), Some("workspace"));
        assert_eq!(cfg.resolve("gone"), "gone");
        let own = parse(
            "config.toml",
            "default = \"attention\"\n[[sort_presets]]\nname = \"attention\"\nsort = [{ field = \"agent\" }]",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(own.default.as_deref(), Some("attention"));
        assert_eq!(own.resolve("attention"), "attention");
    }

    #[test]
    fn herdrs_sorts_can_be_the_default_with_or_without_sort_presets() {
        let cfg = parse("config.toml", "default = \"spaces\"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.default.as_deref(), Some("spaces"));
        let cfg = parse(
            "config.toml",
            "default = \"priority\"\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.default.as_deref(), Some("priority"));
    }

    #[test]
    fn the_template_parses_to_exactly_the_builtins() {
        let cfg = parse("config.toml", TEMPLATE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.presets, listed(builtins()));
        assert_eq!(cfg.default, None);
    }

    #[test]
    fn the_templates_commented_sample_of_herdrs_sorts_matches_the_built_in_ones() {
        // The sample is the comment lines from its first [[sort_presets]] to the first line
        // that is not a comment; uncommented, and under free names, it must parse to the same
        // sorts.
        let sample: String = TEMPLATE
            .lines()
            .skip_while(|l| *l != "# [[sort_presets]]")
            .take_while(|l| l.starts_with('#'))
            .map(|l| l.trim_start_matches('#').trim_start_matches(' '))
            .collect::<Vec<_>>()
            .join("\n")
            .replace("\"spaces\"", "\"s\"")
            .replace("\"priority\"", "\"p\"");
        let cfg = parse("config.toml", &sample).unwrap_or_else(|e| panic!("{e}\n{sample}"));
        let sorts = |ps: &[Preset]| ps.iter().map(|p| p.sort.clone()).collect::<Vec<_>>();
        let names: Vec<&str> = cfg.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["spaces", "priority", "s", "p"]);
        assert_eq!(sorts(&cfg.presets[2..]), sorts(&herdr_sorts()));
    }

    #[test]
    fn the_template_documents_every_section_in_its_comments() {
        for section in ["token", "field", "order", "filter", "default", "[keys]"] {
            assert!(
                TEMPLATE
                    .lines()
                    .any(|l| l.trim_start().starts_with('#') && l.contains(section)),
                "no comment line mentions {section:?}"
            );
        }
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
        assert_eq!(
            names,
            ["spaces", "priority", "prio", "repo"],
            "the config replaces the presets after Herdr's sorts"
        );
        assert_eq!(cfg.presets[2].describe(), "prio desc, attention asc");
        assert_eq!(
            cfg.presets[2].filter,
            Some(json!({"op": "in", "field": "status", "values": ["blocked", "done"]}))
        );
        assert_eq!(cfg.presets[3].filter, None);
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
                "[[sort_presets]]\nname = \"   \"\nsort = [{ field = \"agent\" }]",
                "non-empty string, got \"   \"",
            ),
            (
                "[[sort_presets]]\nname = \" x\"\nsort = [{ field = \"agent\" }]",
                "preset name \" x\" has leading or trailing whitespace",
            ),
            (
                "[[sort_presets]]\nname = \"x \"\nsort = [{ field = \"agent\" }]",
                "preset name \"x \" has leading or trailing whitespace",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\", bogus = 1 }]",
                "unknown key \"bogus\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]\nbogus = 1",
                "unknown key \"bogus\"",
            ),
            ("bogus = 1", "unknown key \"bogus\""),
            (
                "[[sort_presets]]\nname = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nsort = [{ field = \"agent\" }]",
                "preset name \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" is longer than 32",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }]",
                "preset \"x\" has 9 sort entries (at most 8)",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]\nfilter = \"nope\"",
                "filter of preset \"x\" must be a table, got \"nope\"",
            ),
            (
                "[[sort_presets]]\nname = \"x\"\nsort = [{ token = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }]",
                "invalid token \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
            ),
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
            "[[sort_presets]]\nname = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nsort = [{ field = \"agent\" }]",
            "[[sort_presets]]\nname = \"x\"\nsort = [{ token = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }]",
            "[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }, { field = \"agent\" }]",
        ] {
            parse("config.toml", text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        }
    }

    #[test]
    fn a_token_name_has_at_most_32_characters() {
        let ok = format!(
            "[[sort_presets]]\nname = \"x\"\nsort = [{{ token = \"{}\" }}]",
            "a".repeat(32)
        );
        parse("config.toml", &ok).unwrap_or_else(|e| panic!("{e}"));
        let bad = format!(
            "[[sort_presets]]\nname = \"x\"\nsort = [{{ token = \"{}\" }}]",
            "a".repeat(33)
        );
        assert!(err(&bad).contains("invalid token"), "{}", err(&bad));
    }

    #[test]
    fn an_empty_sort_presets_list_leaves_only_herdrs_sorts() {
        let cfg = parse("config.toml", "sort_presets = []").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.presets, herdr_sorts());
        assert_eq!(cfg.default, None);
        let e = err("default = \"recent\"\nsort_presets = []");
        assert!(e.contains("default \"recent\" is not a sort preset"), "{e}");
    }

    #[test]
    fn a_config_without_sort_presets_keeps_the_builtins() {
        let cfg = parse(
            "config.toml",
            "default = \"recent\"\n[keys]\nup = \"ctrl-p\"",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.presets, listed(builtins()));
        assert_eq!(cfg.default.as_deref(), Some("recent"));
    }
}
