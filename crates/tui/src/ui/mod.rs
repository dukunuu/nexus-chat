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
mod diff;
pub mod history;
mod input;
pub mod markdown;
pub mod popups;
mod status;
pub(crate) mod style;
mod welcome;

use history::render_history;
use input::{render_at_popup, render_command_popup, render_input};
pub(crate) use status::short_model_label;
use status::{render_notifications, render_status};

pub fn render(f: &mut Frame, app: &mut AppView) {
    // Popups re-record their list geometry as they draw; a frame without a
    // list must not leave a stale hit area behind.
    app.list_hit.set(None);
    app.popup_hit.set(None);
    let send_ready = crate::interactive::can_send(app);
    app.interactive.begin_frame(send_ready);
    crate::interactive::sync_saved(app);
    // The image viewer keeps the focused document for when it closes.
    let viewing_from_ui = app.popup == Popup::ImageViewer
        && app.image_viewer.return_to() == crate::image_viewer::Return::Interactive;
    if app.popup != Popup::InteractiveUi && !viewing_from_ui {
        app.interactive.unfocus();
    }
    // Paint the base first so widgets that only set foreground colors inherit
    // the configured opaque or terminal-transparent surface.
    f.render_widget(
        Block::default().style(app.theme.background_style()),
        f.area(),
    );

    // Transcript, composer, and status share one reading column.
    let column = reading_column(f.area(), &app.settings.column_width);

    // Grow the input box with its wrapped content (1–20 rows) plus 2 for the
    // border. `measure` wants the width the widget renders at: inside the
    // border and its one-column padding on each side.
    let inner_w = column.width.saturating_sub(4);
    let content_rows = app.input.measure(inner_w).preferred_rows;
    // A pending approval takes the composer's place (Claude Code-style) and
    // sizes it to the request, so the conversation stays readable above.
    let approving = app.popup == Popup::Approval;
    let input_h = if approving {
        popups::approval::docked_height(app, column)
    } else {
        content_rows.saturating_add(2)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),          // history
            Constraint::Length(input_h), // input (auto-height, max 22)
            Constraint::Length(1),       // status (with inline context bar)
        ])
        .split(column);

    render_history(f, app, chunks[0]);
    crate::resize::paint_column_handles(f, app, column);
    if approving {
        popups::approval::render_in(f, app, chunks[1]);
    } else {
        render_input(f, app, chunks[1]);
    }
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
    if app.popup != Popup::None && !approving {
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
        Popup::InteractiveUi => crate::interactive::render_popup(f, app),
        Popup::TuiApp => popups::tui_app::render(f, app),
        Popup::Watch => popups::watches::render(f, app),
        Popup::ResearchLive => popups::research_live::render(f, app),
        Popup::Swarm => popups::swarm::render(f, app),
        Popup::Usage => popups::usage::render(f, app),
        Popup::Jobs => popups::jobs::render(f, app),
        Popup::Harness => popups::harness::render(f, app),
        Popup::Procs => popups::procs::render(f, app),
        Popup::Login => popups::login::render(f, app),
        Popup::Local => popups::local::render(f, app),
        Popup::Help => popups::help::render(f, app),
        Popup::ImageViewer => popups::image_viewer::render(f, app),
        Popup::Connectors => popups::connectors::render(f, app),
        // An approval is drawn in the composer's place above.
        Popup::Approval | Popup::None => {}
    }
    crate::resize::paint_popup_resize(f, app);
}

/// Widest the `auto` column grows, and the narrowest it shrinks to once the
/// terminal is wide enough to center it.
const AUTO_COLUMN_MIN: u16 = 112;
const AUTO_COLUMN_MAX: u16 = 180;
/// Terminals up to this wide use the whole width in `auto` mode.
const AUTO_FULL_UNTIL: u16 = 120;

/// The centered column the conversation lives in, sized by the
/// `column_width` setting: `full` (the width minus a one-cell margin each
/// side), a cell count (capped to the screen), or `auto` — full width on
/// ordinary terminals, and 85% of the width (112..=180 cells) on wide ones, so
/// a big screen isn't mostly empty gutter and lines still read comfortably.
pub(crate) fn reading_column(area: Rect, setting: &str) -> Rect {
    if area.width <= 40 {
        return area;
    }
    let full = area.width - 2;
    let auto = if area.width <= AUTO_FULL_UNTIL {
        full
    } else {
        let wide = u32::from(area.width) * 85 / 100;
        (wide as u16).clamp(AUTO_COLUMN_MIN, AUTO_COLUMN_MAX)
    };
    let width = match setting {
        "full" => full,
        "auto" => auto,
        n => n.parse::<u16>().map_or(auto, |n| n.min(full)),
    };
    Rect {
        x: area.x + (area.width - width) / 2,
        width,
        ..area
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
#[must_use]
pub(crate) fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_view::AppView;

    /// (x, width) of the column for a `width`-wide, 30-row screen.
    fn column(width: u16, setting: &str) -> (u16, u16) {
        let c = reading_column(Rect::new(0, 0, width, 30), setting);
        (c.x, c.width)
    }

    #[test]
    fn auto_uses_the_full_width_up_to_120_then_grows_to_85_percent() {
        // Up to 120 cells: the whole width minus a one-cell margin each side.
        assert_eq!(column(80, "auto"), (1, 78));
        assert_eq!(column(114, "auto"), (1, 112));
        assert_eq!(column(120, "auto"), (1, 118));
        // Wider: 85% of the screen, never under 112 or over 180, centered.
        assert_eq!(column(121, "auto"), (4, 112));
        assert_eq!(column(160, "auto"), (12, 136));
        assert_eq!(column(200, "auto"), (15, 170));
        assert_eq!(column(300, "auto"), (60, 180));
        assert_eq!(column(1000, "auto"), (410, 180));
    }

    #[test]
    fn full_and_numbers_follow_the_setting_and_clamp_to_the_screen() {
        assert_eq!(column(200, "full"), (1, 198));
        assert_eq!(column(200, "100"), (50, 100));
        assert_eq!(column(200, "140"), (30, 140));
        // A number past the screen fills it (minus the margins) instead.
        assert_eq!(column(120, "220"), (1, 118));
        assert_eq!(column(200, "400"), (1, 198));
        // Garbage falls back to auto rather than collapsing the layout.
        assert_eq!(column(200, "wide"), column(200, "auto"));
        assert_eq!(column(200, ""), column(200, "auto"));
    }

    #[test]
    fn tiny_screens_use_the_whole_area_for_every_setting() {
        for setting in ["auto", "full", "100"] {
            assert_eq!(column(40, setting), (0, 40), "{setting}");
            assert_eq!(column(20, setting), (0, 20), "{setting}");
        }
    }

    #[test]
    fn column_keeps_the_area_origin_and_height() {
        let area = Rect::new(3, 2, 200, 30);
        let c = reading_column(area, "auto");
        assert_eq!((c.y, c.height), (2, 30));
        assert_eq!(c.x, 3 + 15);
    }

    fn message(role: &str, content: &str) -> nexus_core::db::Message {
        nexus_core::db::Message {
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        }
    }

    #[test]
    fn a_wide_screen_lays_the_conversation_out_in_the_auto_column() {
        use nexus_core::app::App;
        let root = std::env::temp_dir().join(format!("nexus-wide-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let db = nexus_core::db::Db::open_in_memory().unwrap();
        let mut app = AppView::new(App::new(db, Some("k"), nexus_core::space::Space { root }));
        app.messages
            .push(message("user", "How wide should this column be?"));
        app.messages.push(message(
            "assistant",
            &"A wide terminal should not leave most of the screen empty, so the column grows with it. "
                .repeat(4),
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 16)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..16)
            .map(|y| (0..200).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        // Everything lives in cells 15..185, with the long reply wrapped at
        // the column rather than at the old 112.
        let widest = rows
            .iter()
            .map(|r| r.trim_end().chars().count())
            .max()
            .unwrap();
        assert!(
            widest > 15 + 112 && widest <= 185,
            "{widest}\n{}",
            rows.join("\n")
        );
        assert!(rows.iter().all(|r| r.chars().take(15).all(|c| c == ' ')));
        println!("{}", rows.join("\n"));
    }
}
