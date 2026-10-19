//! Model-authored UI lifecycle, rendering and input. Source remains in normal
//! messages; temporary state is keyed by message, fence ordinal and revision.

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::popups::chrome;
use crate::ui::style::glyph;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use nexus_core::app::Popup;
use nexus_core::interactive::{Document, Node, Session, Tone};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Paragraph, Sparkline, Widget, Wrap};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

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
    Toggle(String),
    Select(usize),
    Table(usize, String),
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
                    .enumerate()
                    .flat_map(|(index, node)| match node {
                        Node::Buttons { items, .. } => items
                            .iter()
                            .map(|button| Control::Action(button.action.clone()))
                            .collect(),
                        Node::Input { key, .. } => vec![Control::Input(key.clone())],
                        Node::Toggle { key, .. } => vec![Control::Toggle(key.clone())],
                        Node::Checklist { items } => items
                            .iter()
                            .map(|item| Control::Toggle(item.key.clone()))
                            .collect(),
                        Node::Select { .. } => vec![Control::Select(index)],
                        Node::Table {
                            select: Some(key), ..
                        } => vec![Control::Table(index, key.clone())],
                        _ => Vec::new(),
                    })
                    .collect()
            },
        )
    }
    fn activate(&mut self, control: usize, can_send: bool) -> Option<String> {
        self.focus = control;
        let control = self.controls().get(control).cloned()?;
        let Ok(session) = &mut self.session else {
            return None;
        };
        let result = match control {
            Control::Action(action) => {
                if session.document().sends(&action) && !can_send {
                    self.error =
                        "Finish the reply and clear your draft before reviewing a UI response."
                            .into();
                    return None;
                }
                session.activate(&action)
            }
            Control::Toggle(key) => session.toggle(&key).map(|()| None),
            Control::Select(node) => session.cycle(node, true).map(|()| None),
            Control::Input(_) | Control::Table(..) => return None,
        };
        match result {
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
    fn navigate(&mut self, code: KeyCode) -> bool {
        let Some(control) = self.controls().get(self.focus).cloned() else {
            return false;
        };
        let Ok(session) = &mut self.session else {
            return false;
        };
        let result = match (control, code) {
            (Control::Select(node), KeyCode::Left | KeyCode::Right) => {
                session.cycle(node, code == KeyCode::Right)
            }
            (Control::Table(node, key), KeyCode::Up | KeyCode::Down) => {
                let row = session.selected_row(&key);
                session.select_row(
                    node,
                    if code == KeyCode::Down {
                        row.saturating_add(1)
                    } else {
                        row.saturating_sub(1)
                    },
                )
            }
            _ => return false,
        };
        self.error = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        true
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
                _ => {
                    let new_hits = render_component(
                        node,
                        session,
                        region,
                        theme,
                        focused.then_some(self.focus),
                        control,
                        buffer,
                    );
                    control += new_hits.len();
                    hits.extend(new_hits);
                }
            }
            y += node.rows();
        }
        hits
    }
}
fn display(session: &Session, template: &str) -> String {
    safe(&session.text(template).unwrap_or_default()).replace('\n', " ")
}
fn control_style(theme: &Theme, selected: bool) -> Style {
    Style::default().fg(if selected { theme.accent } else { theme.fg })
}
fn render_toggle(
    session: &Session,
    key: &str,
    label: &str,
    area: Rect,
    style: Style,
    buffer: &mut Buffer,
) {
    let mark = if session.value(key) == "true" {
        glyph::DOT
    } else {
        glyph::RING
    };
    Paragraph::new(format!("{mark} {}", display(session, label)))
        .style(style)
        .render(area, buffer);
}
fn render_component(
    node: &Node,
    session: &Session,
    region: Rect,
    theme: &Theme,
    focus: Option<usize>,
    mut control: usize,
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    match node {
        Node::Checklist { items } => {
            for (index, item) in items.iter().enumerate() {
                let row = Rect::new(
                    region.x,
                    region.y + u16::try_from(index).unwrap_or(0),
                    region.width,
                    1,
                );
                render_toggle(
                    session,
                    &item.key,
                    &item.label,
                    row,
                    control_style(theme, focus == Some(control)),
                    buffer,
                );
                hits.push((row, control));
                control += 1;
            }
        }
        Node::Toggle { key, label } => {
            render_toggle(
                session,
                key,
                label,
                region,
                control_style(theme, focus == Some(control)),
                buffer,
            );
            hits.push((region, control));
        }
        Node::Select { key, label, .. } => {
            Paragraph::new(format!(
                "{}: < {} >",
                display(session, label),
                display(session, &session.value(key))
            ))
            .style(control_style(theme, focus == Some(control)))
            .render(region, buffer);
            hits.push((region, control));
        }
        Node::Table { select, .. } => {
            render_table(
                node,
                session,
                region,
                theme,
                focus == Some(control) && select.is_some(),
                buffer,
            );
            if select.is_some() {
                hits.push((region, control));
            }
        }
        _ => render_data(node, session, region, theme, buffer),
    }
    hits
}
fn clipped(text: &str, width: u16) -> String {
    if text.width() <= usize::from(width) {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + size >= usize::from(width) {
            break;
        }
        result.push(ch);
        used += size;
    }
    result.push('…');
    result
}
fn render_table(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    focused: bool,
    buffer: &mut Buffer,
) {
    let Node::Table {
        columns,
        rows_data,
        select,
    } = node
    else {
        return;
    };
    let count = u16::try_from(columns.len()).unwrap_or(1);
    let selected = select.as_ref().map(|key| session.selected_row(key));
    for (row, cells) in std::iter::once(columns).chain(rows_data.iter()).enumerate() {
        let is_selected = row > 0 && selected == Some(row - 1);
        let style = if row == 0 {
            Style::default().fg(theme.fg_dim)
        } else {
            control_style(theme, focused && is_selected)
        };
        let marker = if is_selected { glyph::SELECTED } else { " " };
        let y = area.y + u16::try_from(row).unwrap_or(0);
        let lead = u16::from(select.is_some()).min(area.width);
        if lead > 0 {
            buffer.set_string(area.x, y, marker, style);
        }
        let width = area.width.saturating_sub(lead);
        for (column, cell) in cells.iter().enumerate() {
            let column = u16::try_from(column).unwrap_or(0);
            let x = width * column / count;
            let end = width * (column + 1) / count;
            let cell_width = (end - x).saturating_sub(u16::from(column + 1 < count));
            Paragraph::new(clipped(&display(session, cell), cell_width))
                .style(style)
                .render(Rect::new(area.x + lead + x, y, cell_width, 1), buffer);
        }
    }
}
fn render_bar(
    label: &str,
    value: f64,
    max: f64,
    metadata_width: usize,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    let label_width = area.width / 3;
    let metadata = format!("{value}/{max}");
    let meta_width = u16::try_from(metadata_width)
        .unwrap_or(0)
        .min(area.width.saturating_sub(label_width));
    let bar_width = area.width.saturating_sub(label_width + meta_width + 2);
    let ratio = if max > 0.0 {
        (value / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (0..bar_width)
        .take_while(|n| f64::from(*n) + 0.5 <= ratio * f64::from(bar_width))
        .count();
    Paragraph::new(clipped(label, label_width))
        .style(Style::default().fg(theme.fg_dim))
        .render(Rect::new(area.x, area.y, label_width, 1), buffer);
    Paragraph::new(glyph::SECTION.repeat(filled))
        .style(Style::default().fg(theme.accent))
        .render(
            Rect::new(area.x + label_width + 1, area.y, bar_width, 1),
            buffer,
        );
    Paragraph::new(metadata)
        .right_aligned()
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(
                area.right().saturating_sub(meta_width),
                area.y,
                meta_width,
                1,
            ),
            buffer,
        );
}
fn render_data(node: &Node, session: &Session, area: Rect, theme: &Theme, buffer: &mut Buffer) {
    match node {
        Node::Kv { items } => {
            let labels: Vec<_> = items
                .iter()
                .map(|pair| display(session, &pair[0]))
                .collect();
            let label_width = u16::try_from(labels.iter().map(|s| s.width()).max().unwrap_or(0))
                .unwrap_or(u16::MAX)
                .min(area.width / 2);
            for (index, (pair, label)) in items.iter().zip(&labels).enumerate() {
                let y = area.y + u16::try_from(index).unwrap_or(0);
                Paragraph::new(clipped(label, label_width))
                    .style(Style::default().fg(theme.fg_dim))
                    .render(Rect::new(area.x, y, label_width, 1), buffer);
                let offset = (label_width + 1).min(area.width);
                Paragraph::new(display(session, &pair[1]))
                    .style(Style::default().fg(theme.fg))
                    .render(
                        Rect::new(area.x + offset, y, area.width - offset, 1),
                        buffer,
                    );
            }
        }
        Node::Progress { label, value, max } => {
            let (value, max) = session.progress(value, max).unwrap_or_default();
            render_bar(
                &display(session, label),
                value,
                max,
                format!("{value}/{max}").width(),
                area,
                theme,
                buffer,
            );
        }
        Node::Bars { items } => {
            let values: Vec<_> = items
                .iter()
                .map(|item| session.number(&item.value).unwrap_or(0.0).max(0.0))
                .collect();
            let max = values.iter().copied().fold(0.0, f64::max);
            let metadata_width = values
                .iter()
                .map(|value| format!("{value}/{max}").width())
                .max()
                .unwrap_or(0);
            for (index, (item, value)) in items.iter().zip(values).enumerate() {
                render_bar(
                    &display(session, &item.label),
                    value,
                    max,
                    metadata_width,
                    Rect::new(
                        area.x,
                        area.y + u16::try_from(index).unwrap_or(0),
                        area.width,
                        1,
                    ),
                    theme,
                    buffer,
                );
            }
        }
        Node::Sparkline { values, .. } => {
            let values: Vec<_> = values
                .iter()
                .map(|value| session.number(value).unwrap_or(0.0).max(0.0))
                .collect();
            let max = values.iter().copied().fold(0.0, f64::max);
            // Normalize before integer conversion to preserve even tiny fractional samples.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let values: Vec<_> = values
                .iter()
                .map(|value| {
                    if max > 0.0 {
                        (value / max * 1000.0) as u64
                    } else {
                        0
                    }
                })
                .collect();
            let bars = ratatui::symbols::bar::Set {
                empty: " ",
                one_eighth: glyph::RAIL,
                one_quarter: glyph::RAIL,
                three_eighths: glyph::RAIL,
                half: glyph::SECTION,
                five_eighths: glyph::SECTION,
                three_quarters: glyph::SECTION,
                seven_eighths: glyph::SECTION,
                full: glyph::SECTION,
            };
            Sparkline::default()
                .data(values)
                .bar_set(bars)
                .style(Style::default().fg(theme.accent))
                .render(area, buffer);
        }
        _ => {}
    }
}
#[derive(Clone)]
struct Hit {
    rect: Rect,
    key: Key,
    control: Option<usize>,
    row_offset: u16,
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
                            row_offset: u16::try_from(low).unwrap_or(0).saturating_sub(rect.y),
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
            Some((index, Control::Table(node, _))) => {
                entry.focus = index;
                self.focused = Some(hit.key);
                if let Ok(session) = &mut entry.session {
                    let row = hit.row_offset + mouse.row - hit.rect.y;
                    if row > 0 {
                        let result = session.select_row(node, usize::from(row - 1));
                        entry.error = result
                            .err()
                            .map_or_else(String::new, |error| error.to_string());
                    }
                }
                (true, None)
            }
            Some((index, Control::Toggle(_) | Control::Select(_))) => {
                self.focused = Some(hit.key);
                (true, entry.activate(index, can_send))
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
        if entry.navigate(key.code) {
            return (false, None);
        }
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
                    row_offset: low.saturating_sub(rect.y),
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

#[cfg(test)]
#[path = "interactive/components_tests.rs"]
mod components_tests;
