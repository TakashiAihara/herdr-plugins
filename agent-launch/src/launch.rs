use crate::render::render;
use crate::Res;
use serde_json::Value;
use std::process::{Command, Stdio};

// Runs herdr with its output captured: the popup's terminal is live while this runs.
pub fn herdr(bin: &str, args: &[&str]) -> Res<String> {
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    let detail = String::from_utf8_lossy(&out.stderr);
    let detail = detail.trim();
    let what = format!(
        "herdr {} failed ({})",
        args[..args.len().min(2)].join(" "),
        out.status
    );
    Err(if detail.is_empty() {
        what
    } else {
        format!("{what}: {detail}")
    })
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

// The repos command's lines are the candidates as they are; only blank lines are dropped.
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
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
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
    let cmd = render(template, prompt, repo);
    if cmd.trim().is_empty() {
        return Err(format!(
            "the command template renders to nothing: {template}"
        ));
    }
    let created = herdr(
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
    )?;
    // Parsed as JSON: a quote inside the label or cwd is escaped, so no key can be misread from them.
    let reply = first_json(&created)
        .ok_or_else(|| format!("herdr workspace create returned no workspace: {created}"))?;
    let ws = str_at(&reply, &["result", "workspace", "workspace_id"])
        .ok_or_else(|| format!("herdr workspace create returned no workspace: {created}"))?;
    // A workspace made for this launch is not left behind when the command cannot be sent.
    let close = |e: String| {
        let _ = herdr(bin, &["workspace", "close", ws]);
        e
    };
    let pane = str_at(&reply, &["result", "root_pane", "pane_id"]).ok_or_else(|| {
        close(format!(
            "herdr workspace create returned no pane: {created}"
        ))
    })?;
    herdr(bin, &["pane", "run", pane, &cmd]).map_err(close)?;
    herdr(bin, &["workspace", "focus", ws])?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::testutil::{fake_herdr, serial, Fake};

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
        assert!(err.contains("pane run"), "{err}");
        assert_eq!(
            heads(&f),
            ["workspace create", "pane run", "workspace close"]
        );
        assert_eq!(f.calls()[2], ["workspace", "close", "w9"]);
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
        assert!(err.contains("workspace focus"), "{err}");
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
