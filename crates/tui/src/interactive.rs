//! Model-authored UI lifecycle, rendering and input. Source remains in normal
//! messages; temporary state is keyed by message, fence ordinal and revision.

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::popups::chrome;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use nexus_core::app::Popup;
use nexus_core::interactive::{Document, Node, Session, Tone};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use std::collections::HashMap;

/// Stable reserved region in a message's line-based layout.
#[derive(Clone, Debug)]
pub struct Placement {
    pub row: usize,
    pub width: u16,
    pub owner: usize,
    pub ordinal: usize,
    pub height: u16,
    pub source: String,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    owner: usize,
    ordinal: usize,
    source: String,
}
impl Placement {
    fn key(&self) -> Key {
        Key {
            owner: self.owner,
            ordinal: self.ordinal,
            source: self.source.clone(),
        }
    }
}
#[derive(Clone)]
enum Control {
    Action(String),
    Input(String),
}
struct Entry {
    session: Result<Session, String>,
    focus: usize,
    error: String,
    scroll: u16,
}
impl Entry {
    fn controls(&self) -> Vec<Control> {
        self.session.as_ref().map_or_else(
            |_| Vec::new(),
            |session| {
                session
                    .document()
                    .nodes()
                    .iter()
                    .flat_map(|node| match node {
                        Node::Buttons { items, .. } => items
                            .iter()
                            .map(|button| Control::Action(button.action.clone()))
                            .collect(),
                        Node::Input { key, .. } => vec![Control::Input(key.clone())],
                        Node::Text { .. } => Vec::new(),
                    })
                    .collect()
            },
        )
    }
    fn activate(&mut self, control: usize, can_send: bool) -> Option<String> {
        self.focus = control;
        let Some(Control::Action(action)) = self.controls().get(control).cloned() else {
            return None;
        };
        let Ok(session) = &mut self.session else {
            return None;
        };
        if session.document().sends(&action) && !can_send {
            self.error =
                "Finish the reply and clear your draft before reviewing a UI response.".into();
            return None;
        }
        match session.activate(&action) {
            Ok(message) => {
                self.error.clear();
                message
            }
            Err(error) => {
                self.error = error.to_string();
                None
            }
        }
    }
    fn edit(&mut self, text: &str, backspace: bool) {
        let Some(Control::Input(key)) = self.controls().get(self.focus).cloned() else {
            return;
        };
        let Ok(session) = &mut self.session else {
            return;
        };
        let mut value = session.value(&key);
        if backspace {
            value.pop();
        } else {
            value.extend(text.chars().filter(|ch| !ch.is_control()).take(1024));
        }
        match session.input(&key, value) {
            Ok(()) => self.error.clear(),
            Err(error) => self.error = error.to_string(),
        }
    }
    fn canvas(
        &self,
        width: u16,
        theme: &Theme,
        focused: bool,
        can_send: bool,
    ) -> (Buffer, Vec<(Rect, usize)>) {
        let height = self
            .session
            .as_ref()
            .map_or(6, |session| session.document().rows());
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, theme.background_style());
        let hits = Vec::new();
        let Ok(session) = &self.session else {
            let error = self
                .session
                .as_ref()
                .err()
                .map_or("UI unavailable", String::as_str);
            Paragraph::new(format!("Interactive UI unavailable: {error}"))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme.error))
                .render(area, &mut buffer);
            return (buffer, hits);
        };
        Paragraph::new(session.document().title())
            .style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .render(Rect::new(0, 0, width, 1), &mut buffer);
        if !self.error.is_empty() {
            Paragraph::new(safe(&self.error))
                .style(Style::default().fg(theme.error))
                .render(Rect::new(0, 1, width, 1), &mut buffer);
        }
        let hits = self.nodes(session, theme, focused, can_send, &mut buffer);
        Paragraph::new("F2 interact · click controls · Esc back from focus")
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(0, height - 1, width, 1), &mut buffer);
        (buffer, hits)
    }
    fn nodes(
        &self,
        session: &Session,
        theme: &Theme,
        focused: bool,
        can_send: bool,
        buffer: &mut Buffer,
    ) -> Vec<(Rect, usize)> {
        let width = buffer.area.width;
        let mut hits = Vec::new();
        let mut y = 2;
        let mut control = 0;
        for node in session.document().nodes() {
            let region = Rect::new(0, y, width, node.rows());
            match node {
                Node::Text { text, tone, .. } => {
                    let color = match tone {
                        Tone::Normal => theme.fg,
                        Tone::Muted => theme.fg_dim,
                        Tone::Accent => theme.accent,
                        Tone::Success => theme.success,
                        Tone::Warning => theme.warning,
                        Tone::Error => theme.error,
                    };
                    Paragraph::new(safe(
                        &session.text(text).unwrap_or_else(|error| error.to_string()),
                    ))
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(color))
                    .render(region, buffer);
                }
                Node::Buttons { items, .. } => {
                    let labels: Vec<_> = items
                        .iter()
                        .map(|button| {
                            let mut label = safe(&session.text(&button.label).unwrap_or_default())
                                .replace('\n', " ");
                            if session.document().sends(&button.action) {
                                label.push_str(" [review]");
                            }
                            label
                        })
                        .collect();
                    let bodies: Vec<Vec<_>> = items
                        .iter()
                        .map(|button| button.body.iter().map(String::as_str).collect())
                        .collect();
                    let buttons: Vec<_> = items
                        .iter()
                        .enumerate()
                        .map(|(index, button)| chrome::Button {
                            action: control + index,
                            label: &labels[index],
                            body: &bodies[index],
                            selected: focused && self.focus == control + index,
                            enabled: can_send || !session.document().sends(&button.action),
                        })
                        .collect();
                    hits.extend(chrome::render_buttons(buffer, region, &buttons, theme));
                    control += items.len();
                }
                Node::Input { key, label } => {
                    let selected = focused && self.focus == control;
                    let block = chrome::popup_block_focused(
                        label.as_str(),
                        theme,
                        selected,
                        chrome::Tone::Normal,
                    );
                    let inner = block.inner(region);
                    block.render(region, buffer);
                    Paragraph::new(format!(
                        "{}{}",
                        safe(&session.value(key)),
                        if selected { "▏" } else { "" }
                    ))
                    .style(Style::default().fg(theme.fg))
                    .render(inner, buffer);
                    hits.push((region, control));
                    control += 1;
                }
            }
            y += node.rows();
        }
        hits
    }
}
#[derive(Clone)]
struct Hit {
    rect: Rect,
    key: Key,
    control: Option<usize>,
}

/// Disposable UI instances and this frame's hit areas. No filesystem writes.
#[derive(Default)]
pub struct Widgets {
    entries: HashMap<Key, Entry>,
    hits: Vec<Hit>,
    visible: Vec<Key>,
    focused: Option<Key>,
    screen: Option<Rect>,
    send_ready: bool,
}
impl Widgets {
    /// Discard all local state and stale hit areas on a viewport/session reset.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// Rebuild visible instances/hits and submission readiness for this frame.
    pub fn begin_frame(&mut self, send_ready: bool) {
        self.send_ready = send_ready;
        self.hits.clear();
        self.visible.clear();
    }
    /// Return keyboard ownership without resetting document state.
    pub fn unfocus(&mut self) {
        self.focused = None;
    }
    /// Focus the last visible document, if any.
    pub fn focus_latest(&mut self) -> bool {
        self.focused = self.visible.last().cloned();
        self.focused.is_some()
    }
    fn register(&mut self, key: &Key) {
        if self.entries.contains_key(key) || self.entries.len() >= 64 {
            return;
        }
        let session = Document::parse(&key.source)
            .and_then(Session::new)
            .map_err(|error| error.to_string());
        self.entries.insert(
            key.clone(),
            Entry {
                session,
                focus: 0,
                error: String::new(),
                scroll: 0,
            },
        );
    }
    /// Overlay visible regions and capture exact-frame, clipped control hits.
    pub fn paint(
        &mut self,
        f: &mut Frame,
        viewport: Rect,
        top: usize,
        placements: &[Placement],
        theme: &Theme,
        active: bool,
    ) {
        self.screen = Some(f.area());
        for placement in placements {
            if !placement.complete {
                continue;
            }
            let first = top
                .saturating_sub(placement.row)
                .min(usize::from(placement.height));
            let end = (top + usize::from(viewport.height))
                .saturating_sub(placement.row)
                .min(usize::from(placement.height));
            let width = placement.width.min(viewport.width.saturating_sub(4));
            if first >= end || width == 0 {
                continue;
            }
            let key = placement.key();
            self.register(&key);
            let Some(entry) = self.entries.get(&key) else {
                let y = viewport.y + u16::try_from(placement.row.saturating_sub(top)).unwrap_or(0);
                f.render_widget(
                    Paragraph::new("UI instance limit reached; start a new session.")
                        .style(Style::default().fg(theme.error)),
                    Rect::new(viewport.x + 4, y, width, 1),
                );
                continue;
            };
            self.visible.push(key.clone());
            let (canvas, mut hits) = entry.canvas(width, theme, false, self.send_ready);
            let end = end.min(usize::from(canvas.area.height));
            if first >= end {
                continue;
            }
            hits.push((Rect::new(0, placement.height - 1, width, 1), usize::MAX));
            for row in first..end {
                let y = viewport.y + u16::try_from(placement.row + row - top).unwrap_or(0);
                for x in 0..width {
                    f.buffer_mut()[(viewport.x + 4 + x, y)] =
                        canvas[(x, u16::try_from(row).unwrap_or(0))].clone();
                }
            }
            if active {
                for (rect, control) in hits {
                    let low = usize::from(rect.y).max(first);
                    let high = usize::from(rect.bottom()).min(end);
                    if low < high {
                        self.hits.push(Hit {
                            rect: Rect::new(
                                viewport.x + 4 + rect.x,
                                viewport.y + u16::try_from(placement.row + low - top).unwrap_or(0),
                                rect.width,
                                u16::try_from(high - low).unwrap_or(0),
                            ),
                            key: key.clone(),
                            control: (control != usize::MAX).then_some(control),
                        });
                    }
                }
            }
        }
    }
    fn click(&mut self, mouse: MouseEvent, screen: Rect, can_send: bool) -> (bool, Option<String>) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) || self.screen != Some(screen) {
            return (false, None);
        }
        let pos = Position::new(mouse.column, mouse.row);
        let Some(hit) = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(pos))
            .cloned()
        else {
            return (false, None);
        };
        let Some(entry) = self.entries.get_mut(&hit.key) else {
            return (false, None);
        };
        match hit.control.and_then(|index| {
            entry
                .controls()
                .get(index)
                .cloned()
                .map(|control| (index, control))
        }) {
            Some((index, Control::Action(_))) => (true, entry.activate(index, can_send)),
            Some((index, Control::Input(_))) => {
                entry.focus = index;
                self.focused = Some(hit.key);
                (true, None)
            }
            None => {
                self.focused = Some(hit.key);
                (true, None)
            }
        }
    }
    /// Paste into the focused input only; never into the underlying composer.
    pub fn paste(&mut self, text: &str) {
        if let Some(entry) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
        {
            entry.edit(text, false);
        }
    }
    fn key(&mut self, key: KeyEvent, can_send: bool) -> (bool, Option<String>) {
        if key.kind != KeyEventKind::Press
            || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            return (false, None);
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::F(2)) {
            self.unfocus();
            return (true, None);
        }
        let Some(entry) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
        else {
            return (true, None);
        };
        let controls = entry.controls();
        if controls.is_empty() {
            return (false, None);
        }
        entry.focus %= controls.len();
        match key.code {
            KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                entry.focus = (entry.focus + 1) % controls.len();
            }
            KeyCode::BackTab | KeyCode::Tab | KeyCode::Up | KeyCode::Left => {
                entry.focus = (entry.focus + controls.len() - 1) % controls.len();
            }
            KeyCode::Down | KeyCode::Right => entry.focus = (entry.focus + 1) % controls.len(),
            KeyCode::Enter => {
                if matches!(controls[entry.focus], Control::Input(_)) {
                    entry.focus = (entry.focus + 1) % controls.len();
                } else {
                    return (false, entry.activate(entry.focus, can_send));
                }
            }
            KeyCode::Char(ch) if matches!(controls[entry.focus], Control::Input(_)) => {
                entry.edit(&ch.to_string(), false);
            }
            KeyCode::Backspace => entry.edit("", true),
            KeyCode::Char(' ') => return (false, entry.activate(entry.focus, can_send)),
            _ => {}
        }
        (false, None)
    }
    fn popup(&mut self, f: &mut Frame, app_theme: &Theme, area: Rect, can_send: bool) {
        self.hits.clear();
        self.screen = Some(f.area());
        let Some(key) = self.focused.clone() else {
            return;
        };
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        let (canvas, hits) = entry.canvas(area.width, app_theme, true, can_send);
        if let Some((rect, _)) = hits.iter().find(|(_, control)| *control == entry.focus) {
            if rect.bottom() > entry.scroll + area.height {
                entry.scroll = rect.bottom().saturating_sub(area.height);
            }
            if rect.y < entry.scroll {
                entry.scroll = rect.y;
            }
        }
        let height = canvas.area.height;
        entry.scroll = entry.scroll.min(height.saturating_sub(area.height));
        for y in 0..area.height.min(height) {
            for x in 0..area.width {
                f.buffer_mut()[(area.x + x, area.y + y)] = canvas[(x, y + entry.scroll)].clone();
            }
        }
        for (rect, control) in hits {
            let low = rect.y.max(entry.scroll);
            let high = rect.bottom().min(entry.scroll + area.height);
            if low < high {
                self.hits.push(Hit {
                    rect: Rect::new(
                        area.x + rect.x,
                        area.y + low - entry.scroll,
                        rect.width,
                        high - low,
                    ),
                    key: key.clone(),
                    control: Some(control),
                });
            }
        }
    }
}
fn safe(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\u{FE0F}' && (!ch.is_control() || *ch == '\n'))
        .collect()
}
/// A proposed response must not race a reply or overwrite the user's draft.
#[must_use]
pub(crate) fn can_send(app: &AppView) -> bool {
    !app.viewing_stream() && app.input_text().is_empty()
}
fn review(app: &mut AppView, message: Option<String>) {
    if let Some(text) = message {
        // Never promote model-authored text into a user instruction unseen,
        // overwrite a draft, or route it through the slash-command parser.
        if !can_send(app) {
            return;
        }
        app.set_input(&text);
        app.popup = Popup::None;
        app.interactive.unfocus();
        app.push_status("review the UI response — press Enter to send, or edit/discard it");
    }
}
/// Focus the newest visible model-authored output, without a slash command.
pub fn focus(app: &mut AppView) {
    if app.interactive.focus_latest() {
        app.popup = Popup::InteractiveUi;
        app.sel.clear();
    } else {
        app.push_status("no interactive output in view");
    }
}
/// Route inline/popup controls before transcript selection or composer input.
pub fn mouse(app: &mut AppView, event: MouseEvent, screen: Rect) -> bool {
    let (handled, message) = app.interactive.click(event, screen, can_send(app));
    if app.interactive.focused.is_some() {
        app.popup = Popup::InteractiveUi;
    }
    if handled {
        app.sel.clear();
    }
    review(app, message);
    handled
}
/// Focused keyboard events stay inside the selected document.
pub fn key(app: &mut AppView, event: KeyEvent) {
    let (closed, message) = app.interactive.key(event, can_send(app));
    if closed {
        app.popup = Popup::None;
    }
    review(app, message);
}
/// Render a keyboard-accessible enlarged view of the same inline state.
pub fn render_popup(f: &mut Frame, app: &mut AppView) {
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::TALL.1);
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "interactive output"),
        "Tab/arrows focus · Enter activate · type to edit · Esc back",
        app,
        true,
        chrome::Tone::Normal,
    );
    let enabled = can_send(app);
    app.interactive.popup(f, &app.theme, inner, enabled);
}

#[cfg(test)]
mod tests;
