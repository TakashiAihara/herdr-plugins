use crate::config::Scope;
use crate::extract::{fold, Meta};
use crate::index::Index;

const TIER_NAMES: [&str; 3] = ["user", "assistant", "tools"];
// Characters of context kept on each side of a hit.
const SNIPPET_SIDE: usize = 60;

pub struct Entry {
    pub id: String,
    pub meta: Meta,
    pub marked: bool,
    // The searchable meta text, lowercase, one field per line.
    head: String,
    // Loaded the first time a search needs them.
    bodies: [Option<String>; 3],
    read_error: Option<String>,
}

impl Entry {
    pub fn new(id: String, meta: Meta, marked: bool) -> Entry {
        let head = [
            &meta.custom_title,
            &meta.ai_title,
            &meta.first_prompt,
            &meta.cwd,
            &meta.branch,
            &id,
        ]
        .map(|s| fold(s))
        .join("\n");
        Entry {
            id,
            meta,
            marked,
            head,
            bodies: [None, None, None],
            read_error: None,
        }
    }

    pub fn title(&self) -> String {
        self.meta.title(&self.id)
    }

    // A body that could not be read matches nothing this time and is read again the next time.
    fn body(&mut self, index: &Index, tier: usize) -> &str {
        if self.bodies[tier].is_none() {
            match index.body(&self.id, tier, self.meta.lens[tier]) {
                Ok(text) => self.bodies[tier] = Some(text),
                Err(e) => self.read_error = Some(e),
            }
        }
        self.bodies[tier].as_deref().unwrap_or("")
    }

    fn matches(&mut self, index: &Index, terms: &[&str], scope: Scope) -> bool {
        terms.iter().all(|t| {
            self.head.contains(t) || (0..scope.tiers()).any(|k| self.body(index, k).contains(t))
        })
    }
}

struct Last {
    scope: Scope,
    query: String,
    hits: Vec<usize>,
}

pub struct Searcher {
    index: Index,
    // Newest activity first.
    pub entries: Vec<Entry>,
    last: Option<Last>,
}

// The terms of a query, folded as the indexed text is and single-spaced.
fn normalize(query: &str) -> String {
    query
        .split_whitespace()
        .map(fold)
        .collect::<Vec<_>>()
        .join(" ")
}

fn order(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    b.meta
        .last_ms
        .cmp(&a.meta.last_ms)
        .then_with(|| a.id.cmp(&b.id))
}

fn window(text: &str, at: usize, len: usize) -> String {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let from = text[start..at]
        .char_indices()
        .rev()
        .nth(SNIPPET_SIDE - 1)
        .map(|(i, _)| start + i);
    let to = text[at + len..end]
        .char_indices()
        .nth(SNIPPET_SIDE)
        .map(|(i, _)| at + len + i);
    format!(
        "{}{}{}",
        if from.is_some() { "…" } else { "" },
        &text[from.unwrap_or(start)..to.unwrap_or(end)],
        if to.is_some() { "…" } else { "" },
    )
}

impl Searcher {
    pub fn new(index: Index, entries: Vec<Entry>) -> Searcher {
        let mut s = Searcher {
            index,
            entries,
            last: None,
        };
        s.entries.sort_by(order);
        s
    }

    // Adds a session or replaces the one with its id; a session that is no longer listed is
    // removed. Positions held from an earlier search are no longer valid.
    pub fn upsert(&mut self, entry: Entry) {
        self.entries.retain(|e| e.id != entry.id);
        if entry.meta.listed() {
            self.entries.push(entry);
        }
        self.entries.sort_by(order);
        self.last = None;
    }

    // Positions in `entries` of the sessions every term of `query` matches in `scope`.
    pub fn search(&mut self, query: &str, scope: Scope) -> Vec<usize> {
        let q = normalize(query);
        let terms: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
        // The errors kept are those of this search: a body that reads now is not held against it.
        for e in &mut self.entries {
            e.read_error = None;
        }
        // A query that only extends the last one, in the same scope, can only lose matches.
        let candidates: Vec<usize> = match &self.last {
            Some(l) if l.scope == scope && q.starts_with(&l.query) => l.hits.clone(),
            _ => (0..self.entries.len()).collect(),
        };
        let hits: Vec<usize> = candidates
            .into_iter()
            .filter(|&i| self.entries[i].matches(&self.index, &terms, scope))
            .collect();
        // Hits found while a body could not be read are not a base to narrow from.
        let complete = self.entries.iter().all(|e| e.read_error.is_none());
        self.last = complete.then(|| Last {
            scope,
            query: q.clone(),
            hits: hits.clone(),
        });
        hits
    }

    // Index files the last search could not read; taking them empties the list.
    pub fn take_errors(&mut self) -> Vec<String> {
        self.entries
            .iter_mut()
            .filter_map(|e| e.read_error.take())
            .collect()
    }

    // Where the first term that only a body matches sits in session `at`, with its tier's name.
    pub fn snippet(&self, at: usize, query: &str, scope: Scope) -> Option<(&'static str, String)> {
        let e = self.entries.get(at)?;
        let q = normalize(query);
        for term in q
            .split(' ')
            .filter(|t| !t.is_empty() && !e.head.contains(t))
        {
            for (k, body) in e.bodies.iter().take(scope.tiers()).enumerate() {
                let Some(body) = body else { continue };
                if let Some(pos) = body.find(term) {
                    return Some((TIER_NAMES[k], window(body, pos, term.len())));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{corpus as build, Corpus, Spec};
    use std::fs;

    struct Fx {
        root: std::path::PathBuf,
        s: Searcher,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fx(name: &str, specs: &[Spec]) -> Fx {
        let Corpus { root, s } = build(name, specs);
        Fx { root, s }
    }

    fn ids(f: &Fx, hits: &[usize]) -> Vec<String> {
        hits.iter().map(|&i| f.s.entries[i].id.clone()).collect()
    }

    fn corpus(name: &str) -> Fx {
        fx(
            name,
            &[
                (
                    "s-old",
                    1_000,
                    "Deploy script",
                    "please fix the deploy\n",
                    "ok\n",
                    "bash {\"command\":\"ls\"}\n",
                ),
                (
                    "s-mid",
                    3_000,
                    "Billing",
                    "why is billing slow\n",
                    "the cache is cold\n",
                    "grep -rn wantedword src\n",
                ),
                (
                    "s-new",
                    7_000,
                    "ビルド修正",
                    "ビルドが落ちる\n",
                    "原因は設定です\n",
                    "cargo build\n",
                ),
            ],
        )
    }

    #[test]
    fn an_empty_query_lists_everything_newest_activity_first() {
        let mut f = corpus("order");
        let hits = f.s.search("", Scope::Meta);
        assert_eq!(ids(&f, &hits), ["s-new", "s-mid", "s-old"]);
        let hits = f.s.search("   ", Scope::Tools);
        assert_eq!(ids(&f, &hits), ["s-new", "s-mid", "s-old"]);
    }

    #[test]
    fn equal_times_are_ordered_by_id_so_the_order_is_stable() {
        let f = fx(
            "ties",
            &[
                ("b", 5, "t", "", "", ""),
                ("a", 5, "t", "", "", ""),
                ("c", 9, "t", "", "", ""),
            ],
        );
        let order: Vec<_> = f.s.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, ["c", "a", "b"]);
    }

    #[test]
    fn meta_scope_searches_the_title_cwd_and_id_but_not_the_bodies() {
        let mut f = corpus("meta");
        let hits = f.s.search("billing", Scope::Meta);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("/w/s-old", Scope::Meta);
        assert_eq!(ids(&f, &hits), ["s-old"]);
        let hits = f.s.search("slow", Scope::Meta);
        assert!(hits.is_empty());
    }

    #[test]
    fn a_term_only_in_tools_does_not_match_in_assistant_scope_but_does_in_tools_scope() {
        let mut f = corpus("tiers");
        assert!(f.s.search("wantedword", Scope::Assistant).is_empty());
        assert!(f.s.search("wantedword", Scope::User).is_empty());
        let hits = f.s.search("wantedword", Scope::Tools);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
    }

    #[test]
    fn each_scope_includes_the_ones_before_it() {
        let mut f = corpus("cumulative");
        let hits = f.s.search("billing", Scope::Tools);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("cache", Scope::Assistant);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("slow", Scope::Assistant);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
    }

    #[test]
    fn terms_may_match_in_different_tiers_and_all_of_them_must() {
        let mut f = corpus("and");
        let hits = f.s.search("billing cache wantedword", Scope::Tools);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("billing deploy", Scope::Tools);
        assert!(hits.is_empty());
        let hits = f.s.search("billing  cache", Scope::Assistant);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
    }

    #[test]
    fn matching_ignores_case_and_finds_japanese_text() {
        let mut f = corpus("case");
        let hits = f.s.search("BILLING Slow", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("ビルドが", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-new"]);
        let hits = f.s.search("設定", Scope::Assistant);
        assert_eq!(ids(&f, &hits), ["s-new"]);
    }

    #[test]
    fn a_greek_word_matches_whichever_sigma_the_query_or_the_title_ends_with() {
        let mut f = fx("sigma", &[("s1", 5, "ΟΔΟΣ", "", "", "")]);
        assert_eq!(f.s.search("οδοσ", Scope::Meta), [0]);
        assert_eq!(f.s.search("οδος", Scope::Meta), [0]);
    }

    #[test]
    fn extending_the_query_narrows_to_the_same_result_as_a_fresh_search() {
        let mut f = corpus("narrow");
        f.s.search("b", Scope::User);
        f.s.search("bi", Scope::User);
        let narrowed = f.s.search("bil", Scope::User);
        let fresh = corpus("narrow-fresh").s.search("bil", Scope::User);
        assert_eq!(narrowed, fresh);
        assert_eq!(ids(&f, &narrowed), ["s-mid"]);
        // A new term after a space narrows too.
        let two = f.s.search("bil slow", Scope::User);
        assert_eq!(ids(&f, &two), ["s-mid"]);
        let none = f.s.search("bil zzz", Scope::User);
        assert!(none.is_empty());
    }

    #[test]
    fn a_shorter_query_or_another_scope_after_a_narrow_one_is_searched_afresh() {
        let mut f = corpus("widen");
        assert!(f.s.search("zzzz", Scope::Meta).is_empty());
        let hits = f.s.search("billing", Scope::Meta);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("billing", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        f.s.search("slo", Scope::User);
        let hits = f.s.search("slo", Scope::Meta);
        assert!(hits.is_empty());
        let hits = f.s.search("", Scope::Meta);
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn narrowing_does_not_reuse_a_result_the_query_does_not_extend() {
        let mut f = corpus("noprefix");
        f.s.search("deploy", Scope::User);
        let hits = f.s.search("billing", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        let hits = f.s.search("deploy billing", Scope::User);
        assert!(hits.is_empty());
    }

    #[test]
    fn bodies_are_read_only_when_a_scope_and_a_term_need_them() {
        let mut f = corpus("lazy");
        f.s.search("", Scope::Tools);
        f.s.search("billing", Scope::Meta);
        assert!(f
            .s
            .entries
            .iter()
            .all(|e| e.bodies.iter().all(Option::is_none)));
        f.s.search("slow", Scope::User);
        let loaded: Vec<_> =
            f.s.entries
                .iter()
                .map(|e| (e.bodies[0].is_some(), e.bodies[2].is_some()))
                .collect();
        assert_eq!(loaded, [(true, false), (true, false), (true, false)]);
    }

    #[test]
    fn upserting_replaces_by_id_resorts_and_forgets_the_last_result() {
        let mut f = corpus("upsert");
        f.s.search("billing", Scope::Meta);
        let meta = Meta {
            custom_title: "Billing again".into(),
            last_ms: Some(9_999),
            ..Meta::default()
        };
        f.s.upsert(Entry::new("s-old".into(), meta, false));
        let order: Vec<_> = f.s.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, ["s-old", "s-new", "s-mid"]);
        let hits = f.s.search("billing", Scope::Meta);
        assert_eq!(ids(&f, &hits), ["s-old", "s-mid"]);
        f.s.upsert(Entry::new("s-old".into(), Meta::default(), false));
        assert_eq!(f.s.entries.len(), 2);
    }

    #[test]
    fn a_missing_body_file_is_reported_and_matches_nothing_instead_of_failing_silently() {
        let mut f = corpus("unreadable");
        fs::remove_file(f.root.join("index/s-mid.user")).unwrap();
        let hits = f.s.search("billing slow", Scope::User);
        assert!(hits.is_empty());
        let errs = f.s.take_errors();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("s-mid.user"), "{errs:?}");
        assert!(f.s.take_errors().is_empty());
    }

    #[test]
    fn a_body_that_could_not_be_read_is_read_again_and_its_hits_are_not_narrowed_from() {
        let mut f = corpus("retry");
        let path = f.root.join("index/s-mid.user");
        let saved = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        // "y" is in s-old's title and only in s-mid's prompt, which cannot be read.
        let hits = f.s.search("y", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-old"]);
        fs::write(&path, saved).unwrap();
        // The same query again would narrow from the incomplete result if that had been kept.
        let hits = f.s.search("y", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-mid", "s-old"]);
        // A search without a failed read is a base to narrow from again.
        assert!(f.s.last.is_some());
        assert!(f.s.take_errors().is_empty(), "the failed read is behind us");
        // The body read on the retry stays in memory: its file is not needed again.
        fs::remove_file(&path).unwrap();
        let hits = f.s.search("y is", Scope::User);
        assert_eq!(ids(&f, &hits), ["s-mid"]);
        assert!(f.s.take_errors().is_empty());
    }

    #[test]
    fn the_snippet_names_the_tier_and_shows_the_hit_with_context_on_both_sides() {
        let long_left = "l".repeat(100);
        let long_right = "r".repeat(100);
        let record = format!("{long_left}needle{long_right}\nother record\n");
        let mut f = fx("snip", &[("s1", 5, "T", "", &record, "")]);
        f.s.search("needle", Scope::Assistant);
        let (tier, text) = f.s.snippet(0, "needle", Scope::Assistant).unwrap();
        assert_eq!(tier, "assistant");
        assert_eq!(
            text,
            format!("…{}needle{}…", "l".repeat(60), "r".repeat(60))
        );
    }

    #[test]
    fn a_snippet_stays_inside_its_record_and_cuts_on_character_boundaries() {
        let record = "前の記録です\nあいうえお検索語かきくけこ\n次の記録\n";
        let mut f = fx("snip2", &[("s1", 5, "T", record, "", "")]);
        f.s.search("検索語", Scope::User);
        let (tier, text) = f.s.snippet(0, "検索語", Scope::User).unwrap();
        assert_eq!(
            (tier, text.as_str()),
            ("user", "あいうえお検索語かきくけこ")
        );

        let wide = format!("{}検索語{}\n", "あ".repeat(70), "い".repeat(70));
        let mut f = fx("snip3", &[("s1", 5, "T", &wide, "", "")]);
        f.s.search("検索語", Scope::User);
        let (_, text) = f.s.snippet(0, "検索語", Scope::User).unwrap();
        assert_eq!(
            text,
            format!("…{}検索語{}…", "あ".repeat(60), "い".repeat(60))
        );
    }

    #[test]
    fn there_is_no_snippet_for_a_meta_hit_an_empty_query_or_a_scope_that_does_not_reach_the_tier() {
        let mut f = corpus("nosnip");
        f.s.search("billing", Scope::Tools);
        assert_eq!(f.s.snippet(1, "billing", Scope::Tools), None);
        assert_eq!(f.s.snippet(1, "", Scope::Tools), None);
        f.s.search("wantedword", Scope::Tools);
        assert_eq!(f.s.snippet(1, "wantedword", Scope::Assistant), None);
        let (tier, text) = f.s.snippet(1, "wantedword", Scope::Tools).unwrap();
        assert_eq!((tier, text.as_str()), ("tools", "grep -rn wantedword src"));
        assert_eq!(f.s.snippet(99, "wantedword", Scope::Tools), None);
    }

    #[test]
    fn the_snippet_is_for_the_term_a_body_matched_when_another_term_is_in_the_title() {
        let mut f = corpus("snip-two");
        f.s.search("billing cache", Scope::Assistant);
        let (tier, text) = f.s.snippet(1, "billing cache", Scope::Assistant).unwrap();
        assert_eq!((tier, text.as_str()), ("assistant", "the cache is cold"));
    }
}
