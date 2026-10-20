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
use nexus_core::interactive::{Button, Document, Node, Session, Tone};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Sparkline, Widget, Wrap};
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
        let regions = self
            .session
            .as_ref()
            .map_or_else(|_| Vec::new(), |session| node_regions(session, width));
        let height = self.session.as_ref().map_or(6, |_| card_height(&regions));
        let mut buffer = card_buffer(
            width,
            height,
            self.session
                .as_ref()
                .map_or("UI unavailable", |s| s.document().title()),
            theme,
            if focused {
                "Tab next · Enter press · Esc done"
            } else {
                "F2 keyboard"
            },
        );
        let area = buffer.area;
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
        if !self.error.is_empty() {
            Paragraph::new(safe(&self.error))
                .style(Style::default().fg(theme.error))
                .render(Rect::new(2, 1, width.saturating_sub(4), 1), &mut buffer);
        }
        let mut hits = render_nodes(
            session,
            &regions,
            theme,
            focused.then_some(self.focus),
            can_send,
            &mut buffer,
        );
        if width >= 8 {
            let expand = Rect::new(width - 4, 0, 3, 1);
            if focused && self.focus == session_controls(session) {
                Paragraph::new(" ⤢ ")
                    .style(
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::UNDERLINED),
                    )
                    .render(expand, &mut buffer);
            }
            hits.push((expand, session_controls(session)));
        }
        (buffer, hits)
    }
    fn nodes(
        session: &Session,
        regions: &[Rect],
        theme: &Theme,
        focus: Option<usize>,
        can_send: bool,
        buffer: &mut Buffer,
    ) -> Vec<(Rect, usize)> {
        let mut hits = Vec::new();
        let mut control = 0;
        let primary = primary_button(session.document());
        for (index, (node, region)) in session.document().nodes().iter().zip(regions).enumerate() {
            let region = *region;
            match node {
                Node::Text { text, tone, .. } => {
                    let color = match tone {
                        Tone::Normal | Tone::Accent => theme.fg,
                        Tone::Muted => theme.fg_dim,
                        Tone::Success => theme.success,
                        Tone::Warning => theme.warning,
                        Tone::Error => theme.error,
                    };
                    Paragraph::new(text_lines(session, text, region.width))
                        .style(Style::default().fg(color))
                        .render(region, buffer);
                }
                Node::Buttons { items, .. } => {
                    for (item, rect) in button_regions(session, items, region) {
                        let button = &items[item];
                        let selected = focus == Some(control + item);
                        let enabled = can_send || !session.document().sends(&button.action);
                        let label = button_label(session, button);
                        let body: Vec<_> = button.body.iter().map(String::as_str).collect();
                        hits.extend(chrome::render_buttons(
                            buffer,
                            rect,
                            &[chrome::Button {
                                action: control + item,
                                label: &label,
                                body: &body,
                                selected,
                                primary: primary == Some((index, item)),
                                enabled,
                            }],
                            theme,
                        ));
                    }
                    control += items.len();
                }
                Node::Input { key, label } => {
                    let selected = focus == Some(control);
                    let label = clipped(
                        &safe(label),
                        (region.width / 2).min(region.width.saturating_sub(18)),
                    );
                    let offset = u16::try_from(label.width())
                        .unwrap_or(0)
                        .saturating_add(2)
                        .min(region.width.saturating_sub(16));
                    Paragraph::new(label)
                        .style(Style::default().fg(theme.fg_dim))
                        .render(region, buffer);
                    let value = format!(
                        "{}{}",
                        safe(&session.value(key)),
                        if selected { "▁" } else { "" }
                    );
                    let scroll = if selected {
                        u16::try_from(value.width())
                            .unwrap_or(u16::MAX)
                            .saturating_sub(region.width - offset)
                    } else {
                        0
                    };
                    Paragraph::new(value)
                        .scroll((0, scroll))
                        .style(
                            control_style(theme, selected)
                                .bg(theme.raised)
                                .add_modifier(Modifier::UNDERLINED)
                                .underline_color(if selected {
                                    theme.accent
                                } else {
                                    theme.border_dim
                                }),
                        )
                        .render(
                            Rect::new(region.x + offset, region.y, region.width - offset, 1),
                            buffer,
                        );
                    hits.push((region, control));
                    control += 1;
                }
                _ => {
                    let new_hits =
                        render_component(node, session, region, theme, focus, control, buffer);
                    control += new_hits.len();
                    hits.extend(new_hits);
                }
            }
        }
        hits
    }
}
/// Render validated nodes into their measured, padded regions.
pub(crate) fn render_nodes(
    session: &Session,
    regions: &[Rect],
    theme: &Theme,
    focus: Option<usize>,
    can_send: bool,
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    Entry::nodes(session, regions, theme, focus, can_send, buffer)
}
fn text_lines(session: &Session, text: &str, width: u16) -> Vec<Line<'static>> {
    safe(&session.text(text).unwrap_or_else(|error| error.to_string()))
        .split('\n')
        .flat_map(|line| {
            crate::ui::markdown::wrap_styled_line(&Line::from(line), usize::from(width))
        })
        .collect()
}
fn primary_button(document: &Document) -> Option<(usize, usize)> {
    let buttons = || {
        document
            .nodes()
            .iter()
            .enumerate()
            .flat_map(|(node, value)| {
                let items = match value {
                    Node::Buttons { items, .. } => items.as_slice(),
                    _ => &[],
                };
                items
                    .iter()
                    .enumerate()
                    .map(move |(item, button)| ((node, item), button))
            })
    };
    buttons()
        .find(|(_, button)| button.primary)
        .or_else(|| buttons().find(|(_, button)| document.sends(&button.action)))
        .map(|(index, _)| index)
}
fn session_controls(session: &Session) -> usize {
    session
        .document()
        .nodes()
        .iter()
        .map(|node| match node {
            Node::Buttons { items, .. } => items.len(),
            Node::Checklist { items } => items.len(),
            Node::Input { .. }
            | Node::Toggle { .. }
            | Node::Select { .. }
            | Node::Table {
                select: Some(_), ..
            } => 1,
            _ => 0,
        })
        .sum()
}
fn button_label(session: &Session, button: &Button) -> String {
    format!(
        "{}{}",
        display(session, &button.label),
        if session.document().sends(&button.action) {
            " ↵"
        } else {
            ""
        }
    )
}
fn button_regions(session: &Session, items: &[Button], area: Rect) -> Vec<(usize, Rect)> {
    let mut regions = Vec::new();
    if items.iter().any(|button| !button.body.is_empty()) {
        let content_width = items
            .iter()
            .flat_map(|button| {
                std::iter::once(button_label(session, button).width())
                    .chain(button.body.iter().map(|line| safe(line).width()))
            })
            .max()
            .unwrap_or(0);
        let minimum_width = u16::try_from(content_width)
            .unwrap_or(u16::MAX)
            .saturating_add(4);
        let columns =
            usize::from((area.width.saturating_add(2) / minimum_width.saturating_add(2)).max(1))
                .min(items.len());
        let count = u16::try_from(columns).unwrap_or(1);
        let width = area.width.saturating_sub((count - 1) * 2) / count;
        let mut y = area.y;
        for (row, chunk) in items.chunks(columns).enumerate() {
            let height = chunk
                .iter()
                .map(|button| u16::try_from(button.body.len()).unwrap_or(0) + 3)
                .max()
                .unwrap_or(3);
            for item in 0..chunk.len() {
                let x = area.x + u16::try_from(item).unwrap_or(0) * (width + 2);
                regions.push((row * columns + item, Rect::new(x, y, width, height)));
            }
            y = y.saturating_add(height).saturating_add(1);
        }
    } else {
        let mut x = area.x;
        let mut y = area.y;
        let order = (0..items.len())
            .filter(|i| !session.document().sends(&items[*i].action))
            .chain((0..items.len()).filter(|i| session.document().sends(&items[*i].action)));
        for item in order {
            let width = u16::try_from(
                button_label(session, &items[item])
                    .width()
                    .saturating_add(2),
            )
            .unwrap_or(u16::MAX)
            .min(area.width);
            if x > area.x && x.saturating_add(width) > area.right() {
                x = area.x;
                y = y.saturating_add(1);
            }
            if session.document().sends(&items[item].action) {
                let remaining: u16 = items[item..]
                    .iter()
                    .filter(|b| session.document().sends(&b.action))
                    .map(|b| {
                        u16::try_from(button_label(session, b).width() + 3)
                            .unwrap_or(u16::MAX)
                            .min(area.width)
                    })
                    .fold(0, u16::saturating_add);
                x = x.max(area.right().saturating_sub(remaining.saturating_sub(1)));
            }
            regions.push((item, Rect::new(x, y, width, 1)));
            x = x.saturating_add(width).saturating_add(1);
        }
    }
    regions
}
/// Measure actual content at the available width; declared rows remain core caps.
pub(crate) fn node_regions(session: &Session, width: u16) -> Vec<Rect> {
    let width = width.saturating_sub(4);
    let mut y: u16 = 2;
    let mut previous_text = false;
    session
        .document()
        .nodes()
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let text = matches!(node, Node::Text { .. });
            if index > 0 && !(text && previous_text) {
                y = y.saturating_add(1);
            }
            previous_text = text;
            let height = match node {
                Node::Text { text, .. } => u16::try_from(text_lines(session, text, width).len())
                    .unwrap_or(u16::MAX)
                    .max(1),
                Node::Buttons { items, .. } => {
                    button_regions(session, items, Rect::new(2, 0, width, 0))
                        .iter()
                        .map(|(_, rect)| rect.bottom())
                        .max()
                        .unwrap_or(1)
                }
                Node::Input { .. } => 1,
                _ => node.rows(),
            };
            let region = Rect::new(
                2,
                y,
                width,
                height.min(u16::MAX.saturating_sub(y.saturating_add(2))),
            );
            y = region.bottom();
            region
        })
        .collect()
}
pub(crate) fn card_height(regions: &[Rect]) -> u16 {
    regions
        .last()
        .map_or(4, |rect| rect.bottom().saturating_add(2))
}
pub(crate) fn source_height(source: &str, width: u16) -> u16 {
    Document::parse(source)
        .and_then(Session::new)
        .map_or(6, |session| card_height(&node_regions(&session, width)))
}
pub(crate) fn card_buffer(
    width: u16,
    height: u16,
    title: &str,
    theme: &Theme,
    hint: &str,
) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    buffer.set_style(area, theme.background_style());
    chrome::popup_block_focused(
        Line::from(Span::styled(
            format!("─ {} ", clipped(&safe(title), width.saturating_sub(12))),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        )),
        theme,
        false,
        chrome::Tone::Normal,
    )
    .title_top(Line::from(Span::styled(" ⤢ ", Style::default().fg(theme.fg_dim))).right_aligned())
    .title_bottom(
        Line::from(Span::styled(
            format!(" {} ", clipped(hint, width.saturating_sub(6))),
            Style::default().fg(theme.fg_dim),
        ))
        .right_aligned(),
    )
    .render(area, &mut buffer);
    buffer
}
fn display(session: &Session, template: &str) -> String {
    safe(&session.text(template).unwrap_or_default()).replace('\n', " ")
}
fn control_style(theme: &Theme, selected: bool) -> Style {
    let style = Style::default().fg(if selected { theme.accent } else { theme.fg });
    if selected {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style
    }
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
    expanded: bool,
    pinned: bool,
    layout: Option<(Rect, usize, Vec<Placement>)>,
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
        self.expanded = false;
        self.pinned = false;
    }
    /// Focus the last visible document, if any.
    pub fn focus_latest(&mut self) -> bool {
        self.focused = self.visible.last().cloned();
        self.expanded = false;
        self.pinned = false;
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
    /// Keep transcript reservations in sync with locally edited, wrapping content.
    pub(crate) fn reflow(
        &mut self,
        cache: &mut crate::history_cache::HistoryCache,
        top: usize,
        height: u16,
    ) {
        for index in 0..cache.widgets.len() {
            let placement = &cache.widgets[index];
            if placement.complete
                && placement.row < top + usize::from(height)
                && placement.row + usize::from(placement.height) > top
            {
                self.register(&placement.key());
            }
            let Some(Ok(session)) = self
                .entries
                .get(&placement.key())
                .map(|entry| &entry.session)
            else {
                continue;
            };
            let height = card_height(&node_regions(session, placement.width));
            if height == placement.height {
                continue;
            }
            let start = placement.row;
            let old_height = placement.height;
            let old_end = start + usize::from(old_height);
            let count = usize::from(height);
            let owner = cache.owner[start];
            let code = cache.code[start];
            cache
                .lines
                .splice(start..old_end, vec![Line::default(); count]);
            cache
                .plain
                .splice(start..old_end, vec![String::new(); count]);
            cache.owner.splice(start..old_end, vec![owner; count]);
            cache.code.splice(start..old_end, vec![code; count]);
            cache
                .image_at_line
                .splice(start..old_end, vec![None; count]);
            for diagram in &mut cache.diagrams {
                if diagram.row >= old_end {
                    diagram.row = diagram.row - usize::from(old_height) + count;
                }
            }
            for widget in &mut cache.widgets[index + 1..] {
                widget.row = widget.row - usize::from(old_height) + count;
            }
            cache.widgets[index].height = height;
        }
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
        self.layout = Some((viewport, top, placements.to_vec()));
        if self
            .focused
            .as_ref()
            .is_some_and(|key| !placements.iter().any(|p| p.complete && p.key() == *key))
        {
            self.unfocus();
        }
        if self.focused.as_ref().is_some_and(|key| {
            placements.iter().any(|p| {
                p.key() == *key
                    && (p.row + usize::from(p.height) <= top
                        || p.row >= top + usize::from(viewport.height))
            })
        }) {
            self.pinned = true;
        }
        let mut ordered: Vec<_> = placements.iter().collect();
        ordered.sort_by_key(|p| self.pinned && self.focused.as_ref() == Some(&p.key()));
        for placement in ordered {
            if !placement.complete {
                continue;
            }
            let key = placement.key();
            let pinned = self.pinned && self.focused.as_ref() == Some(&key);
            let row = if pinned { top } else { placement.row };
            let first = top.saturating_sub(row).min(usize::from(placement.height));
            let end = (top + usize::from(viewport.height))
                .saturating_sub(row)
                .min(usize::from(placement.height));
            let width = placement.width.min(viewport.width.saturating_sub(2));
            if first >= end || width == 0 {
                continue;
            }
            self.register(&key);
            let Some(entry) = self.entries.get(&key) else {
                let y = viewport.y + u16::try_from(row.saturating_sub(top)).unwrap_or(0);
                f.render_widget(
                    Paragraph::new("UI instance limit reached; start a new session.")
                        .style(Style::default().fg(theme.error)),
                    Rect::new(viewport.x + 2, y, width, 1),
                );
                continue;
            };
            self.visible.push(key.clone());
            let focused = self.focused.as_ref() == Some(&key);
            let (mut canvas, hits) = entry.canvas(width, theme, focused, self.send_ready);
            let end = end.min(usize::from(canvas.area.height));
            if pinned {
                let overlay = Rect::new(
                    viewport.x + 2,
                    viewport.y,
                    width,
                    u16::try_from(end).unwrap_or(0),
                );
                self.hits
                    .retain(|hit| hit.rect.intersection(overlay).is_empty());
                let bottom = u16::try_from(end).unwrap_or(0).saturating_sub(1);
                let border = card_buffer(width, bottom + 1, "", theme, "▴ pinned · Esc to unpin");
                for x in 0..width {
                    canvas[(x, bottom)] = border[(x, bottom)].clone();
                }
            }
            if first >= end {
                continue;
            }
            for offset in first..end {
                let y = viewport.y + u16::try_from(row + offset - top).unwrap_or(0);
                for x in 0..width {
                    f.buffer_mut()[(viewport.x + 2 + x, y)] =
                        canvas[(x, u16::try_from(offset).unwrap_or(0))].clone();
                }
            }
            if active {
                for (rect, control) in hits {
                    let low = usize::from(rect.y).max(first);
                    let high =
                        usize::from(rect.bottom()).min(end.saturating_sub(usize::from(pinned)));
                    if low < high {
                        self.hits.push(Hit {
                            rect: Rect::new(
                                viewport.x + 2 + rect.x,
                                viewport.y + u16::try_from(row + low - top).unwrap_or(0),
                                rect.width,
                                u16::try_from(high - low).unwrap_or(0),
                            ),
                            key: key.clone(),
                            control: Some(control),
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
        if self.focused.as_ref() != Some(&hit.key) {
            self.pinned = false;
        }
        self.focused = Some(hit.key.clone());
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
                entry.focus = entry.controls().len();
                self.expanded = true;
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
        let count = controls.len() + 1;
        entry.focus %= count;
        if entry.focus == controls.len() && matches!(key.code, KeyCode::Enter | KeyCode::Char(' '))
        {
            self.expanded = true;
            return (false, None);
        }
        if entry.navigate(key.code) {
            return (false, None);
        }
        match key.code {
            KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                entry.focus = (entry.focus + 1) % count;
            }
            KeyCode::BackTab | KeyCode::Tab | KeyCode::Up | KeyCode::Left => {
                entry.focus = (entry.focus + count - 1) % count;
            }
            KeyCode::Down | KeyCode::Right => entry.focus = (entry.focus + 1) % count,
            KeyCode::Enter => {
                if matches!(controls.get(entry.focus), Some(Control::Input(_))) {
                    entry.focus = (entry.focus + 1) % count;
                } else {
                    return (false, entry.activate(entry.focus, can_send));
                }
            }
            KeyCode::Char(ch) if matches!(controls.get(entry.focus), Some(Control::Input(_))) => {
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
        f.render_widget(Clear, area);
        f.buffer_mut().set_style(area, app_theme.background_style());
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
pub(crate) fn safe(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\u{FE0F}')
        .map(|ch| {
            if ch.is_control() && ch != '\n' {
                ch.escape_default().to_string()
            } else {
                ch.to_string()
            }
        })
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
        app.push_status(
            "UI response in the composer for review — edit/discard it, or press Enter to send",
        );
    }
}
/// Focus the newest visible model-authored output, without a slash command.
pub fn focus(app: &mut AppView) {
    if app.interactive.focus_latest() {
        app.popup = Popup::InteractiveUi;
        app.sel.clear();
        app.push_status("UI send actions prepare a message in the composer for review");
    } else {
        app.push_status("no interactive output in view");
    }
}
/// Route inline/popup controls before transcript selection or composer input.
pub fn mouse(app: &mut AppView, event: MouseEvent, screen: Rect) -> bool {
    if app.popup == Popup::InteractiveUi {
        match event.kind {
            MouseEventKind::ScrollUp => {
                app.scroll = app.scroll.saturating_add(3).min(app.max_scroll);
                return true;
            }
            MouseEventKind::ScrollDown => {
                app.scroll = app.scroll.saturating_sub(3);
                return true;
            }
            MouseEventKind::Down(MouseButton::Left)
                if app
                    .input_inner
                    .contains(Position::new(event.column, event.row)) =>
            {
                app.interactive.unfocus();
                app.popup = Popup::None;
                return true;
            }
            _ => {}
        }
    }
    let (handled, message) = app.interactive.click(event, screen, can_send(app));
    if app.interactive.focused.is_some() {
        if handled && app.popup != Popup::InteractiveUi {
            app.push_status("UI send actions prepare a message in the composer for review");
        }
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
    if event.kind != KeyEventKind::Press
        || !event.modifiers.difference(KeyModifiers::SHIFT).is_empty()
    {
        return;
    }
    match event.code {
        KeyCode::PageUp => {
            app.scroll = app
                .scroll
                .saturating_add(app.history_page())
                .min(app.max_scroll);
            return;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(app.history_page());
            return;
        }
        _ => {}
    }
    let (closed, message) = app.interactive.key(event, can_send(app));
    if closed {
        app.popup = Popup::None;
    }
    review(app, message);
}
/// Render a keyboard-accessible enlarged view of the same inline state.
pub fn render_popup(f: &mut Frame, app: &mut AppView) {
    let Some((viewport, top, placements)) = app.interactive.layout.clone() else {
        return;
    };
    if app.interactive.expanded {
        let enabled = can_send(app);
        app.interactive.popup(f, &app.theme, viewport, enabled);
    } else {
        // The popup dispatcher dims the base view. Repaint just this card at
        // full strength while retaining inline keyboard ownership.
        app.interactive.hits.clear();
        app.interactive
            .paint(f, viewport, top, &placements, &app.theme, true);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "interactive/components_tests.rs"]
mod components_tests;
