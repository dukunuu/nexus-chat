use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::ListItem;

use crate::app::App;

use super::chrome;

/// Live per-searcher activity: the same `research_stage` rows shown inline in
/// the transcript (one per searcher label, updated in place as
/// Status/ToolCall events arrive — see `run_searcher` in `app/research.rs`),
/// isolated into their own view plus a steer input line and a queued-steers
/// section (steers not yet picked up at a round boundary).
pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 76, 70);

    let dim = Style::default().fg(app.theme.fg_dim);
    // The stage rows come from the job-level mirror (`research_stage_rows`),
    // kept in sync by `mirror_stage` on every `Stage` update regardless of
    // which session is viewed — no db read (and no silent fallback) inside
    // rendering, and opening the popup from another session still shows the
    // job's searchers and steers.
    let rows = &app.research_stage_rows;
    let mut items: Vec<ListItem> = Vec::new();
    // Quick win: steers queued but not yet drained at a round boundary stay
    // visible here. The pipeline acknowledges drained steers as `steer #N`
    // stage rows and the App records those positions in
    // `research_steer_acked` (parsed from `Stage` updates, so it's correct
    // even when the popup is opened from another session); steer k is
    // picked up iff k is in the ack set. The retained log drops acked
    // entries, so it holds exactly the still-queued steers.
    let pending: Vec<&(usize, String)> = app
        .research_steer_log
        .iter()
        .filter(|(pos, _)| !app.research_steer_acked.contains(pos))
        .collect();
    let has_pending = !pending.is_empty();
    if has_pending {
        items.push(ListItem::new(Line::from(Span::styled(
            "queued steers — picked up at the next round boundary:",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))));
        for (_, s) in pending {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("● ", Style::default().fg(app.theme.accent)),
                Span::styled(s.clone(), dim),
            ])));
        }
        items.push(ListItem::new(Line::from("")));
    }
    if rows.is_empty() && !has_pending {
        items.push(ListItem::new(Line::from(Span::styled(
            "waiting for the first update…",
            dim,
        ))));
    } else {
        // Newest agents first so the currently active synthesis/verifier/
        // writer remains visible even after a full six-searcher fan-out.
        for content in rows.iter().rev() {
            let (label, detail) = content.split_once(':').unwrap_or((content.as_str(), ""));
            let detail = detail.trim();
            let (glyph, color, detail) = if let Some(rest) = detail.strip_prefix("done —") {
                ("✓", app.theme.success, rest.trim())
            } else if let Some(rest) = detail.strip_prefix("error —") {
                ("×", app.theme.error, rest.trim())
            } else if let Some(rest) = detail.strip_prefix("working —") {
                ("●", app.theme.accent, rest.trim())
            } else {
                ("○", app.theme.fg_dim, detail)
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(
                    label.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ])];
            // Multi-line details (e.g. critic gaps listed as questions) get
            // one indented row per line.
            for detail_line in detail.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(detail_line.to_string(), dim),
                ]));
            }
            items.push(ListItem::new(Text::from(lines)));
        }
    }

    let inner = chrome::render_frame(
        f,
        area,
        chrome::input_title(
            app,
            "research agents · steer",
            &app.research_live_input,
            "type instruction · Enter send · Ctrl+↑ agents · Ctrl+X stop · Esc close",
        ),
        &app.theme,
        true,
    );
    let list = chrome::standard_list(items);
    f.render_widget(list, inner);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key
        .modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('x') if ctrl => app.stop_research(),
        KeyCode::Esc => app.popup = crate::app::Popup::None,
        KeyCode::Enter => {
            let text = app.research_live_input.trim().to_string();
            app.research_live_input.clear();
            app.steer_research(&text);
        }
        KeyCode::Backspace => {
            app.research_live_input.pop();
        }
        KeyCode::Char(c) => app.research_live_input.push(c),
        _ => {}
    }
}
