//! Shared popup chrome: the rounded, theme-colored border + bold title every
//! popup now uses, plus an optional wrapped "description of the selected row"
//! strip along the bottom (settings, skills) — no markdown, just wrapped
//! plain text, so a long description doesn't get cut off mid-word.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use crate::theme::Theme;

/// The rounded border + bold accent title shared by every popup.
pub(crate) fn popup_block<'a>(title: impl Into<Line<'a>>, theme: &Theme) -> Block<'a> {
    let title = title.into().style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD));
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
        .title(title)
}

/// Max rows the detail strip will grow to before truncating.
const MAX_DETAIL_ROWS: u16 = 4;

/// Split a popup's inner area into (list area, detail-strip area). The strip
/// is sized to `desc`'s wrapped height (0 when `desc` is empty, so an
/// item with no description doesn't waste a row).
pub(crate) fn split_with_detail(area: Rect, desc: &str) -> (Rect, Rect) {
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
pub(crate) fn render_detail(f: &mut Frame, area: Rect, desc: &str, theme: &Theme) {
    if area.height == 0 || desc.trim().is_empty() {
        return;
    }
    let block = Block::default().borders(Borders::TOP).border_style(Style::default().fg(theme.border_dim));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let p = Paragraph::new(desc.to_string())
        .style(Style::default().fg(theme.fg_dim))
        .wrap(Wrap { trim: true });
    f.render_widget(p, inner);
}
