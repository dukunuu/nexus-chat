//! The composer half of the old core `input.rs` (2e split): everything that
//! operates on the view-owned `TextArea` — text editing, OS-clipboard
//! cut/copy/paste, slash-command autocomplete, and `@`-file autocomplete.
//! The pure command catalog (`COMMANDS`, `fuzzy_score`) stayed in core
//! (`nexus_core::app::commands`).

// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::style::{Color, Style};
use tui_textarea::{CursorMove, TextArea};

use nexus_core::app::{AppCommand, Match, Popup};

use crate::app_view::AppView;

const COMPOSER_MULTI_CLICK: Duration = Duration::from_millis(400);

/// A composer `TextArea` with sane styling: no underlined cursor line, and a
/// selection highlight that keeps the text readable (default is a blank white bg).
pub fn new_textarea() -> TextArea<'static> {
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

impl AppView {
    /// Current composer text, newlines joined.
    pub fn input_text(&self) -> String {
        self.input.lines().join("\n")
    }

    /// Replace the composer contents, cursor at end.
    pub fn set_input(&mut self, text: &str) {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map_or(0, |l| l.chars().count());
        self.input.set_lines(lines, (row, col));
    }

    pub fn clear_input(&mut self) {
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
            self.push_status(msg);
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
            self.push_status(msg);
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
        use nexus_core::app::{AppsMode, FilesMode, SessionMode, SkillsMode, SpaceMode};
        if text.is_empty() {
            return;
        }
        match self.popup {
            Popup::None => {
                // A dropped/pasted file path becomes an import offer instead of text.
                if let Some(path) = pasted_file_path(text) {
                    self.open_files_popup(nexus_core::app::FilesTab::Files);
                    self.start_files_add();
                    self.files_edit = path.to_string_lossy().to_string();
                    self.push_status(
                        "import this file? Enter to confirm · Esc to cancel".to_string(),
                    );
                    return;
                }
                self.input.insert_str(text);
            }
            Popup::Key => self.key_input.push_str(text),
            Popup::Session if self.session_mode == SessionMode::Rename => {
                self.session_edit.push_str(text);
            }
            Popup::Space if matches!(self.space_mode, SpaceMode::Create | SpaceMode::Rename) => {
                self.space_edit.push_str(text);
            }
            Popup::Skills if self.skills_mode == SkillsMode::Install => {
                self.skills_edit.push_str(text);
            }
            Popup::Apps if self.apps_mode == AppsMode::EditFile => self.apps_edit.push_str(text),
            Popup::Files if self.files_mode == FilesMode::Add => {
                self.files_edit.push_str(text);
            }
            Popup::Files
                if self.files_tab == nexus_core::app::FilesTab::Scripts
                    && self.scripts_mode == nexus_core::app::ScriptsMode::Create =>
            {
                self.scripts_edit.push_str(text);
            }
            Popup::Files if self.files_mode == FilesMode::Pick => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    self.picker_filter_push(c);
                }
            }
            Popup::Settings => {
                if let Some(i) = self.text_index() {
                    use nexus_core::app::SettingsField;
                    let numeric = !matches!(
                        self.settings_field(),
                        Some(
                            SettingsField::SearxngUrl
                                | SettingsField::LangsearchKey
                                | SettingsField::EmbeddingModel
                                | SettingsField::BlockedDomains
                        )
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
        if self.popup == Popup::None
            && let Some(img) = self.clipboard.as_mut().and_then(|cb| cb.get_image().ok())
        {
            if let Some(md) = self.save_clipboard_image(img.width, img.height, &img.bytes) {
                self.input.insert_str(&md);
                self.push_status("image attached as markdown".to_string());
            }
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
        // `anchor` is already the exact start of its word (stored right
        // after a `WordBack` in `select_composer_word`) — don't jump
        // back again, or a boundary-aligned anchor would skip to the
        // *previous* word's start (`WordBack` from an exact word-start
        // lands on the prior word, matching typical vim `b` semantics).
        self.jump_cursor(anchor);
        if cur >= anchor {
            self.input.start_selection();
            self.jump_cursor(cur);
            self.input.move_cursor(CursorMove::WordEnd);
            self.input.move_cursor(CursorMove::Forward); // inclusive -> exclusive
        } else {
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
    pub fn command_matches(&self) -> Vec<Match> {
        let text = self.input_text();
        let Some(rest) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if rest.contains(char::is_whitespace) {
            return Vec::new();
        }
        let mut scored: Vec<(i32, Match)> = nexus_core::app::COMMANDS
            .iter()
            .filter_map(|c| nexus_core::app::command_score(c, rest).map(|s| (s, Match::Builtin(c))))
            .collect();
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

    /// Find `@` before the cursor and match against space files.
    /// Returns the query text after `@` (owned) and the byte offset of `@`.
    fn at_query(&self) -> Option<(String, usize)> {
        let text = self.input_text();
        let cursor = self.input.cursor();
        // tui-textarea cursor is (row, col). Single-line input → col is byte offset.
        let pos = cursor.1;
        let before = text.get(..pos)?;
        let at = before.rfind('@')?;
        let rest = &text[at + 1..pos];
        // Only match if there's no whitespace or `/` between @ and cursor.
        if rest.contains(char::is_whitespace) || rest.contains('/') {
            return None;
        }
        Some((rest.to_string(), at))
    }

    /// Compute @-autocomplete matches from the space's file cache.
    pub fn refresh_at_matches(&mut self) {
        let Some((query, at_offset)) = self.at_query() else {
            self.at_state = None;
            return;
        };
        if query.is_empty() {
            // Show all files when @ is typed with no query yet
            let all = self.files_cache.clone();
            if all.is_empty() {
                self.at_state = None;
                return;
            }
            self.at_state = Some((all, 0, at_offset));
            return;
        }
        let lower = query.to_lowercase();
        let mut scored: Vec<(i32, &nexus_core::db::FileRow)> = self
            .files_cache
            .iter()
            .filter_map(|f| {
                let name_lower = f.name.to_lowercase();
                let score = if name_lower.starts_with(&lower) {
                    100 - (name_lower.len() as i32)
                } else if let Some(idx) = name_lower.find(&lower) {
                    50 - idx as i32
                } else if nexus_core::app::fuzzy_score(&query, &f.name).is_some() {
                    10
                } else {
                    return None;
                };
                Some((score, f))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        let matches: Vec<nexus_core::db::FileRow> =
            scored.into_iter().map(|(_, f)| f.clone()).collect();
        if matches.is_empty() {
            self.at_state = None;
            return;
        }
        let selected = self
            .at_state
            .as_ref()
            .map_or(0, |s| s.1.min(matches.len().saturating_sub(1)));
        self.at_state = Some((matches, selected, at_offset));
    }

    /// Accept the highlighted @-autocomplete match: replace `@<query>` with the filename.
    pub fn accept_at_match(&mut self) {
        let Some((ref matches, selected, at_offset)) = self.at_state.clone() else {
            return;
        };
        let Some(f) = matches.get(selected) else {
            return;
        };
        let text = self.input_text();
        let cursor = self.input.cursor();
        let pos = cursor.1;
        let suffix = if pos < text.len() { &text[pos..] } else { "" };
        self.set_input(&format!("{}{} {suffix}", &text[..at_offset], f.name));
        self.at_state = None;
    }

    /// Move @-autocomplete selection.
    pub const fn move_at_selection(&mut self, delta: i32) {
        let Some((ref matches, ref mut selected, _)) = self.at_state else {
            return;
        };
        let n = matches.len() as i32;
        if n == 0 {
            return;
        }
        *selected = (*selected as i32 + delta).rem_euclid(n) as usize;
    }

    /// Send the composer's current text (Enter): clear, then route through
    /// `run_command` for `/`-commands or `Send` for plain messages. Domain
    /// failure paths restore the text via `AppEvent::ComposerSet`.
    pub fn submit(&mut self) -> Result<()> {
        let text = self.input_text();
        self.clear_input();
        self.sel.clear(); // history line indices are about to shift
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd)?;
        } else {
            self.core.send_message(text.to_string())?;
        }
        Ok(())
    }

    /// The command seam's view front: parse into the seam, then execute with
    /// view interception (popup opens, quit). Unknown commands surface as a
    /// status line rather than an error.
    pub fn run_command(&mut self, cmd: &str) -> Result<()> {
        match self.core.parse_command(cmd) {
            Ok(cmd) => self.execute(cmd),
            Err(message) => {
                self.push_status(message);
                Ok(())
            }
        }
    }

    /// Execute one command, intercepting the view-only intents (quit, popup
    /// opens, the watch picker) and delegating everything else to the domain
    /// `App::execute`.
    pub fn execute(&mut self, cmd: AppCommand) -> Result<()> {
        match cmd {
            AppCommand::Quit => self.should_quit = true,
            AppCommand::OpenSessionPicker => self.open_session_picker()?,
            AppCommand::OpenSpacePicker => {
                self.open_space_picker()?;
            }
            AppCommand::OpenModelPicker => self.open_model_picker(),
            AppCommand::OpenLogin => self.open_login_popup(),
            AppCommand::OpenSwarm => self.open_swarm_popup(),
            AppCommand::OpenSettings => self.open_settings(),
            AppCommand::OpenCopyMenu => self.open_copy_menu(),
            AppCommand::OpenSkills => self.open_skills_popup(),
            AppCommand::OpenFiles { tab } => {
                if self.core.incognito {
                    self.push_status("not available in incognito mode");
                } else {
                    self.open_files_popup(tab);
                }
            }
            AppCommand::OpenApps => {
                if self.core.incognito {
                    self.push_status("apps not available in incognito mode");
                } else {
                    self.open_apps_popup();
                }
            }
            AppCommand::OpenUsage => self.open_usage_popup(),
            AppCommand::Watch { topic } => {
                if !self.core.is_research_session() {
                    self.push_status(
                        "watch is only available in research sessions — use /research first",
                    );
                } else if let Some(t) = topic {
                    self.core.create_watch(&t);
                } else {
                    self.open_watch_picker()?;
                }
            }
            other => self.core.execute(other)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::Db;
    use nexus_core::space::Space;

    fn test_app() -> AppView {
        let db = Db::open_in_memory().unwrap();
        let space = Space {
            root: std::env::temp_dir().join(format!("nexus-input-test-{}", uuid::Uuid::new_v4())),
        };
        AppView::new(App::new(db, Some("k"), space))
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
        a.popup = Popup::Key;
        a.paste("sk-or-abc123");
        assert_eq!(a.key_input, "sk-or-abc123");
    }

    #[test]
    fn paste_goes_into_files_add_field() {
        let mut a = test_app();
        a.popup = Popup::Files;
        a.files_mode = nexus_core::app::FilesMode::Add;
        a.paste("/tmp/report.pdf");
        assert_eq!(a.files_edit, "/tmp/report.pdf");
    }

    #[test]
    fn paste_in_picker_mode_feeds_the_filter() {
        let mut a = test_app();
        a.popup = Popup::Files;
        a.files_mode = nexus_core::app::FilesMode::Pick;
        a.paste("doc");
        assert_eq!(a.picker_filter, "doc");
    }

    #[test]
    fn paste_into_numeric_settings_field_filters_non_numeric_chars() {
        let mut a = test_app();
        a.popup = Popup::Settings;
        a.settings_selected = 6; // Temperature (numeric)
        a.paste("0.7abc");
        assert_eq!(a.settings_inputs[0], "0.7");
    }

    #[test]
    fn paste_into_url_settings_field_keeps_full_text() {
        let mut a = test_app();
        a.popup = Popup::Settings;
        a.settings_selected = 15; // SearxngUrl (free text)
        a.paste("http://localhost:8080");
        assert_eq!(a.settings_inputs[4], "http://localhost:8080");
    }

    #[test]
    fn pasting_a_file_path_offers_import() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-paste-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&src, "x").unwrap();

        a.paste(&src.to_string_lossy());
        assert_eq!(a.popup, Popup::Files);
        assert!(a.files_mode == nexus_core::app::FilesMode::Add);
        assert_eq!(a.files_edit, src.to_string_lossy());
        assert!(a.input_text().is_empty()); // path did not land in the composer

        // file:// URIs and quoted paths (file-manager drag/drop) work too.
        let mut a = test_app();
        a.paste(&format!("file://{}", src.to_string_lossy()));
        assert_eq!(a.popup, Popup::Files);

        // Ordinary text is untouched.
        let mut a = test_app();
        a.paste("/not/a/real/path and some prose");
        assert_eq!(a.popup, Popup::None);
        assert_eq!(a.input_text(), "/not/a/real/path and some prose");
    }

    #[test]
    fn submit_routes_slash_commands_through_run_command() {
        let mut a = test_app();
        a.set_input("/unknowncmd");
        a.submit().unwrap();
        // Unknown command: status line, nothing sent, composer cleared.
        assert!(a.last_status().contains("unknown command"));
        assert!(a.session.is_none());
        assert!(a.input_text().is_empty());
    }

    #[tokio::test]
    async fn submit_clears_composer_and_sends() {
        let mut a = test_app();
        a.current_model = Some("a/one".into());
        a.set_input("hello");
        a.submit().unwrap();
        assert!(a.session.is_some());
        assert!(a.input_text().is_empty());
        assert_eq!(a.core.messages.len(), 1);
    }

    #[tokio::test]
    async fn run_command_executes_through_the_seam() {
        let mut a = test_app();
        a.run_command("model").unwrap();
        assert_eq!(a.popup, Popup::Model);
        a.run_command("quit").unwrap();
        assert!(a.should_quit);
        // Unknown commands surface as a status line, not an error.
        a.run_command("bogus").unwrap();
        assert!(a.last_status().contains("unknown command"));
    }

    #[test]
    fn accept_command_fill_vs_run() {
        // Tab fills the composer with the canonical command, doesn't run it.
        let mut a = test_app();
        a.set_input("/hist");
        a.accept_command(false).unwrap();
        assert_eq!(a.input_text(), "/session ");

        // Enter on the "new" alias runs it, clearing the composer.
        let mut b = test_app();
        b.current_model = Some("a/one".into());
        let space_id = b.active_space.id.clone();
        b.session = Some(
            b.db.create_session("old chat", "a/one", &space_id, "chat")
                .unwrap(),
        );
        b.set_input("/clear");
        b.accept_command(true).unwrap();
        assert!(b.input_text().is_empty());
        assert!(b.session.is_none()); // /new clears the view; no row created until a message is sent
    }

    #[test]
    fn command_autocomplete_fuzzy_matches_names_aliases_and_desc() {
        let mut a = test_app();
        a.skills.clear(); // isolate this test from installed skills

        // Bare "/" lists everything; a space closes the popup.
        a.set_input("/");
        assert_eq!(a.command_matches().len(), nexus_core::app::COMMANDS.len());
        a.set_input("/new foo");
        assert!(a.command_matches().is_empty());

        // Alias fuzzy-matches to the canonical command.
        a.set_input("/history");
        assert_eq!(a.command_matches()[0].name(), "session");

        // Description is searchable ("stats" -> config).
        a.set_input("/stats");
        assert_eq!(a.command_matches()[0].name(), "config");

        // Non-subsequence garbage matches nothing.
        a.set_input("/zzzz");
        assert!(a.command_matches().is_empty());
    }
}
