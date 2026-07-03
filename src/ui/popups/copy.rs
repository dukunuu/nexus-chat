use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 50, 60);
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .copy_options
        .iter()
        .map(|o| ListItem::new(o.label.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(crate::ui::hint_title(app, " copy ", "copy — ↑/↓, Enter, Esc")),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    let mut state = ListState::default();
    if !app.copy_options.is_empty() {
        state.select(Some(app.copy_selected.min(app.copy_options.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = crate::app::Popup::None,
        KeyCode::Enter => app.confirm_copy(),
        KeyCode::Up => app.move_copy_selection(-1),
        KeyCode::Down => app.move_copy_selection(1),
        _ => {}
    }
}
