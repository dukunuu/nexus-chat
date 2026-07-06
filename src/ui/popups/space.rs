use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};

use crate::app::App;

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SpaceMode;
    use crate::db::DEFAULT_SPACE;
    let area = crate::ui::centered(f.area(), 50, 60);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(app.theme.fg_dim);
    let spaces = app.filtered_spaces();
    let items: Vec<ListItem> = spaces
        .iter()
        .map(|s| {
            let n = app.db.count_sessions(&s.id).unwrap_or(0);
            let mark = if s.name == app.active_space.name { "● " } else { "  " };
            let name = if s.name == DEFAULT_SPACE {
                format!("{mark}{} (default)", s.name)
            } else {
                format!("{mark}{}", s.name)
            };
            let line = Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(format!("  {n} session{}", if n == 1 { "" } else { "s" }), dim),
                Span::styled(format!("  · {}", crate::ui::fmt_created(&s.created_at)), dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title: Line = match app.space_mode {
        SpaceMode::Create => Line::from(format!(" new space: {}▏  (Enter create · Esc cancel) ", app.space_edit)),
        SpaceMode::Rename => Line::from(format!(" rename: {}▏  (Enter save · Esc cancel) ", app.space_edit)),
        SpaceMode::ConfirmDelete => {
            let name = app.selected_space().map(|s| s.name).unwrap_or_default();
            Line::from(format!(" delete \"{name}\"? sessions move to default. (Ctrl+D confirm · Esc cancel) "))
        }
        SpaceMode::Browse => {
            let keys = if app.settings.hide_hints {
                ""
            } else {
                "  (Ctrl+N new · Ctrl+R rename · Ctrl+D delete · Ctrl+E instructions · Ctrl+K memory)"
            };
            let mut spans = vec![Span::raw(" space — search: ")];
            spans.extend(app.space_filter.spans(&app.theme));
            spans.push(Span::raw(format!("{keys} ")));
            Line::from(spans)
        }
    };

    let list = List::new(items)
        .block(chrome::popup_block(title, &app.theme))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !spaces.is_empty() {
        state.select(Some(app.space_selected.min(spaces.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key, classify_edit_key};
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
                return app.confirm_space();
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
                    if app.selected_space().is_some_and(|s| s.name != DEFAULT_SPACE) =>
                {
                    app.space_mode = SpaceMode::ConfirmDelete;
                }
                Some(super::BrowseAction::ConfirmDelete) => {}
                Some(super::BrowseAction::Backspace) => app.space_filter_pop(),
                Some(super::BrowseAction::Filter(c)) => app.space_filter_push(c),
                None => {}
            }
        }
    }
    Ok(())
}
