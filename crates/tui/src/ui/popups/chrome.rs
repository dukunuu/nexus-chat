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
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

use nexus_core::app::App;
use nexus_core::theme::{Theme, blend};

/// Canonical popup sizes — every popup picks one so the whole family reads
/// as the same design: small prompts, standard lists, tall lists, wide
/// workhorses (and the model picker's fixed dual-pane layout).
pub const SMALL: (u16, u16) = (56, 40);
pub const STANDARD: (u16, u16) = (64, 60);
pub const TALL: (u16, u16) = (64, 74);
pub const WIDE: (u16, u16) = (78, 66);

/// Frame tone: Normal (focused border brightens) or Danger (error-colored
/// border, for destructive confirm states — matches `danger_title`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Danger,
}

/// The rounded border + title style shared by every popup. The block also
/// carries a subtle surface tint (theme bg blended toward the border color)
/// so popups read as cards floating above the conversation, matching the
/// user-message bubbles.
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
        .style(Style::default().bg(blend(theme.bg, theme.border, 0.05)))
        .title(title)
}

/// The popup frame plus the standard bottom-right hint bar (dim, hidden by
/// `hide_hints`). Every list popup renders through this, so the hint lives
/// in the same place everywhere instead of crowding the title. Long hints
/// are truncated with `…` to the frame's inner width.
pub fn hinted_block<'a>(
    title: impl Into<Line<'a>>,
    hint: &str,
    app: &App,
    focused: bool,
    tone: Tone,
    width: u16,
) -> Block<'a> {
    let mut block = popup_block_focused(title, &app.theme, focused, tone);
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        let max = (width.saturating_sub(4)) as usize; // +2 for the border, +2 for the hint padding
        let text: String = if hint.chars().count() > max {
            let keep = max.saturating_sub(1);
            format!("{}…", hint.chars().take(keep).collect::<String>())
        } else {
            hint.to_string()
        };
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
    app: &App,
    focused: bool,
    tone: Tone,
) -> Rect {
    f.render_widget(Clear, area);
    let block = hinted_block(title, hint, app, focused, tone, area.width);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Standard popup list selection: accent `▸ ` marker + bold selection on a
/// subtle accent-tinted row — the same surface language as the chat cards.
pub fn standard_list<'a>(items: Vec<ListItem<'a>>, theme: &Theme) -> List<'a> {
    List::new(items)
        .highlight_symbol(Span::styled("▸ ", Style::default().fg(theme.accent)))
        .highlight_style(
            Style::default()
                .bg(blend(theme.bg, theme.accent, 0.08))
                .add_modifier(Modifier::BOLD),
        )
}

/// Truncate `s` to `max` chars, appending `…` when it overflows — the same
/// ellipsis language as the hint bar, for row names across popups.
pub fn truncate(s: &str, max: usize) -> String {
    let max = max.max(1);
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}…", s.chars().take(keep).collect::<String>())
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
    theme: &Theme,
) {
    let list_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(1).max(1),
        height: area.height,
    };
    f.render_stateful_widget(list, list_area, state);
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

/// Browse-mode title for a filterable list popup: a per-popup glyph
/// (accent2) + `label` alone when the filter is empty, `label: <filter>▏`
/// (live cursor) while typing.
pub fn filter_title(
    app: &App,
    glyph: &str,
    label: impl Into<String>,
    filter: &str,
) -> Line<'static> {
    let label = label.into();
    let mut spans = vec![
        Span::styled(format!(" {glyph} "), Style::default().fg(app.theme.accent2)),
        Span::styled(
            label,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !filter.is_empty() {
        spans.push(Span::styled(
            format!(": {filter}▏"),
            Style::default().fg(app.theme.fg),
        ));
    }
    Line::from(spans)
}

/// The standard popup title: a per-popup glyph (accent2) + bold accent name.
/// Every popup picks its own glyph so the family reads distinct at a glance
/// while staying visually identical in frame.
pub fn popup_title(app: &App, glyph: &str, name: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {glyph} "), Style::default().fg(app.theme.accent2)),
        Span::styled(
            name.into(),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ])
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
