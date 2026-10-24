use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use crossterm::event::KeyModifiers;
use nexus_core::app::AppsMode;

use super::chrome;

/// The file-name and rename prompts: an input line over an empty list.
fn render_prompt(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) -> bool {
    let (label, hint) = match app.apps_mode {
        AppsMode::EditFile => {
            let name = app
                .apps_cache
                .get(app.apps_selected)
                .cloned()
                .unwrap_or_default();
            (
                format!("edit {name}/"),
                "Enter open in $EDITOR · Esc cancel",
            )
        }
        AppsMode::RenamePanel => {
            let name = app
                .selected_pinned()
                .map(|pinned| pinned.name.clone())
                .unwrap_or_default();
            (format!("rename {name} to"), "Enter rename · Esc cancel")
        }
        _ => return false,
    };
    let title = chrome::input_title(app, label, &app.apps_edit, hint);
    let inner = chrome::render_frame(f, area, title, &app.theme, true, chrome::Tone::Normal);
    let list = chrome::standard_list(Vec::<ListItem>::new(), &app.theme);
    f.render_widget(list, inner);
    true
}

/// Web apps first, then pinned panels.
fn rows(app: &AppView, content_w: usize) -> Vec<ListItem<'static>> {
    let dim = Style::default().fg(app.theme.fg_dim);
    let row = |name: &str, meta: &str| {
        ListItem::new(vec![
            Line::from(Span::styled(
                chrome::truncate(name, content_w.saturating_sub(1)),
                Style::default().fg(app.theme.fg),
            )),
            Line::from(Span::styled(
                format!("  {}", chrome::truncate(meta, content_w.saturating_sub(3))),
                dim,
            )),
            Line::from(""),
        ])
    };
    if app.apps_total() == 0 {
        return vec![ListItem::new(Line::from(Span::styled(
            "no apps yet — ask the model to build one",
            dim,
        )))];
    }
    let web = app.apps_cache.iter().map(|name| {
        let n = app.app_file_count(name);
        let url = app
            .app_url(name)
            .unwrap_or_else(|| "server not running".to_string());
        row(
            name,
            &format!("{n} file{} · {url}", if n == 1 { "" } else { "s" }),
        )
    });
    let panels = app
        .pinned_apps
        .iter()
        .map(|pinned| row(&pinned.name, &format!("panel · {}", pinned.title)));
    web.chain(panels).collect()
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "apps", chrome::STANDARD);
    if render_prompt(f, app, area) {
        return;
    }
    let total = app.apps_total();
    let content_w = (area.width.saturating_sub(5 + 2 * chrome::PAD)) as usize;
    let items = rows(app, content_w);

    let title = match app.apps_mode {
        AppsMode::EditFile | AppsMode::RenamePanel => unreachable!(),
        AppsMode::ConfirmDelete => {
            let question = app.selected_pinned().map_or_else(
                || {
                    let name = app
                        .apps_cache
                        .get(app.apps_selected)
                        .cloned()
                        .unwrap_or_default();
                    format!("remove app \"{name}\" and all its files?")
                },
                |pinned| format!("unpin panel app \"{}\"?", pinned.name),
            );
            chrome::danger_title(app, question, "Ctrl+D confirm · Esc cancel")
        }
        AppsMode::Browse => chrome::popup_title(app, "apps"),
    };
    let hint = match app.apps_mode {
        AppsMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        _ if total == 0 => "ask the model to build you one".to_string(),
        _ if app.selected_pinned().is_some() => format!(
            "{}↑↓ · Enter open · Ctrl+R rename · Ctrl+D unpin",
            chrome::count_hint(total, "app")
        ),
        _ => format!(
            "{}↑↓ · Enter open · Ctrl+E edit file · Ctrl+D remove",
            chrome::count_hint(total, "app")
        ),
    };
    let tone = if app.apps_mode == AppsMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(app.apps_selected.min(total - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, total, 3, app);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
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
        AppsMode::RenamePanel => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.apps_mode = AppsMode::Browse,
            Some(EditAction::Save) => app.confirm_panel_rename(),
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
            if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.start_panel_rename();
                return Ok(());
            }
            match classify_browse_key(key, false, false) {
                Some(BrowseAction::Close) => app.popup = nexus_core::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_apps_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_apps_selection(1),
                Some(
                    a @ (BrowseAction::PageUp
                    | BrowseAction::PageDown
                    | BrowseAction::Top
                    | BrowseAction::Bottom),
                ) => {
                    super::apply_page(|d| app.move_apps_selection(d), a, 10);
                }
                Some(BrowseAction::ConfirmDelete) if app.apps_total() > 0 => {
                    app.apps_mode = AppsMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
