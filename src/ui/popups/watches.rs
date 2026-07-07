use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};

use crate::app::App;

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = if app.watches_cache.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "no watches yet — /watch <topic> to start one",
            dim,
        )))]
    } else {
        app.watches_cache
            .iter()
            .map(|w| {
                let last_run = match &w.last_run_at {
                    Some(t) => crate::ui::fmt_created(t),
                    None => "never".to_string(),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(w.topic.clone(), Style::default().fg(app.theme.fg)),
                    Span::styled(format!("  every {}h", w.interval_hours), dim),
                    Span::styled(format!("  last run: {last_run}"), dim),
                ]))
            })
            .collect()
    };

    let title = crate::ui::hint_title(
        app,
        " watches ",
        "watches — Enter jump to session · d delete",
    );

    let list = List::new(items)
        .block(chrome::popup_block(Line::from(title), &app.theme))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.watches_cache.is_empty() {
        state.select(Some(app.watch_selected.min(app.watches_cache.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => app.popup = crate::app::Popup::None,
        KeyCode::Up => app.move_watch_selection(-1),
        KeyCode::Down => app.move_watch_selection(1),
        KeyCode::Enter => app.confirm_watch_session()?,
        KeyCode::Char('d') => app.delete_selected_watch(),
        _ => {}
    }
    Ok(())
}
