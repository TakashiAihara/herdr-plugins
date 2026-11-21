use crate::state::State;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub struct Named {
    pub name: String,
    pub by: Vec<String>,
}

pub struct Group {
    pub name: String,
    pub levels: Vec<Vec<String>>,
}

pub trait HasName {
    fn name(&self) -> &str;
}

impl HasName for Named {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for Group {
    fn name(&self) -> &str {
        &self.name
    }
}

// How the popup's filter matches a query word: fuzzy, letters in order with gaps allowed, or
// substring, the word appearing contiguously.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Match {
    Fuzzy,
    Substring,
}

impl Match {
    pub fn name(self) -> &'static str {
        match self {
            Match::Fuzzy => "fuzzy",
            Match::Substring => "substring",
        }
    }

    fn from_name(name: &str) -> Option<Match> {
        match name {
            "fuzzy" => Some(Match::Fuzzy),
            "substring" => Some(Match::Substring),
            _ => None,
        }
    }

    pub fn other(self) -> Match {
        match self {
            Match::Fuzzy => Match::Substring,
            Match::Substring => Match::Fuzzy,
        }
    }
}

pub struct Keys {
    pub sort: String,
    pub group: String,
    pub r#match: String,
    pub preview: String,
    pub wrap: String,
    pub up: String,
    pub down: String,
    pub clear: String,
}

pub struct Config {
    pub columns: Vec<String>,
    pub search: Vec<String>,
    pub sorts: Vec<Named>,
    pub groups: Vec<Group>,
    pub priority_token: String,
    pub priority_max: u8,
    pub r#match: Match,
    pub preview: bool,
    pub preview_wrap: bool,
    pub preview_hide: Vec<String>,
    pub keys: Keys,
}

impl Config {
    // The mode in effect: the state's saved mode when it names one, else the config's. A saved
    // value that is not a mode name is ignored.
    pub fn mode(&self, st: &State) -> Match {
        st.get("match")
            .and_then(Value::as_str)
            .and_then(Match::from_name)
            .unwrap_or(self.r#match)
    }

    // Whether the preview pane starts shown: the state's saved choice when it is a boolean,
    // else the config's `preview`.
    pub fn preview_on(&self, st: &State) -> bool {
        st.get("preview")
            .and_then(Value::as_bool)
            .unwrap_or(self.preview)
    }

    // Whether long preview lines wrap with a hanging indent: the state's saved choice when it
    // is a boolean, else the config's `preview_wrap`.
    pub fn wrap_on(&self, st: &State) -> bool {
        st.get("preview_wrap")
            .and_then(Value::as_bool)
            .unwrap_or(self.preview_wrap)
    }
}

fn defaults() -> Value {
    json!({
        "columns": ["prio", "agent_status", "title"],
        "search": ["title", "cwd", "agent", "agent_status", "terminal_title_stripped", "agent_session.value", "tokens.*"],
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
        "priority_max": 5,
        "match": "fuzzy",
        "preview": true,
        "preview_wrap": false,
        "preview_hide": [],
        "keys": {"sort": "ctrl-s", "group": "ctrl-g", "match": "ctrl-r", "preview": "ctrl-o", "wrap": "ctrl-w", "up": "alt-up", "down": "alt-down", "clear": "alt-0"}
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

// A group names one fallback chain in `by`, or several in `levels` (one per header level); `by: []`
// means no grouping, but a `levels` chain may not be empty. Having both keys, or neither, is a
// config error.
fn groups(v: &Value) -> Option<Vec<Group>> {
    let items = v.as_array()?;
    if items.is_empty() {
        return None;
    }
    let mut out: Vec<Group> = Vec::new();
    for item in items {
        let o = item.as_object()?;
        let name = o.get("name")?.as_str().filter(|s| !s.is_empty())?;
        if out.iter().any(|g| g.name == name) {
            return None;
        }
        let levels = match (o.get("by"), o.get("levels")) {
            (Some(by), None) => {
                let chain = strs(by)?;
                if chain.is_empty() {
                    Vec::new()
                } else {
                    vec![chain]
                }
            }
            (None, Some(levels)) => levels
                .as_array()
                .filter(|c| !c.is_empty())?
                .iter()
                .map(non_empty_strs)
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        out.push(Group {
            name: name.to_string(),
            levels,
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
        groups: groups(o.get("groups")?)?,
        priority_token: token_name(o.get("priority_token")?)?,
        priority_max: o
            .get("priority_max")?
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())?,
        r#match: Match::from_name(o.get("match")?.as_str()?)?,
        preview: o.get("preview")?.as_bool()?,
        preview_wrap: o.get("preview_wrap")?.as_bool()?,
        preview_hide: strs(o.get("preview_hide")?)?,
        keys: Keys {
            sort: key(keys, "sort")?,
            group: key(keys, "group")?,
            r#match: key(keys, "match")?,
            preview: key(keys, "preview")?,
            wrap: key(keys, "wrap")?,
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
    if let Some(v) = merged.get("priority_max") {
        if !matches!(v.as_u64(), Some(n) if (1..=9).contains(&n)) {
            return Err(format!(
                "{path}: priority_max must be an integer from 1 to 9"
            ));
        }
    }
    if let Some(v) = merged.get("match") {
        if !matches!(v.as_str(), Some("fuzzy" | "substring")) {
            return Err(format!("{path}: match must be \"fuzzy\" or \"substring\""));
        }
    }
    if let Some(v) = merged.get("preview") {
        if !v.is_boolean() {
            return Err(format!("{path}: preview must be true or false"));
        }
    }
    if let Some(v) = merged.get("preview_wrap") {
        if !v.is_boolean() {
            return Err(format!("{path}: preview_wrap must be true or false"));
        }
    }
    if let Some(v) = merged.get("preview_hide") {
        let ok = v
            .as_array()
            .is_some_and(|a| a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())));
        if !ok {
            return Err(format!(
                "{path}: preview_hide must be a list of non-empty strings"
            ));
        }
    }
    let cfg = extract(&merged).ok_or_else(|| {
        let fields = "columns and search must be non-empty lists of fields, sorts and groups non-empty lists of {name, by} (or {name, levels} for groups) with unique names, priority_token a Herdr token name (1-32 letters, digits, _ or -)";
        if need_keys {
            format!("{path}: {fields}, and keys sort/group/match/preview/wrap/up/down/clear non-empty strings")
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
    for spec in cfg.groups.iter().flat_map(|g| g.levels.iter().flatten()) {
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
    fn a_priority_max_outside_1_to_9_is_a_config_error_naming_the_key() {
        for bad in ["0", "10", "5.5", "\"5\"", "-1", "null"] {
            let err = load_json(&format!("{{\"priority_max\":{bad}}}"))
                .err()
                .unwrap_or_default();
            assert!(
                err.contains("priority_max must be an integer from 1 to 9"),
                "{bad}: {err}"
            );
        }
        for good in ["1", "9"] {
            let cfg = load_json(&format!("{{\"priority_max\":{good}}}"))
                .unwrap_or_else(|e| panic!("{good}: {e}"));
            assert_eq!(cfg.priority_max, good.parse::<u8>().unwrap_or_default());
        }
        assert_eq!(
            load_json("{}").map(|c| c.priority_max).unwrap_or_default(),
            5
        );
    }

    #[test]
    fn a_match_other_than_fuzzy_or_substring_is_a_config_error_naming_the_key() {
        for bad in ["\"regexp\"", "5", "null", "\"\"", "\"Fuzzy\""] {
            let err = load_json(&format!("{{\"match\":{bad}}}"))
                .err()
                .unwrap_or_default();
            assert!(
                err.contains("match must be \"fuzzy\" or \"substring\""),
                "{bad}: {err}"
            );
        }
        for (good, mode) in [("fuzzy", Match::Fuzzy), ("substring", Match::Substring)] {
            let cfg = load_json(&format!("{{\"match\":\"{good}\"}}"))
                .unwrap_or_else(|e| panic!("{good}: {e}"));
            assert_eq!(cfg.r#match, mode);
        }
        assert_eq!(load_json("{}").map(|c| c.r#match), Ok(Match::Fuzzy));
    }

    #[test]
    fn the_saved_mode_wins_over_the_config_and_an_unknown_one_is_ignored() {
        let cfg = load_json("{}").unwrap_or_else(|e| panic!("{e}"));
        let sub = load_json(r#"{"match":"substring"}"#).unwrap_or_else(|e| panic!("{e}"));
        let st = |s: &str| State::parse(Some(s.as_bytes()));
        assert_eq!(cfg.mode(&st(r#"{"match":"substring"}"#)), Match::Substring);
        assert_eq!(cfg.mode(&st(r#"{"match":"bogus"}"#)), Match::Fuzzy);
        assert_eq!(cfg.mode(&st(r#"{"match":5}"#)), Match::Fuzzy);
        assert_eq!(sub.mode(&st(r#"{"match":"fuzzy"}"#)), Match::Fuzzy);
        assert_eq!(sub.mode(&st(r#"{"match":"bogus"}"#)), Match::Substring);
        assert_eq!(sub.mode(&st("{}")), Match::Substring);
    }

    #[test]
    fn the_match_key_merges_key_by_key_and_is_validated_like_the_others() {
        let cfg = load_json(r#"{"keys":{"match":"ctrl-t"}}"#).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.keys.r#match, "ctrl-t");
        assert_eq!(cfg.keys.sort, "ctrl-s");
        let err = load_json(r#"{"keys":{"match":"ctrl-s"}}"#)
            .err()
            .unwrap_or_default();
        assert!(
            err.contains("keys.match \"ctrl-s\" is the same key as keys.sort"),
            "{err}"
        );
        assert!(load_json(r#"{"keys":{"match":""}}"#).is_err());
    }

    #[test]
    fn a_preview_other_than_true_or_false_is_a_config_error_naming_the_key() {
        for bad in ["\"yes\"", "5", "null", "[]"] {
            let err = load_json(&format!("{{\"preview\":{bad}}}"))
                .err()
                .unwrap_or_default();
            assert!(
                err.contains("preview must be true or false"),
                "{bad}: {err}"
            );
        }
        assert_eq!(load_json("{}").map(|c| c.preview), Ok(true));
        assert_eq!(
            load_json(r#"{"preview":false}"#).map(|c| c.preview),
            Ok(false)
        );
    }

    #[test]
    fn a_preview_wrap_other_than_true_or_false_is_a_config_error_naming_the_key() {
        for bad in ["\"yes\"", "5", "null", "[]"] {
            let err = load_json(&format!("{{\"preview_wrap\":{bad}}}"))
                .err()
                .unwrap_or_default();
            assert!(
                err.contains("preview_wrap must be true or false"),
                "{bad}: {err}"
            );
        }
        assert_eq!(load_json("{}").map(|c| c.preview_wrap), Ok(false));
        assert_eq!(
            load_json(r#"{"preview_wrap":true}"#).map(|c| c.preview_wrap),
            Ok(true)
        );
    }

    #[test]
    fn the_saved_wrap_choice_wins_over_the_config_and_a_non_boolean_is_ignored() {
        let cfg = load_json("{}").unwrap_or_else(|e| panic!("{e}"));
        let on = load_json(r#"{"preview_wrap":true}"#).unwrap_or_else(|e| panic!("{e}"));
        let st = |s: &str| State::parse(Some(s.as_bytes()));
        assert!(cfg.wrap_on(&st(r#"{"preview_wrap":true}"#)));
        assert!(!cfg.wrap_on(&st(r#"{"preview_wrap":"bogus"}"#)));
        assert!(!cfg.wrap_on(&st("{}")));
        assert!(!on.wrap_on(&st(r#"{"preview_wrap":false}"#)));
        assert!(on.wrap_on(&st(r#"{"preview_wrap":"bogus"}"#)));
        assert!(on.wrap_on(&st("{}")));
    }

    #[test]
    fn the_default_search_fields_hold_the_session_id_after_the_terminal_title_only() {
        let cfg = load_json("{}").unwrap_or_else(|e| panic!("{e}"));
        let i = cfg
            .search
            .iter()
            .position(|s| s == "terminal_title_stripped")
            .unwrap_or(usize::MAX);
        assert_eq!(
            cfg.search.get(i + 1).map(String::as_str),
            Some("agent_session.value")
        );
        // Searchable does not mean shown: the session id stays out of the columns.
        assert_eq!(
            cfg.columns,
            ["prio", "agent_status", "title"].map(String::from)
        );
    }

    #[test]
    fn the_wrap_key_merges_key_by_key_and_is_validated_like_the_others() {
        let cfg = load_json(r#"{"keys":{"wrap":"ctrl-t"}}"#).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.keys.wrap, "ctrl-t");
        assert_eq!(cfg.keys.sort, "ctrl-s");
        let err = load_json(r#"{"keys":{"wrap":"ctrl-s"}}"#)
            .err()
            .unwrap_or_default();
        assert!(
            err.contains("keys.wrap \"ctrl-s\" is the same key as keys.sort"),
            "{err}"
        );
        assert!(load_json(r#"{"keys":{"wrap":""}}"#).is_err());
    }

    #[test]
    fn a_preview_hide_entry_that_is_not_a_non_empty_string_is_a_config_error_naming_the_key() {
        for bad in ["\"tokens.repo\"", "5", "[\"\"]", "[5]", "[null]", "{}"] {
            let err = load_json(&format!("{{\"preview_hide\":{bad}}}"))
                .err()
                .unwrap_or_default();
            assert!(
                err.contains("preview_hide must be a list of non-empty strings"),
                "{bad}: {err}"
            );
        }
        let cfg = load_json(r#"{"preview_hide":["tokens.flags","state_labels"]}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.preview_hide, ["tokens.flags", "state_labels"]);
        assert_eq!(load_json("{}").map(|c| c.preview_hide), Ok(vec![]));
    }

    #[test]
    fn the_saved_preview_choice_wins_over_the_config_and_a_non_boolean_is_ignored() {
        let cfg = load_json("{}").unwrap_or_else(|e| panic!("{e}"));
        let off = load_json(r#"{"preview":false}"#).unwrap_or_else(|e| panic!("{e}"));
        let st = |s: &str| State::parse(Some(s.as_bytes()));
        assert!(!cfg.preview_on(&st(r#"{"preview":false}"#)));
        assert!(cfg.preview_on(&st(r#"{"preview":"bogus"}"#)));
        assert!(cfg.preview_on(&st("{}")));
        assert!(off.preview_on(&st(r#"{"preview":true}"#)));
        assert!(!off.preview_on(&st(r#"{"preview":"bogus"}"#)));
        assert!(!off.preview_on(&st("{}")));
    }

    #[test]
    fn the_preview_key_merges_key_by_key_and_is_validated_like_the_others() {
        let cfg = load_json(r#"{"keys":{"preview":"ctrl-t"}}"#).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.keys.preview, "ctrl-t");
        assert_eq!(cfg.keys.sort, "ctrl-s");
        let err = load_json(r#"{"keys":{"preview":"ctrl-s"}}"#)
            .err()
            .unwrap_or_default();
        assert!(
            err.contains("keys.preview \"ctrl-s\" is the same key as keys.sort"),
            "{err}"
        );
        assert!(load_json(r#"{"keys":{"preview":""}}"#).is_err());
    }

    #[test]
    fn a_group_needs_exactly_one_of_by_or_levels() {
        for bad in [
            r#"{"groups":[{"name":"g"}]}"#,
            r#"{"groups":[{"name":"g","by":["a"],"levels":[["b"]]}]}"#,
            r#"{"groups":[{"name":"g","by":[],"levels":[["b"]]}]}"#,
            r#"{"groups":[{"name":"g","levels":[]}]}"#,
            r#"{"groups":[{"name":"g","levels":[[]]}]}"#,
            r#"{"groups":[{"name":"g","levels":[["a"],[]]}]}"#,
            r#"{"groups":[{"name":"g","levels":[["a"],5]}]}"#,
            r#"{"groups":[{"name":"g","levels":"a"}]}"#,
        ] {
            assert!(load_json(bad).is_err(), "{bad}");
        }
        let flat = load_json(r#"{"groups":[{"name":"g","by":["a","b"]}]}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            flat.groups[0].levels,
            [vec!["a".to_string(), "b".to_string()]]
        );
        let none =
            load_json(r#"{"groups":[{"name":"g","by":[]}]}"#).unwrap_or_else(|e| panic!("{e}"));
        assert!(none.groups[0].levels.is_empty());
        let tree = load_json(r#"{"groups":[{"name":"g","levels":[["a","b"],["c"]]}]}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            tree.groups[0].levels,
            [
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string()]
            ]
        );
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
pub fn pick<T: HasName>(names: &[T], chosen: Option<&Value>) -> usize {
    chosen
        .and_then(Value::as_str)
        .and_then(|s| names.iter().position(|n| n.name() == s))
        .unwrap_or(0)
}
