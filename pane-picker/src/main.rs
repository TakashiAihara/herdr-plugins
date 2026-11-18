mod config;
mod rows;
mod state;

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
            format!("{base}/herdr-pane-picker")
        });
        let state_dir = var("HERDR_PLUGIN_STATE_DIR").unwrap_or_else(|| {
            let base = var("XDG_STATE_HOME").unwrap_or_else(|| format!("{home}/.local/state"));
            format!("{base}/herdr-pane-picker")
        });
        Env {
            herdr: var("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            config_path: format!("{config_dir}/config.json"),
            state_path: format!("{state_dir}/state.json"),
            state_dir,
            list: var("PANE_PICKER_LIST"),
            plugin_id: var("HERDR_PLUGIN_ID").unwrap_or_else(|| "takashiaihara.pane-picker".into()),
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
                    .stderr(Stdio::inherit())
                    .output()
                    .map_err(|_| err())?;
                if !out.status.success() {
                    return Err(err());
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

    fn rows(&self, cfg: &Config, st: &State) -> Res<Vec<Row>> {
        rows::build(self.pane_list()?.as_ref(), cfg, st)
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
    out(&rows::lines(&env.rows(&cfg, &st)?));
    Ok(())
}

fn header(env: &Env) -> Res<()> {
    let cfg = env.config()?;
    let st = State::load(&env.state_path);
    let sort = &cfg.sorts[pick(&cfg.sorts, st.get("sort"))].name;
    let group = &cfg.groups[pick(&cfg.groups, st.get("group"))].name;
    let k = &cfg.keys;
    out(&format!(
        "sort: {sort}  group: {group}\n{} sort  {} group  {}/{} move  alt-1..9 prio  {} clear\n",
        k.sort, k.group, k.up, k.down, k.clear
    ));
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

fn act(env: &Env, args: &[String]) -> Res<()> {
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
    let op = arg(0);
    let cfg = env.config()?;
    let st = State::load(&env.state_path);
    let mut new = st.0.clone();

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
    let rows = env.rows(&cfg, &st)?;

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
                    let mut order: Map<String, Value> = Map::new();
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
                let mut prio = object(st.get("prio"));
                if op == "clear" {
                    prio.remove(&r.key);
                } else {
                    prio.insert(r.key.clone(), Value::from(n.parse::<u64>().unwrap_or(0)));
                }
                new.insert("prio".into(), Value::Object(prio));
            }
        }
    }

    // Every write drops priorities and positions of panes that are gone, so the state does not grow.
    for k in ["prio", "order"] {
        let mut m = object(new.get(k));
        m.retain(|key, _| rows.iter().any(|r| &r.key == key));
        new.insert(k.into(), Value::Object(m));
    }
    state::write(&env.state_dir, &env.state_path, &Value::Object(new))
}

fn raw(v: Option<&Value>) -> String {
    v.map_or_else(|| "null".to_string(), tostring)
}

fn run_ok(herdr: &str, args: &[&str], quiet_err: bool) -> bool {
    Command::new(herdr)
        .args(args)
        .stdout(Stdio::null())
        .stderr(if quiet_err {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .status()
        .is_ok_and(|s| s.success())
}

// Herdr has no focus-by-pane-id for plain terminals: agent panes go through `agent focus` (which also
// marks a done agent seen, as looking at it in Herdr does); others, and an agent that exited since
// the list was taken, focus their workspace and tab.
// shortcut: a plain terminal sharing a tab with other panes lands on the tab, not the pane;
// walk pane.layout with pane.focus_direction if that case shows up.
fn focus(env: &Env, id: &str) -> Res<()> {
    if id.is_empty() || id.starts_with("-:") {
        return Ok(());
    }
    let list = env.pane_list()?;
    let panes = lenient_panes(&list);
    let Some(pane) = pane_with_id(&panes, id).into_iter().next() else {
        return Err(format!("pane {id} is gone"));
    };
    let has_agent = match present(pane.get("agent")) {
        None => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    if has_agent && run_ok(&env.herdr, &["agent", "focus", id], true) {
        return Ok(());
    }
    let ws = raw(pane.get("workspace_id"));
    let tab = raw(pane.get("tab_id"));
    if run_ok(&env.herdr, &["workspace", "focus", &ws], false)
        && run_ok(&env.herdr, &["tab", "focus", &tab], false)
    {
        Ok(())
    } else {
        Err(format!("cannot focus {id}"))
    }
}

fn preview(env: &Env, id: &str) -> Res<()> {
    let list = env.pane_list()?;
    let mut text = String::new();
    for pane in pane_with_id(&lenient_panes(&list), id) {
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
    out(&text);
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
            "picker",
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
        "ui" => Err("ui is not implemented yet".to_string()),
        "lines" => lines(&env),
        "header" => header(&env),
        "enter" => {
            enter(first(0));
            Ok(())
        }
        "act" => act(&env, rest),
        "focus" => focus(&env, first(0)),
        "preview" => preview(&env, first(0)),
        _ => Err("usage: picker.sh open|ui|lines|header|enter|act|focus|preview".to_string()),
    };
    if let Err(msg) = result {
        eprintln!("pane-picker: {msg}");
        std::process::exit(1);
    }
}
