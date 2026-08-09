// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, FilesMode, FilesTab, ImagesMode, ScriptsMode};

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    match app.files_tab {
        FilesTab::Files => render_files(f, app),
        FilesTab::Images => render_images(f, app),
        FilesTab::Scripts => render_scripts(f, app),
    }
}

#[allow(clippy::too_many_lines)] // tabs + picker + browse rows
fn render_files(f: &mut Frame, app: &App) {
    use crate::app::FilesMode;
    let area = crate::ui::centered(f.area(), 64, 60);

    if app.files_mode == FilesMode::Pick {
        let entries = app.filtered_picker_entries();
        let items: Vec<ListItem> = entries
            .iter()
            .map(|e| {
                let max = (area.width.saturating_sub(5)) as usize;
                if e.is_dir {
                    ListItem::new(Line::from(Span::styled(
                        chrome::truncate(&format!("{}/", e.name), max),
                        Style::default().fg(app.theme.accent),
                    )))
                } else {
                    ListItem::new(Line::from(Span::styled(
                        chrome::truncate(&e.name, max),
                        Style::default().fg(app.theme.fg),
                    )))
                }
            })
            .collect();
        let inner = chrome::render_hinted(
            f,
            area,
            chrome::input_title(
                app,
                app.picker_dir.display().to_string(),
                &app.picker_filter,
                "",
            ),
            "Enter open/import · Backspace up · Esc cancel",
            app,
            true,
            chrome::Tone::Normal,
        );
        let list = chrome::standard_list(items, &app.theme);
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(app.picker_selected.min(entries.len() - 1)));
        }
        chrome::render_list(f, list, &mut state, inner, entries.len(), 1, &app.theme);
        return;
    }

    let dim = Style::default().fg(app.theme.fg_dim);
    let content_w = (area.width.saturating_sub(5)) as usize; // border + scrollbar + highlight
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
            // Status can carry long error text — cap the meta column so the
            // name keeps a fair share of the row.
            let meta = chrome::truncate(
                &format!(
                    "  {}  {}",
                    crate::app::human_size(file.size.unsigned_abs()),
                    file.status
                ),
                36,
            );
            let name = chrome::truncate(
                &file.name,
                content_w.saturating_sub(meta.chars().count() + 1),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, status_style),
            ]))
        })
        .collect();

    let title = match app.files_mode {
        FilesMode::Add => chrome::input_title(app, "import path", &app.files_edit, ""),
        FilesMode::Rename => chrome::input_title(app, "rename to", &app.files_edit, ""),
        FilesMode::ConfirmDelete => {
            let name = app
                .files_cache
                .get(app.files_selected)
                .map(|f| f.name.clone())
                .unwrap_or_default();
            chrome::danger_title(app, format!("remove \"{name}\"?"), "")
        }
        FilesMode::Browse => chrome::popup_title(app, "📁", "files"),
        FilesMode::Pick => Line::from(""),
    };
    let hint = match app.files_mode {
        FilesMode::Add => "Enter import · Esc cancel".to_string(),
        FilesMode::Rename => "Enter rename · Esc cancel".to_string(),
        FilesMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        FilesMode::Browse if app.files_cache.is_empty() => "Ctrl+N add · Tab tab".to_string(),
        FilesMode::Browse => format!(
            "{}↑↓ · Enter open · Ctrl+N add · Ctrl+R rename · Ctrl+D remove · Tab tab",
            chrome::count_hint(app.files_cache.len(), "file")
        ),
        FilesMode::Pick => String::new(),
    };
    let tone = if app.files_mode == FilesMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.files_cache.is_empty() {
        state.select(Some(app.files_selected.min(app.files_cache.len() - 1)));
    }
    chrome::render_list(
        f,
        list,
        &mut state,
        inner,
        app.files_cache.len(),
        1,
        &app.theme,
    );
}

fn render_images(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    let dim = Style::default().fg(app.theme.fg_dim);

    let items: Vec<ListItem> = if app.images_cache.is_empty() {
        vec![chrome::empty_placeholder(
            "no images yet — paste one, or have the model generate one",
            &app.theme,
        )]
    } else {
        app.images_cache
            .iter()
            .map(|img| {
                let created = crate::ui::fmt_created(&img.modified);
                let meta = format!("  {}  {created}", crate::app::human_size(img.size));
                let name = chrome::truncate(
                    &img.name,
                    (area.width.saturating_sub(5) as usize)
                        .saturating_sub(meta.chars().count() + 1),
                );
                ListItem::new(Line::from(vec![
                    Span::styled(name, Style::default().fg(app.theme.fg)),
                    Span::styled(meta, dim),
                ]))
            })
            .collect()
    };

    let title = match app.images_mode {
        ImagesMode::ConfirmDelete => {
            let name = app
                .images_cache
                .get(app.images_selected)
                .map(|i| i.name.clone())
                .unwrap_or_default();
            chrome::danger_title(app, format!("remove \"{name}\"?"), "")
        }
        ImagesMode::Browse => chrome::popup_title(app, "🖼", "images"),
    };
    let hint = match app.images_mode {
        ImagesMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        ImagesMode::Browse if app.images_cache.is_empty() => "Tab switch tab".to_string(),
        ImagesMode::Browse => format!(
            "{}↑↓ · Enter open · Ctrl+D remove · Tab tab",
            chrome::count_hint(app.images_cache.len(), "image")
        ),
    };
    let tone = if app.images_mode == ImagesMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.images_cache.is_empty() {
        state.select(Some(app.images_selected.min(app.images_cache.len() - 1)));
    }
    chrome::render_list(
        f,
        list,
        &mut state,
        inner,
        app.images_cache.len(),
        1,
        &app.theme,
    );
}

fn render_scripts(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    let dim = Style::default().fg(app.theme.fg_dim);

    if app.scripts_mode == ScriptsMode::Create {
        let title = chrome::input_title(app, "script name", &app.scripts_edit, "");
        let inner = chrome::render_hinted(
            f,
            area,
            title,
            "Enter create+edit · Esc cancel",
            app,
            true,
            chrome::Tone::Normal,
        );
        let list = chrome::standard_list(Vec::<ListItem>::new(), &app.theme);
        f.render_widget(list, inner);
        return;
    }

    if app.scripts_mode == ScriptsMode::Rename {
        let title = chrome::input_title(app, "rename to", &app.scripts_edit, "");
        let inner = chrome::render_hinted(
            f,
            area,
            title,
            "Enter rename · Esc cancel",
            app,
            true,
            chrome::Tone::Normal,
        );
        let list = chrome::standard_list(Vec::<ListItem>::new(), &app.theme);
        f.render_widget(list, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .scripts_cache
        .iter()
        .map(|s| {
            let created = crate::ui::fmt_created(&s.modified);
            let meta = format!("  {}  {created}", crate::app::human_size(s.size));
            let name = chrome::truncate(
                &s.name,
                (area.width.saturating_sub(5) as usize).saturating_sub(meta.chars().count() + 1),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, dim),
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
            chrome::danger_title(app, format!("remove \"{name}\"?"), "")
        }
        ScriptsMode::Browse => chrome::popup_title(app, "📜", "scripts"),
        _ => unreachable!(),
    };
    let hint = match app.scripts_mode {
        ScriptsMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        ScriptsMode::Browse if app.scripts_cache.is_empty() => {
            "Ctrl+N create · Tab tab".to_string()
        }
        _ => format!(
            "{}↑↓ · Enter edit · Ctrl+N create · Ctrl+R rename · Ctrl+D remove · Tab tab",
            chrome::count_hint(app.scripts_cache.len(), "script")
        ),
    };
    let tone = if app.scripts_mode == ScriptsMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.scripts_cache.is_empty() {
        state.select(Some(app.scripts_selected.min(app.scripts_cache.len() - 1)));
    }
    chrome::render_list(
        f,
        list,
        &mut state,
        inner,
        app.scripts_cache.len(),
        1,
        &app.theme,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // Tab switches tab
    if key.code == KeyCode::Tab {
        app.files_tab = match app.files_tab {
            FilesTab::Files => FilesTab::Images,
            FilesTab::Images => FilesTab::Scripts,
            FilesTab::Scripts => FilesTab::Files,
        };
        // Refresh cache for the new tab
        match app.files_tab {
            FilesTab::Files => app.rescan_files(),
            FilesTab::Images => app.refresh_images(),
            FilesTab::Scripts => app.refresh_scripts(),
        }
        app.files_mode = FilesMode::Browse;
        return Ok(());
    }

    match app.files_tab {
        FilesTab::Files => handle_files_key(app, key),
        FilesTab::Images => handle_images_key(app, key),
        FilesTab::Scripts => handle_scripts_key(app, key),
    }
}

fn handle_files_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.files_mode {
        FilesMode::Add | FilesMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.files_mode = FilesMode::Browse,
            Some(EditAction::Save) if app.files_mode == FilesMode::Add => {
                app.confirm_files_add();
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
            if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.reextract_selected_file();
                return Ok(());
            }
            if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.reocr_selected_file();
                return Ok(());
            }
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
            KeyCode::Enter => app.picker_enter(),
            KeyCode::Backspace => app.picker_backspace(),
            KeyCode::Up => app.move_picker_selection(-1),
            KeyCode::Down => app.move_picker_selection(1),
            KeyCode::PageUp => app.move_picker_selection(-10),
            KeyCode::PageDown => app.move_picker_selection(10),
            KeyCode::Home => app.move_picker_selection(i32::MIN / 2),
            KeyCode::End => app.move_picker_selection(i32::MAX / 2),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.picker_filter_push(c);
            }
            _ => {}
        },
    }
    Ok(())
}

fn handle_images_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, classify_browse_key, classify_confirm_delete_key,
    };
    match app.images_mode {
        ImagesMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_images_delete()?,
            Some(ConfirmDeleteAction::No) => app.images_mode = ImagesMode::Browse,
            None => {}
        },
        ImagesMode::Browse => {
            if key.code == KeyCode::Enter && !app.images_cache.is_empty() {
                app.open_selected_image();
                return Ok(());
            }
            match classify_browse_key(key, false, false) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_images_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_images_selection(1),
                Some(BrowseAction::ConfirmDelete) if !app.images_cache.is_empty() => {
                    app.images_mode = ImagesMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_scripts_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.scripts_mode {
        ScriptsMode::Create | ScriptsMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => {
                if app.scripts_mode == ScriptsMode::Rename {
                    app.scripts_edit.clear();
                }
                app.scripts_mode = ScriptsMode::Browse;
            }
            Some(EditAction::Save) if app.scripts_mode == ScriptsMode::Create => {
                app.confirm_script_create()?;
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
