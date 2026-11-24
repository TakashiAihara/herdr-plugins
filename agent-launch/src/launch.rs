use crate::render::render;
use crate::Res;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HERDR_TIMEOUT: Duration = Duration::from_secs(10);

// Runs herdr with its output captured: the popup's terminal is live while this runs.
pub fn herdr(bin: &str, args: &[&str]) -> Res<String> {
    herdr_within(bin, args, HERDR_TIMEOUT)
}

// `herdr` bounded by `limit`: past it the process is killed and an error names the command.
pub fn herdr_within(bin: &str, args: &[&str], limit: Duration) -> Res<String> {
    call(bin, args, limit).map_err(HerdrError::into_msg)
}

// Why a bounded herdr call failed; both variants carry the error text. `TimedOut` means the
// process was killed for not answering in time and may still have taken effect.
enum HerdrError {
    TimedOut(String),
    Failed(String),
}

impl HerdrError {
    fn into_msg(self) -> String {
        match self {
            HerdrError::TimedOut(msg) | HerdrError::Failed(msg) => msg,
        }
    }
}

impl From<String> for HerdrError {
    fn from(msg: String) -> HerdrError {
        HerdrError::Failed(msg)
    }
}

// One herdr call bounded by `limit`, with a timeout kept apart from other failures: a `pane
// run` that times out may still have reached the pane. stdout and stderr drain on their own
// threads while waiting — `herdr pane list` writes more than a pipe buffer, and a child
// blocked on a full pipe would look like one that never answers.
fn call(bin: &str, args: &[&str], limit: Duration) -> Result<String, HerdrError> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let mut out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        out_pipe.read_to_end(&mut buf).map(|_| buf)
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        err_pipe.read_to_end(&mut buf).map(|_| buf)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < limit => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                // A child of the killed process can hold the pipes open, so they are not
                // waited for here; the reader threads end when the writers do.
                let _ = child.kill();
                let _ = child.wait();
                return Err(HerdrError::TimedOut(format!(
                    "herdr {} did not answer within {} s and was stopped; it may still have taken effect",
                    args[..args.len().min(2)].join(" "),
                    limit.as_secs()
                )));
            }
            Err(e) => return Err(HerdrError::Failed(format!("cannot run {bin}: {e}"))),
        }
    };
    let stdout = out
        .join()
        .map_err(|_| "reading herdr's output failed".to_string())?
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let stderr = err
        .join()
        .map_err(|_| "reading herdr's output failed".to_string())?
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    if status.success() {
        return Ok(stdout);
    }
    let detail = String::from_utf8_lossy(&stderr);
    let detail = detail.trim();
    let what = format!(
        "herdr {} failed ({})",
        args[..args.len().min(2)].join(" "),
        status
    );
    Err(HerdrError::Failed(if detail.is_empty() {
        what
    } else {
        format!("{what}: {detail}")
    }))
}

// The first JSON value in Herdr's stdout; a line Herdr may print after it does not break parsing.
pub fn first_json(s: &str) -> Option<Value> {
    serde_json::Deserializer::from_str(s)
        .into_iter::<Value>()
        .next()?
        .ok()
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(v, |v, k| v.get(k))?
        .as_str()
        .filter(|s| !s.is_empty())
}

// The repos command's lines are the candidates as they are; blank lines and lines holding a
// control character are dropped.
pub fn list_repos(cmd: &str, cwd: &str) -> Res<Vec<String>> {
    let out = Command::new("sh")
        .args(["-c", cmd])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run the repos command: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!(
            "repos command failed ({}){}{first}",
            out.status,
            if first.is_empty() { "" } else { ": " }
        ));
    }
    // The chosen line is typed into the new pane, where a control byte (Ctrl-C, ESC) would act on
    // the shell before quoting applies, so such a line is not offered at all.
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.chars().any(char::is_control))
        .map(String::from)
        .collect())
}

// The label is 24 characters, not bytes: a cut inside a Japanese character would be invalid text.
pub fn label(prompt: &str) -> String {
    prompt.chars().take(24).collect()
}

// Creates a workspace in `cwd`, types the rendered command into its pane, then focuses it. Focus
// moves last: the popup may close once it loses focus, and the command must be sent by then.
pub fn launch(bin: &str, cwd: &str, template: &str, repo: &str, prompt: &str) -> Res<()> {
    launch_within(bin, cwd, template, repo, prompt, HERDR_TIMEOUT)
}

// `launch` with `limit` on every herdr call. A `pane run` that times out may still have
// reached the pane, so the workspace is left open and the error says so instead of closing it.
pub fn launch_within(
    bin: &str,
    cwd: &str,
    template: &str,
    repo: &str,
    prompt: &str,
    limit: Duration,
) -> Res<()> {
    let cmd = render(template, prompt, repo);
    if cmd.trim().is_empty() {
        return Err(format!(
            "the command template renders to nothing: {template}"
        ));
    }
    let created = herdr_within(
        bin,
        &[
            "workspace",
            "create",
            "--cwd",
            cwd,
            "--label",
            &label(prompt),
            "--no-focus",
        ],
        limit,
    )?;
    // Parsed as JSON: a quote inside the label or cwd is escaped, so no key can be misread from them.
    let reply = first_json(&created)
        .ok_or_else(|| format!("herdr workspace create returned no workspace: {created}"))?;
    let ws = str_at(&reply, &["result", "workspace", "workspace_id"])
        .ok_or_else(|| format!("herdr workspace create returned no workspace: {created}"))?;
    // A workspace made for this launch is not left behind when the command cannot be sent; a
    // failed close is reported, or the workspace would stay open without anyone knowing.
    let close = |e: String| match call(bin, &["workspace", "close", ws], limit) {
        Ok(_) => e,
        Err(HerdrError::TimedOut(c)) => format!("{e}; workspace {ws} may be left open: {c}"),
        Err(HerdrError::Failed(c)) => format!("{e}; workspace {ws} is left open: {c}"),
    };
    let pane = str_at(&reply, &["result", "root_pane", "pane_id"]).ok_or_else(|| {
        close(format!(
            "herdr workspace create returned no pane: {created}"
        ))
    })?;
    match call(bin, &["pane", "run", pane, &cmd], limit) {
        Ok(_) => {}
        Err(HerdrError::TimedOut(e)) => {
            return Err(format!("{e}; workspace {ws} is left open"));
        }
        Err(HerdrError::Failed(e)) => return Err(close(e)),
    }
    // The command already runs in the pane; a failed focus must not read as if nothing was sent.
    herdr_within(bin, &["workspace", "focus", ws], limit)
        .map_err(|e| format!("the command was sent to workspace {ws}, but {e}"))?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::testutil::{fake_herdr, serial, Fake};

    #[test]
    fn repo_lines_with_control_characters_are_not_offered() {
        let got = list_repos(r"printf 'o/a\n\033[31mo/b\no/c\003\no/d\n'", "/").unwrap();
        assert_eq!(got, ["o/a", "o/d"]);
    }

    #[test]
    fn a_line_after_herdrs_json_does_not_break_parsing() {
        let v = first_json("{\"a\":1}\nwarning: something\n").unwrap();
        assert_eq!(v["a"], 1);
        assert!(first_json("warning first\n{\"a\":1}").is_none());
    }

    const CREATED: &str = r#"{"result":{"root_pane":{"cwd":"/x}y","focused":false,"pane_id":"w9:p1","revision":0,"scroll":{"viewport_rows":74},"tab_id":"w9:t1","workspace_id":"wX"},"tab":{"tab_id":"w9:t1"},"workspace":{"label":"a}b","workspace_id":"w9"}}}"#;

    fn heads(f: &Fake) -> Vec<String> {
        f.calls().iter().map(|c| c[..2].join(" ")).collect()
    }

    #[test]
    fn creates_unfocused_then_runs_then_focuses_in_that_order() {
        let _g = serial();
        let f = fake_herdr(&[("workspace create", &format!("echo '{CREATED}'"))]);
        launch(&f.bin, "/b", "claude -- {prompt}", "", "fix it's $HOME `x`").unwrap();
        assert_eq!(
            heads(&f),
            ["workspace create", "pane run", "workspace focus"]
        );
        let calls = f.calls();
        assert_eq!(
            calls[0],
            [
                "workspace",
                "create",
                "--cwd",
                "/b",
                "--label",
                "fix it's $HOME `x`",
                "--no-focus"
            ]
        );
        assert_eq!(
            calls[1],
            ["pane", "run", "w9:p1", r"claude -- 'fix it'\''s $HOME `x`'"]
        );
        assert_eq!(calls[2], ["workspace", "focus", "w9"]);
    }

    #[test]
    fn cwd_and_label_with_spaces_are_one_argument_each() {
        let _g = serial();
        let f = fake_herdr(&[("workspace create", &format!("echo '{CREATED}'"))]);
        launch(&f.bin, "/b c", "x {prompt}", "", "fix the build").unwrap();
        assert_eq!(
            f.calls()[0],
            [
                "workspace",
                "create",
                "--cwd",
                "/b c",
                "--label",
                "fix the build",
                "--no-focus"
            ]
        );
    }

    #[test]
    fn the_label_is_24_characters_of_a_japanese_prompt() {
        let prompt = &format!("{}{}{}", "あ".repeat(10), "い".repeat(10), "う".repeat(8));
        assert_eq!(prompt.chars().count(), 28);
        assert_eq!(
            label(prompt),
            format!("{}{}{}", "あ".repeat(10), "い".repeat(10), "う".repeat(4))
        );
        assert_eq!(label(prompt).chars().count(), 24);
        assert_eq!(
            label("abcdefghijklmnopqrstuvwxyz0123"),
            "abcdefghijklmnopqrstuvwx"
        );
        assert_eq!(label("short"), "short");
        let _g = serial();
        let f = fake_herdr(&[("workspace create", &format!("echo '{CREATED}'"))]);
        launch(&f.bin, "/b", "x {prompt}", "", prompt).unwrap();
        assert_eq!(f.calls()[0][5], label(prompt));
    }

    #[test]
    fn a_failed_pane_run_closes_the_new_workspace_and_fails() {
        let _g = serial();
        let f = fake_herdr(&[
            ("workspace create", &format!("echo '{CREATED}'")),
            ("pane run", "exit 1"),
        ]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(
            err.contains("pane run") && !err.contains("workspace close"),
            "{err}"
        );
        assert_eq!(
            heads(&f),
            ["workspace create", "pane run", "workspace close"]
        );
        assert_eq!(f.calls()[2], ["workspace", "close", "w9"]);
    }

    #[test]
    fn a_failed_close_after_a_failed_pane_run_is_reported_with_the_workspace_left_open() {
        let _g = serial();
        let f = fake_herdr(&[
            ("workspace create", &format!("echo '{CREATED}'")),
            ("pane run", "echo norun >&2; exit 1"),
            ("workspace close", "echo noclose >&2; exit 1"),
        ]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(err.contains("pane run") && err.contains("norun"), "{err}");
        assert!(
            err.contains("workspace w9 is left open")
                && err.contains("workspace close")
                && err.contains("noclose"),
            "{err}"
        );
    }

    #[test]
    fn a_herdr_that_does_not_answer_is_stopped_and_named() {
        let _g = serial();
        let dir = crate::testutil::unique_dir("hang");
        let marker = dir.join("answered");
        // Left alone, the fake answers after 3 s; stopped, it never writes the marker.
        let f = fake_herdr(&[(
            "pane list",
            &format!(
                "sleep 3; touch {}",
                crate::render::quote(marker.to_str().unwrap())
            ),
        )]);
        let start = std::time::Instant::now();
        let limit = std::time::Duration::from_secs(1);
        let err = herdr_within(&f.bin, &["pane", "list"], limit).unwrap_err();
        let took = start.elapsed();
        assert!(
            err.contains(
                "herdr pane list did not answer within 1 s and was stopped; it may still have taken effect"
            ),
            "{err}"
        );
        assert!(took < std::time::Duration::from_millis(2500), "{took:?}");
        std::thread::sleep(std::time::Duration::from_millis(3500).saturating_sub(took));
        let answered = marker.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!answered, "the fake herdr was left running");
    }

    #[test]
    fn a_pane_run_that_does_not_answer_leaves_the_workspace_open() {
        let _g = serial();
        // Left alone the fake answers after 3 s: the command may still reach the pane.
        let f = fake_herdr(&[
            ("workspace create", &format!("echo '{CREATED}'")),
            ("pane run", "sleep 3"),
        ]);
        let start = std::time::Instant::now();
        let limit = std::time::Duration::from_secs(1);
        let err = launch_within(&f.bin, "/b", "x {prompt}", "", "do x", limit).unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_millis(2500));
        assert!(
            err.contains("herdr pane run did not answer within 1 s")
                && err.contains("workspace w9 is left open"),
            "{err}"
        );
        assert_eq!(heads(&f), ["workspace create", "pane run"]);
    }

    #[test]
    fn the_limit_given_to_a_launch_bounds_its_first_call_too() {
        let _g = serial();
        let f = fake_herdr(&[("workspace create", "sleep 3")]);
        let limit = std::time::Duration::from_secs(1);
        let err = launch_within(&f.bin, "/b", "x {prompt}", "", "do x", limit).unwrap_err();
        assert!(
            err.contains("herdr workspace create did not answer within 1 s"),
            "{err}"
        );
        assert_eq!(heads(&f), ["workspace create"]);
    }

    #[test]
    fn the_limit_given_to_a_launch_bounds_the_close_and_the_focus_too() {
        let _g = serial();
        let limit = std::time::Duration::from_secs(1);
        let f = fake_herdr(&[
            ("workspace create", &format!("echo '{CREATED}'")),
            ("pane run", "exit 1"),
            ("workspace close", "sleep 3"),
        ]);
        let err = launch_within(&f.bin, "/b", "x {prompt}", "", "do x", limit).unwrap_err();
        assert!(
            err.contains("workspace w9 may be left open: herdr workspace close did not answer"),
            "{err}"
        );
        let f = fake_herdr(&[
            ("workspace create", &format!("echo '{CREATED}'")),
            ("workspace focus", "sleep 3"),
        ]);
        let err = launch_within(&f.bin, "/b", "x {prompt}", "", "do x", limit).unwrap_err();
        assert!(
            err.contains("but herdr workspace focus did not answer within 1 s"),
            "{err}"
        );
    }

    #[test]
    fn the_limit_on_a_herdr_call_is_ten_seconds() {
        assert_eq!(HERDR_TIMEOUT, std::time::Duration::from_secs(10));
    }

    #[test]
    fn a_reply_larger_than_a_pipe_buffer_is_read_whole_on_both_streams() {
        let _g = serial();
        // Each stream alone overflows a pipe: reading one to its end before the other blocks the fake.
        let f = fake_herdr(&[(
            "pane list",
            "head -c 200000 /dev/zero | tr '\\0' y >&2; head -c 200000 /dev/zero | tr '\\0' x",
        )]);
        let limit = std::time::Duration::from_secs(10);
        let out = herdr_within(&f.bin, &["pane", "list"], limit).unwrap();
        assert_eq!(out.len(), 200_000);
        assert!(out.bytes().all(|b| b == b'x'));
    }

    #[test]
    fn a_failed_workspace_create_runs_nothing() {
        let _g = serial();
        let f = fake_herdr(&[("workspace create", "echo boom >&2; exit 1")]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(
            err.contains("workspace create") && err.contains("boom"),
            "{err}"
        );
        assert_eq!(heads(&f), ["workspace create"]);
    }

    #[test]
    fn a_reply_without_a_workspace_or_pane_fails_and_closes_what_was_made() {
        let _g = serial();
        let f = fake_herdr(&[("workspace create", "echo '{\"result\":{}}'")]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(err.contains("returned no workspace"), "{err}");
        assert_eq!(heads(&f), ["workspace create"]);

        let f = fake_herdr(&[("workspace create", "echo 'not json'")]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(err.contains("returned no workspace"), "{err}");

        let f = fake_herdr(&[(
            "workspace create",
            r#"echo '{"result":{"workspace":{"workspace_id":"w9"}}}'"#,
        )]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(err.contains("returned no pane"), "{err}");
        assert_eq!(heads(&f), ["workspace create", "workspace close"]);
    }

    #[test]
    fn a_template_that_renders_to_nothing_creates_nothing() {
        let _g = serial();
        let f = fake_herdr(&[]);
        let err = launch(&f.bin, "/b", "{repo}", "", "do x").unwrap_err();
        assert!(err.contains("renders to nothing"), "{err}");
        assert!(f.calls().is_empty());
    }

    #[test]
    fn the_repos_command_runs_in_the_cwd_and_its_lines_are_the_candidates() {
        let _g = serial();
        let dir = crate::testutil::unique_dir("repos");
        let cwd = dir.join("sub dir");
        std::fs::create_dir(&cwd).unwrap();
        let got = list_repos(r"pwd; printf 'o/a\n\n  \n o/b \n'", cwd.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        // The shell may print the cwd through a symlink; what matters is the lines after it.
        let got = got.unwrap();
        assert_eq!(got[1..], ["o/a", " o/b "]);
        assert!(got[0].ends_with("sub dir"), "{got:?}");
    }

    #[test]
    fn a_failing_repos_command_is_an_error_with_its_first_stderr_line() {
        let _g = serial();
        let err = list_repos("echo partial; echo nope >&2; exit 3", "/").unwrap_err();
        assert!(
            err.contains("exit status: 3") && err.contains(": nope"),
            "{err}"
        );
        let err = list_repos("exit 1", "/").unwrap_err();
        assert!(
            err.starts_with("repos command failed") && !err.ends_with(": "),
            "{err}"
        );
    }

    #[test]
    fn a_repos_command_in_a_missing_directory_is_an_error() {
        let _g = serial();
        let err = list_repos("true", "/nonexistent/dir/x").unwrap_err();
        assert!(err.contains("cannot run the repos command"), "{err}");
    }

    #[test]
    fn a_failed_focus_fails_but_keeps_the_workspace() {
        let _g = serial();
        let f = fake_herdr(&[
            ("workspace create", &format!("echo '{CREATED}'")),
            ("workspace focus", "exit 1"),
        ]);
        let err = launch(&f.bin, "/b", "x {prompt}", "", "do x").unwrap_err();
        assert!(
            err.contains("the command was sent to workspace w9") && err.contains("workspace focus"),
            "{err}"
        );
        assert_eq!(
            heads(&f),
            ["workspace create", "pane run", "workspace focus"]
        );
    }

    #[test]
    fn answers_are_not_substituted_again_on_their_way_to_the_pane() {
        let _g = serial();
        let f = fake_herdr(&[("workspace create", &format!("echo '{CREATED}'"))]);
        launch(
            &f.bin,
            "/b",
            "ccxn {repo} {prompt}",
            "o/{prompt}",
            "fix A&B in {repo}",
        )
        .unwrap();
        assert_eq!(f.calls()[1][3], "ccxn 'o/{prompt}' 'fix A&B in {repo}'");
    }
}
