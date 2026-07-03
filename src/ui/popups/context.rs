use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Popup};

/// Context breakdown popup (Ctrl+I): estimated tokens spent on system
/// instructions, memory, conversation, and (pending) skills.
pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 56, 40);
    f.render_widget(Clear, area);
    let b = app.context_breakdown();
    let dim = Style::default().fg(Color::DarkGray);

    let pct_of = |tok: u64| -> String {
        match b.limit.filter(|&l| l > 0) {
            Some(l) => format!(" ({}%)", tok * 100 / l),
            None => String::new(),
        }
    };
    let row = |label: &'static str, tok: u64, color: Color| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{label:<13}"), Style::default().fg(Color::White)),
            Span::styled(crate::ui::humanize(tok), Style::default().fg(color)),
            Span::styled(pct_of(tok), dim),
        ])
    };

    let mut lines = vec![
        row("System", b.system_tokens, Color::Cyan),
        row("Memory", b.memory_tokens, Color::Magenta),
        row("Skills", b.skills_tokens, Color::Yellow),
        row("Conversation", b.conversation_tokens, Color::Green),
    ];
    if b.compacted {
        lines.push(Line::from(Span::styled(
            "  ⤷ this session has been auto-compacted — press v to view/edit the digest",
            dim,
        )));
    }
    lines.push(Line::from(""));
    let total = b.system_tokens + b.memory_tokens + b.skills_tokens + b.conversation_tokens;
    let limit_s = b.limit.map(crate::ui::humanize).unwrap_or_else(|| "?".to_string());
    lines.push(Line::from(vec![
        Span::styled("Total        ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} / {}", crate::ui::humanize(total), limit_s), Style::default().fg(Color::Yellow)),
    ]));

    let hint = if b.compacted {
        "context — v views digest, Ctrl+G toggles, Esc closes"
    } else {
        "context — Ctrl+G toggles, Esc closes"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(crate::ui::hint_title(app, " context ", hint));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.popup = Popup::None;
    }
}
