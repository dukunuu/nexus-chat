//! `/login`'s provider selector: `OpenRouter`, `OpenCode` Go, `OpenAI`, or Codex.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

const ROWS: [(&str, &str); 4] = [
    ("OpenRouter", "paste a key, or reads $OPENROUTER_API_KEY"),
    ("OpenCode Go", "paste a key, or reads $OPENCODE_API_KEY"),
    ("OpenAI", "paste a key, or reads $OPENAI_API_KEY"),
    ("Codex", "ChatGPT subscription — device-code login"),
];

pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), chrome::SMALL.0, chrome::SMALL.1);
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "🔑", "login"),
        "↑↓ · Enter pick · Esc close",
        app,
        true,
        chrome::Tone::Normal,
    );

    let items: Vec<ListItem> = ROWS
        .iter()
        .map(|(name, hint)| {
            ListItem::new(Line::from(vec![
                ratatui::text::Span::styled(
                    format!("{name:<12}"),
                    Style::default().fg(app.theme.fg),
                ),
                ratatui::text::Span::styled(
                    format!(" {hint}"),
                    Style::default().fg(app.theme.fg_dim),
                ),
            ]))
        })
        .collect();

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(Some(app.login_selected));
    f.render_stateful_widget(list, inner, &mut state);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = crate::app::Popup::None,
        KeyCode::Up => app.move_login_selection(-1),
        KeyCode::Down => app.move_login_selection(1),
        KeyCode::Enter => app.confirm_login_selection(),
        _ => {}
    }
}
