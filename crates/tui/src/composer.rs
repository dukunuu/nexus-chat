//! The composer: text editing on the view-owned `TextArea`, OS-clipboard
//! cut/copy/paste, slash-command autocomplete, and `@`-file autocomplete.
//! The command catalog itself lives in `nexus_core::app::commands`.

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

use nexus_core::app::{AppCommand, Match, PendingEditor, Popup};

use crate::app_view::AppView;

const COMPOSER_MULTI_CLICK: Duration = Duration::from_millis(400);

/// What a composer line starting with `!` asks to run.
#[derive(Debug, PartialEq, Eq)]
enum UserCommand {
    /// `!` (or `!tty`) with nothing after it.
    Empty,
    /// Output captured into the conversation.
    Captured(String),
    /// Gets the real terminal: `!tty <cmd>`, or a program that needs one
    /// (`sudo`, `ssh`, `vim`, `less`, …; see `usershell::terminal_program`).
    Interactive(String),
}

/// Whether a draft is a `!` shell line. A draft that opens with a Markdown
/// image (`![alt](path)`, which is how a pasted image lands) is a message.
pub fn is_shell_draft(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('!') else {
        return false;
    };
    let markdown_image = rest
        .strip_prefix('[')
        .and_then(|alt| alt.find(']').map(|end| &alt[end + 1..]))
        .is_some_and(|after| after.starts_with('('));
    !markdown_image
}

/// Parse a `!` line; `None` means an ordinary message.
fn parse_user_command(text: &str) -> Option<UserCommand> {
    let text = text.trim();
    if !is_shell_draft(text) {
        return None;
    }
    let cmd = text[1..].trim();
    let tty = cmd
        .strip_prefix("tty")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace));
    Some(match (tty, cmd.split_whitespace().next()) {
        (Some(rest), _) if rest.trim().is_empty() => UserCommand::Empty,
        (Some(rest), _) => UserCommand::Interactive(rest.trim().to_string()),
        (None, None) => UserCommand::Empty,
        (None, Some(_)) if nexus_core::usershell::terminal_program(cmd).is_some() => {
            UserCommand::Interactive(cmd.to_string())
        }
        (None, Some(_)) => UserCommand::Captured(cmd.to_string()),
    })
}

/// Visible shell candidates and the word they replace.
pub struct ShellCompletionState {
    pub candidates: Vec<String>,
    pub selected: usize,
    pub range: std::ops::Range<usize>,
}

/// A background result, tied to the exact draft and context that requested it.
pub struct ShellCompletionResult {
    pub text: String,
    pub cursor: usize,
    pub cwd: std::path::PathBuf,
    pub session: Option<String>,
    pub completion: nexus_core::usershell::Completion,
}

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
pub(crate) fn pasted_file_path(text: &str) -> Option<std::path::PathBuf> {
    let t = text.trim().trim_matches('"').trim_matches('\'');
    let t = t.strip_prefix("file://").unwrap_or(t);
    if t.contains('\n') || !t.starts_with('/') {
        return None;
    }
    let path = std::path::PathBuf::from(t);
    path.is_file().then_some(path)
}

impl AppView {
    pub fn input_text(&self) -> String {
        self.input.lines().join("\n")
    }

    /// Replace the composer contents, cursor at end.
    pub fn set_input(&mut self, text: &str) {
        self.shell_state = None;
        self.shell_completion_rx = None;
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map_or(0, |l| l.chars().count());
        self.input.set_lines(lines, (row, col));
    }

    pub fn clear_input(&mut self) {
        self.shell_state = None;
        self.shell_completion_rx = None;
        self.input = new_textarea();
    }

    /// Copy the current selection to the OS clipboard, then clear the highlight
    /// (the cleared highlight signals success).
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
            Popup::ImageViewer => self.viewer_drop(text),
            Popup::InteractiveUi => self.interactive.paste(text),
            Popup::TuiApp => crate::ui::popups::tui_app::paste(self, text),
            Popup::Key => self.key_input.push_str(text),
            Popup::Connectors => crate::ui::popups::connectors::paste(self, text),
            Popup::Jobs => {
                if let Some(form) = &mut self.jobs_popup.form {
                    form.values[form.field].push_str(text);
                }
            }
            Popup::Session if self.session_mode == SessionMode::Rename => {
                self.session_edit.push_str(text);
            }
            Popup::Space if matches!(self.space_mode, SpaceMode::Create | SpaceMode::Rename) => {
                self.space_edit.push_str(text);
            }
            Popup::Skills if self.skills_mode == SkillsMode::Install => {
                self.skills_edit.push_str(text);
            }
            Popup::Apps if matches!(self.apps_mode, AppsMode::EditFile | AppsMode::RenamePanel) => {
                self.apps_edit.push_str(text);
            }
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

    // --- user-shell completion ---

    /// Whether the composer holds a `!` shell line. `!` lines are an
    /// agent-mode feature: in chat mode the same text is an ordinary message.
    pub fn shell_draft(&self) -> bool {
        self.work_mode.is_agent() && is_shell_draft(&self.input_text())
    }

    /// Up/Down recall shell history: agent mode, empty or `!` draft.
    pub fn shell_history_mode(&self) -> bool {
        self.work_mode.is_agent() && {
            let text = self.input_text();
            text.is_empty() || is_shell_draft(&text)
        }
    }

    fn cursor_byte(&self) -> usize {
        let (row, col) = self.input.cursor();
        self.input
            .lines()
            .iter()
            .take(row)
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + self.input.lines()[row]
                .char_indices()
                .nth(col)
                .map_or(self.input.lines()[row].len(), |(i, _)| i)
    }

    fn replace_shell_word(&mut self, range: std::ops::Range<usize>, replacement: &str) {
        let mut text = self.input_text();
        let end = range.start + replacement.len();
        text.replace_range(range, replacement);
        self.set_input(&text);
        let before = &text[..end];
        let row = before.bytes().filter(|b| *b == b'\n').count();
        let col = before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        self.jump_cursor((row, col));
    }

    pub fn start_shell_completion(&mut self) {
        if !self.shell_draft() {
            return;
        }
        let text = self.input_text();
        self.at_state = None;
        let cursor = self.cursor_byte();
        let cwd = self.user_command_cwd();
        let session = self.session.as_ref().map(|s| s.id.clone());
        let setup = std::sync::Arc::clone(&self.user_shell);
        let data = self.space.root.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.shell_completion_rx = Some(rx);
        tokio::spawn(async move {
            let setup = setup
                .get_or_init(|| nexus_core::usershell::UserShell::load(&data))
                .await;
            let completion = nexus_core::usershell::complete(setup, &text, cursor, &cwd).await;
            let _ = tx.send(ShellCompletionResult {
                text,
                cursor,
                cwd,
                session,
                completion,
            });
        });
    }

    pub fn finish_shell_completion(&mut self, result: ShellCompletionResult) {
        self.shell_completion_rx = None;
        self.report_user_shell_warning();
        if self.input_text() != result.text
            || self.cursor_byte() != result.cursor
            || self.user_command_cwd() != result.cwd
            || self.session.as_ref().map(|s| s.id.clone()) != result.session
        {
            return;
        }
        let completion = result.completion;
        if completion.candidates.is_empty() {
            return;
        }
        if completion.candidates.len() == 1 {
            self.shell_state = Some(ShellCompletionState {
                candidates: completion.candidates,
                selected: 0,
                range: completion.range,
            });
            self.accept_shell_match();
            return;
        }
        let prefix = nexus_core::usershell::common_prefix(&completion.candidates);
        let range = if prefix.is_empty() {
            completion.range
        } else {
            let start = completion.range.start;
            self.replace_shell_word(completion.range, &prefix);
            start..start + prefix.len()
        };
        self.shell_state = Some(ShellCompletionState {
            candidates: completion.candidates,
            selected: 0,
            range,
        });
    }

    pub fn move_shell_selection(&mut self, delta: i32) {
        if let Some(state) = &mut self.shell_state {
            state.selected =
                (state.selected as i32 + delta).rem_euclid(state.candidates.len() as i32) as usize;
        }
    }

    pub fn accept_shell_match(&mut self) {
        let Some(state) = self.shell_state.take() else {
            return;
        };
        let mut candidate = state.candidates[state.selected].clone();
        let text = self.input_text();
        if !candidate.ends_with('/') && !text[state.range.end..].starts_with(char::is_whitespace) {
            candidate.push(' ');
        }
        self.replace_shell_word(state.range, &candidate);
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
        if rest.contains(char::is_whitespace) || rest.contains('/') {
            return None;
        }
        Some((rest.to_string(), at))
    }

    pub fn refresh_at_matches(&mut self) {
        if self.shell_draft() {
            self.at_state = None;
            return;
        }
        let Some((query, at_offset)) = self.at_query() else {
            self.at_state = None;
            return;
        };
        if query.is_empty() {
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

    /// Alt+↑: replace the composer with the previous message this session
    /// sent, stashing the current draft on the first step.
    pub fn recall_prev(&mut self) {
        if self.shell_history_mode() {
            self.recall_shell_prev();
            return;
        }
        let sent: Vec<String> = self
            .core
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.clone())
            .collect();
        if sent.is_empty() {
            return;
        }
        let back = self.recall.map_or(0, |i| (i + 1).min(sent.len() - 1));
        if self.recall.is_none() {
            self.recall_draft = self.input_text();
        }
        self.recall = Some(back);
        self.set_input(&sent[sent.len() - 1 - back]);
    }

    /// Alt+↓: step forward through recalled messages, back to the stashed
    /// draft past the newest one.
    pub fn recall_next(&mut self) {
        if let Some((history, index, draft)) = self.shell_recall.take() {
            if index == 0 {
                self.set_input(&draft);
            } else {
                self.set_input(&history[index - 1]);
                self.shell_recall = Some((history, index - 1, draft));
            }
            return;
        }
        match self.recall {
            None => {}
            Some(0) => {
                self.recall = None;
                let draft = std::mem::take(&mut self.recall_draft);
                self.set_input(&draft);
            }
            Some(i) => {
                self.recall = Some(i - 1);
                let sent: Vec<&str> = self
                    .core
                    .messages
                    .iter()
                    .filter(|m| m.role == "user")
                    .map(|m| m.content.as_str())
                    .collect();
                if let Some(text) = sent.len().checked_sub(i).and_then(|n| sent.get(n)) {
                    let text = (*text).to_string();
                    self.set_input(&text);
                }
            }
        }
    }

    fn recall_shell_prev(&mut self) {
        if let Some((history, index, draft)) = self.shell_recall.take() {
            let index = (index + 1).min(history.len() - 1);
            self.set_input(&history[index]);
            self.shell_recall = Some((history, index, draft));
            return;
        }
        let mut history = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let session_commands = self
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == "user_command")
            .filter_map(|m| serde_json::from_str::<serde_json::Value>(&m.content).ok())
            .filter_map(|row| row["cmd"].as_str().map(str::to_string));
        for cmd in session_commands.chain(self.db.recent_user_commands().unwrap_or_default()) {
            if !cmd.is_empty() && seen.insert(cmd.clone()) {
                history.push(format!("!{cmd}"));
                if history.len() == 200 {
                    break;
                }
            }
        }
        if history.is_empty() {
            return;
        }
        let draft = self.input_text();
        self.set_input(&history[0]);
        self.recall = None;
        self.shell_recall = Some((history, 0, draft));
    }

    /// Send the composer's current text (Enter): clear, then route through
    /// `run_command` for `/`-commands, the user-command path for `!cmd`, or
    /// `Send` for plain messages. Domain failure paths restore the text via
    /// `AppEvent::ComposerSet`.
    pub fn submit(&mut self) -> Result<()> {
        self.recall = None;
        self.shell_recall = None;
        let text = self.input_text();
        self.clear_input();
        self.sel.clear(); // history line indices are about to shift
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd)?;
        } else if let Some(command) = self
            .work_mode
            .is_agent()
            .then(|| parse_user_command(text))
            .flatten()
        {
            match command {
                UserCommand::Empty => {}
                UserCommand::Captured(cmd) => self.execute(AppCommand::RunUserCommand { cmd })?,
                UserCommand::Interactive(_) if self.core.is_streaming() => {
                    self.push_status("wait — response still streaming");
                    self.set_input(text);
                }
                // The event loop owns the terminal, so it runs this.
                UserCommand::Interactive(cmd) => {
                    self.pending_editor = Some(PendingEditor::UserCommand(cmd));
                }
            }
        } else {
            let text = self.with_studio_context(text.to_string());
            self.core.send_message(text)?;
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
        if self.popup == Popup::Approval
            && !matches!(cmd, AppCommand::Approve { .. } | AppCommand::Cancel { .. })
        {
            return Ok(());
        }
        match cmd {
            AppCommand::Approve { decision } => {
                self.core.execute(AppCommand::Approve { decision })?;
                self.approval_request = None;
                if self.popup == Popup::Approval {
                    self.popup = Popup::None;
                }
            }
            AppCommand::Quit => self.should_quit = true,
            AppCommand::OpenSessionPicker => self.open_session_picker()?,
            AppCommand::OpenSpacePicker => {
                self.open_space_picker()?;
            }
            AppCommand::OpenModelPicker => self.open_model_picker(),
            AppCommand::OpenLogin => self.open_login_popup(),
            // A bare `/local` picks interactively; a spec applies directly.
            AppCommand::ConfigureLocal { spec } if spec.trim().is_empty() => {
                self.open_local_popup();
            }
            // `/local status` has a table to show, so it opens the picker the
            // survey renders into rather than only printing a summary line.
            AppCommand::LocalServer { verb, runtime } if verb == "status" => {
                self.core.manage_local_server(&verb, &runtime);
                self.open_local_popup();
            }
            AppCommand::OpenSwarm => self.open_swarm_popup(),
            AppCommand::OpenSettings => self.open_settings(),
            AppCommand::SetTheme { mode } => self.set_background_mode(&mode)?,
            AppCommand::OpenCopyMenu => self.open_copy_menu(),
            AppCommand::OpenSkills => self.open_skills_popup(),
            AppCommand::OpenFiles { tab } => {
                if self.core.incognito {
                    self.push_status("not available in incognito mode");
                } else {
                    self.open_files_popup(tab);
                }
            }
            AppCommand::OpenConnectors => crate::ui::popups::connectors::open(self),
            AppCommand::OpenApps => {
                if self.core.incognito {
                    self.push_status("apps not available in incognito mode");
                } else {
                    self.open_apps_popup();
                }
            }
            AppCommand::OpenTuiApp(name) => crate::ui::popups::tui_app::open(self, &name),
            AppCommand::PinPanel { name } => {
                if self.core.incognito {
                    self.push_status("pinning is not available in incognito mode");
                } else {
                    crate::interactive::pin_focused(self, name.as_deref());
                }
            }
            AppCommand::OpenImage { target } => self.open_image_command(&target),
            AppCommand::OpenStudio { deck } => {
                if let Err(error) = self.open_studio(deck.as_deref()) {
                    self.push_status(format!("{error:#}"));
                }
            }
            AppCommand::OpenJobs => crate::ui::popups::jobs::open(self, false)?,
            AppCommand::OpenJobForm => crate::ui::popups::jobs::open(self, true)?,
            AppCommand::OpenUsage => self.open_usage_popup(),
            AppCommand::OpenHelp => self.open_help(),
            AppCommand::OpenProcs => {
                self.core.execute(AppCommand::OpenProcs)?;
                self.procs_output = None;
                self.procs_kill_confirm = None;
                self.procs_scroll = 0;
                self.popup = Popup::Procs;
            }
            AppCommand::Harness => {
                self.core.execute(AppCommand::Harness)?;
                self.popup = Popup::Harness;
            }
            // The picker (browse/open/delete watches) works anywhere; creating
            // one starts research, which from a plain chat would migrate the
            // conversation — so that part stays research-session-only.
            AppCommand::Watch { topic: None } => self.open_watch_picker()?,
            AppCommand::Watch { topic: Some(t) } => {
                if self.core.is_research_session() {
                    self.core.create_watch(&t);
                } else {
                    self.push_status(
                        "new watches start from a research session — use /research first",
                    );
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

    /// `!` shell lines are an agent-mode feature.
    fn agent_app() -> AppView {
        let mut app = test_app();
        app.work_mode = nexus_core::app::WorkMode::Agent;
        app
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
        // A bare `/local` is view-only; a spec would reach the domain (and
        // the user's config file), so the test stops at the popup.
        a.run_command("local").unwrap();
        assert_eq!(a.popup, Popup::Local);
        a.run_command("theme opaque").unwrap();
        assert_eq!(a.background_mode, crate::theme::BackgroundMode::Opaque);
        assert!(
            a.db.load_settings()
                .unwrap()
                .iter()
                .any(|(key, value)| key == "ui_background" && value == "opaque")
        );
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

    #[test]
    fn watch_picker_opens_anywhere_but_new_watches_need_research() {
        let mut a = test_app();
        a.run_command("watch").unwrap();
        assert!(
            a.popup == Popup::Watch,
            "the picker needs no research session"
        );
        a.popup = Popup::None;
        a.run_command("watch rust async").unwrap();
        assert!(a.last_status().contains("research session"));
        assert!(a.popup == Popup::None);
    }

    #[test]
    fn alt_arrows_recall_sent_messages_and_restore_the_draft() {
        let mut a = test_app();
        let user = |content: &str| nexus_core::db::Message {
            role: "user".into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        };
        a.core.messages = vec![user("first"), user("second")];
        a.set_input("half-typed");
        a.recall_prev();
        assert_eq!(a.input_text(), "second");
        a.recall_prev();
        assert_eq!(a.input_text(), "first");
        a.recall_prev();
        assert_eq!(a.input_text(), "first", "stops at the oldest");
        a.recall_next();
        assert_eq!(a.input_text(), "second");
        a.recall_next();
        assert_eq!(a.input_text(), "half-typed", "the draft comes back");
        a.recall_next();
        assert_eq!(a.input_text(), "half-typed");
    }

    #[tokio::test]
    async fn shell_completion_replaces_a_quoted_word_at_a_unicode_cursor() {
        let mut app = agent_app();
        let root = app.space.root.clone();
        let cwd = app.user_command_cwd();
        std::fs::write(cwd.join("foo bar"), "").unwrap();
        let text = "!echo é \"foo b\" tail";
        app.set_input(text);
        app.input.move_cursor(CursorMove::Jump(0, 14));
        let cursor = app.cursor_byte();
        let setup = nexus_core::usershell::UserShell {
            shell: "/bin/sh".into(),
            snapshot: None,
            path: String::new(),
            names: Vec::new(),
            warning: None,
        };
        let completion = nexus_core::usershell::complete(&setup, text, cursor, &cwd).await;
        app.finish_shell_completion(ShellCompletionResult {
            text: text.into(),
            cursor,
            cwd,
            session: None,
            completion,
        });
        assert_eq!(app.input_text(), "!echo é foo\\ bar tail");
        assert!(app.shell_state.is_none());
        assert_eq!(app.input.cursor(), (0, 16));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shell_history_is_session_first_unique_and_restores_the_bang_draft() {
        let mut app = agent_app();
        let current = app
            .db
            .create_session("current", "a/one", &app.active_space.id, "chat")
            .unwrap();
        for cmd in ["ls", "pwd", "ls"] {
            app.db
                .add_user_command_message(&current.id, &serde_json::json!({"cmd": cmd}).to_string())
                .unwrap();
        }
        let other = app
            .db
            .create_session("other", "a/one", &app.active_space.id, "chat")
            .unwrap();
        for cmd in ["old", "ls", "recent"] {
            app.db
                .add_user_command_message(&other.id, &serde_json::json!({"cmd": cmd}).to_string())
                .unwrap();
        }
        app.core.messages = app.db.load_messages(&current.id).unwrap();
        app.set_input("!draft");
        for expected in ["!ls", "!pwd", "!recent", "!old", "!old"] {
            app.recall_prev();
            assert_eq!(app.input_text(), expected);
        }
        for expected in ["!recent", "!pwd", "!ls", "!draft", "!draft"] {
            app.recall_next();
            assert_eq!(app.input_text(), expected);
        }
    }

    #[test]
    fn recent_shell_history_is_capped_at_two_hundred_unique_commands() {
        let app = agent_app();
        let session = app
            .db
            .create_session("history", "a/one", &app.active_space.id, "chat")
            .unwrap();
        for i in 0..205 {
            app.db
                .add_user_command_message(
                    &session.id,
                    &serde_json::json!({"cmd": format!("echo {i}")}).to_string(),
                )
                .unwrap();
        }
        app.db
            .add_user_command_message(&session.id, r#"{"cmd":"echo 204"}"#)
            .unwrap();
        let history = app.db.recent_user_commands().unwrap();
        assert_eq!(history.len(), 200);
        assert_eq!(history[0], "echo 204");
        assert_eq!(history.last().unwrap(), "echo 5");
    }

    #[test]
    fn bang_lines_parse_into_user_commands() {
        for (text, expected) in [
            ("!ls", Some(UserCommand::Captured("ls".into()))),
            (
                "  ! cargo test  ",
                Some(UserCommand::Captured("cargo test".into())),
            ),
            ("!tty vim x", Some(UserCommand::Interactive("vim x".into()))),
            (
                "!sudo make install",
                Some(UserCommand::Interactive("sudo make install".into())),
            ),
            (
                "!ssh host",
                Some(UserCommand::Interactive("ssh host".into())),
            ),
            (
                "!less log.txt",
                Some(UserCommand::Interactive("less log.txt".into())),
            ),
            ("!sudoku", Some(UserCommand::Captured("sudoku".into()))),
            ("!ttyd", Some(UserCommand::Captured("ttyd".into()))),
            ("!", Some(UserCommand::Empty)),
            ("!tty ", Some(UserCommand::Empty)),
            ("hello !x", None),
            ("![pasted image](pasted-1.png)", None),
            ("![pasted image](a.png) what is this?", None),
            (
                "![ -f x ] && echo y",
                Some(UserCommand::Captured("[ -f x ] && echo y".into())),
            ),
        ] {
            assert_eq!(parse_user_command(text), expected, "{text:?}");
        }
    }

    #[test]
    fn submitting_bang_lines_never_sends_them_to_the_model() {
        let mut a = agent_app();
        a.current_model = Some("a/one".into());
        a.set_input("!");
        a.submit().unwrap();
        assert!(a.session.is_none());
        assert!(a.core.messages.is_empty());
        assert!(a.pending_editor.is_none());
        a.set_input("!tty vim x");
        a.submit().unwrap();
        assert!(matches!(
            a.pending_editor.take(),
            Some(PendingEditor::UserCommand(cmd)) if cmd == "vim x"
        ));
        assert!(a.session.is_none());
        assert!(a.input_text().is_empty());
    }

    #[test]
    fn a_pasted_image_draft_is_a_message_not_a_shell_line() {
        let mut a = agent_app();
        a.set_input("![pasted image](pasted-1.png) what is this?");
        assert!(!a.shell_history_mode());
        a.start_shell_completion();
        assert!(
            a.shell_completion_rx.is_none(),
            "Tab does not complete it as a command"
        );
        a.set_input("!ls");
        assert!(a.shell_history_mode());
    }

    #[tokio::test]
    async fn chat_mode_bang_draft_is_an_ordinary_message_with_no_shell_affordances() {
        let mut a = test_app();
        assert_eq!(a.work_mode, nexus_core::app::WorkMode::Chat);
        a.current_model = Some("a/one".into());
        a.set_input("!ls");
        // The composer title, Tab completion and refresh_at_matches all key off
        // `shell_draft`, which is false here.
        assert!(!a.shell_draft());
        assert!(!a.shell_history_mode());
        a.start_shell_completion();
        assert!(a.shell_completion_rx.is_none(), "Tab does not complete");
        a.set_input("");
        assert!(
            !a.shell_history_mode(),
            "an empty chat draft does not recall shell history"
        );
        a.set_input("!ls");
        a.submit().unwrap();
        assert!(a.pending_editor.is_none());
        assert!(!a.user_command_running());
        let sent = a.core.messages.last().expect("the line was sent");
        assert_eq!((sent.role.as_str(), sent.content.as_str()), ("user", "!ls"));
        // `!tty` and terminal programs are equally just text.
        a.set_input("!tty vim x");
        a.submit().unwrap();
        assert!(a.pending_editor.is_none());
    }

    #[test]
    fn agent_modes_keep_the_shell_affordances() {
        for mode in [
            nexus_core::app::WorkMode::Agent,
            nexus_core::app::WorkMode::AgentAuto,
        ] {
            let mut a = test_app();
            a.work_mode = mode;
            a.set_input("!ls");
            assert!(a.shell_draft());
            assert!(a.shell_history_mode());
            a.set_input("");
            assert!(a.shell_history_mode());
            a.set_input("hello");
            assert!(!a.shell_draft());
            assert!(!a.shell_history_mode());
        }
    }
}
