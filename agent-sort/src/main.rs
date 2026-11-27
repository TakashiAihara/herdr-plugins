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
                "agent-sort-{name}-{}-{nanos}-{n}",
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
mod request;
mod state;
mod ui;

use config::{Config, Preset};
use serde_json::Value;
use state::Saved;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

type Res<T> = Result<T, String>;

struct Env {
    herdr: String,
    config_dir: String,
    config_path: String,
    state_dir: String,
    state_path: String,
    socket: String,
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
            format!("{base}/herdr-agent-sort")
        });
        let state_dir = var("HERDR_PLUGIN_STATE_DIR").unwrap_or_else(|| {
            let base = var("XDG_STATE_HOME").unwrap_or_else(|| format!("{home}/.local/state"));
            format!("{base}/herdr-agent-sort")
        });
        Env {
            herdr: var("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            config_path: format!("{config_dir}/config.toml"),
            config_dir,
            state_path: format!("{state_dir}/state.json"),
            state_dir,
            socket: var("HERDR_SOCKET_PATH").unwrap_or_default(),
            plugin_id: var("HERDR_PLUGIN_ID").unwrap_or_else(|| "takashiaihara.agent-sort".into()),
        }
    }

    #[cfg(test)]
    pub fn test(config_dir: &str, state_dir: &str) -> Env {
        Env {
            herdr: "true".into(),
            config_path: format!("{config_dir}/config.toml"),
            config_dir: config_dir.into(),
            state_path: format!("{state_dir}/state.json"),
            state_dir: state_dir.into(),
            socket: String::new(),
            plugin_id: "test.agent-sort".into(),
        }
    }

    fn config(&self) -> Res<Config> {
        config::load(&self.config_path)
    }

    // One newline-delimited JSON request over Herdr's Unix socket. A response with an `error`
    // object fails with the response's `message`.
    fn call(&self, req: &Value) -> Res<Value> {
        if self.socket.is_empty() {
            return Err("HERDR_SOCKET_PATH is not set".into());
        }
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|e| format!("cannot connect to {}: {e}", self.socket))?;
        let timeout = Duration::from_secs(5);
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|_| stream.set_write_timeout(Some(timeout)))
            .map_err(|e| format!("cannot configure {}: {e}", self.socket))?;
        let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .map_err(|e| format!("cannot write to {}: {e}", self.socket))?;
        let mut resp = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut resp)
            .map_err(|e| format!("cannot read from {}: {e}", self.socket))?;
        let v: Value = serde_json::from_str(resp.trim_end())
            .map_err(|e| format!("invalid reply from {}: {e}", self.socket))?;
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| err.to_string(), String::from);
            return Err(msg);
        }
        Ok(v)
    }

    // `herdr notification show`; output is captured, never inherited, and a failure changes
    // nothing: a notification is nice to have, not part of the result. Herdr sanitizes and trims
    // the title itself, so it goes as is.
    fn notify(&self, title: &str, body: Option<&str>) {
        let mut args = vec!["notification".into(), "show".into(), title.to_string()];
        if let Some(b) = body {
            args.push("--body".into());
            args.push(b.into());
        }
        let _ = Command::new(&self.herdr).args(&args).output();
    }

    fn notify_ok(&self, title: &str) {
        self.notify(title, None);
    }

    fn notify_error(&self, msg: &str) {
        self.notify("agent-sort: error", Some(msg));
    }
}

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

// The preset the plugin counts as active: the state's name when it is still in the config,
// nothing when the state says cleared, else the config's default.
fn current<'a>(cfg: &'a Config, saved: &'a Saved) -> Option<&'a str> {
    match saved {
        Saved::Named(n) if cfg.preset(n).is_some() => Some(n.as_str()),
        Saved::Cleared => None,
        Saved::Named(_) | Saved::Unset => cfg.default.as_deref(),
    }
}

// Sends the set request, then saves the preset now that Herdr accepted it.
fn apply_raw(env: &Env, preset: &Preset) -> Res<()> {
    env.call(&request::set(&env.plugin_id, preset))?;
    state::write(&env.state_dir, &env.state_path, Some(&preset.name))
}

// apply_raw plus the notifications the apply subcommands share.
fn apply(env: &Env, preset: &Preset) -> Res<()> {
    let result = apply_raw(env, preset);
    match &result {
        Ok(()) => env.notify_ok(&format!("Agents sorted by {}", preset.name)),
        Err(e) => env.notify_error(e),
    }
    result
}

// Errors before `apply` runs still get the failure notification.
fn notified(env: &Env, e: String) -> String {
    env.notify_error(&e);
    e
}

fn presets(env: &Env) -> Res<()> {
    let cfg = env.config()?;
    let mut text = String::new();
    for p in &cfg.presets {
        text.push_str(&p.name);
        text.push('\n');
    }
    out(&text);
    Ok(())
}

// The preset `next` applies: the one after the current, wrapping; the first when the current is
// none. The current is the state's name when it is in the config, else the config's default.
fn next_preset<'a>(cfg: &'a Config, saved: &Saved) -> &'a Preset {
    let i = match current(cfg, saved).and_then(|n| cfg.index(n)) {
        Some(i) => (i + 1) % cfg.presets.len(),
        None => 0,
    };
    &cfg.presets[i]
}

fn next(env: &Env) -> Res<()> {
    let cfg = env.config().map_err(|e| notified(env, e))?;
    let saved = state::read(&env.state_path);
    apply(env, next_preset(&cfg, &saved))
}

fn set(env: &Env, name: &str) -> Res<()> {
    let cfg = env.config().map_err(|e| notified(env, e))?;
    let Some(preset) = cfg.preset(cfg.resolve(name)) else {
        return Err(notified(env, format!("unknown preset {name:?}")));
    };
    apply(env, preset)
}

// What the Agents view looks like after `agent.view.clear` with our source.
pub enum ClearOutcome {
    // Nothing is overriding the view anymore.
    Cleared,
    // Our clear did not own the active view, so another source's view is still on: it names the
    // remaining view's source.
    Kept(String),
}

// What the clear result says: `active` and `source` describe the override left after our clear,
// so `active` means another source holds the view (Herdr never reports our source there: a clear
// that owned the view removes it). A reply without that shape is not an answer.
fn clear_outcome(resp: &Value) -> Res<ClearOutcome> {
    let unexpected = || format!("unexpected reply from Herdr: {resp}");
    let result = resp.get("result").ok_or_else(unexpected)?;
    match result.get("active") {
        Some(Value::Bool(true)) => {
            let source = result
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("another source");
            Ok(ClearOutcome::Kept(source.to_string()))
        }
        Some(Value::Bool(false)) => Ok(ClearOutcome::Cleared),
        _ => Err(unexpected()),
    }
}

// The one-line outcome the notification and the popup's status line share.
fn clear_message(outcome: &ClearOutcome) -> String {
    match outcome {
        ClearOutcome::Cleared => "Agents sort cleared".into(),
        ClearOutcome::Kept(source) => format!("Agents view is set by {source}; left as is"),
    }
}

// Sends the clear, then saves `{"preset":null}`: our preset is not applied in either outcome.
fn clear_view(env: &Env) -> Res<ClearOutcome> {
    let resp = env.call(&request::clear(&env.plugin_id))?;
    let outcome = clear_outcome(&resp)?;
    state::write(&env.state_dir, &env.state_path, None)?;
    Ok(outcome)
}

fn clear(env: &Env) -> Res<()> {
    let outcome = clear_view(env).map_err(|e| notified(env, e))?;
    env.notify_ok(&clear_message(&outcome));
    Ok(())
}

// What restore does: the preset to apply (if any), and the notification body when the state's
// name is gone from the config — it names the default that takes over, or says nothing is
// applied. A warning always means the state file must be removed so the next startup does not
// warn again.
fn restore_plan<'a>(cfg: &'a Config, saved: &Saved) -> (Option<&'a Preset>, Option<String>) {
    let warn = match saved {
        Saved::Named(n) if cfg.preset(n).is_none() => Some(match &cfg.default {
            Some(d) => format!(
                "the saved preset {n:?} is no longer in the config; applying the default {d:?}"
            ),
            None => {
                format!("the saved preset {n:?} is no longer in the config; nothing is applied")
            }
        }),
        _ => None,
    };
    (current(cfg, saved).and_then(|n| cfg.preset(n)), warn)
}

// The startup hook: re-apply what the state (or the config default) says. Success is silent;
// failures notify. A saved preset the config no longer has gets one error notification and its
// state file is removed, then the config default is applied when there is one.
fn restore(env: &Env) -> Res<()> {
    let cfg = env.config().map_err(|e| notified(env, e))?;
    let saved = state::read(&env.state_path);
    let (preset, warn) = restore_plan(&cfg, &saved);
    if let Some(w) = warn {
        env.notify_error(&w);
        state::remove(&env.state_path).map_err(|e| notified(env, e))?;
    }
    let Some(preset) = preset else {
        return Ok(());
    };
    env.call(&request::set(&env.plugin_id, preset))
        .map_err(|e| notified(env, e))?;
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

pub enum EditResult {
    // The current preset — the state's name, or the config default it falls back to — was
    // re-applied.
    Applied(String),
    // The state's preset is gone from the new config and there is no default; our view was cleared.
    Gone(String),
    // The state's preset (first) is gone from the new config, so the default (second) was applied.
    Replaced(String, String),
    // There is no current preset to re-apply.
    Nothing,
}

// The editor argv: the variable split on whitespace, then the config path last.
fn editor_argv(editor: &str, path: &str) -> Vec<String> {
    editor
        .split_whitespace()
        .map(String::from)
        .chain(std::iter::once(path.to_string()))
        .collect()
}

fn run_editor(env: &Env) -> Res<()> {
    // A blank value would leave the config path as the command to run.
    let set = |k: &str| var(k).filter(|v| !v.trim().is_empty());
    let editor = set("VISUAL")
        .or_else(|| set("EDITOR"))
        .unwrap_or_else(|| "vi".into());
    let argv = editor_argv(&editor, &env.config_path);
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| format!("cannot run the editor {:?}: {e}", argv[0]))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the editor {:?} exited with {status}", argv[0]))
    }
}

// The `edit` flow, shared with the popup's edit key: write the template when there is no config,
// open the editor, reload, and re-apply the current preset — the state's name when the new config
// still has it, else the default — so the view and the plugin agree.
fn edit_flow(env: &Env) -> Res<EditResult> {
    if !Path::new(&env.config_path).exists() {
        std::fs::create_dir_all(&env.config_dir)
            .map_err(|_| format!("cannot create {}", env.config_dir))?;
        std::fs::write(&env.config_path, config::TEMPLATE)
            .map_err(|_| format!("cannot write {}", env.config_path))?;
    }
    run_editor(env)?;
    let cfg = env.config()?;
    let saved = state::read(&env.state_path);
    // current() falls back to the default for a dropped state name; report the drop itself.
    let dropped = match &saved {
        Saved::Named(n) if cfg.preset(n).is_none() => Some(n.clone()),
        _ => None,
    };
    // The drop is reported here, so forget the name: restore would report it again at startup.
    if dropped.is_some() {
        state::remove(&env.state_path)?;
    }
    match current(&cfg, &saved).and_then(|n| cfg.preset(n)) {
        Some(preset) => {
            env.call(&request::set(&env.plugin_id, preset))?;
            Ok(match dropped {
                Some(gone) => EditResult::Replaced(gone, preset.name.clone()),
                None => EditResult::Applied(preset.name.clone()),
            })
        }
        None => match dropped {
            // The dropped preset is still the view; take ours back down so Herdr's own sort shows.
            Some(gone) => {
                env.call(&request::clear(&env.plugin_id))?;
                Ok(EditResult::Gone(gone))
            }
            None => Ok(EditResult::Nothing),
        },
    }
}

// The notification for an edit result; returns the status line version of it for the popup.
fn notify_edit(env: &Env, result: &EditResult) -> String {
    match result {
        EditResult::Applied(name) => {
            env.notify_ok(&format!("Agents sorted by {name}"));
            format!("applied {name}")
        }
        EditResult::Gone(name) => {
            env.notify_ok(&format!("Agents sort preset {name:?} is gone"));
            format!("preset {name:?} is gone")
        }
        EditResult::Replaced(gone, name) => {
            env.notify_ok(&format!(
                "Agents sort preset {gone:?} is gone; sorted by {name}"
            ));
            format!("preset {gone:?} is gone; applied {name}")
        }
        EditResult::Nothing => "config saved".to_string(),
    }
}

fn edit(env: &Env) -> Res<()> {
    match edit_flow(env) {
        Ok(result) => {
            notify_edit(env, &result);
            Ok(())
        }
        Err(e) => Err(notified(env, e)),
    }
}

fn usage() -> ! {
    eprintln!(
        "agent-sort: usage: agent-sort probe|presets|next|set <name>|clear|restore|open|edit|ui"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::new();
    let rest = &args[args.len().min(1)..];
    let result = match args.first().map(String::as_str).unwrap_or("") {
        "probe" => {
            out("ok\n");
            Ok(())
        }
        "presets" => presets(&env),
        "next" => next(&env),
        "set" => match rest.first() {
            Some(name) => set(&env, name),
            None => usage(),
        },
        "clear" => clear(&env),
        "restore" => restore(&env),
        "open" => std::process::exit(open(&env)),
        "edit" => edit(&env),
        "ui" => ui::run(&env),
        _ => usage(),
    };
    if let Err(msg) = result {
        eprintln!("agent-sort: {msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(names: &[&str], default: Option<&str>) -> Config {
        Config::test(names, default)
    }

    #[test]
    fn current_is_the_state_name_then_cleared_then_the_default() {
        let c = cfg(&["a", "b"], Some("b"));
        assert_eq!(current(&c, &Saved::Named("a".into())), Some("a"));
        assert_eq!(current(&c, &Saved::Cleared), None);
        assert_eq!(current(&c, &Saved::Unset), Some("b"));
        assert_eq!(
            current(&c, &Saved::Named("gone".into())),
            Some("b"),
            "a state name not in the config falls back to the default"
        );
        let no_default = cfg(&["a"], None);
        assert_eq!(current(&no_default, &Saved::Unset), None);
        assert_eq!(current(&no_default, &Saved::Named("gone".into())), None);
    }

    #[test]
    fn next_cycles_wraps_and_starts_from_the_first_preset() {
        let c = cfg(&["a", "b", "c"], None);
        assert_eq!(next_preset(&c, &Saved::Unset).name, "a");
        assert_eq!(next_preset(&c, &Saved::Cleared).name, "a");
        assert_eq!(next_preset(&c, &Saved::Named("a".into())).name, "b");
        assert_eq!(next_preset(&c, &Saved::Named("c".into())).name, "a");
        assert_eq!(
            next_preset(&c, &Saved::Named("gone".into())).name,
            "a",
            "a gone preset with no default starts from the first"
        );
    }

    #[test]
    fn next_starts_from_the_config_default_when_the_state_is_unset_or_gone() {
        let c = cfg(&["prio", "repo"], Some("prio"));
        assert_eq!(next_preset(&c, &Saved::Unset).name, "repo");
        assert_eq!(
            next_preset(&c, &Saved::Named("gone".into())).name,
            "repo",
            "a gone preset counts as the default, so next continues after it"
        );
    }

    #[test]
    fn restore_reapplies_warns_on_a_gone_preset_and_falls_back_to_the_default() {
        let c = cfg(&["a", "b"], Some("b"));
        let (p, w) = restore_plan(&c, &Saved::Named("a".into()));
        assert_eq!(p.map(|p| p.name.as_str()), Some("a"));
        assert_eq!(w, None);
        let (p, w) = restore_plan(&c, &Saved::Named("gone".into()));
        assert_eq!(
            p.map(|p| p.name.as_str()),
            Some("b"),
            "the config default applies"
        );
        assert_eq!(
            w.as_deref(),
            Some(
                "the saved preset \"gone\" is no longer in the config; applying the default \"b\""
            )
        );
        let no_default = cfg(&["a"], None);
        let (p, w) = restore_plan(&no_default, &Saved::Named("gone".into()));
        assert_eq!(p.map(|p| p.name.as_str()), None, "nothing to fall back to");
        assert_eq!(
            w.as_deref(),
            Some("the saved preset \"gone\" is no longer in the config; nothing is applied")
        );
        let (p, w) = restore_plan(&c, &Saved::Cleared);
        assert_eq!((p.is_none(), w.is_none()), (true, true));
        let (p, w) = restore_plan(&c, &Saved::Unset);
        assert_eq!((p.map(|p| p.name.as_str()), w.is_none()), (Some("b"), true));
        let (p, w) = restore_plan(&no_default, &Saved::Unset);
        assert_eq!((p.is_none(), w.is_none()), (true, true));
    }

    #[test]
    fn a_clear_result_reads_the_view_left_afterwards_or_errors() {
        let cleared = clear_outcome(&serde_json::json!({
            "result": {"type": "agent_view", "active": false}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(clear_message(&cleared), "Agents sort cleared");
        let kept = clear_outcome(&serde_json::json!({
            "result": {"type": "agent_view", "active": true, "source": "plugin:other.thing", "label": "x"}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            clear_message(&kept),
            "Agents view is set by plugin:other.thing; left as is"
        );
        let kept_unnamed = clear_outcome(&serde_json::json!({
            "result": {"type": "agent_view", "active": true}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            clear_message(&kept_unnamed),
            "Agents view is set by another source; left as is"
        );
        for bad in [
            serde_json::json!({"id": "agent-sort"}),
            serde_json::json!({"result": {"type": "agent_view"}}),
            serde_json::json!({"result": {"type": "agent_view", "active": "yes"}}),
        ] {
            let err = clear_outcome(&bad).err().unwrap_or_default();
            assert!(err.contains("unexpected reply from Herdr"), "{bad}: {err}");
        }
    }

    #[test]
    fn the_editor_argv_splits_on_whitespace_and_puts_the_path_last() {
        assert_eq!(
            editor_argv("code --wait", "/c/config.toml"),
            ["code", "--wait", "/c/config.toml"]
        );
        assert_eq!(
            editor_argv("vi", "/c/config.toml"),
            ["vi", "/c/config.toml"]
        );
        assert_eq!(
            editor_argv("  emacs  -nw ", "/c/config.toml"),
            ["emacs", "-nw", "/c/config.toml"]
        );
    }
}
