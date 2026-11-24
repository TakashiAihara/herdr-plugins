use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// A one-line text field. The cursor counts characters: bytes would cut a multibyte character.
#[derive(Default)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
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

    // The greatest grapheme-cluster boundary below the cursor, in chars: inside a cluster
    // that is the cluster's start, on a boundary the start of the cluster ending there.
    fn prev_boundary(&self) -> usize {
        let mut edge = 0;
        let mut prev = 0;
        for g in self.text.graphemes(true) {
            edge += g.chars().count();
            if edge >= self.cursor {
                return prev;
            }
            prev = edge;
        }
        prev
    }

    // The smallest grapheme-cluster boundary above the cursor, in chars: inside a cluster
    // that is the cluster's end, on a boundary the end of the cluster starting there.
    fn next_boundary(&self) -> usize {
        let mut edge = 0;
        for g in self.text.graphemes(true) {
            edge += g.chars().count();
            if edge > self.cursor {
                return edge;
            }
        }
        edge
    }

    // Pasted text is one line here: each line break (CRLF counts as one) and other control
    // character becomes a space.
    pub fn paste(&mut self, s: &str) {
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
    pub fn edit(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Left, _) => self.cursor = self.prev_boundary(),
            (KeyCode::Right, _) => self.cursor = self.next_boundary(),
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
                let from = self.prev_boundary();
                let range = self.byte(from)..self.byte(self.cursor);
                self.text.drain(range);
                self.cursor = from;
                return true;
            }
            (KeyCode::Delete, _) if self.cursor < self.len() => {
                let to = self.next_boundary();
                let range = self.byte(self.cursor)..self.byte(to);
                self.text.drain(range);
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
    pub fn view(&self, avail: usize) -> (String, usize) {
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
        // The cluster under the cursor must fit too, or a wide one is cut at the right edge; one
        // wider than the whole field leaves the field empty. Past
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
        if first > 0 && groups[first..].iter().all(|g| g.2 == 0) {
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

#[cfg(test)]
// Tests place the text and the cursor directly, to put the cursor inside a grapheme cluster.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn press(i: &mut Input, code: KeyCode) -> bool {
        i.edit(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn editing_counts_characters_so_multibyte_text_is_never_cut() {
        let mut i = Input::default();
        for c in "あいう".chars() {
            assert!(press(&mut i, KeyCode::Char(c)));
        }
        press(&mut i, KeyCode::Left);
        press(&mut i, KeyCode::Char('x'));
        assert_eq!(i.text, "あいxう");
        press(&mut i, KeyCode::Backspace);
        press(&mut i, KeyCode::Backspace);
        assert_eq!(i.text, "あう");
        press(&mut i, KeyCode::Delete);
        assert_eq!(i.text, "あ");
        press(&mut i, KeyCode::Home);
        press(&mut i, KeyCode::Delete);
        assert_eq!(i.text, "");
        // Backspace and Delete at the ends change nothing and report it.
        assert!(!press(&mut i, KeyCode::Backspace));
        assert!(!press(&mut i, KeyCode::Delete));
        assert_eq!(i.cursor, 0);
    }

    #[test]
    fn keys_that_only_move_the_cursor_report_no_change_and_ctrl_u_clears_to_the_start() {
        let mut i = Input::default();
        i.paste("abc");
        assert!(!press(&mut i, KeyCode::Left));
        assert!(i.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert_eq!(i.text, "c");
        assert!(!i.edit(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)));
        assert_eq!(i.text, "c");
    }

    #[test]
    fn a_pasted_crlf_is_one_space() {
        let mut i = Input::default();
        i.paste("a\r\nb");
        assert_eq!(i.text, "a b");
    }

    #[test]
    fn an_emoji_sequence_is_kept_whole_when_it_fits() {
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
    fn a_cluster_wider_than_the_field_shows_nothing_rather_than_panicking() {
        let mut i = Input::default();
        i.text = "aあ".into();
        i.cursor = 1;
        assert_eq!(i.view(1), (String::new(), 0));
    }

    #[test]
    fn a_trailing_zero_width_space_does_not_blank_the_field() {
        let mut i = Input::default();
        i.text = "あ\u{200b}".into();
        i.cursor = 2;
        assert!(i.view(2).0.starts_with('あ'), "{:?}", i.view(2));
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
        // A paste can leave the cursor between U+2764 and U+FE0F; the heart stays visible with the
        // cursor on it.
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
    fn the_cursor_and_the_delete_keys_take_a_whole_grapheme_cluster() {
        let key = |i: &mut Input, code| i.edit(KeyEvent::new(code, KeyModifiers::NONE));
        // 👩‍💻 is three chars and 🇺🇸 two; each is one cluster, as it is drawn.
        let mut i = Input::default();
        i.paste("a👩\u{200d}💻🇺🇸b");
        assert_eq!(i.cursor, 7);
        key(&mut i, KeyCode::Left);
        key(&mut i, KeyCode::Left);
        assert_eq!(i.cursor, 4);
        key(&mut i, KeyCode::Left);
        assert_eq!(i.cursor, 1);
        key(&mut i, KeyCode::Right);
        assert_eq!(i.cursor, 4);
        assert!(key(&mut i, KeyCode::Backspace));
        assert_eq!((i.text.as_str(), i.cursor), ("a🇺🇸b", 1));
        assert!(key(&mut i, KeyCode::Delete));
        assert_eq!((i.text.as_str(), i.cursor), ("ab", 1));
    }

    #[test]
    fn from_inside_a_cluster_left_and_right_reach_its_edges() {
        let key = |i: &mut Input, code| i.edit(KeyEvent::new(code, KeyModifiers::NONE));
        let mut i = Input::default();
        // The cluster is chars 1..4, so a one-char step from inside it would stop inside it.
        i.text = "x👩\u{200d}💻y".into();
        i.cursor = 2;
        key(&mut i, KeyCode::Right);
        assert_eq!(i.cursor, 4);
        i.cursor = 3;
        key(&mut i, KeyCode::Left);
        assert_eq!(i.cursor, 1);
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
        // A generous bound: linear work takes milliseconds, quadratic work on this size minutes.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
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
        // A generous bound: linear work takes milliseconds, quadratic work on this size minutes.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
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
    fn a_wide_character_under_the_cursor_is_not_cut_at_the_edge() {
        // 3 cells free: "a" + "あ" (2 cells) with the cursor on "あ" must keep "あ" whole.
        let mut i = Input::default();
        i.text = "abあ".into();
        i.cursor = 2;
        let (shown, col) = i.view(3);
        assert!(shown.contains('あ') && col + 2 <= 3, "{shown:?} {col}");
    }
}
