use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem};

use crate::app::App;

use super::chrome;

/// Live per-searcher activity: the same `research_stage` rows shown inline in
/// the transcript (one per searcher label, updated in place as
/// Status/ToolCall events arrive — see `run_searcher` in `app/research.rs`),
/// isolated into their own view plus a steer input line.
pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 76, 70);
    f.render_widget(Clear, area);

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
        rows.iter()
            .map(|m| {
                ListItem::new(Line::from(Span::styled(
                    m.content.clone(),
                    Style::default().fg(app.theme.fg),
                )))
            })
            .collect()
    };

    let title = format!(
        " research — steer: {}▏  (Enter send · Esc close) ",
        app.research_live_input
    );
    let list = List::new(items).block(chrome::popup_block(
        Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        &app.theme,
    ));
    f.render_widget(list, area);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
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
