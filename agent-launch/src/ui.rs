use crate::config::{Agent, Config};
use crate::launch;
use crate::{Env, Res};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const HERE: &str = "(this directory)";

enum Row {
    Here,
    Typed(String),
    Repo(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Step {
    Repo,
    Prompt,
}

#[derive(PartialEq, Debug)]
pub struct Request {
    pub template: String,
    pub repo: String,
    pub prompt: String,
}

#[derive(PartialEq, Debug)]
pub enum Action {
    None,
    Quit,
    Launch(Request),
}

// A one-line text field. The cursor counts characters: bytes would cut a multibyte character.
#[derive(Default)]
pub struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    // Pasted text is one line here: each line break (CRLF counts as one) and other control
    // character becomes a space.
    fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n");
        let clean: String = s
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        self.text.insert_str(self.byte(self.cursor), &clean);
        self.cursor += clean.chars().count();
    }

    // Returns whether the text changed. Keys that only move the cursor or mean nothing here
    // return false.
    fn edit(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Left, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) => self.cursor = (self.cursor + 1).min(self.len()),
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => self.cursor = 0,
            (KeyCode::Char('u'), KeyModifiers::CONTROL) if self.cursor > 0 => {
                self.text.drain(..self.byte(self.cursor));
                self.cursor = 0;
                return true;
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cursor = self.len()
            }
            (KeyCode::Backspace, _) if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.byte(self.cursor));
                return true;
            }
            (KeyCode::Delete, _) if self.cursor < self.len() => {
                self.text.remove(self.byte(self.cursor));
                return true;
            }
            (KeyCode::Char(c), m) if m == KeyModifiers::NONE || m == KeyModifiers::SHIFT => {
                self.text.insert(self.byte(self.cursor), c);
                self.cursor += 1;
                return true;
            }
            _ => {}
        }
        false
    }

    // The text from the first character that keeps the cursor inside `avail` cells, and the
    // cursor's column in it.
    fn view(&self, avail: usize) -> (String, usize) {
        let chars: Vec<char> = self.text.chars().collect();
        // Grapheme clusters, the units ratatui draws (base + combining marks, ZWJ sequences, skin
        // tones, flags), each measured once as a string. Fitting, the cursor column and the shown
        // slice all use these widths, a cluster is never split, and a redraw stays linear.
        let mut groups: Vec<(usize, usize, usize)> = Vec::new();
        let mut start = 0;
        for g in self.text.graphemes(true) {
            let n = g.chars().count();
            groups.push((start, start + n, g.width()));
            start += n;
        }
        // The cluster the cursor is on, also when the cursor is inside it.
        let at = groups
            .iter()
            .position(|g| self.cursor < g.1)
            .unwrap_or(groups.len());
        // The cluster under the cursor must fit too, or a wide one is cut at the right edge. Past
        // the end one cell is kept for the cursor, unless that leaves nothing at all to show (a
        // wide last character in a very narrow field); the cursor is clamped to the last cell then.
        let fit = |under: usize| {
            let mut first = at;
            let mut used = under;
            while first > 0 && used + groups[first - 1].2 <= avail {
                first -= 1;
                used += groups[first].2;
            }
            first
        };
        let mut first = fit(groups.get(at).map_or(1, |g| g.2.max(1)));
        if first == groups.len() && first > 0 {
            first = fit(0);
        }
        let col = groups[first..at].iter().map(|g| g.2).sum();
        let mut used = 0;
        let last = groups[first..]
            .iter()
            .take_while(|g| {
                used += g.2;
                used <= avail
            })
            .last()
            .map_or(0, |g| g.1);
        let start = groups.get(first).map_or(chars.len(), |g| g.0);
        (chars[start..last.max(start)].iter().collect(), col)
    }
}

pub struct App {
    cwd: String,
    agents: Vec<Agent>,
    agent: usize,
    repos: Vec<String>,
    step: Step,
    repo_step: bool,
    filter: Input,
    // Indexes into `repos`, best match first.
    shown: Vec<usize>,
    // An index into rows().
    cur: usize,
    repo: String,
    prompt: Input,
    // The last failure, shown under the list until the next key.
    pub status: String,
}

impl App {
    pub fn new(cfg: Config, repos: Vec<String>, cwd: String, status: String) -> App {
        let repo_step = cfg.wants_repo();
        let mut app = App {
            cwd,
            agents: cfg.agents,
            agent: 0,
            shown: Vec::new(),
            repos,
            step: if repo_step { Step::Repo } else { Step::Prompt },
            repo_step,
            filter: Input::default(),
            cur: 0,
            repo: String::new(),
            prompt: Input::default(),
            status,
        };
        app.refilter();
        app
    }

    #[cfg(test)]
    pub fn repos(&self) -> &[String] {
        &self.repos
    }

    fn query(&self) -> &str {
        self.filter.text.trim()
    }

    fn refilter(&mut self) {
        let q = self.query();
        if q.is_empty() {
            self.shown = (0..self.repos.len()).collect();
            self.cur = 0;
            return;
        }
        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(usize, u32)> = self
            .repos
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                pattern
                    .score(Utf32Str::new(r, &mut buf), &mut matcher)
                    .map(|s| (i, s))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        self.shown = scored.into_iter().map(|(i, _)| i).collect();
        // Typing then Enter takes the best match; with none, the typed text.
        let rows = self.rows();
        self.cur = rows
            .iter()
            .position(|r| matches!(r, Row::Repo(_)))
            .or_else(|| rows.iter().position(|r| matches!(r, Row::Typed(_))))
            .unwrap_or(0);
    }

    // The repo list: "(this directory)", then the typed text as its own row (fuzzy matches can
    // hide a new name inside an existing one, so it must stay pickable), then the matches.
    fn rows(&self) -> Vec<Row> {
        let q = self.query();
        // Only an exact candidate actually shown makes the typed row a duplicate: the pattern syntax
        // can hide one (a trailing `$` anchors), and then the typed text must stay pickable.
        let typed = !q.is_empty() && !self.shown.iter().any(|&i| self.repos[i] == q);
        std::iter::once(Row::Here)
            .chain(typed.then(|| Row::Typed(q.to_string())))
            .chain(self.shown.iter().map(|&i| Row::Repo(i)))
            .collect()
    }

    fn repo_choice(&self) -> String {
        match self.rows().get(self.cur) {
            Some(Row::Typed(t)) => t.clone(),
            Some(Row::Repo(i)) => self.repos[*i].clone(),
            _ => String::new(),
        }
    }

    fn move_cur(&mut self, delta: isize) {
        let last = self.rows().len() - 1;
        self.cur = self.cur.saturating_add_signed(delta).min(last);
    }

    fn cycle_agent(&mut self, delta: isize) {
        let n = self.agents.len();
        self.agent = (self.agent + n).wrapping_add_signed(delta) % n;
    }
}

// The cwd and config values are drawn here; a control byte in them would act on the terminal.
fn shown(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

pub fn handle_paste(app: &mut App, s: &str) -> Action {
    app.status.clear();
    match app.step {
        Step::Repo => {
            app.filter.paste(s);
            app.refilter();
        }
        Step::Prompt => app.prompt.paste(s),
    }
    Action::None
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    app.status.clear();
    // Ctrl-D closes as EOF did in 0.1's read prompt.
    if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        && key.modifiers == KeyModifiers::CONTROL
    {
        return Action::Quit;
    }
    match app.step {
        Step::Repo => match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Action::Quit,
            (KeyCode::Enter, _) => {
                app.repo = app.repo_choice();
                app.step = Step::Prompt;
                Action::None
            }
            (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                app.move_cur(-1);
                Action::None
            }
            (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                app.move_cur(1);
                Action::None
            }
            _ => {
                if app.filter.edit(key) {
                    app.refilter();
                }
                Action::None
            }
        },
        Step::Prompt => match (key.code, key.modifiers) {
            (KeyCode::Esc, _) if app.repo_step => {
                app.step = Step::Repo;
                Action::None
            }
            (KeyCode::Esc, _) => Action::Quit,
            (KeyCode::Enter, _) => {
                let prompt = app.prompt.text.trim();
                if prompt.is_empty() {
                    return Action::Quit;
                }
                Action::Launch(Request {
                    template: app.agents[app.agent].command.clone(),
                    repo: app.repo.clone(),
                    prompt: prompt.to_string(),
                })
            }
            (KeyCode::Tab, _) => {
                app.cycle_agent(1);
                Action::None
            }
            (KeyCode::BackTab, _) => {
                app.cycle_agent(-1);
                Action::None
            }
            _ => {
                app.prompt.edit(key);
                Action::None
            }
        },
    }
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

pub fn draw(frame: &mut Frame, app: &App) {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let agent = &app.agents[app.agent];
    let head_lines = match app.step {
        Step::Repo => format!("in: {}\nrepo (Enter picks, Esc closes)", shown(&app.cwd)),
        Step::Prompt => format!(
            "in: {}\nrepo: {}\nagent: {} (Tab for the next)  {}",
            shown(&app.cwd),
            if app.repo.is_empty() {
                HERE.into()
            } else {
                shown(&app.repo)
            },
            shown(&agent.name),
            shown(&agent.command)
        ),
    };
    let head_height = u16::try_from(head_lines.lines().count()).unwrap_or(2);
    let [head, input, body, status] = Layout::vertical([
        Constraint::Length(head_height),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(head_lines).style(dim), head);
    match app.step {
        Step::Repo => input_line(frame, &app.filter, input),
        Step::Prompt => input_line(frame, &app.prompt, input),
    }
    if app.step == Step::Repo {
        let items: Vec<ListItem> = app
            .rows()
            .into_iter()
            .map(|r| match r {
                Row::Here => ListItem::new(HERE),
                Row::Typed(t) => ListItem::new(format!("use {t:?}")),
                Row::Repo(i) => ListItem::new(app.repos[i].as_str()),
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::bordered())
                .highlight_symbol("> ")
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            body,
            &mut ListState::default().with_selected(Some(app.cur)),
        );
    }
    let (text, style) = if !app.status.is_empty() {
        (shown(&app.status), Style::new().fg(Color::Red))
    } else {
        (String::new(), dim)
    };
    frame.render_widget(Paragraph::new(text).style(style), status);
}

// A subprocess's stderr can be part of the message; its control bytes must not reach the terminal.
// Line breaks are kept so a multi-line stderr stays readable.
fn error_text(msg: &str) -> String {
    let body: String = msg
        .split('\n')
        .map(|l| shown(&l.replace('\t', " ")))
        .collect::<Vec<_>>()
        .join("\n");
    format!("agent-launch: {body}\n\nPress Enter (or Esc).")
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

// Everything that can fail before the screen is needed. The repos command runs here, before the
// terminal goes raw, so Ctrl-C still stops one that hangs.
pub fn prepare(env: &Env) -> Res<App> {
    // open always sets it; without it the agent would start in some other directory.
    let cwd = env
        .cwd
        .clone()
        .ok_or("AGENT_LAUNCH_CWD is not set (open the popup with the plugin action)")?;
    let cfg = crate::config::load(&env.config_path)?;
    let mut repos = Vec::new();
    let mut status = String::new();
    if let (true, Some(cmd)) = (cfg.wants_repo(), &cfg.repos) {
        match launch::list_repos(cmd, &cwd) {
            Ok(r) => repos = r,
            Err(e) => status = e,
        }
    }
    Ok(App::new(cfg, repos, cwd, status))
}

fn session(env: &Env, mut app: App, term: &mut DefaultTerminal) -> Res<()> {
    loop {
        term.draw(|f| draw(f, &app))
            .map_err(|e| format!("cannot draw the screen: {e}"))?;
        let action = match event::read().map_err(|e| format!("cannot read the keyboard: {e}"))? {
            Event::Key(key) => handle_key(&mut app, key),
            Event::Paste(s) => handle_paste(&mut app, &s),
            _ => continue,
        };
        match action {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::Launch(r) => {
                return launch::launch(&env.herdr, &app.cwd, &r.template, &r.repo, &r.prompt)
            }
        }
    }
}

pub fn run(env: &Env) -> Res<()> {
    let prepared = prepare(env);
    let mut term = match ratatui::try_init() {
        Ok(t) => t,
        // Without a terminal the problem found while preparing is the one worth reporting.
        Err(e) => {
            return Err(prepared
                .err()
                .unwrap_or_else(|| format!("cannot start the terminal UI: {e}")))
        }
    };
    // Without bracketed paste a pasted task would arrive as key presses, and a pasted newline as Enter.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = prepared.and_then(|app| session(env, app, &mut term));
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn agent(name: &str, command: &str) -> Agent {
        Agent {
            name: name.into(),
            command: command.into(),
        }
    }

    fn cfg(agents: Vec<Agent>) -> Config {
        Config {
            agents,
            repos: None,
        }
    }

    fn with_repo() -> Vec<Agent> {
        vec![
            agent("claude", "ccxn {repo} {prompt}"),
            agent("devin", "ccxn --agent devin {repo} {prompt}"),
        ]
    }

    fn repos() -> Vec<String> {
        [
            "github.com/o/alpha",
            "github.com/o/beta",
            "gitlab.com/x/alphabet",
        ]
        .map(String::from)
        .to_vec()
    }

    fn app() -> App {
        App::new(cfg(with_repo()), repos(), "/b c".into(), String::new())
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

    fn launch_req(repo: &str, prompt: &str, template: &str) -> Action {
        Action::Launch(Request {
            template: template.into(),
            repo: repo.into(),
            prompt: prompt.into(),
        })
    }

    #[test]
    fn the_repo_step_is_skipped_when_no_agent_has_the_placeholder() {
        let mut a = App::new(
            cfg(vec![agent("claude", "claude -- {prompt}")]),
            vec![],
            "/b".into(),
            String::new(),
        );
        assert_eq!(a.step, Step::Prompt);
        type_str(&mut a, "do x");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            launch_req("", "do x", "claude -- {prompt}")
        );
    }

    #[test]
    fn esc_closes_when_the_repo_step_was_skipped() {
        let mut a = App::new(
            cfg(vec![agent("claude", "claude -- {prompt}")]),
            vec![],
            "/b".into(),
            String::new(),
        );
        assert_eq!(press(&mut a, KeyCode::Esc), Action::Quit);
    }

    #[test]
    fn this_directory_is_the_first_row_and_gives_an_empty_repo() {
        let mut a = app();
        assert_eq!(a.step, Step::Repo);
        assert_eq!(a.cur, 0);
        assert_eq!(press(&mut a, KeyCode::Enter), Action::None);
        assert_eq!((a.step, a.repo.as_str()), (Step::Prompt, ""));
        type_str(&mut a, "do x");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            launch_req("", "do x", "ccxn {repo} {prompt}")
        );
    }

    #[test]
    fn a_candidate_row_is_taken_as_listed() {
        let mut a = app();
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "github.com/o/beta");
    }

    #[test]
    fn up_and_down_stop_at_the_ends_and_ctrl_p_n_do_the_same() {
        let mut a = app();
        press(&mut a, KeyCode::Up);
        assert_eq!(a.cur, 0);
        for _ in 0..10 {
            ctrl(&mut a, 'n');
        }
        assert_eq!(a.cur, 3);
        ctrl(&mut a, 'p');
        assert_eq!(a.cur, 2);
    }

    #[test]
    fn typing_filters_fuzzily_and_enter_takes_the_best_match() {
        let mut a = app();
        type_str(&mut a, "beta");
        assert_eq!(a.shown, [1]);
        // 0 is "(this directory)", 1 the typed text, 2 the best match.
        assert_eq!(a.cur, 2);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "github.com/o/beta");

        let mut a = app();
        type_str(&mut a, "ala");
        // alpha and alphabet both match; the earlier line wins a tie, so the order is stable.
        assert_eq!(a.shown, [0, 2]);
    }

    #[test]
    fn this_directory_stays_reachable_while_filtering() {
        let mut a = app();
        type_str(&mut a, "beta");
        press(&mut a, KeyCode::Up);
        press(&mut a, KeyCode::Up);
        assert_eq!(a.cur, 0);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "");
    }

    #[test]
    fn typed_text_no_candidate_matches_is_the_repo() {
        let mut a = app();
        type_str(&mut a, "new/repo");
        assert!(a.shown.is_empty());
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "new/repo");
        type_str(&mut a, "go");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            launch_req("new/repo", "go", "ccxn {repo} {prompt}")
        );
    }

    #[test]
    fn typed_repo_works_with_no_candidates_at_all() {
        let mut a = App::new(cfg(with_repo()), vec![], "/b".into(), "boom".into());
        type_str(&mut a, "o/r");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "o/r");
    }

    #[test]
    fn backspacing_the_filter_empty_goes_back_to_this_directory() {
        let mut a = app();
        type_str(&mut a, "be");
        assert_eq!(a.cur, 2);
        press(&mut a, KeyCode::Backspace);
        press(&mut a, KeyCode::Backspace);
        assert_eq!((a.cur, a.shown.len()), (0, 3));
    }

    #[test]
    fn tab_cycles_the_agent_forward_and_shift_tab_backward_and_enter_uses_it() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.agent, 0);
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.agent, 1);
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.agent, 0);
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.agent, 1);
        type_str(&mut a, "go");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            launch_req("", "go", "ccxn --agent devin {repo} {prompt}")
        );
    }

    #[test]
    fn tab_and_shift_tab_walk_three_agents_in_opposite_directions() {
        let mut a = App::new(
            cfg(vec![
                agent("a", "a {prompt}"),
                agent("b", "b {prompt}"),
                agent("c", "c {prompt}"),
            ]),
            vec![],
            "/b".into(),
            String::new(),
        );
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.agent, 2);
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.agent, 1);
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.agent, 0);
    }

    #[test]
    fn a_single_agent_stays_put_on_tab() {
        let mut a = App::new(
            cfg(vec![agent("claude", "claude -- {prompt}")]),
            vec![],
            "/b".into(),
            String::new(),
        );
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.agent, 0);
    }

    #[test]
    fn esc_goes_back_to_the_repo_step_keeping_the_prompt_then_closes() {
        let mut a = app();
        type_str(&mut a, "be");
        press(&mut a, KeyCode::Enter);
        type_str(&mut a, "do x");
        assert_eq!(press(&mut a, KeyCode::Esc), Action::None);
        assert_eq!(a.step, Step::Repo);
        assert_eq!(a.filter.text, "be");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.prompt.text, "do x");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            launch_req("github.com/o/beta", "do x", "ccxn {repo} {prompt}")
        );
        let mut a = app();
        assert_eq!(press(&mut a, KeyCode::Esc), Action::Quit);
    }

    #[test]
    fn ctrl_c_closes_from_either_step() {
        let mut a = app();
        assert_eq!(ctrl(&mut a, 'c'), Action::Quit);
        press(&mut a, KeyCode::Enter);
        assert_eq!(ctrl(&mut a, 'c'), Action::Quit);
    }

    #[test]
    fn a_blank_prompt_starts_nothing() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(press(&mut a, KeyCode::Enter), Action::Quit);
        type_str(&mut a, "   ");
        assert_eq!(press(&mut a, KeyCode::Enter), Action::Quit);
    }

    #[test]
    fn the_prompt_is_trimmed() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        type_str(&mut a, "  do x ");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            launch_req("", "do x", "ccxn {repo} {prompt}")
        );
    }

    #[test]
    fn editing_counts_characters_so_multibyte_text_is_never_cut() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        type_str(&mut a, "あいう");
        press(&mut a, KeyCode::Left);
        type_str(&mut a, "x");
        assert_eq!(a.prompt.text, "あいxう");
        press(&mut a, KeyCode::Backspace);
        press(&mut a, KeyCode::Backspace);
        assert_eq!(a.prompt.text, "あう");
        press(&mut a, KeyCode::Delete);
        assert_eq!(a.prompt.text, "あ");
        press(&mut a, KeyCode::Home);
        press(&mut a, KeyCode::Delete);
        assert_eq!(a.prompt.text, "");
        // Backspace and Delete at the ends do nothing.
        press(&mut a, KeyCode::Backspace);
        press(&mut a, KeyCode::Delete);
        assert_eq!(a.prompt.cursor, 0);
    }

    fn screen(app: &App, width: u16) -> (Vec<String>, ratatui::layout::Position) {
        let mut term = Terminal::new(TestBackend::new(width, 12)).unwrap_or_else(|e| panic!("{e}"));
        term.draw(|f| draw(f, app))
            .unwrap_or_else(|e| panic!("{e}"));
        let pos = term.get_cursor_position().unwrap_or_else(|e| panic!("{e}"));
        let buf = term.backend().buffer();
        let lines = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        (lines, pos)
    }

    #[test]
    fn the_repo_step_shows_this_directory_first_then_the_candidates() {
        let a = app();
        let (lines, pos) = screen(&a, 50);
        let text = lines.join("\n");
        assert!(lines[0].starts_with("in: /b c"), "{text}");
        let here = text.find(HERE).unwrap_or(usize::MAX);
        let alpha = text.find("github.com/o/alpha").unwrap_or(usize::MAX);
        assert!(here < alpha, "{text}");
        assert!(text.contains("gitlab.com/x/alphabet"), "{text}");
        assert_eq!((pos.x, pos.y), (2, 2));
    }

    #[test]
    fn the_prompt_step_shows_the_repo_and_the_agent() {
        let mut a = app();
        type_str(&mut a, "beta");
        press(&mut a, KeyCode::Enter);
        press(&mut a, KeyCode::Tab);
        let (lines, _) = screen(&a, 70);
        let text = lines.join("\n");
        assert!(text.contains("repo: github.com/o/beta"), "{text}");
        assert!(text.contains("agent: devin"), "{text}");
        assert!(
            text.contains("ccxn --agent devin {repo} {prompt}"),
            "{text}"
        );
        assert!(!text.contains(HERE), "{text}");
    }

    #[test]
    fn a_failed_repos_command_shows_in_the_status_line_until_a_key() {
        let mut a = App::new(
            cfg(with_repo()),
            vec![],
            "/b".into(),
            "repos command failed".into(),
        );
        let (lines, _) = screen(&a, 50);
        assert!(lines[11].starts_with("repos command failed"), "{lines:?}");
        type_str(&mut a, "o");
        let (lines, _) = screen(&a, 50);
        assert!(!lines.join("\n").contains("repos command failed"));
    }

    #[test]
    fn typed_text_without_a_match_is_the_highlighted_row() {
        let mut a = app();
        type_str(&mut a, "zzz");
        assert_eq!(a.cur, 1);
        let (lines, _) = screen(&a, 50);
        assert!(
            lines.iter().any(|l| l.contains("> use \"zzz\"")),
            "{lines:?}"
        );
    }

    #[test]
    fn typed_text_that_fuzzy_matches_a_candidate_can_still_be_picked() {
        let mut a = app();
        // "o/ab" fuzzily matches github.com/o/alphabet but is a different, new repo.
        type_str(&mut a, "o/ab");
        assert!(!a.shown.is_empty());
        press(&mut a, KeyCode::Up);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "o/ab");
    }

    #[test]
    fn typing_a_candidate_exactly_adds_no_typed_row() {
        let mut a = app();
        type_str(&mut a, "github.com/o/beta");
        assert_eq!(a.rows().len(), 2);
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "github.com/o/beta");
    }

    #[test]
    fn a_paste_goes_into_the_focused_input_as_one_line() {
        let mut a = app();
        handle_paste(&mut a, "beta");
        assert_eq!(
            (a.filter.text.as_str(), a.shown.as_slice()),
            ("beta", &[1][..])
        );
        press(&mut a, KeyCode::Enter);
        handle_paste(&mut a, "fix\nthe\tbuild");
        assert_eq!(a.prompt.text, "fix the build");
        assert_eq!(a.prompt.cursor, 13);
    }

    #[test]
    fn control_characters_in_the_status_and_cwd_are_not_drawn() {
        // The status carries the repos command's stderr; the cwd comes from Herdr.
        let a = App::new(
            cfg(with_repo()),
            vec![],
            "/b\u{1b}x".into(),
            "repos command failed: \u{1b}[31mboom".into(),
        );
        let (lines, _) = screen(&a, 60);
        let all = lines.join("\n");
        assert!(!all.contains('\u{1b}'), "{all}");
        assert!(
            all.contains("/b\u{FFFD}x") && all.contains("\u{FFFD}[31mboom"),
            "{all}"
        );
    }

    #[test]
    fn the_error_screen_does_not_draw_control_characters() {
        let t = error_text("pane run failed: \u{1b}[31mboom\nsecond\tline");
        assert!(
            !t.contains('\u{1b}') && t.contains("\u{FFFD}[31mboom"),
            "{t}"
        );
        assert!(t.contains("boom\nsecond line"), "{t}");
    }

    #[test]
    fn a_pasted_crlf_is_one_space() {
        let mut i = Input::default();
        i.paste("a\r\nb");
        assert_eq!(i.text, "a b");
    }

    #[test]
    fn an_emoji_sequence_is_kept_whole_when_its_characters_fit() {
        let mut i = Input::default();
        i.text = "👩\u{200d}💻a".into();
        i.cursor = 0;
        // The emoji draws as two cells and "a" as one, so three hold all of it.
        assert_eq!(i.view(3).0, "👩\u{200d}💻a");
        // Two cells cannot hold "a" as well, and the emoji is never split.
        assert_eq!(i.view(2), ("👩\u{200d}💻".to_string(), 0));
        // With the cursor on "a", the emoji scrolls out whole rather than half.
        i.cursor = 3;
        assert_eq!(i.view(2), ("a".to_string(), 0));
    }

    #[test]
    fn skin_tones_and_flags_are_one_cluster() {
        let mut i = Input::default();
        i.text = "👍🏽".into();
        i.cursor = 2;
        assert_eq!(i.view(10), ("👍🏽".to_string(), 2));
        i.text = "🇺🇸".into();
        assert_eq!(i.view(3), ("🇺🇸".to_string(), 2));
    }

    #[test]
    fn a_cursor_inside_a_cluster_keeps_that_cluster_on_screen() {
        // Left can stop between U+2764 and U+FE0F; the heart stays visible with the cursor on it.
        let mut i = Input::default();
        i.text = "❤\u{fe0f}X".into();
        i.cursor = 1;
        assert_eq!(i.view(2), ("❤\u{fe0f}".to_string(), 0));
    }

    #[test]
    fn a_wide_last_character_stays_visible_with_the_cursor_at_the_end() {
        let mut i = Input::default();
        i.text = "あ".into();
        i.cursor = 1;
        assert_eq!(i.view(2).0, "あ");
        assert_eq!(Input::default().view(0), (String::new(), 0));
    }

    #[test]
    fn a_variation_selector_emoji_fits_and_puts_the_cursor_where_it_draws() {
        // "❤️" is U+2764 + U+FE0F: one char of width 1 plus a zero-width one, drawn as two cells.
        let mut i = Input::default();
        i.text = "❤\u{fe0f}❤\u{fe0f}".into();
        i.cursor = 4;
        let (shown, col) = i.view(3);
        assert_eq!((shown.as_str(), col), ("❤\u{fe0f}", 2));
    }

    #[test]
    fn a_key_release_does_nothing() {
        let mut a = app();
        let rel =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(handle_key(&mut a, rel), Action::None);
        assert_eq!(a.step, Step::Repo);
    }

    #[test]
    fn ctrl_d_closes_and_ctrl_u_clears_to_the_start() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        type_str(&mut a, "abc");
        press(&mut a, KeyCode::Left);
        assert_eq!(ctrl(&mut a, 'u'), Action::None);
        assert_eq!(a.prompt.text, "c");
        assert_eq!(ctrl(&mut a, 'd'), Action::Quit);
    }

    #[test]
    fn a_long_run_of_zero_width_characters_stays_quick() {
        // U+0301 (combining acute accent) takes no cell.
        let mut i = Input::default();
        i.text = format!("a{}", "\u{301}".repeat(50_000));
        let start = std::time::Instant::now();
        i.cursor = i.text.chars().count();
        let (end_view, _) = i.view(18);
        i.cursor = 0;
        let (start_view, _) = i.view(18);
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
        // The visible "a" is never pushed out by the invisible run around it.
        assert!(end_view.starts_with('a') && start_view.starts_with('a'));
    }

    #[test]
    fn a_zero_width_run_before_the_cursor_keeps_the_visible_text() {
        let mut i = Input::default();
        i.text = format!("a{}b", "\u{301}".repeat(200));
        i.cursor = 201;
        let (shown, col) = i.view(18);
        assert!(shown.starts_with('a') && shown.ends_with('b'), "{shown:?}");
        assert_eq!(col, 1);
    }

    #[test]
    fn only_what_fits_is_returned_with_the_cursor_at_the_start() {
        let mut i = Input::default();
        i.text = "a".repeat(50_000);
        i.cursor = 0;
        assert_eq!(i.view(18), ("a".repeat(18), 0));
    }

    #[test]
    fn a_long_line_scrolls_to_the_same_view_quickly() {
        let mut i = Input::default();
        i.text = "a".repeat(50_000);
        i.cursor = 50_000;
        let start = std::time::Instant::now();
        let (shown, col) = i.view(18);
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
        assert_eq!((shown.len(), col), (17, 17));
    }

    #[test]
    fn the_cursor_column_measures_an_emoji_sequence_as_drawn() {
        // 👩‍💻 is three chars (woman, ZWJ, laptop) but two cells on screen.
        let mut i = Input::default();
        i.text = "👩\u{200d}💻a".into();
        i.cursor = i.text.chars().count();
        assert_eq!(i.view(40).1, 3);
    }

    #[test]
    fn typed_text_stays_pickable_when_the_pattern_syntax_hides_the_same_candidate() {
        // A trailing `$` anchors in the filter syntax, so the candidate "o/r$" is not shown.
        let mut a = App::new(
            cfg(with_repo()),
            vec!["o/r$".into()],
            "/b".into(),
            String::new(),
        );
        type_str(&mut a, "o/r$");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.repo, "o/r$");
    }

    #[test]
    fn a_wide_character_under_the_cursor_is_not_cut_at_the_edge() {
        // 3 cells free: "a" + "あ" (2 cells) with the cursor on "あ" must keep "あ" whole.
        let mut i = Input::default();
        i.text = "abあ".into();
        i.cursor = 2;
        let (shown, col) = i.view(3);
        assert!(shown.contains('あ') && col + 2 <= 3, "{shown:?} {col}");
    }

    #[test]
    fn the_cursor_follows_wide_characters_and_stays_inside_the_line() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        type_str(&mut a, "あい");
        let (_, pos) = screen(&a, 40);
        assert_eq!((pos.x, pos.y), (2 + 4, 3));
        for _ in 0..300 {
            press(&mut a, KeyCode::Char('う'));
        }
        let (lines, pos) = screen(&a, 20);
        assert!(pos.x < 20, "{pos:?}");
        // A wide character fills two cells; the second shows as a space.
        assert!(lines[3].trim_end().ends_with('う'), "{lines:?}");
    }
}
