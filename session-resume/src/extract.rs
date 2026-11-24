use serde_json::Value;

pub const USER: usize = 0;
pub const ASSISTANT: usize = 1;
pub const TOOLS: usize = 2;

// What the index keeps about one transcript besides the body files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    // The transcript's size when it was last read, and the end of the last complete line read.
    pub size: u64,
    pub offset: u64,
    // The transcript's inode and modification time (ns) at that read: a file put in its place, or
    // rewritten to the same length, is another transcript.
    pub ino: u64,
    pub mtime_ns: i64,
    // The transcript these were read from; the same id found elsewhere is another file.
    pub source: String,
    pub custom_title: String,
    pub ai_title: String,
    pub first_prompt: String,
    pub cwd: String,
    pub branch: String,
    // None until a user or assistant line has been seen: such a session is not listed.
    pub last_ms: Option<i64>,
    // The length of each body file as of this meta.
    pub lens: [u64; 3],
}

impl Meta {
    pub fn listed(&self) -> bool {
        self.last_ms.is_some()
    }

    // The last activity, or None when no line carried a usable time (stored as 0 to sort last).
    pub fn time(&self) -> Option<i64> {
        self.last_ms.filter(|&ms| ms > 0)
    }

    pub fn title(&self, id: &str) -> String {
        [&self.custom_title, &self.ai_title, &self.first_prompt]
            .into_iter()
            .find(|t| !t.is_empty())
            .map_or_else(|| id.to_string(), |t| t.to_string())
    }
}

// One extracted record per line of each body, newest last.
#[derive(Default)]
pub struct Bodies {
    pub text: [String; 3],
}

// The form both the indexed text and a query are compared in.
// NOTE: lowercase plus the Greek final sigma, not a full Unicode case fold (ß and ss, dotted İ);
// use a case-folding crate when a language needs those.
pub fn fold(s: &str) -> String {
    s.to_lowercase().replace('ς', "σ")
}

// A record is one line of folded text, so a substring search is case-insensitive and a hit never
// spans two records.
fn record(s: &str) -> Option<String> {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    (!flat.is_empty()).then(|| fold(flat))
}

// Text Claude Code wraps in these tags is written by the harness (a slash command, its output, a
// `!` command, a paste), not typed as a prompt. Text starting with any other tag is taken as one.
const WRAPPERS: [&str; 13] = [
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "task-notification",
    "system-reminder",
    "user-prompt-submit-hook",
    "pasted_content",
];

fn is_wrapper(t: &str) -> bool {
    t.strip_prefix('<').is_some_and(|rest| {
        WRAPPERS.iter().any(|w| {
            rest.strip_prefix(w)
                .is_some_and(|after| after.starts_with(['>', ' ']))
        })
    })
}

fn push(bodies: &mut Bodies, tier: usize, s: &str) {
    if let Some(r) = record(s) {
        bodies.text[tier].push_str(&r);
        bodies.text[tier].push('\n');
    }
}

// The prompt as shown: control characters become spaces, cut to 200 characters.
fn first_prompt(s: &str) -> String {
    s.chars()
        .take(200)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn text_of_result(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn user_text(meta: &mut Meta, bodies: &mut Bodies, text: &str, is_meta: bool) {
    if is_meta {
        push(bodies, TOOLS, text);
        return;
    }
    push(bodies, USER, text);
    let t = text.trim_start();
    if meta.first_prompt.is_empty() && !t.is_empty() && !is_wrapper(t) {
        meta.first_prompt = first_prompt(t);
    }
}

// Reads one transcript line into `meta` and `bodies`. A line that is not JSON, or not one of the
// kinds that matter, is skipped.
pub fn apply_line(meta: &mut Meta, bodies: &mut Bodies, line: &str) {
    // The substring test spares parsing the very large lines that cannot matter (file snapshots,
    // API requests). Claude Code writes compact JSON, so the key and value touch.
    const KINDS: [&str; 4] = [
        "\"type\":\"user\"",
        "\"type\":\"assistant\"",
        "\"type\":\"ai-title\"",
        "\"type\":\"custom-title\"",
    ];
    if !KINDS.iter().any(|k| line.contains(k)) {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if v["isSidechain"] == true {
        return;
    }
    let kind = v["type"].as_str().unwrap_or("");
    match kind {
        "ai-title" => {
            if let Some(t) = v["aiTitle"].as_str() {
                meta.ai_title = t.to_string();
            }
            return;
        }
        "custom-title" => {
            if let Some(t) = v["customTitle"].as_str() {
                meta.custom_title = t.to_string();
            }
            return;
        }
        "user" | "assistant" => {}
        _ => return,
    }
    if let Some(ms) = v["timestamp"].as_str().and_then(parse_ms) {
        meta.last_ms = Some(ms);
    } else if meta.last_ms.is_none() {
        // A line without a usable time still makes the session real; it sorts last.
        meta.last_ms = Some(0);
    }
    if meta.cwd.is_empty() {
        if let Some(c) = v["cwd"].as_str() {
            meta.cwd = c.to_string();
        }
    }
    if let Some(b) = v["gitBranch"].as_str().filter(|b| !b.is_empty()) {
        meta.branch = b.to_string();
    }
    // Not typed by a person: skill bodies, the summary a compaction leaves, and lines with an
    // origin other than a human (task notifications). A line without an origin is a prompt too
    // (`claude -p`, or a version that did not record one).
    let is_meta = v["isMeta"] == true
        || v["isCompactSummary"] == true
        || v["origin"]["kind"].as_str().is_some_and(|k| k != "human");
    let content = &v["message"]["content"];
    if let Some(s) = content.as_str() {
        if kind == "user" {
            user_text(meta, bodies, s, is_meta);
        } else {
            push(bodies, ASSISTANT, s);
        }
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };
    for b in blocks {
        match (kind, b["type"].as_str().unwrap_or("")) {
            ("user", "text") => user_text(meta, bodies, b["text"].as_str().unwrap_or(""), is_meta),
            ("assistant", "text") => push(bodies, ASSISTANT, b["text"].as_str().unwrap_or("")),
            (_, "tool_use") => {
                let input = serde_json::to_string(&b["input"]).unwrap_or_default();
                push(
                    bodies,
                    TOOLS,
                    &format!("{} {input}", b["name"].as_str().unwrap_or("")),
                );
            }
            (_, "tool_result") => push(bodies, TOOLS, &text_of_result(&b["content"])),
            _ => {}
        }
    }
}

// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

// `2026-11-04T14:44:21.665Z`: UTC only, which is all Claude Code writes.
pub fn parse_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let num = |t: &str| {
        t.parse::<i64>()
            .ok()
            .filter(|_| t.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut d = date.split('-');
    // A four-digit year keeps the arithmetic below far inside i64.
    let y = d.next().filter(|y| y.len() == 4).and_then(num)?;
    let (mo, da) = (num(d.next()?)?, num(d.next()?)?);
    let (hms, frac) = match time.split_once('.') {
        // A point is followed by digits, however many; only the first three are kept.
        Some((hms, frac)) if !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()) => {
            (hms, frac)
        }
        Some(_) => return None,
        None => (time, ""),
    };
    let mut t = hms.split(':');
    let (h, mi, se) = (num(t.next()?)?, num(t.next()?)?, num(t.next()?)?);
    if d.next().is_some() || t.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let ms = if frac.is_empty() {
        0
    } else {
        let digits: String = format!("{frac:0<3}").chars().take(3).collect();
        num(&digits)?
    };
    Some((days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se) * 1000 + ms)
}

pub fn format_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(lines: &[&str]) -> (Meta, Bodies) {
        let mut meta = Meta::default();
        let mut bodies = Bodies::default();
        for l in lines {
            apply_line(&mut meta, &mut bodies, l);
        }
        (meta, bodies)
    }

    const TS: &str = "2026-11-04T14:44:21.665Z";

    fn user(content: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{TS}","cwd":"/w/a","gitBranch":"b1","message":{{"content":{content}}}}}"#
        )
    }

    fn assistant(content: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{TS}","cwd":"/w/a","message":{{"content":{content}}}}}"#
        )
    }

    #[test]
    fn a_user_string_prompt_goes_to_the_user_tier_lowercased_on_one_line() {
        let (_, b) = read(&[&user(r#""Fix THE\nBuild\tNow""#)]);
        assert_eq!(b.text[USER], "fix the build now\n");
        assert_eq!(
            (b.text[ASSISTANT].as_str(), b.text[TOOLS].as_str()),
            ("", "")
        );
    }

    #[test]
    fn user_text_blocks_go_to_the_user_tier_and_japanese_is_kept() {
        let (_, b) = read(&[&user(
            r#"[{"type":"text","text":"ビルドを直して"},{"type":"text","text":"Second"}]"#,
        )]);
        assert_eq!(b.text[USER], "ビルドを直して\nsecond\n");
    }

    #[test]
    fn assistant_text_goes_to_the_assistant_tier_and_thinking_is_left_out() {
        let (_, b) = read(&[&assistant(
            r#"[{"type":"thinking","thinking":"secret plan"},{"type":"text","text":"Done: PATCHED"}]"#,
        )]);
        assert_eq!(b.text[ASSISTANT], "done: patched\n");
        assert!(!b.text.iter().any(|t| t.contains("secret")));
    }

    #[test]
    fn an_assistant_string_content_is_assistant_text() {
        let (_, b) = read(&[&assistant(r#""Plain Reply""#)]);
        assert_eq!(b.text[ASSISTANT], "plain reply\n");
    }

    #[test]
    fn tool_use_is_its_name_and_compact_input_in_the_tools_tier() {
        let (_, b) = read(&[&assistant(
            r#"[{"type":"tool_use","name":"Bash","input":{"command":"ls -la","n":2}}]"#,
        )]);
        assert_eq!(b.text[TOOLS], "bash {\"command\":\"ls -la\",\"n\":2}\n");
        assert_eq!(b.text[ASSISTANT], "");
    }

    #[test]
    fn tool_results_in_both_shapes_go_to_the_tools_tier_not_the_user_tier() {
        let (_, b) = read(&[&user(
            r#"[{"type":"tool_result","content":"Output ONE"},{"type":"tool_result","content":[{"type":"text","text":"Out"},{"type":"text","text":"TWO"}]}]"#,
        )]);
        assert_eq!(b.text[TOOLS], "output one\nout two\n");
        assert_eq!(b.text[USER], "");
    }

    #[test]
    fn meta_user_text_is_tools_and_never_the_first_prompt() {
        let meta_line = format!(
            r#"{{"type":"user","isMeta":true,"timestamp":"{TS}","message":{{"content":"Skill BODY"}}}}"#
        );
        let (m, b) = read(&[&meta_line, &user(r#""Real Prompt""#)]);
        assert_eq!(b.text[TOOLS], "skill body\n");
        assert_eq!(b.text[USER], "real prompt\n");
        assert_eq!(m.first_prompt, "Real Prompt");
    }

    #[test]
    fn user_lines_the_harness_wrote_are_tools_and_a_line_without_an_origin_is_a_prompt() {
        let line = |extra: &str, text: &str| {
            format!(
                r#"{{"type":"user",{extra}"timestamp":"{TS}","message":{{"content":"{text}"}}}}"#
            )
        };
        let (m, b) = read(&[
            &line(
                r#""origin":{"kind":"task-notification"},"#,
                "Agent Finished",
            ),
            &line(r#""isCompactSummary":true,"#, "Earlier Summary"),
            &line("", "Sdk Prompt"),
            &line(r#""origin":{"kind":"human"},"#, "Typed Prompt"),
        ]);
        assert_eq!(b.text[TOOLS], "agent finished\nearlier summary\n");
        assert_eq!(b.text[USER], "sdk prompt\ntyped prompt\n");
        assert_eq!(m.first_prompt, "Sdk Prompt");
    }

    #[test]
    fn sidechain_lines_are_ignored_entirely() {
        let side = r#"{"type":"user","isSidechain":true,"timestamp":"2027-01-01T00:00:00Z","cwd":"/side","gitBranch":"side","message":{"content":"agent prompt"}}"#;
        let side_title = r#"{"type":"ai-title","isSidechain":true,"aiTitle":"Side"}"#;
        let (m, b) = read(&[side, side_title, &user(r#""main""#)]);
        assert_eq!(b.text[USER], "main\n");
        assert_eq!((m.cwd.as_str(), m.branch.as_str()), ("/w/a", "b1"));
        assert_eq!(m.ai_title, "");
        assert_eq!(m.last_ms, parse_ms(TS));
    }

    #[test]
    fn a_custom_title_beats_an_ai_title_whatever_the_order_and_last_of_each_wins() {
        let ai = |t: &str| format!(r#"{{"type":"ai-title","aiTitle":"{t}"}}"#);
        let custom = |t: &str| format!(r#"{{"type":"custom-title","customTitle":"{t}"}}"#);
        let (m, _) = read(&[
            &custom("C1"),
            &ai("A1"),
            &ai("A2"),
            &custom("C2"),
            &ai("A3"),
        ]);
        assert_eq!((m.custom_title.as_str(), m.ai_title.as_str()), ("C2", "A3"));
        assert_eq!(m.title("id1"), "C2");
        let (m, _) = read(&[&ai("A1"), &user(r#""first prompt""#)]);
        assert_eq!(m.title("id1"), "A1");
    }

    #[test]
    fn the_title_falls_back_to_the_first_prompt_then_the_id() {
        let (m, _) = read(&[
            &user(r#""<command-name>/x</command-name>""#),
            &user(r#""  Real one""#),
        ]);
        assert_eq!(m.title("id1"), "Real one");
        assert_eq!(Meta::default().title("id1"), "id1");
    }

    #[test]
    fn the_first_prompt_skips_harness_wrappers_and_is_cut_to_200_characters() {
        let long = "あ".repeat(250);
        let (m, _) = read(&[
            &user(r#""<local-command-stdout>x</local-command-stdout>""#),
            &user(&format!("\"{long}\"")),
            &user(r#""later""#),
        ]);
        assert_eq!(m.first_prompt, "あ".repeat(200));
    }

    #[test]
    fn a_prompt_that_starts_with_a_tag_the_harness_does_not_write_is_the_first_prompt() {
        let (m, _) = read(&[
            &user(r#""<pasted_content id=\"1\">log</pasted_content>""#),
            &user(r#""<bash-input>ls</bash-input>""#),
            &user(r#""<div> breaks the layout""#),
            &user(r#""later""#),
        ]);
        assert_eq!(m.first_prompt, "<div> breaks the layout");
        // A tag that only begins with a wrapper's name is not that wrapper.
        let (m, _) = read(&[&user(r#""<command-names> is misspelt""#)]);
        assert_eq!(m.first_prompt, "<command-names> is misspelt");
    }

    #[test]
    fn folding_lowercases_and_makes_both_greek_sigmas_one() {
        assert_eq!(fold("ΟΔΟΣ Abc"), "οδοσ abc");
        assert_eq!(fold("οδος"), fold("οδοσ"));
        let (_, b) = read(&[&user(r#""ΟΔΟΣ""#)]);
        assert_eq!(b.text[USER], "οδοσ\n");
    }

    #[test]
    fn cwd_is_the_first_seen_and_branch_the_last_seen() {
        let a = r#"{"type":"user","timestamp":"2026-11-04T14:44:21Z","cwd":"/first","gitBranch":"one","message":{"content":"a"}}"#;
        let b = r#"{"type":"assistant","timestamp":"2026-11-04T14:45:21Z","cwd":"/second","gitBranch":"two","message":{"content":"b"}}"#;
        let c = r#"{"type":"user","timestamp":"2026-11-04T14:46:21Z","cwd":"/third","gitBranch":"","message":{"content":"c"}}"#;
        let (m, _) = read(&[a, b, c]);
        assert_eq!((m.cwd.as_str(), m.branch.as_str()), ("/first", "two"));
        assert_eq!(m.last_ms, parse_ms("2026-11-04T14:46:21Z"));
    }

    #[test]
    fn the_last_activity_is_the_last_line_s_time_even_when_an_earlier_line_is_later() {
        let a = r#"{"type":"user","timestamp":"2026-11-04T14:46:21Z","message":{"content":"a"}}"#;
        let b =
            r#"{"type":"assistant","timestamp":"2026-11-04T14:44:21Z","message":{"content":"b"}}"#;
        let (m, _) = read(&[a, b]);
        assert_eq!(m.last_ms, parse_ms("2026-11-04T14:44:21Z"));
    }

    #[test]
    fn invalid_json_and_other_kinds_of_line_are_skipped() {
        let (m, b) = read(&[
            "{not json",
            r#"{"type":"user","timestamp":"#,
            r#"{"type":"attachment","message":{"content":"ignored"}}"#,
            r#"{"type":"api-request","note":"\"type\":\"user\""}"#,
            "",
        ]);
        assert!(!m.listed());
        assert!(b.text.iter().all(String::is_empty));
    }

    #[test]
    fn a_session_with_a_message_but_no_usable_time_is_listed() {
        let (m, _) = read(&[r#"{"type":"user","message":{"content":"a"}}"#]);
        assert_eq!(m.last_ms, Some(0));
        let (m, _) = read(&[
            &user(r#""a""#),
            r#"{"type":"user","timestamp":"bad","message":{"content":"b"}}"#,
        ]);
        assert_eq!(m.last_ms, parse_ms(TS));
    }

    #[test]
    fn empty_records_are_not_stored() {
        let (_, b) = read(&[
            &user(r#""  \n ""#),
            &assistant(r#"[{"type":"text","text":""}]"#),
        ]);
        assert_eq!(
            (b.text[USER].as_str(), b.text[ASSISTANT].as_str()),
            ("", "")
        );
    }

    #[test]
    fn timestamps_parse_to_known_epoch_milliseconds_and_format_back() {
        assert_eq!(parse_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_ms("2026-11-04T14:44:21.665Z"),
            Some(1_793_803_461_665)
        );
        assert_eq!(parse_ms("2024-02-29T23:59:59.5Z"), Some(1_709_251_199_500));
        assert_eq!(
            parse_ms("2024-02-29T23:59:59.5000000000000000000001Z"),
            Some(1_709_251_199_500)
        );
        assert_eq!(format_ms(1_793_803_461_665), "2026-11-04 14:44 UTC");
        assert_eq!(format_ms(1_709_251_199_500), "2024-02-29 23:59 UTC");
        for bad in [
            "",
            "2026-11-04",
            "2026-11-04T14:44:21",
            "2026-13-04T14:44:21Z",
            "2026-11-04T25:00:00Z",
            "x-y-zT1:2:3Z",
            "2026-11-04T14:44Z",
            "300000000-01-01T00:00:00Z",
            "026-11-04T14:44:21Z",
            "2026-11-04T14:44:21.123xyzZ",
            "2026-11-04T14:44:21.Z",
        ] {
            assert_eq!(parse_ms(bad), None, "{bad}");
        }
    }
}
