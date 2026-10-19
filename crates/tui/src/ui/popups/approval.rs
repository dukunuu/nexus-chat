use crossterm::event::{KeyCode, KeyEvent};
use nexus_core::app::AppCommand;
use nexus_core::tools::Decision;
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthChar as _;

use crate::app_view::AppView;

use super::chrome;

fn escaped_rows(text: &str, width: usize, normal: Style, dim: Style) -> Vec<Line<'static>> {
    let mut rows = vec![Line::default()];
    let mut used = 0;
    for c in text.chars() {
        let (text, style) = if c == '\t' {
            ("    ".to_string(), normal)
        } else if c.is_control() {
            (c.escape_unicode().to_string(), dim)
        } else {
            (c.to_string(), normal)
        };
        for c in text.chars() {
            let cells = c.width().unwrap_or(0);
            if used + cells > width.max(1) && used > 0 {
                rows.push(Line::default());
                used = 0;
            }
            let row = rows.last_mut().expect("approval has a row");
            match row.spans.last_mut() {
                Some(span) if span.style == style => span.content.to_mut().push(c),
                _ => row.spans.push(Span::styled(c.to_string(), style)),
            }
            used += cells;
        }
    }
    rows
}

fn shell_unread(app: &AppView) -> bool {
    app.approval_request
        .as_ref()
        .is_some_and(|request| request.tool == "shell")
        && app.approval_max_scroll.get() > 0
        && !app.approval_seen_end.get()
}

pub fn render(f: &mut Frame, app: &AppView) {
    let Some(request) = &app.approval_request else {
        return;
    };
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::WIDE.1);
    let inner = chrome::popup_block_focused("", &app.theme, true, chrome::Tone::Normal).inner(area);
    let width = usize::from(inner.width);
    let height = usize::from(inner.height);
    let dim = Style::default().fg(app.theme.fg_dim);
    let mut lines = escaped_rows(
        &format!("{} · {}", request.tool, request.action),
        width,
        Style::default().fg(app.theme.accent),
        dim,
    );
    lines.push(Line::default());
    for line in request.summary.split('\n') {
        lines.extend(escaped_rows(
            line,
            width,
            Style::default().fg(app.theme.fg),
            dim,
        ));
    }
    let max_scroll = lines.len().saturating_sub(height);
    let offset = app.approval_scroll.get().min(max_scroll);
    app.approval_scroll.set(offset);
    app.approval_max_scroll.set(max_scroll);
    app.approval_page_height
        .set(height.saturating_sub(1).max(1));
    if offset == max_scroll && height > 0 && width > 0 {
        app.approval_seen_end.set(true);
    }
    let queued = app.approval_count();
    let title = if queued > 1 {
        format!(" approve{}1 of {queued} ", crate::ui::style::SEP)
    } else {
        " approve ".to_string()
    };
    let hint = if shell_unread(app) {
        "↑↓ scroll · scroll to the end to allow · n deny"
    } else if max_scroll > 0 {
        "↑↓ scroll · y allow · a allow for this session · n deny"
    } else {
        "y allow · a allow for this session · n deny"
    };
    chrome::render_hinted(
        f,
        area,
        Line::from(Span::styled(title, Style::default().fg(app.theme.accent))),
        hint,
        app,
        true,
        chrome::Tone::Normal,
    );
    let visible = if offset < max_scroll {
        height.saturating_sub(1)
    } else {
        height
    };
    let end = offset.saturating_add(visible).min(lines.len());
    let more = lines.len() - end;
    let mut visible: Vec<_> = lines.into_iter().skip(offset).take(visible).collect();
    if more > 0 && height > 0 {
        visible.push(Line::from(Span::styled(
            format!("… {more} more lines"),
            dim,
        )));
    }
    f.render_widget(Paragraph::new(visible), inner);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> anyhow::Result<()> {
    let page = app.approval_page_height.get();
    let (up, amount) = match key.code {
        KeyCode::Up | KeyCode::Char('k') if !super::is_chord(key) => (true, 1),
        KeyCode::Down | KeyCode::Char('j') if !super::is_chord(key) => (false, 1),
        KeyCode::PageUp => (true, page),
        KeyCode::PageDown => (false, page),
        _ => (false, 0),
    };
    if amount > 0 {
        let offset = app.approval_scroll.get();
        app.approval_scroll.set(if up {
            offset.saturating_sub(amount)
        } else {
            offset
                .saturating_add(amount)
                .min(app.approval_max_scroll.get())
        });
        return Ok(());
    }
    let decision = match key.code {
        KeyCode::Esc => Decision::Deny,
        KeyCode::Char('y') if !super::is_chord(key) => Decision::Allow,
        KeyCode::Char('a') if !super::is_chord(key) => Decision::AllowForSession,
        KeyCode::Char('n') if !super::is_chord(key) => Decision::Deny,
        _ => return Ok(()),
    };
    if decision != Decision::Deny
        && (shell_unread(app)
            || app
                .approval_opened_at
                .is_some_and(|opened| opened.elapsed() < app.approval_delay))
    {
        return Ok(());
    }
    app.execute(AppCommand::Approve { decision })
}
