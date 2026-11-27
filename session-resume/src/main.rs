#[cfg(test)]
mod testutil {
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
                "session-resume-{name}-{}-{nanos}-{n}",
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

    // An executable written by a child process, never by this one: a fork in another test thread
    // would inherit this process's write descriptor for a moment, and executing the file while
    // that copy is open fails with "Text file busy".
    pub fn write_exec(path: &std::path::Path, script: &str) {
        use std::io::Write;
        let mut sh = std::process::Command::new("sh")
            .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let wrote = sh.stdin.take().unwrap().write_all(script.as_bytes());
        assert!(
            sh.wait().unwrap().success() && wrote.is_ok(),
            "cannot write {path:?}"
        );
    }

    // The tests that take this lock run one at a time: several of them start processes and
    // assert on elapsed time, and side by side they would add to each other's.
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
        write_exec(&bin, &script);
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

    // (id, last_ms, title, user, assistant, tools): each tier is one record per `\n`.
    pub type Spec<'a> = (&'a str, i64, &'a str, &'a str, &'a str, &'a str);

    // Sessions whose index files are written under a fresh directory, as the searcher reads them.
    // The caller removes `root`.
    pub struct Corpus {
        pub root: std::path::PathBuf,
        pub s: crate::search::Searcher,
    }

    pub fn corpus(name: &str, specs: &[Spec]) -> Corpus {
        let root = unique_dir(name);
        let index = crate::index::Index::open(&root).unwrap();
        let mut entries = vec![];
        for (id, ms, title, user, assistant, tools) in specs {
            let mut meta = crate::extract::Meta {
                custom_title: title.to_string(),
                cwd: format!("/w/{id}"),
                last_ms: Some(*ms),
                ..crate::extract::Meta::default()
            };
            for (k, text) in [user, assistant, tools].into_iter().enumerate() {
                let ext = ["user", "assistant", "tools"][k];
                std::fs::write(root.join("index").join(format!("{id}.{ext}")), text).unwrap();
                meta.lens[k] = text.len() as u64;
            }
            entries.push(crate::search::Entry::new(id.to_string(), meta, false));
        }
        Corpus {
            root,
            s: crate::search::Searcher::new(index, entries),
        }
    }
}

mod config;
mod extract;
mod index;
mod input;
mod launch;
mod render;
mod search;
mod ui;

type Res<T> = Result<T, String>;

pub struct Env {
    herdr: String,
    config_path: String,
    plugin_id: String,
    state_dir: String,
    home: String,
    claude_config_dir: Option<String>,
}

// Like bash `${VAR:-}`: an empty variable counts as unset.
fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

// The directory Herdr names, else `<xdg base or $HOME/<home_rel>>/herdr-session-resume`.
fn resolve_dir(
    explicit: Option<String>,
    xdg: Option<String>,
    home: &str,
    home_rel: &str,
) -> String {
    explicit.unwrap_or_else(|| {
        let base = xdg.unwrap_or_else(|| format!("{home}/{home_rel}"));
        format!("{base}/herdr-session-resume")
    })
}

impl Env {
    fn new() -> Env {
        let home = std::env::var("HOME").unwrap_or_default();
        let config_dir = resolve_dir(
            var("HERDR_PLUGIN_CONFIG_DIR"),
            var("XDG_CONFIG_HOME"),
            &home,
            ".config",
        );
        let state_dir = resolve_dir(
            var("HERDR_PLUGIN_STATE_DIR"),
            var("XDG_STATE_HOME"),
            &home,
            ".local/state",
        );
        Env {
            herdr: var("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
            config_path: format!("{config_dir}/config.json"),
            plugin_id: var("HERDR_PLUGIN_ID")
                .unwrap_or_else(|| "takashiaihara.session-resume".into()),
            state_dir,
            home,
            claude_config_dir: var("CLAUDE_CONFIG_DIR"),
        }
    }
}

fn open(env: &Env) -> Res<()> {
    launch::herdr(
        &env.herdr,
        &[
            "plugin",
            "pane",
            "open",
            "--plugin",
            &env.plugin_id,
            "--entrypoint",
            "picker",
        ],
    )
    .map(drop)
}

// Brings the whole index up to date without a UI and says how much work that was.
fn index_all(env: &Env) -> Res<String> {
    let start = std::time::Instant::now();
    let cfg = ui::load_config(env)?;
    let (all, mut problems) = index::transcripts(std::path::Path::new(&cfg.projects))?;
    let idx = index::Index::open(std::path::Path::new(&env.state_dir))?;
    idx.prune(&all, &mut problems);
    let (mut listed, mut refreshed) = (0, 0);
    let mut failures = vec![];
    for (id, path) in &all {
        match idx.refresh(id, path) {
            Ok((meta, did)) => {
                listed += usize::from(meta.listed());
                refreshed += usize::from(did);
            }
            Err(e) => failures.push(e),
        }
    }
    if let Some(first) = failures.first() {
        problems.insert(
            0,
            format!(
                "{} of {} sessions could not be indexed: {first}",
                failures.len(),
                all.len()
            ),
        );
    }
    // The sessions that could be read are indexed all the same; the command still fails.
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(format!(
        "sessions {listed}, refreshed {refreshed}, {} ms",
        start.elapsed().as_millis()
    ))
}

fn main() {
    let result = match std::env::args().nth(1).as_deref().unwrap_or("") {
        "open" => open(&Env::new()),
        "ui" => ui::run(&Env::new()),
        "index" => index_all(&Env::new()).map(|line| println!("{line}")),
        // The prebuilt binary's install step runs this to see that it starts on this machine.
        "version" => {
            println!("session-resume {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err("usage: session-resume open|ui|index|version".to_string()),
    };
    if let Err(msg) = result {
        eprintln!("session-resume: {msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fake_herdr, serial, unique_dir};

    #[test]
    fn a_fake_herdr_runs_while_other_threads_start_processes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // Set when dropped, so a panic below still ends the threads the scope waits for.
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _g = serial();
        let stop = AtomicBool::new(false);
        let failed = std::thread::scope(|s| {
            let _stop = Stop(&stop);
            for _ in 0..4 {
                s.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        std::process::Command::new("true").status().unwrap();
                    }
                });
            }
            (0..200).find_map(|_| {
                let f = fake_herdr(&[]);
                match std::process::Command::new(&f.bin).status() {
                    Ok(status) if status.success() => None,
                    other => Some(format!("{other:?}")),
                }
            })
        });
        assert!(failed.is_none(), "{failed:?}");
    }

    fn env_in(dir: &std::path::Path, herdr: &str) -> Env {
        Env {
            herdr: herdr.into(),
            config_path: dir.join("cfg/config.json").to_str().unwrap().into(),
            plugin_id: "takashiaihara.session-resume".into(),
            state_dir: dir.join("state").to_str().unwrap().into(),
            home: dir.join("home").to_str().unwrap().into(),
            claude_config_dir: Some(dir.join("claude").to_str().unwrap().into()),
        }
    }

    #[test]
    fn the_directory_comes_from_herdr_then_xdg_then_home() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            resolve_dir(some("/h/cfg"), some("/x"), "/home/u", ".config"),
            "/h/cfg"
        );
        assert_eq!(
            resolve_dir(None, some("/x"), "/home/u", ".config"),
            "/x/herdr-session-resume"
        );
        assert_eq!(
            resolve_dir(None, None, "/home/u", ".local/state"),
            "/home/u/.local/state/herdr-session-resume"
        );
    }

    #[test]
    fn open_runs_plugin_pane_open_for_the_picker_entrypoint() {
        let _g = serial();
        let f = fake_herdr(&[]);
        open(&env_in(std::path::Path::new("/nonexistent"), &f.bin)).unwrap();
        assert_eq!(
            f.calls(),
            [[
                "plugin",
                "pane",
                "open",
                "--plugin",
                "takashiaihara.session-resume",
                "--entrypoint",
                "picker"
            ]]
        );
    }

    #[test]
    fn open_fails_with_the_herdr_error_when_the_popup_cannot_open() {
        let _g = serial();
        let f = fake_herdr(&[("plugin pane", "echo denied >&2; exit 1")]);
        let err = open(&env_in(std::path::Path::new("/nonexistent"), &f.bin)).unwrap_err();
        assert!(
            err.contains("plugin pane") && err.contains("denied"),
            "{err}"
        );
    }

    fn user_line(text: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-11-04T10:00:00Z\",\"cwd\":\"/w/x\",\"message\":{{\"content\":\"{text}\"}}}}\n"
        )
    }

    #[test]
    fn index_all_counts_sessions_and_only_the_changed_ones_are_refreshed_the_second_time() {
        let _g = serial();
        let dir = unique_dir("index-cmd");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("s1.jsonl"), user_line("one")).unwrap();
        std::fs::write(proj.join("s2.jsonl"), user_line("two")).unwrap();
        std::fs::write(proj.join("s3.jsonl"), "{\"type\":\"attachment\"}\n").unwrap();
        let env = env_in(&dir, "/nonexistent/herdr");
        let first = index_all(&env).unwrap();
        assert!(first.starts_with("sessions 2, refreshed 3, "), "{first}");
        assert!(first.ends_with(" ms"), "{first}");
        let again = index_all(&env).unwrap();
        assert!(again.starts_with("sessions 2, refreshed 0, "), "{again}");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(proj.join("s2.jsonl"))
            .unwrap();
        std::io::Write::write_all(&mut f, user_line("more").as_bytes()).unwrap();
        let third = index_all(&env).unwrap();
        assert!(third.starts_with("sessions 2, refreshed 1, "), "{third}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dangling_symlink_is_not_a_session_and_the_others_are_still_indexed() {
        let _g = serial();
        let dir = unique_dir("index-cmd-link");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("s1.jsonl"), user_line("one")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", proj.join("s2.jsonl")).unwrap();
        let r = index_all(&env_in(&dir, "/nonexistent/herdr")).unwrap();
        assert!(r.starts_with("sessions 1, refreshed 1, "), "{r}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_that_cannot_be_indexed_fails_the_command_naming_it_and_the_count() {
        let _g = serial();
        let dir = unique_dir("index-cmd-bad");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("s1.jsonl"), user_line("one")).unwrap();
        std::fs::write(proj.join("s2.jsonl"), user_line("two")).unwrap();
        // A directory where s2's body file belongs makes writing that session fail.
        std::fs::create_dir_all(dir.join("state/index/s2.user")).unwrap();
        let err = index_all(&env_in(&dir, "/nonexistent/herdr")).unwrap_err();
        assert!(
            err.starts_with("1 of 2 sessions could not be indexed: "),
            "{err}"
        );
        assert!(err.contains("s2.user"), "{err}");
        // With a second problem both are named, the sessions first.
        let knot = dir.join("claude/projects/-w-knot");
        std::os::unix::fs::symlink("-w-knot", &knot).unwrap();
        let err = index_all(&env_in(&dir, "/nonexistent/herdr")).unwrap_err();
        assert!(
            err.starts_with("1 of 2 sessions could not be indexed: ")
                && err.contains(&format!("; cannot read {}: ", knot.display())),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreadable_project_directory_fails_the_command_after_the_rest_is_indexed_and_kept() {
        let _g = serial();
        let dir = unique_dir("index-cmd-knot");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("s1.jsonl"), user_line("one")).unwrap();
        std::fs::write(proj.join("s2.jsonl"), user_line("two")).unwrap();
        let env = env_in(&dir, "/nonexistent/herdr");
        index_all(&env).unwrap();
        // s2 disappears together with a directory that stops being readable: its cache stays.
        std::fs::remove_file(proj.join("s2.jsonl")).unwrap();
        std::fs::write(proj.join("s3.jsonl"), user_line("three")).unwrap();
        let knot = dir.join("claude/projects/-w-knot");
        std::os::unix::fs::symlink("-w-knot", &knot).unwrap();
        let err = index_all(&env).unwrap_err();
        assert!(
            err.starts_with(&format!("cannot read {}: ", knot.display())),
            "{err}"
        );
        let cached = |id: &str| dir.join(format!("state/index/{id}.meta.json")).exists();
        assert!(cached("s2") && cached("s3"));
        std::fs::remove_file(&knot).unwrap();
        index_all(&env).unwrap();
        assert!(!cached("s2"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn index_all_with_an_invalid_config_names_the_problem() {
        let _g = serial();
        let dir = unique_dir("index-cmd-cfg");
        std::fs::create_dir_all(dir.join("cfg")).unwrap();
        std::fs::write(dir.join("cfg/config.json"), r#"{"scope":"nope"}"#).unwrap();
        let err = index_all(&env_in(&dir, "/nonexistent/herdr")).unwrap_err();
        assert!(err.contains("scope must be one of"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
