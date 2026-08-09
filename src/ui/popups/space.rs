use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    use crate::app::SpaceMode;
    use crate::db::DEFAULT_SPACE;
    let area = crate::ui::centered(f.area(), chrome::STANDARD.0, chrome::STANDARD.1);

    let dim = Style::default().fg(app.theme.fg_dim);
    let spaces = app.filtered_spaces();
    let items: Vec<ListItem> = spaces
        .iter()
        .map(|s| {
            let n = app.db.count_sessions(&s.id).unwrap_or(0);
            let mark = if s.name == app.active_space.name {
                "● "
            } else {
                "  "
            };
            let name = if s.name == DEFAULT_SPACE {
                format!("{mark}{} (default)", s.name)
            } else {
                format!("{mark}{}", s.name)
            };
            let line = Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(
                    format!("  {n} session{}", if n == 1 { "" } else { "s" }),
                    dim,
                ),
                Span::styled(
                    format!("  · {}", crate::ui::fmt_created(&s.created_at)),
                    dim,
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = match app.space_mode {
        SpaceMode::Create => chrome::input_title(
            app,
            "new space",
            &app.space_edit,
            "Enter create · Esc cancel",
        ),
        SpaceMode::Rename => chrome::input_title(
            app,
            "rename space",
            &app.space_edit,
            "Enter save · Esc cancel",
        ),
        SpaceMode::ConfirmDelete => {
            let name = app.selected_space().map(|s| s.name).unwrap_or_default();
            chrome::danger_title(
                app,
                format!("delete \"{name}\"? sessions move to default."),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        SpaceMode::Browse => chrome::filter_title(app, "🗃", "spaces", &app.space_filter),
    };

    let hint = match app.space_mode {
        SpaceMode::Create => "Enter create · Esc cancel".to_string(),
        SpaceMode::Rename => "Enter save · Esc cancel".to_string(),
        SpaceMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        SpaceMode::Browse if spaces.is_empty() => {
            "no spaces match — type to clear the filter".to_string()
        }
        SpaceMode::Browse => format!(
            "{}↑↓ · Enter open · Ctrl+N new · Ctrl+R rename · Ctrl+D delete · Ctrl+E docs · Ctrl+K memory",
            chrome::count_hint(spaces.len(), "space")
        ),
    };
    let tone = if app.space_mode == SpaceMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !spaces.is_empty() {
        state.select(Some(app.space_selected.min(spaces.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use crate::app::SpaceMode;
    use crate::db::DEFAULT_SPACE;
    match app.space_mode {
        SpaceMode::Create | SpaceMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.space_mode = SpaceMode::Browse,
            Some(EditAction::Save) => match app.space_mode {
                SpaceMode::Create => app.confirm_space_create()?,
                _ => app.confirm_space_rename()?,
            },
            Some(EditAction::Backspace) => {
                app.space_edit.pop();
            }
            Some(EditAction::Push(c)) => app.space_edit.push(c),
            None => {}
        },
        SpaceMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_space_delete()?,
            Some(ConfirmDeleteAction::No) => app.space_mode = SpaceMode::Browse,
            None => {}
        },
        SpaceMode::Browse => {
            if key.code == KeyCode::Enter {
                app.confirm_space();
                return Ok(());
            }
            if app.space_filter.key(key, &mut app.clipboard) {
                return Ok(());
            }
            match classify_browse_key(key, true, true) {
                Some(super::BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(super::BrowseAction::MoveUp) => app.move_space_selection(-1),
                Some(super::BrowseAction::MoveDown) => app.move_space_selection(1),
                Some(super::BrowseAction::Create) => app.start_space_create(),
                Some(super::BrowseAction::Rename) => app.start_space_rename(),
                // The default space is never deletable.
                Some(super::BrowseAction::ConfirmDelete)
                    if app
                        .selected_space()
                        .is_some_and(|s| s.name != DEFAULT_SPACE) =>
                {
                    app.space_mode = SpaceMode::ConfirmDelete;
                }
                Some(super::BrowseAction::ConfirmDelete) | None => {}
                Some(super::BrowseAction::Backspace) => app.space_filter_pop(),
                Some(super::BrowseAction::Filter(c)) => app.space_filter_push(c),
            }
        }
    }
    Ok(())
}
