mod config;
mod keys;
mod rows;
mod state;
mod ui;

use config::{pick, Config};
use rows::{clean, cmp, kv, tostring, Row, LIST_ERR};
use serde_json::{Map, Value};
use state::{present, State};
use std::io::Write;
use std::process::{Command, Stdio};

type Res<T> = Result<T, String>;

struct Env {
    herdr: String,
    config_path: String,
    state_dir: String,
    state_path: String,
    list: Option<String>,
    plugin_id: String,
}

// Like bash `${VAR:-}`: an empty variable counts as unset.
fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

impl Env {
    fn new() -> Env {
        let home = std::env::var("HOME").unwrap_or_default();
        let config_dir = var("HERDR_PLUGIN_CONFIG_DIR").unwrap_or_else(|| {
            let base = var("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{home}/.config"));
            format!("{base}/herdr-agent-search")
        });
        let state_dir = var("HERDR_PLUGIN_STATE_DIR").unwrap_or_else(|| {
            let base = var("XDG_STATE_HOME").unwrap_or_else(|| format!("{home}/.local/state"));
            format!("{base}/herdr-agent-search")
        });
        Env {
            herdr: var("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            config_path: format!("{config_dir}/config.json"),
            state_path: format!("{state_dir}/state.json"),
            state_dir,
            list: var("AGENT_SEARCH_LIST"),
            plugin_id: var("HERDR_PLUGIN_ID")
                .unwrap_or_else(|| "takashiaihara.agent-search".into()),
        }
    }

    // None when the list is empty, which jq reads as no input at all.
    fn pane_list(&self) -> Res<Option<Value>> {
        let err = || LIST_ERR.to_string();
        let bytes = match &self.list {
            Some(p) => std::fs::read(p).map_err(|_| err())?,
            None => {
                let out = Command::new(&self.herdr)
                    .args(["pane", "list"])
                    .output()
                    .map_err(|_| err())?;
                if !out.status.success() {
                    return Err(with_detail(err(), &String::from_utf8_lossy(&out.stderr)));
                }
                out.stdout
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text).map(Some).map_err(|_| err())
    }

    fn config(&self) -> Res<Config> {
        config::load(&self.config_path)
    }

    // `herdr pane report-metadata` for the priority token: Some(n) sets it, None clears it.
    // Output is captured, never inherited: the popup's terminal is live while this runs. Err holds stderr.
    fn report_priority(&self, token: &str, id: &str, n: Option<&str>) -> Result<(), String> {
        let token_arg = match n {
            Some(n) => ["--token".to_string(), format!("{token}={n}")],
            None => ["--clear-token".to_string(), token.to_string()],
        };
        let out = Command::new(&self.herdr)
            .args(["pane", "report-metadata", id, "--source", &self.plugin_id])
            .args(token_arg)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
}

// One pane list for a whole command, read on first use. The popup keeps one for its whole session.
struct Snapshot(Option<Option<Value>>);

impl Snapshot {
    fn empty() -> Snapshot {
        Snapshot(None)
    }

    fn get(&mut self, env: &Env) -> Res<&Option<Value>> {
        let list = match self.0.take() {
            Some(l) => l,
            None => env.pane_list()?,
        };
        Ok(self.0.insert(list))
    }

    fn rows(&mut self, env: &Env, cfg: &Config, st: &State) -> Res<Vec<Row>> {
        rows::build(self.get(env)?.as_ref(), cfg, st)
    }

    // None when the list is empty: no panes at all is not evidence that every saved pane is gone.
    fn live_keys(&mut self, env: &Env) -> Res<Option<std::collections::HashSet<String>>> {
        Ok(self.get(env)?.as_ref().and_then(rows::live_keys))
    }

    // Mirrors a priority change Herdr accepted, so rows rebuilt from this snapshot show it.
    fn set_token(&mut self, id: &str, token: &str, value: Option<&str>) {
        let panes = self
            .0
            .as_mut()
            .and_then(|l| l.as_mut())
            .and_then(|l| l.get_mut("result"))
            .and_then(|r| r.get_mut("panes"))
            .and_then(Value::as_array_mut);
        let Some(pane) = panes.and_then(|ps| {
            ps.iter_mut()
                .find(|p| p.get("pane_id").and_then(Value::as_str) == Some(id))
        }) else {
            return;
        };
        let Some(fields) = pane.as_object_mut() else {
            return;
        };
        match value {
            Some(v) => {
                let tokens = fields.entry("tokens").or_insert(Value::Null);
                if !tokens.is_object() {
                    *tokens = Value::Object(Map::new());
                }
                if let Some(t) = tokens.as_object_mut() {
                    t.insert(token.to_string(), Value::String(v.to_string()));
                }
            }
            None => {
                if let Some(t) = fields.get_mut("tokens").and_then(Value::as_object_mut) {
                    t.remove(token);
                }
            }
        }
    }
}

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

// `.result.panes[]?`: anything that is not a list of panes is no panes.
fn lenient_panes(list: &Option<Value>) -> Vec<&Value> {
    let panes = list
        .as_ref()
        .and_then(|l| l.get("result"))
        .and_then(|r| r.get("panes"));
    match panes {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(o)) => o.values().collect(),
        _ => vec![],
    }
}

fn pane_with_id<'a>(panes: &[&'a Value], id: &str) -> Vec<&'a Value> {
    panes
        .iter()
        .copied()
        .filter(|p| p.get("pane_id").and_then(Value::as_str) == Some(id))
        .collect()
}

fn lines(env: &Env) -> Res<()> {
    let cfg = env.config()?;
    let st = State::load(&env.state_path);
    out(&rows::lines(&Snapshot::empty().rows(env, &cfg, &st)?));
    Ok(())
}

fn header_text(cfg: &Config, st: &State) -> String {
    let sort = &cfg.sorts[pick(&cfg.sorts, st.get("sort"))].name;
    let group = &cfg.groups[pick(&cfg.groups, st.get("group"))].name;
    let k = &cfg.keys;
    format!(
        "sort: {sort}  group: {group}\n{} sort  {} group  {}/{} move  alt-1..9 prio  {} clear\n",
        k.sort, k.group, k.up, k.down, k.clear
    )
}

fn header(env: &Env) -> Res<()> {
    let cfg = env.config()?;
    out(&header_text(&cfg, &State::load(&env.state_path)));
    Ok(())
}

fn enter(id: &str) {
    if id.is_empty() {
        return;
    }
    out(if id.starts_with("-:") {
        "down\n"
    } else {
        "accept\n"
    });
}

fn object(v: Option<&Value>) -> Map<String, Value> {
    present(v)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

// `prune` drops the saved priorities and positions of panes that are no longer in Herdr's pane list. It
// is for acts that read the live list; the popup acts on a snapshot, where a pane opened after the
// snapshot would lose its state.
fn act(env: &Env, cfg: &Config, snap: &mut Snapshot, args: &[String], prune: bool) -> Res<()> {
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
    let op = arg(0);

    let (dir, id, n) = match op {
        "sort-next" | "group-next" => ("", "", ""),
        "move" => {
            if arg(1) != "up" && arg(1) != "down" {
                return Err("move needs up or down".into());
            }
            (arg(1), arg(2), "")
        }
        "prio" => {
            if !(arg(1).len() == 1 && arg(1).as_bytes()[0].is_ascii_digit() && arg(1) != "0") {
                return Err("prio needs 1-9".into());
            }
            ("", arg(2), arg(1))
        }
        "clear" => ("", arg(1), ""),
        _ => return Err(format!("unknown act: {op}")),
    };
    if matches!(op, "move" | "prio" | "clear") && id.is_empty() {
        return Err(format!("{op} needs a pane id"));
    }

    let _lock = state::Lock::acquire(&env.state_dir, &env.state_path)?;
    let before = state::read_raw(&env.state_path)?;
    let st = State::parse(before.as_deref());
    let mut new = st.0.clone();
    let rows = snap.rows(env, cfg, &st)?;
    // Set after the state is written, so a failed write never leaves Herdr ahead of the state.
    let mut report: Option<Option<&str>> = None;

    match op {
        "sort-next" | "group-next" => {
            let (names, k) = if op == "sort-next" {
                (&cfg.sorts, "sort")
            } else {
                (&cfg.groups, "group")
            };
            let next = names
                .get(pick(names, st.get(k)) + 1)
                .map_or(Value::Null, |n| Value::String(n.name.clone()));
            new.insert(k.into(), next);
        }
        "move" => {
            let i = rows.iter().position(|r| r.id == id);
            let j = i.and_then(|i| {
                if dir == "up" {
                    i.checked_sub(1)
                } else {
                    Some(i + 1)
                }
            });
            if let (Some(i), Some(j)) = (i, j) {
                // The neighbor must be in the same group: groups are rebuilt on every reload, so a
                // swap across a header would snap back.
                if j < rows.len() && rows[j].group == rows[i].group {
                    let mut by_manual: Vec<&Row> = rows.iter().collect();
                    by_manual.sort_by(|a, b| cmp(&(kv(&a.manual), a.idx), &(kv(&b.manual), b.idx)));
                    // Only the listed panes are renumbered; the saved position of any other pane stays.
                    let mut order = object(st.get("order"));
                    for (pos, r) in by_manual.iter().enumerate() {
                        order.insert(r.key.clone(), Value::from(pos));
                    }
                    let ni = order.get(&rows[i].key).cloned().unwrap_or(Value::Null);
                    let nj = order.get(&rows[j].key).cloned().unwrap_or(Value::Null);
                    order.insert(rows[i].key.clone(), nj);
                    order.insert(rows[j].key.clone(), ni);
                    new.insert("order".into(), Value::Object(order));
                }
            }
        }
        _ => {
            if let Some(r) = rows.iter().find(|r| r.id == id) {
                let value = (op == "prio").then_some(n);
                // The state remembers it so `restore` can set the token again after a Herdr restart.
                let mut prio = object(st.get("prio"));
                match value {
                    Some(n) => {
                        prio.insert(r.key.clone(), Value::from(n.parse::<u64>().unwrap_or(0)))
                    }
                    None => prio.remove(&r.key),
                };
                new.insert("prio".into(), Value::Object(prio));
                report = Some(value);
            }
        }
    }

    // A pane is gone when no pane in the list, agent or not, has its key.
    let live = if prune { snap.live_keys(env)? } else { None };
    for k in ["prio", "order"] {
        let mut m = object(new.get(k));
        if let Some(live) = &live {
            m.retain(|key, _| live.contains(key));
        }
        new.insert(k.into(), Value::Object(m));
    }
    state::write(&env.state_dir, &env.state_path, &Value::Object(new))?;

    if let Some(value) = report {
        if let Err(detail) = env.report_priority(&cfg.priority_token, id, value) {
            state::rollback(&env.state_dir, &env.state_path, before.as_deref())?;
            return Err(with_detail(
                format!("cannot set the priority of {id}"),
                &detail,
            ));
        }
        snap.set_token(id, &cfg.priority_token, value);
    }
    Ok(())
}

// The first line of what a Herdr subprocess printed on stderr, to put after our own message.
fn with_detail(msg: String, detail: &str) -> String {
    match detail
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        Some(l) => format!("{msg} ({l})"),
        None => msg,
    }
}

// Herdr drops tokens when its server restarts: set again the saved priority of every pane that lost it.
fn restore(env: &Env) -> Res<()> {
    let cfg = env.config()?;
    let st = State::load(&env.state_path);
    for r in Snapshot::empty().rows(env, &cfg, &st)? {
        let Some(saved) = st.lookup("prio", &r.key).filter(|_| !r.has_token) else {
            continue;
        };
        if let Err(detail) =
            env.report_priority(&cfg.priority_token, &r.id, Some(&tostring(&saved)))
        {
            eprintln!(
                "agent-search: {}",
                with_detail(format!("cannot restore the priority of {}", r.id), &detail)
            );
        }
    }
    Ok(())
}

fn raw(v: Option<&Value>) -> String {
    v.map_or_else(|| "null".to_string(), tostring)
}

// Err holds the subprocess's stderr, which is captured because the popup's terminal is live.
fn run_ok(herdr: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(herdr)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

// Herdr has no focus-by-pane-id for plain terminals: agent panes go through `agent focus` (which also
// marks a done agent seen, as looking at it in Herdr does); others, and an agent that exited since
// the list was taken, focus their workspace and tab.
// shortcut: a plain terminal sharing a tab with other panes lands on the tab, not the pane;
// walk pane.layout with pane.focus_direction if that case shows up.
fn focus_in(env: &Env, list: &Option<Value>, id: &str) -> Res<()> {
    if id.is_empty() || id.starts_with("-:") {
        return Ok(());
    }
    let panes = lenient_panes(list);
    let Some(pane) = pane_with_id(&panes, id).into_iter().next() else {
        return Err(format!("pane {id} is gone"));
    };
    if rows::is_agent(pane) && run_ok(&env.herdr, &["agent", "focus", id]).is_ok() {
        return Ok(());
    }
    let ws = raw(pane.get("workspace_id"));
    let tab = raw(pane.get("tab_id"));
    run_ok(&env.herdr, &["workspace", "focus", &ws])
        .and_then(|_| run_ok(&env.herdr, &["tab", "focus", &tab]))
        .map_err(|detail| with_detail(format!("cannot focus {id}"), &detail))
}

fn focus(env: &Env, id: &str) -> Res<()> {
    if id.is_empty() || id.starts_with("-:") {
        return Ok(());
    }
    focus_in(env, &env.pane_list()?, id)
}

// Every field of the pane except tokens, scroll and revision, then its tokens, as `key<TAB>value`.
fn preview_text(list: &Option<Value>, id: &str) -> String {
    let mut text = String::new();
    for pane in pane_with_id(&lenient_panes(list), id) {
        let Some(fields) = pane.as_object() else {
            continue;
        };
        let mut entries: Vec<(String, &Value)> = fields
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "tokens" | "scroll" | "revision"))
            .map(|(k, v)| (k.clone(), v))
            .collect();
        match present(fields.get("tokens")) {
            Some(Value::Object(m)) => {
                entries.extend(m.iter().map(|(k, v)| (format!("tokens.{k}"), v)))
            }
            Some(Value::Array(a)) => entries.extend(
                a.iter()
                    .enumerate()
                    .map(|(i, v)| (format!("tokens.{i}"), v)),
            ),
            _ => {}
        }
        for (k, v) in entries.into_iter().filter(|(_, v)| !v.is_null()) {
            text.push_str(&format!("{}\t{}\n", clean(&k), clean(&tostring(v))));
        }
    }
    text
}

fn preview(env: &Env, id: &str) -> Res<()> {
    out(&preview_text(&env.pane_list()?, id));
    Ok(())
}

fn open(env: &Env) -> i32 {
    Command::new(&env.herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            &env.plugin_id,
            "--entrypoint",
            "search",
        ])
        .stdout(Stdio::null())
        .status()
        .map_or(127, |s| s.code().unwrap_or(1))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::new();
    let rest = &args[args.len().min(1)..];
    let first = |i: usize| rest.get(i).map_or("", String::as_str);
    let result = match args.first().map(String::as_str).unwrap_or("") {
        "open" => std::process::exit(open(&env)),
        "ui" => ui::run(&env),
        "lines" => lines(&env),
        "header" => header(&env),
        "enter" => {
            enter(first(0));
            Ok(())
        }
        "act" => env
            .config()
            .and_then(|cfg| act(&env, &cfg, &mut Snapshot::empty(), rest, true)),
        "restore" => restore(&env),
        "focus" => focus(&env, first(0)),
        "preview" => preview(&env, first(0)),
        _ => Err(
            "usage: agent-search open|ui|lines|header|enter|act|focus|preview|restore".to_string(),
        ),
    };
    if let Err(msg) = result {
        eprintln!("agent-search: {msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(pane: Value) -> Snapshot {
        Snapshot(Some(Some(json!({"result": {"panes": [pane]}}))))
    }

    fn tokens(snap: &Snapshot) -> Value {
        snap.0
            .as_ref()
            .and_then(|l| l.as_ref())
            .map_or(Value::Null, |l| l["result"]["panes"][0]["tokens"].clone())
    }

    #[test]
    fn set_token_works_whether_tokens_is_an_object_null_or_missing() {
        for pane in [
            json!({"pane_id": "p"}),
            json!({"pane_id": "p", "tokens": null}),
            json!({"pane_id": "p", "tokens": "oops"}),
            json!({"pane_id": "p", "tokens": {"repo": "a"}}),
        ] {
            let mut snap = snapshot(pane.clone());
            snap.set_token("p", "prio", Some("4"));
            assert_eq!(tokens(&snap)["prio"], "4", "{pane}");
            snap.set_token("p", "prio", None);
            assert!(tokens(&snap).get("prio").is_none(), "{pane}");
        }
        let mut keeps = snapshot(json!({"pane_id": "p", "tokens": {"repo": "a"}}));
        keeps.set_token("p", "prio", Some("2"));
        assert_eq!(tokens(&keeps), json!({"repo": "a", "prio": "2"}));
    }

    fn env_with(dir: &str, panes: Value, saved: Value) -> Env {
        let dir =
            std::env::temp_dir().join(format!("agent-search-act-{dir}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let list = dir.join("list.json");
        std::fs::write(&list, json!({"result": {"panes": panes}}).to_string())
            .unwrap_or_else(|e| panic!("{e}"));
        let state = dir.join("state.json");
        std::fs::write(&state, saved.to_string()).unwrap_or_else(|e| panic!("{e}"));
        Env {
            herdr: "herdr-that-does-not-exist".into(),
            config_path: dir.join("config.json").to_string_lossy().into_owned(),
            state_dir: dir.to_string_lossy().into_owned(),
            state_path: state.to_string_lossy().into_owned(),
            list: Some(list.to_string_lossy().into_owned()),
            plugin_id: "t".into(),
        }
    }

    #[test]
    fn an_act_on_a_snapshot_does_not_prune_but_one_on_the_live_list_does() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let saved = json!({"prio": {"p1": 2, "opened-later": 5}, "order": {"opened-later": 1}});
        for (prune, kept) in [
            (false, json!({"p1": 2, "opened-later": 5})),
            (true, json!({"p1": 2})),
        ] {
            let env = env_with(&format!("prune{prune}"), panes.clone(), saved.clone());
            let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
            let args = vec!["sort-next".to_string()];
            act(&env, &cfg, &mut Snapshot::empty(), &args, prune).unwrap_or_else(|e| panic!("{e}"));
            let after = State::load(&env.state_path);
            assert_eq!(after.get("prio"), Some(&kept), "prune={prune}");
        }
    }
}
