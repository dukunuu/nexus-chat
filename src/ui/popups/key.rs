use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Popup};

use super::chrome;

/// API-key entry: a centered masked value under the input title, with the
/// paste hint in the frame footer — same chrome family as every other popup.
pub fn render(f: &mut Frame, app: &App) {
    let outer = crate::ui::centered(f.area(), chrome::SMALL.0, chrome::SMALL.1);
    let masked = "*".repeat(app.key_input.chars().count());
    let title = chrome::input_title(
        app,
        format!("🔑 {} key", app.key_target_label()),
        masked.as_str(),
        "",
    );
    let inner = chrome::render_hinted(
        f,
        outer,
        title,
        "paste or type · Enter save · Esc back",
        app,
        true,
        chrome::Tone::Normal,
    );

    let shown = if masked.is_empty() {
        "• • •".to_string()
    } else {
        masked
    };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            shown,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            if app.settings.hide_hints {
                String::new()
            } else {
                "keys never leave this machine — stored in your config".to_string()
            },
            Style::default().fg(app.theme.fg_dim),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
        inner,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
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
