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
    // nothing: a notification is nice to have, not part of the result.
    fn notify(&self, title: &str, body: Option<&str>) {
        let title: String = title
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(79)
            .collect();
        let mut args = vec!["notification".into(), "show".into(), title];
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

// The preset the plugin counts as active: the state's name, nothing when the state says cleared,
// else the config's default.
fn current<'a>(cfg: &'a Config, saved: &'a Saved) -> Option<&'a str> {
    match saved {
        Saved::Named(n) => Some(n.as_str()),
        Saved::Cleared => None,
        Saved::Unset => cfg.default.as_deref(),
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
// unset or no longer in the config.
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
    let Some(preset) = cfg.preset(name) else {
        return Err(notified(env, format!("unknown preset {name:?}")));
    };
    apply(env, preset)
}

fn clear(env: &Env) -> Res<()> {
    env.call(&request::clear(&env.plugin_id))
        .map_err(|e| notified(env, e))?;
    state::write(&env.state_dir, &env.state_path, None).map_err(|e| notified(env, e))?;
    env.notify_ok("Agents sort cleared");
    Ok(())
}

// The startup hook: re-apply what the state (or the config default) says, without touching the
// state and without a notification.
fn restore(env: &Env) -> Res<()> {
    let cfg = env.config().map_err(|e| notified(env, e))?;
    let saved = state::read(&env.state_path);
    let Some(preset) = current(&cfg, &saved).and_then(|n| cfg.preset(n)) else {
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
    // The current preset still exists in the new config and was re-applied.
    Applied(String),
    // The current preset is gone from the new config; nothing was sent.
    Gone(String),
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
    let editor = var("VISUAL")
        .or_else(|| var("EDITOR"))
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
// open the editor, reload, and re-apply the current preset when the new config still has it.
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
    let Some(name) = current(&cfg, &saved).map(String::from) else {
        return Ok(EditResult::Nothing);
    };
    match cfg.preset(&name) {
        Some(preset) => {
            env.call(&request::set(&env.plugin_id, preset))?;
            Ok(EditResult::Applied(name))
        }
        None => Ok(EditResult::Gone(name)),
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
        assert_eq!(current(&c, &Saved::Named("x".into())), Some("x"));
        assert_eq!(current(&c, &Saved::Cleared), None);
        assert_eq!(current(&c, &Saved::Unset), Some("b"));
        let no_default = cfg(&["a"], None);
        assert_eq!(current(&no_default, &Saved::Unset), None);
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
            "a preset the config no longer has starts from the first"
        );
    }

    #[test]
    fn next_starts_from_the_config_default_when_the_state_is_unset() {
        let c = cfg(&["prio", "repo"], Some("prio"));
        assert_eq!(next_preset(&c, &Saved::Unset).name, "repo");
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
