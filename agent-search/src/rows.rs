use crate::config::{pick, Config};
use crate::state::{present, State};
use crate::status::{Indicator, Status};
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Span;
use serde_json::{json, Value};
use std::cmp::Ordering;

pub const LIST_ERR: &str = "cannot read the pane list";

pub struct Row {
    pub id: String,
    pub key: String,
    pub manual: Value,
    pub idx: usize,
    // The priority token's numeric value, 0 included: the sync needs it to tell "pane says no
    // priority" (a 0 clears the saved value) apart from "pane has no token".
    pub prio: Option<Value>,
    pub has_token: bool,
    pub groups: Vec<String>,
    pub cols: Vec<String>,
    // The foreground each column cell is drawn with, parallel to cols: Some only on an
    // agent_status glyph (its status color), None everywhere else.
    pub fg: Vec<Option<Color>>,
    pub search: String,
}

// jq's value order for the types a sort key can hold: null < false < true < number < string < array.
#[derive(PartialEq, PartialOrd)]
pub enum K {
    Null,
    False,
    True,
    Num(f64),
    Str(String),
    Arr(Vec<K>),
}

pub fn kv(v: &Value) -> K {
    match v {
        Value::Null => K::Null,
        Value::Bool(false) => K::False,
        Value::Bool(true) => K::True,
        Value::Number(n) => K::Num(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => K::Str(s.clone()),
        Value::Array(a) => K::Arr(a.iter().map(kv).collect()),
        Value::Object(_) => K::Str(v.to_string()),
    }
}

pub fn cmp<T: PartialOrd>(a: &T, b: &T) -> Ordering {
    a.partial_cmp(b).unwrap_or(Ordering::Equal)
}

pub fn tostring(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        o => o.to_string(),
    }
}

// Tabs, newlines and escapes in a value would split or fake fzf rows, so they become spaces.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if (c as u32) < 0x20 || c == '\u{7f}' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

// The cell width ratatui renders a string at, so padding matches the screen.
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

// jq's split: an empty string has no parts.
fn split(s: &str, sep: char) -> Vec<&str> {
    if s.is_empty() {
        vec![]
    } else {
        s.split(sep).collect()
    }
}

// jq getpath: null stays null, indexing a non-object is an error.
fn getpath(v: &Value, path: &[&str]) -> Result<Value, ()> {
    let mut cur = v;
    for p in path {
        match cur {
            Value::Null => return Ok(Value::Null),
            Value::Object(m) => cur = m.get(*p).unwrap_or(&Value::Null),
            _ => return Err(()),
        }
    }
    Ok(cur.clone())
}

pub fn field(spec: &str, pane: &Value) -> Result<Value, ()> {
    let parts = split(spec, ':');
    let path = split(parts.first().ok_or(())?, '.');
    let v = if path.last() == Some(&"*") {
        let inner = getpath(pane, &path[..path.len() - 1])?;
        let items: Vec<&Value> = match &inner {
            Value::Object(m) => m.values().collect(),
            Value::Array(a) => a.iter().collect(),
            _ => vec![],
        };
        Value::String(
            items
                .into_iter()
                .map(tostring)
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        getpath(pane, &path)?
    };
    if parts.get(1) == Some(&"basename") && !v.is_null() {
        let last = tostring(&v)
            .split('/')
            .filter(|s| !s.is_empty())
            .next_back()
            .map(|s| Value::String(s.to_string()));
        Ok(last.unwrap_or(v))
    } else {
        Ok(v)
    }
}

fn blank(v: &Value) -> bool {
    v.is_null() || v == &json!("")
}

fn to_number(v: &Value) -> Option<Value> {
    match v {
        Value::Number(_) => Some(v.clone()),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<i64>().ok().map(Value::from).or_else(|| {
                s.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
            })
        }
        _ => None,
    }
}

// Only agent panes are listed, the same set as Herdr's Agents view. None when the list has no
// .result.panes array.
fn agent_panes(list: &Value) -> Option<Vec<&Value>> {
    let all = list.get("result")?.get("panes")?.as_array()?;
    Some(all.iter().filter(|p| is_agent(p)).collect())
}

// The one test for an agent pane: `agent` is a non-empty string.
pub fn is_agent(pane: &Value) -> bool {
    pane.get("agent")
        .and_then(Value::as_str)
        .is_some_and(|a| !a.is_empty())
}

// The pane id and session id of every pane in the list, agent or not, which are the keys a saved
// priority or position can belong to. None when the list has no .result.panes array or no pane.
pub fn live_keys(list: &Value) -> Option<std::collections::HashSet<String>> {
    let panes = list.get("result")?.get("panes")?.as_array()?;
    if panes.is_empty() {
        return None;
    }
    let mut keys = std::collections::HashSet::new();
    for p in panes {
        if let Some(id) = p.get("pane_id").and_then(Value::as_str) {
            keys.insert(id.to_string());
        }
        let sid = session_id(p);
        if !sid.is_empty() {
            keys.insert(sid);
        }
    }
    Some(keys)
}

fn session_id(p: &Value) -> String {
    p.get("agent_session")
        .and_then(|s| s.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

struct Pending<'a> {
    pane: &'a Value,
    id: String,
    key: String,
    idx: usize,
    prio: Option<Value>,
    has_token: bool,
    manual: Value,
}

// Resolves config, state and panes into rows in display order. A pane's key is its agent session id
// when it has one, so manual order and priority survive a pane id change; otherwise the pane id.
// Panes that share a session id would share one position and one priority, so only the first pane in
// the list keeps the session id as its key.
pub fn build(
    list: Option<&Value>,
    cfg: &Config,
    st: &State,
    ind: Indicator,
) -> Result<Vec<Row>, String> {
    let err = || LIST_ERR.to_string();
    let Some(list) = list else {
        return Ok(vec![]);
    };
    let panes = agent_panes(list).ok_or_else(err)?;
    let sids: Vec<String> = panes.iter().map(|p| session_id(p)).collect();

    let mut pending = Vec::new();
    for (i, &pane) in panes.iter().enumerate() {
        let id = pane
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(err)?
            .to_string();
        let sid = &sids[i];
        let key = if !sid.is_empty() && !sids[..i].contains(sid) {
            sid.clone()
        } else {
            id.clone()
        };
        let token = pane
            .get("tokens")
            .and_then(|t| t.get(&cfg.priority_token))
            .filter(|v| !v.is_null());
        let prio = token.and_then(to_number);
        let manual = st
            .lookup("order", &key)
            .unwrap_or_else(|| Value::from(1_000_000 + i));
        pending.push(Pending {
            pane,
            id,
            key,
            idx: i,
            prio,
            has_token: token.is_some(),
            manual,
        });
    }

    let si = pick(&cfg.sorts, st.get("sort"));
    let gi = pick(&cfg.groups, st.get("group"));
    let by = &cfg.sorts[si].by;
    let chains = &cfg.groups[gi].levels;

    let mut keyed = Vec::new();
    for p in pending {
        let mut ks: Vec<(bool, K)> = Vec::new();
        for spec in by {
            // Only a trailing ":desc" is a direction; anything else (":basename") stays in the field.
            let (f, desc) = match spec.strip_suffix(":desc") {
                Some(f) => (f, true),
                None => (spec.as_str(), false),
            };
            let v = match f {
                // A token of 0 means no priority: it sorts with the panes that have none.
                "prio" => p
                    .prio
                    .clone()
                    .filter(|v| v.as_i64() != Some(0))
                    .unwrap_or(Value::Null),
                "manual" => p.manual.clone(),
                _ => field(f, p.pane).map_err(|_| err())?,
            };
            let x = match &v {
                Value::Number(n) => {
                    let n = n.as_f64().unwrap_or(0.0);
                    K::Num(if desc { -n } else { n })
                }
                _ => {
                    let s = tostring(&present(Some(&v)).cloned().unwrap_or_else(|| json!("")));
                    if desc {
                        K::Arr(
                            s.chars()
                                .map(|c| K::Num(-f64::from(c as u32)))
                                .chain([K::Num(0.0)])
                                .collect(),
                        )
                    } else {
                        K::Str(s)
                    }
                }
            };
            ks.push((blank(&v), x));
        }
        keyed.push((ks, p));
    }
    keyed.sort_by(|a, b| cmp(&a.0, &b.0));

    let mut rows = Vec::new();
    for (_, p) in keyed {
        // Each level is a fallback chain: the first field with a value names the group at that
        // level, and a pane with none lands on "(none)".
        let mut groups = Vec::new();
        for chain in chains {
            let mut name = "(none)".to_string();
            for spec in chain {
                let v = field(spec, p.pane).map_err(|_| err())?;
                if !blank(&v) {
                    name = clean(&tostring(&v));
                    break;
                }
            }
            groups.push(name);
        }
        let mut cols = Vec::new();
        let mut fg = Vec::new();
        for c in &cfg.columns {
            if c == "prio" {
                cols.push(match &p.prio {
                    Some(v) if v.as_i64() != Some(0) => format!("★{}", tostring(v)),
                    _ => "  ".to_string(),
                });
                fg.push(None);
                continue;
            }
            let v = field(c, p.pane).map_err(|_| err())?;
            let v = present(Some(&v)).cloned().unwrap_or_else(|| json!(""));
            // An agent_status column shows Herdr's one-cell status glyph instead of the word;
            // the word stays in the search text and the sort and group fields. A value that is
            // not a known status shows as is.
            if c.split(':').next() == Some("agent_status") {
                if let Some(s) = v.as_str().and_then(Status::from_name) {
                    cols.push(s.glyph(ind).to_string());
                    fg.push(Some(cfg.status_color(s)));
                    continue;
                }
            }
            cols.push(clean(&tostring(&v)));
            fg.push(None);
        }
        let mut search = Vec::new();
        for s in &cfg.search {
            // A field that cannot be read on a pane (an intermediate value of another type)
            // does not match it; sort, group and column fields stay strict.
            let v = field(s, p.pane).unwrap_or(Value::Null);
            if !blank(&v) {
                search.push(clean(&tostring(&v)));
            }
        }
        rows.push(Row {
            id: p.id,
            key: p.key,
            manual: p.manual,
            idx: p.idx,
            prio: p.prio,
            has_token: p.has_token,
            groups,
            cols,
            fg,
            search: search.join(" "),
        });
    }

    Ok(nest(rows, 0))
}

// Groups at one level are ordered by their highest-ranked agent (first appearance in the sorted
// list), and each group holds its next level ordered the same way: the result is a depth-first
// walk of the tree.
fn nest(rows: Vec<Row>, depth: usize) -> Vec<Row> {
    if rows.iter().all(|r| r.groups.len() <= depth) {
        return rows;
    }
    let mut buckets: Vec<(Option<String>, Vec<Row>)> = Vec::new();
    for r in rows {
        let g = r.groups.get(depth).cloned();
        match buckets.iter_mut().find(|(k, _)| *k == g) {
            Some((_, v)) => v.push(r),
            None => buckets.push((g, vec![r])),
        }
    }
    buckets
        .into_iter()
        .flat_map(|(_, v)| nest(v, depth + 1))
        .collect()
}

// The fzf input: id<TAB>display<TAB>search text, with a header row "-:<n>" before each group.
pub fn col_widths(rows: &[Row]) -> Vec<usize> {
    let ncols = rows.first().map_or(0, |r| r.cols.len());
    (0..ncols)
        .map(|c| rows.iter().map(|r| width(&r.cols[c])).max().unwrap_or(0))
        .collect()
}

// The columns of one row, padded to the widths except the last one.
pub fn display(r: &Row, w: &[usize]) -> String {
    let last = r.cols.len().saturating_sub(1);
    let cols: Vec<String> = r
        .cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i == last {
                c.clone()
            } else {
                format!("{c}{}", " ".repeat(w[i].saturating_sub(width(c))))
            }
        })
        .collect();
    cols.join("  ")
}

// The columns of one row as spans, padded like display(): cells with a color (the status
// glyph) are styled, the rest plain.
pub fn display_spans(r: &Row, w: &[usize]) -> Vec<Span<'static>> {
    let last = r.cols.len().saturating_sub(1);
    let mut spans = Vec::new();
    for (i, c) in r.cols.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let pad = if i == last {
            String::new()
        } else {
            " ".repeat(w[i].saturating_sub(width(c)))
        };
        let text = format!("{c}{pad}");
        match r.fg.get(i).copied().flatten() {
            Some(color) => spans.push(Span::styled(text, Style::new().fg(color))),
            None => spans.push(Span::raw(text)),
        }
    }
    spans
}

pub fn lines(rows: &[Row]) -> String {
    let w = col_widths(rows);
    let mut out = String::new();
    let mut prev: &[String] = &[];
    let mut n = 0;
    for r in rows {
        // A header opens wherever the group path first diverges from the previous row's, one line
        // per new level, indented two spaces per level.
        let common = r
            .groups
            .iter()
            .zip(prev.iter())
            .take_while(|(a, b)| a == b)
            .count();
        for d in common..r.groups.len() {
            let count = rows
                .iter()
                .filter(|x| x.groups.starts_with(&r.groups[..=d]))
                .count();
            out.push_str(&format!(
                "-:{n}\t{}\x1b[1m── {} ({}) ──\x1b[0m\t\n",
                "  ".repeat(d),
                r.groups[d],
                count
            ));
            n += 1;
        }
        prev = r.groups.as_slice();
        out.push_str(&format!(
            "{}\t{}{}\t\x1b[2m{}\x1b[0m\n",
            r.id,
            "  ".repeat(r.groups.len()),
            display(r, &w),
            r.search
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Group, Keys, Match, Named};

    fn cfg(group: Group) -> Config {
        Config {
            columns: vec!["title".into()],
            search: vec!["title".into()],
            sorts: vec![Named {
                name: "t".into(),
                by: vec!["title".into()],
            }],
            groups: vec![group],
            priority_token: "prio".into(),
            priority_max: 5,
            r#match: Match::Fuzzy,
            preview: true,
            preview_wrap: false,
            preview_hide: vec![],
            status_colors: vec![],
            keys: Keys {
                sort: "ctrl-s".into(),
                group: "ctrl-g".into(),
                r#match: "ctrl-r".into(),
                preview: "ctrl-o".into(),
                wrap: "ctrl-w".into(),
                up: "alt-up".into(),
                down: "alt-down".into(),
                clear: "alt-0".into(),
            },
        }
    }

    #[test]
    fn levels_group_rows_into_a_tree_ordered_by_each_levels_best_agent() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "a", "title": "a1", "workspace_id": "w1", "tokens": {"repo": "x"}},
            {"pane_id": "p2", "agent": "a", "title": "a2", "workspace_id": "w2", "tokens": {"repo": "x"}},
            {"pane_id": "p3", "agent": "a", "title": "b1", "workspace_id": "w1", "tokens": {"repo": "y"}},
            {"pane_id": "p4", "agent": "a", "title": "b2", "workspace_id": "w2"},
            {"pane_id": "p5", "agent": "a", "title": "c1", "workspace_id": "w1", "tokens": {"repo": "x"}}
        ]}});
        let cfg = cfg(Group {
            name: "r/w".into(),
            levels: vec![vec!["tokens.repo".into()], vec!["workspace_id".into()]],
        });
        let rows = build(
            Some(&list),
            &cfg,
            &State(Default::default()),
            Indicator::Dots,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["p1", "p5", "p2", "p3", "p4"]
        );
        assert_eq!(
            rows.iter().map(|r| r.groups.clone()).collect::<Vec<_>>(),
            [
                vec!["x".to_string(), "w1".to_string()],
                vec!["x".to_string(), "w1".to_string()],
                vec!["x".to_string(), "w2".to_string()],
                vec!["y".to_string(), "w1".to_string()],
                vec!["(none)".to_string(), "w2".to_string()]
            ]
        );
        assert_eq!(
            lines(&rows),
            concat!(
                "-:0\t\x1b[1m── x (3) ──\x1b[0m\t\n",
                "-:1\t  \x1b[1m── w1 (2) ──\x1b[0m\t\n",
                "p1\t    a1\t\x1b[2ma1\x1b[0m\n",
                "p5\t    c1\t\x1b[2mc1\x1b[0m\n",
                "-:2\t  \x1b[1m── w2 (1) ──\x1b[0m\t\n",
                "p2\t    a2\t\x1b[2ma2\x1b[0m\n",
                "-:3\t\x1b[1m── y (1) ──\x1b[0m\t\n",
                "-:4\t  \x1b[1m── w1 (1) ──\x1b[0m\t\n",
                "p3\t    b1\t\x1b[2mb1\x1b[0m\n",
                "-:5\t\x1b[1m── (none) (1) ──\x1b[0m\t\n",
                "-:6\t  \x1b[1m── w2 (1) ──\x1b[0m\t\n",
                "p4\t    b2\t\x1b[2mb2\x1b[0m\n"
            )
        );
    }

    #[test]
    fn a_token_of_zero_counts_as_no_priority() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "claude", "title": "b", "tokens": {"prio": "0"}},
            {"pane_id": "p2", "agent": "claude", "title": "a", "tokens": {"prio": "3"}},
            {"pane_id": "p3", "agent": "claude", "title": "c"}
        ]}});
        let mut c = cfg(Group {
            name: "g".into(),
            levels: vec![vec![]],
        });
        c.sorts = vec![Named {
            name: "p".into(),
            by: vec!["prio:desc".into()],
        }];
        c.columns = vec!["prio".into(), "title".into()];
        let rows = build(Some(&list), &c, &State(Default::default()), Indicator::Dots)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["p2", "p1", "p3"],
            "a token of 0 sorts with the panes that have none, below a real priority"
        );
        assert_eq!(
            rows.iter().map(|r| r.cols[0].as_str()).collect::<Vec<_>>(),
            ["★3", "  ", "  "],
            "a token of 0 shows an empty priority column"
        );
        assert_eq!(
            rows[1].prio,
            Some(json!(0)),
            "the raw token value stays on the row so the sync can clear the saved priority"
        );
        // And the ascending `prio` sort treats it the same way.
        c.sorts = vec![Named {
            name: "p".into(),
            by: vec!["prio".into()],
        }];
        let rows = build(Some(&list), &c, &State(Default::default()), Indicator::Dots)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["p2", "p1", "p3"]
        );
    }

    #[test]
    fn a_session_id_is_search_text_and_an_unreadable_search_field_only_skips_its_pane() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "claude", "title": "fix", "agent_session": {"value": "sess-1"}},
            {"pane_id": "p2", "agent": "claude", "title": "docs", "agent_session": "not-an-object"},
            {"pane_id": "p3", "agent": "claude", "title": "sess-1 typo"}
        ]}});
        let mut c = cfg(Group {
            name: "g".into(),
            levels: vec![vec![]],
        });
        c.search = vec!["agent_session.value".into()];
        let rows = build(Some(&list), &c, &State(Default::default()), Indicator::Dots)
            .unwrap_or_else(|e| panic!("{e}"));
        let search_of = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .map(|r| r.search.as_str())
                .unwrap_or("<missing>")
        };
        // p2's agent_session is a string, so the field cannot be read on it: the pane stays in
        // the list with no session text instead of failing the whole build. p3's title holds
        // "sess-1" but title is not a search field here, so it does not match either.
        assert_eq!(search_of("p1"), "sess-1");
        assert_eq!(search_of("p2"), "");
        assert_eq!(search_of("p3"), "");
    }

    #[test]
    fn width_counts_cells_the_way_the_terminal_draws_them() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("e\u{301}"), 1);
    }

    #[test]
    fn only_a_non_empty_string_agent_makes_an_agent_pane() {
        let pane = |v: Value| json!({"pane_id": "p", "agent": v});
        assert!(is_agent(&pane(json!("claude"))));
        for no in [json!(""), json!(5), json!(true), json!(null), json!({})] {
            assert!(!is_agent(&pane(no)));
        }
        assert!(!is_agent(&json!({"pane_id": "p"})));
    }

    #[test]
    fn live_keys_cover_every_pane_not_only_agents() {
        let list = json!({"result": {"panes": [
            {"pane_id": "w1:p1", "agent": "claude", "agent_session": {"value": "s1"}},
            {"pane_id": "w1:p2"},
            {"pane_id": "w1:p3", "agent": "", "agent_session": {"value": "s9"}}
        ]}});
        let keys = live_keys(&list).unwrap_or_default();
        let mut got: Vec<&str> = keys.iter().map(String::as_str).collect();
        got.sort();
        assert_eq!(got, ["s1", "s9", "w1:p1", "w1:p2", "w1:p3"]);
        assert!(live_keys(&json!({"result": {"panes": []}})).is_none());
        assert!(live_keys(&json!({"result": {}})).is_none());
    }

    #[test]
    fn an_agent_status_column_shows_the_glyph_and_keeps_the_word_for_search() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "claude", "title": "a", "agent_status": "working"},
            {"pane_id": "p2", "agent": "claude", "title": "b", "agent_status": "done"},
            {"pane_id": "p3", "agent": "claude", "title": "c", "agent_status": "paused"},
            {"pane_id": "p4", "agent": "claude", "title": "d"}
        ]}});
        let mut c = cfg(Group {
            name: "g".into(),
            levels: vec![vec![]],
        });
        c.columns = vec!["agent_status".into(), "title".into()];
        c.search = vec!["title".into(), "agent_status".into()];
        let rows = build(Some(&list), &c, &State(Default::default()), Indicator::Dots)
            .unwrap_or_else(|e| panic!("{e}"));
        let by_id = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id}"))
        };
        // Under dots, working and done share the ● glyph; the color tells them apart. A status
        // word Herdr does not report and a missing status show as is.
        assert_eq!(by_id("p1").cols, ["●", "a"]);
        assert_eq!(by_id("p2").cols, ["●", "b"]);
        assert_eq!(by_id("p3").cols, ["paused", "c"]);
        assert_eq!(by_id("p4").cols, ["", "d"]);
        assert_eq!(by_id("p1").fg, [Some(Color::Yellow), None]);
        assert_eq!(by_id("p2").fg, [Some(Color::Cyan), None]);
        assert_eq!(by_id("p3").fg, [None, None]);
        assert_eq!(by_id("p4").fg, [None, None]);
        // The word stays searchable even though the column shows the glyph.
        assert_eq!(by_id("p1").search, "a working");
        assert_eq!(by_id("p2").search, "b done");
        assert_eq!(by_id("p3").search, "c paused");
        assert_eq!(by_id("p4").search, "d");
    }

    #[test]
    fn the_indicator_style_chooses_the_status_glyphs() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "claude", "title": "a", "agent_status": "working"},
            {"pane_id": "p2", "agent": "claude", "title": "b", "agent_status": "blocked"},
            {"pane_id": "p3", "agent": "claude", "title": "c", "agent_status": "done"},
            {"pane_id": "p4", "agent": "claude", "title": "d", "agent_status": "idle"},
            {"pane_id": "p5", "agent": "claude", "title": "e", "agent_status": "unknown"}
        ]}});
        let mut c = cfg(Group {
            name: "g".into(),
            levels: vec![vec![]],
        });
        c.columns = vec!["agent_status".into()];
        let glyphs = |ind: Indicator| {
            build(Some(&list), &c, &State(Default::default()), ind)
                .unwrap_or_else(|e| panic!("{e}"))
                .iter()
                .map(|r| r.cols[0].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(glyphs(Indicator::Dots), ["●", "●", "●", "○", "·"]);
        assert_eq!(glyphs(Indicator::Symbols), ["◐", "×", "✓", "○", "·"]);
    }

    #[test]
    fn status_colors_overrides_the_default_glyph_color() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "claude", "title": "a", "agent_status": "working"},
            {"pane_id": "p2", "agent": "claude", "title": "b", "agent_status": "idle"}
        ]}});
        let mut c = cfg(Group {
            name: "g".into(),
            levels: vec![vec![]],
        });
        c.columns = vec!["agent_status".into(), "title".into()];
        c.status_colors = vec![
            (Status::Working, Color::Rgb(1, 2, 3)),
            (Status::Idle, Color::Blue),
        ];
        let rows = build(Some(&list), &c, &State(Default::default()), Indicator::Dots)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rows[0].fg, [Some(Color::Rgb(1, 2, 3)), None]);
        assert_eq!(rows[1].fg, [Some(Color::Blue), None]);
        // The spans the popup draws carry the color on the glyph cell only.
        let spans = display_spans(&rows[0], &col_widths(&rows));
        assert_eq!(spans[0].content.as_ref(), "●");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(spans[2].style.fg, None);
    }

    #[test]
    fn an_agent_status_field_with_a_transform_shows_the_glyph_too() {
        let list = json!({"result": {"panes": [
            {"pane_id": "p1", "agent": "claude", "title": "a", "agent_status": "working"}
        ]}});
        let mut c = cfg(Group {
            name: "g".into(),
            levels: vec![vec![]],
        });
        c.columns = vec!["agent_status:basename".into()];
        let rows = build(Some(&list), &c, &State(Default::default()), Indicator::Dots)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rows[0].cols, ["●"]);
        assert_eq!(rows[0].fg, [Some(Color::Yellow)]);
    }
}
