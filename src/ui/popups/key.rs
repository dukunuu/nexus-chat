use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Popup};

pub(crate) fn render(f: &mut Frame, app: &App) {
    let outer = crate::ui::centered(f.area(), 60, 20);
    f.render_widget(Clear, outer);
    // Mask the key so it isn't shown in the clear.
    let masked = "*".repeat(app.key_input.chars().count());
    let para = Paragraph::new(format!("{masked}▏")).block(
        Block::default()
            .borders(Borders::ALL)
            .title(crate::ui::hint_title(app, " OpenRouter key ", "OpenRouter key — Enter to save, Esc to cancel")),
    );
    f.render_widget(para, outer);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Enter => app.confirm_key(),
        KeyCode::Char(c) => app.key_input.push(c),
        KeyCode::Backspace => {
            app.key_input.pop();
        }
        _ => {}
    }
}
