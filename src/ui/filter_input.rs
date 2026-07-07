//! A single-line, cursor-addressable text field with selection and OS
//! clipboard copy/paste — used by the popup search/filter boxes (session,
//! space, model picker, skills install). These used to be raw `String` with
//! push/pop-only editing: no cursor movement, no selection, no clipboard.
//! `Deref<Target = str>` keeps existing `.trim()`/`.to_lowercase()`/
//! `.is_empty()` call sites unchanged.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::theme::Theme;

#[derive(Default, Clone)]
pub struct FilterInput {
    text: String,
    cursor: usize, // char index
    anchor: Option<usize>,
}

impl std::ops::Deref for FilterInput {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl From<&str> for FilterInput {
    fn from(s: &str) -> Self {
        let mut f = FilterInput::default();
        f.set(s);
        f
    }
}

impl FilterInput {
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.text.chars().count();
        self.anchor = None;
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn selection_range(&self) -> Option<(usize, usize)> {
        self.anchor
            .map(|a| (a.min(self.cursor), a.max(self.cursor)))
            .filter(|(s, e)| s != e)
    }

    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection_range() else {
            return false;
        };
        let chars: Vec<char> = self.text.chars().collect();
        self.text = chars[..s].iter().chain(chars[e..].iter()).collect();
        self.cursor = s;
        self.anchor = None;
        true
    }

    pub fn insert_char(&mut self, c: char) {
        self.delete_selection();
        let mut chars: Vec<char> = self.text.chars().collect();
        chars.insert(self.cursor.min(chars.len()), c);
        self.text = chars.into_iter().collect();
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.text.chars().collect();
        chars.remove(self.cursor - 1);
        self.text = chars.into_iter().collect();
        self.cursor -= 1;
    }

    fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor >= self.len() {
            return;
        }
        let mut chars: Vec<char> = self.text.chars().collect();
        chars.remove(self.cursor);
        self.text = chars.into_iter().collect();
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = pos.min(self.len());
    }

    fn paste(&mut self, s: &str) {
        self.delete_selection();
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let n = clean.chars().count();
        let mut chars: Vec<char> = self.text.chars().collect();
        for (i, c) in clean.chars().enumerate() {
            chars.insert(self.cursor + i, c);
        }
        self.text = chars.into_iter().collect();
        self.cursor += n;
    }

    fn selected_text(&self) -> Option<String> {
        self.selection_range()
            .map(|(s, e)| self.text.chars().skip(s).take(e - s).collect())
    }

    /// Handle a cursor/selection/clipboard keystroke. Returns whether the key
    /// was consumed — callers fall through to their own char-insert/Backspace
    /// handling (which still goes through `insert_char`/`backspace` above) or
    /// list-navigation handling when this returns `false`.
    pub fn key(&mut self, key: KeyEvent, clipboard: &mut Option<arboard::Clipboard>) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Left => {
                let to = self.cursor.saturating_sub(1);
                self.move_to(to, shift);
                true
            }
            KeyCode::Right => {
                let to = self.cursor + 1;
                self.move_to(to, shift);
                true
            }
            KeyCode::Home => {
                self.move_to(0, shift);
                true
            }
            KeyCode::End => {
                let to = self.len();
                self.move_to(to, shift);
                true
            }
            KeyCode::Delete => {
                self.delete_forward();
                true
            }
            KeyCode::Char('a') if ctrl => {
                self.anchor = Some(0);
                self.cursor = self.len();
                true
            }
            KeyCode::Char('c') if ctrl => {
                if let Some(sel) = self.selected_text() {
                    let _ = clipboard.as_mut().map(|cb| cb.set_text(sel));
                }
                true
            }
            KeyCode::Char('x') if ctrl => {
                if let Some(sel) = self.selected_text() {
                    let _ = clipboard.as_mut().map(|cb| cb.set_text(sel));
                    self.delete_selection();
                }
                true
            }
            KeyCode::Char('v') if ctrl => {
                if let Some(text) = clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                    self.paste(&text);
                }
                true
            }
            _ => false,
        }
    }

    /// Styled spans for this field's current text: cursor marker (or
    /// highlighted char) when there's no selection, a highlighted range when
    /// there is one. Callers splice these into a popup title `Line` alongside
    /// their own label/hint spans.
    pub fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        let chars: Vec<char> = self.text.chars().collect();
        let sel_style = Style::default().bg(theme.accent).fg(Color::Black);
        if let Some((s, e)) = self.selection_range() {
            vec![
                Span::raw(chars[..s].iter().collect::<String>()),
                Span::styled(chars[s..e].iter().collect::<String>(), sel_style),
                Span::raw(chars[e..].iter().collect::<String>()),
            ]
        } else if self.cursor >= chars.len() {
            vec![
                Span::raw(self.text.clone()),
                Span::styled(
                    "▏",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]
        } else {
            vec![
                Span::raw(chars[..self.cursor].iter().collect::<String>()),
                Span::styled(chars[self.cursor].to_string(), sel_style),
                Span::raw(chars[self.cursor + 1..].iter().collect::<String>()),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_moves_and_edits_mid_string() {
        let mut f = FilterInput::from("helo");
        f.move_to(2, false); // "he|lo"
        f.insert_char('l'); // "hel|lo"
        assert_eq!(&*f, "hello");
        assert_eq!(f.cursor, 3);
    }

    #[test]
    fn select_all_then_copy_yields_full_text() {
        let mut f = FilterInput::from("abc");
        f.anchor = Some(0);
        f.cursor = f.len();
        assert_eq!(f.selected_text().as_deref(), Some("abc"));
    }

    #[test]
    fn backspace_deletes_selection_not_last_char() {
        let mut f = FilterInput::from("abcdef");
        f.anchor = Some(1);
        f.cursor = 4; // selects "bcd"
        f.backspace();
        assert_eq!(&*f, "aef");
    }
}
