use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::FilesMode;
    let area = crate::ui::centered(f.area(), 64, 60);
    f.render_widget(Clear, area);

    if app.files_mode == FilesMode::Pick {
        let entries = app.filtered_picker_entries();
        let items: Vec<ListItem> = entries
            .iter()
            .map(|e| {
                if e.is_dir {
                    ListItem::new(Line::from(Span::styled(
                        format!("{}/", e.name),
                        Style::default().fg(Color::Cyan),
                    )))
                } else {
                    ListItem::new(Line::from(Span::styled(e.name.clone(), Style::default().fg(Color::White))))
                }
            })
            .collect();
        let title = format!(
            " {} — filter: {}▏  (Enter open/import · Backspace up · Esc cancel) ",
            app.picker_dir.display(),
            app.picker_filter,
        );
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("▸ ");
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(app.picker_selected.min(entries.len() - 1)));
        }
        f.render_stateful_widget(list, area, &mut state);
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = app
        .files_cache
        .iter()
        .map(|file| {
            let ok = file.status == "ok";
            let status_style = if ok { dim } else { Style::default().fg(Color::Yellow) };
            ListItem::new(Line::from(vec![
                Span::styled(file.name.clone(), Style::default().fg(Color::White)),
                Span::styled(format!("  {}", crate::app::human_size(file.size)), dim),
                Span::styled(format!("  {}", file.status), status_style),
            ]))
        })
        .collect();

    let title = match app.files_mode {
        FilesMode::Add => format!(" import path: {}▏  (Enter import · Esc cancel) ", app.files_edit),
        FilesMode::ConfirmDelete => {
            let name = app.files_cache.get(app.files_selected).map(|f| f.name.clone()).unwrap_or_default();
            format!(" remove \"{name}\"? (Ctrl+D confirm · Esc cancel) ")
        }
        FilesMode::Browse => crate::ui::hint_title(
            app,
            " files ",
            "files — Enter open · Ctrl+N add · Ctrl+D remove (or drop files into the space dir)",
        ),
        // Pick short-circuits with an early return above; this arm is
        // unreachable and only keeps the match exhaustive.
        FilesMode::Pick => unreachable!("Pick returns earlier in render()"),
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.files_cache.is_empty() {
        state.select(Some(app.files_selected.min(app.files_cache.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key, classify_edit_key};
    use crate::app::FilesMode;
    match app.files_mode {
        FilesMode::Add => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.files_mode = FilesMode::Browse,
            Some(EditAction::Save) => app.confirm_files_add()?,
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
            // Create = Add (Ctrl+N); no rename; no browse text filter (small lists).
            match classify_browse_key(key, true, false) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_files_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_files_selection(1),
                Some(BrowseAction::Create) => app.open_file_picker(),
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
            KeyCode::Char(c) if !key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                app.picker_filter_push(c)
            }
            _ => {}
        },
    }
    Ok(())
}
