//! The status bar (model, status line, context meter) and the completed-
//! task toasts that float above the composer.

// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data.
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
use ratatui::widgets::{Clear, Paragraph};

use super::{humanize, popups};
use crate::app_view::AppView;

/// Green → yellow → red gradient for `t` in 0.0..=1.0.
fn gradient(t: f64) -> Color {
    // Two linear segments: green→yellow (0..0.5), yellow→red (0.5..1).
    let (r, g) = if t < 0.5 {
        let k = t / 0.5;
        ((40.0 + k * 190.0) as u8, 200u8) // 40→230 red, green steady
    } else {
        let k = (t - 0.5) / 0.5;
        (230u8, (200.0 - k * 190.0) as u8) // red steady, green 200→10
    };
    Color::Rgb(r, g, 40)
}

pub(super) fn render_status(f: &mut Frame, app: &AppView, area: Rect) {
    use nexus_core::db::DEFAULT_SPACE;
    let theme = &app.theme;
    let dim = Style::default().fg(theme.fg_dim);
    let sep = || Span::styled(" · ", Style::default().fg(theme.border_dim));

    // Left: the model (accent), then the quiet mode tags.
    let model = app
        .current_model
        .as_deref()
        .map_or_else(|| "no model".to_string(), short_model_label);
    let mut tags: Vec<String> = Vec::new();
    if app.active_space.name != DEFAULT_SPACE {
        tags.push(format!("⌂ {}", app.active_space.name));
    }
    if app.web_mode {
        tags.push("🌐 web".into());
    }
    if app.incognito {
        tags.push("🕶 incognito".into());
    }
    let badge_max = (area.width as usize * 2 / 5).max(12);
    let tag_w: usize = tags.iter().map(|t| t.chars().count() + 3).sum();
    let model = fit_badge("", &model, badge_max.saturating_sub(tag_w + 2).max(8));
    let mut left = vec![
        Span::styled(" ◆ ", Style::default().fg(theme.accent)),
        Span::styled(
            model,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    for tag in tags {
        left.push(sep());
        left.push(Span::styled(tag, Style::default().fg(theme.accent2)));
    }

    // Right: the context meter and numbers, when the window is known.
    let mut right: Vec<Span> = Vec::new();
    if app.settings.show_stats
        && let Some(limit) = app.context_limit()
    {
        right.extend(context_meter(app, limit, 10));
        right.push(Span::raw(" "));
        right.push(Span::styled(context_numbers(app, limit), dim));
    } else if let Some(rate) = app.turn_cache.rate() {
        let partial = if app.turn_cache.is_partial() { "~" } else { "" };
        right.push(Span::styled(
            format!("{partial}{:.0}% cached", rate * 100.0),
            dim,
        ));
    }
    if !right.is_empty() {
        right.insert(0, Span::raw("  "));
        right.push(Span::raw(" "));
    }

    let width_of = |spans: &[Span]| -> u16 {
        u16::try_from(spans.iter().map(Span::width).sum::<usize>()).unwrap_or(u16::MAX)
    };
    let (left_w, right_w) = (width_of(&left), width_of(&right));
    // On a narrow bar the meter yields first, then the status text.
    let right_w = if left_w + right_w + 8 > area.width {
        0
    } else {
        right_w
    };
    let cols = Layout::horizontal([
        Constraint::Length(left_w),
        Constraint::Min(0),
        Constraint::Length(right_w),
    ])
    .split(area);
    f.render_widget(Paragraph::new(Line::from(left)), cols[0]);
    if !app.status.is_empty() {
        let text = popups::chrome::truncate(&app.status, cols[1].width.saturating_sub(3) as usize);
        f.render_widget(
            Paragraph::new(Line::from(vec![sep(), Span::styled(text, dim)])),
            cols[1],
        );
    }
    if right_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(right)).alignment(ratatui::layout::Alignment::Right),
            cols[2],
        );
    }
}

/// `▰▰▰▱▱▱▱▱` — how full the context window is, each filled cell colored by
/// its position along a green → yellow → red ramp.
fn context_meter(app: &AppView, limit: u64, cells: usize) -> Vec<Span<'static>> {
    let ratio = if limit > 0 {
        (app.context_used() as f64 / limit as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (ratio * cells as f64).ceil() as usize;
    (0..cells)
        .map(|x| {
            if x < filled {
                let t = x as f64 / (cells - 1).max(1) as f64;
                Span::styled("▰", Style::default().fg(gradient(t)))
            } else {
                Span::styled("▱", Style::default().fg(app.theme.border_dim))
            }
        })
        .collect()
}

/// `34% 44k/128k · 82% cached`. The cache rate covers the whole turn, not
/// its last request: a tool loop's final request sits on the longest cached
/// prefix, so reporting it alone would flatter the number. `~` marks a turn
/// some provider did not fully account for.
fn context_numbers(app: &AppView, limit: u64) -> String {
    let used = app.context_used();
    let pct = if limit > 0 {
        used as f64 / limit as f64 * 100.0
    } else {
        0.0
    };
    let mut label = format!("{pct:.0}% {}/{}", humanize(used), humanize(limit));
    if let Some(rate) = app.turn_cache.rate() {
        let partial = if app.turn_cache.is_partial() { "~" } else { "" };
        let _ = std::fmt::Write::write_fmt(
            &mut label,
            format_args!(" · {partial}{:.0}% cached", rate * 100.0),
        );
    }
    label
}

/// `prefix` + `model` within `max` chars. A too-long label shortens the model
/// name but keeps its ` · backend` tag, which is the part that disambiguates.
pub(super) fn fit_badge(prefix: &str, model: &str, max: usize) -> String {
    let full = format!("{prefix}{model}");
    if full.chars().count() <= max {
        return full;
    }
    let (name, tag) = model
        .rsplit_once(" · ")
        .map_or((model, String::new()), |(n, t)| (n, format!(" · {t}")));
    let room = max.saturating_sub(prefix.chars().count() + tag.chars().count());
    if room < 4 {
        return popups::chrome::truncate(&full, max);
    }
    format!("{prefix}{}{tag}", popups::chrome::truncate(name, room))
}

/// The status-bar name for a model id: the last path segment, with the
/// backend named when it isn't `OpenRouter` — `local:org/Model-7B` reads as
/// `Model-7B · local`. `OpenRouter` ids may contain `:` (`…:free`), so only a
/// known backend prefix is split off.
pub(crate) fn short_model_label(id: &str) -> String {
    let (backend, rest) = match id.split_once(':') {
        Some((tag, rest)) if matches!(tag, "openai" | "opencode" | "go" | "codex" | "local") => {
            (Some(tag), rest)
        }
        _ => (None, id),
    };
    // OpenCode's flat-fee bundle nests its own `go:` tag inside the id.
    let (backend, rest) = match rest.strip_prefix("go:") {
        Some(inner) if backend == Some("opencode") => (Some("go"), inner),
        _ => (backend, rest),
    };
    let name = rest.rsplit('/').next().unwrap_or(rest);
    match backend {
        Some(tag) => format!("{name} · {tag}"),
        None => name.to_string(),
    }
}

/// Persistent, direct-click targets for completed chat tasks. The queue keeps
/// every completion; only the newest five are painted to avoid covering a
/// small terminal.
pub(super) fn render_notifications(f: &mut Frame, app: &mut AppView, area: Rect) {
    app.notification_areas.clear();
    let rows = app.notifications.len().min(5) as u16;
    if rows == 0 || area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width.min(64);
    let start = app.notifications.len().saturating_sub(rows as usize);
    let y = area.y + area.height.saturating_sub(rows);
    for (offset, index) in (start..app.notifications.len()).enumerate() {
        let notification = &app.notifications[index];
        let (glyph, color) = if notification.success {
            ("✓", app.theme.success)
        } else {
            ("×", app.theme.error)
        };
        // A toast: colored rail and glyph, the session in bold, the outcome
        // dimmed — clicking it opens that session.
        let bg = Style::default().bg(app.theme.raised);
        let line = popups::chrome::fit_line(
            Line::from(vec![
                Span::styled("▎", Style::default().fg(color)),
                Span::styled(
                    format!("{glyph} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    notification.title.clone(),
                    Style::default()
                        .fg(app.theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {} ", notification.text),
                    Style::default().fg(app.theme.fg_dim),
                ),
            ]),
            width as usize,
        );
        // The toast hugs its content at the pane's right edge, clear of the
        // scrollbar gutter; the rect is also its click target.
        let w = u16::try_from(line.width()).unwrap_or(width).min(width);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w + 1),
            y: y + offset as u16,
            width: w,
            height: 1,
        };
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(line).style(app.theme.background_style().patch(bg)),
            rect,
        );
        app.notification_areas.push((rect, index));
    }
}
