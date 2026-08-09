use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, WatchMode};

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), chrome::STANDARD.0, chrome::STANDARD.1);
    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = if app.watches_cache.is_empty() {
        vec![chrome::empty_placeholder(
            "no watches yet — /watch <topic> to start one",
            &app.theme,
        )]
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

    let title = match app.watch_mode {
        WatchMode::ConfirmDelete => {
            let topic = app
                .watches_cache
                .get(app.watch_selected)
                .map(|w| w.topic.clone())
                .unwrap_or_default();
            chrome::danger_title(app, format!("delete watch \"{topic}\"?"), "")
        }
        WatchMode::Browse => chrome::popup_title(app, "⏰", "watches"),
    };
    let hint = match app.watch_mode {
        WatchMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        WatchMode::Browse if app.watches_cache.is_empty() => {
            "no watches yet — /watch <topic> to start one".to_string()
        }
        WatchMode::Browse => format!(
            "{}↑↓ · Enter jump to session · Ctrl+D delete",
            chrome::count_hint(app.watches_cache.len(), "watch")
        ),
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.watches_cache.is_empty() {
        state.select(Some(app.watch_selected.min(app.watches_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    match app.watch_mode {
        WatchMode::ConfirmDelete => match super::classify_confirm_delete_key(key) {
            Some(super::ConfirmDeleteAction::Yes) => {
                app.delete_selected_watch();
                app.watch_mode = WatchMode::Browse;
            }
            Some(super::ConfirmDeleteAction::No) => app.watch_mode = WatchMode::Browse,
            None => {}
        },
        WatchMode::Browse => match key.code {
            KeyCode::Esc => app.popup = crate::app::Popup::None,
            KeyCode::Up => app.move_watch_selection(-1),
            KeyCode::Down => app.move_watch_selection(1),
            KeyCode::Enter => app.confirm_watch_session()?,
            KeyCode::Char('d')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                    && !app.watches_cache.is_empty() =>
            {
                app.watch_mode = WatchMode::ConfirmDelete;
            }
            _ => {}
        },
    }
    Ok(())
}
