use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), chrome::SMALL.0, chrome::SMALL.1);
    let hint = if app.copy_options.is_empty() {
        "Esc close".to_string()
    } else {
        format!(
            "{}↑↓ · Enter copy · Esc close",
            chrome::count_hint(app.copy_options.len(), "option")
        )
    };
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "📋", "copy"),
        &hint,
        app,
        true,
        chrome::Tone::Normal,
    );

    let items: Vec<ListItem> = if app.copy_options.is_empty() {
        vec![chrome::empty_placeholder(
            "nothing to copy here yet",
            &app.theme,
        )]
    } else {
        app.copy_options
            .iter()
            .map(|o| ListItem::new(o.label.clone()))
            .collect()
    };
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.copy_options.is_empty() {
        state.select(Some(app.copy_selected.min(app.copy_options.len() - 1)));
    }
    chrome::render_list(
        f,
        list,
        &mut state,
        inner,
        app.copy_options.len(),
        1,
        &app.theme,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = crate::app::Popup::None,
        KeyCode::Enter => app.confirm_copy(),
        KeyCode::Up => app.move_copy_selection(-1),
        KeyCode::Down => app.move_copy_selection(1),
        _ => {}
    }
}
