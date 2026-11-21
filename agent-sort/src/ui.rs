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
    // The last failure or edit result, shown under the help line until the next key.
    pub status: String,
    pub help: String,
}

impl App {
    pub fn new(presets: Vec<Preset>, current: Option<String>, status: String, help: String) -> App {
        let cur = current
            .as_ref()
            .and_then(|n| presets.iter().position(|p| &p.name == n))
            .unwrap_or(0);
        App {
            presets,
            current,
            cur,
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
}

// The resolved key sets, or all defaults when the [keys] table cannot be used; the error goes to
// the status line and the popup keeps working.
fn key_sets(env: &Env, cfg: &crate::config::Config, status: &mut String) -> Vec<KeySet> {
    match keys::resolve(cfg.keys_raw()) {
        Ok(sets) => sets,
        Err(e) => {
            *status = format!("{}: {e}", env.config_path);
            keys::resolve(None).unwrap_or_default()
        }
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
            };
        }
    };
    let mut status = String::new();
    let sets = key_sets(env, &cfg, &mut status);
    let current = current(&cfg, &state::read(&env.state_path)).map(String::from);
    Loaded {
        binds: Binds::new(&sets),
        app: App::new(cfg.presets, current, status, help_line(&sets)),
    }
}

pub fn handle_key(app: &mut App, binds: &Binds, key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    app.status.clear();
    let pressed = keys::normalize(key.code, key.modifiers);
    let Some((_, op)) = binds.0.iter().find(|(b, _)| *b == pressed) else {
        return Action::None;
    };
    match op {
        Op::Up => {
            app.move_cursor(-1);
            Action::None
        }
        Op::Down => {
            app.move_cursor(1);
            Action::None
        }
        // From a broken config there is nothing to apply or clear; only edit and quit work.
        Op::Select if app.presets.is_empty() => Action::None,
        Op::Clear if app.presets.is_empty() => Action::None,
        Op::Select => Action::Apply(app.cur),
        Op::Clear => Action::Clear,
        Op::Edit => Action::Edit,
        Op::Quit => Action::Quit,
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let [list_area, desc_area, help_area, status_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

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
    let selected = (!app.presets.is_empty()).then_some(app.cur);
    frame.render_stateful_widget(
        list,
        list_area,
        &mut ListState::default().with_selected(selected),
    );

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
        Paragraph::new(app.status.as_str()).style(Style::new().fg(Color::Red)),
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
        term.draw(|f| draw(f, &loaded.app))
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
            Action::Clear => match crate::clear(env) {
                Ok(()) => return Ok(()),
                Err(e) => loaded.app.status = e,
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
                loaded.app.status = status;
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
            "up/k down/j move".into(),
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
        let mut term = Terminal::new(TestBackend::new(50, 10)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, &app))
            .unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let screen: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let all = screen.join("\n");
        assert!(all.contains("attention"), "{all}");
        assert!(all.contains("recent (current)"), "{all}");
        assert!(all.contains("sort: attention desc"), "{all}");
        assert!(all.contains("up/k down/j move"), "{all}");
        assert!(all.contains("cannot apply"), "{all}");
    }

    #[test]
    fn a_key_error_falls_back_to_all_default_binds() {
        let raw = toml::from_str::<toml::Table>("[keys]\nselect = \"ctrl-nonsense\"")
            .unwrap_or_else(|e| panic!("{e}"));
        let sets = keys::resolve(raw.get("keys")).err();
        assert!(sets.is_some());
        let sets = keys::resolve(None).unwrap_or_else(|e| panic!("{e}"));
        let binds = Binds::new(&sets);
        let mut app = app();
        assert_eq!(
            handle_key(&mut app, &binds, key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Apply(1),
            "the default select still applies after a key error"
        );
    }
}
