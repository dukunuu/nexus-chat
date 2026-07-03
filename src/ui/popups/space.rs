use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SpaceMode;
    use crate::db::DEFAULT_SPACE;
    let area = crate::ui::centered(f.area(), 50, 60);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
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
                Span::styled(name, Style::default().fg(Color::White)),
                Span::styled(format!("  {n} session{}", if n == 1 { "" } else { "s" }), dim),
                Span::styled(format!("  · {}", crate::ui::fmt_created(&s.created_at)), dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = match app.space_mode {
        SpaceMode::Create => format!(" new space: {}▏  (Enter create · Esc cancel) ", app.space_edit),
        SpaceMode::Rename => format!(" rename: {}▏  (Enter save · Esc cancel) ", app.space_edit),
        SpaceMode::ConfirmDelete => {
            let name = app.selected_space().map(|s| s.name).unwrap_or_default();
            format!(" delete \"{name}\"? sessions move to default. (Ctrl+D confirm · Esc cancel) ")
        }
        SpaceMode::Browse => {
            let keys = if app.settings.hide_hints {
                ""
            } else {
                "  (Ctrl+N new · Ctrl+R rename · Ctrl+D delete · Ctrl+E instructions · Ctrl+K memory)"
            };
            format!(" space — search: {}▏{keys} ", app.space_filter)
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
    if !spaces.is_empty() {
        state.select(Some(app.space_selected.min(spaces.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use crate::app::SpaceMode;
    use crate::db::DEFAULT_SPACE;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match app.space_mode {
        SpaceMode::Create | SpaceMode::Rename => match key.code {
            KeyCode::Esc => app.space_mode = SpaceMode::Browse,
            KeyCode::Enter => match app.space_mode {
                SpaceMode::Create => app.confirm_space_create()?,
                _ => app.confirm_space_rename()?,
            },
            KeyCode::Backspace => {
                app.space_edit.pop();
            }
            KeyCode::Char(c) => app.space_edit.push(c),
            _ => {}
        },
        SpaceMode::ConfirmDelete => match key.code {
            KeyCode::Char('d') if ctrl => app.confirm_space_delete()?,
            KeyCode::Esc => app.space_mode = SpaceMode::Browse,
            _ => {}
        },
        SpaceMode::Browse => match key.code {
            KeyCode::Esc => app.popup = crate::app::Popup::None,
            KeyCode::Enter => app.confirm_space()?,
            KeyCode::Up => app.move_space_selection(-1),
            KeyCode::Down => app.move_space_selection(1),
            KeyCode::Char('n') if ctrl => app.start_space_create(),
            KeyCode::Char('r') if ctrl => app.start_space_rename(),
            // The default space is never deletable.
            KeyCode::Char('d')
                if ctrl && app.selected_space().is_some_and(|s| s.name != DEFAULT_SPACE) =>
            {
                app.space_mode = SpaceMode::ConfirmDelete;
            }
            KeyCode::Backspace => app.space_filter_pop(),
            KeyCode::Char(c) if !ctrl => app.space_filter_push(c),
            _ => {}
        },
    }
    Ok(())
}
