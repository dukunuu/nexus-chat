//! Mouse text-selection over the history pane. Coordinates are in *wrapped-line*
//! space: `(line, col)` where `line` indexes the fully-wrapped conversation and
//! `col` is a char offset into that line. The UI records the rendered layout each
//! frame (`record_render`); the event loop drives gestures (down/drag/up + a
//! long-press timer). Copy lives on `App` (it owns the clipboard).

use std::time::{Duration, Instant};

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

const MULTI_CLICK: Duration = Duration::from_millis(400);
const LONG_PRESS: Duration = Duration::from_millis(450);

/// (line, col) in wrapped-line space.
pub type Pos = (usize, usize);

/// What a long press produced.
pub enum LongPress {
    /// Raw code text, already exact — copy verbatim.
    Code(String),
    /// The message index under the press — `App` resolves this to the
    /// message's exact original content.
    Message(usize),
    /// A URL under the press — long-press copies it instead of opening it
    /// (opening happens on a plain click instead).
    Url(String),
}

/// What releasing a press should do.
pub enum Action {
    Copy(String),
    OpenUrl(String),
}

#[derive(Default)]
pub struct HistorySel {
    // Layout snapshot from the last render.
    inner: Rect,
    top: usize,
    lines: Vec<String>,
    /// Message index each line belongs to (None for none), for message-scoped
    /// selection.
    owner: Vec<Option<usize>>,
    /// Code-block id each line belongs to (None if not code), and the raw text
    /// of each block, so a long-press on code copies clean code.
    code: Vec<Option<usize>>,
    code_raw: Vec<String>,

    /// Active selection as (anchor, cursor); not necessarily ordered.
    sel: Option<(Pos, Pos)>,

    // Gesture state.
    anchor: Option<Pos>,
    down_at: Option<Instant>,
    down_pos: Pos,
    moved: bool,
    long_pressed: bool,
    last_click: Option<(Instant, Pos, u8)>,
    /// Click count of the *current* press (computed at press time, not
    /// release), so a drag following a double/triple click can extend by
    /// word/line instead of falling back to plain char range.
    click_count: u8,
}

impl HistorySel {
    /// Remember the rendered layout so screen coords can be mapped to positions.
    pub fn record_render(
        &mut self,
        inner: Rect,
        top: usize,
        lines: Vec<String>,
        owner: Vec<Option<usize>>,
        code: Vec<Option<usize>>,
        code_raw: Vec<String>,
    ) {
        self.inner = inner;
        self.top = top;
        self.lines = lines;
        self.owner = owner;
        self.code = code;
        self.code_raw = code_raw;
    }

    /// The plain text of a rendered line from the last frame (cache prefix
    /// plus streaming tail), if any — lets the history viewport re-find the
    /// line it was pinned to after a cache re-wrap.
    pub fn line_at(&self, li: usize) -> Option<&str> {
        self.lines.get(li).map(String::as_str)
    }

    /// The message each rendered line belonged to in the last frame, if any
    /// — lets the history viewport re-pin by message after a cache re-wrap.
    pub fn owner_at(&self, li: usize) -> Option<usize> {
        self.owner.get(li).copied().flatten()
    }

    /// Map a screen cell to a wrapped-line position, clamped to real content.
    pub fn pos_at(&self, col: u16, row: u16) -> Option<Pos> {
        if self.lines.is_empty() || !self.inner.contains(Position::new(col, row)) {
            return None;
        }
        let li = self.top + (row - self.inner.y) as usize;
        if li >= self.lines.len() {
            let last = self.lines.len() - 1;
            return Some((last, self.lines[last].chars().count()));
        }
        let max = self.lines[li].chars().count();
        Some((li, ((col - self.inner.x) as usize).min(max)))
    }

    /// The message index the active selection's anchor line belongs to, if
    /// any — used to resolve "the citation under the current selection" back
    /// to the message whose content holds the Sources list.
    pub fn owner_at_selection_start(&self) -> Option<usize> {
        let (anchor, _) = self.sel?;
        self.owner.get(anchor.0).copied().flatten()
    }

    pub fn on_down(&mut self, pos: Pos) {
        // Count this press against the *previous* click's time+place, so a
        // drag that follows immediately already knows it's part of a
        // double/triple click (see `click_count`'s doc comment).
        self.click_count = match self.last_click {
            Some((t, p, c)) if t.elapsed() <= MULTI_CLICK && p == pos => c + 1,
            _ => 1,
        };
        self.anchor = Some(pos);
        self.sel = None;
        self.down_at = Some(Instant::now());
        self.down_pos = pos;
        self.moved = false;
        self.long_pressed = false;
    }

    /// Extend the selection to `pos`. A drag following a double/triple click
    /// snaps to word/line boundaries instead of a plain char range, so
    /// dragging after a double-click selects whole words at a time.
    pub fn on_drag(&mut self, pos: Pos) {
        self.moved = true;
        let Some(a) = self.anchor else { return };
        self.sel = Some(match self.click_count {
            2 => self.word_extend(a, pos),
            n if n >= 3 => self.line_extend(a, pos),
            _ => (a, pos),
        });
    }

    /// Finish a press. `Action::Copy` for a drag-selection or double/triple
    /// click; `Action::OpenUrl` for a plain single click landing on a URL.
    pub fn on_up(&mut self, pos: Option<Pos>) -> Option<Action> {
        self.down_at = None;
        if self.long_pressed {
            self.long_pressed = false;
            return None; // already handled + copied by the timer
        }
        let pos = pos.unwrap_or(self.down_pos);
        self.last_click = Some((Instant::now(), pos, self.click_count));
        if self.moved {
            return self.selected_text().map(Action::Copy);
        }
        match self.click_count {
            2 => {
                self.select_word(pos);
                self.selected_text().map(Action::Copy)
            }
            n if n >= 3 => {
                self.select_line(pos);
                self.selected_text().map(Action::Copy)
            }
            _ => {
                self.sel = None; // single click clears
                self.url_at(pos).map(Action::OpenUrl)
            }
        }
    }

    /// The URL (if any) under `pos`, scanned from that line's plain text.
    pub fn url_at(&self, (line, col): Pos) -> Option<String> {
        let text = self.lines.get(line)?;
        scan_bare_urls(text)
            .into_iter()
            .find(|(range, _)| range.contains(&col))
            .map(|(_, url)| url)
    }

    /// When to wake the event loop to check for a long press (if a press is held
    /// still and unmoved).
    pub fn deadline(&self) -> Option<Instant> {
        match self.down_at {
            Some(t) if !self.moved && !self.long_pressed => Some(t + LONG_PRESS),
            _ => None,
        }
    }

    /// Fire the long press. On a code block, its raw code (already exact); over
    /// a message, that message's index — `App` resolves this to the message's
    /// exact original text rather than a wrap-reconstructed approximation.
    pub fn check_long_press(&mut self) -> Option<LongPress> {
        match self.down_at {
            Some(t) if !self.moved && !self.long_pressed && t.elapsed() >= LONG_PRESS => {
                self.long_pressed = true;
                let line = self.down_pos.0;
                if let Some(url) = self.url_at(self.down_pos) {
                    return Some(LongPress::Url(url));
                }
                if let Some(&Some(id)) = self.code.get(line) {
                    self.select_code_block(id);
                    return self.code_raw.get(id).cloned().map(LongPress::Code);
                }
                let msg = (*self.owner.get(line)?)?;
                self.select_message(self.down_pos);
                Some(LongPress::Message(msg))
            }
            _ => None,
        }
    }

    /// Select every line belonging to code block `id`.
    fn select_code_block(&mut self, id: usize) {
        let first = self.code.iter().position(|c| *c == Some(id));
        let last = self.code.iter().rposition(|c| *c == Some(id));
        if let (Some(first), Some(last)) = (first, last) {
            let len = self.lines.get(last).map_or(0, |l| l.chars().count());
            self.sel = Some(((first, 0), (last, len)));
        }
    }

    pub const fn clear(&mut self) {
        self.sel = None;
        self.anchor = None;
    }

    /// The word-boundary range (start, end) around `pos`, on its own line.
    fn word_bounds(&self, (line, col): Pos) -> (Pos, Pos) {
        let chars: Vec<char> = self
            .lines
            .get(line)
            .map(|l| l.chars().collect())
            .unwrap_or_default();
        let mut lo = col.min(chars.len());
        let mut hi = lo;
        while lo > 0 && !chars[lo - 1].is_whitespace() {
            lo -= 1;
        }
        while hi < chars.len() && !chars[hi].is_whitespace() {
            hi += 1;
        }
        ((line, lo), (line, hi))
    }

    fn select_word(&mut self, pos: Pos) {
        let (lo, hi) = self.word_bounds(pos);
        self.sel = (lo < hi).then_some((lo, hi));
    }

    fn select_line(&mut self, (line, _): Pos) {
        let len = self.lines.get(line).map_or(0, |l| l.chars().count());
        self.sel = Some(((line, 0), (line, len)));
    }

    /// Word-snapped range covering both `anchor`'s word and `pos`'s word, so
    /// dragging in either direction from a double-click keeps growing by
    /// whole words.
    fn word_extend(&self, anchor: Pos, pos: Pos) -> (Pos, Pos) {
        let (a_lo, a_hi) = self.word_bounds(anchor);
        let (p_lo, p_hi) = self.word_bounds(pos);
        (a_lo.min(p_lo), a_hi.max(p_hi))
    }

    /// Line-snapped range covering every line between `anchor` and `pos`.
    fn line_extend(&self, anchor: Pos, pos: Pos) -> (Pos, Pos) {
        let (lo_line, hi_line) = (anchor.0.min(pos.0), anchor.0.max(pos.0));
        let len = self.lines.get(hi_line).map_or(0, |l| l.chars().count());
        ((lo_line, 0), (hi_line, len))
    }

    /// Select every line belonging to the same message as `pos`.
    fn select_message(&mut self, (line, _): Pos) {
        let Some(&Some(msg)) = self.owner.get(line) else {
            return;
        };
        let first = self.owner.iter().position(|o| *o == Some(msg));
        let last = self.owner.iter().rposition(|o| *o == Some(msg));
        if let (Some(first), Some(last)) = (first, last) {
            let len = self.lines.get(last).map_or(0, |l| l.chars().count());
            self.sel = Some(((first, 0), (last, len)));
        }
    }

    /// The selected text (lines joined by newlines), or None if empty.
    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.ordered()?;
        let mut out = String::new();
        for li in a.0..=b.0 {
            let chars: Vec<char> = self.lines.get(li)?.chars().collect();
            let lo = if li == a.0 { a.1 } else { 0 };
            let hi = if li == b.0 { b.1 } else { chars.len() };
            let hi = hi.min(chars.len());
            if lo < hi {
                out.extend(&chars[lo..hi]);
            }
            if li < b.0 {
                out.push('\n');
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// Restyle `line` (at absolute index `li`) with the selection highlight, or
    /// None if the selection doesn't touch this line.
    pub fn highlight(&self, li: usize, line: &Line) -> Option<Line<'static>> {
        let (a, b) = self.ordered()?;
        if li < a.0 || li > b.0 {
            return None;
        }
        let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        let lo = if li == a.0 { a.1 } else { 0 };
        let hi = if li == b.0 { b.1 } else { total };
        if lo >= hi {
            return None;
        }
        let hl = Style::default().bg(Color::Blue).fg(Color::White);
        let mut out: Vec<(char, Style)> = Vec::new();
        let mut idx = 0;
        for sp in &line.spans {
            for c in sp.content.chars() {
                let st = if idx >= lo && idx < hi { hl } else { sp.style };
                out.push((c, st));
                idx += 1;
            }
        }
        Some(regroup(out))
    }

    fn ordered(&self) -> Option<(Pos, Pos)> {
        let (a, b) = self.sel?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }
}

/// Find `http(s)://...` runs in `text`, as `(char range, url)`. A URL extends
/// to the next whitespace, then trims trailing punctuation that's usually
/// sentence structure, not part of the link (`.`, `,`, `)`, `]`, etc.).
fn scan_bare_urls(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let mut end = i;
            while end < chars.len() && !chars[end].is_whitespace() {
                end += 1;
            }
            let mut trimmed_end = end;
            while trimmed_end > i
                && matches!(
                    chars[trimmed_end - 1],
                    '.' | ',' | ')' | ']' | '>' | ':' | ';' | '"' | '\''
                )
            {
                trimmed_end -= 1;
            }
            if trimmed_end > i {
                let url: String = chars[i..trimmed_end].iter().collect();
                hits.push((i..trimmed_end, url));
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    hits
}

/// Merge a run of styled chars back into a `Line` with minimal spans.
fn regroup(row: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<Style> = None;
    for (c, st) in row {
        if cur == Some(st) {
            buf.push(c);
        } else {
            if let Some(s) = cur {
                spans.push(Span::styled(std::mem::take(&mut buf), s));
            }
            buf.push(c);
            cur = Some(st);
        }
    }
    if let Some(s) = cur {
        spans.push(Span::styled(buf, s));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(
        lines: &[&str],
        owner: &[Option<usize>],
        code: &[Option<usize>],
        raw: &[&str],
    ) -> HistorySel {
        let mut s = HistorySel::default();
        s.record_render(
            Rect::new(0, 0, 40, 10),
            0,
            lines.iter().map(std::string::ToString::to_string).collect(),
            owner.to_vec(),
            code.to_vec(),
            raw.iter().map(std::string::ToString::to_string).collect(),
        );
        s
    }

    fn sel_with(lines: &[&str]) -> HistorySel {
        build(
            lines,
            &vec![Some(0); lines.len()],
            &vec![None; lines.len()],
            &[],
        )
    }

    fn with_owner(lines: &[&str], owner: &[Option<usize>]) -> HistorySel {
        build(lines, owner, &vec![None; lines.len()], &[])
    }

    #[test]
    fn owner_at_selection_start_resolves_the_anchor_lines_message() {
        let mut s = with_owner(&["line a", "line b"], &[Some(0), Some(1)]);
        s.on_down((1, 0));
        s.on_drag((1, 3));
        assert_eq!(s.owner_at_selection_start(), Some(1));
        s.sel = None;
        assert_eq!(s.owner_at_selection_start(), None);
    }

    #[test]
    fn drag_selects_range_across_lines() {
        let mut s = sel_with(&["hello world", "foo bar"]);
        assert_eq!(s.pos_at(6, 0), Some((0, 6))); // click maps to line 0 col 6
        s.on_down((0, 0));
        s.on_drag((0, 5));
        assert_eq!(s.selected_text().as_deref(), Some("hello"));
        // Backwards drag still yields ordered text; spanning lines joins with \n.
        s.on_down((1, 3));
        s.on_drag((0, 6));
        assert_eq!(s.selected_text().as_deref(), Some("world\nfoo"));
    }

    #[test]
    fn double_click_word_triple_click_line() {
        let mut s = sel_with(&["hello world"]);
        // Single click: no selection.
        s.on_down((0, 2));
        assert!(s.on_up(Some((0, 2))).is_none());
        // Second click at the same cell -> word.
        s.on_down((0, 2));
        assert_eq!(copy_of(s.on_up(Some((0, 2)))), Some("hello".to_string()));
        // Third click -> whole line.
        s.on_down((0, 2));
        assert_eq!(
            copy_of(s.on_up(Some((0, 2)))),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn drag_after_double_click_extends_word_by_word() {
        let mut s = sel_with(&["foo bar baz qux"]);
        // First click primes the double-click; second click starts the drag.
        s.on_down((0, 6)); // "bar" in "foo bar baz qux"
        s.on_up(Some((0, 6)));
        s.on_down((0, 6));
        s.on_drag((0, 10)); // dragged into "baz"
        assert_eq!(s.selected_text().as_deref(), Some("bar baz"));
        // Dragging back the other way (past the anchor word) still snaps to words.
        s.on_drag((0, 1)); // dragged into "foo"
        assert_eq!(s.selected_text().as_deref(), Some("foo bar"));
    }

    #[test]
    fn drag_after_triple_click_extends_line_by_line() {
        let mut s = sel_with(&["one", "two", "three"]);
        s.on_down((0, 1));
        s.on_up(Some((0, 1)));
        s.on_down((0, 1));
        s.on_up(Some((0, 1)));
        s.on_down((0, 1));
        s.on_drag((1, 2));
        assert_eq!(s.selected_text().as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn single_click_drag_is_still_plain_char_range() {
        let mut s = sel_with(&["hello world"]);
        s.on_down((0, 0));
        s.on_drag((0, 5));
        assert_eq!(s.selected_text().as_deref(), Some("hello"));
    }

    fn copy_of(a: Option<Action>) -> Option<String> {
        match a {
            Some(Action::Copy(t)) => Some(t),
            _ => None,
        }
    }

    #[test]
    fn long_press_selects_only_its_message() {
        // Two messages: lines 0-1 -> msg 0, lines 2-3 -> msg 1.
        let mut s = with_owner(
            &["a", "bb", "cc", "d"],
            &[Some(0), Some(0), Some(1), Some(1)],
        );
        s.select_message((2, 0));
        assert_eq!(s.selected_text().as_deref(), Some("cc\nd"));
    }

    #[test]
    fn scan_bare_urls_trims_trailing_punctuation() {
        let hits = scan_bare_urls("see https://example.com/a, and (https://x.io/b).");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].1, "https://example.com/a");
        assert_eq!(hits[1].1, "https://x.io/b");
    }

    #[test]
    fn url_at_finds_url_under_click() {
        let s = sel_with(&["go to https://example.com/x now"]);
        // "https://example.com/x" starts at char index 6.
        assert_eq!(s.url_at((0, 10)).as_deref(), Some("https://example.com/x"));
        assert_eq!(s.url_at((0, 2)), None);
    }

    #[test]
    fn recording_an_empty_layout_invalidates_stale_lines() {
        let mut s = sel_with(&["go to https://example.com/x now"]);
        assert!(s.pos_at(6, 0).is_some());
        // The welcome screen (fresh /new session) re-records an empty layout:
        // stale lines from the previous session must not stay clickable — a
        // plain click on them would open the old session's URL.
        s.record_render(
            Rect::default(),
            0,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(s.pos_at(6, 0), None);
        assert_eq!(s.url_at((0, 10)), None);
    }

    #[test]
    fn click_on_url_opens_instead_of_clearing_selection() {
        let mut s = sel_with(&["see https://example.com/x here"]);
        s.on_down((0, 6));
        match s.on_up(Some((0, 6))) {
            Some(Action::OpenUrl(u)) => assert_eq!(u, "https://example.com/x"),
            _ => panic!("expected OpenUrl"),
        }
    }

    #[test]
    fn select_code_block_highlights_lines_and_has_raw() {
        // Lines 1-3 are code block 0 (top border, code, bottom border).
        let mut s = build(
            &["intro", "┌──┐", "│ x │", "└──┘"],
            &[Some(0), Some(0), Some(0), Some(0)],
            &[None, Some(0), Some(0), Some(0)],
            &["let x = 1;"],
        );
        s.select_code_block(0);
        // Highlight covers the whole box (lines 1..=3).
        assert_eq!(s.ordered(), Some(((1, 0), (3, 4))));
        // The raw code (what a long-press copies) is stored verbatim.
        assert_eq!(s.code_raw[0], "let x = 1;");
    }
}
