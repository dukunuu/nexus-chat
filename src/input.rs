//! The message composer: text editing, OS-clipboard cut/copy/paste, and
//! slash-command autocomplete. `App` owns the `TextArea`; this module holds
//! everything that operates on it.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::style::{Color, Style};
use tui_textarea::{CursorMove, TextArea};

use crate::app::App;

const COMPOSER_MULTI_CLICK: Duration = Duration::from_millis(400);

/// A slash command: canonical `name`, a short (≤20 char) `desc`, and alias
/// keywords. Names, aliases, and the description are all fuzzy-searchable, so
/// typing `/history` surfaces `session`.
pub struct Command {
    pub name: &'static str,
    pub desc: &'static str,
    pub aliases: &'static [&'static str],
}

/// One row of the slash-command autocomplete: either a static builtin or an
/// installed skill (`/<skill-name>` forces it, same as `/model` etc.).
pub enum Match {
    Builtin(&'static Command),
    Skill { name: String, desc: String },
}

impl Match {
    pub fn name(&self) -> &str {
        match self {
            Match::Builtin(c) => c.name,
            Match::Skill { name, .. } => name,
        }
    }

    pub fn desc(&self) -> &str {
        match self {
            Match::Builtin(c) => c.desc,
            Match::Skill { desc, .. } => desc,
        }
    }
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "new",
        desc: "start new chat",
        aliases: &["chat", "clear"],
    },
    Command {
        name: "compact",
        desc: "summarize old messages",
        aliases: &["compaction", "summarize"],
    },
    Command {
        name: "session",
        desc: "switch sessions",
        aliases: &["sessions", "history", "resume", "continue", "switch"],
    },
    Command {
        name: "space",
        desc: "switch spaces",
        aliases: &["spaces", "project", "workspace"],
    },
    Command {
        name: "model",
        desc: "pick a model",
        aliases: &["models", "llm"],
    },
    Command {
        name: "login",
        desc: "pick a backend to log into",
        aliases: &[
            "key",
            "apikey",
            "token",
            "auth",
            "codex",
            "subscription",
            "oauth",
            "chatgpt",
            "opencode",
        ],
    },
    Command {
        name: "swarm",
        desc: "multi-persona roundtable roster",
        aliases: &["swarms", "personas", "panel"],
    },
    Command {
        name: "config",
        desc: "settings & stats",
        aliases: &["settings", "stats", "nerd", "params"],
    },
    Command {
        name: "skills",
        desc: "manage skills",
        aliases: &["addskill"],
    },
    Command {
        name: "files",
        desc: "space files",
        aliases: &["file", "attach", "upload", "docs"],
    },
    Command {
        name: "apps",
        desc: "view space apps",
        aliases: &["app", "webapps"],
    },
    Command {
        name: "research",
        desc: "deep multi-agent research (blank = scope topic from this chat)",
        aliases: &["deep-research"],
    },
    Command {
        name: "export",
        desc: "write session's report + sources to a file",
        aliases: &["save-report"],
    },
    Command {
        name: "watch",
        desc: "standing research, re-runs every 24h",
        aliases: &["watches"],
    },
    Command {
        name: "web",
        desc: "toggle web answer mode (search-first, cited)",
        aliases: &["websearch"],
    },
    Command {
        name: "steer",
        desc: "inject a research instruction mid-flight",
        aliases: &["nudge"],
    },
    Command {
        name: "stop",
        desc: "stop active response, research, or swarm",
        aliases: &["cancel", "abort"],
    },
    Command {
        name: "edit",
        desc: "open app file in $EDITOR",
        aliases: &["open"],
    },
    Command {
        name: "copy",
        desc: "copy last reply",
        aliases: &["yank", "clip"],
    },
    Command {
        name: "help",
        desc: "list commands",
        aliases: &["commands"],
    },
    Command {
        name: "quit",
        desc: "exit the app",
        aliases: &["q", "exit"],
    },
];

/// Subsequence fuzzy score, case-insensitive. `None` if `needle` isn't a
/// subsequence of `hay`; higher is a better match (bonuses for contiguous runs
/// and matching at the start).
pub(crate) fn fuzzy_score(hay: &str, needle: &str) -> Option<i32> {
    let hay = hay.to_lowercase();
    let needle = needle.to_lowercase();
    let mut chars = hay.chars();
    let mut score = 0i32;
    let mut prev_matched = false;
    let mut pos = 0i32;
    for nc in needle.chars() {
        loop {
            let hc = chars.next()?;
            if hc == nc {
                score += 1;
                if prev_matched {
                    score += 2;
                }
                if pos == 0 {
                    score += 3;
                }
                prev_matched = true;
                pos += 1;
                break;
            }
            prev_matched = false;
            pos += 1;
        }
    }
    Some(score)
}

/// Best fuzzy score of `needle` across a command's name/aliases/desc, with the
/// name weighted highest and the description lowest.
fn command_score(c: &Command, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let mut best: Option<i32> = None;
    let mut upd = |s: &str, bonus: i32| {
        if let Some(sc) = fuzzy_score(s, needle) {
            let v = sc + bonus;
            best = Some(best.map_or(v, |b| b.max(v)));
        }
    };
    upd(c.name, 100);
    for a in c.aliases {
        upd(a, 50);
    }
    upd(c.desc, 0);
    best
}

/// A composer TextArea with sane styling: no underlined cursor line, and a
/// selection highlight that keeps the text readable (default is a blank white bg).
pub(crate) fn new_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_cursor_line_style(Style::default());
    ta.set_selection_style(Style::default().bg(Color::Blue).fg(Color::White));
    // Soft-wrap long lines and grow up to 20 rows like an HTML textarea.
    ta.set_wrap_mode(tui_textarea::WrapMode::WordOrGlyph);
    ta.set_max_rows(20);
    ta
}

/// Copy `text` to the OS clipboard and format the status-line message for
/// it: `"copied {n} chars"` on success, `"clipboard unavailable"` if there's
/// no clipboard or the set failed, or an empty string if `text` was empty
/// (callers should leave the existing status untouched in that case).
pub(crate) fn copy_to_clipboard(clipboard: &mut Option<arboard::Clipboard>, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let n = text.chars().count();
    if clipboard
        .as_mut()
        .is_some_and(|cb| cb.set_text(text.to_string()).is_ok())
    {
        format!("copied {n} chars")
    } else {
        "clipboard unavailable".to_string()
    }
}

/// If pasted text is a single absolute path to an existing regular file
/// (optionally `file://`-prefixed or quoted, as file managers produce on
/// drag/drop), return the cleaned path.
fn pasted_file_path(text: &str) -> Option<std::path::PathBuf> {
    let t = text.trim().trim_matches('"').trim_matches('\'');
    let t = t.strip_prefix("file://").unwrap_or(t);
    if t.contains('\n') || !t.starts_with('/') {
        return None;
    }
    let path = std::path::PathBuf::from(t);
    path.is_file().then_some(path)
}

impl App {
    /// Current composer text, newlines joined.
    pub fn input_text(&self) -> String {
        self.input.lines().join("\n")
    }

    /// Replace the composer contents, cursor at end.
    pub fn set_input(&mut self, text: &str) {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map(|l| l.chars().count()).unwrap_or(0);
        self.input.set_lines(lines, (row, col));
    }

    pub(crate) fn clear_input(&mut self) {
        self.input = new_textarea();
    }

    /// Copy the current selection to the OS clipboard, then clear the highlight
    /// (so a following Ctrl+C quits, and the cleared highlight signals success).
    pub fn copy_selection(&mut self) {
        self.input.copy();
        let text = self.input.yank_text();
        self.input.cancel_selection();
        let msg = copy_to_clipboard(&mut self.clipboard, &text);
        if !msg.is_empty() {
            self.status = msg;
        }
    }

    /// Copy the current selection to the OS clipboard *without* clearing it —
    /// `TextArea::copy()` clears the selection as a side effect, which is
    /// fine for an explicit copy but wrong for keeping the clipboard synced
    /// while a keyboard (Shift+arrow) selection is still growing.
    pub fn copy_selection_live(&mut self) {
        let Some(((r1, c1), (r2, c2))) = self.input.selection_range() else {
            return;
        };
        let lines = self.input.lines();
        let text = if r1 == r2 {
            lines[r1].chars().skip(c1).take(c2 - c1).collect::<String>()
        } else {
            let mut out = lines[r1].chars().skip(c1).collect::<String>();
            for line in &lines[r1 + 1..r2] {
                out.push('\n');
                out.push_str(line);
            }
            out.push('\n');
            out.push_str(&lines[r2].chars().take(c2).collect::<String>());
            out
        };
        let msg = copy_to_clipboard(&mut self.clipboard, &text);
        if !msg.is_empty() {
            self.status = msg;
        }
    }

    /// Cut the current selection to the OS clipboard.
    pub fn cut_selection(&mut self) {
        if self.input.cut()
            && let Some(cb) = self.clipboard.as_mut()
        {
            let _ = cb.set_text(self.input.yank_text());
        }
    }

    /// Paste `text` into whatever's focused right now: the composer, or the
    /// currently-open popup's text field, if it has one. Used for both
    /// bracketed paste and an explicit Ctrl+V (some terminals send neither
    /// reliably for every popup, so both paths funnel through here).
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        use crate::app::{Popup, SessionMode, SkillsMode, SpaceMode};
        match self.popup {
            Popup::None => {
                // A dropped/pasted file path becomes an import offer instead of text.
                if let Some(path) = pasted_file_path(text) {
                    self.open_files_popup();
                    self.start_files_add();
                    self.files_edit = path.to_string_lossy().to_string();
                    self.status = "import this file? Enter to confirm · Esc to cancel".to_string();
                    return;
                }
                self.input.insert_str(text);
            }
            Popup::Key => self.key_input.push_str(text),
            Popup::Session if self.session_mode == SessionMode::Rename => {
                self.session_edit.push_str(text)
            }
            Popup::Space if matches!(self.space_mode, SpaceMode::Create | SpaceMode::Rename) => {
                self.space_edit.push_str(text);
            }
            Popup::Skills if self.skills_mode == SkillsMode::Install => {
                self.skills_edit.push_str(text)
            }
            Popup::Files if self.files_mode == crate::app::FilesMode::Add => {
                self.files_edit.push_str(text)
            }
            Popup::Files if self.files_mode == crate::app::FilesMode::Pick => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    self.picker_filter_push(c);
                }
            }
            Popup::Settings => {
                if let Some(i) = self.text_index() {
                    use crate::app::SettingsField;
                    let numeric = !matches!(
                        self.settings_field(),
                        Some(SettingsField::SearxngUrl)
                            | Some(SettingsField::LangsearchKey)
                            | Some(SettingsField::EmbeddingModel)
                            | Some(SettingsField::BlockedDomains)
                    );
                    let filtered: String = if numeric {
                        text.chars()
                            .filter(|c| c.is_ascii_digit() || *c == '.')
                            .collect()
                    } else {
                        text.chars().filter(|c| !c.is_control()).collect()
                    };
                    self.settings_inputs[i].push_str(&filtered);
                }
            }
            _ => {}
        }
    }

    /// Read `text` from the OS clipboard and paste it into whatever's focused
    /// (Ctrl+V fallback for terminals that don't send bracketed paste).
    pub fn paste_from_clipboard(&mut self) {
        // An image on the clipboard (screenshot, copied picture) beats text —
        // but only for the composer; popup fields are text-only.
        if self.popup == crate::app::Popup::None
            && let Some(img) = self.clipboard.as_mut().and_then(|cb| cb.get_image().ok())
        {
            self.attach_clipboard_image(img);
            return;
        }
        let Some(cb) = self.clipboard.as_mut() else {
            return;
        };
        let Ok(text) = cb.get_text() else { return };
        self.paste(&text);
    }

    // --- composer double/triple-click, and word/line-drag extension ---

    /// Register a press at `screen_pos` (raw column/row) against the previous
    /// one, returning the click count (2 = double, 3+ = triple+) so the mouse
    /// handler knows whether to word/line-select and, if a drag follows,
    /// which granularity to extend by.
    pub fn composer_click_down(&mut self, screen_pos: (u16, u16)) -> u8 {
        let count = match self.composer_click {
            Some((t, p)) if t.elapsed() <= COMPOSER_MULTI_CLICK && p == screen_pos => {
                self.composer_click_count + 1
            }
            _ => 1,
        };
        self.composer_click = Some((Instant::now(), screen_pos));
        self.composer_click_count = count;
        count
    }

    /// Select the word at the cursor (already placed via a screen-to-data
    /// jump) and remember its start as the drag anchor.
    pub fn select_composer_word(&mut self) {
        self.input.cancel_selection();
        self.input.move_cursor(CursorMove::WordBack);
        self.composer_word_anchor = Some(self.input.cursor());
        self.input.start_selection();
        // WordEnd lands ON the word's last char (inclusive); nudge forward
        // one so the selection range (which is exclusive) includes it.
        self.input.move_cursor(CursorMove::WordEnd);
        self.input.move_cursor(CursorMove::Forward);
    }

    /// Select the whole line at the cursor.
    pub fn select_composer_line(&mut self) {
        self.input.cancel_selection();
        self.input.move_cursor(CursorMove::Head);
        self.composer_word_anchor = Some(self.input.cursor());
        self.input.start_selection();
        self.input.move_cursor(CursorMove::End);
    }

    /// Extend a word-mode drag: re-select from the anchor word out to
    /// whichever word the cursor (already jumped to the drag position) is
    /// over now, snapping both ends to word boundaries.
    pub fn extend_composer_word_selection(&mut self) {
        let Some(anchor) = self.composer_word_anchor else {
            return;
        };
        let cur = self.input.cursor();
        self.input.cancel_selection();
        if cur >= anchor {
            // `anchor` is already the exact start of its word (stored right
            // after a `WordBack` in `select_composer_word`) — don't jump
            // back again, or a boundary-aligned anchor would skip to the
            // *previous* word's start (`WordBack` from an exact word-start
            // lands on the prior word, matching typical vim `b` semantics).
            self.jump_cursor(anchor);
            self.input.start_selection();
            self.jump_cursor(cur);
            self.input.move_cursor(CursorMove::WordEnd);
            self.input.move_cursor(CursorMove::Forward); // inclusive -> exclusive
        } else {
            self.jump_cursor(anchor);
            self.input.move_cursor(CursorMove::WordEnd);
            self.input.move_cursor(CursorMove::Forward); // inclusive -> exclusive
            self.input.start_selection();
            self.jump_cursor(cur);
            self.input.move_cursor(CursorMove::WordBack);
        }
    }

    /// Extend a line-mode drag: re-select every full line between the anchor
    /// line and wherever the cursor (already jumped to the drag position) is.
    pub fn extend_composer_line_selection(&mut self) {
        let Some(anchor) = self.composer_word_anchor else {
            return;
        };
        let cur = self.input.cursor();
        self.input.cancel_selection();
        if cur >= anchor {
            self.jump_cursor((anchor.0, 0));
            self.input.start_selection();
            self.jump_cursor(cur);
            self.input.move_cursor(CursorMove::End);
        } else {
            self.jump_cursor(anchor);
            self.input.move_cursor(CursorMove::End);
            self.input.start_selection();
            self.jump_cursor((cur.0, 0));
        }
    }

    fn jump_cursor(&mut self, (row, col): (usize, usize)) {
        self.input
            .move_cursor(CursorMove::Jump(row as u16, col as u16));
    }

    // --- slash command autocomplete ---

    /// Fuzzy-ranked command suggestions for the current composer text. Empty
    /// unless the text is a bare `/token` still being typed (no space yet).
    /// Merges built-in commands with installed skills, so `/some-skill`
    /// autocompletes and forces the skill exactly like a builtin.
    pub fn command_matches(&self) -> Vec<Match> {
        let text = self.input_text();
        let Some(rest) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if rest.contains(char::is_whitespace) {
            return Vec::new();
        }
        let mut scored: Vec<(i32, Match)> = COMMANDS
            .iter()
            .filter_map(|c| command_score(c, rest).map(|s| (s, Match::Builtin(c))))
            .collect();
        scored.extend(self.skills.iter().filter_map(|s| {
            if rest.is_empty() {
                return Some((
                    0,
                    Match::Skill {
                        name: s.name.clone(),
                        desc: s.description.clone(),
                    },
                ));
            }
            let name_score = fuzzy_score(&s.name, rest).map(|sc| sc + 100);
            let desc_score = fuzzy_score(&s.description, rest);
            name_score.into_iter().chain(desc_score).max().map(|score| {
                (
                    score,
                    Match::Skill {
                        name: s.name.clone(),
                        desc: s.description.clone(),
                    },
                )
            })
        }));
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name().cmp(b.1.name())));
        scored.into_iter().map(|(_, m)| m).collect()
    }

    /// Highlighted suggestion index, clamped to the current match count.
    pub fn command_selected(&self) -> usize {
        let n = self.command_matches().len();
        if n == 0 {
            0
        } else {
            self.cmd_selected.min(n - 1)
        }
    }

    pub fn move_command_selection(&mut self, delta: i32) {
        let n = self.command_matches().len() as i32;
        if n == 0 {
            return;
        }
        self.cmd_selected = (self.command_selected() as i32 + delta).rem_euclid(n) as usize;
    }

    /// Accept the highlighted suggestion. `run` = execute it now (Enter);
    /// otherwise just fill it into the composer (Tab).
    pub fn accept_command(&mut self, run: bool) -> Result<()> {
        let matches = self.command_matches();
        let Some(idx) = matches
            .get(self.command_selected())
            .map(|_| self.command_selected())
        else {
            return Ok(());
        };
        let name = matches[idx].name().to_string();
        self.cmd_selected = 0;
        if run {
            self.clear_input();
            self.run_command(&name)?;
        } else {
            self.set_input(&format!("/{name} "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let space = Space {
            root: std::env::temp_dir().join(format!("nexus-input-test-{}", uuid::Uuid::new_v4())),
        };
        App::new(db, Some("k".into()), space)
    }

    #[test]
    fn double_click_selects_word_and_remembers_anchor() {
        let mut a = test_app();
        a.set_input("foo bar baz");
        a.input.move_cursor(CursorMove::Jump(0, 5)); // inside "bar"
        a.select_composer_word();
        assert!(a.input.is_selecting());
        a.input.copy();
        assert_eq!(a.input.yank_text(), "bar");
    }

    #[test]
    fn triple_click_selects_whole_line() {
        let mut a = test_app();
        a.set_input("foo bar baz");
        a.input.move_cursor(CursorMove::Jump(0, 5));
        a.select_composer_line();
        a.input.copy();
        assert_eq!(a.input.yank_text(), "foo bar baz");
    }

    #[test]
    fn drag_after_double_click_extends_word_by_word() {
        let mut a = test_app();
        a.set_input("foo bar baz qux");
        a.input.move_cursor(CursorMove::Jump(0, 5)); // "bar"
        a.select_composer_word();
        a.input.move_cursor(CursorMove::Jump(0, 9)); // into "baz"
        a.extend_composer_word_selection();
        a.input.copy();
        assert_eq!(a.input.yank_text(), "bar baz");
    }

    #[test]
    fn drag_before_double_click_anchor_extends_backward_word_by_word() {
        let mut a = test_app();
        a.set_input("foo bar baz qux");
        a.input.move_cursor(CursorMove::Jump(0, 9)); // "baz"
        a.select_composer_word();
        a.input.move_cursor(CursorMove::Jump(0, 5)); // dragged back into "bar"
        a.extend_composer_word_selection();
        a.input.copy();
        assert_eq!(a.input.yank_text(), "bar baz");
    }

    #[test]
    fn composer_click_down_counts_rapid_same_pos_clicks() {
        let mut a = test_app();
        assert_eq!(a.composer_click_down((5, 0)), 1);
        assert_eq!(a.composer_click_down((5, 0)), 2);
        assert_eq!(a.composer_click_down((5, 0)), 3);
        // A different position resets the count.
        assert_eq!(a.composer_click_down((6, 0)), 1);
    }

    #[test]
    fn paste_inserts_into_composer_when_no_popup_open() {
        let mut a = test_app();
        a.set_input("hello ");
        a.paste("world");
        assert_eq!(a.input_text(), "hello world");
    }

    #[test]
    fn paste_goes_into_key_popup_field() {
        let mut a = test_app();
        a.popup = crate::app::Popup::Key;
        a.paste("sk-or-abc123");
        assert_eq!(a.key_input, "sk-or-abc123");
    }

    #[test]
    fn paste_goes_into_files_add_field() {
        let mut a = test_app();
        a.popup = crate::app::Popup::Files;
        a.files_mode = crate::app::FilesMode::Add;
        a.paste("/tmp/report.pdf");
        assert_eq!(a.files_edit, "/tmp/report.pdf");
    }

    #[test]
    fn paste_in_picker_mode_feeds_the_filter() {
        let mut a = test_app();
        a.popup = crate::app::Popup::Files;
        a.files_mode = crate::app::FilesMode::Pick;
        a.paste("doc");
        assert_eq!(a.picker_filter, "doc");
    }

    #[test]
    fn paste_into_numeric_settings_field_filters_non_numeric_chars() {
        let mut a = test_app();
        a.popup = crate::app::Popup::Settings;
        a.settings_selected = 6; // Temperature (numeric)
        a.paste("0.7abc");
        assert_eq!(a.settings_inputs[0], "0.7");
    }

    #[test]
    fn paste_into_url_settings_field_keeps_full_text() {
        let mut a = test_app();
        a.popup = crate::app::Popup::Settings;
        a.settings_selected = 18; // SearxngUrl (free text)
        a.paste("http://localhost:8080");
        assert_eq!(a.settings_inputs[4], "http://localhost:8080");
    }

    #[test]
    fn pasting_a_file_path_offers_import() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-paste-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&src, "x").unwrap();

        a.paste(&src.to_string_lossy());
        assert!(a.popup == crate::app::Popup::Files);
        assert!(a.files_mode == crate::app::FilesMode::Add);
        assert_eq!(a.files_edit, src.to_string_lossy());
        assert!(a.input_text().is_empty()); // path did not land in the composer

        // file:// URIs and quoted paths (file-manager drag/drop) work too.
        let mut a = test_app();
        a.paste(&format!("file://{}", src.to_string_lossy()));
        assert!(a.popup == crate::app::Popup::Files);

        // Ordinary text is untouched.
        let mut a = test_app();
        a.paste("/not/a/real/path and some prose");
        assert!(a.popup == crate::app::Popup::None);
        assert_eq!(a.input_text(), "/not/a/real/path and some prose");
    }
}
