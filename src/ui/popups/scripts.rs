use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, ScriptsMode};

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    let dim = Style::default().fg(app.theme.fg_dim);

    // Create mode: text input
    if app.scripts_mode == ScriptsMode::Create {
        let title = chrome::input_title(
            app,
            "script name",
            &app.scripts_edit,
            "Enter create+edit · Esc cancel",
        );
        let inner = chrome::render_frame(f, area, title, &app.theme, true);
        let list = chrome::standard_list(Vec::<ListItem>::new());
        f.render_widget(list, inner);
        return;
    }

    // Rename mode: text input
    if app.scripts_mode == ScriptsMode::Rename {
        let title = chrome::input_title(
            app,
            "rename to",
            &app.scripts_edit,
            "Enter rename · Esc cancel",
        );
        let inner = chrome::render_frame(f, area, title, &app.theme, true);
        let list = chrome::standard_list(Vec::<ListItem>::new());
        f.render_widget(list, inner);
        return;
    }

    // Browse + ConfirmDelete: show the list
    let items: Vec<ListItem> = app
        .scripts_cache
        .iter()
        .map(|s| {
            let created = crate::ui::fmt_created(&s.modified);
            ListItem::new(Line::from(vec![
                Span::styled(s.name.clone(), Style::default().fg(app.theme.fg)),
                Span::styled(format!("  {}", crate::app::human_size(s.size)), dim),
                Span::styled(format!("  {created}"), dim),
            ]))
        })
        .collect();

    let title = match app.scripts_mode {
        ScriptsMode::ConfirmDelete => {
            let name = app
                .scripts_cache
                .get(app.scripts_selected)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            chrome::danger_title(
                app,
                format!("remove \"{name}\"?"),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        ScriptsMode::Browse => chrome::hinted_title(
            app,
            "scripts",
            "Enter edit · Ctrl+N create · Ctrl+R rename · Ctrl+D remove",
        ),
        _ => unreachable!(),
    };

    let inner = chrome::render_frame(f, area, title, &app.theme, true);
    let list = chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.scripts_cache.is_empty() {
        state.select(Some(app.scripts_selected.min(app.scripts_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.scripts_mode {
        ScriptsMode::Create | ScriptsMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.scripts_mode = ScriptsMode::Browse,
            Some(EditAction::Save) if app.scripts_mode == ScriptsMode::Create => {
                app.confirm_script_create()?
            }
            Some(EditAction::Save) => app.confirm_script_rename()?,
            Some(EditAction::Backspace) => {
                app.scripts_edit.pop();
            }
            Some(EditAction::Push(c)) => app.scripts_edit.push(c),
            None => {}
        },
        ScriptsMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_script_delete()?,
            Some(ConfirmDeleteAction::No) => app.scripts_mode = ScriptsMode::Browse,
            None => {}
        },
        ScriptsMode::Browse => {
            if key.code == KeyCode::Enter && !app.scripts_cache.is_empty() {
                app.open_selected_script();
                return Ok(());
            }
            match classify_browse_key(key, true, true) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_scripts_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_scripts_selection(1),
                Some(BrowseAction::Create) => app.start_script_create(),
                Some(BrowseAction::Rename) => app.start_script_rename(),
                Some(BrowseAction::ConfirmDelete) if !app.scripts_cache.is_empty() => {
                    app.scripts_mode = ScriptsMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
