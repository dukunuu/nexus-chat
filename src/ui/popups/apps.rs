use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};

use crate::app::{App, AppsMode};

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = if app.apps_cache.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "no apps yet — ask the model to build one",
            dim,
        )))]
    } else {
        app.apps_cache
            .iter()
            .map(|name| {
                let n = app.app_file_count(name);
                let url = app.app_url(name).unwrap_or_else(|| "server not running".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(name.clone(), Style::default().fg(app.theme.fg)),
                    Span::styled(format!("  {n} file{}", if n == 1 { "" } else { "s" }), dim),
                    Span::styled(format!("  {url}"), dim),
                ]))
            })
            .collect()
    };

    let title = match app.apps_mode {
        AppsMode::ConfirmDelete => {
            let name = app.apps_cache.get(app.apps_selected).cloned().unwrap_or_default();
            format!(" remove app \"{name}\" and all its files? (Ctrl+D confirm · Esc cancel) ")
        }
        AppsMode::Browse => crate::ui::hint_title(
            app,
            " apps ",
            "apps — Enter open in browser · Ctrl+D remove · /edit <app>/<file> to edit",
        ),
    };

    let list = List::new(items)
        .block(chrome::popup_block(title, &app.theme))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.apps_cache.is_empty() {
        state.select(Some(app.apps_selected.min(app.apps_cache.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{BrowseAction, ConfirmDeleteAction, classify_browse_key, classify_confirm_delete_key};
    match app.apps_mode {
        AppsMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_app_delete()?,
            Some(ConfirmDeleteAction::No) => app.apps_mode = AppsMode::Browse,
            None => {}
        },
        AppsMode::Browse => {
            if key.code == KeyCode::Enter {
                app.open_selected_app();
                return Ok(());
            }
            match classify_browse_key(key, false, false) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_apps_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_apps_selection(1),
                Some(BrowseAction::ConfirmDelete) if !app.apps_cache.is_empty() => {
                    app.apps_mode = AppsMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
