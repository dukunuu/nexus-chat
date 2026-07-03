use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SessionMode;
    let area = crate::ui::centered(f.area(), 64, 74);
    f.render_widget(Clear, area);

    let sessions = app.filtered_sessions();
    let width = area.width.saturating_sub(4) as usize; // inside border + highlight symbol
    let dim = Style::default().fg(Color::DarkGray);

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            // id on top (model-generated slug, else a uuid prefix), with the
            // created-at date right-aligned on the same row.
            let id = s.slug.clone().unwrap_or_else(|| format!("{}…", &s.id[..8.min(s.id.len())]));
            let when = crate::ui::fmt_created(&s.created_at);
            let gap = width.saturating_sub(id.chars().count() + 1 + when.chars().count() + 2);
            let top = Line::from(vec![
                Span::styled(format!("#{id}"), Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(gap)),
                Span::styled(when, dim),
            ]);
            // title (truncated) with the model dimmed after it.
            let title = truncate(&s.title, width.saturating_sub(s.model.chars().count() + 5));
            let body = Line::from(vec![
                Span::styled(title, Style::default().fg(Color::White)),
                Span::styled(format!("  {}", s.model), dim),
            ]);
            ListItem::new(vec![top, body, Line::from("")])
        })
        .collect();

    // Title bar doubles as the search box / rename field / delete prompt.
    let title = match app.session_mode {
        SessionMode::Rename => format!(" rename: {}▏  (Enter save · Esc cancel) ", app.session_edit),
        SessionMode::ConfirmDelete => {
            let name = app.selected_session().map(|s| s.title).unwrap_or_default();
            format!(" delete \"{}\"? (Ctrl+D confirm · Esc cancel) ", truncate(&name, 30))
        }
        SessionMode::Browse => {
            let keys = if app.settings.hide_hints { "" } else { "  (Ctrl+R rename · Ctrl+D delete)" };
            format!(" session — search: {}▏{keys} ", app.session_filter)
        }
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
    use crate::app::SessionMode;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match app.session_mode {
        // Renaming: type into the edit buffer; Enter saves, Esc cancels.
        SessionMode::Rename => match key.code {
            KeyCode::Esc => app.session_mode = SessionMode::Browse,
            KeyCode::Enter => app.confirm_rename()?,
            KeyCode::Backspace => {
                app.session_edit.pop();
            }
            KeyCode::Char(c) => app.session_edit.push(c),
            _ => {}
        },
        // Delete confirm: Ctrl+D again deletes, Esc cancels, anything else ignored.
        SessionMode::ConfirmDelete => match key.code {
            KeyCode::Char('d') if ctrl => app.confirm_delete()?,
            KeyCode::Esc => app.session_mode = SessionMode::Browse,
            _ => {}
        },
        SessionMode::Browse => match key.code {
            KeyCode::Esc => app.popup = crate::app::Popup::None,
            KeyCode::Enter => app.confirm_session()?,
            KeyCode::Up => app.move_session_selection(-1),
            KeyCode::Down => app.move_session_selection(1),
            // Ctrl+R rename, Ctrl+D delete — leave plain letters for the filter.
            KeyCode::Char('r') if ctrl => app.start_rename(),
            KeyCode::Char('d') if ctrl => app.session_mode = SessionMode::ConfirmDelete,
            KeyCode::Backspace => app.session_filter_pop(),
            KeyCode::Char(c) if !ctrl => app.session_filter_push(c),
            _ => {}
        },
    }
    Ok(())
}
