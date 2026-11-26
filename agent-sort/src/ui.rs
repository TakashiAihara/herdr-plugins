use crate::config::Preset;
use crate::keys::{self, Bind, KeySet};
use crate::state;
use crate::{current, edit_flow, notify_edit, Env, Res};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Up,
    Down,
    Select,
    Edit,
    Clear,
    Quit,
}

#[derive(PartialEq, Debug)]
pub enum Action {
    None,
    Apply(usize),
    Clear,
    Edit,
    Quit,
}

pub struct Binds(Vec<(Bind, Op)>);

impl Binds {
    pub fn new(sets: &[KeySet]) -> Binds {
        let mut list = Vec::new();
        for set in sets {
            let op = match set.action {
                "up" => Op::Up,
                "down" => Op::Down,
                "select" => Op::Select,
                "edit" => Op::Edit,
                "clear" => Op::Clear,
                "quit" => Op::Quit,
                _ => continue,
            };
            for bind in &set.binds {
                list.push((*bind, op));
            }
        }
        Binds(list)
    }
}

pub struct App {
    pub presets: Vec<Preset>,
    pub current: Option<String>,
    pub cur: usize,
    // Kept between frames so a list longer than the screen does not lose its scroll position.
    pub list: ListState,
    // The last failure or edit result, shown under the help line until the next action.
    pub status: String,
    pub help: String,
}

impl App {
    pub fn new(presets: Vec<Preset>, current: Option<String>, status: String, help: String) -> App {
        let cur = current
            .as_ref()
            .and_then(|n| presets.iter().position(|p| &p.name == n))
            .unwrap_or(0);
        let mut list = ListState::default();
        list.select((!presets.is_empty()).then_some(cur));
        App {
            presets,
            current,
            cur,
            list,
            status,
            help,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.presets.is_empty() {
            return;
        }
        let last = self.presets.len() - 1;
        self.cur = (self.cur as isize + delta).clamp(0, last as isize) as usize;
        self.list.select(Some(self.cur));
    }
}

fn help_line(sets: &[KeySet]) -> String {
    let names = |action: &str| {
        sets.iter()
            .find(|s| s.action == action)
            .map(|s| s.names.join("/"))
            .unwrap_or_default()
    };
    format!(
        "{}/{} move   {} select   {} edit   {} clear   {} quit",
        names("up"),
        names("down"),
        names("select"),
        names("edit"),
        names("clear"),
        names("quit")
    )
}

struct Loaded {
    app: App,
    binds: Binds,
    // The config error when it did not load (the message already carries the path); an edit
    // reload joins it back into the status line so an editor failure does not hide it.
    cfg_err: Option<String>,
    // The [keys] resolution error, with the config path, when the binds are all defaults; joined
    // back into the status line the same way.
    key_err: Option<String>,
}

// The resolved key sets, or all defaults when the [keys] table cannot be used; the error carries
// the config path, goes to the status line, and the popup keeps working.
fn key_sets(path: &str, cfg: &crate::config::Config) -> (Vec<KeySet>, Option<String>) {
    match keys::resolve(cfg.keys_raw()) {
        Ok(sets) => (sets, None),
        Err(e) => (
            keys::resolve(None).unwrap_or_default(),
            Some(format!("{path}: {e}")),
        ),
    }
}

// Builds the popup state from the config and state files. A config that does not parse leaves an
// empty list with the error and an edit hint; only edit and quit still work there.
fn load(env: &Env) -> Loaded {
    let cfg = match env.config() {
        Ok(cfg) => cfg,
        Err(e) => {
            let sets = keys::resolve(None).unwrap_or_default();
            return Loaded {
                app: App::new(
                    vec![],
                    None,
                    format!("{e} (press e to edit)"),
                    help_line(&sets),
                ),
                binds: Binds::new(&sets),
                cfg_err: Some(e),
                key_err: None,
            };
        }
    };
    let (sets, key_err) = key_sets(&env.config_path, &cfg);
    let status = key_err.clone().unwrap_or_default();
    let current = current(&cfg, &state::read(&env.state_path)).map(String::from);
    Loaded {
        binds: Binds::new(&sets),
        app: App::new(cfg.presets, current, status, help_line(&sets)),
        cfg_err: None,
        key_err,
    }
}

pub fn handle_key(app: &mut App, binds: &Binds, key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    let pressed = keys::normalize(key.code, key.modifiers);
    let Some((_, op)) = binds.0.iter().find(|(b, _)| *b == pressed) else {
        return Action::None;
    };
    match op {
        // On an empty list a move changes nothing, so the status line stays.
        Op::Up if app.presets.is_empty() => Action::None,
        Op::Down if app.presets.is_empty() => Action::None,
        Op::Up => {
            app.move_cursor(-1);
            app.status.clear();
            Action::None
        }
        Op::Down => {
            app.move_cursor(1);
            app.status.clear();
            Action::None
        }
        // From a broken config there is nothing to apply or clear; only edit and quit work. A
        // bound key that does nothing leaves the status line alone.
        Op::Select if app.presets.is_empty() => Action::None,
        Op::Clear if app.presets.is_empty() => Action::None,
        Op::Select => {
            app.status.clear();
            Action::Apply(app.cur)
        }
        Op::Clear => {
            app.status.clear();
            Action::Clear
        }
        Op::Edit => {
            app.status.clear();
            Action::Edit
        }
        Op::Quit => {
            app.status.clear();
            Action::Quit
        }
    }
}

// Rows the status needs once Paragraph::wrap folds it to `width` columns, so a long config error
// and its edit hint stay readable instead of truncating. Words are packed greedily; a word longer
// than the width is broken across lines the way the widget breaks it. An empty status takes no
// row at all.
fn wrapped_height(text: &str, width: usize) -> usize {
    if width == 0 || text.is_empty() {
        return 0;
    }
    let mut lines = 0;
    for logical in text.split('\n') {
        let mut col = 0usize;
        for word in logical.split_whitespace() {
            let w = ratatui::text::Line::from(word).width();
            if col > 0 && col + 1 + w <= width {
                col += 1 + w;
            } else {
                if col > 0 {
                    lines += 1;
                }
                if w <= width {
                    col = w;
                } else {
                    lines += w / width;
                    col = w % width;
                }
            }
        }
        lines += 1;
    }
    lines
}

fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // The list needs its row plus the description and help lines; the status takes the rest, up
    // to what the wrapped text needs.
    let status_rows = wrapped_height(&app.status, area.width as usize)
        .min(area.height.saturating_sub(3) as usize);
    let [list_area, desc_area, help_area, status_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(status_rows as u16),
    ])
    .areas(area);

    let items: Vec<ListItem> = app
        .presets
        .iter()
        .map(|p| {
            let mut text = p.name.clone();
            if app.current.as_deref() == Some(p.name.as_str()) {
                text.push_str(" (current)");
            }
            ListItem::new(text)
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title("agent sort"))
        .highlight_symbol("> ")
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, list_area, &mut app.list);

    let desc = app
        .presets
        .get(app.cur)
        .map_or(String::new(), |p| format!("sort: {}", p.describe()));
    frame.render_widget(
        Paragraph::new(desc).style(Style::new().add_modifier(Modifier::DIM)),
        desc_area,
    );
    frame.render_widget(
        Paragraph::new(app.help.as_str()).style(Style::new().add_modifier(Modifier::DIM)),
        help_area,
    );
    frame.render_widget(
        Paragraph::new(app.status.as_str())
            .style(Style::new().fg(Color::Red))
            .wrap(Wrap { trim: false }),
        status_area,
    );
}

fn show_error(term: &mut DefaultTerminal, msg: &str) {
    loop {
        let text = format!("agent-sort: {msg}\n\nPress Enter.");
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

fn session(env: &Env, term: &mut DefaultTerminal) -> Res<()> {
    let mut loaded = load(env);
    loop {
        term.draw(|f| draw(f, &mut loaded.app))
            .map_err(|e| format!("cannot draw the screen: {e}"))?;
        let Event::Key(key) =
            event::read().map_err(|e| format!("cannot read the keyboard: {e}"))?
        else {
            continue;
        };
        match handle_key(&mut loaded.app, &loaded.binds, key) {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::Apply(i) => {
                let preset = loaded.app.presets[i].clone();
                match crate::apply(env, &preset) {
                    Ok(()) => return Ok(()),
                    Err(e) => loaded.app.status = e,
                }
            }
            Action::Clear => match crate::clear_view(env) {
                Ok(outcome) => {
                    env.notify_ok(&crate::clear_message(&outcome));
                    match outcome {
                        crate::ClearOutcome::Cleared => return Ok(()),
                        crate::ClearOutcome::Kept(_) => {
                            loaded.app.status = crate::clear_message(&outcome);
                            // The state is null now: nothing of ours is applied anymore.
                            loaded.app.current = None;
                        }
                    }
                }
                Err(e) => {
                    env.notify_error(&e);
                    loaded.app.status = e;
                }
            },
            Action::Edit => {
                // The editor needs the real terminal: leave the alternate screen, run the same
                // flow as `edit`, then come back and reload.
                ratatui::restore();
                let result = edit_flow(env);
                *term = ratatui::try_init()
                    .map_err(|e| format!("cannot restart the terminal UI: {e}"))?;
                let status = match &result {
                    Ok(r) => notify_edit(env, r),
                    Err(e) => {
                        env.notify_error(e);
                        e.clone()
                    }
                };
                loaded = load(env);
                // The edit result joins what is still wrong: a config error that survived the
                // edit (skipped when it is already the result) plus its hint, and a key error.
                let mut parts = vec![status];
                if let Some(e) = loaded.cfg_err.clone() {
                    if e != parts[0] {
                        parts.push(e);
                    }
                    parts.push("press e to edit".into());
                }
                if let Some(e) = loaded.key_err.clone() {
                    parts.push(format!("keys: {e}"));
                }
                loaded.app.status = parts.join("; ");
            }
        }
    }
}

pub fn run(env: &Env) -> Res<()> {
    let mut term = ratatui::try_init().map_err(|e| format!("cannot start the terminal UI: {e}"))?;
    let result = session(env, &mut term);
    if let Err(msg) = &result {
        show_error(&mut term, msg);
    }
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SortEntry, SortKey};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn preset(name: &str, desc: bool) -> Preset {
        Preset {
            name: name.into(),
            sort: vec![SortEntry {
                key: SortKey::Field("attention".into()),
                desc,
            }],
            filter: None,
        }
    }

    fn app() -> App {
        App::new(
            vec![
                preset("attention", true),
                preset("recent", true),
                preset("workspace", false),
            ],
            Some("recent".into()),
            String::new(),
            help_line(&keys::resolve(None).unwrap_or_else(|e| panic!("{e}"))),
        )
    }

    fn binds() -> Binds {
        Binds::new(&keys::resolve(None).unwrap_or_else(|e| panic!("{e}")))
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn the_cursor_starts_on_the_current_preset_and_moves_within_the_list() {
        let mut app = app();
        assert_eq!(app.cur, 1, "cursor on current \"recent\"");
        assert_eq!(app.list.selected(), Some(1));
        let binds = binds();
        handle_key(&mut app, &binds, key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.cur, 2);
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert_eq!(app.cur, 2, "the cursor stays at the last row");
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        assert_eq!(app.cur, 1);
        handle_key(&mut app, &binds, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.cur, 0);
        handle_key(&mut app, &binds, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.cur, 0);
        assert_eq!(
            app.list.selected(),
            Some(0),
            "the list state tracks the cursor"
        );
    }

    #[test]
    fn the_cursor_falls_back_to_the_first_row_when_the_current_preset_is_gone() {
        let app = App::new(
            vec![preset("a", false), preset("b", false)],
            Some("gone".into()),
            String::new(),
            String::new(),
        );
        assert_eq!(app.cur, 0);
        assert_eq!(app.list.selected(), Some(0));
    }

    #[test]
    fn a_key_that_does_nothing_keeps_the_status_an_action_clears_it() {
        let mut app = app();
        app.status = "boom".into();
        let binds = binds();
        handle_key(
            &mut app,
            &binds,
            key(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.status, "boom", "an unbound key keeps the status");
        handle_key(&mut app, &binds, key(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.status.is_empty(), "a move clears the status");

        let mut empty = App::new(vec![], None, "bad config".into(), String::new());
        handle_key(&mut empty, &binds, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            empty.status, "bad config",
            "select on an empty list does nothing and keeps the status"
        );
    }

    #[test]
    fn select_clear_quit_and_edit_dispatch_through_the_binds() {
        let binds = binds();
        let mut app = app();
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Apply(1)
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('0'), KeyModifiers::NONE)
            ),
            Action::Clear
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('e'), KeyModifiers::NONE)
            ),
            Action::Edit
        );
        for code in [
            key(KeyCode::Esc, KeyModifiers::NONE),
            key(KeyCode::Char('q'), KeyModifiers::NONE),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(handle_key(&mut app, &binds, code), Action::Quit);
        }
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('x'), KeyModifiers::NONE)
            ),
            Action::None,
            "an unbound key does nothing"
        );
    }

    #[test]
    fn a_broken_config_list_dispatches_only_edit_and_quit() {
        let sets = keys::resolve(None).unwrap_or_else(|e| panic!("{e}"));
        let binds = Binds::new(&sets);
        let mut app = App::new(
            vec![],
            None,
            "bad config (press e to edit)".into(),
            String::new(),
        );
        for (code, mods) in [
            (KeyCode::Enter, KeyModifiers::NONE),
            (KeyCode::Char('0'), KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ] {
            assert_eq!(handle_key(&mut app, &binds, key(code, mods)), Action::None);
        }
        assert_eq!(
            app.status, "bad config (press e to edit)",
            "keys that do nothing on an empty list keep the status"
        );
        assert_eq!(
            handle_key(
                &mut app,
                &binds,
                key(KeyCode::Char('e'), KeyModifiers::NONE)
            ),
            Action::Edit
        );
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Quit
        );
    }

    #[test]
    fn the_screen_shows_the_list_preview_help_and_status() {
        let mut app = app();
        app.status = "cannot apply".into();
        let help = app.help.clone();
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, &mut app))
            .unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let screen: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let all = screen.join("\n");
        assert!(all.contains("attention"), "{all}");
        assert!(all.contains("recent (current)"), "{all}");
        assert!(all.contains("sort: attention desc"), "{all}");
        assert!(all.contains(&help), "{all}");
        assert!(all.contains("cannot apply"), "{all}");
    }

    #[test]
    fn the_help_line_names_the_default_binds() {
        let sets = keys::resolve(None).unwrap_or_else(|e| panic!("{e}"));
        let help = help_line(&sets);
        // Each group is "key/key/... action"; compare whole groups so a key name hidden in a
        // label cannot pass.
        let groups: Vec<&str> = help.split("   ").collect();
        for group in [
            "up/k/down/j move",
            "enter select",
            "e edit",
            "0 clear",
            "esc/q/ctrl-c quit",
        ] {
            assert!(groups.contains(&group), "{group} not in {help}");
        }
        assert_eq!(app().help, help);
    }

    #[test]
    fn a_long_status_wraps_over_as_many_rows_as_it_needs() {
        let mut app = app();
        app.status =
            "/cfg/config.toml: unknown key \"bogus\" in keys.edit (press e to edit)".into();
        let mut term = Terminal::new(TestBackend::new(30, 8)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, &mut app))
            .unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let screen: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let all = screen.join("\n");
        // Wrapping splits phrases across rows; compare on collapsed whitespace.
        let flat = all.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(all.contains("bogus"), "{all}");
        assert!(flat.contains("press e to edit"), "{all}");
    }

    #[test]
    fn wrapped_height_counts_word_wrapped_lines() {
        assert_eq!(wrapped_height("", 10), 0, "no status, no row");
        assert_eq!(wrapped_height("one", 10), 1);
        assert_eq!(wrapped_height("aa bb cc", 5), 2);
        assert_eq!(
            wrapped_height("a bbbbbb c", 5),
            3,
            "a word wider than the line breaks"
        );
        assert_eq!(
            wrapped_height("aaaaaaaaaaaa", 5),
            3,
            "a word more than twice the width"
        );
        assert_eq!(
            wrapped_height("日本語", 2),
            4,
            "display width counts, not chars: 6 columns break over 2"
        );
        assert_eq!(wrapped_height("aaa\nbbb", 10), 2);
        assert_eq!(wrapped_height("anything", 0), 0);
    }

    #[test]
    fn a_key_error_falls_back_to_all_default_binds() {
        let dir = crate::testutil::unique_dir("ui-keys");
        let cfg_dir = dir.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[keys]\nselect = \"ctrl-nonsense\"\n[[sort_presets]]\nname = \"x\"\nsort = [{ field = \"agent\" }]",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let env = Env::test(
            &cfg_dir.to_string_lossy(),
            &dir.join("state").to_string_lossy(),
        );
        let loaded = load(&env);
        assert!(
            loaded.key_err.is_some_and(|e| e.contains("ctrl-nonsense")),
            "the key error is kept for the status join"
        );
        assert!(loaded.app.status.contains("config.toml"));
        let mut app = loaded.app;
        assert_eq!(
            handle_key(
                &mut app,
                &loaded.binds,
                key(KeyCode::Enter, KeyModifiers::NONE)
            ),
            Action::Apply(0),
            "the default select still applies after a key error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
