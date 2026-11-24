use crate::render::render;
use crate::Res;
use serde_json::Value;
use std::collections::HashMap;
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

// A control byte in text from a transcript would act on the terminal or on the shell that
// receives a label, so each becomes a space.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

// The label is 24 characters, not bytes: a cut inside a Japanese character would be invalid text.
pub fn label(title: &str, id: &str) -> String {
    let l: String = clean(title).trim().chars().take(24).collect();
    let l = l.trim_end();
    if l.is_empty() {
        clean(id).chars().take(8).collect()
    } else {
        l.to_string()
    }
}

// The pane each running agent session sits in, by the session id Herdr detected.
pub fn running(bin: &str) -> Res<HashMap<String, String>> {
    let list = herdr(bin, &["pane", "list"])?;
    let list = first_json(&list).ok_or("herdr pane list did not return JSON")?;
    let panes = list["result"]["panes"]
        .as_array()
        .ok_or("herdr pane list has no panes")?;
    Ok(panes
        .iter()
        .filter_map(|p| {
            let id = str_at(p, &["agent_session", "value"])?;
            Some((id.to_string(), str_at(p, &["pane_id"])?.to_string()))
        })
        .collect())
}

// Creates a workspace in `cwd`, types the rendered command into its pane, then focuses it. Focus
// moves last: the popup may close once it loses focus, and the command must be sent by then.
pub fn launch(bin: &str, cwd: &str, template: &str, id: &str, label: &str) -> Res<()> {
    let cmd = render(template, id, cwd);
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
            label,
            "--no-focus",
        ],
    )?;
    // Parsed as JSON: a quote inside the label or cwd is escaped, so no key can be misread from them.
    let reply = first_json(&created)
        .ok_or_else(|| format!("herdr workspace create returned no workspace: {created}"))?;
    let ws = str_at(&reply, &["result", "workspace", "workspace_id"])
        .ok_or_else(|| format!("herdr workspace create returned no workspace: {created}"))?;
    // A workspace made for this resume is not left behind when the command cannot be sent.
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

// Focuses the pane the session already runs in; otherwise starts it where it began. The pane
// list is read again here, not taken from when the popup opened: the session may have started
// or stopped since.
pub fn resume(bin: &str, template: &str, id: &str, cwd: &str, title: &str) -> Res<()> {
    if let Some(pane) = running(bin)?.get(id) {
        herdr(bin, &["agent", "focus", pane])?;
        return Ok(());
    }
    if cwd.is_empty() {
        return Err(format!(
            "the transcript of session {id} records no directory"
        ));
    }
    if !std::path::Path::new(cwd).is_dir() {
        return Err(format!(
            "the directory of session {id} no longer exists: {cwd}"
        ));
    }
    launch(bin, cwd, template, id, &label(title, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fake_herdr, serial, unique_dir, Fake};

    const CREATED_SH: &str = r#"echo '{"result":{"root_pane":{"cwd":"/x}y","focused":false,"pane_id":"w9:p1","tab_id":"w9:t1","workspace_id":"wX"},"workspace":{"label":"a}b","workspace_id":"w9"}}}'"#;

    // printf, not echo: sh's echo turns a JSON `\b` into a backspace.
    fn reply(json: &str) -> String {
        format!("printf '%s' '{json}'")
    }

    fn heads(f: &Fake) -> Vec<String> {
        f.calls().iter().map(|c| c[..2].join(" ")).collect()
    }

    fn created() -> (&'static str, &'static str) {
        ("workspace create", CREATED_SH)
    }

    #[test]
    fn a_line_after_herdrs_json_does_not_break_parsing() {
        let v = first_json("{\"a\":1}\nwarning: something\n").unwrap();
        assert_eq!(v["a"], 1);
        assert!(first_json("warning first\n{\"a\":1}").is_none());
    }

    #[test]
    fn running_maps_each_detected_session_id_to_its_pane() {
        let _g = serial();
        let f = fake_herdr(&[(
            "pane list",
            &reply(
                r#"{"result":{"panes":[{"pane_id":"p1","agent_session":{"kind":"id","value":"sess-a"}},{"pane_id":"p2"},{"pane_id":"p3","agent_session":null},{"pane_id":"p4","agent_session":{"value":""}},{"pane_id":"p5","agent_session":{"value":"sess-b"}}]}}"#,
            ),
        )]);
        let got = running(&f.bin).unwrap();
        let mut got: Vec<_> = got.into_iter().collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("sess-a".to_string(), "p1".to_string()),
                ("sess-b".to_string(), "p5".to_string())
            ]
        );
    }

    #[test]
    fn running_fails_on_a_failing_herdr_or_a_reply_that_is_not_the_list() {
        let _g = serial();
        let f = fake_herdr(&[("pane list", "echo nope >&2; exit 1")]);
        let err = running(&f.bin).unwrap_err();
        assert!(err.contains("pane list") && err.contains("nope"), "{err}");
        for (r, want) in [
            ("not json", "did not return JSON"),
            (r#"{"result":{}}"#, "no panes"),
        ] {
            let f = fake_herdr(&[("pane list", &reply(r))]);
            let err = running(&f.bin).unwrap_err();
            assert!(err.contains(want), "{r}: {err}");
        }
    }

    #[test]
    fn creates_unfocused_then_runs_then_focuses_in_that_order() {
        let _g = serial();
        let f = fake_herdr(&[created()]);
        launch(&f.bin, "/b c", "claude --resume {id}", "9c1f", "my title").unwrap();
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
                "/b c",
                "--label",
                "my title",
                "--no-focus"
            ]
        );
        assert_eq!(calls[1], ["pane", "run", "w9:p1", "claude --resume '9c1f'"]);
        assert_eq!(calls[2], ["workspace", "focus", "w9"]);
    }

    #[test]
    fn id_and_cwd_reach_the_pane_as_single_quoted_words() {
        let _g = serial();
        let f = fake_herdr(&[created()]);
        launch(&f.bin, "/it's", "r {cwd} {id}", "i d", "t").unwrap();
        assert_eq!(f.calls()[1][3], r"r '/it'\''s' 'i d'");
    }

    #[test]
    fn the_label_is_24_characters_of_a_japanese_title_without_control_characters() {
        let title = format!("{}{}{}", "あ".repeat(10), "い".repeat(10), "う".repeat(8));
        assert_eq!(
            label(&title, "id"),
            format!("{}{}{}", "あ".repeat(10), "い".repeat(10), "う".repeat(4))
        );
        assert_eq!(
            label("abcdefghijklmnopqrstuvwxyz0123", "id"),
            "abcdefghijklmnopqrstuvwx"
        );
        assert_eq!(label("short", "id"), "short");
        assert_eq!(label("a\u{1b}[31mb\nc", "id"), "a [31mb c");
        assert_eq!(label("\u{1b}\u{7}", "0123456789abcdef"), "01234567");
        assert_eq!(label("  ", "abc"), "abc");
        assert_eq!(label("", "a\tb\u{1b}c"), "a b c");
        assert_eq!(
            label(&format!("{} z", "x".repeat(23)), "id"),
            "x".repeat(23)
        );
    }

    #[test]
    fn a_label_with_control_characters_reaches_workspace_create_without_them() {
        let _g = serial();
        let dir = unique_dir("ctl");
        let f = fake_herdr(&[
            ("pane list", &reply(r#"{"result":{"panes":[]}}"#)),
            created(),
        ]);
        resume(
            &f.bin,
            "x {id}",
            "s1",
            dir.to_str().unwrap(),
            "evil\u{1b}]0;pwn\u{7}title",
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let label = &f.calls()[1][5];
        assert_eq!(label, "evil ]0;pwn title");
        assert!(!label.chars().any(char::is_control));
    }

    #[test]
    fn a_failed_pane_run_closes_the_new_workspace_and_fails() {
        let _g = serial();
        let f = fake_herdr(&[created(), ("pane run", "exit 1")]);
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
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
            created(),
            ("pane run", "echo norun >&2; exit 1"),
            ("workspace close", "echo noclose >&2; exit 1"),
        ]);
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
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
        let dir = unique_dir("hang");
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
        let limit = std::time::Duration::from_millis(300);
        let err = herdr_within(&f.bin, &["pane", "list"], limit).unwrap_err();
        let took = start.elapsed();
        assert!(
            err.contains("herdr pane list") && err.contains("did not answer"),
            "{err}"
        );
        assert!(took < std::time::Duration::from_millis(2500), "{took:?}");
        std::thread::sleep(std::time::Duration::from_millis(3500).saturating_sub(took));
        let answered = marker.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!answered, "the fake herdr was left running");
    }

    #[test]
    fn a_reply_larger_than_a_pipe_buffer_is_read_whole_on_both_streams() {
        let _g = serial();
        // stderr first: a reader that drains stdout before stderr would block the fake forever.
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
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
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
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
        assert!(err.contains("returned no workspace"), "{err}");
        assert_eq!(heads(&f), ["workspace create"]);

        let f = fake_herdr(&[("workspace create", "echo 'not json'")]);
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
        assert!(err.contains("returned no workspace"), "{err}");

        let f = fake_herdr(&[(
            "workspace create",
            r#"echo '{"result":{"workspace":{"workspace_id":"w9"}}}'"#,
        )]);
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
        assert!(err.contains("returned no pane"), "{err}");
        assert_eq!(heads(&f), ["workspace create", "workspace close"]);
    }

    #[test]
    fn a_template_that_renders_to_nothing_creates_nothing() {
        let _g = serial();
        let f = fake_herdr(&[]);
        let err = launch(&f.bin, "/b", "  ", "s", "t").unwrap_err();
        assert!(err.contains("renders to nothing"), "{err}");
        assert!(f.calls().is_empty());
    }

    #[test]
    fn a_failed_focus_fails_but_keeps_the_workspace() {
        let _g = serial();
        let f = fake_herdr(&[created(), ("workspace focus", "exit 1")]);
        let err = launch(&f.bin, "/b", "x {id}", "s", "t").unwrap_err();
        assert!(
            err.contains("the command was sent to workspace w9") && err.contains("workspace focus"),
            "{err}"
        );
        assert_eq!(
            heads(&f),
            ["workspace create", "pane run", "workspace focus"]
        );
    }

    const LIST_WITH_S1: &str =
        r#"{"result":{"panes":[{"pane_id":"w3:p2","agent_session":{"value":"s1"}}]}}"#;

    #[test]
    fn a_running_session_is_focused_and_no_workspace_is_created() {
        let _g = serial();
        let dir = unique_dir("running");
        let f = fake_herdr(&[("pane list", &reply(LIST_WITH_S1)), created()]);
        resume(
            &f.bin,
            "claude --resume {id}",
            "s1",
            dir.to_str().unwrap(),
            "t",
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(f.calls()[1], ["agent", "focus", "w3:p2"]);
        assert_eq!(heads(&f), ["pane list", "agent focus"]);
    }

    #[test]
    fn a_running_session_is_focused_even_when_its_directory_is_gone() {
        let _g = serial();
        let f = fake_herdr(&[("pane list", &reply(LIST_WITH_S1))]);
        resume(&f.bin, "x {id}", "s1", "/nonexistent/dir", "t").unwrap();
        assert_eq!(heads(&f), ["pane list", "agent focus"]);
    }

    #[test]
    fn a_failed_focus_of_a_running_session_is_an_error() {
        let _g = serial();
        let f = fake_herdr(&[
            ("pane list", &reply(LIST_WITH_S1)),
            ("agent focus", "exit 1"),
        ]);
        let err = resume(&f.bin, "x {id}", "s1", "/", "t").unwrap_err();
        assert!(err.contains("agent focus"), "{err}");
    }

    #[test]
    fn a_session_whose_directory_is_gone_creates_nothing_and_names_the_directory() {
        let _g = serial();
        let f = fake_herdr(&[
            ("pane list", &reply(r#"{"result":{"panes":[]}}"#)),
            created(),
        ]);
        let err = resume(&f.bin, "x {id}", "s1", "/nonexistent/gone dir", "t").unwrap_err();
        assert!(err.contains("/nonexistent/gone dir"), "{err}");
        assert_eq!(heads(&f), ["pane list"]);
    }

    #[test]
    fn a_session_without_a_recorded_directory_creates_nothing_and_says_so() {
        let _g = serial();
        let f = fake_herdr(&[
            ("pane list", &reply(r#"{"result":{"panes":[]}}"#)),
            created(),
        ]);
        let err = resume(&f.bin, "x {id}", "s1", "", "t").unwrap_err();
        assert!(err.contains("records no directory"), "{err}");
        assert_eq!(heads(&f), ["pane list"]);
    }

    #[test]
    fn a_session_not_running_is_created_run_and_focused_in_its_directory() {
        let _g = serial();
        let dir = unique_dir("notrunning");
        let cwd = dir.to_str().unwrap();
        let f = fake_herdr(&[
            (
                "pane list",
                &reply(
                    r#"{"result":{"panes":[{"pane_id":"p1","agent_session":{"value":"other"}}]}}"#,
                ),
            ),
            created(),
        ]);
        resume(&f.bin, "my-resume {id}", "s1", cwd, "Fix the build").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            heads(&f),
            [
                "pane list",
                "workspace create",
                "pane run",
                "workspace focus"
            ]
        );
        let calls = f.calls();
        assert_eq!(
            calls[1],
            [
                "workspace",
                "create",
                "--cwd",
                cwd,
                "--label",
                "Fix the build",
                "--no-focus"
            ]
        );
        assert_eq!(calls[2], ["pane", "run", "w9:p1", "my-resume 's1'"]);
        assert_eq!(calls[3], ["workspace", "focus", "w9"]);
    }

    #[test]
    fn a_failing_pane_list_at_enter_stops_before_anything_is_created() {
        let _g = serial();
        let f = fake_herdr(&[("pane list", "exit 1"), created()]);
        let err = resume(&f.bin, "x {id}", "s1", "/", "t").unwrap_err();
        assert!(err.contains("pane list"), "{err}");
        assert_eq!(heads(&f), ["pane list"]);
    }
}
