//! `/help` (or F1): the keybinding reference plus the slash-command catalog.
//! Commands render straight from `COMMANDS`, so the list can't drift from
//! what the composer actually accepts.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;
use crate::theme::Theme;
use nexus_core::app::{COMMANDS, Popup};

use super::chrome;

/// Keybindings by area: (section, [(keys, what they do)]).
pub const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "composer",
        &[
            ("Enter", "send"),
            ("Shift/Ctrl+Enter", "new line"),
            ("Esc", "stop the reply, or clear the composer"),
            ("/  @", "command menu · reference a space file"),
            ("Ctrl+A · Ctrl+X", "select all · cut"),
            ("Ctrl+Shift+C · Ctrl+V", "copy selection · paste"),
            ("Ctrl+Backspace", "delete the previous word"),
            ("Ctrl+C", "back out one step; twice to quit"),
        ],
    ),
    (
        "conversation",
        &[
            ("↑/↓", "scroll (at the composer's edge)"),
            ("PgUp/PgDn", "scroll a page"),
            ("Ctrl+Home/End", "jump to the start / latest"),
            ("mouse wheel", "scroll"),
            ("drag · double/triple click", "select (copies on release)"),
            ("long press", "copy the message, code block, or link"),
            ("Ctrl+R · Ctrl+T", "expand reasoning · expand tool calls"),
            ("Ctrl+O", "open the selected session link"),
        ],
    ),
    (
        "sessions",
        &[
            ("Ctrl+N", "new session"),
            ("Ctrl+Shift+N", "new incognito session"),
            ("Ctrl+G", "context breakdown"),
            ("Ctrl+↑", "live research view (while research runs)"),
        ],
    ),
    (
        "popups",
        &[
            ("↑/↓ · Enter", "move · pick"),
            ("wheel · click", "move · select (click again to pick)"),
            ("type", "filter the list"),
            ("Esc", "close"),
        ],
    ),
];

/// The full help text, one `Line` per row.
pub fn help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let heading = |s: &str| {
        Line::from(Span::styled(
            format!("▍{s}"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let row = |left: String, right: &str, left_w: usize| {
        Line::from(vec![
            Span::styled(
                format!("  {left:<left_w$}  "),
                Style::default().fg(theme.fg),
            ),
            Span::styled(right.to_string(), Style::default().fg(theme.fg_dim)),
        ])
    };
    let key_w = KEYS
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(k, _)| k.chars().count()))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (section, rows) in KEYS {
        lines.push(heading(section));
        for (keys, what) in *rows {
            lines.push(row((*keys).to_string(), what, key_w));
        }
        lines.push(Line::from(""));
    }
    lines.push(heading("commands"));
    let cmd_w = COMMANDS
        .iter()
        .map(|c| c.name.chars().count() + 1)
        .max()
        .unwrap_or(0);
    for c in COMMANDS {
        lines.push(row(format!("/{}", c.name), c.desc, cmd_w));
    }
    lines
}

pub fn render(f: &mut Frame, app: &mut AppView) {
    // Wide: the key column alone is ~28 cells.
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::TALL.1);
    let title = chrome::popup_title(app, "?", "help");
    let inner = chrome::render_hinted(
        f,
        area,
        title,
        "↑↓/PgUp/PgDn scroll · Esc close",
        app,
        true,
        chrome::Tone::Normal,
    );
    let lines = help_lines(&app.theme);
    // Clamp here, where the viewport height is known, so scrolling past the
    // end never leaves a blank pane.
    let max = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(inner.height);
    app.help_scroll = app.help_scroll.min(max);
    f.render_widget(
        Paragraph::new(chrome::fit_lines(lines, inner.width)).scroll((app.help_scroll, 0)),
        inner,
    );
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::F(1) | KeyCode::Char('q') => {
            app.popup = Popup::None;
        }
        KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.help_scroll = app.help_scroll.saturating_add(1),
        KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(10),
        KeyCode::PageDown => app.help_scroll = app.help_scroll.saturating_add(10),
        KeyCode::Home => app.help_scroll = 0,
        KeyCode::End => app.help_scroll = u16::MAX,
        _ => {}
    }
}
