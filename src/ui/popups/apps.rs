use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, AppsMode};
use crossterm::event::KeyModifiers;

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    let dim = Style::default().fg(app.theme.fg_dim);

    if app.apps_mode == AppsMode::EditFile {
        let name = app
            .apps_cache
            .get(app.apps_selected)
            .cloned()
            .unwrap_or_default();
        let title = chrome::input_title(
            app,
            format!("edit {name}/"),
            &app.apps_edit,
            "Enter open in $EDITOR · Esc cancel",
        );
        let inner = chrome::render_frame(f, area, title, &app.theme, true);
        let list = chrome::standard_list(Vec::<ListItem>::new());
        f.render_widget(list, inner);
        return;
    }

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
                let url = app
                    .app_url(name)
                    .unwrap_or_else(|| "server not running".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(name.clone(), Style::default().fg(app.theme.fg)),
                    Span::styled(format!("  {n} file{}", if n == 1 { "" } else { "s" }), dim),
                    Span::styled(format!("  {url}"), dim),
                ]))
            })
            .collect()
    };

    let title = match app.apps_mode {
        AppsMode::EditFile => unreachable!(),
        AppsMode::ConfirmDelete => {
            let name = app
                .apps_cache
                .get(app.apps_selected)
                .cloned()
                .unwrap_or_default();
            chrome::danger_title(
                app,
                format!("remove app \"{name}\" and all its files?"),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        AppsMode::Browse => {
            chrome::hinted_title(app, "apps", "Enter open · Ctrl+E edit file · Ctrl+D remove")
        }
    };

    let inner = chrome::render_frame(f, area, title, &app.theme, true);
    let list = chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.apps_cache.is_empty() {
        state.select(Some(app.apps_selected.min(app.apps_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.apps_mode {
        AppsMode::EditFile => match classify_edit_key(key) {
            Some(EditAction::Cancel) => {
                app.apps_mode = AppsMode::Browse;
            }
            Some(EditAction::Save) => app.confirm_app_edit(),
            Some(EditAction::Backspace) => {
                app.apps_edit.pop();
            }
            Some(EditAction::Push(c)) => app.apps_edit.push(c),
            None => {}
        },
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
            if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.start_app_edit();
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
