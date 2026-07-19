use anyhow::Result;
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
/// isolated into their own view plus a steer input line.
pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 76, 70);

    let dim = Style::default().fg(app.theme.fg_dim);
    let rows: Vec<&crate::db::Message> = app
        .messages
        .iter()
        .filter(|m| m.role == "research_stage")
        .collect();
    let items: Vec<ListItem> = if rows.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "waiting for the first update…",
            dim,
        )))]
    } else {
        // Newest agents first so the currently active synthesis/verifier/
        // writer remains visible even after a full six-searcher fan-out.
        rows.iter()
            .rev()
            .map(|m| {
                let (label, detail) = m.content.split_once(':').unwrap_or((&m.content, ""));
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
                if !detail.is_empty() {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(detail.to_string(), Style::default().fg(app.theme.fg_dim)),
                    ]));
                }
                ListItem::new(Text::from(lines))
            })
            .collect()
    };

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

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
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
    Ok(())
}
