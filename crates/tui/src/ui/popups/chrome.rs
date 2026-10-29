//! Shared popup chrome: rounded blocks, consistent list selection, and small
//! title helpers for browse / edit / confirm modal states.

// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap,
};

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::style::width;

/// Canonical popup sizes — every popup picks one so the whole family reads
/// as the same design: small prompts, standard lists, tall lists, wide
/// workhorses (and the model picker's fixed dual-pane layout).
pub const SMALL: (u16, u16) = (56, 40);
pub const STANDARD: (u16, u16) = (64, 60);
pub const TALL: (u16, u16) = (64, 74);
pub const WIDE: (u16, u16) = (78, 66);

/// Where a centered popup was drawn last frame: its rect, its id in
/// `popup_sizes`, and the default percentages it falls back to. Mouse
/// resizing hit-tests against this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupHit {
    pub rect: Rect,
    pub id: &'static str,
    pub default: (u16, u16),
}

/// Smallest a resized popup gets, in cells (or the whole screen if smaller).
pub const MIN_POPUP_W: u16 = 40;
pub const MIN_POPUP_H: u16 = 10;

/// The rect popup `id` occupies on `screen`: its persisted size if the user
/// dragged one, else the `default` percentages. Pure, so mouse hit-testing
/// can ask where a popup is without drawing it.
pub fn popup_rect(app: &AppView, screen: Rect, id: &str, default: (u16, u16)) -> Rect {
    let custom = nexus_core::app::popup_size(&app.settings.popup_sizes, id);
    let (pct_w, pct_h) = custom.unwrap_or(default);
    let rect = crate::ui::centered(screen, pct_w, pct_h);
    if custom.is_none() {
        return rect;
    }
    // A saved percentage must stay usable on a small terminal: never below
    // the minimum, never off the screen.
    let w = rect.width.max(MIN_POPUP_W.min(screen.width));
    let h = rect.height.max(MIN_POPUP_H.min(screen.height));
    Rect {
        x: screen.x + (screen.width - w) / 2,
        y: screen.y + (screen.height - h) / 2,
        width: w,
        height: h,
    }
}

/// [`popup_rect`] for a popup that is about to draw: also records the rect so
/// a border press can start a resize. Every centered popup gets its area here.
pub fn popup_area(app: &AppView, screen: Rect, id: &'static str, default: (u16, u16)) -> Rect {
    let rect = popup_rect(app, screen, id, default);
    app.popup_hit.set(Some(PopupHit { rect, id, default }));
    rect
}

/// Horizontal padding inside every popup frame, in cells per side. Row-width
/// math that starts from a popup's outer width subtracts `2 * PAD`.
pub const PAD: u16 = 1;

/// Frame tone: Normal (focused border brightens) or Danger (error-colored
/// border, for destructive confirm states — matches `danger_title`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Danger,
}

/// The rounded border + title style shared by every popup. The interior uses
/// the configured UI surface, which can be either the active theme's opaque
/// background or the terminal's default background.
pub fn popup_block_focused<'a>(
    title: impl Into<Line<'a>>,
    theme: &Theme,
    focused: bool,
    tone: Tone,
) -> Block<'a> {
    let border = match tone {
        Tone::Normal if focused => theme.border,
        Tone::Normal => theme.border_dim,
        Tone::Danger => theme.error,
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(theme.background_style())
        .padding(ratatui::widgets::Padding::horizontal(PAD))
        .title(title)
}

/// The popup frame plus the standard bottom-right hint bar (dim, hidden by
/// `hide_hints`). Every list popup renders through this, so the hint lives
/// in the same place everywhere instead of crowding the title. Long hints
/// are truncated with `…` to the frame's inner width.
pub fn hinted_block<'a>(
    title: impl Into<Line<'a>>,
    hint: &str,
    app: &AppView,
    focused: bool,
    tone: Tone,
    width: u16,
) -> Block<'a> {
    let mut block = popup_block_focused(title, &app.theme, focused, tone);
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        let max = (width.saturating_sub(4)) as usize; // +2 for the border, +2 for the hint padding
        let text = truncate(hint, max);
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {text} "),
                Style::default().fg(app.theme.fg_dim),
            ))
            .right_aligned(),
        );
    }
    block
}

/// Clear a popup area and render its rounded frame, returning the inner rect.
pub fn render_frame<'a>(
    f: &mut Frame,
    area: Rect,
    title: impl Into<Line<'a>>,
    theme: &Theme,
    focused: bool,
    tone: Tone,
) -> Rect {
    f.render_widget(Clear, area);
    let block = popup_block_focused(title, theme, focused, tone);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Like [`render_frame`] but with the standard bottom hint bar.
pub fn render_hinted<'a>(
    f: &mut Frame,
    area: Rect,
    title: impl Into<Line<'a>>,
    hint: &str,
    app: &AppView,
    focused: bool,
    tone: Tone,
) -> Rect {
    f.render_widget(Clear, area);
    let block = hinted_block(title, hint, app, focused, tone, area.width);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// One keyboard-labelled action card. Its hit area exists only while enabled.
pub struct Button<'a, A> {
    /// Opaque caller-owned action returned by hit-testing.
    pub action: A,
    /// Visible label, including the keyboard shortcut.
    pub label: &'a str,
    /// Optional artwork; its last line is the compact description.
    pub body: &'a [&'a str],
    /// Whether this card holds keyboard focus.
    pub selected: bool,
    /// Whether activation is currently permitted.
    pub enabled: bool,
    /// The document's resolved primary action.
    pub primary: bool,
}

/// Render compact pills or artwork tiles in measured regions. The returned
/// hit areas correspond exactly to this frame, not guessed row positions.
pub fn render_buttons<A: Copy>(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    buttons: &[Button<'_, A>],
    theme: &Theme,
) -> Vec<(Rect, A)> {
    if buttons.is_empty() || area.is_empty() {
        return Vec::new();
    }
    let columns = usize::from((area.width / 16).max(1)).min(buttons.len());
    let row_count = buttons.len().div_ceil(columns);
    let rows = Layout::vertical(vec![
        Constraint::Ratio(
            1,
            u32::try_from(row_count).unwrap_or(u32::MAX)
        );
        row_count
    ])
    .split(area);
    let areas: Vec<_> = rows
        .iter()
        .zip(buttons.chunks(columns))
        .flat_map(|(row, items)| {
            let count = u32::try_from(items.len()).unwrap_or(u32::MAX);
            Layout::horizontal(vec![Constraint::Ratio(1, count); items.len()])
                .spacing(1)
                .split(*row)
                .to_vec()
        })
        .collect();
    let mut hits = Vec::new();
    for (button, rect) in buttons.iter().zip(areas.iter().copied()) {
        if rect.is_empty() {
            continue;
        }
        let label = crate::interactive::safe(button.label);
        if rect.height > 1 && buttons.len() == 1 || !button.body.is_empty() {
            render_tile(buffer, rect, &label, button.body, button.selected, theme);
        } else {
            let mut style = Style::default()
                .fg(if !button.enabled {
                    theme.fg_dim
                } else if button.primary {
                    theme.bg
                } else {
                    theme.fg
                })
                .bg(if button.primary && button.enabled {
                    theme.accent
                } else {
                    theme.control
                });
            if button.primary {
                style = style.add_modifier(Modifier::BOLD);
            }
            if button.selected {
                style = if button.primary {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style.fg(theme.accent)
                }
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(theme.accent);
            }
            Paragraph::new(fit_line(
                Line::from(format!(" {label} ")),
                usize::from(rect.width),
            ))
            .style(style)
            .render(Rect::new(rect.x, rect.y, rect.width, 1), buffer);
        }
        if button.enabled {
            hits.push((rect, button.action));
        }
    }
    hits
}

fn render_tile(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    label: &str,
    body: &[&str],
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered().border_style(Style::default().fg(if focused {
        theme.accent
    } else {
        theme.border_dim
    }));
    let inner = block.inner(area);
    block.render(area, buffer);
    let art = body.len().saturating_sub(1);
    for (row, text) in body.iter().take(art).enumerate() {
        Paragraph::new(crate::interactive::safe(text))
            .centered()
            .style(Style::default().fg(theme.fg))
            .render(
                Rect::new(
                    inner.x,
                    inner.y + u16::try_from(row).unwrap_or(0),
                    inner.width,
                    1,
                ),
                buffer,
            );
    }
    let text_width = inner.width.saturating_sub(2);
    Paragraph::new(fit_line(Line::from(label), usize::from(text_width)))
        .style(Style::default().fg(theme.fg).add_modifier(Modifier::BOLD))
        .render(
            Rect::new(
                inner.x + 1,
                inner.y + u16::try_from(art).unwrap_or(0),
                text_width,
                1,
            ),
            buffer,
        );
    if let Some(metadata) = body.last() {
        Paragraph::new(fit_line(
            Line::from(crate::interactive::safe(metadata)),
            usize::from(text_width),
        ))
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(
                inner.x + 1,
                inner.y + u16::try_from(art).unwrap_or(0) + 1,
                text_width,
                1,
            ),
            buffer,
        );
    }
}

/// Standard popup list selection: accent `▸ ` marker, bold text, and the
/// theme's raised shade across the row.
pub fn standard_list<'a>(items: Vec<ListItem<'a>>, theme: &Theme) -> List<'a> {
    List::new(items)
        .highlight_symbol(Span::styled(
            format!("{} ", crate::ui::style::glyph::SELECTED),
            Style::default().fg(theme.accent),
        ))
        .highlight_style(
            Style::default()
                .bg(theme.raised)
                .add_modifier(Modifier::BOLD),
        )
}

/// Truncate `s` to `max` display columns, appending `…` when it overflows —
/// the same ellipsis language as the hint bar, for row names across popups.
pub fn truncate(s: &str, max: usize) -> String {
    let max = max.max(1);
    if width(s) <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        take_cols(s.chars(), max - 1)
            .into_iter()
            .collect::<String>()
    )
}

/// Truncate `s` to `max` display columns by cutting the middle — for file
/// names, where the start tells files apart and the end carries the
/// descriptive slug and extension (`b0db75e4-2…_reasoning_model_what.png`).
pub fn truncate_middle(s: &str, max: usize) -> String {
    let max = max.max(1);
    if width(s) <= max {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep / 3;
    let start: String = take_cols(s.chars(), head).into_iter().collect();
    let end: String = take_cols(s.chars().rev(), keep - width(&start))
        .into_iter()
        .rev()
        .collect();
    format!("{start}…{end}")
}

/// The leading chars of `chars` that fit in `cols` display columns.
fn take_cols(chars: impl Iterator<Item = char>, cols: usize) -> Vec<char> {
    let mut used = 0;
    chars
        .take_while(|&c| {
            used += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            used <= cols
        })
        .collect()
}

/// Fit a styled line to `width` columns: unchanged when it fits, otherwise
/// cut across spans with a trailing `…` in the style of the span it lands in —
/// so narrow terminals read "0 cache wr…" instead of a silent mid-word cut.
pub fn fit_line(line: Line<'_>, cols: usize) -> Line<'_> {
    let total: usize = line.spans.iter().map(|s| width(&s.content)).sum();
    if total <= cols {
        return line;
    }
    let mut spans = Vec::with_capacity(line.spans.len());
    if cols > 0 {
        let mut left = cols - 1;
        for span in line.spans {
            let n = width(&span.content);
            if n <= left {
                left -= n;
                spans.push(span);
            } else {
                let kept: String = take_cols(span.content.chars(), left).into_iter().collect();
                spans.push(Span::styled(format!("{kept}…"), span.style));
                break;
            }
        }
    }
    Line { spans, ..line }
}

/// [`fit_line`] over every line.
pub fn fit_lines(lines: Vec<Line<'_>>, width: u16) -> Vec<Line<'_>> {
    lines
        .into_iter()
        .map(|l| fit_line(l, width as usize))
        .collect()
}

/// Where a popup list was drawn last frame, for mouse clicks: row `r` of
/// `area` shows item `offset + (r - area.y) / item_lines`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListHit {
    pub area: Rect,
    pub item_lines: u16,
    pub offset: usize,
    pub selected: Option<usize>,
    pub total: usize,
}

impl ListHit {
    /// The item index under a screen cell, if the cell is on a list row.
    pub fn index_at(&self, column: u16, row: u16) -> Option<usize> {
        if !self
            .area
            .contains(ratatui::layout::Position::new(column, row))
        {
            return None;
        }
        let index = self.offset + usize::from((row - self.area.y) / self.item_lines.max(1));
        (index < self.total).then_some(index)
    }
}

/// Render a popup list statefully with a right-edge scrollbar when the
/// content overflows. `total` is the number of items, `item_lines` how many
/// terminal rows each occupies (1 for single-line rows, 3 for two-line+
/// blank rows) — used to compute the visible count so the thumb is honest.
/// The list renders one column narrower than `area` so the gutter never
/// overlaps row text; the scrollbar follows `state.offset`, which the List
/// widget updates during its own render.
pub fn render_list(
    f: &mut Frame,
    list: List<'_>,
    state: &mut ListState,
    area: Rect,
    total: usize,
    item_lines: u16,
    app: &AppView,
) {
    let theme = &app.theme;
    let list_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(1).max(1),
        height: area.height,
    };
    f.render_stateful_widget(list, list_area, state);
    app.list_hit.set(Some(ListHit {
        area: list_area,
        item_lines: item_lines.max(1),
        offset: state.offset(),
        selected: state.selected(),
        total,
    }));
    let viewport = (area.height / item_lines.max(1)) as usize;
    if total > viewport {
        let gutter = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut sb_state = ratatui::widgets::ScrollbarState::new(total)
            .viewport_content_length(viewport)
            .position(state.offset());
        let sb =
            ratatui::widgets::Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(theme.accent))
                .track_style(Style::default().fg(theme.border_dim));
        f.render_stateful_widget(sb, gutter, &mut sb_state);
    }
}

/// A dim italic placeholder row for empty lists, so every popup's empty
/// state looks intentional rather than blank.
pub fn empty_placeholder(message: &str, theme: &Theme) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        message.to_string(),
        Style::default()
            .fg(theme.fg_dim)
            .add_modifier(Modifier::ITALIC),
    )))
}

/// `"12 items · "`-style prefix for footer hints (singular-aware).
pub fn count_hint(n: usize, label: &str) -> String {
    format!("{n} {label}{} · ", if n == 1 { "" } else { "s" })
}

fn titled_line(app: &AppView, text: impl Into<String>, color: Color, hint: &str) -> Line<'static> {
    let text = text.into();
    let mut spans = vec![Span::styled(
        format!(" {text} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        spans.push(Span::styled(
            format!("— {hint} "),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    Line::from(spans)
}

/// Browse-mode title for a filterable list popup: a per-popup glyph
/// (accent2) + `label` alone when the filter is empty, `label: <filter>▏`
/// (live cursor) while typing.
pub fn filter_title(app: &AppView, label: impl Into<String>, filter: &str) -> Line<'static> {
    if filter.is_empty() {
        return Line::from(crate::ui::style::title(&app.theme, label));
    }
    Line::from(vec![
        Span::styled(
            format!(" {}: ", label.into()),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{filter}▏ "), Style::default().fg(app.theme.fg)),
    ])
}

/// The standard popup title: a per-popup glyph (accent2) + bold accent name.
/// Every popup picks its own glyph so the family reads distinct at a glance
/// while staying visually identical in frame.
pub fn popup_title(app: &AppView, name: impl Into<String>) -> Line<'static> {
    Line::from(crate::ui::style::title(&app.theme, name))
}

/// A title for text-entry popups: label + live value + trailing cursor.
pub fn input_title(
    app: &AppView,
    label: impl Into<String>,
    value: impl AsRef<str>,
    hint: &str,
) -> Line<'static> {
    let label = label.into();
    let mut spans = vec![Span::styled(
        format!(" {label}: "),
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        format!("{}▏", value.as_ref()),
        Style::default().fg(app.theme.fg),
    ));
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        spans.push(Span::styled(
            format!("  ({hint}) "),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    Line::from(spans)
}

fn confirmish_title(
    app: &AppView,
    text: impl Into<String>,
    color: Color,
    hint: &str,
) -> Line<'static> {
    titled_line(app, text, color, hint)
}

/// Destructive prompt title (error color).
pub fn danger_title(app: &AppView, text: impl Into<String>, hint: &str) -> Line<'static> {
    confirmish_title(app, text, app.theme.error, hint)
}

/// Max rows the detail strip will grow to before truncating.
const MAX_DETAIL_ROWS: u16 = 4;

/// Split a popup's inner area into (list area, detail-strip area). The strip
/// is sized to `desc`'s wrapped height (0 when `desc` is empty, so an
/// item with no description doesn't waste a row).
pub fn split_with_detail(area: Rect, desc: &str) -> (Rect, Rect) {
    if desc.trim().is_empty() {
        return (area, Rect { height: 0, ..area });
    }
    let inner_w = area.width.max(1) as usize;
    let wrapped = textwrap::wrap(desc, inner_w).len().max(1) as u16;
    let desc_h = wrapped.min(MAX_DETAIL_ROWS) + 1; // +1 for the divider
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(desc_h)]).split(area);
    (chunks[0], chunks[1])
}

/// Render the wrapped description strip: a dim divider rule, then plain
/// wrapped text — no markdown, no per-word styling.
pub fn render_detail(f: &mut Frame, area: Rect, desc: &str, theme: &Theme) {
    if area.height == 0 || desc.trim().is_empty() {
        return;
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border_dim));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let p = Paragraph::new(desc.to_string())
        .style(Style::default().fg(theme.fg_dim))
        .wrap(Wrap { trim: true });
    f.render_widget(p, inner);
}
