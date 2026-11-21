#[cfg(test)]
mod testutil {
    // A fresh directory made with `create_dir`, which fails if the name exists, so a test never
    // writes into a directory somebody else prepared at a predictable path.
    pub fn unique_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for n in 0..1000u32 {
            // Under the target directory (CARGO_TARGET_DIR when set; removed by `cargo clean`), not
            // the system temp directory: a failed assertion skips any cleanup, and /tmp is often a
            // small tmpfs.
            let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
                || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"),
                std::path::PathBuf::from,
            );
            let base = target.join("test-tmp");
            std::fs::create_dir_all(&base)
                .unwrap_or_else(|e| panic!("cannot create {base:?}: {e}"));
            let dir = base.join(format!(
                "agent-search-{name}-{}-{nanos}-{n}",
                std::process::id()
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return dir,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => panic!("cannot create a test directory: {e}"),
            }
        }
        panic!("no free test directory name for {name}");
    }
}

mod config;
mod keys;
mod rows;
mod state;
mod status;
mod ui;

use config::{pick, Config};
use rows::{clean, cmp, kv, tostring, Row, K, LIST_ERR};
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
    status_indicators: status::Indicator,
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
        let file = state::file_name(var("HERDR_SOCKET_PATH").as_deref());
        Env {
            herdr: var("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            config_path: format!("{config_dir}/config.json"),
            state_path: format!("{state_dir}/{file}"),
            state_dir,
            list: var("AGENT_SEARCH_LIST"),
            plugin_id: var("HERDR_PLUGIN_ID")
                .unwrap_or_else(|| "takashiaihara.agent-search".into()),
            status_indicators: status::indicators(),
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

    // `restore` only: it never binds keys, so a config whose keys cannot be bound must not stop it.
    fn config_for_restore(&self) -> Res<Config> {
        config::load_for_restore(&self.config_path)
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
        rows::build(self.get(env)?.as_ref(), cfg, st, env.status_indicators)
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
    let mode = cfg.mode(st).name();
    let k = &cfg.keys;
    format!(
        "sort: {sort}  group: {group}  match: {mode}\n{} sort  {} group  {} match  {} preview  {} wrap  {}/{} move  alt-1..{} prio  {} clear\n",
        k.sort, k.group, k.r#match, k.preview, k.wrap, k.up, k.down, cfg.priority_max, k.clear
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

// A pane's own priority token is the source of truth: whenever a live pane list is read (the popup
// opening or a CLI act), a token that is an integer in 1..=max is copied into the saved state when
// it differs from the saved value, and a token of 0 removes the saved value, so a priority an agent
// set or cleared itself survives a Herdr restart. Any other token is shown but not copied, and a
// pane without the token keeps its saved value: it cannot be told apart from a pane whose token
// Herdr dropped at restart.
fn merged_priorities(rows: &[Row], st: &State, max: u8) -> Option<Map<String, Value>> {
    let mut prio = object(st.get("prio"));
    let mut changed = false;
    for r in rows {
        let Some(n) = r.prio.as_ref().and_then(Value::as_i64) else {
            continue;
        };
        if n == 0 {
            if st.lookup("prio", &r.key).is_some() {
                prio.remove(&r.key);
                changed = true;
            }
            continue;
        }
        if !(1..=i64::from(max)).contains(&n) {
            continue;
        }
        let cur = Value::from(n);
        if st.lookup("prio", &r.key).as_ref() == Some(&cur) {
            continue;
        }
        prio.insert(r.key.clone(), cur);
        changed = true;
    }
    changed.then_some(prio)
}

// The prio map to write under the state lock: the token merge of the locked state, but a key is
// touched only when its saved value is still the one `unlocked` read before the lock (or still
// absent). A key a CLI act changed meanwhile keeps its locked value, so a stale token can neither
// overwrite a newer priority nor bring back a cleared one.
fn fresh_priorities(
    rows: &[Row],
    unlocked: &State,
    locked: &State,
    max: u8,
) -> Option<Map<String, Value>> {
    let mut prio = object(locked.get("prio"));
    let mut changed = false;
    for r in rows {
        if locked.lookup("prio", &r.key) != unlocked.lookup("prio", &r.key) {
            continue;
        }
        match r.prio.as_ref().and_then(Value::as_i64) {
            Some(0) => {
                if prio.remove(&r.key).is_some() {
                    changed = true;
                }
            }
            Some(n) if (1..=i64::from(max)).contains(&n) => {
                let cur = Value::from(n);
                if prio.get(&r.key) != Some(&cur) {
                    prio.insert(r.key.clone(), cur);
                    changed = true;
                }
            }
            _ => {}
        }
    }
    changed.then_some(prio)
}

// The popup's copy of the sync `act` does on the live list. The list read and the diff run without
// the lock, so a slow `herdr pane list` never holds other writers up and an open with nothing to
// copy creates neither the state dir nor the lock file. Under the lock a key is written only when
// its saved value is still the one read before the lock (or still absent): a CLI `act prio` or
// `act clear` that ran meanwhile wins over the stale token. Like `act`, the state is read with
// `read_raw`, so an unreadable file fails the sync instead of being overwritten empty.
fn sync_priorities(env: &Env, cfg: &Config, snap: &mut Snapshot) -> Res<()> {
    let unlocked = State::load(&env.state_path);
    let rows = snap.rows(env, cfg, &unlocked)?;
    if merged_priorities(&rows, &unlocked, cfg.priority_max).is_none() {
        return Ok(());
    }
    let _lock = state::Lock::acquire(&env.state_dir, &env.state_path)?;
    let st = State::parse(state::read_raw(&env.state_path)?.as_deref());
    let Some(prio) = fresh_priorities(&rows, &unlocked, &st, cfg.priority_max) else {
        return Ok(());
    };
    let mut new = st.0.clone();
    new.insert("prio".into(), Value::Object(prio));
    state::write(&env.state_dir, &env.state_path, &Value::Object(new))
}

// `prune` drops the saved priorities and positions of panes that are no longer in Herdr's pane list. It
// is for acts that read the live list; the popup acts on a snapshot, where a pane opened after the
// snapshot would lose its state.
fn act(env: &Env, cfg: &Config, snap: &mut Snapshot, args: &[String], prune: bool) -> Res<()> {
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
    let op = arg(0);

    let (dir, id, n) = match op {
        "sort-next" | "group-next" | "match-next" | "preview-next" | "wrap-next" => ("", "", ""),
        "move" => {
            if arg(1) != "up" && arg(1) != "down" {
                return Err("move needs up or down".into());
            }
            (arg(1), arg(2), "")
        }
        "prio" => {
            let in_range = arg(1)
                .parse::<u64>()
                .is_ok_and(|n| (1..=u64::from(cfg.priority_max)).contains(&n));
            if !in_range {
                return Err(format!("prio needs 1-{}", cfg.priority_max));
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
    // The token merge runs only on a fresh pane list (the CLI act, which also prunes): the popup's
    // snapshot can be minutes old, and its tokens would overwrite a priority set since it was taken.
    if prune {
        if let Some(prio) = merged_priorities(&rows, &st, cfg.priority_max) {
            new.insert("prio".into(), Value::Object(prio));
        }
    }
    // Set after the state is written, so a failed write never leaves Herdr ahead of the state.
    let mut report: Option<Option<&str>> = None;

    match op {
        "sort-next" | "group-next" => {
            let k = if op == "sort-next" { "sort" } else { "group" };
            let next = if op == "sort-next" {
                cfg.sorts
                    .get(pick(&cfg.sorts, st.get(k)) + 1)
                    .map(|n| &n.name)
            } else {
                cfg.groups
                    .get(pick(&cfg.groups, st.get(k)) + 1)
                    .map(|n| &n.name)
            };
            let next = next.map_or(Value::Null, |n| Value::String(n.clone()));
            new.insert(k.into(), next);
            // Rows must build under the new selection before it is saved: a sort or group whose
            // fields fail on this list would otherwise be saved and break every later command.
            snap.rows(env, cfg, &State(new.clone()))?;
        }
        "match-next" => {
            let next = cfg.mode(&st).other();
            new.insert("match".into(), Value::String(next.name().into()));
        }
        "preview-next" => {
            new.insert("preview".into(), Value::Bool(!cfg.preview_on(&st)));
        }
        "wrap-next" => {
            new.insert("preview_wrap".into(), Value::Bool(!cfg.wrap_on(&st)));
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
                if j < rows.len() && rows[j].groups == rows[i].groups {
                    // Renumber the listed panes together with the keys already in `order` that this
                    // view does not list (a pane hidden right now keeps its place among the others);
                    // listed panes without a saved position count as last, in list order.
                    let mut order = object(st.get("order"));
                    let mut seq: Vec<(K, usize, String)> = rows
                        .iter()
                        .map(|r| (kv(&r.manual), r.idx, r.key.clone()))
                        .collect();
                    for (key, v) in &order {
                        if !rows.iter().any(|r| &r.key == key) {
                            seq.push((kv(v), usize::MAX, key.clone()));
                        }
                    }
                    seq.sort_by(|a, b| cmp(&(&a.0, a.1), &(&b.0, b.1)));
                    for (pos, (_, _, key)) in seq.into_iter().enumerate() {
                        order.insert(key, Value::from(pos));
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
                let mut prio = object(new.get("prio"));
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
    let cfg = env.config_for_restore()?;
    // The pane list is fetched before any lock is taken, so a slow `herdr pane list` never holds
    // one up (other acts wait at most 2 s for the lock).
    let list = env.pane_list()?;
    // The plan is read under the same lock as `act`, so it never sees a change an act has written
    // but may still roll back.
    let planned = {
        let _lock = state::Lock::acquire(&env.state_dir, &env.state_path)?;
        State::load(&env.state_path)
    };
    let todo: Vec<(String, String, Value)> =
        rows::build(list.as_ref(), &cfg, &planned, env.status_indicators)?
            .into_iter()
            .filter(|r| !r.has_token)
            .filter_map(|r| {
                planned
                    .lookup("prio", &r.key)
                    .map(|saved| (r.id, r.key, saved))
            })
            .collect();
    // `act` holds the state lock across its write and its herdr call, so holding the same lock for
    // each pane's check and call serializes the two: a priority set or cleared after the state
    // above was read is seen on the re-read, and the pane is skipped instead of overwritten.
    for (id, key, saved) in todo {
        // A lock held past the wait by a slow act skips only this pane; the rest are still restored.
        let _lock = match state::Lock::acquire(&env.state_dir, &env.state_path) {
            Ok(lock) => lock,
            Err(e) => {
                eprintln!("agent-search: cannot restore the priority of {id}: {e}");
                continue;
            }
        };
        if State::load(&env.state_path).lookup("prio", &key).as_ref() != Some(&saved) {
            continue;
        }
        if let Err(detail) = env.report_priority(&cfg.priority_token, &id, Some(&tostring(&saved)))
        {
            eprintln!(
                "agent-search: {}",
                with_detail(format!("cannot restore the priority of {id}"), &detail)
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

// One entry of a preview section: a scalar on one line, or a node whose children indent two
// spaces below its name.
enum Node {
    Leaf(String, String),
    Tree(String, Vec<Node>),
}

impl Node {
    fn key(&self) -> &str {
        match self {
            Node::Leaf(k, _) | Node::Tree(k, _) => k,
        }
    }
}

// The node for one field: an object or an array becomes a node with children (an array's key is
// the index), anything else a scalar line. `path` is the dot-joined key path preview_hide names,
// so a nested entry or a whole object can be left out.
fn node(path: &str, key: &str, v: &Value, hide: &[String]) -> Option<Node> {
    if hide.iter().any(|p| p == path) {
        return None;
    }
    let key = clean(key);
    match v {
        Value::Object(m) => Some(Node::Tree(
            key,
            m.iter()
                .filter(|(_, v)| !v.is_null())
                .filter_map(|(k, v)| node(&format!("{path}.{k}"), k, v, hide))
                .collect(),
        )),
        Value::Array(a) => Some(Node::Tree(
            key,
            a.iter()
                .enumerate()
                .filter(|(_, v)| !v.is_null())
                .filter_map(|(i, v)| node(&format!("{path}.{i}"), &i.to_string(), v, hide))
                .collect(),
        )),
        _ => Some(Node::Leaf(key, clean(&tostring(v)))),
    }
}

// The widest indented key under the section: the value column is that plus two spaces, shared by
// every level so a nested value lines up with a top-level one.
fn node_width(nodes: &[Node], indent: usize) -> usize {
    nodes
        .iter()
        .map(|n| {
            let key = indent + rows::width(n.key());
            match n {
                Node::Tree(_, children) => key.max(node_width(children, indent + 2)),
                Node::Leaf(..) => key,
            }
        })
        .max()
        .unwrap_or(0)
}

// One row of the preview: the logical line, the column a wrapped continuation hangs at,
// and for a `key  value` row the byte offset where the value starts (None for a bare
// outline parent or a section header, which the wrapper cannot split). A scalar's hang is
// its section's value column; a parent's is its indent + 4 — deeper than its children at
// indent + 2, so a continuation does not read as a child key.
pub struct PreviewLine {
    pub text: String,
    pub hang: usize,
    pub split: Option<usize>,
}

fn render_nodes(nodes: &[Node], indent: usize, w: usize, out: &mut Vec<PreviewLine>) {
    for n in nodes {
        let pad = " ".repeat(indent);
        match n {
            Node::Leaf(k, v) => {
                let gap = " ".repeat(w - indent - rows::width(k));
                out.push(PreviewLine {
                    text: format!("{pad}{k}{gap}{v}"),
                    hang: w,
                    split: Some(pad.len() + k.len() + gap.len()),
                });
            }
            Node::Tree(k, children) => {
                out.push(PreviewLine {
                    text: format!("{pad}{k}"),
                    hang: indent + 4,
                    split: None,
                });
                render_nodes(children, indent + 2, w, out);
            }
        }
    }
}

// The preview as an outline in up to three sections: `[ids]` the pane's `*_id` fields under
// their Herdr names, `[metadata]` every other field but `tokens` (object and array fields as
// nodes whose entries indent two spaces below them, recursively), `[tokens]` each token as its
// own line named without the `tokens.` prefix. Each section lines its values up on a column of
// their own — two spaces past the section's widest key at any depth — and a section with
// nothing in it is left out. A pane id listed twice shows only its first entry.
pub fn preview_lines(list: &Option<Value>, id: &str, hide: &[String]) -> Vec<PreviewLine> {
    let mut ids: Vec<Node> = Vec::new();
    let mut meta: Vec<Node> = Vec::new();
    let mut toks: Vec<Node> = Vec::new();
    for pane in pane_with_id(&lenient_panes(list), id).into_iter().take(1) {
        let Some(fields) = pane.as_object() else {
            continue;
        };
        for (k, v) in fields {
            if matches!(k.as_str(), "scroll" | "revision") || v.is_null() {
                continue;
            }
            if k == "tokens" {
                // The tokens map's entries are the [tokens] section's lines; node() still
                // names their paths `tokens.<name>` so preview_hide keeps working.
                if let Some(n) = node("tokens", "tokens", v, hide) {
                    match n {
                        Node::Tree(_, children) => toks.extend(children),
                        n => toks.push(n),
                    }
                }
                continue;
            }
            let target = if k.ends_with("_id") {
                &mut ids
            } else {
                &mut meta
            };
            if let Some(n) = node(k, k, v, hide) {
                target.push(n);
            }
        }
    }
    let sections = [("[ids]", ids), ("[metadata]", meta), ("[tokens]", toks)];
    let mut lines = Vec::new();
    for (name, nodes) in &sections {
        if nodes.is_empty() {
            continue;
        }
        // A blank line before every section but the first, so each heading stands apart.
        if !lines.is_empty() {
            lines.push(PreviewLine {
                text: String::new(),
                hang: 0,
                split: None,
            });
        }
        lines.push(PreviewLine {
            text: (*name).to_string(),
            hang: 0,
            split: None,
        });
        // A short section is not pushed right by a long key in another one.
        let w = node_width(nodes, 0) + 2;
        render_nodes(nodes, 0, w, &mut lines);
    }
    lines
}

fn preview_text(list: &Option<Value>, id: &str, hide: &[String]) -> String {
    let mut text = String::new();
    for l in preview_lines(list, id, hide) {
        text.push_str(&l.text);
        text.push('\n');
    }
    text
}

fn preview(env: &Env, id: &str) -> Res<()> {
    let cfg = env.config()?;
    out(&preview_text(&env.pane_list()?, id, &cfg.preview_hide));
    Ok(())
}

fn open(env: &Env) -> i32 {
    // A config that does not load still opens the popup, at the manifest's size (no --width /
    // --height, so Herdr uses the pane's own): the popup itself shows the config error, where
    // pressing the key otherwise would show nothing at all. The flags are only valid while the
    // manifest's placement is "popup".
    let size = match env.config() {
        Ok(cfg) => vec![
            "--width".to_string(),
            cfg.popup_width,
            "--height".to_string(),
            cfg.popup_height,
        ],
        Err(msg) => {
            eprintln!("agent-search: {msg}");
            vec![]
        }
    };
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
        .args(&size)
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
        let dir = testutil::unique_dir(&format!("act-{dir}"));
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
            status_indicators: status::Indicator::Dots,
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

    #[test]
    fn an_act_copies_a_panes_own_priority_token_into_the_state() {
        let panes = json!([
            {"pane_id": "p1", "agent": "claude", "tokens": {"prio": "3"}},
            {"pane_id": "p2", "agent": "claude"},
            {"pane_id": "p3", "agent": "claude", "tokens": {"prio": "x"}},
            {"pane_id": "p4", "agent": "claude", "tokens": {"prio": "9"}},
            {"pane_id": "p5", "agent": "claude", "tokens": {"prio": "0"}},
            {"pane_id": "p6", "agent": "claude", "tokens": {"prio": "2.5"}}
        ]);
        // p1's agent-set priority wins over the saved one; p2 without the token keeps its saved
        // value; p3's non-number, p4's out-of-range and p6's non-integer tokens are shown but not
        // copied; p5's token of 0 clears its saved priority.
        let env = env_with(
            "copytoken",
            panes,
            json!({"prio": {"p1": 1, "p2": 7, "p3": 2, "p5": 4}}),
        );
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let args = vec!["sort-next".to_string()];
        act(&env, &cfg, &mut Snapshot::empty(), &args, true).unwrap_or_else(|e| panic!("{e}"));
        let after = State::load(&env.state_path);
        assert_eq!(after.get("prio"), Some(&json!({"p1": 3, "p2": 7, "p3": 2})));
    }

    #[test]
    fn an_act_on_the_popup_snapshot_does_not_merge_pane_tokens() {
        let panes = json!([{"pane_id": "p1", "agent": "claude", "tokens": {"prio": "3"}}]);
        let env = env_with("snapmerge", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let args = vec!["sort-next".to_string()];
        // prune=false is the popup path: the snapshot can be stale, so its tokens are not saved.
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(State::load(&env.state_path).get("prio"), Some(&json!({})));
        // prune=true is the live-list path: the token is merged.
        act(&env, &cfg, &mut Snapshot::empty(), &args, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("prio"),
            Some(&json!({"p1": 3}))
        );
    }

    #[test]
    fn an_act_on_one_pane_keeps_the_priorities_just_merged_from_other_panes() {
        use std::os::unix::fs::PermissionsExt;
        let panes = json!([
            {"pane_id": "p1", "agent": "claude", "tokens": {"prio": "4"}},
            {"pane_id": "p2", "agent": "claude"}
        ]);
        let mut env = env_with("actmerge", panes, json!({}));
        // A herdr that accepts every report-metadata call, so act prio and act clear succeed.
        let herdr = std::path::PathBuf::from(&env.state_dir).join("herdr");
        std::fs::write(&herdr, "#!/bin/sh\nexit 0\n").unwrap_or_else(|e| panic!("{e}"));
        std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("{e}"));
        env.herdr = herdr.to_string_lossy().into_owned();
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));

        let args: Vec<String> = ["prio", "2", "p2"].map(String::from).to_vec();
        act(&env, &cfg, &mut Snapshot::empty(), &args, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("prio"),
            Some(&json!({"p1": 4, "p2": 2})),
            "p1's merged token and the new priority on p2 both survive"
        );

        let args: Vec<String> = ["clear", "p2"].map(String::from).to_vec();
        act(&env, &cfg, &mut Snapshot::empty(), &args, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("prio"),
            Some(&json!({"p1": 4})),
            "clearing p2 leaves the priority merged from p1's token"
        );
    }

    #[test]
    fn sync_priorities_writes_only_the_priority_map_and_only_when_a_token_differs() {
        use std::os::unix::fs::MetadataExt;
        let panes = json!([
            {"pane_id": "p1", "agent": "claude", "tokens": {"prio": "4"}},
            {"pane_id": "p2", "agent": "claude"}
        ]);
        let env = env_with("sync", panes, json!({"sort": "manual", "prio": {"p2": 2}}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        sync_priorities(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        let after = State::load(&env.state_path);
        assert_eq!(
            after.get("prio"),
            Some(&json!({"p1": 4, "p2": 2})),
            "p1's token is saved; p2 keeps its saved value without a token"
        );
        assert_eq!(after.get("sort"), Some(&json!("manual")));
        // A second sync with nothing new to copy must not rewrite the file; `state::write` would
        // replace it, so the inode changes.
        let inode = std::fs::metadata(&env.state_path).map(|m| m.ino()).ok();
        let mut snap = Snapshot::empty();
        sync_priorities(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            std::fs::metadata(&env.state_path).map(|m| m.ino()).ok(),
            inode
        );
    }

    #[test]
    fn sync_priorities_makes_no_lock_file_when_nothing_differs() {
        let panes = json!([{"pane_id": "p1", "agent": "claude", "tokens": {"prio": "2"}}]);
        let env = env_with("syncnodiff", panes, json!({"prio": {"p1": 2}}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        sync_priorities(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        assert!(!std::path::Path::new(&format!("{}.lock", env.state_path)).exists());
    }

    #[test]
    fn a_priority_changed_since_the_unlocked_read_is_not_overwritten_by_a_stale_token() {
        let panes = json!([
            {"pane_id": "p1", "agent": "claude", "tokens": {"prio": "3"}},
            {"pane_id": "p2", "agent": "claude", "tokens": {"prio": "2"}},
            {"pane_id": "p3", "agent": "claude", "tokens": {"prio": "4"}},
            {"pane_id": "p4", "agent": "claude", "tokens": {"prio": "0"}},
            {"pane_id": "p5", "agent": "claude", "tokens": {"prio": "2"}},
            {"pane_id": "p6", "agent": "claude", "tokens": {"prio": "1"}}
        ]);
        let env = env_with("syncrace", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        let unlocked = State::parse(Some(br#"{"prio":{"p1":3,"p2":4,"p3":1,"p4":2}}"#));
        let rows = snap
            .rows(&env, &cfg, &unlocked)
            .unwrap_or_else(|e| panic!("{e}"));
        // Between the unlocked read and the lock a CLI act set p1 to 5, cleared p2 and set p6 to 9.
        let locked = State::parse(Some(br#"{"prio":{"p1":5,"p3":1,"p4":2,"p6":9}}"#));
        assert_eq!(
            fresh_priorities(&rows, &unlocked, &locked, cfg.priority_max).map(Value::Object),
            Some(json!({"p1": 5, "p3": 4, "p5": 2, "p6": 9})),
            "p1's token must not clobber the 5 the act saved, p2's token must not revive \
             the cleared value, p6's must not overwrite the 9; p3, p4 and p5 are written as usual"
        );
    }

    #[test]
    fn opening_the_popup_saves_a_priority_an_agent_set_on_its_own_pane() {
        let panes = json!([{"pane_id": "p1", "agent": "claude", "tokens": {"prio": "3"}}]);
        let env = env_with("opensync", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        let app = ui::open(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(app.status, "");
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.rows[0].id, "p1");
        assert_eq!(app.rows[0].prio, Some(json!(3)));
        assert_eq!(
            State::load(&env.state_path).get("prio"),
            Some(&json!({"p1": 3}))
        );
    }

    #[test]
    fn open_and_every_reload_apply_the_saved_mode_and_preview_choices() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with(
            "openmode",
            panes,
            json!({"match": "substring", "preview": false, "preview_wrap": true}),
        );
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        let mut app = ui::open(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(app.mode, config::Match::Substring);
        assert!(!app.show_preview);
        assert!(app.preview_wrap);
        // The same choices are re-read when the popup reloads its list.
        std::fs::write(
            &env.state_path,
            r#"{"match":"fuzzy","preview":true,"preview_wrap":false}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        ui::apply_state(&mut app, &cfg, &State::load(&env.state_path));
        assert_eq!(app.mode, config::Match::Fuzzy);
        assert!(app.show_preview);
        assert!(!app.preview_wrap);
    }

    #[test]
    fn a_query_that_is_a_session_id_finds_its_pane_through_the_default_search_fields() {
        // The whole path: the default config's search fields, rows::build and the popup's
        // matching. No token holds the session id, so it can only match via
        // agent_session.value in the default `search` list.
        let panes = json!([
            {"pane_id": "p1", "agent": "claude", "title": "fix", "agent_session": {"value": "sess-42"}},
            {"pane_id": "p2", "agent": "claude", "title": "zz", "tokens": {"repo": "qq"}}
        ]);
        let env = env_with("sessfind", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        let mut app = ui::open(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        for c in "sess-42".chars() {
            app.type_char(c);
        }
        assert_eq!(app.selected_id().as_deref(), Some("p1"));
        assert_eq!(
            app.items
                .iter()
                .filter(|i| matches!(i, ui::Item::Pane(_)))
                .count(),
            1,
            "only the pane carrying the session id matches"
        );
    }

    #[test]
    fn a_failed_sync_opens_the_popup_with_the_error_in_the_status_line() {
        let panes = json!([{"pane_id": "p1", "agent": "claude", "tokens": {"prio": "3"}}]);
        let env = env_with("syncfail", panes, json!({}));
        // A state file that exists but cannot be read: the sync fails, the popup still opens.
        std::fs::remove_file(&env.state_path).unwrap_or_else(|e| panic!("{e}"));
        std::fs::create_dir(&env.state_path).unwrap_or_else(|e| panic!("{e}"));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let mut snap = Snapshot::empty();
        let app = ui::open(&env, &cfg, &mut snap).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(app.status, format!("cannot read {}", env.state_path));
        assert_eq!(app.rows.len(), 1);
    }

    #[test]
    fn restore_does_not_copy_a_panes_token_into_the_state() {
        let panes = json!([{"pane_id": "p1", "agent": "claude", "tokens": {"prio": "4"}}]);
        let env = env_with("restorenocopy", panes, json!({}));
        assert!(restore(&env).is_ok());
        assert_eq!(State::load(&env.state_path).get("prio"), None);
    }

    #[test]
    fn the_preview_is_an_outline_in_ids_metadata_and_tokens_sections() {
        let list = Some(json!({"result": {"panes": [{
            "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
            "agent": "claude", "title": "fix", "session_id": "sx",
            "agent_session": {"value": "s1", "kind": "id"},
            "tokens": {"repo": "a", "flags": "f", "note": {"deep": "d"}, "arr": ["x", "y"]},
            "scroll": 1
        }]}}));
        assert_eq!(
            preview_text(&list, "w1:p1", &[]),
            concat!(
                "[ids]\n",
                "pane_id       w1:p1\n",
                "workspace_id  w1\n",
                "tab_id        w1:t1\n",
                "session_id    sx\n",
                "\n",
                "[metadata]\n",
                "agent          claude\n",
                "title          fix\n",
                "agent_session\n",
                "  value        s1\n",
                "  kind         id\n",
                "\n",
                "[tokens]\n",
                "repo    a\n",
                "flags   f\n",
                "note\n",
                "  deep  d\n",
                "arr\n",
                "  0     x\n",
                "  1     y\n"
            )
        );
        // The wrap metadata: a `key  value` line hangs at its section's value column, a bare
        // outline parent at its indent + 4.
        let lines = preview_lines(&list, "w1:p1", &[]);
        let at = |text: &str| lines.iter().find(|l| l.text == text).map(|l| l.hang);
        assert_eq!(at("[ids]"), Some(0));
        assert_eq!(at("agent          claude"), Some(15));
        assert_eq!(at("agent_session"), Some(4));
        assert_eq!(at("  value        s1"), Some(15));
        assert_eq!(at("note"), Some(4));
        assert_eq!(at("  deep  d"), Some(8));
        // A pane with no tokens and no extra identifiers leaves those sections out.
        let bare = Some(json!({"result": {"panes": [
            {"pane_id": "w1:p1", "title": "fix"}
        ]}}));
        assert_eq!(
            preview_text(&bare, "w1:p1", &[]),
            "[ids]\npane_id  w1:p1\n\n[metadata]\ntitle  fix\n"
        );
    }

    #[test]
    fn a_nested_value_starts_in_the_same_column_as_a_top_level_value_within_its_section() {
        // Inside [metadata] the nested key plus its indent is the widest, so it sets the
        // section's value column and both values line up on it.
        let list = Some(json!({"result": {"panes": [{
            "pane_id": "w1:p1", "agent": "claude",
            "agent_session": {"a_rather_long_session_key": "deep"}
        }]}}));
        let out = preview_text(&list, "w1:p1", &[]);
        let col = |prefix: &str, value: &str| {
            out.lines()
                .find(|l| l.trim_start().starts_with(prefix))
                .and_then(|l| l.find(value))
        };
        let top = col("agent ", "claude");
        assert!(top.is_some(), "{out}");
        assert_eq!(top, col("a_rather_long_session_key", "deep"), "{out}");
    }

    #[test]
    fn a_pane_id_listed_twice_in_the_pane_list_is_previewed_once() {
        let list = Some(json!({"result": {"panes": [
            {"pane_id": "w1:p1", "agent": "claude", "title": "first"},
            {"pane_id": "w1:p1", "agent": "claude", "title": "second", "tokens": {"repo": "x"}}
        ]}}));
        let out = preview_text(&list, "w1:p1", &[]);
        assert!(out.contains("first"), "{out}");
        assert!(!out.contains("second"), "{out}");
        assert_eq!(out.matches("pane_id").count(), 1, "{out}");
    }

    #[test]
    fn preview_hide_leaves_out_named_entries_and_whole_objects() {
        let list = Some(json!({"result": {"panes": [{
            "pane_id": "w1:p1", "workspace_id": "w1", "agent": "claude",
            "agent_session": {"value": "s1", "kind": "id"},
            "tokens": {"repo": "a", "flags": "f"},
            "state_labels": {"x": "y"}
        }]}}));
        let hide = |h: &[&str]| h.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A whole object, a nested entry, and an [ids] field.
        let out = preview_text(
            &list,
            "w1:p1",
            &hide(&[
                "state_labels",
                "tokens.flags",
                "agent_session.kind",
                "workspace_id",
            ]),
        );
        assert!(!out.contains("state_labels"));
        assert!(out.contains("repo") && !out.contains("flags"));
        assert!(out.contains("agent_session") && !out.contains("kind"));
        assert!(!out.contains("workspace_id"));
        // Hiding every [ids] entry drops the section itself.
        let out = preview_text(&list, "w1:p1", &hide(&["pane_id", "workspace_id"]));
        assert!(!out.contains("[ids]"));
        // An empty tokens object leaves the [tokens] section out entirely.
        let empty = Some(json!({"result": {"panes": [
            {"pane_id": "w1:p1", "tokens": {}}
        ]}}));
        assert_eq!(
            preview_text(&empty, "w1:p1", &[]),
            "[ids]\npane_id  w1:p1\n"
        );
    }

    #[test]
    fn a_move_renumbers_over_saved_keys_the_view_does_not_list() {
        let panes = json!([
            {"pane_id": "p1", "agent": "claude"},
            {"pane_id": "p2", "agent": "claude"}
        ]);
        let env = env_with("hidden", panes, json!({"order": {"hidden": 1}}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let args: Vec<String> = ["move", "down", "p1"].map(String::from).to_vec();
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        let after = State::load(&env.state_path);
        let order = after.get("order").cloned().unwrap_or(Value::Null);
        assert_eq!(order, json!({"hidden": 0, "p1": 2, "p2": 1}));
    }

    #[test]
    fn restore_waits_for_the_state_lock_and_fails_when_it_is_held() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("restorelock", panes, json!({"prio": {"p1": 3}}));
        let held =
            state::Lock::acquire(&env.state_dir, &env.state_path).unwrap_or_else(|e| panic!("{e}"));
        let err = restore(&env).err().unwrap_or_default();
        assert!(err.starts_with("cannot lock "), "{err}");
        drop(held);
        // herdr is not installed here, so the one saved priority fails and is reported, not raised.
        assert!(restore(&env).is_ok());
    }

    #[test]
    fn restore_reads_the_pane_list_before_it_waits_for_the_lock() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("restorelist", panes, json!({"prio": {"p1": 3}}));
        std::fs::write(env.list.as_deref().unwrap_or_default(), "garbage")
            .unwrap_or_else(|e| panic!("{e}"));
        let held =
            state::Lock::acquire(&env.state_dir, &env.state_path).unwrap_or_else(|e| panic!("{e}"));
        // The pane list is fetched first, so its error wins over the held lock instead of the
        // fetch running under it.
        let err = restore(&env).err().unwrap_or_default();
        assert_eq!(err, LIST_ERR);
        drop(held);
    }

    #[test]
    fn act_match_next_toggles_the_mode_saved_in_the_state() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("matchnext", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let args = vec!["match-next".to_string()];
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("match"),
            Some(&json!("substring"))
        );
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("match"),
            Some(&json!("fuzzy"))
        );
    }

    #[test]
    fn act_match_next_toggles_from_the_config_when_the_saved_mode_is_missing_or_unknown() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let args = vec!["match-next".to_string()];
        let env = env_with("matchcfg", panes.clone(), json!({}));
        std::fs::write(&env.config_path, r#"{"match":"substring"}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("match"),
            Some(&json!("fuzzy")),
            "substring configured, so the first toggle lands on fuzzy"
        );
        let env = env_with("matchbad", panes, json!({"match": "bogus"}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("match"),
            Some(&json!("substring")),
            "a saved value that is not a mode is ignored and the config default toggles"
        );
    }

    #[test]
    fn the_header_names_the_current_mode_next_to_sort_and_group() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("header", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let st = |s: &str| State::parse(Some(s.as_bytes()));
        assert_eq!(
            header_text(&cfg, &st("{}")),
            "sort: priority  group: none  match: fuzzy\n\
             ctrl-s sort  ctrl-g group  ctrl-r match  ctrl-o preview  ctrl-w wrap  alt-up/alt-down move  alt-1..5 prio  alt-0 clear\n"
        );
        assert!(
            header_text(&cfg, &st(r#"{"match":"substring"}"#))
                .starts_with("sort: priority  group: none  match: substring\n"),
            "the saved mode is shown"
        );
        assert!(
            header_text(&cfg, &st(r#"{"match":"bogus"}"#)).contains("match: fuzzy"),
            "a saved value that is not a mode falls back to the config"
        );
    }

    #[test]
    fn act_preview_next_toggles_the_choice_saved_in_the_state() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("pvnext", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let args = vec!["preview-next".to_string()];
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview"),
            Some(&json!(false))
        );
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview"),
            Some(&json!(true))
        );
    }

    #[test]
    fn act_preview_next_toggles_from_the_config_when_the_saved_choice_is_missing_or_not_a_boolean()
    {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let args = vec!["preview-next".to_string()];
        let env = env_with("pvcfg", panes.clone(), json!({}));
        std::fs::write(&env.config_path, r#"{"preview":false}"#).unwrap_or_else(|e| panic!("{e}"));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview"),
            Some(&json!(true)),
            "preview off configured, so the first toggle lands on true"
        );
        let env = env_with("pvbad", panes, json!({"preview": "bogus"}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview"),
            Some(&json!(false)),
            "a saved value that is not a boolean is ignored and the config default toggles"
        );
    }

    #[test]
    fn act_wrap_next_toggles_the_choice_saved_in_the_state() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("wrnext", panes, json!({}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let args = vec!["wrap-next".to_string()];
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview_wrap"),
            Some(&json!(true))
        );
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview_wrap"),
            Some(&json!(false))
        );
    }

    #[test]
    fn act_wrap_next_toggles_from_the_config_when_the_saved_choice_is_missing_or_not_a_boolean() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let args = vec!["wrap-next".to_string()];
        let env = env_with("wrcfg", panes.clone(), json!({}));
        std::fs::write(&env.config_path, r#"{"preview_wrap":true}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview_wrap"),
            Some(&json!(false)),
            "wrap on configured, so the first toggle lands on false"
        );
        let env = env_with("wrbad", panes, json!({"preview_wrap": "bogus"}));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        act(&env, &cfg, &mut Snapshot::empty(), &args, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            State::load(&env.state_path).get("preview_wrap"),
            Some(&json!(true)),
            "a saved value that is not a boolean is ignored and the config default toggles"
        );
    }

    #[test]
    fn restore_works_when_the_configs_keys_cannot_be_bound() {
        let panes = json!([{"pane_id": "p1", "agent": "claude"}]);
        let env = env_with("restorekeys", panes, json!({"prio": {"p1": 3}}));
        std::fs::write(&env.config_path, r#"{"keys":[]}"#).unwrap_or_else(|e| panic!("{e}"));
        // herdr is not installed here, so the one saved priority fails and is reported, not raised.
        assert!(restore(&env).is_ok());
    }

    #[test]
    fn restore_skips_a_saved_priority_that_changed_after_it_read_the_state() {
        use std::os::unix::fs::PermissionsExt;
        let panes = json!([
            {"pane_id": "p1", "agent": "claude"},
            {"pane_id": "p2", "agent": "claude"}
        ]);
        let mut env = env_with("restorerecheck", panes, json!({"prio": {"p1": 4, "p2": 3}}));
        let dir = std::path::PathBuf::from(&env.state_dir);
        let calls = dir.join("calls");
        // The fake herdr stands in for a concurrent `act prio 9 p2`: while restore is in p1's
        // report-metadata call, the saved priority of p2 becomes 9, so restore must not send p2's
        // old value 3.
        let herdr = dir.join("herdr");
        std::fs::write(
            &herdr,
            format!(
                "#!/bin/sh\necho \"$*\" >> \"{}\"\n[ \"$3\" = \"p1\" ] && printf %s '{}' > \"{}\"\nexit 0\n",
                calls.display(),
                r#"{"prio":{"p1":4,"p2":9}}"#,
                env.state_path,
            ),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("{e}"));
        env.herdr = herdr.to_string_lossy().into_owned();
        assert!(restore(&env).is_ok());
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap_or_default(),
            "pane report-metadata p1 --source t --token prio=4\n"
        );
    }

    #[test]
    fn selecting_a_sort_that_fails_to_build_keeps_the_old_state() {
        let panes = json!([{"pane_id": "p1", "agent": "claude", "tokens": {"repo": "a"}}]);
        let env = env_with("badsort", panes, json!({"group": "x"}));
        std::fs::write(
            &env.config_path,
            r#"{"sorts":[{"name":"a","by":["title"]},{"name":"bad","by":["tokens.repo.x"]}]}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let cfg = env.config().unwrap_or_else(|e| panic!("{e}"));
        let before = std::fs::read(&env.state_path).unwrap_or_default();
        let args = vec!["sort-next".to_string()];
        let err = act(&env, &cfg, &mut Snapshot::empty(), &args, true).err();
        assert_eq!(err.as_deref(), Some(LIST_ERR));
        assert_eq!(std::fs::read(&env.state_path).unwrap_or_default(), before);
    }
}
