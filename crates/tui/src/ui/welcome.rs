//! The start screen: banner, greeting, clock, and the recent-session table.

// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::dim;
use super::history::rule_line;
use crate::app_view::AppView;

/// The empty start screen: a rounded panel holding the gradient banner, a
/// random greeting, a live clock, quick-start chips, and the most recent
/// sessions.
pub(super) fn render_welcome(f: &mut Frame, app: &mut AppView, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    // Per-line gradient across the accent ramp: accent -> accent2.
    let banner_lines: Vec<&str> = app.banner.lines().collect();
    let n = banner_lines.len().max(1);
    for (i, l) in banner_lines.into_iter().enumerate() {
        let t = if n > 1 {
            i as f32 / (n - 1) as f32
        } else {
            0.0
        };
        let color = ramp(app.theme.accent, app.theme.accent2, t);
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.greeting.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(dim(
        chrono::Local::now()
            .format("%A, %B %-d %Y · %H:%M:%S")
            .to_string(),
        &app.theme,
    )));
    if !app.settings.hide_hints {
        lines.push(Line::from(""));
        lines.push(chip_row(
            &["/research", "/swarm", "/model", "/help"],
            &app.theme,
        ));
    }
    // Most recent sessions across this space, as a quick-jump list: click a
    // row or press Alt+1…4. Row indices are remembered to map clicks back.
    let mut recent_rows: Vec<(usize, String)> = Vec::new();
    if let Ok(sessions) = app.db.list_sessions(&app.active_space.id) {
        let recent: Vec<_> = sessions.into_iter().take(4).collect();
        if !recent.is_empty() {
            lines.push(Line::from(""));
            // The panel is at most 86 wide; the rule spans the table below
            // it, so both center on the same axis.
            let inner_w = area.width.min(86).saturating_sub(4) as usize;
            // One fixed-width table, centered as a block: numbers, titles,
            // and right-aligned dates line up instead of zigzagging.
            let row_w = inner_w.min(60);
            rule_line(&mut lines, "recent", row_w, &app.theme);
            for (i, s) in recent.iter().enumerate() {
                let when = super::fmt_created(&s.created_at);
                let title_w = row_w.saturating_sub(4 + when.chars().count());
                let title = crate::ui::popups::chrome::truncate(&s.title, title_w);
                let pad = title_w.saturating_sub(title.chars().count());
                recent_rows.push((lines.len(), s.id.clone()));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} ", i + 1),
                        Style::default()
                            .fg(app.theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(title, Style::default().fg(app.theme.fg)),
                    Span::raw(" ".repeat(pad + 2)),
                    Span::styled(when, Style::default().fg(app.theme.fg_dim)),
                ]));
            }
            if !app.settings.hide_hints {
                lines.push(Line::from(dim("Alt+1–4 or click to reopen", &app.theme)));
            }
        }
    }

    let panel_w = area.width.min(86);
    let panel_h = (lines.len() + 2).min(area.height as usize) as u16;
    let panel = Rect {
        x: area.x + area.width.saturating_sub(panel_w) / 2,
        y: area.y + area.height.saturating_sub(panel_h) / 2,
        width: panel_w,
        height: panel_h,
    };
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.border_dim));
    let inner = block.inner(panel);
    f.render_widget(block, panel);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
    app.welcome_targets = recent_rows
        .into_iter()
        .filter_map(|(row, id)| {
            let y = inner.y + u16::try_from(row).ok()?;
            (y < inner.bottom()).then(|| (Rect::new(inner.x, y, inner.width, 1), id))
        })
        .collect();
}

/// Linear blend between two colors at `t` in 0.0..=1.0.
fn ramp(a: Color, b: Color, t: f32) -> Color {
    let mix = |x: u8, y: u8| {
        let (xf, yf) = (f32::from(x), f32::from(y));
        (xf + (yf - xf) * t).round() as u8
    };
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => a,
    }
}

/// `[ /cmd ]` chips in a dim bracket style, separated by two spaces.
fn chip_row(cmds: &[&str], theme: &crate::theme::Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, cmd) in cmds.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("[ ", Style::default().fg(theme.border_dim)));
        spans.push(Span::styled(
            cmd.to_string(),
            Style::default().fg(theme.accent),
        ));
        spans.push(Span::styled(" ]", Style::default().fg(theme.border_dim)));
    }
    Line::from(spans)
}
