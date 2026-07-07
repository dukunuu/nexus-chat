use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};

use crate::app::App;

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SessionMode;
    let area = crate::ui::centered(f.area(), 64, 74);
    f.render_widget(Clear, area);

    let sessions = app.filtered_sessions();
    let width = area.width.saturating_sub(4) as usize; // inside border + highlight symbol
    let dim = Style::default().fg(app.theme.fg_dim);

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            // id on top (model-generated slug, else a uuid prefix), with the
            // created-at date right-aligned on the same row.
            let id = s
                .slug
                .clone()
                .unwrap_or_else(|| format!("{}…", &s.id[..8.min(s.id.len())]));
            let when = crate::ui::fmt_created(&s.created_at);
            // ⟳ = a response is streaming here; 🔎 = a research job is running
            // here; ● = finished while unviewed.
            let streaming_here = app
                .stream_session
                .as_ref()
                .is_some_and(|(id, _)| *id == s.id);
            let researching_here = app
                .research_running
                .as_ref()
                .is_some_and(|(id, _)| *id == s.id);
            let marker = if streaming_here {
                Some(Span::styled("⟳ ", Style::default().fg(app.theme.accent)))
            } else if researching_here {
                Some(Span::styled("🔎 ", Style::default().fg(app.theme.accent2)))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled("● ", Style::default().fg(app.theme.warning)))
            } else {
                None
            };
            let mlen = if marker.is_some() { 2 } else { 0 };
            let gap =
                width.saturating_sub(mlen + id.chars().count() + 1 + when.chars().count() + 2);
            let mut top_spans = Vec::new();
            if let Some(m) = marker {
                top_spans.push(m);
            }
            top_spans.extend([
                Span::styled(format!("#{id}"), Style::default().fg(app.theme.accent)),
                Span::raw(" ".repeat(gap)),
                Span::styled(when, dim),
            ]);
            let top = Line::from(top_spans);
            // title (truncated) with the model dimmed after it.
            let title = truncate(&s.title, width.saturating_sub(s.model.chars().count() + 5));
            let body = Line::from(vec![
                Span::styled(title, Style::default().fg(app.theme.fg)),
                Span::styled(format!("  {}", s.model), dim),
            ]);
            ListItem::new(vec![top, body, Line::from("")])
        })
        .collect();

    // Title bar doubles as the search box / rename field / delete prompt.
    let title: Line = match app.session_mode {
        SessionMode::Rename => Line::from(format!(
            " rename: {}▏  (Enter save · Esc cancel) ",
            app.session_edit
        )),
        SessionMode::ConfirmDelete => {
            let name = app.selected_session().map(|s| s.title).unwrap_or_default();
            Line::from(format!(
                " delete \"{}\"? (Ctrl+D confirm · Esc cancel) ",
                truncate(&name, 30)
            ))
        }
        SessionMode::Browse => {
            let keys = if app.settings.hide_hints {
                ""
            } else {
                "  (Ctrl+R rename · Ctrl+D delete)"
            };
            let mut spans = vec![Span::raw(" session — search: ")];
            spans.extend(app.session_filter.spans(&app.theme));
            spans.push(Span::raw(format!("{keys} ")));
            Line::from(spans)
        }
    };

    let list = List::new(items)
        .block(chrome::popup_block(title, &app.theme))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !sessions.is_empty() {
        state.select(Some(app.session_selected.min(sessions.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// Truncate `s` to `max` chars, appending `…` when it overflows.
fn truncate(s: &str, max: usize) -> String {
    let max = max.max(1);
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}…", s.chars().take(keep).collect::<String>())
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use crate::app::SessionMode;
    match app.session_mode {
        // Renaming: type into the edit buffer; Enter saves, Esc cancels.
        SessionMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.session_mode = SessionMode::Browse,
            Some(EditAction::Save) => app.confirm_rename()?,
            Some(EditAction::Backspace) => {
                app.session_edit.pop();
            }
            Some(EditAction::Push(c)) => app.session_edit.push(c),
            None => {}
        },
        // Delete confirm: Ctrl+D again deletes, Esc cancels, anything else ignored.
        SessionMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_delete()?,
            Some(ConfirmDeleteAction::No) => app.session_mode = SessionMode::Browse,
            None => {}
        },
        SessionMode::Browse => {
            if key.code == KeyCode::Enter {
                return app.confirm_session();
            }
            // Cursor move/select/clipboard on the search filter takes priority
            // over the rest of Browse mode's bindings.
            if app.session_filter.key(key, &mut app.clipboard) {
                return Ok(());
            }
            // No create/rename-gating divergence here: session supports
            // rename but not create.
            match classify_browse_key(key, false, true) {
                Some(super::BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(super::BrowseAction::MoveUp) => app.move_session_selection(-1),
                Some(super::BrowseAction::MoveDown) => app.move_session_selection(1),
                Some(super::BrowseAction::Rename) => app.start_rename(),
                Some(super::BrowseAction::ConfirmDelete) => {
                    app.session_mode = SessionMode::ConfirmDelete
                }
                Some(super::BrowseAction::Backspace) => app.session_filter_pop(),
                Some(super::BrowseAction::Filter(c)) => app.session_filter_push(c),
                Some(super::BrowseAction::Create) | None => {}
            }
        }
    }
    Ok(())
}
