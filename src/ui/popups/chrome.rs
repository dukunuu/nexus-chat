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
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::app::App;
use crate::theme::Theme;

/// The rounded border + title style shared by every popup.
pub fn popup_block_focused<'a>(
    title: impl Into<Line<'a>>,
    theme: &Theme,
    focused: bool,
) -> Block<'a> {
    let border = if focused {
        theme.border
    } else {
        theme.border_dim
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(title)
}

/// Clear a popup area and render its rounded frame, returning the inner rect.
pub fn render_frame<'a>(
    f: &mut Frame,
    area: Rect,
    title: impl Into<Line<'a>>,
    theme: &Theme,
    focused: bool,
) -> Rect {
    f.render_widget(Clear, area);
    let block = popup_block_focused(title, theme, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Standard popup list selection styling: `▸ ` marker + bold selection.
pub fn standard_list(items: Vec<ListItem<'_>>) -> List<'_> {
    List::new(items)
        .highlight_symbol("▸ ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
}

fn titled_line(app: &App, text: impl Into<String>, color: Color, hint: &str) -> Line<'static> {
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

/// A normal popup title with optional hint text.
pub fn hinted_title(app: &App, text: impl Into<String>, hint: &str) -> Line<'static> {
    titled_line(app, text, app.theme.accent, hint)
}

/// A title for text-entry popups: label + live value + trailing cursor.
pub fn input_title(
    app: &App,
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

fn confirmish_title(app: &App, text: impl Into<String>, color: Color, hint: &str) -> Line<'static> {
    titled_line(app, text, color, hint)
}

/// Confirmation prompt title (warning color).
pub fn confirm_title(app: &App, text: impl Into<String>, hint: &str) -> Line<'static> {
    confirmish_title(app, text, app.theme.warning, hint)
}

/// Destructive prompt title (error color).
pub fn danger_title(app: &App, text: impl Into<String>, hint: &str) -> Line<'static> {
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
