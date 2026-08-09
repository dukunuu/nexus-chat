use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

pub fn render(f: &mut Frame, app: &mut App) {
    use crate::app::SessionMode;
    let area = crate::ui::centered(f.area(), chrome::TALL.0, chrome::TALL.1);

    // Preview strip: the selected session's last exchange, so the picker
    // tells you what a session is about before you open it. Re-queried only
    // when the selection changes (selected_session returns an owned clone,
    // so this must run before filtered_sessions borrows the cache).
    let sid = app.selected_session().map(|s| s.id).unwrap_or_default();
    if app.session_preview.as_ref().map(|(id, _)| id) != Some(&sid) {
        app.session_preview = app
            .db
            .last_message_preview(&sid)
            .map(|c| (sid.clone(), truncate(&c, 260)));
    }
    let preview = app
        .session_preview
        .as_ref()
        .map(|(_, p)| format!("↳ {p}"))
        .unwrap_or_default();

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
            let streaming_here = app.chat_task_for_session(&s.id).is_some();
            let researching_here = app
                .research_running
                .as_ref()
                .is_some_and(|(id, _)| *id == s.id);
            let is_research = s.kind == "research";
            let is_linked_research = s.research_parent_id.is_some();
            let marker = if streaming_here {
                Some(Span::styled("⟳ ", Style::default().fg(app.theme.accent)))
            } else if researching_here {
                Some(Span::styled("🔎 ", Style::default().fg(app.theme.accent2)))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled("● ", Style::default().fg(app.theme.warning)))
            } else if is_linked_research {
                Some(Span::styled("↪ ", dim))
            } else if is_research {
                Some(Span::styled("🔬 ", Style::default().fg(app.theme.accent2)))
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

    // Title bar doubles as the search box / rename field / delete prompt;
    // the hint bar sits in the frame footer instead of crowding the title.
    let title = match app.session_mode {
        SessionMode::Rename => chrome::input_title(app, "rename session", &app.session_edit, ""),
        SessionMode::ConfirmDelete => {
            let name = app.selected_session().map(|s| s.title).unwrap_or_default();
            chrome::danger_title(app, format!("delete \"{}\"?", truncate(&name, 30)), "")
        }
        SessionMode::Browse => chrome::filter_title(app, "🗂", "sessions", &app.session_filter),
    };
    let hint = match app.session_mode {
        SessionMode::Rename => "Enter save · Esc cancel".to_string(),
        SessionMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        SessionMode::Browse if sessions.is_empty() => {
            "no sessions match — type to clear the filter".to_string()
        }
        SessionMode::Browse => format!(
            "{}↑↓ · Enter open · Ctrl+R rename · Ctrl+D delete",
            chrome::count_hint(sessions.len(), "session")
        ),
    };
    let tone = if app.session_mode == SessionMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let (list_area, detail_area) = chrome::split_with_detail(inner, &preview);
    chrome::render_detail(f, detail_area, &preview, &app.theme);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !sessions.is_empty() {
        state.select(Some(app.session_selected.min(sessions.len() - 1)));
    }
    f.render_stateful_widget(list, list_area, &mut state);
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

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
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
                    app.session_mode = SessionMode::ConfirmDelete;
                }
                Some(super::BrowseAction::Backspace) => app.session_filter_pop(),
                Some(super::BrowseAction::Filter(c)) => app.session_filter_push(c),
                Some(super::BrowseAction::Create) | None => {}
            }
        }
    }
    Ok(())
}
