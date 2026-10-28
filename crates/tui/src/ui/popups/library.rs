//! `/library`: every deck and doc of the space, newest first. Enter opens one in
//! studio (linking the window), `e` exports it through the operating system's
//! Save dialog, `d` moves it to the space's `.trash` folder after a confirm.
//!
//! The listing, the file operations and the dialogs are `nexus_core::canvas`'s,
//! shared with studio and `nexus library`; this module only draws and keys.

use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::{Popup, clamp_cursor};
use nexus_core::canvas::dialogs::{self, Dialogs};
use nexus_core::canvas::{self, Entry, files, settings};
use nexus_core::deck::link::Kind;
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use crate::ui::style::SEP;

use super::chrome;

/// What the popup is asking right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Browse,
    /// `e`: which format to export the selected file in.
    ExportAs,
    /// `d`: really move the selected file to the trash folder?
    ConfirmDelete,
}

#[derive(Default)]
pub struct LibraryPopup {
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub mode: Mode,
}

/// The formats `e` offers, as `(key, word, label)`.
const DECK_FORMATS: &[(char, &str, &str)] = &[
    ('p', "pptx", "pptx"),
    ('f', "pdf", "pdf"),
    ('g', "png", "png zip"),
    ('h', "html", "html"),
    ('j', "json", "json"),
];
const DOC_FORMATS: &[(char, &str, &str)] = &[
    ('w', "docx", "docx"),
    ('f', "pdf", "pdf"),
    ('m', "md", "md"),
    ('h', "html", "html"),
];

fn formats(kind: Kind) -> &'static [(char, &'static str, &'static str)] {
    match kind {
        Kind::Deck => DECK_FORMATS,
        Kind::Doc => DOC_FORMATS,
    }
}

fn files_dir(app: &AppView) -> PathBuf {
    app.space.files_dir(&app.active_space.name)
}

/// `/library`: list the space and open the popup.
pub fn open(app: &mut AppView) {
    if app.incognito {
        app.push_status("library isn't available in incognito mode");
        return;
    }
    refresh(app);
    app.library.selected = 0;
    app.library.mode = Mode::Browse;
    app.popup = Popup::Library;
}

fn refresh(app: &mut AppView) {
    app.library.entries = canvas::library(&files_dir(app));
    app.library.selected = app
        .library
        .selected
        .min(app.library.entries.len().saturating_sub(1));
}

fn selected(app: &AppView) -> Option<&Entry> {
    app.library.entries.get(app.library.selected)
}

fn rows(app: &AppView, width: usize) -> Vec<ListItem<'static>> {
    let dim = Style::default().fg(app.theme.fg_dim);
    if app.library.entries.is_empty() {
        return vec![chrome::empty_placeholder(
            "no decks or docs yet — ask the model to draft one",
            &app.theme,
        )];
    }
    let now = Utc::now();
    app.library
        .entries
        .iter()
        .map(|entry| {
            let tag = match entry.kind {
                Kind::Deck => "deck",
                Kind::Doc => "doc ",
            };
            let title = Line::from(vec![
                Span::styled(format!("{tag}  "), dim),
                Span::styled(
                    chrome::truncate(&entry.title, width.saturating_sub(6)),
                    Style::default().fg(app.theme.fg),
                ),
            ]);
            let mut meta = vec![
                entry.count_label(),
                canvas::relative_time(entry.modified, now),
            ];
            if let Some(design) = &entry.design {
                meta.push(design.clone());
            }
            let meta = Line::from(Span::styled(format!("      {}", meta.join(SEP)), dim));
            ListItem::new(vec![
                chrome::fit_line(title, width),
                chrome::fit_line(meta, width),
                Line::from(""),
            ])
        })
        .collect()
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "library", chrome::WIDE);
    let content_w = usize::from(area.width.saturating_sub(5 + 2 * chrome::PAD));
    let total = app.library.entries.len();
    let (title, hint, tone) = match (app.library.mode, selected(app)) {
        (Mode::ConfirmDelete, Some(entry)) => (
            chrome::danger_title(
                app,
                format!(
                    "move \"{}\" to the trash folder?",
                    chrome::truncate(&entry.title, 32)
                ),
                "y confirm · esc cancel",
            ),
            "y confirm · esc cancel".to_string(),
            chrome::Tone::Danger,
        ),
        (Mode::ExportAs, Some(entry)) => {
            let choices = formats(entry.kind)
                .iter()
                .map(|(key, _, label)| format!("{key} {label}"))
                .collect::<Vec<_>>()
                .join(SEP);
            (
                chrome::popup_title(
                    app,
                    format!("export \"{}\" as", chrome::truncate(&entry.title, 28)),
                ),
                format!("{choices} · esc back"),
                chrome::Tone::Normal,
            )
        }
        _ => (
            chrome::popup_title(app, "library"),
            if total == 0 {
                "esc close".to_string()
            } else {
                format!(
                    "{}↑↓ · enter open in studio · e export · d delete",
                    chrome::count_hint(total, "file")
                )
            },
            chrome::Tone::Normal,
        ),
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(rows(app, content_w), &app.theme);
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(app.library.selected.min(total - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, total, 3, app);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match app.library.mode {
        Mode::ConfirmDelete => confirm_key(app, key),
        Mode::ExportAs => export_key(app, key),
        Mode::Browse => browse_key(app, key),
    }
}

fn browse_key(app: &mut AppView, key: KeyEvent) {
    let total = app.library.entries.len();
    let page = 10;
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => {
            app.library.selected = clamp_cursor(app.library.selected, total, -1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.library.selected = clamp_cursor(app.library.selected, total, 1);
        }
        KeyCode::PageUp => app.library.selected = clamp_cursor(app.library.selected, total, -page),
        KeyCode::PageDown => app.library.selected = clamp_cursor(app.library.selected, total, page),
        KeyCode::Home => app.library.selected = 0,
        KeyCode::End => app.library.selected = total.saturating_sub(1),
        KeyCode::Enter => open_selected(app),
        KeyCode::Char('e') if selected(app).is_some() => app.library.mode = Mode::ExportAs,
        KeyCode::Char('d') if selected(app).is_some() => app.library.mode = Mode::ConfirmDelete,
        _ => {}
    }
}

/// Enter: open the file in studio, which links the window to this conversation.
fn open_selected(app: &mut AppView) {
    let Some(path) = selected(app).map(|entry| entry.path.display().to_string()) else {
        return;
    };
    app.popup = Popup::None;
    if let Err(error) = app.open_studio(Some(&path)) {
        app.push_status(format!("{error:#}"));
    }
}

fn confirm_key(app: &mut AppView, key: KeyEvent) {
    let ctrl_d = key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('y' | 'Y') => delete_selected(app),
        _ if ctrl_d => delete_selected(app),
        KeyCode::Esc | KeyCode::Char('n') => app.library.mode = Mode::Browse,
        _ => {}
    }
}

fn delete_selected(app: &mut AppView) {
    app.library.mode = Mode::Browse;
    let Some((relative, title)) = selected(app).map(|e| (e.relative.clone(), e.title.clone()))
    else {
        return;
    };
    match files::delete(&files_dir(app), &relative) {
        Ok(_) => app.push_status(format!("moved \"{title}\" to the trash folder (.trash)")),
        Err(error) => app.push_status(format!("delete failed: {error:#}")),
    }
    refresh(app);
}

fn export_key(app: &mut AppView, key: KeyEvent) {
    let Some(kind) = selected(app).map(|e| e.kind) else {
        app.library.mode = Mode::Browse;
        return;
    };
    match key.code {
        KeyCode::Esc => app.library.mode = Mode::Browse,
        KeyCode::Char(c) => {
            if let Some((_, word, _)) = formats(kind).iter().find(|(key, _, _)| *key == c) {
                app.library.mode = Mode::Browse;
                export_selected(app, word);
            }
        }
        _ => {}
    }
}

/// Export the selected file in `word` format: through the Save dialog when a
/// desktop session is there, otherwise into the space's `exports/` folder.
fn export_selected(app: &mut AppView, word: &str) {
    let Some(entry) = selected(app).cloned() else {
        return;
    };
    let files = files_dir(app);
    let result = (|| -> Result<Option<(PathBuf, bool)>> {
        let extension = files::export_extension(entry.kind, word)?;
        let dialog = app.dialogs.available();
        let chosen = if dialog {
            let start = settings::export_dir(&app.space.root, &files);
            let name = files::export_file_name(&entry.title, extension);
            let filters = dialogs::save_filter(extension.rsplit('.').next().unwrap_or(extension));
            let Some(chosen) = app.dialogs.save_file(
                &format!("Export {}", entry.title),
                start.as_deref(),
                &name,
                &filters,
            ) else {
                return Ok(None);
            };
            Some(files::with_extension(chosen, extension))
        } else {
            None
        };
        // Markdown's images go beside the file, or into exports/assets.
        let images = chosen
            .as_deref()
            .and_then(std::path::Path::parent)
            .map_or_else(|| files.join("exports/assets"), |dir| dir.join("assets"));
        let output = files::export_file(&files, &entry.relative, word, None, &images)?;
        let written = match &chosen {
            Some(path) => {
                let written = files::write_chosen(&output.bytes, path)?;
                let _ = settings::remember_export(&app.space.root, &files, &written);
                written
            }
            None => nexus_core::deck::export::write_export(&output, &files, None)?,
        };
        Ok(Some((written, dialog)))
    })();
    match result {
        Ok(Some((path, true))) => app.push_status(format!("exported to {}", path.display())),
        Ok(Some((path, false))) => app.push_status(format!(
            "no desktop session for a Save dialog — exported to {}",
            path.display()
        )),
        Ok(None) => app.push_status("export cancelled"),
        Err(error) => app.push_status(format!("export failed: {error:#}")),
    }
}

/// The dialogs the TUI asks: the operating system's, except in tests.
#[must_use]
pub fn default_dialogs() -> Box<dyn Dialogs> {
    #[cfg(test)]
    {
        Box::new(dialogs::Scripted::new([]))
    }
    #[cfg(not(test))]
    {
        Box::new(dialogs::Native)
    }
}
