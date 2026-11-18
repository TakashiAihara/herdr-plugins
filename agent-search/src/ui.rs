use crate::config::{Config, Keys};
use crate::keys::{parse_key, Bind};
use crate::rows::{self, Row};
use crate::state::State;
use crate::{act, focus_in, header_text, preview_text, Env, Res, Snapshot};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
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
    pub fn new(k: &Keys) -> Res<Binds> {
        let mut list = Vec::new();
        for (name, op) in [
            (&k.sort, Op::SortNext),
            (&k.group, Op::GroupNext),
            (&k.up, Op::Up),
            (&k.down, Op::Down),
            (&k.clear, Op::Clear),
        ] {
            let bind =
                parse_key(name).ok_or_else(|| format!("unknown key name {name:?} in keys"))?;
            list.push((bind, op));
        }
        for n in 1..=9u8 {
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
    // The last failure of an action, shown under the list until the next key.
    pub status: String,
}

// Rows whose search text matches every word of the query, in the order they have.
fn matching(rows: &[Row], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..rows.len()).collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
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
    // the first pane.
    fn refresh(&mut self, keep: Option<String>) {
        let hits = matching(&self.rows, &self.query);
        let count = |g: &str| {
            hits.iter()
                .filter(|&&i| self.rows[i].group.as_deref() == Some(g))
                .count()
        };
        let mut items = Vec::new();
        let mut prev: Option<&str> = None;
        for &i in &hits {
            let group = self.rows[i].group.as_deref();
            if let Some(g) = group.filter(|g| prev != Some(*g)) {
                items.push(Item::Header(format!("── {g} ({}) ──", count(g))));
            }
            prev = group;
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
    // Some terminals report a typed capital without SHIFT; a bind for it always carries SHIFT.
    let shifted = matches!(key.code, KeyCode::Char(c) if c.is_ascii_uppercase());
    let pressed = Bind {
        code: key.code,
        mods: if shifted {
            key.modifiers | KeyModifiers::SHIFT
        } else {
            key.modifiers
        },
    };
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
    let needs_pane = !matches!(op, Op::SortNext | Op::GroupNext);
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

pub fn draw(frame: &mut Frame, app: &App, header: &str, preview: &str) {
    let [head, input, body, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [list_area, preview_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);

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
            Item::Pane(i) => ListItem::new(rows::display(&app.rows[*i], &widths)),
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

    frame.render_widget(
        Paragraph::new(preview.replace('\t', "  "))
            .block(Block::bordered().title("preview"))
            .wrap(Wrap { trim: false }),
        preview_area,
    );
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

// The popup uses this one config for the list, the keys and every act.
fn session(env: &Env, cfg: &Config, term: &mut DefaultTerminal) -> Res<()> {
    let binds = Binds::new(&cfg.keys)?;
    let mut snap = Snapshot::empty();
    let mut app = App::new(snap.rows(env, cfg, &State::load(&env.state_path))?);
    loop {
        let st = State::load(&env.state_path);
        let list = snap.get(env)?;
        let preview = app
            .selected_id()
            .map_or(String::new(), |id| preview_text(list, &id));
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
            has_token: false,
            group: group.map(String::from),
            cols: vec![format!("title {id}")],
            search: search.into(),
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
            up: "alt-up".into(),
            down: "alt-down".into(),
            clear: "alt-0".into(),
        };
        let binds = Binds::new(&keys("ctrl-s")).unwrap_or_else(|e| panic!("{e}"));
        let mut app = grouped();
        let press = |app: &mut App, c, m| handle_key(app, &binds, key(c, m));
        assert_eq!(
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL),
            Action::Op(Op::SortNext)
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
        assert!(Binds::new(&keys("ctrl-nonsense")).is_err());
    }

    #[test]
    fn empty_query_shows_every_row_under_its_header() {
        assert_eq!(
            shown(&grouped()),
            [
                "── alpha (2) ──",
                "a1",
                "a2",
                "── beta (1) ──",
                "b1",
                "── gamma (1) ──",
                "c1"
            ]
        );
    }

    #[test]
    fn filtering_keeps_the_order_and_shows_headers_only_with_matches() {
        let mut app = grouped();
        for c in "alpha".chars() {
            app.type_char(c);
        }
        assert_eq!(shown(&app), ["── alpha (2) ──", "a1", "a2"]);
        app.query.clear();
        for c in "w".chars() {
            app.type_char(c);
        }
        // "w" is in "write docs alpha", "review beta" and "fix login..." has none; order stays a2, b1.
        assert_eq!(
            shown(&app),
            ["── alpha (1) ──", "a2", "── beta (1) ──", "b1"]
        );
        app.backspace();
        assert_eq!(shown(&app).len(), 7);
    }

    #[test]
    fn a_query_with_no_match_leaves_nothing_selectable() {
        let mut app = grouped();
        for c in "zzzz".chars() {
            app.type_char(c);
        }
        assert!(app.items.is_empty());
        assert_eq!(app.selected_id(), None);
        let binds = Binds::new(&Keys {
            sort: "ctrl-s".into(),
            group: "ctrl-g".into(),
            up: "alt-up".into(),
            down: "alt-down".into(),
            clear: "alt-0".into(),
        })
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
        let binds = Binds::new(&Keys {
            sort: "ctrl-s".into(),
            group: "ctrl-g".into(),
            up: "alt-up".into(),
            down: "alt-down".into(),
            clear: "alt-0".into(),
        })
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
                "title\tfix login\n",
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
        assert!(all.contains("── alpha (2) ──"));
        assert!(all.contains("> title a2"));
        assert!(all.contains("title  fix login"));
    }

    fn default_keys(sort: &str) -> Keys {
        Keys {
            sort: sort.into(),
            group: "ctrl-g".into(),
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

        let binds = Binds::new(&default_keys("ctrl-s")).unwrap_or_else(|e| panic!("{e}"));
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
        keys.group = "A".into();
        let binds = Binds::new(&keys).unwrap_or_else(|e| panic!("{e}"));
        let mut app = grouped();
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('A'), KeyModifiers::SHIFT)
            ),
            Action::Op(Op::GroupNext)
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('A'), KeyModifiers::NONE)
            ),
            Action::Op(Op::GroupNext),
            "a terminal that reports a capital without SHIFT"
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('a'), KeyModifiers::NONE)
            ),
            Action::None
        );
        assert_eq!(
            app.query, "a",
            "the lowercase letter is typed into the query"
        );
    }

    fn screen(width: u16, app: &App) -> (Vec<String>, ratatui::layout::Position) {
        let mut term = Terminal::new(TestBackend::new(width, 8)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, app, "h\nk\n", ""))
            .unwrap_or_else(|e| panic!("{e}"));
        let pos = term.get_cursor_position().unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let lines = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        (lines, pos)
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
}
