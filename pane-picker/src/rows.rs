use crate::config::{pick, Config};
use crate::state::{present, State};
use serde_json::{json, Value};
use std::cmp::Ordering;

pub const LIST_ERR: &str = "cannot read the pane list";

pub struct Row {
    pub id: String,
    pub key: String,
    pub manual: Value,
    pub idx: usize,
    pub group: Option<String>,
    pub cols: Vec<String>,
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

// Terminal cell width: East Asian wide characters and most emoji take two cells, combining marks
// and variation selectors none. shortcut: ambiguous-width characters count as one cell.
pub fn width(s: &str) -> usize {
    const WIDE: [(u32, u32); 10] = [
        (4352, 4447),
        (11904, 42191),
        (44032, 55203),
        (63744, 64255),
        (65040, 65049),
        (65072, 65135),
        (65280, 65376),
        (65504, 65510),
        (127744, 129791),
        (131072, 262141),
    ];
    s.chars()
        .map(|c| {
            let c = c as u32;
            if WIDE.iter().any(|&(lo, hi)| c >= lo && c <= hi) {
                2
            } else if (768..=879).contains(&c)
                || (65024..=65039).contains(&c)
                || c == 8203
                || c == 8205
            {
                0
            } else {
                1
            }
        })
        .sum()
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
    manual: Value,
}

// Resolves config, state and panes into rows in display order. A pane's key is its agent session id
// when it has one, so manual order and priority survive a pane id change; otherwise the pane id.
// Panes that share a session id would share one position and one priority, so only the first pane in
// the list keeps the session id as its key.
pub fn build(list: Option<&Value>, cfg: &Config, st: &State) -> Result<Vec<Row>, String> {
    let err = || LIST_ERR.to_string();
    let Some(list) = list else {
        return Ok(vec![]);
    };
    let panes = list
        .get("result")
        .and_then(|r| r.get("panes"))
        .and_then(Value::as_array)
        .ok_or_else(err)?;
    let sids: Vec<String> = panes.iter().map(session_id).collect();

    let mut pending = Vec::new();
    for (i, pane) in panes.iter().enumerate() {
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
        let prio = match st.lookup("prio", &key) {
            Some(v) => Some(v),
            None if cfg.priority_source != "manual" => {
                to_number(&field(&cfg.priority_source, pane).map_err(|_| err())?)
            }
            None => None,
        };
        let manual = st
            .lookup("order", &key)
            .unwrap_or_else(|| Value::from(1_000_000 + i));
        pending.push(Pending {
            pane,
            id,
            key,
            idx: i,
            prio,
            manual,
        });
    }

    let si = pick(&cfg.sorts, st.get("sort"));
    let gi = pick(&cfg.groups, st.get("group"));
    let by = &cfg.sorts[si].by;
    let gby = &cfg.groups[gi].by;

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
                "prio" => p.prio.clone().unwrap_or(Value::Null),
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
        let mut groups = Vec::new();
        for g in gby {
            let v = field(g, p.pane).map_err(|_| err())?;
            if !blank(&v) {
                groups.push(clean(&tostring(&v)));
            }
        }
        let group = groups
            .into_iter()
            .next()
            .or_else(|| (!gby.is_empty()).then(|| "(none)".to_string()));
        let mut cols = Vec::new();
        for c in &cfg.columns {
            cols.push(if c == "prio" {
                match &p.prio {
                    None | Some(Value::Null) => "  ".to_string(),
                    Some(v) => format!("★{}", tostring(v)),
                }
            } else {
                let v = field(c, p.pane).map_err(|_| err())?;
                clean(&tostring(
                    &present(Some(&v)).cloned().unwrap_or_else(|| json!("")),
                ))
            });
        }
        let mut search = Vec::new();
        for s in &cfg.search {
            let v = field(s, p.pane).map_err(|_| err())?;
            if !blank(&v) {
                search.push(clean(&tostring(&v)));
            }
        }
        rows.push(Row {
            id: p.id,
            key: p.key,
            manual: p.manual,
            idx: p.idx,
            group,
            cols,
            search: search.join(" "),
        });
    }

    if gby.is_empty() {
        return Ok(rows);
    }
    // Groups in order of first appearance after sorting.
    let mut buckets: Vec<(Option<String>, Vec<Row>)> = Vec::new();
    for r in rows {
        match buckets.iter_mut().find(|(g, _)| *g == r.group) {
            Some((_, v)) => v.push(r),
            None => buckets.push((r.group.clone(), vec![r])),
        }
    }
    Ok(buckets.into_iter().flat_map(|(_, v)| v).collect())
}

// The fzf input: id<TAB>display<TAB>search text, with a header row "-:<n>" before each group.
pub fn lines(rows: &[Row]) -> String {
    let ncols = rows.first().map_or(0, |r| r.cols.len());
    let w: Vec<usize> = (0..ncols)
        .map(|c| rows.iter().map(|r| width(&r.cols[c])).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut prev: Option<&str> = None;
    for r in rows {
        if let Some(g) = r.group.as_deref() {
            if prev != Some(g) {
                if !seen.contains(&g) {
                    seen.push(g);
                }
                let n = seen.iter().position(|s| *s == g).unwrap_or(0);
                let count = rows
                    .iter()
                    .filter(|x| x.group.as_deref() == Some(g))
                    .count();
                out.push_str(&format!("-:{n}\t\x1b[1m── {g} ({count}) ──\x1b[0m\t\n"));
            }
        }
        prev = r.group.as_deref();
        let display: Vec<String> = r
            .cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == ncols - 1 {
                    c.clone()
                } else {
                    format!("{c}{}", " ".repeat(w[i].saturating_sub(width(c))))
                }
            })
            .collect();
        out.push_str(&format!(
            "{}\t{}\t\x1b[2m{}\x1b[0m\n",
            r.id,
            display.join("  "),
            r.search
        ));
    }
    out
}
