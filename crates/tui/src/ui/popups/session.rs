use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome;
use crate::ui::style::glyph;

#[allow(clippy::too_many_lines)] // row design + preview strip
pub fn render(f: &mut Frame, app: &mut AppView) {
    use nexus_core::app::SessionMode;
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
            .map(|c| (sid.clone(), chrome::truncate(&c, 260)));
    }
    let preview = app
        .session_preview
        .as_ref()
        .map(|(_, p)| p.clone())
        .unwrap_or_default();

    let sessions = app.filtered_sessions();
    // Inside the border, minus the scrollbar gutter, the `▸ ` highlight, and
    // one column of air before the scrollbar.
    let width = area.width.saturating_sub(6 + 2 * chrome::PAD) as usize;
    let dim = Style::default().fg(app.theme.fg_dim);
    let active_id = app.session.as_ref().map(|s| s.id.clone());

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            // Running work (reply, compaction, research) is the assistant
            // busy: ⟳ in the agent color. A finished-while-away reply waits
            // on you: ● in the interactive accent. Otherwise the session's
            // kind: ↪ linked research, ◇ research.
            let compacting_here = app.is_compacting_session(&s.id);
            let streaming_here = app.chat_task_for_session(&s.id).is_some();
            let researching_here = app
                .research_running
                .as_ref()
                .is_some_and(|(id, _)| *id == s.id);
            let agent = Style::default().fg(app.theme.accent2);
            let marker = if compacting_here || streaming_here || researching_here {
                Some(Span::styled(format!("{} ", glyph::RUNNING), agent))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled(
                    format!("{} ", glyph::DOT),
                    Style::default().fg(app.theme.accent),
                ))
            } else if s.research_parent_id.is_some() {
                Some(Span::styled(format!("{} ", glyph::LINK), dim))
            } else if s.kind == "research" {
                Some(Span::styled(format!("{} ", glyph::RESEARCH), agent))
            } else {
                None
            };
            // Title first — it's what people scan for — with the date
            // right-aligned; widths are display cells (emoji markers are 2).
            let when = crate::ui::fmt_created(&s.created_at);
            let marker_w = marker.as_ref().map_or(0, Span::width);
            let title_max = width.saturating_sub(marker_w + when.chars().count() + 2);
            let title = chrome::truncate(&s.title, title_max);
            let gap = width.saturating_sub(marker_w + title.chars().count() + when.chars().count());
            let mut top_spans: Vec<Span> = marker.into_iter().collect();
            top_spans.extend([
                Span::styled(
                    title,
                    Style::default()
                        .fg(app.theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ".repeat(gap)),
                Span::styled(when, dim),
            ]);
            // Second line: slug (else a uuid prefix) · model, or an explicit
            // label while compaction runs so ⟳ never needs guessing.
            let slug = s
                .slug
                .clone()
                .unwrap_or_else(|| format!("{}…", &s.id[..8.min(s.id.len())]));
            let mut detail = if compacting_here {
                format!("#{slug} · {} compacting…", glyph::RUNNING)
            } else {
                format!("#{slug} · {}", crate::ui::short_model_label(&s.model))
            };
            if active_id.as_deref() == Some(s.id.as_str()) {
                detail.push_str(" · open");
            }
            let body = Line::from(Span::styled(chrome::truncate(&detail, width), dim));
            ListItem::new(vec![Line::from(top_spans), body, Line::from("")])
        })
        .collect();

    // Title bar doubles as the search box / rename field / delete prompt;
    // the hint bar sits in the frame footer instead of crowding the title.
    let title = match app.session_mode {
        SessionMode::Rename => chrome::input_title(app, "rename session", &app.session_edit, ""),
        SessionMode::ConfirmDelete => {
            let name = app.selected_session().map(|s| s.title).unwrap_or_default();
            chrome::danger_title(
                app,
                format!("delete \"{}\"?", chrome::truncate(&name, 30)),
                "",
            )
        }
        SessionMode::Browse => chrome::filter_title(app, "sessions", &app.session_filter),
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
    chrome::render_list(f, list, &mut state, list_area, sessions.len(), 3, app);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use nexus_core::app::SessionMode;
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
            // Sessions support rename but not create.
            match classify_browse_key(key, false, true) {
                Some(super::BrowseAction::Close) => app.popup = nexus_core::app::Popup::None,
                Some(super::BrowseAction::MoveUp) => app.move_session_selection(-1),
                Some(super::BrowseAction::MoveDown) => app.move_session_selection(1),
                Some(
                    a @ (super::BrowseAction::PageUp
                    | super::BrowseAction::PageDown
                    | super::BrowseAction::Top
                    | super::BrowseAction::Bottom),
                ) => {
                    super::apply_page(|d| app.move_session_selection(d), a, 10);
                }
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
