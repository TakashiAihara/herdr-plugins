use crate::config::{Config, Keys, Match};
use crate::keys::{self, Bind};
use crate::rows::{self, Row};
use crate::state::State;
use crate::{
    act, focus_in, header_text, preview_lines, sync_priorities, Env, PreviewLine, Res, Snapshot,
};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

pub const REORDER_HINT: &str = "clear the search to reorder";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    SortNext,
    GroupNext,
    MatchNext,
    PreviewNext,
    WrapNext,
    Up,
    Down,
    Clear,
    Prio(u8),
}

#[derive(PartialEq, Debug)]
pub enum Action {
    None,
    Quit,
    Focus(String),
    Op(Op),
}

pub struct Binds(Vec<(Bind, Op)>);

impl Binds {
    // Only alt-1..alt-<max> set a priority; the rest of the digits do nothing (they stay reserved
    // in key validation either way).
    pub fn new(k: &Keys, max: u8) -> Res<Binds> {
        let mut list = Vec::new();
        let ops = [
            Op::SortNext,
            Op::GroupNext,
            Op::MatchNext,
            Op::PreviewNext,
            Op::WrapNext,
            Op::Up,
            Op::Down,
            Op::Clear,
        ];
        for ((_, bind), op) in keys::validate(k)?.into_iter().zip(ops) {
            list.push((bind, op));
        }
        for n in 1..=max {
            let code = KeyCode::Char(char::from(b'0' + n));
            list.push((
                Bind {
                    code,
                    mods: KeyModifiers::ALT,
                },
                Op::Prio(n),
            ));
        }
        Ok(Binds(list))
    }
}

pub enum Item {
    Header(String),
    Pane(usize),
}

pub struct App {
    pub rows: Vec<Row>,
    pub query: String,
    pub items: Vec<Item>,
    pub cur: usize,
    pub mode: Match,
    // Whether the preview pane is shown; false gives the list the full width.
    pub show_preview: bool,
    // Whether a preview line wider than the pane wraps with a hanging indent; false cuts it
    // at the pane's right edge.
    pub preview_wrap: bool,
    // The last failure of an action, shown under the list until the next key.
    pub status: String,
}

// The same word syntax as fuzzy mode (', ^, $ and ! keep their meaning) but a plain word must
// appear as a contiguous substring instead of matching with gaps.
fn substring_pattern(query: &str) -> Pattern {
    let mut pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    for atom in &mut pattern.atoms {
        if atom.kind == AtomKind::Fuzzy {
            atom.kind = AtomKind::Substring;
        }
    }
    pattern
}

// Rows whose search text matches every word of the query, in the order they have.
fn matching(rows: &[Row], query: &str, mode: Match) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..rows.len()).collect();
    }
    let pattern = match mode {
        Match::Fuzzy => Pattern::parse(query, CaseMatching::Smart, Normalization::Smart),
        Match::Substring => substring_pattern(query),
    };
    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let mut buf = Vec::new();
    (0..rows.len())
        .filter(|&i| {
            pattern
                .score(Utf32Str::new(&rows[i].search, &mut buf), &mut matcher)
                .is_some()
        })
        .collect()
}

impl App {
    pub fn new(rows: Vec<Row>) -> App {
        let mut app = App {
            rows,
            query: String::new(),
            items: vec![],
            cur: 0,
            mode: Match::Fuzzy,
            show_preview: true,
            preview_wrap: false,
            status: String::new(),
        };
        app.refresh(None);
        app
    }

    pub fn selected_id(&self) -> Option<String> {
        match self.items.get(self.cur) {
            Some(Item::Pane(i)) => Some(self.rows[*i].id.clone()),
            _ => None,
        }
    }

    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    // Rebuilds the visible items; the cursor stays on pane `keep` when it is still shown, else goes to
    // the first pane. A header opens wherever the group path diverges from the previous row's, never
    // selectable and never matching the search, so a header with no matching agents is not shown.
    fn refresh(&mut self, keep: Option<String>) {
        let hits = matching(&self.rows, &self.query, self.mode);
        let count = |prefix: &[String]| {
            hits.iter()
                .filter(|&&i| self.rows[i].groups.starts_with(prefix))
                .count()
        };
        let mut items = Vec::new();
        let mut prev: &[String] = &[];
        for &i in &hits {
            let groups = &self.rows[i].groups;
            let common = groups
                .iter()
                .zip(prev.iter())
                .take_while(|(a, b)| a == b)
                .count();
            for d in common..groups.len() {
                items.push(Item::Header(format!(
                    "{}▾ {} ({})",
                    "  ".repeat(d),
                    groups[d],
                    count(&groups[..=d])
                )));
            }
            prev = groups.as_slice();
            items.push(Item::Pane(i));
        }
        self.cur = keep
            .and_then(|id| {
                items
                    .iter()
                    .position(|it| matches!(it, Item::Pane(i) if self.rows[*i].id == id))
            })
            .or_else(|| items.iter().position(|it| matches!(it, Item::Pane(_))))
            .unwrap_or(0);
        self.items = items;
    }

    pub fn set_rows(&mut self, rows: Vec<Row>) {
        let keep = self.selected_id();
        self.rows = rows;
        self.refresh(keep);
    }

    // A new mode refilters the query as it is; the cursor stays on the selected pane.
    pub fn set_mode(&mut self, mode: Match) {
        if mode != self.mode {
            self.mode = mode;
            let keep = self.selected_id();
            self.refresh(keep);
        }
    }

    pub fn type_char(&mut self, c: char) {
        let keep = self.selected_id();
        self.query.push(c);
        self.refresh(keep);
    }

    pub fn backspace(&mut self) {
        let keep = self.selected_id();
        self.query.pop();
        self.refresh(keep);
    }

    // Moves to the next pane in the direction, stepping over group headers; stays at the ends.
    pub fn move_cursor(&mut self, delta: isize) {
        let mut i = self.cur as isize + delta;
        while i >= 0 && (i as usize) < self.items.len() {
            if matches!(self.items[i as usize], Item::Pane(_)) {
                self.cur = i as usize;
                return;
            }
            i += delta;
        }
    }
}

pub fn handle_key(app: &mut App, binds: &Binds, key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    app.status.clear();
    let pressed = keys::normalize(key.code, key.modifiers);
    if let Some((_, op)) = binds.0.iter().find(|(b, _)| *b == pressed) {
        return Action::Op(*op);
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
        (KeyCode::Enter, _) => app.selected_id().map_or(Action::None, Action::Focus),
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            app.move_cursor(-1);
            Action::None
        }
        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            app.move_cursor(1);
            Action::None
        }
        (KeyCode::Backspace, _) => {
            app.backspace();
            Action::None
        }
        (KeyCode::Char(c), m) if m == KeyModifiers::NONE || m == KeyModifiers::SHIFT => {
            app.type_char(c);
            Action::None
        }
        _ => Action::None,
    }
}

fn op_args(op: Op, id: &str) -> Vec<String> {
    match op {
        Op::SortNext => vec!["sort-next".into()],
        Op::GroupNext => vec!["group-next".into()],
        Op::MatchNext => vec!["match-next".into()],
        Op::PreviewNext => vec!["preview-next".into()],
        Op::WrapNext => vec!["wrap-next".into()],
        Op::Up => vec!["move".into(), "up".into(), id.into()],
        Op::Down => vec!["move".into(), "down".into(), id.into()],
        Op::Clear => vec!["clear".into(), id.into()],
        Op::Prio(n) => vec!["prio".into(), n.to_string(), id.into()],
    }
}

// Runs one op for the selected pane. `run` performs the act and returns the rebuilt rows; a failure
// goes to the status line and the popup stays open.
pub fn dispatch(app: &mut App, op: Op, run: impl FnOnce(Vec<String>) -> Res<Vec<Row>>) {
    // While a query hides rows, the pane's neighbors in the manual order are not the ones on screen.
    if matches!(op, Op::Up | Op::Down) && app.searching() {
        app.status = REORDER_HINT.to_string();
        return;
    }
    let needs_pane = matches!(op, Op::Up | Op::Down | Op::Clear | Op::Prio(_));
    let id = match app.selected_id() {
        Some(id) => id,
        None if needs_pane => return,
        None => String::new(),
    };
    match run(op_args(op, &id)) {
        Ok(rows) => app.set_rows(rows),
        Err(msg) => app.status = msg,
    }
}

// The column a wrapped continuation of one preview row hangs at in a pane `width` cells
// wide: the row's own `hang` while it leaves at least 10 cells for the continuation, else
// the key's indent + 2, else column 0 — a continuation never gets less room than that.
fn hang_for(text: &str, hang: usize, width: usize) -> usize {
    let mut h = hang;
    if width.saturating_sub(h) < 10 {
        h = text.chars().take_while(|c| *c == ' ').count() + 2;
    }
    if width.saturating_sub(h) < 10 {
        h = 0;
    }
    h
}

// Splits one logical preview row into lines of at most `width` display cells. The first
// line starts at column 0; continuations hang at `hang` columns. A character wider than
// the space left on a line moves whole to the next one; one that does not fit on a fresh
// continuation either is cut, never split or left overflowing the pane. When the hang
// alone fills the width there is no room for a continuation, so the line is left whole.
fn wrap_cells(text: &str, hang: usize, width: usize) -> Vec<String> {
    if hang >= width {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    let mut prefix = 0usize;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width && used > prefix {
            lines.push(std::mem::replace(&mut cur, " ".repeat(hang)));
            used = hang;
            prefix = hang;
        }
        if used + w <= width {
            cur.push(c);
            used += w;
        }
    }
    lines.push(cur);
    lines
}

pub fn draw(frame: &mut Frame, app: &App, header: &str, preview: &[PreviewLine]) {
    let [head, input, body, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let (list_area, preview_area) = if app.show_preview {
        let [list, preview] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(body);
        (list, Some(preview))
    } else {
        (body, None)
    };

    frame.render_widget(
        Paragraph::new(header.trim_end()).style(Style::new().add_modifier(Modifier::DIM)),
        head,
    );
    frame.render_widget(Paragraph::new(format!("> {}", app.query)), input);
    let typed = u16::try_from(rows::width(&app.query)).unwrap_or(u16::MAX);
    let x = input
        .x
        .saturating_add(2)
        .saturating_add(typed)
        .min(input.right().saturating_sub(1));
    frame.set_cursor_position((x, input.y));

    let widths = rows::col_widths(&app.rows);
    let items: Vec<ListItem> = app
        .items
        .iter()
        .map(|it| match it {
            Item::Header(h) => ListItem::new(Line::from(Span::styled(
                h.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ))),
            Item::Pane(i) => ListItem::new(format!(
                "{}{}",
                "  ".repeat(app.rows[*i].groups.len()),
                rows::display(&app.rows[*i], &widths)
            )),
        })
        .collect();
    let selected = app.selected_id().map(|_| app.cur);
    let list = List::new(items)
        .block(Block::bordered())
        .highlight_symbol("> ")
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(
        list,
        list_area,
        &mut ListState::default().with_selected(selected),
    );

    if let Some(area) = preview_area {
        // The border takes one column on each side. A wider line is either cut at the right
        // edge or wrapped by hand, hanging at each line's value column; the paragraph itself
        // never wraps, so ratatui's wrapper cannot move a continuation to column 0.
        let width = usize::from(area.width.saturating_sub(2));
        let mut text = String::new();
        for l in preview {
            if app.preview_wrap {
                let hang = hang_for(&l.text, l.hang, width);
                for w in wrap_cells(&l.text, hang, width) {
                    text.push_str(&w);
                    text.push('\n');
                }
            } else {
                text.push_str(&l.text);
                text.push('\n');
            }
        }
        frame.render_widget(
            Paragraph::new(text).block(Block::bordered().title("preview")),
            area,
        );
    }
    frame.render_widget(
        Paragraph::new(app.status.as_str()).style(Style::new().fg(Color::Red)),
        status,
    );
}

fn show_error(term: &mut DefaultTerminal, msg: &str) {
    loop {
        let text = format!("agent-search: {msg}\n\nPress Enter.");
        if term
            .draw(|f| f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), f.area()))
            .is_err()
        {
            return;
        }
        match event::read() {
            Ok(Event::Key(k))
                if k.kind != KeyEventKind::Release
                    && (matches!(k.code, KeyCode::Enter | KeyCode::Esc)
                        || (k.code == KeyCode::Char('c')
                            && k.modifiers == KeyModifiers::CONTROL)) =>
            {
                return
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

// The mode and preview choice the saved state dictates, applied at open and on every reload of
// the list so a choice saved from outside the popup shows up on the next key.
pub(crate) fn apply_state(app: &mut App, cfg: &Config, st: &State) {
    app.set_mode(cfg.mode(st));
    app.show_preview = cfg.preview_on(st);
    app.preview_wrap = cfg.wrap_on(st);
}

// Opening the popup first copies the priority tokens agents set themselves into the state. A
// failed sync does not stop the popup: the error waits in the status line.
pub(crate) fn open(env: &Env, cfg: &Config, snap: &mut Snapshot) -> Res<App> {
    let failed = sync_priorities(env, cfg, snap).err();
    let st = State::load(&env.state_path);
    let mut app = App::new(snap.rows(env, cfg, &st)?);
    apply_state(&mut app, cfg, &st);
    app.status = failed.unwrap_or_default();
    Ok(app)
}

// The popup uses this one config for the list, the keys and every act.
fn session(env: &Env, cfg: &Config, term: &mut DefaultTerminal) -> Res<()> {
    let binds = Binds::new(&cfg.keys, cfg.priority_max)?;
    let mut snap = Snapshot::empty();
    let mut app = open(env, cfg, &mut snap)?;
    loop {
        let st = State::load(&env.state_path);
        apply_state(&mut app, cfg, &st);
        let list = snap.get(env)?;
        let preview = if app.show_preview {
            app.selected_id()
                .map_or_else(Vec::new, |id| preview_lines(list, &id, &cfg.preview_hide))
        } else {
            Vec::new()
        };
        let header = header_text(cfg, &st);
        term.draw(|f| draw(f, &app, &header, &preview))
            .map_err(|e| format!("cannot draw the screen: {e}"))?;
        let Event::Key(key) =
            event::read().map_err(|e| format!("cannot read the keyboard: {e}"))?
        else {
            continue;
        };
        match handle_key(&mut app, &binds, key) {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::Focus(id) => return focus_in(env, snap.get(env)?, &id),
            // The snapshot is the popup's whole view of Herdr, so an act here must not prune against it.
            Action::Op(op) => dispatch(&mut app, op, |args| {
                act(env, cfg, &mut snap, &args, false)?;
                snap.rows(env, cfg, &State::load(&env.state_path))
            }),
        }
    }
}

pub fn run(env: &Env) -> Res<()> {
    let mut term = ratatui::try_init().map_err(|e| format!("cannot start the terminal UI: {e}"))?;
    let result = env.config().and_then(|cfg| session(env, &cfg, &mut term));
    if let Err(msg) = &result {
        show_error(&mut term, msg);
    }
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::Value;

    fn row(id: &str, group: Option<&str>, search: &str) -> Row {
        Row {
            id: id.into(),
            key: id.into(),
            manual: Value::Null,
            idx: 0,
            prio: None,
            has_token: false,
            groups: group.map(|g| vec![g.to_string()]).unwrap_or_default(),
            cols: vec![format!("title {id}")],
            search: search.into(),
        }
    }

    fn row_path(id: &str, groups: &[&str], search: &str) -> Row {
        Row {
            groups: groups.iter().map(|g| g.to_string()).collect(),
            ..row(id, None, search)
        }
    }

    fn grouped() -> App {
        App::new(vec![
            row("a1", Some("alpha"), "fix login alpha"),
            row("a2", Some("alpha"), "write docs alpha"),
            row("b1", Some("beta"), "review beta"),
            row("c1", Some("gamma"), "deploy gamma"),
        ])
    }

    fn nested() -> App {
        App::new(vec![
            row_path("a1", &["alpha"], "fix login alpha"),
            row_path("a2", &["alpha"], "write docs alpha"),
            row_path("b1", &["beta", "w1"], "review beta"),
            row_path("b2", &["beta", "w2"], "deploy beta"),
            row_path("c1", &["beta", "w2"], "build beta"),
        ])
    }

    fn shown(app: &App) -> Vec<String> {
        app.items
            .iter()
            .map(|it| match it {
                Item::Header(h) => h.clone(),
                Item::Pane(i) => app.rows[*i].id.clone(),
            })
            .collect()
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn default_keys_become_binds_and_a_bad_name_is_an_error() {
        let keys = |sort: &str| Keys {
            sort: sort.into(),
            group: "ctrl-g".into(),
            r#match: "ctrl-r".into(),
            preview: "ctrl-o".into(),
            wrap: "ctrl-w".into(),
            up: "alt-up".into(),
            down: "alt-down".into(),
            clear: "alt-0".into(),
        };
        let binds = Binds::new(&keys("ctrl-s"), 9).unwrap_or_else(|e| panic!("{e}"));
        let mut app = grouped();
        let press = |app: &mut App, c, m| handle_key(app, &binds, key(c, m));
        assert_eq!(
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL),
            Action::Op(Op::SortNext)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL),
            Action::Op(Op::MatchNext)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL),
            Action::Op(Op::PreviewNext)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL),
            Action::Op(Op::WrapNext)
        );
        assert_eq!(
            press(&mut app, KeyCode::Down, KeyModifiers::ALT),
            Action::Op(Op::Down)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('0'), KeyModifiers::ALT),
            Action::Op(Op::Clear)
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('7'), KeyModifiers::ALT),
            Action::Op(Op::Prio(7))
        );
        assert!(Binds::new(&keys("ctrl-nonsense"), 9).is_err());
    }

    #[test]
    fn alt_digits_above_the_priority_max_do_nothing() {
        let binds = Binds::new(&default_keys("ctrl-s"), 5).unwrap_or_else(|e| panic!("{e}"));
        let mut app = grouped();
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Char('5'), KeyModifiers::ALT)),
            Action::Op(Op::Prio(5))
        );
        for digit in '6'..='9' {
            assert_eq!(
                handle_key(
                    &mut app,
                    &binds,
                    key(KeyCode::Char(digit), KeyModifiers::ALT)
                ),
                Action::None,
                "alt-{digit}"
            );
            assert!(app.query.is_empty(), "alt-{digit} must not type");
        }
    }

    #[test]
    fn empty_query_shows_every_row_under_its_header() {
        assert_eq!(
            shown(&grouped()),
            [
                "▾ alpha (2)",
                "a1",
                "a2",
                "▾ beta (1)",
                "b1",
                "▾ gamma (1)",
                "c1"
            ]
        );
    }

    #[test]
    fn nested_groups_show_a_header_per_level_counting_the_matches_under_it() {
        assert_eq!(
            shown(&nested()),
            [
                "▾ alpha (2)",
                "a1",
                "a2",
                "▾ beta (3)",
                "  ▾ w1 (1)",
                "b1",
                "  ▾ w2 (2)",
                "b2",
                "c1"
            ]
        );
        let mut app = nested();
        for c in "deploy".chars() {
            app.type_char(c);
        }
        // Only the headers over matching agents are shown, with their count of matches.
        assert_eq!(shown(&app), ["▾ beta (1)", "  ▾ w2 (1)", "b2"]);
        app.query.clear();
        app.refresh(None);
        assert_eq!(shown(&app).len(), 9);
    }

    #[test]
    fn filtering_keeps_the_order_and_shows_headers_only_with_matches() {
        let mut app = grouped();
        for c in "alpha".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["▾ alpha (2)", "a1", "a2"]);
        app.query.clear();
        for c in "w".chars() {
            app.type_char(c);
        }
        // "w" is in "write docs alpha", "review beta" and "fix login..." has none; order stays a2, b1.
        assert_eq!(shown(&app), ["▾ alpha (1)", "a2", "▾ beta (1)", "b1"]);
        app.backspace();
        assert_eq!(shown(&app).len(), 7);
    }

    #[test]
    fn substring_mode_needs_each_word_contiguous_where_fuzzy_allows_gaps() {
        let rows = || {
            vec![
                row("a1", None, "agent-search"),
                row("a2", None, "agents"),
                row("a3", None, "argstar"),
            ]
        };
        let mut app = App::new(rows());
        for c in "agsr".chars() {
            app.type_char(c);
        }
        // "agsr" is a fuzzy match in "agent-search" and "argstar" but a substring of neither.
        assert_eq!(shown(&app), ["a1", "a3"]);
        app.set_mode(Match::Substring);
        assert!(app.items.is_empty());
        let mut app = App::new(rows());
        app.set_mode(Match::Substring);
        for c in "agent".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["a1", "a2"]);
    }

    #[test]
    fn in_substring_mode_every_word_must_appear() {
        let mut app = grouped();
        app.set_mode(Match::Substring);
        for c in "fix alpha".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["▾ alpha (1)", "a1"]);
        app.query.clear();
        for c in "alpha beta".chars() {
            app.type_char(c);
        }
        assert!(
            app.items.is_empty(),
            "no row holds both words as substrings"
        );
    }

    #[test]
    fn the_fzf_prefixes_keep_their_meaning_in_substring_mode() {
        let make = || {
            let mut app = grouped();
            app.set_mode(Match::Substring);
            app
        };
        let mut app = make();
        for c in "'alpha".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["▾ alpha (2)", "a1", "a2"]);
        let mut app = make();
        for c in "^fix".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["▾ alpha (1)", "a1"]);
        let mut app = make();
        for c in "alpha$".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["▾ alpha (2)", "a1", "a2"]);
        let mut app = make();
        for c in "!alpha".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["▾ beta (1)", "b1", "▾ gamma (1)", "c1"]);
    }

    #[test]
    fn substring_mode_turns_only_plain_words_into_substrings() {
        let fresh = || {
            let mut app = App::new(vec![
                row("a1", None, "word first"),
                row("a2", None, "last word"),
                row("a3", None, "a word inside"),
            ]);
            app.set_mode(Match::Substring);
            app
        };
        let mut app = fresh();
        for c in "^word".chars() {
            app.type_char(c);
        }
        assert_eq!(
            shown(&app),
            ["a1"],
            "^ keeps anchoring at the start: a mid-string word must not match"
        );
        let mut app = fresh();
        for c in "word$".chars() {
            app.type_char(c);
        }
        assert_eq!(
            shown(&app),
            ["a2"],
            "$ keeps anchoring at the end: a mid-string word must not match"
        );
    }

    #[test]
    fn substring_mode_keeps_smart_case() {
        let make = || {
            let mut app = App::new(vec![
                row("a1", None, "Login page"),
                row("a2", None, "login page"),
            ]);
            app.set_mode(Match::Substring);
            app
        };
        let mut app = make();
        for c in "login".chars() {
            app.type_char(c);
        }
        assert_eq!(
            shown(&app),
            ["a1", "a2"],
            "a word without capitals ignores case"
        );
        let mut app = make();
        for c in "Login".chars() {
            app.type_char(c);
        }
        assert_eq!(
            shown(&app),
            ["a1"],
            "a capital makes the word case-sensitive"
        );
    }

    #[test]
    fn the_match_toggle_needs_no_pane_and_keeps_the_query() {
        let mut app = grouped();
        for c in "zzzz".chars() {
            app.type_char(c);
        }
        let mut ran = false;
        dispatch(&mut app, Op::MatchNext, |args| {
            ran = true;
            assert_eq!(args, ["match-next"]);
            Ok(vec![])
        });
        assert!(ran);
        assert_eq!(app.query, "zzzz");
    }

    #[test]
    fn the_preview_toggle_needs_no_pane() {
        let mut app = grouped();
        let mut ran = false;
        dispatch(&mut app, Op::PreviewNext, |args| {
            ran = true;
            assert_eq!(args, ["preview-next"]);
            Ok(vec![])
        });
        assert!(ran);
    }

    #[test]
    fn a_query_with_no_match_leaves_nothing_selectable() {
        let mut app = grouped();
        for c in "zzzz".chars() {
            app.type_char(c);
        }
        assert!(app.items.is_empty());
        assert_eq!(app.selected_id(), None);
        let binds = Binds::new(
            &Keys {
                sort: "ctrl-s".into(),
                group: "ctrl-g".into(),
                r#match: "ctrl-r".into(),
                preview: "ctrl-o".into(),
                wrap: "ctrl-w".into(),
                up: "alt-up".into(),
                down: "alt-down".into(),
                clear: "alt-0".into(),
            },
            5,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
    }

    #[test]
    fn the_cursor_starts_on_a_pane_and_skips_headers() {
        let mut app = grouped();
        assert_eq!(app.selected_id().as_deref(), Some("a1"));
        app.move_cursor(1);
        assert_eq!(app.selected_id().as_deref(), Some("a2"));
        app.move_cursor(1);
        assert_eq!(app.selected_id().as_deref(), Some("b1"));
        app.move_cursor(-1);
        assert_eq!(app.selected_id().as_deref(), Some("a2"));
        app.move_cursor(-1);
        app.move_cursor(-1);
        assert_eq!(app.selected_id().as_deref(), Some("a1"));
        for _ in 0..10 {
            app.move_cursor(1);
        }
        assert_eq!(app.selected_id().as_deref(), Some("c1"));
    }

    #[test]
    fn rebuilding_the_rows_keeps_the_cursor_on_the_same_pane() {
        let mut app = grouped();
        app.move_cursor(1);
        app.move_cursor(1);
        assert_eq!(app.selected_id().as_deref(), Some("b1"));
        app.set_rows(vec![
            row("c1", Some("gamma"), "deploy gamma"),
            row("b1", Some("beta"), "review beta"),
            row("a2", Some("alpha"), "write docs alpha"),
        ]);
        assert_eq!(app.selected_id().as_deref(), Some("b1"));
        app.set_rows(vec![row("c1", Some("gamma"), "deploy gamma")]);
        assert_eq!(app.selected_id().as_deref(), Some("c1"));
    }

    #[test]
    fn typing_keeps_the_selected_pane_when_it_still_matches() {
        let mut app = grouped();
        app.move_cursor(1);
        app.move_cursor(1);
        app.type_char('b');
        assert_eq!(app.selected_id().as_deref(), Some("b1"));
    }

    #[test]
    fn plain_keys_edit_the_query_and_enter_focuses() {
        let binds = Binds::new(
            &Keys {
                sort: "ctrl-s".into(),
                group: "ctrl-g".into(),
                r#match: "ctrl-r".into(),
                preview: "ctrl-o".into(),
                wrap: "ctrl-w".into(),
                up: "alt-up".into(),
                down: "alt-down".into(),
                clear: "alt-0".into(),
            },
            5,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut app = grouped();
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Char('b'), KeyModifiers::NONE),
        );
        assert_eq!(app.query, "b");
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(app.query, "");
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Char('n'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Focus("c1".into())
        );
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Quit
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            Action::Quit
        );
    }

    #[test]
    fn the_screen_shows_header_input_list_and_preview() {
        let mut app = grouped();
        app.move_cursor(1);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| {
            draw(
                f,
                &app,
                "sort: priority  group: repo\nctrl-s sort\n",
                &[PreviewLine {
                    text: "title  fix login".into(),
                    hang: 7,
                }],
            )
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let screen: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert!(screen[0].starts_with("sort: priority  group: repo"));
        assert!(screen[1].starts_with("ctrl-s sort"));
        assert!(screen[2].starts_with("> "));
        let all = screen.join("\n");
        assert!(all.contains("▾ alpha (2)"));
        assert!(
            all.contains(">   title a2"),
            "agent rows indent under their header"
        );
        assert!(all.contains("title  fix login"));
    }

    fn default_keys(sort: &str) -> Keys {
        Keys {
            sort: sort.into(),
            group: "ctrl-g".into(),
            r#match: "ctrl-r".into(),
            preview: "ctrl-o".into(),
            wrap: "ctrl-w".into(),
            up: "alt-up".into(),
            down: "alt-down".into(),
            clear: "alt-0".into(),
        }
    }

    #[test]
    fn a_failed_act_goes_to_the_status_line_and_keeps_the_rows_and_the_popup() {
        let mut app = grouped();
        app.move_cursor(1);
        dispatch(&mut app, Op::Prio(3), |args| {
            assert_eq!(args, ["prio", "3", "a2"]);
            Err("cannot set the priority of a2".to_string())
        });
        assert_eq!(app.status, "cannot set the priority of a2");
        assert_eq!(app.selected_id().as_deref(), Some("a2"));
        assert_eq!(shown(&app).len(), 7);

        let binds = Binds::new(&default_keys("ctrl-s"), 5).unwrap_or_else(|e| panic!("{e}"));
        handle_key(&mut app, &binds, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.status, "", "the next key clears the status");

        dispatch(&mut app, Op::SortNext, |_| Ok(vec![row("z9", None, "zed")]));
        assert_eq!(app.selected_id().as_deref(), Some("z9"));
    }

    #[test]
    fn reordering_while_searching_is_refused_with_a_hint() {
        let mut app = grouped();
        app.type_char('a');
        for op in [Op::Up, Op::Down] {
            dispatch(&mut app, op, |_| {
                panic!("the act must not run while a query is active")
            });
            assert_eq!(app.status, REORDER_HINT);
            app.status.clear();
        }
        app.backspace();
        let mut ran = false;
        dispatch(&mut app, Op::Down, |args| {
            ran = true;
            assert_eq!(args, ["move", "down", "a1"]);
            Ok(vec![row("a1", None, "x")])
        });
        assert!(ran);
        assert_eq!(app.status, "");
    }

    #[test]
    fn a_pane_op_with_nothing_selected_does_not_run() {
        let mut app = grouped();
        for c in "zzzz".chars() {
            app.type_char(c);
        }
        dispatch(&mut app, Op::Clear, |_| panic!("no pane is selected"));
        assert_eq!(app.status, "");
    }

    #[test]
    fn a_capital_letter_bind_is_distinct_from_the_lowercase_letter() {
        let mut keys = default_keys("ctrl-s");
        keys.group = "alt-A".into();
        let binds = Binds::new(&keys, 5).unwrap_or_else(|e| panic!("{e}"));
        let mut app = grouped();
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('A'), KeyModifiers::ALT | KeyModifiers::SHIFT)
            ),
            Action::Op(Op::GroupNext)
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('a'), KeyModifiers::ALT | KeyModifiers::SHIFT)
            ),
            Action::Op(Op::GroupNext),
            "a terminal that reports the lowercase letter with SHIFT"
        );
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Char('A'), KeyModifiers::ALT)),
            Action::Op(Op::GroupNext),
            "a terminal that reports a capital without SHIFT"
        );
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Char('a'), KeyModifiers::ALT)),
            Action::None
        );
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(
            app.query, "a",
            "the lowercase letter is typed into the query"
        );
    }

    fn screen(width: u16, app: &App) -> (Vec<String>, ratatui::layout::Position) {
        let mut term = Terminal::new(TestBackend::new(width, 8)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, app, "h\nk\n", &[]))
            .unwrap_or_else(|e| panic!("{e}"));
        let pos = term.get_cursor_position().unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let lines = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        (lines, pos)
    }

    #[test]
    fn a_hidden_preview_gives_the_list_the_full_width() {
        let mut app = grouped();
        let (shown, _) = screen(60, &app);
        assert!(shown.join("\n").contains("preview"));
        assert_eq!(shown[4].matches('│').count(), 4);
        app.show_preview = false;
        let (hidden, _) = screen(60, &app);
        assert!(!hidden.join("\n").contains("preview"));
        assert_eq!(hidden[4].matches('│').count(), 2);
    }

    #[test]
    fn the_status_line_is_drawn_under_the_list() {
        let mut app = grouped();
        app.status = "cannot set the priority of a2".into();
        let (lines, _) = screen(60, &app);
        assert!(lines[7].starts_with("cannot set the priority of a2"));
    }

    #[test]
    fn the_cursor_stays_inside_the_input_line_whatever_the_query_length() {
        let mut app = grouped();
        app.query = "x".repeat(70_000);
        let (_, pos) = screen(20, &app);
        assert_eq!((pos.x, pos.y), (19, 2));
        app.query = "ab".into();
        let (_, pos) = screen(20, &app);
        assert_eq!((pos.x, pos.y), (4, 2));
    }

    #[test]
    fn wrap_cells_hangs_continuations_at_the_given_column() {
        assert_eq!(
            wrap_cells("key  averylongvalue", 4, 10),
            ["key  avery", "    longva", "    lue"]
        );
        // A line that fits is returned whole.
        assert_eq!(wrap_cells("key  short", 4, 10), ["key  short"]);
    }

    #[test]
    fn wrap_cells_counts_display_cells_and_keeps_wide_characters_whole() {
        // "key  " is 5 cells and the value hangs at column 5; width 8 leaves 3 cells a line.
        // Each CJK character is 2 cells and moves to the next line rather than split.
        assert_eq!(
            wrap_cells("key  日本abc", 5, 8),
            ["key  日", "     本a", "     bc"]
        );
        // An emoji is 2 cells too: it does not straddle the edge either.
        assert_eq!(wrap_cells("a😀bc", 1, 4), ["a😀b", " c"]);
        // A hang that fills the width leaves no room for a continuation: the line stays whole.
        assert_eq!(wrap_cells("key  value", 8, 8), ["key  value"]);
    }

    #[test]
    fn wrap_cells_cuts_a_character_that_fits_nowhere() {
        // width - hang = 1: a fresh continuation holds one cell, so each wide character is
        // cut rather than overflow. The second one must not break again — that would emit
        // a continuation holding only the hang.
        assert_eq!(wrap_cells("abcde日本f", 4, 5), ["abcde", "    f"]);
        // With two cells a continuation holds one wide character per line, still whole.
        assert_eq!(
            wrap_cells("abcde日本f", 4, 6),
            ["abcde", "    日", "    本", "    f"]
        );
    }

    #[test]
    fn hang_for_gives_up_the_value_column_when_it_leaves_too_little_room() {
        // The row's own hang while it leaves 10 cells of continuation or more.
        assert_eq!(hang_for("agent          claude", 15, 30), 15);
        // Fewer than 10: the key's indent + 2 — column 2 for a top-level key.
        assert_eq!(hang_for("agent          claude", 25, 30), 2);
        // A nested key's fallback is its own indent + 2.
        assert_eq!(hang_for("  value        s1", 20, 28), 4);
        // Still fewer than 10 there: continuations start at column 0.
        assert_eq!(hang_for("  value        s1", 20, 12), 0);
        // An outline parent's hang is its indent + 2 already; the same rule applies.
        assert_eq!(hang_for("agent_session", 2, 30), 2);
        assert_eq!(hang_for("agent_session", 2, 8), 0);
    }

    fn preview_screen(width: u16, app: &App, preview: &[PreviewLine]) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, 8)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, app, "h\nk\n", preview))
            .unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn the_preview_wrap_toggle_switches_between_cutting_and_hanging_indent() {
        let mut app = grouped();
        // At width 60 the preview's inner width is 22 cells; the line is 32 cells long and
        // its value column hangs at 12.
        let preview = [PreviewLine {
            text: "ab          abcdefghijklmnopqrst".into(),
            hang: 12,
        }];
        app.preview_wrap = false;
        let cut = preview_screen(60, &app, &preview).join("\n");
        assert!(cut.contains("ab          abcdefghij"), "{cut}");
        assert!(
            !cut.contains("klmnopqrst"),
            "a cut line has no continuation: {cut}"
        );
        app.preview_wrap = true;
        let wrapped = preview_screen(60, &app, &preview).join("\n");
        assert!(wrapped.contains("ab          abcdefghij"), "{wrapped}");
        assert!(
            wrapped.contains("            klmnopqrst"),
            "the continuation hangs at the value column: {wrapped}"
        );
    }

    #[test]
    fn a_value_column_too_far_right_falls_back_to_the_keys_indent() {
        let mut app = grouped();
        app.preview_wrap = true;
        // At width 60 the preview's inner width is 22 cells; hang 20 leaves 2, under the
        // 10 a continuation needs, so it hangs at the key's indent + 2 = column 2.
        let preview = [PreviewLine {
            text: "ab                  abcdefghijklmnopqrst".into(),
            hang: 20,
        }];
        let lines = preview_screen(60, &app, &preview);
        let cont = lines
            .iter()
            .find(|l| l.contains("cdefghijklmnopqrst"))
            .unwrap_or_else(|| panic!("{lines:?}"));
        assert!(cont.contains("│  cdefghijklmnopqrst"), "{cont}");
    }

    #[test]
    fn the_wrap_toggle_needs_no_pane() {
        let mut app = grouped();
        for c in "zzzz".chars() {
            app.type_char(c);
        }
        assert!(app.items.is_empty());
        let mut ran = false;
        dispatch(&mut app, Op::WrapNext, |args| {
            ran = true;
            assert_eq!(args, ["wrap-next"]);
            Ok(vec![])
        });
        assert!(ran);
    }
}
