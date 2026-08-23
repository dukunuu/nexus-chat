use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;
use nexus_core::app::Popup;

use super::chrome;

/// Context breakdown popup (Ctrl+I): estimated tokens spent on system
/// instructions, memory, conversation, and (pending) skills.
pub fn render(f: &mut Frame, app: &AppView) {
    let area = crate::ui::centered(f.area(), chrome::SMALL.0, chrome::SMALL.1);
    let b = app.context_breakdown();
    let dim = Style::default().fg(app.theme.fg_dim);

    let pct_of = |tok: u64| -> String {
        match b.limit.filter(|&l| l > 0) {
            Some(l) => format!(" ({}%)", tok * 100 / l),
            None => String::new(),
        }
    };
    let row = |label: &'static str, tok: u64, color: Color| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{label:<13}"), Style::default().fg(app.theme.fg)),
            Span::styled(crate::ui::humanize(tok), Style::default().fg(color)),
            Span::styled(pct_of(tok), dim),
        ])
    };

    let mut lines = vec![
        row("System", b.system_tokens, app.theme.accent),
        row("Memory", b.memory_tokens, app.theme.accent2),
        row("Skills", b.skills_tokens, app.theme.warning),
        row("Conversation", b.conversation_tokens, app.theme.success),
    ];
    if b.compacted {
        lines.push(Line::from(Span::styled(
            "  ⤷ this session has been auto-compacted — press v to view/edit the digest",
            dim,
        )));
    }
    lines.push(Line::from(""));
    let total = b.system_tokens + b.memory_tokens + b.skills_tokens + b.conversation_tokens;
    let limit_s = b.limit.map_or_else(|| "?".to_string(), crate::ui::humanize);
    lines.push(Line::from(vec![
        Span::styled(
            "Total        ",
            Style::default()
                .fg(app.theme.fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} / {}", crate::ui::humanize(total), limit_s),
            Style::default().fg(app.theme.warning),
        ),
    ]));

    // Cache detail lives here, where the two figures can be labelled apart:
    // the status line shows the turn, this shows the request behind it.
    let turn = app.turn_cache;
    if turn.rate().is_some() || app.last_cache_rate.is_some() {
        lines.push(Line::from(""));
        let pct = |rate: Option<f64>| {
            rate.map_or_else(|| "—".to_string(), |r| format!("{:.0}%", r * 100.0))
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Cache        ",
                Style::default()
                    .fg(app.theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} this turn", pct(turn.rate())),
                Style::default().fg(app.theme.success),
            ),
            Span::styled(format!(" · {} last request", pct(app.last_cache_rate)), dim),
        ]));
        if turn.is_partial() {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ⤷ {} request(s) this turn reported no cache accounting — excluded",
                    turn.unrated_requests
                ),
                dim,
            )));
        }
    }

    let hint = if b.compacted {
        "v digest · Esc close"
    } else {
        "Ctrl+G toggle · Esc close"
    };
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "📊", "context"),
        hint,
        app,
        true,
        chrome::Tone::Normal,
    );
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.popup = Popup::None;
    }
}
