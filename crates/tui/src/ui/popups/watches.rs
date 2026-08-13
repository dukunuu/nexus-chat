use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use nexus_core::app::{App, WatchMode};

use super::chrome;

#[allow(clippy::too_many_lines)] // two-line rows + due/scheduled status
pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), chrome::STANDARD.0, chrome::STANDARD.1);
    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = if app.watches_cache.is_empty() {
        vec![chrome::empty_placeholder(
            "no watches yet — /watch <topic> to start one",
            &app.theme,
        )]
    } else {
        app.watches_cache
            .iter()
            .map(|w| {
                let width = area.width.saturating_sub(6) as usize;
                let last_run = match &w.last_run_at {
                    Some(t) => crate::ui::fmt_created(t),
                    None => "never".to_string(),
                };
                // Next run: last run + interval, or "due" when past.
                let next = match &w.last_run_at {
                    Some(t) => chrono::DateTime::parse_from_rfc3339(t).ok().map_or_else(
                        || "soon".to_string(),
                        |dt| {
                            let due = dt.with_timezone(&chrono::Local)
                                + chrono::Duration::hours(w.interval_hours);
                            if due < chrono::Local::now() {
                                "due now".to_string()
                            } else {
                                let mins = (due - chrono::Local::now()).num_minutes();
                                if mins >= 48 * 60 {
                                    format!("in {}d", mins / (24 * 60))
                                } else if mins >= 120 {
                                    format!("in {}h", mins / 60)
                                } else {
                                    format!("in {mins}m")
                                }
                            }
                        },
                    ),
                    None => "due now".to_string(),
                };
                let active = app.chat_task_for_session(&w.session_id).is_some();
                let state = if active {
                    Span::styled("⟳ checking", Style::default().fg(app.theme.accent))
                } else {
                    Span::styled(next, dim)
                };
                let topic = chrome::truncate(
                    &w.topic,
                    width.saturating_sub(state.content.chars().count() + 2),
                );
                let pad =
                    width.saturating_sub(topic.chars().count() + state.content.chars().count() + 2);
                let top = Line::from(vec![
                    Span::styled(topic, Style::default().fg(app.theme.fg)),
                    Span::raw(" ".repeat(pad)),
                    state,
                ]);
                let meta = format!("every {}h · last run: {last_run}", w.interval_hours);
                let meta = chrome::truncate(&meta, (area.width.saturating_sub(4)) as usize);
                ListItem::new(vec![
                    top,
                    Line::from(Span::styled(meta, dim)),
                    Line::from(""),
                ])
            })
            .collect()
    };

    let title = match app.watch_mode {
        WatchMode::ConfirmDelete => {
            let topic = app
                .watches_cache
                .get(app.watch_selected)
                .map(|w| w.topic.clone())
                .unwrap_or_default();
            chrome::danger_title(app, format!("delete watch \"{topic}\"?"), "")
        }
        WatchMode::Browse => chrome::popup_title(app, "⏰", "watches"),
    };
    let hint = match app.watch_mode {
        WatchMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        WatchMode::Browse if app.watches_cache.is_empty() => {
            "no watches yet — /watch <topic> to start one".to_string()
        }
        WatchMode::Browse => format!(
            "{}↑↓ · Enter jump to session · Ctrl+D delete",
            chrome::count_hint(app.watches_cache.len(), "watch")
        ),
    };
    let tone = if app.watch_mode == WatchMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.watches_cache.is_empty() {
        state.select(Some(app.watch_selected.min(app.watches_cache.len() - 1)));
    }
    chrome::render_list(
        f,
        list,
        &mut state,
        inner,
        app.watches_cache.len(),
        3,
        &app.theme,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    match app.watch_mode {
        WatchMode::ConfirmDelete => match super::classify_confirm_delete_key(key) {
            Some(super::ConfirmDeleteAction::Yes) => {
                app.delete_selected_watch();
                app.watch_mode = WatchMode::Browse;
            }
            Some(super::ConfirmDeleteAction::No) => app.watch_mode = WatchMode::Browse,
            None => {}
        },
        WatchMode::Browse => match key.code {
            KeyCode::Esc => app.popup = nexus_core::app::Popup::None,
            KeyCode::Up => app.move_watch_selection(-1),
            KeyCode::Down => app.move_watch_selection(1),
            KeyCode::PageUp => app.move_watch_selection(-10),
            KeyCode::PageDown => app.move_watch_selection(10),
            KeyCode::Home => app.move_watch_selection(i32::MIN / 2),
            KeyCode::End => app.move_watch_selection(i32::MAX / 2),
            KeyCode::Enter => app.confirm_watch_session()?,
            KeyCode::Char('d')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
                    && !app.watches_cache.is_empty() =>
            {
                app.watch_mode = WatchMode::ConfirmDelete;
            }
            _ => {}
        },
    }
    Ok(())
}
