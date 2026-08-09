//! `/login`'s provider selector: `OpenRouter`, `OpenCode` Go, `OpenAI`, or Codex.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
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
        "↑↓ · PgUp/Dn · Enter pick · Esc close",
        app,
        true,
        chrome::Tone::Normal,
    );

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = ROWS
        .iter()
        .enumerate()
        .map(|(i, (name, hint))| {
            // State chip: which backends already have a key configured.
            let configured = match i {
                0 => app.backends.openrouter.is_some(),
                1 => app.backends.opencode.is_some(),
                2 => app.backends.openai.is_some(),
                _ => app.backends.codex.is_some(),
            };
            let chip = if configured {
                Span::styled("✓ configured", Style::default().fg(app.theme.success))
            } else {
                Span::styled("○ no key", dim)
            };
            let width = area.width.saturating_sub(6) as usize;
            let pad = width.saturating_sub(name.chars().count() + chip.content.chars().count() + 2);
            let top = Line::from(vec![
                ratatui::text::Span::styled(name.to_string(), Style::default().fg(app.theme.fg)),
                ratatui::text::Span::raw(" ".repeat(pad)),
                chip,
            ]);
            ListItem::new(vec![
                top,
                Line::from(ratatui::text::Span::styled(format!("  {hint}"), dim)),
                Line::from(""),
            ])
        })
        .collect();

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(Some(app.login_selected));
    chrome::render_list(f, list, &mut state, inner, ROWS.len(), 3, &app.theme);
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
