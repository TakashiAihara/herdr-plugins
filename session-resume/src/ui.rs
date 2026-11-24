use crate::config::{Config, Defaults, Scope, Show};
use crate::extract::{format_ms, Meta};
use crate::index::{self, Index};
use crate::input::Input;
use crate::launch;
use crate::search::{Entry, Searcher};
use crate::{Env, Res};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PAGE: isize = 10;
const PATH_WIDTH: usize = 24;

#[derive(PartialEq, Debug)]
pub struct Target {
    pub id: String,
    pub cwd: String,
    pub title: String,
}

#[derive(PartialEq, Debug)]
pub enum Action {
    None,
    Quit,
    Resume(Target),
}

pub fn is_marked(marker: Option<&str>, id: &str) -> bool {
    marker.is_some_and(|m| Path::new(&m.replace("{id}", id)).exists())
}

pub struct App {
    s: Searcher,
    query: Input,
    scope: Scope,
    show: Show,
    marker: Option<String>,
    running: HashSet<String>,
    // Positions in the searcher's entries, newest first, after the marker filter.
    hits: Vec<usize>,
    cur: usize,
    snippet: Option<(&'static str, String)>,
    now_ms: i64,
    // The last failure, shown under the list until the next key.
    pub status: String,
    // Sessions read so far of those that needed it, while the background refresh runs.
    pub progress: Option<(usize, usize)>,
}

impl App {
    pub fn new(
        s: Searcher,
        cfg: &Config,
        running: HashSet<String>,
        now_ms: i64,
        status: String,
    ) -> App {
        let mut app = App {
            s,
            query: Input::default(),
            scope: cfg.scope,
            show: cfg.show,
            marker: cfg.marker.clone(),
            running,
            hits: Vec::new(),
            cur: 0,
            snippet: None,
            now_ms,
            status,
            progress: None,
        };
        app.research(None);
        app
    }

    fn selected(&self) -> Option<&Entry> {
        self.hits.get(self.cur).map(|&i| &self.s.entries[i])
    }

    // Searches again; the selection stays on session `keep` when it still matches, and goes back
    // to the first row otherwise.
    fn research(&mut self, keep: Option<String>) {
        let raw = self.s.search(&self.query.text, self.scope);
        let show = self.show;
        let hits: Vec<usize> = raw
            .into_iter()
            .filter(|&i| match show {
                Show::All => true,
                Show::Unmarked => !self.s.entries[i].marked,
                Show::Marked => self.s.entries[i].marked,
            })
            .collect();
        self.cur = keep
            .and_then(|id| hits.iter().position(|&i| self.s.entries[i].id == id))
            .unwrap_or(0);
        self.hits = hits;
        if let Some(e) = self.s.take_errors().into_iter().next() {
            self.status = e;
        }
        self.refresh_snippet();
    }

    fn refresh_snippet(&mut self) {
        self.snippet = self
            .hits
            .get(self.cur)
            .and_then(|&i| self.s.snippet(i, &self.query.text, self.scope));
    }

    fn move_cur(&mut self, delta: isize) {
        let last = self.hits.len().saturating_sub(1);
        self.cur = self.cur.saturating_add_signed(delta).min(last);
        self.refresh_snippet();
    }

    // Takes in sessions the background refresh has read.
    pub fn apply(&mut self, metas: Vec<(String, Meta)>, progress: Option<(usize, usize)>) {
        self.progress = progress;
        if !metas.is_empty() {
            // Read before the upserts: they move the entries the hits point at. An untouched
            // cursor stays on the first row, where the newest session arrives.
            let keep = self
                .selected()
                .filter(|_| self.cur > 0)
                .map(|e| e.id.clone());
            for (id, meta) in metas {
                let marked = is_marked(self.marker.as_deref(), &id);
                self.s.upsert(Entry::new(id, meta, marked));
            }
            self.research(keep);
        }
    }
}

pub fn handle_paste(app: &mut App, s: &str) -> Action {
    app.status.clear();
    app.query.paste(s);
    app.research(None);
    Action::None
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    app.status.clear();
    // Ctrl-D closes as EOF would in a shell prompt.
    if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        && key.modifiers == KeyModifiers::CONTROL
    {
        return Action::Quit;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => return Action::Quit,
        (KeyCode::Enter, _) => {
            return app.selected().map_or(Action::None, |e| {
                Action::Resume(Target {
                    id: e.id.clone(),
                    cwd: e.meta.cwd.clone(),
                    title: e.title(),
                })
            });
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => app.move_cur(-1),
        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => app.move_cur(1),
        (KeyCode::PageUp, _) => app.move_cur(-PAGE),
        (KeyCode::PageDown, _) => app.move_cur(PAGE),
        (KeyCode::Tab, _) => {
            app.scope = app.scope.step(1);
            app.research(None);
        }
        (KeyCode::BackTab, _) => {
            app.scope = app.scope.step(-1);
            app.research(None);
        }
        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
            if app.marker.is_some() {
                app.show = app.show.next();
                app.research(None);
            }
        }
        _ => {
            if app.query.edit(key) {
                app.research(None);
            }
        }
    }
    Action::None
}

// "5m", "3h", "2d", "4w": the largest whole unit.
fn age(now_ms: i64, then_ms: i64) -> String {
    let secs = ((now_ms - then_ms) / 1000).max(0);
    match secs {
        0..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        86_400..604_800 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

// The last two components of a path: enough to tell sibling checkouts apart.
fn tail2(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    parts[parts.len().saturating_sub(2)..].join("/")
}

// The text cut to at most `width` cells, on grapheme boundaries.
fn cut(s: &str, width: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for g in s.graphemes(true) {
        used += g.width();
        if used > width {
            break;
        }
        out.push_str(g);
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let s = cut(s, width);
    let fill = width.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(fill))
}

fn input_line(frame: &mut Frame, input: &Input, area: Rect) {
    let avail = usize::from(area.width.saturating_sub(2));
    let (text, col) = input.view(avail);
    frame.render_widget(Paragraph::new(format!("> {text}")), area);
    let x = area
        .x
        .saturating_add(2)
        .saturating_add(u16::try_from(col).unwrap_or(u16::MAX))
        .min(area.right().saturating_sub(1));
    frame.set_cursor_position((x, area.y));
}

fn row(app: &App, e: &Entry, width: usize) -> String {
    let mark = match (&app.marker, e.marked) {
        (None, _) => "",
        (Some(_), true) => "✓ ",
        (Some(_), false) => "  ",
    };
    let run = if app.running.contains(&e.id) {
        "●"
    } else {
        " "
    };
    let when = age(app.now_ms, e.meta.last_ms.unwrap_or(0));
    let path = pad(&tail2(&launch::clean(&e.meta.cwd)), PATH_WIDTH);
    let head = format!("{mark}{run} {when:>3} {path} ");
    let rest = width.saturating_sub(head.width());
    format!("{head}{}", cut(&launch::clean(&e.title()), rest))
}

pub fn draw(frame: &mut Frame, app: &App) {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let [head, input, body, detail, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(9),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let mut spans = vec![];
    for (i, s) in Scope::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", dim));
        }
        let style = if *s == app.scope {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            dim
        };
        spans.push(Span::styled(s.label(), style));
    }
    spans.push(Span::raw(format!(
        "   {} of {}",
        app.hits.len(),
        app.s.entries.len()
    )));
    if app.marker.is_some() {
        spans.push(Span::raw(format!("   show: {} (Ctrl-T)", app.show.label())));
    }
    if let Some((done, total)) = app.progress {
        spans.push(Span::raw(format!("   indexing {done}/{total}")));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), head);
    input_line(frame, &app.query, input);

    // The list's border and "> " highlight symbol take four columns.
    let width = usize::from(body.width.saturating_sub(4));
    let items: Vec<ListItem> = app
        .hits
        .iter()
        .map(|&i| ListItem::new(row(app, &app.s.entries[i], width)))
        .collect();
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::bordered())
            .highlight_symbol("> ")
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        body,
        &mut ListState::default().with_selected(Some(app.cur)),
    );

    let lines: Vec<Line> = match app.selected() {
        None => vec![Line::from("no session matches")],
        Some(e) => {
            let m = &e.meta;
            let mut lines = vec![
                Line::from(format!("id: {}", launch::clean(&e.id))),
                Line::from(format!("cwd: {}", launch::clean(&m.cwd))),
                Line::from(format!("branch: {}", launch::clean(&m.branch))),
                Line::from(format!(
                    "last activity: {}",
                    m.last_ms.map_or_else(String::new, format_ms)
                )),
                Line::from(format!("prompt: {}", launch::clean(&m.first_prompt))),
            ];
            if let Some((tier, text)) = &app.snippet {
                lines.push(Line::from(format!(
                    "match in {tier}: {}",
                    launch::clean(text)
                )));
            }
            lines
        }
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered()),
        detail,
    );

    let (text, style) = if app.status.is_empty() {
        (String::new(), dim)
    } else {
        (launch::clean(&app.status), Style::new().fg(Color::Red))
    };
    frame.render_widget(Paragraph::new(text).style(style), status);
}

// A subprocess's stderr can be part of the message; its control bytes must not reach the terminal.
// Line breaks are kept so a multi-line stderr stays readable.
fn error_text(msg: &str) -> String {
    let body: String = msg
        .split('\n')
        .map(|l| launch::clean(&l.replace('\t', " ")))
        .collect::<Vec<_>>()
        .join("\n");
    format!("session-resume: {body}\n\nPress Enter (or Esc).")
}

// The popup would vanish with a message printed just before exit, so the error waits for a key.
fn show_error(term: &mut DefaultTerminal, msg: &str) {
    loop {
        let text = error_text(msg);
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

pub type Refreshed = (String, Res<Meta>);

// Reads the sessions that changed since the index was written, one at a time, off the UI thread.
pub struct Background {
    rx: Receiver<Refreshed>,
    total: usize,
    done: usize,
}

impl Background {
    pub fn start(index: Index, stale: Vec<(String, PathBuf)>) -> Option<Background> {
        if stale.is_empty() {
            return None;
        }
        let (tx, rx) = channel();
        let total = stale.len();
        std::thread::spawn(move || {
            for (id, path) in stale {
                let r = index.refresh(&id, &path).map(|(m, _)| m);
                // The popup closing drops the receiver; reading more would be wasted.
                if tx.send((id, r)).is_err() {
                    break;
                }
            }
        });
        Some(Background { rx, total, done: 0 })
    }

    // What arrived since the last call, and the progress to show (None once everything is read).
    pub fn drain(&mut self) -> Arrived {
        let (mut metas, mut errors) = (vec![], vec![]);
        loop {
            match self.rx.try_recv() {
                Ok((id, Ok(m))) => {
                    self.done += 1;
                    metas.push((id, m));
                }
                Ok((_, Err(e))) => {
                    self.done += 1;
                    errors.push(e);
                }
                Err(TryRecvError::Empty) => break,
                // The reader is gone with sessions unread; say so instead of counting forever.
                Err(TryRecvError::Disconnected) => {
                    if self.done < self.total {
                        errors.push(format!(
                            "indexing stopped after {} of {} sessions",
                            self.done, self.total
                        ));
                        self.done = self.total;
                    }
                    break;
                }
            }
        }
        let progress = (self.done < self.total).then_some((self.done, self.total));
        Arrived {
            metas,
            errors,
            progress,
        }
    }
}

pub struct Arrived {
    pub metas: Vec<(String, Meta)>,
    pub errors: Vec<String>,
    pub progress: Option<(usize, usize)>,
}

pub struct Prepared {
    pub app: App,
    pub index: Index,
    pub stale: Vec<(String, PathBuf)>,
    pub resume: String,
}

pub fn load_config(env: &Env) -> Res<Config> {
    crate::config::load(
        &env.config_path,
        &Defaults {
            home: &env.home,
            claude_config_dir: env.claude_config_dir.as_deref(),
        },
    )
}

// Everything that can fail before the screen is needed. Sessions whose index is current are
// listed at once from their metas (stats only); the rest wait for the background refresh.
pub fn prepare(env: &Env, now_ms: i64) -> Res<Prepared> {
    let cfg = load_config(env)?;
    let all = index::transcripts(Path::new(&cfg.projects))?;
    let index = Index::open(Path::new(&env.state_dir))?;
    let mut status = String::new();
    let live: HashSet<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
    if let Err(e) = index.sweep(&live) {
        status = e;
    }
    let mut entries = vec![];
    let mut stale = vec![];
    for (id, path) in all {
        match index.meta(&id) {
            Some(m) if index.is_fresh(&id, &m, &path) => {
                if m.listed() {
                    let marked = is_marked(cfg.marker.as_deref(), &id);
                    entries.push(Entry::new(id, m, marked));
                }
            }
            _ => stale.push((id, path)),
        }
    }
    // Without the running set nothing is marked as running; the failure is shown, not guessed past.
    let running = match launch::running(&env.herdr) {
        Ok(map) => map.into_keys().collect(),
        Err(e) => {
            status = e;
            HashSet::new()
        }
    };
    let s = Searcher::new(index.clone(), entries);
    Ok(Prepared {
        app: App::new(s, &cfg, running, now_ms, status),
        index,
        stale,
        resume: cfg.resume,
    })
}

fn session(env: &Env, prepared: Prepared, term: &mut DefaultTerminal) -> Res<()> {
    let Prepared {
        mut app,
        index,
        stale,
        resume,
    } = prepared;
    let mut bg = Background::start(index, stale);
    if let Some(b) = &bg {
        app.progress = Some((0, b.total));
    }
    loop {
        if let Some(b) = &mut bg {
            let Arrived {
                metas,
                errors,
                progress,
            } = b.drain();
            app.apply(metas, progress);
            if let Some(e) = errors.into_iter().next() {
                app.status = e;
            }
            if progress.is_none() {
                bg = None;
            }
        }
        term.draw(|f| draw(f, &app))
            .map_err(|e| format!("cannot draw the screen: {e}"))?;
        // A timeout lets the progress and the refreshed sessions show without a key press.
        if !event::poll(Duration::from_millis(100))
            .map_err(|e| format!("cannot read the keyboard: {e}"))?
        {
            continue;
        }
        let action = match event::read().map_err(|e| format!("cannot read the keyboard: {e}"))? {
            Event::Key(key) => handle_key(&mut app, key),
            Event::Paste(s) => handle_paste(&mut app, &s),
            _ => continue,
        };
        match action {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::Resume(t) => {
                return launch::resume(&env.herdr, &resume, &t.id, &t.cwd, &t.title)
            }
        }
    }
}

pub fn run(env: &Env) -> Res<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    let prepared = prepare(env, now_ms);
    let mut term = match ratatui::try_init() {
        Ok(t) => t,
        // Without a terminal the problem found while preparing is the one worth reporting.
        Err(e) => {
            return Err(prepared
                .err()
                .unwrap_or_else(|| format!("cannot start the terminal UI: {e}")))
        }
    };
    // Without bracketed paste a pasted query would arrive as key presses, and a pasted newline as Enter.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = prepared.and_then(|p| session(env, p, &mut term));
    if let Err(msg) = &result {
        show_error(&mut term, msg);
    }
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{corpus, fake_herdr, serial, unique_dir, Corpus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    // Times are chosen far apart so every age unit appears: now is 2026-11-10 00:00 UTC.
    const NOW: i64 = 1_794_268_800_000;
    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;

    fn cfg(marker: Option<&str>, show: Show) -> Config {
        Config {
            resume: "claude --resume {id}".into(),
            projects: "/unused".into(),
            marker: marker.map(String::from),
            show,
            scope: Scope::Meta,
        }
    }

    fn app_from(f: Corpus, marker: Option<&str>, running: &[&str]) -> (App, std::path::PathBuf) {
        let Corpus { root, s } = f;
        let app = App::new(
            s,
            &cfg(marker, Show::All),
            running.iter().map(|s| s.to_string()).collect(),
            NOW,
            String::new(),
        );
        (app, root)
    }

    fn specs() -> Vec<crate::testutil::Spec<'static>> {
        vec![
            ("s-min", NOW - 5 * MIN, "Five minutes", "alpha", "", ""),
            (
                "s-hour",
                NOW - 3 * HOUR,
                "Three hours",
                "beta",
                "",
                "toolonly-token",
            ),
            (
                "s-day",
                NOW - 2 * DAY,
                "Two days",
                "gamma",
                "assistant words",
                "",
            ),
            ("s-week", NOW - 28 * DAY, "ビルド修正", "delta", "", ""),
        ]
    }

    fn app() -> (App, std::path::PathBuf) {
        app_from(corpus("ui", &specs()), None, &[])
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(app: &mut App, c: char) -> Action {
        handle_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            assert_eq!(press(app, KeyCode::Char(c)), Action::None);
        }
    }

    fn shown_ids(a: &App) -> Vec<&str> {
        a.hits.iter().map(|&i| a.s.entries[i].id.as_str()).collect()
    }

    fn done(root: std::path::PathBuf) {
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_list_starts_newest_first_with_the_first_row_selected() {
        let (a, root) = app();
        assert_eq!(shown_ids(&a), ["s-min", "s-hour", "s-day", "s-week"]);
        assert_eq!(a.cur, 0);
        done(root);
    }

    #[test]
    fn typing_filters_in_the_current_scope_and_backspacing_restores_the_list() {
        let (mut a, root) = app();
        type_str(&mut a, "hours");
        assert_eq!(shown_ids(&a), ["s-hour"]);
        for _ in 0..5 {
            press(&mut a, KeyCode::Backspace);
        }
        assert_eq!(a.hits.len(), 4);
        done(root);
    }

    #[test]
    fn tab_widens_the_scope_and_shift_tab_narrows_it_and_both_wrap() {
        let (mut a, root) = app();
        type_str(&mut a, "toolonly");
        assert!(a.hits.is_empty());
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.scope, Scope::User);
        assert!(a.hits.is_empty());
        press(&mut a, KeyCode::Tab);
        assert!(a.hits.is_empty());
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.scope, Scope::Tools);
        assert_eq!(shown_ids(&a), ["s-hour"]);
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.scope, Scope::Meta);
        assert!(a.hits.is_empty());
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.scope, Scope::Tools);
        assert_eq!(shown_ids(&a), ["s-hour"]);
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.scope, Scope::Assistant);
        done(root);
    }

    #[test]
    fn up_down_ctrl_p_n_and_pages_stop_at_the_ends() {
        let (mut a, root) = app();
        press(&mut a, KeyCode::Up);
        assert_eq!(a.cur, 0);
        press(&mut a, KeyCode::Down);
        ctrl(&mut a, 'n');
        assert_eq!(a.cur, 2);
        ctrl(&mut a, 'p');
        assert_eq!(a.cur, 1);
        press(&mut a, KeyCode::PageDown);
        assert_eq!(a.cur, 3);
        press(&mut a, KeyCode::PageUp);
        assert_eq!(a.cur, 0);
        done(root);
    }

    #[test]
    fn enter_yields_the_selected_session_and_nothing_when_nothing_matches() {
        let (mut a, root) = app();
        press(&mut a, KeyCode::Down);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            Action::Resume(Target {
                id: "s-hour".into(),
                cwd: "/w/s-hour".into(),
                title: "Three hours".into(),
            })
        );
        type_str(&mut a, "zzzz");
        assert_eq!(press(&mut a, KeyCode::Enter), Action::None);
        done(root);
    }

    #[test]
    fn esc_ctrl_c_and_ctrl_d_close() {
        let (mut a, root) = app();
        assert_eq!(press(&mut a, KeyCode::Esc), Action::Quit);
        assert_eq!(ctrl(&mut a, 'c'), Action::Quit);
        assert_eq!(ctrl(&mut a, 'd'), Action::Quit);
        done(root);
    }

    #[test]
    fn a_key_release_does_nothing() {
        let (mut a, root) = app();
        let rel =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(handle_key(&mut a, rel), Action::None);
        done(root);
    }

    #[test]
    fn a_paste_goes_into_the_query_as_one_line() {
        let (mut a, root) = app();
        handle_paste(&mut a, "assistant\nwords");
        assert_eq!(a.query.text, "assistant words");
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Tab);
        assert_eq!(shown_ids(&a), ["s-day"]);
        done(root);
    }

    #[test]
    fn editing_the_query_goes_back_to_the_first_row() {
        let (mut a, root) = app();
        press(&mut a, KeyCode::PageDown);
        assert_eq!(a.cur, 3);
        type_str(&mut a, "t");
        assert_eq!(a.cur, 0);
        done(root);
    }

    #[test]
    fn ctrl_t_without_a_marker_does_nothing() {
        let (mut a, root) = app();
        ctrl(&mut a, 't');
        assert_eq!((a.show, a.hits.len()), (Show::All, 4));
        done(root);
    }

    fn marked_app(name: &str) -> (App, std::path::PathBuf) {
        let mut f = corpus(name, &specs());
        for e in &mut f.s.entries {
            e.marked = e.id == "s-hour" || e.id == "s-week";
        }
        app_from(f, Some("/m/{id}/done"), &[])
    }

    #[test]
    fn ctrl_t_cycles_all_unmarked_marked_and_filters_the_hits() {
        let (mut a, root) = marked_app("marked");
        ctrl(&mut a, 't');
        assert_eq!(a.show, Show::Unmarked);
        assert_eq!(shown_ids(&a), ["s-min", "s-day"]);
        ctrl(&mut a, 't');
        assert_eq!(a.show, Show::Marked);
        assert_eq!(shown_ids(&a), ["s-hour", "s-week"]);
        ctrl(&mut a, 't');
        assert_eq!(a.show, Show::All);
        assert_eq!(a.hits.len(), 4);
        done(root);
    }

    #[test]
    fn the_marker_filter_combines_with_the_query() {
        let (mut a, root) = marked_app("marked-q");
        ctrl(&mut a, 't');
        type_str(&mut a, "hour");
        assert!(a.hits.is_empty());
        ctrl(&mut a, 't');
        assert_eq!(shown_ids(&a), ["s-hour"]);
        done(root);
    }

    #[test]
    fn the_configured_show_is_the_starting_filter() {
        let mut f = corpus("show", &specs());
        f.s.entries[2].marked = true;
        let Corpus { root, s } = f;
        let c = cfg(Some("/m/{id}"), Show::Marked);
        let a = App::new(s, &c, HashSet::new(), NOW, String::new());
        assert_eq!(shown_ids(&a), ["s-day"]);
        done(root);
    }

    #[test]
    fn a_marker_path_is_marked_only_when_the_file_for_that_id_exists() {
        let dir = unique_dir("marker");
        std::fs::create_dir(dir.join("s1")).unwrap();
        std::fs::write(dir.join("s1/archived"), "").unwrap();
        let tpl = format!("{}/{{id}}/archived", dir.display());
        assert!(is_marked(Some(&tpl), "s1"));
        assert!(!is_marked(Some(&tpl), "s2"));
        assert!(!is_marked(None, "s1"));
        done(dir);
    }

    #[test]
    fn age_is_the_largest_whole_unit() {
        let t = 1_000_000_000_000;
        let a = |ms: i64| age(t + ms, t);
        assert_eq!(a(5 * MIN + 59_000), "5m");
        assert_eq!(a(0), "0m");
        assert_eq!(a(59 * MIN), "59m");
        assert_eq!(a(HOUR), "1h");
        assert_eq!(a(23 * HOUR + 59 * MIN), "23h");
        assert_eq!(a(DAY), "1d");
        assert_eq!(a(6 * DAY), "6d");
        assert_eq!(a(7 * DAY), "1w");
        assert_eq!(a(28 * DAY), "4w");
        assert_eq!(age(t, t + HOUR), "0m");
    }

    #[test]
    fn tail2_keeps_the_last_two_path_components() {
        assert_eq!(tail2("/home/u/work/repo"), "work/repo");
        assert_eq!(tail2("/repo"), "repo");
        assert_eq!(tail2("/"), "");
        assert_eq!(tail2("/a/b/"), "a/b");
    }

    fn screen(app: &App, width: u16) -> (Vec<String>, Terminal<TestBackend>) {
        let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, app))
            .unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let lines = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        (lines, term)
    }

    #[test]
    fn a_row_shows_age_the_last_two_path_components_and_the_title() {
        let (a, root) = app();
        let (lines, _) = screen(&a, 70);
        let text = lines.join("\n");
        assert!(lines[3].starts_with("│> "), "{text}");
        assert!(lines[3].contains(" 5m w/s-min "), "{text}");
        assert!(lines[3].contains("Five minutes"), "{text}");
        assert!(lines[4].contains(" 3h w/s-hour "), "{text}");
        assert!(lines[5].contains(" 2d w/s-day "), "{text}");
        assert!(lines[6].contains(" 4w w/s-week "), "{text}");
        // A wide character fills two cells; the second shows as a space.
        assert!(lines[6].replace(' ', "").contains("ビルド修正"), "{text}");
        done(root);
    }

    #[test]
    fn the_header_shows_four_scopes_with_the_current_one_highlighted_and_the_counts() {
        let (mut a, root) = app();
        press(&mut a, KeyCode::Tab);
        let (lines, term) = screen(&a, 80);
        assert!(
            lines[0].starts_with("meta | +user | +assistant | +tools   4 of 4"),
            "{}",
            lines[0]
        );
        let buf = term.backend().buffer();
        let x_user = u16::try_from(lines[0].find("+user").unwrap()).unwrap();
        let x_meta = 0;
        assert!(buf[(x_user, 0)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(x_meta, 0)].modifier.contains(Modifier::REVERSED));
        assert!(!lines[0].contains("show:") && !lines[0].contains("indexing"));
        done(root);
    }

    #[test]
    fn the_marker_column_and_the_filter_state_show_only_when_a_marker_is_configured() {
        let (a, root) = marked_app("marker-draw");
        let (lines, _) = screen(&a, 80);
        assert!(lines[0].contains("show: all (Ctrl-T)"), "{}", lines[0]);
        assert!(lines[4].contains("✓"), "{}", lines[4]);
        assert!(lines[6].contains("✓"), "{}", lines[6]);
        assert!(!lines[3].contains("✓") && !lines[5].contains("✓"));
        let (plain, root2) = app();
        let (lines, _) = screen(&plain, 80);
        assert!(!lines.join("\n").contains('✓'));
        done(root);
        done(root2);
    }

    #[test]
    fn a_running_session_gets_a_dot_and_the_others_do_not() {
        let (a, root) = app_from(corpus("run", &specs()), None, &["s-day"]);
        let (lines, _) = screen(&a, 70);
        assert!(lines[5].contains("● "), "{}", lines[5]);
        assert_eq!(lines[3..7].iter().filter(|l| l.contains('●')).count(), 1);
        done(root);
    }

    #[test]
    fn progress_shows_while_the_background_refresh_runs_and_goes_when_it_ends() {
        let (mut a, root) = app();
        a.apply(vec![], Some((12, 40)));
        let (lines, _) = screen(&a, 80);
        assert!(lines[0].contains("indexing 12/40"), "{}", lines[0]);
        a.apply(vec![], None);
        let (lines, _) = screen(&a, 80);
        assert!(!lines[0].contains("indexing"));
        done(root);
    }

    #[test]
    fn the_detail_shows_the_session_and_the_snippet_names_the_tier() {
        let (mut a, root) = app();
        type_str(&mut a, "assistant words");
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Tab);
        let (lines, _) = screen(&a, 80);
        let text = lines.join("\n");
        assert!(text.contains("id: s-day"), "{text}");
        assert!(text.contains("cwd: /w/s-day"), "{text}");
        assert!(
            text.contains("last activity: 2026-11-08 00:00 UTC"),
            "{text}"
        );
        assert!(
            text.contains("match in assistant: assistant words"),
            "{text}"
        );
        done(root);
    }

    #[test]
    fn no_match_says_so_and_a_status_shows_in_the_last_line_until_a_key() {
        let (mut a, root) = app();
        a.status = "herdr pane list failed".into();
        type_str(&mut a, "zzzz");
        assert_eq!(a.status, "");
        let (lines, _) = screen(&a, 60);
        assert!(lines.join("\n").contains("no session matches"));
        a.status = "boom".into();
        let (lines, _) = screen(&a, 60);
        assert!(lines[23].starts_with("boom"), "{lines:?}");
        done(root);
    }

    #[test]
    fn control_characters_from_a_transcript_are_not_drawn() {
        let mut f = corpus("ctl-draw", &specs());
        f.s.upsert(Entry::new(
            "s-evil".into(),
            Meta {
                custom_title: "bad\u{1b}[31mtitle".into(),
                first_prompt: "p\u{7}rompt".into(),
                cwd: "/w/ev\u{1b}il".into(),
                branch: "br\u{1b}".into(),
                last_ms: Some(NOW),
                ..Meta::default()
            },
            false,
        ));
        let (mut a, root) = app_from(f, None, &[]);
        a.status = "err \u{1b}[0m".into();
        let (lines, _) = screen(&a, 80);
        let all = lines.join("\n");
        assert!(!all.chars().any(|c| c.is_control() && c != '\n'), "{all:?}");
        assert!(all.contains("bad [31mtitle"), "{all}");
        assert!(all.contains("prompt: p rompt"), "{all}");
        assert!(all.contains("cwd: /w/ev il"), "{all}");
        done(root);
    }

    #[test]
    fn the_error_screen_does_not_draw_control_characters() {
        let t = error_text("pane run failed: \u{1b}[31mboom\nsecond\tline");
        assert!(!t.contains('\u{1b}') && t.contains(" [31mboom"), "{t}");
        assert!(t.contains("boom\nsecond line"), "{t}");
        assert!(t.starts_with("session-resume: "), "{t}");
    }

    #[test]
    fn a_wide_title_is_cut_by_display_width_not_by_characters() {
        assert_eq!(cut("ビルド修正", 7), "ビルド");
        assert_eq!(cut("ab", 7), "ab");
        assert_eq!(pad("あ", 4), "あ  ");
        assert_eq!(pad("abcdef", 3), "abc");
    }

    #[test]
    fn refreshed_sessions_join_the_list_in_order_and_the_selection_stays_on_its_session() {
        let (mut a, root) = app();
        press(&mut a, KeyCode::Down);
        assert_eq!(a.selected().unwrap().id, "s-hour");
        let newest = Meta {
            custom_title: "Brand new".into(),
            last_ms: Some(NOW),
            ..Meta::default()
        };
        a.apply(vec![("s-new".into(), newest)], Some((1, 2)));
        assert_eq!(
            shown_ids(&a),
            ["s-new", "s-min", "s-hour", "s-day", "s-week"]
        );
        assert_eq!(a.selected().unwrap().id, "s-hour");
        assert_eq!(a.progress, Some((1, 2)));
        done(root);
    }

    #[test]
    fn a_cursor_left_on_the_first_row_stays_there_when_a_newer_session_arrives() {
        let (mut a, root) = app();
        assert_eq!(a.selected().unwrap().id, "s-min");
        let newest = Meta {
            custom_title: "Brand new".into(),
            last_ms: Some(NOW),
            ..Meta::default()
        };
        a.apply(vec![("s-new".into(), newest)], None);
        assert_eq!(a.selected().unwrap().id, "s-new");
        done(root);
    }

    #[test]
    fn a_refreshed_session_is_marked_when_its_marker_file_exists() {
        let dir = unique_dir("apply-marker");
        std::fs::write(dir.join("s-fresh.done"), "").unwrap();
        let tpl = format!("{}/{{id}}.done", dir.display());
        let (mut a, root) = app_from(corpus("apply-marker-app", &specs()), Some(&tpl), &[]);
        let meta = |ms| Meta {
            last_ms: Some(ms),
            ..Meta::default()
        };
        a.apply(
            vec![
                ("s-fresh".into(), meta(NOW)),
                ("s-plain".into(), meta(NOW - 1)),
            ],
            None,
        );
        ctrl(&mut a, 't');
        ctrl(&mut a, 't');
        assert_eq!(a.show, Show::Marked);
        assert_eq!(shown_ids(&a), ["s-fresh"]);
        done(root);
        done(dir);
    }

    #[test]
    fn a_refreshed_session_that_no_longer_matches_drops_the_selection_to_the_first_row() {
        let (mut a, root) = app();
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        assert_eq!((a.cur, a.selected().unwrap().id.as_str()), (2, "s-day"));
        // No user or assistant line any more: the session leaves the list.
        let gone = Meta::default();
        a.apply(vec![("s-day".into(), gone)], None);
        assert_eq!(shown_ids(&a), ["s-min", "s-hour", "s-week"]);
        assert_eq!((a.cur, a.selected().unwrap().id.as_str()), (0, "s-min"));
        done(root);
    }

    #[test]
    fn a_background_reader_that_died_ends_the_progress_and_says_how_far_it_got() {
        let (tx, rx) = channel::<Refreshed>();
        let meta = Meta {
            last_ms: Some(NOW),
            ..Meta::default()
        };
        tx.send(("a".into(), Ok(meta))).unwrap();
        drop(tx);
        let mut bg = Background {
            rx,
            total: 3,
            done: 0,
        };
        let got = bg.drain();
        assert_eq!(got.metas.len(), 1);
        assert_eq!(got.errors, ["indexing stopped after 1 of 3 sessions"]);
        assert_eq!(got.progress, None);
        assert!(bg.drain().errors.is_empty());
    }

    // The Env of a popup whose projects, state and config live in `dir`, with a herdr that
    // answers `pane list` with `list`.
    fn env_in(dir: &Path, herdr: &str) -> Env {
        Env {
            herdr: herdr.into(),
            config_path: dir.join("cfg/config.json").to_str().unwrap().into(),
            plugin_id: "takashiaihara.session-resume".into(),
            state_dir: dir.join("state").to_str().unwrap().into(),
            home: dir.join("home").to_str().unwrap().into(),
            claude_config_dir: Some(dir.join("claude").to_str().unwrap().into()),
        }
    }

    fn user_line(ts: &str, text: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"{ts}\",\"cwd\":\"/w/x\",\"message\":{{\"content\":\"{text}\"}}}}\n"
        )
    }

    const EMPTY_PANES: &str = r#"{"result":{"panes":[]}}"#;

    #[test]
    fn prepare_lists_current_sessions_from_cached_metas_and_defers_the_changed_ones() {
        let _g = serial();
        let dir = unique_dir("prepare");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("s-cur.jsonl"),
            user_line("2026-11-04T10:00:00Z", "cur"),
        )
        .unwrap();
        std::fs::write(
            proj.join("s-chg.jsonl"),
            user_line("2026-11-04T11:00:00Z", "chg"),
        )
        .unwrap();
        let f = fake_herdr(&[("pane list", &format!("printf '%s' '{EMPTY_PANES}'"))]);
        let env = env_in(&dir, &f.bin);
        let index = Index::open(Path::new(&env.state_dir)).unwrap();
        index.refresh("s-cur", &proj.join("s-cur.jsonl")).unwrap();
        index.refresh("s-chg", &proj.join("s-chg.jsonl")).unwrap();
        let mut more = std::fs::OpenOptions::new()
            .append(true)
            .open(proj.join("s-chg.jsonl"))
            .unwrap();
        std::io::Write::write_all(
            &mut more,
            user_line("2026-11-04T12:00:00Z", "more").as_bytes(),
        )
        .unwrap();
        std::fs::write(
            proj.join("s-new.jsonl"),
            user_line("2026-11-04T09:00:00Z", "new"),
        )
        .unwrap();

        let p = prepare(&env, NOW).unwrap();
        let listed: Vec<_> = p.app.s.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(listed, ["s-cur"]);
        let mut stale: Vec<_> = p.stale.iter().map(|(id, _)| id.as_str()).collect();
        stale.sort();
        assert_eq!(stale, ["s-chg", "s-new"]);
        assert_eq!(p.app.status, "");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prepare_removes_the_cache_of_a_session_whose_transcript_is_gone() {
        let _g = serial();
        let dir = unique_dir("prepare-sweep");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("s-gone.jsonl"),
            user_line("2026-11-04T10:00:00Z", "g"),
        )
        .unwrap();
        let f = fake_herdr(&[("pane list", &format!("printf '%s' '{EMPTY_PANES}'"))]);
        let env = env_in(&dir, &f.bin);
        let index = Index::open(Path::new(&env.state_dir)).unwrap();
        index.refresh("s-gone", &proj.join("s-gone.jsonl")).unwrap();
        std::fs::remove_file(proj.join("s-gone.jsonl")).unwrap();
        prepare(&env, NOW).unwrap();
        assert_eq!(index.meta("s-gone"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prepare_marks_the_sessions_herdr_reports_running() {
        let _g = serial();
        let dir = unique_dir("prepare-run");
        let proj = dir.join("claude/projects/-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        let tr = proj.join("s-run.jsonl");
        std::fs::write(&tr, user_line("2026-11-04T10:00:00Z", "r")).unwrap();
        let f = fake_herdr(&[(
            "pane list",
            r#"printf '%s' '{"result":{"panes":[{"pane_id":"p1","agent_session":{"value":"s-run"}}]}}'"#,
        )]);
        let env = env_in(&dir, &f.bin);
        Index::open(Path::new(&env.state_dir))
            .unwrap()
            .refresh("s-run", &tr)
            .unwrap();
        let p = prepare(&env, NOW).unwrap();
        assert!(p.app.running.contains("s-run"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failing_pane_list_is_a_status_and_marks_nothing_running() {
        let _g = serial();
        let dir = unique_dir("prepare-nolist");
        std::fs::create_dir_all(dir.join("claude/projects")).unwrap();
        let f = fake_herdr(&[("pane list", "echo broken >&2; exit 1")]);
        let p = prepare(&env_in(&dir, &f.bin), NOW).unwrap();
        assert!(
            p.app.status.contains("pane list") && p.app.status.contains("broken"),
            "{}",
            p.app.status
        );
        assert!(p.app.running.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prepare_refuses_an_invalid_config_and_a_missing_projects_directory_naming_the_problem() {
        let _g = serial();
        let dir = unique_dir("prepare-bad");
        let f = fake_herdr(&[]);
        let env = env_in(&dir, &f.bin);
        let err = prepare(&env, NOW).err().unwrap();
        assert!(err.contains("cannot read the projects directory"), "{err}");
        std::fs::create_dir_all(dir.join("cfg")).unwrap();
        std::fs::write(dir.join("cfg/config.json"), r#"{"agents":[]}"#).unwrap();
        let err = prepare(&env, NOW).err().unwrap();
        assert!(err.contains(r#"unknown key "agents""#), "{err}");
        assert!(f.calls().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_background_refresh_reads_each_session_and_reports_progress_to_the_end() {
        let dir = unique_dir("bg");
        let index = Index::open(&dir).unwrap();
        let mut stale = vec![];
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            let p = dir.join(format!("{id}.jsonl"));
            std::fs::write(&p, user_line("2026-11-04T10:00:00Z", &format!("t{i}"))).unwrap();
            stale.push((id.to_string(), p));
        }
        stale.push(("gone".to_string(), dir.join("gone.jsonl")));
        let mut bg = Background::start(index, stale).unwrap();
        let (mut metas, mut errors) = (vec![], vec![]);
        let mut last = Some((0, 4));
        for _ in 0..500 {
            let Arrived {
                metas: m,
                errors: e,
                progress: p,
            } = bg.drain();
            metas.extend(m);
            errors.extend(e);
            last = p;
            if last.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(last, None);
        let mut ids: Vec<_> = metas.iter().map(|(id, _)| id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["a", "b", "c"]);
        assert!(metas.iter().all(|(_, m)| m.listed() && m.cwd == "/w/x"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("gone.jsonl"), "{errors:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_to_refresh_starts_no_background_work() {
        let dir = unique_dir("bg-none");
        let index = Index::open(&dir).unwrap();
        assert!(Background::start(index, vec![]).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
