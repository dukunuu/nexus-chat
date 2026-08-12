//! The `/usage` popup: lifetime token, prompt-cache, and cost analytics by
//! backend and model, plus a scrollable log of recent requests. Rendered as
//! a dashboard: a hero summary with a cache bar, colored per-backend rows,
//! ranked model list, and a live-looking recent-request feed.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::Theme;
use crate::ui::{fmt_cost, humanize};

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::WIDE.1);
    let Some(data) = &app.usage_data else {
        return;
    };
    let theme = &app.theme;
    let dim = Style::default().fg(theme.fg_dim);
    let title = chrome::popup_title(app, "📊", format!("usage · {}", app.usage_range.label()));
    let hint = format!(
        "{}←→/t {} · ↑↓ recent · Ctrl+R refresh · Esc close",
        chrome::count_hint(data.totals.requests as usize, "request"),
        app.usage_range.title()
    );
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, chrome::Tone::Normal);
    if data.totals.requests == 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                app.usage_range.empty_message(),
                dim,
            ))),
            inner,
        );
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(4),                                // hero summary
        Constraint::Length(1),                                // backends header
        Constraint::Length(1 + data.by_backend.len() as u16), // backends rows
        Constraint::Length(1),                                // gap
        Constraint::Length(1),                                // models header
        Constraint::Length(1 + data.by_model.len() as u16),   // models rows
        Constraint::Length(1),                                // gap
        Constraint::Length(1),                                // recent header
        Constraint::Min(0),                                   // recent rows (scrolls)
    ])
    .split(inner);

    render_summary(f, app, rows[0]);
    let width = rows[0].width;

    // --- by backend ---
    f.render_widget(
        Paragraph::new(section_header(theme, "by backend", width)),
        rows[1],
    );
    // Header mirrors the row layout exactly — the "  " prefix matches the
    // "● " marker and the cached cell carries the row's "% " suffix width,
    // so every numeric column lines up under its label.
    let mut backend_lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("  ", dim),
        Span::styled(format!("{:<14}", "backend"), dim),
        Span::styled(format!("{:>6} {:>9} {:>8} ", "req", "prompt", "out"), dim),
        Span::styled(format!("{:<6}  ", "cached"), dim),
        Span::styled(format!("{:>9}", "cost"), dim),
    ])];
    for b in &data.by_backend {
        let cached = rate(b.cache_read_tokens, b.prompt_tokens);
        let name = chrome::truncate(&b.backend, 14);
        let mut row = vec![
            Span::styled("● ", Style::default().fg(backend_color(theme, &b.backend))),
            Span::styled(
                format!("{name:<14}"),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
        ];
        row.extend(numeric_cells(
            theme,
            b.requests,
            b.prompt_tokens,
            b.completion_tokens,
            cached,
            b.cost,
        ));
        backend_lines.push(Line::from(row));
    }
    f.render_widget(Paragraph::new(backend_lines), rows[2]);

    // --- most used models ---
    f.render_widget(
        Paragraph::new(section_header(theme, "most used models", width)),
        rows[4],
    );
    let mut model_lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("   ", dim),
        Span::styled(format!("{:<30}", "model"), dim),
        Span::styled(format!("{:>6} {:>9} {:>8} ", "req", "prompt", "out"), dim),
        Span::styled(format!("{:<6}  ", "cached"), dim),
        Span::styled(format!("{:>9}", "cost"), dim),
    ])];
    for (i, m) in data.by_model.iter().enumerate() {
        let cached = rate(m.cache_read_tokens, m.prompt_tokens);
        let rank_style = if i == 0 {
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        let name = chrome::truncate(&m.model, 30);
        let mut row = vec![
            Span::styled(format!("{:>2} ", i + 1), rank_style),
            Span::styled(format!("{name:<30}"), Style::default().fg(theme.fg)),
        ];
        row.extend(numeric_cells(
            theme,
            m.requests,
            m.prompt_tokens,
            m.completion_tokens,
            cached,
            m.cost,
        ));
        model_lines.push(Line::from(row));
    }
    f.render_widget(Paragraph::new(model_lines), rows[5]);

    // --- recent requests (scrollable) ---
    f.render_widget(
        Paragraph::new(section_header(theme, "recent requests", width)),
        rows[7],
    );
    let recent_area = rows[8];
    let visible = recent_area.height as usize;
    let top = app
        .usage_scroll
        .min(data.recent.len().saturating_sub(visible));
    let mut recent_lines: Vec<Line> = Vec::with_capacity(visible);
    for r in data.recent.iter().skip(top).take(visible) {
        let cached = rate(r.cache_read_tokens, r.prompt_tokens);
        let time = r
            .created_at
            .parse::<chrono::DateTime<chrono::Utc>>()
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_else(|_| "--:--".to_string());
        let model = chrome::truncate(&r.model, 24);
        let tokens = format!(
            "{}→{}",
            humanize(r.prompt_tokens),
            humanize(r.completion_tokens)
        );
        recent_lines.push(Line::from(vec![
            Span::styled(format!("{time} "), dim),
            Span::styled("● ", Style::default().fg(backend_color(theme, &r.backend))),
            Span::styled(format!("{model:<24}"), Style::default().fg(theme.fg)),
            // 13 cells fits the widest possible pair ("999.9m→999.9m"); the
            // old 9-cell field overflowed on realistic values like
            // "122.2k→672", shoving the bar/percent/cost columns right by
            // the overflow amount — which varied row to row.
            Span::styled(format!("{tokens:>13} "), dim),
            Span::styled(
                "█".repeat((cached * 8.0).round() as usize),
                Style::default().fg(cache_color(theme, cached)),
            ),
            Span::styled(
                "░".repeat(8 - (cached * 8.0).round() as usize),
                Style::default().fg(theme.border_dim),
            ),
            Span::styled(
                format!(" {:>4}%", (cached * 100.0).round()),
                Style::default()
                    .fg(cache_color(theme, cached))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {:>9}", fmt_cost(r.cost)),
                cost_style(theme, r.cost.unwrap_or(0.0)),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(recent_lines), recent_area);
}

/// Hero summary: colored headline numbers plus a cache bar over all prompt
/// tokens ever logged.
fn render_summary(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let Some(data) = &app.usage_data else { return };
    let t = &data.totals;
    let dim = Style::default().fg(theme.fg_dim);
    let accent_bold = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let headline = Line::from(vec![
        Span::styled(format!("{} requests", t.requests), accent_bold),
        Span::styled(
            format!(
                " · {} prompt · {} output · {} cache writes",
                humanize(t.prompt_tokens),
                humanize(t.completion_tokens),
                humanize(t.cache_creation_tokens),
            ),
            dim,
        ),
        Span::styled(
            format!(" · {}", fmt_cost_agg(t.cost)),
            cost_style(theme, t.cost),
        ),
    ]);
    let cached = rate(t.cache_read_tokens, t.prompt_tokens);
    let bar_line = if t.prompt_tokens > 0 {
        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        spans.extend(cache_bar(cached, 24, theme));
        spans.push(Span::styled(
            format!(" {:.0}% of prompt served from cache", cached * 100.0),
            Style::default()
                .fg(cache_color(theme, cached))
                .add_modifier(Modifier::BOLD),
        ));
        Line::from(spans)
    } else {
        Line::from(Span::styled("  no cache data reported", dim))
    };
    f.render_widget(
        Paragraph::new(vec![headline, Line::from(""), bar_line]),
        area,
    );
}

/// The shared `req prompt out cached% cost` cells for a backend/model row.
/// The header counterpart is built inline with the identical widths (a "  "
/// name-column prefix and a `{:<6}  ` cached cell carrying the row's "% "
/// suffix), so label and numbers share column edges.
fn numeric_cells(
    theme: &Theme,
    requests: u64,
    prompt: u64,
    completion: u64,
    cached: f64,
    cost: f64,
) -> Vec<Span<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    vec![
        Span::styled(
            format!(
                "{:>6} {:>9} {:>8} ",
                requests,
                humanize(prompt),
                humanize(completion)
            ),
            dim,
        ),
        Span::styled(
            format!("{:>6}% ", (cached * 100.0).round()),
            Style::default().fg(cache_color(theme, cached)),
        ),
        Span::styled(
            format!("{:>9}", fmt_cost_agg(cost)),
            cost_style(theme, cost),
        ),
    ]
}

/// `▍title ───────────` — accent section marker with a dim rule.
fn section_header(theme: &Theme, title: &str, width: u16) -> Line<'static> {
    let rule = "─".repeat(width.saturating_sub(3 + title.len() as u16) as usize);
    Line::from(vec![
        Span::styled("▍", Style::default().fg(theme.accent)),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {rule}"), Style::default().fg(theme.border_dim)),
    ])
}

/// Cache hit fraction 0..=1 from raw token counts (0 when nothing to divide).
fn rate(cache_read: u64, prompt: u64) -> f64 {
    if prompt == 0 {
        0.0
    } else {
        (cache_read as f64 / prompt as f64).clamp(0.0, 1.0)
    }
}

/// Threshold color for a cache rate: green ≥70%, yellow ≥40%, red below.
fn cache_color(theme: &Theme, rate: f64) -> Color {
    if rate >= 0.7 {
        theme.success
    } else if rate >= 0.4 {
        theme.warning
    } else {
        theme.error
    }
}

/// One accent hue per backend, so the feed reads at a glance.
fn backend_color(theme: &Theme, backend: &str) -> Color {
    match backend {
        "OpenRouter" => theme.accent,
        "OpenAI" => theme.success,
        "Codex" => theme.accent2,
        _ => theme.warning,
    }
}

/// `██████░░` — filled cells in the cache color, empty in the dim border.
fn cache_bar(rate: f64, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    let filled = (rate * width as f64).round() as usize;
    vec![
        Span::styled(
            "█".repeat(filled),
            Style::default().fg(cache_color(theme, rate)),
        ),
        Span::styled(
            "░".repeat(width - filled),
            Style::default().fg(theme.border_dim),
        ),
    ]
}

fn cost_style(theme: &Theme, cost: f64) -> Style {
    if cost > 0.0 {
        Style::default()
            .fg(theme.success)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg_dim)
    }
}

/// Aggregate cost: `—` when nothing was billable (no known prices, or all
/// free-tier usage) — a `$0.00` would falsely imply a priced bill.
fn fmt_cost_agg(cost: f64) -> String {
    if cost > 0.0 {
        fmt_cost(Some(cost))
    } else {
        "—".to_string()
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    use crate::app::Popup;
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => app.scroll_usage(-1),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_usage(1),
        KeyCode::PageUp => app.scroll_usage(-10),
        KeyCode::PageDown => app.scroll_usage(10),
        // Time window: ←/h step back, →/l and t step forward.
        KeyCode::Left | KeyCode::Char('h') => app.cycle_usage_range(-1),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('t') => app.cycle_usage_range(1),
        KeyCode::Char('r')
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL) =>
        {
            app.refresh_usage();
        }
        _ => {}
    }
}
