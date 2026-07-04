use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{dim, dot, line_text};
use crate::app::App;
use crate::db::Message;

pub(super) fn render_history(f: &mut Frame, app: &mut App, area: Rect) {
    // Borderless: the conversation fills the whole pane.
    let inner = area;
    if app.is_welcome() {
        app.max_scroll = 0;
        render_welcome(f, app, inner);
        return;
    }
    let width = inner.width.max(1) as usize;
    let (lines, line_msg, line_code, code_blocks) = wrap_conversation(app, width);

    // Scroll: app.scroll counts lines scrolled UP from the bottom (0 = follow bottom).
    let height = inner.height as usize;
    let max_top = lines.len().saturating_sub(height);
    app.max_scroll = max_top as u16; // let the event loop clamp scrolling
    app.scroll = app.scroll.min(app.max_scroll);
    let top = max_top.saturating_sub(app.scroll as usize);

    // Snapshot the layout + plain text + per-line message owner so mouse
    // selection can map screen cells and scope a selection to one message.
    let plain: Vec<String> = lines.iter().map(line_text).collect();
    app.sel.record_render(inner, top, plain, line_msg, line_code, code_blocks);

    // Paint the selection highlight over the visible slice.
    let visible: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(li, line)| app.sel.highlight(li, line).unwrap_or_else(|| line.clone()))
        .collect();

    f.render_widget(Paragraph::new(visible), inner);
}

/// The empty start screen: centered banner, a random greeting, and a live clock.
fn render_welcome(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for l in app.banner.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.greeting.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(dim(
        chrono::Local::now().format("%A, %B %-d %Y · %H:%M:%S").to_string(),
    )));

    // Center vertically.
    let pad = (area.height as usize).saturating_sub(lines.len()) / 2;
    let mut out = vec![Line::from(""); pad];
    out.extend(lines);

    f.render_widget(Paragraph::new(out).alignment(Alignment::Center), area);
}

/// Wrap every message (and the in-progress stream) to `width`. Prefixes are
/// glyphs, not words: `❯` for you, `⏺` for the AI. Returns the wrapped lines and
/// a parallel vec of the message index each line belongs to (for message-scoped
/// selection); the streaming reply gets index `messages.len()`.
type Wrapped = (Vec<Line<'static>>, Vec<Option<usize>>, Vec<Option<usize>>, Vec<String>);

fn wrap_conversation(app: &App, width: usize) -> Wrapped {
    let mut out: Vec<Line> = Vec::new();
    let mut owner: Vec<Option<usize>> = Vec::new();
    let mut code: Vec<Option<usize>> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    for (i, m) in app.messages.iter().enumerate() {
        if m.role == "user" {
            if !m.images.is_empty() {
                out.push(Line::from(dim(format!("🖼 {} image{}", m.images.len(), if m.images.len() == 1 { "" } else { "s" }))));
            }
            push_user(&mut out, &m.content, width);
        } else {
            push_assistant_stored(&mut out, m, &app.settings, width, &mut code, &mut blocks);
        }
        owner.resize(out.len(), Some(i));
        code.resize(out.len(), None);
    }
    if app.streaming.is_some() {
        push_assistant_streaming(&mut out, app, width, &mut code, &mut blocks);
        owner.resize(out.len(), Some(app.messages.len()));
        code.resize(out.len(), None);
    }
    (out, owner, code, blocks)
}

/// A user message: `❯ ` prefix, wrapped with a 2-col hanging indent.
fn push_user(out: &mut Vec<Line<'static>>, content: &str, width: usize) {
    let head = Span::styled(
        "❯ ",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    let mut first = true;
    for line in wrap_plain(content, width.saturating_sub(2)) {
        if first {
            out.push(Line::from(vec![head.clone(), Span::raw(line)]));
            first = false;
        } else {
            out.push(Line::from(format!("  {line}")));
        }
    }
    if first {
        out.push(Line::from(head));
    }
    out.push(Line::from(""));
}

/// A stored assistant reply: dot, collapsible reasoning, the answer, then a
/// dim `· model · stats` footer (stats only when show_stats is on).
fn push_assistant_stored(
    out: &mut Vec<Line<'static>>,
    msg: &Message,
    settings: &crate::app::Settings,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
) {
    // Completion line: "⏺ Vibed for 13.3 seconds" (dot green, text dim).
    match (&msg.phrase, msg.secs) {
        (Some(p), Some(secs)) => out.push(Line::from(vec![
            Span::styled(
                "⏺ ",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            dim(format!("{p} for {secs:.1} seconds")),
        ])),
        _ => out.push(dot()),
    }

    if let Some(r) = &msg.reasoning {
        if settings.show_reasoning {
            out.push(Line::from(dim("▾ reasoning")));
            for line in wrap_plain(r, width.saturating_sub(2)) {
                out.push(Line::from(dim(format!("┆ {line}"))));
            }
        } else {
            let n = r.chars().count();
            let hint = if settings.hide_hints { "" } else { " — Ctrl+R to expand" };
            out.push(Line::from(dim(format!("▸ reasoning ({n} chars){hint}"))));
        }
    }

    push_rendered(out, code, blocks, crate::markdown::render(&msg.content, width));

    // Footer below the response.
    if let Some(m) = &msg.model {
        let mut footer = format!("· {m}");
        if settings.show_stats
            && let (Some(tok), Some(secs)) = (msg.tokens, msg.secs) {
                let tps = if secs > 0.0 { tok as f64 / secs } else { 0.0 };
                footer.push_str(&format!("  ·  {tps:.1} tok/s · ~{tok} tok · {secs:.2}s"));
            }
        out.push(Line::from(dim(footer)));
    }
    out.push(Line::from(""));
}

/// The in-progress reply: "spinner Phrase" on one line in a random colour, live
/// reasoning below, then the answer as it streams.
fn push_assistant_streaming(
    out: &mut Vec<Line<'static>>,
    app: &App,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
) {
    let color = app.spinner_color();
    out.push(Line::from(vec![
        Span::styled(
            format!("{} ", app.spinner_char()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.thinking_phrase().to_string(), Style::default().fg(color)),
    ]));

    if let Some(t) = app.thinking_text() {
        for line in wrap_plain(t, width.saturating_sub(2)) {
            out.push(Line::from(dim(format!("┆ {line}"))));
        }
    }

    let buf = app.streaming.as_deref().unwrap_or("");
    push_rendered(out, code, blocks, crate::markdown::render(buf, width));
    out.push(Line::from(""));
}

/// Splice a `markdown::Rendered` into the running line/code/block vecs, keeping
/// `code` aligned to `out` and offsetting local block ids to global ones.
fn push_rendered(
    out: &mut Vec<Line<'static>>,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
    r: crate::markdown::Rendered,
) {
    code.resize(out.len(), None); // align past any dot/reasoning lines
    let base = blocks.len();
    code.extend(r.code.iter().map(|c| c.map(|id| id + base)));
    blocks.extend(r.blocks);
    out.extend(r.lines);
}

/// Wrap text to `width`, preserving explicit newlines.
fn wrap_plain(content: &str, width: usize) -> Vec<String> {
    let w = width.max(1);
    let mut out = Vec::new();
    for raw in content.split('\n') {
        for piece in textwrap::wrap(raw, w) {
            out.push(piece.into_owned());
        }
    }
    out
}
