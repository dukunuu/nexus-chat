use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 50, 60);
    let inner = chrome::render_frame(
        f,
        area,
        chrome::hinted_title(app, "copy", "↑/↓, Enter, Esc"),
        &app.theme,
        true,
    );

    let items: Vec<ListItem> = app
        .copy_options
        .iter()
        .map(|o| ListItem::new(o.label.clone()))
        .collect();
    let list = chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.copy_options.is_empty() {
        state.select(Some(app.copy_selected.min(app.copy_options.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
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
