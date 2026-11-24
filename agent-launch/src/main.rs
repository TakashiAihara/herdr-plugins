#[cfg(test)]
mod testutil {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, MutexGuard};

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
                "agent-launch-{name}-{}-{nanos}-{n}",
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

    // Tests that write an executable and spawn processes take this lock. A fork in one test
    // inherits the descriptor another test is still writing its script through, and executing
    // that script then fails with "Text file busy".
    pub fn serial() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // A herdr that records each call (one argument per line, calls separated by `--`) and answers
    // the commands named in `handlers` with the given shell snippet; anything else succeeds with
    // no output. It never reaches a real herdr.
    pub struct Fake {
        pub bin: String,
        dir: std::path::PathBuf,
    }

    pub fn fake_herdr(handlers: &[(&str, &str)]) -> Fake {
        let dir = unique_dir("fake");
        let log = dir.join("log");
        let mut script = format!(
            "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; echo '--'; }} >> {}\ncase \"$1 $2\" in\n",
            crate::render::quote(log.to_str().unwrap())
        );
        for (cmd, body) in handlers {
            script.push_str(&format!("'{cmd}') {body} ;;\n"));
        }
        script.push_str("esac\nexit 0\n");
        let bin = dir.join("herdr");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake {
            bin: bin.to_str().unwrap().to_string(),
            dir,
        }
    }

    impl Fake {
        pub fn calls(&self) -> Vec<Vec<String>> {
            let text = std::fs::read_to_string(self.dir.join("log")).unwrap_or_default();
            let mut calls = vec![];
            let mut cur = vec![];
            for line in text.lines() {
                if line == "--" {
                    calls.push(std::mem::take(&mut cur));
                } else {
                    cur.push(line.to_string());
                }
            }
            calls
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

mod config;
mod launch;
mod render;
mod ui;

use serde_json::Value;

type Res<T> = Result<T, String>;

pub struct Env {
    herdr: String,
    config_path: String,
    plugin_id: String,
    cwd: Option<String>,
}

// Like bash `${VAR:-}`: an empty variable counts as unset.
fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

impl Env {
    fn new() -> Env {
        let config_dir = var("HERDR_PLUGIN_CONFIG_DIR").unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            let base = var("XDG_CONFIG_HOME").unwrap_or_else(|| format!("{home}/.config"));
            format!("{base}/herdr-agent-launch")
        });
        Env {
            herdr: var("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            config_path: format!("{config_dir}/config.json"),
            plugin_id: var("HERDR_PLUGIN_ID")
                .unwrap_or_else(|| "takashiaihara.agent-launch".into()),
            cwd: var("AGENT_LAUNCH_CWD"),
        }
    }
}

// The pane you were in decides where the new workspace starts, so a command without a repo
// (claude, or a launcher given none) runs in that repo.
fn open(env: &Env) -> Res<()> {
    let list = launch::herdr(&env.herdr, &["pane", "list"])?;
    let list = launch::first_json(&list).ok_or("herdr pane list did not return JSON")?;
    let panes = list["result"]["panes"]
        .as_array()
        .ok_or("herdr pane list has no panes")?;
    // Herdr 0.9.5 marks one pane in all workspaces; if that ever changes, the first could be wrong.
    let focused: Vec<&Value> = panes.iter().filter(|p| p["focused"] == true).collect();
    let [pane] = focused.as_slice() else {
        return Err(if focused.is_empty() {
            "no focused pane in herdr pane list"
        } else {
            "more than one focused pane"
        }
        .into());
    };
    let cwd = pane["cwd"]
        .as_str()
        .filter(|c| !c.is_empty())
        .ok_or("the focused pane has no cwd")?;
    launch::herdr(
        &env.herdr,
        &[
            "plugin",
            "pane",
            "open",
            "--plugin",
            &env.plugin_id,
            "--entrypoint",
            "prompt",
            "--env",
            &format!("AGENT_LAUNCH_CWD={cwd}"),
        ],
    )
    .map(drop)
}

fn main() {
    let result = match std::env::args().nth(1).as_deref().unwrap_or("") {
        "open" => open(&Env::new()),
        "ui" => ui::run(&Env::new()),
        // The prebuilt binary's install step runs this to see that it starts on this machine.
        "version" => {
            println!("agent-launch {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err("usage: agent-launch open|ui|version".to_string()),
    };
    if let Err(msg) = result {
        eprintln!("agent-launch: {msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fake_herdr, serial, Fake};

    fn env_for(f: &Fake) -> Env {
        Env {
            herdr: f.bin.clone(),
            config_path: "/nonexistent/config.json".into(),
            plugin_id: "takashiaihara.agent-launch".into(),
            cwd: None,
        }
    }

    fn list_reply(json: &str) -> String {
        // printf, not echo: sh's echo turns a JSON `\b` into a backspace.
        format!("printf '%s' '{json}'")
    }

    fn opened(f: &Fake) -> Vec<Vec<String>> {
        f.calls()
            .into_iter()
            .filter(|c| c[..2] == ["plugin", "pane"])
            .collect()
    }

    #[test]
    fn open_passes_the_focused_panes_cwd_as_one_env_argument() {
        let _g = serial();
        let f = fake_herdr(&[(
            "pane list",
            &list_reply(
                r#"{"result":{"panes":[{"agent":"claude","cwd":"/a","focused":false,"pane_id":"p1"},{"cwd":"/b c","focused":true,"pane_id":"p2"}]}}"#,
            ),
        )]);
        open(&env_for(&f)).unwrap();
        assert_eq!(
            opened(&f),
            [[
                "plugin",
                "pane",
                "open",
                "--plugin",
                "takashiaihara.agent-launch",
                "--entrypoint",
                "prompt",
                "--env",
                "AGENT_LAUNCH_CWD=/b c"
            ]]
        );
    }

    #[test]
    fn open_without_a_focused_pane_fails_and_opens_nothing() {
        let _g = serial();
        let f = fake_herdr(&[(
            "pane list",
            &list_reply(r#"{"result":{"panes":[{"cwd":"/a","focused":false,"pane_id":"p1"}]}}"#),
        )]);
        let err = open(&env_for(&f)).unwrap_err();
        assert!(err.contains("no focused pane"), "{err}");
        assert!(opened(&f).is_empty());
    }

    #[test]
    fn open_counts_only_panes_that_say_focused_true() {
        let _g = serial();
        let f = fake_herdr(&[(
            "pane list",
            &list_reply(
                r#"{"result":{"panes":[{"cwd":"/a","pane_id":"p1"},{"cwd":"/n","focused":null,"pane_id":"p3"},{"cwd":"/b","focused":true,"pane_id":"p2"}]}}"#,
            ),
        )]);
        open(&env_for(&f)).unwrap();
        assert_eq!(opened(&f)[0][8], "AGENT_LAUNCH_CWD=/b");
    }

    #[test]
    fn open_with_two_focused_panes_refuses_instead_of_taking_the_first() {
        let _g = serial();
        let f = fake_herdr(&[(
            "pane list",
            &list_reply(
                r#"{"result":{"panes":[{"cwd":"/a","focused":true,"pane_id":"p1"},{"cwd":"/b","focused":true,"pane_id":"p2"}]}}"#,
            ),
        )]);
        let err = open(&env_for(&f)).unwrap_err();
        assert!(err.contains("more than one focused pane"), "{err}");
        assert!(opened(&f).is_empty());
    }

    #[test]
    fn open_takes_a_cwd_with_json_escapes_as_it_really_is() {
        let _g = serial();
        let f = fake_herdr(&[(
            "pane list",
            &list_reply(
                r#"{"result":{"panes":[{"cwd":"/a\\b\"c","focused":true,"pane_id":"p1"}]}}"#,
            ),
        )]);
        open(&env_for(&f)).unwrap();
        assert_eq!(opened(&f)[0][8], r#"AGENT_LAUNCH_CWD=/a\b"c"#);
    }

    #[test]
    fn open_fails_on_a_focused_pane_without_a_cwd_or_a_reply_that_is_not_the_list() {
        let _g = serial();
        for (reply, want) in [
            (
                r#"{"result":{"panes":[{"focused":true,"pane_id":"p1"}]}}"#,
                "no cwd",
            ),
            (
                r#"{"result":{"panes":[{"cwd":"","focused":true,"pane_id":"p1"}]}}"#,
                "no cwd",
            ),
            (r#"{"result":{}}"#, "no panes"),
            ("not json", "did not return JSON"),
        ] {
            let f = fake_herdr(&[("pane list", &list_reply(reply))]);
            let err = open(&env_for(&f)).unwrap_err();
            assert!(err.contains(want), "{reply}: {err}");
            assert!(opened(&f).is_empty(), "{reply}");
        }
    }

    #[test]
    fn open_fails_when_pane_list_or_the_popup_cannot_run() {
        let _g = serial();
        let f = fake_herdr(&[("pane list", "exit 1")]);
        assert!(open(&env_for(&f)).unwrap_err().contains("pane list"));
        let f = fake_herdr(&[
            (
                "pane list",
                &list_reply(r#"{"result":{"panes":[{"cwd":"/a","focused":true}]}}"#),
            ),
            ("plugin pane", "echo denied >&2; exit 1"),
        ]);
        let err = open(&env_for(&f)).unwrap_err();
        assert!(
            err.contains("plugin pane") && err.contains("denied"),
            "{err}"
        );
    }

    #[test]
    fn the_popup_without_the_remembered_directory_refuses_and_creates_nothing() {
        let _g = serial();
        let f = fake_herdr(&[]);
        let err = ui::prepare(&env_for(&f)).err().unwrap();
        assert!(err.contains("AGENT_LAUNCH_CWD is not set"), "{err}");
        assert!(f.calls().is_empty());
    }

    #[test]
    fn the_popup_with_an_invalid_config_refuses_naming_the_problem() {
        let _g = serial();
        let dir = testutil::unique_dir("badcfg");
        std::fs::write(dir.join("config.json"), r#"{"agents":[]}"#).unwrap();
        let f = fake_herdr(&[]);
        let mut env = env_for(&f);
        env.cwd = Some("/b".into());
        env.config_path = dir.join("config.json").to_str().unwrap().into();
        let err = ui::prepare(&env).err().unwrap();
        let _ = std::fs::remove_dir_all(dir);
        assert!(err.contains("agents must be a non-empty list"), "{err}");
        assert!(f.calls().is_empty());
    }

    #[test]
    fn the_popup_reads_the_repos_command_and_a_failure_becomes_the_status() {
        let _g = serial();
        let dir = testutil::unique_dir("repos-cfg");
        let write = |body: &str| std::fs::write(dir.join("config.json"), body).unwrap();
        let f = fake_herdr(&[]);
        let mut env = env_for(&f);
        env.cwd = Some("/".into());
        env.config_path = dir.join("config.json").to_str().unwrap().into();

        write(
            r#"{"agents":[{"name":"c","command":"ccxn {repo} {prompt}"}],"repos":"echo o/a; echo o/b"}"#,
        );
        let app = ui::prepare(&env).unwrap();
        assert_eq!(app.status, "");
        assert_eq!(app.repos(), ["o/a", "o/b"]);

        write(r#"{"agents":[{"name":"c","command":"ccxn {repo} {prompt}"}],"repos":"exit 2"}"#);
        let app = ui::prepare(&env).unwrap();
        assert!(
            app.status.contains("repos command failed"),
            "{}",
            app.status
        );
        assert!(app.repos().is_empty());

        // Without a {repo} anywhere the command is not worth running.
        let marker = dir.join("ran");
        write(&format!(
            r#"{{"agents":[{{"name":"c","command":"c {{prompt}}"}}],"repos":"touch {}"}}"#,
            marker.display()
        ));
        let app = ui::prepare(&env).unwrap();
        assert_eq!(app.status, "");
        assert!(!marker.exists(), "the repos command ran");
        let _ = std::fs::remove_dir_all(dir);
    }
}
