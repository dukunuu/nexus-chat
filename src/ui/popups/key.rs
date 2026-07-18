use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::widgets::Paragraph;

use crate::app::{App, Popup};

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let outer = crate::ui::centered(f.area(), 60, 20);
    let masked = "*".repeat(app.key_input.chars().count());
    let title = chrome::input_title(
        app,
        format!("{} key", app.key_target_label()),
        masked.as_str(),
        "Enter to save, Esc to go back",
    );
    let inner = chrome::render_frame(f, outer, title, &app.theme, true);
    f.render_widget(Paragraph::new(""), inner);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = Popup::Login,
        KeyCode::Enter => app.confirm_key(),
        KeyCode::Char(c) => app.key_input.push(c),
        KeyCode::Backspace => {
            app.key_input.pop();
        }
        _ => {}
    }
}
