use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::FilesMode;
    let area = crate::ui::centered(f.area(), 64, 60);

    if app.files_mode == FilesMode::Pick {
        let entries = app.filtered_picker_entries();
        let items: Vec<ListItem> = entries
            .iter()
            .map(|e| {
                if e.is_dir {
                    ListItem::new(Line::from(Span::styled(
                        format!("{}/", e.name),
                        Style::default().fg(app.theme.accent),
                    )))
                } else {
                    ListItem::new(Line::from(Span::styled(
                        e.name.clone(),
                        Style::default().fg(app.theme.fg),
                    )))
                }
            })
            .collect();
        let inner = chrome::render_frame(
            f,
            area,
            chrome::input_title(
                app,
                app.picker_dir.display().to_string(),
                &app.picker_filter,
                "Enter open/import · Backspace up · Esc cancel",
            ),
            &app.theme,
            true,
        );
        let list = chrome::standard_list(items);
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(app.picker_selected.min(entries.len() - 1)));
        }
        f.render_stateful_widget(list, inner, &mut state);
        return;
    }

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = app
        .files_cache
        .iter()
        .map(|file| {
            let ok = file.status == "ok";
            let status_style = if ok {
                dim
            } else {
                Style::default().fg(app.theme.warning)
            };
            ListItem::new(Line::from(vec![
                Span::styled(file.name.clone(), Style::default().fg(app.theme.fg)),
                Span::styled(format!("  {}", crate::app::human_size(file.size)), dim),
                Span::styled(format!("  {}", file.status), status_style),
            ]))
        })
        .collect();

    let title = match app.files_mode {
        FilesMode::Add => chrome::input_title(
            app,
            "import path",
            &app.files_edit,
            "Enter import · Esc cancel",
        ),
        FilesMode::Rename => chrome::input_title(
            app,
            "rename to",
            &app.files_edit,
            "Enter rename · Esc cancel",
        ),
        FilesMode::ConfirmDelete => {
            let name = app
                .files_cache
                .get(app.files_selected)
                .map(|f| f.name.clone())
                .unwrap_or_default();
            chrome::danger_title(
                app,
                format!("remove \"{name}\"?"),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        FilesMode::Browse => chrome::hinted_title(
            app,
            "files",
            "Enter open · Ctrl+N add · Ctrl+R rename · Ctrl+O re-extract · Ctrl+D remove",
        ),
        // Pick short-circuits with an early return above; this arm only
        // keeps the match exhaustive (a panic here would kill the whole TUI).
        FilesMode::Pick => Line::from(""),
    };

    let inner = chrome::render_frame(f, area, title, &app.theme, true);
    let list = chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.files_cache.is_empty() {
        state.select(Some(app.files_selected.min(app.files_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    use crate::app::FilesMode;
    match app.files_mode {
        FilesMode::Add | FilesMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.files_mode = FilesMode::Browse,
            Some(EditAction::Save) if app.files_mode == FilesMode::Add => {
                app.confirm_files_add()?
            }
            Some(EditAction::Save) => app.confirm_files_rename()?,
            Some(EditAction::Backspace) => {
                app.files_edit.pop();
            }
            Some(EditAction::Push(c)) => app.files_edit.push(c),
            None => {}
        },
        FilesMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_files_delete()?,
            Some(ConfirmDeleteAction::No) => app.files_mode = FilesMode::Browse,
            None => {}
        },
        FilesMode::Browse => {
            if key.code == KeyCode::Enter {
                app.open_selected_file();
                return Ok(());
            }
            // Ctrl+O: re-extract with the current OCR engine (clears old text).
            if key.code == KeyCode::Char('o')
                && key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                app.reextract_selected_file();
                return Ok(());
            }
            // Create = Add (Ctrl+N); Rename = Ctrl+R; no browse text filter (small lists).
            match classify_browse_key(key, true, true) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_files_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_files_selection(1),
                Some(BrowseAction::Create) => app.open_file_picker(),
                Some(BrowseAction::Rename) => app.start_files_rename(),
                Some(BrowseAction::ConfirmDelete) if !app.files_cache.is_empty() => {
                    app.files_mode = FilesMode::ConfirmDelete;
                }
                _ => {}
            }
        }
        FilesMode::Pick => match key.code {
            KeyCode::Esc => app.files_mode = FilesMode::Browse,
            KeyCode::Enter => app.picker_enter()?,
            KeyCode::Backspace => app.picker_backspace(),
            KeyCode::Up => app.move_picker_selection(-1),
            KeyCode::Down => app.move_picker_selection(1),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                app.picker_filter_push(c)
            }
            _ => {}
        },
    }
    Ok(())
}
