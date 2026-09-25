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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Block;

use crate::app_view::AppView;
use nexus_core::app::Popup;

mod cards;
pub mod citations_style;
pub mod history;
mod input;
pub mod markdown;
pub mod popups;
mod status;
mod welcome;

use history::render_history;
use input::{render_at_popup, render_command_popup, render_input};
pub(crate) use status::short_model_label;
use status::{render_notifications, render_status};

pub fn render(f: &mut Frame, app: &mut AppView) {
    // Popups re-record their list geometry as they draw; a frame without a
    // list must not leave a stale hit area behind.
    app.list_hit.set(None);
    // Paint the base first so widgets that only set foreground colors inherit
    // the configured opaque or terminal-transparent surface.
    f.render_widget(
        Block::default().style(app.theme.background_style()),
        f.area(),
    );

    // Transcript, composer, and status share one reading column.
    let column = reading_column(f.area());

    // Grow the input box with its wrapped content (1–20 rows) plus 2 for the
    // border. `measure` wants the width the widget renders at: inside the
    // border and its one-column padding on each side.
    let inner_w = column.width.saturating_sub(4);
    let content_rows = app.input.measure(inner_w).preferred_rows;
    let input_h = content_rows.saturating_add(2);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),          // history
            Constraint::Length(input_h), // input (auto-height, max 22)
            Constraint::Length(1),       // status (with inline context bar)
        ])
        .split(column);

    render_history(f, app, chunks[0]);
    render_input(f, app, chunks[1]);
    render_status(f, app, chunks[2]);
    // Autocomplete floats above the input; only when no modal popup is open.
    if app.popup == Popup::None {
        render_command_popup(f, app, chunks[1]);
        render_at_popup(f, app, chunks[1]);
        render_notifications(f, app, chunks[0]);
    } else {
        app.notification_areas.clear();
    }

    // Recede everything behind an open popup into the quiet border color, so
    // the modal reads as the focus even on a transparent terminal (every
    // popup clears its own rect first, so it stays at full strength).
    if app.popup != Popup::None {
        let area = f.area();
        f.buffer_mut().set_style(
            area,
            Style::default()
                .fg(app.theme.border_dim)
                .remove_modifier(Modifier::BOLD),
        );
    }

    match app.popup {
        Popup::Model => popups::model::render(f, app),
        Popup::Session => popups::session::render(f, app),
        Popup::Copy => popups::copy::render(f, app),
        Popup::Key => popups::key::render(f, app),
        Popup::Settings => popups::settings::render(f, app),
        Popup::Space => popups::space::render(f, app),
        Popup::Context => popups::context::render(f, app),
        Popup::Skills => popups::skills::render(f, app),
        Popup::Files => popups::files::render(f, app),
        Popup::Apps => popups::apps::render(f, app),
        Popup::Watch => popups::watches::render(f, app),
        Popup::ResearchLive => popups::research_live::render(f, app),
        Popup::Swarm => popups::swarm::render(f, app),
        Popup::Usage => popups::usage::render(f, app),
        Popup::Login => popups::login::render(f, app),
        Popup::Local => popups::local::render(f, app),
        Popup::Help => popups::help::render(f, app),
        Popup::None => {}
    }
}

/// Widest the reading column gets. Past this, lines get too long to read
/// comfortably, so wide terminals center the column instead of stretching.
const COLUMN_MAX: u16 = 112;

/// The centered column the conversation lives in: `COLUMN_MAX` wide on big
/// terminals, else the full width minus a one-column margin each side.
pub(crate) fn reading_column(area: Rect) -> Rect {
    if area.width > COLUMN_MAX + 2 {
        Rect {
            x: area.x + (area.width - COLUMN_MAX) / 2,
            width: COLUMN_MAX,
            ..area
        }
    } else if area.width > 40 {
        Rect {
            x: area.x + 1,
            width: area.width - 2,
            ..area
        }
    } else {
        area
    }
}

pub fn dim(s: impl Into<String>, theme: &crate::theme::Theme) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(theme.fg_dim))
}

/// Map core's abstract spinner palette onto terminal colors.
pub fn to_color(c: nexus_core::app::SpinnerColor) -> Color {
    match c {
        nexus_core::app::SpinnerColor::Green => Color::Green,
        nexus_core::app::SpinnerColor::Cyan => Color::Cyan,
        nexus_core::app::SpinnerColor::Magenta => Color::Magenta,
    }
}

/// Compact token counts: 940, 1.2k, 128k, 1.0m.
fn humanize(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    }
}

/// `$0.0042`, `$1.23`, `$0.000012`, or `—` when the price is unknown.
/// Small-but-real costs keep enough decimals that they never read `$0.0000`.
fn fmt_cost(cost: Option<f64>) -> String {
    match cost {
        Some(c) if c > 0.0 && c < 1.0 => {
            let four = format!("${c:.4}");
            if four == "$0.0000" {
                format!("${c:.6}")
            } else {
                four
            }
        }
        Some(c) => format!("${c:.2}"),
        None => "—".to_string(),
    }
}

/// Short absolute timestamp from an rfc3339 string (falls back to the raw text).
pub(super) fn fmt_created(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339).map_or_else(
        |_| rfc3339.to_string(),
        |dt| {
            dt.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        },
    )
}

/// A rect `pct_w` × `pct_h` percent of `area`, centered.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_h) / 2),
            Constraint::Percentage(pct_h),
            Constraint::Percentage((100 - pct_h) / 2),
        ])
        .split(area);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_w) / 2),
            Constraint::Percentage(pct_w),
            Constraint::Percentage((100 - pct_w) / 2),
        ])
        .split(v[1]);
    h[1]
}
