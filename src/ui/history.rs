use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use image::GenericImageView;

use super::{dim, dot, line_text};
use crate::app::App;
use crate::db::Message;

/// Wrapped render of the stored (immutable) message prefix, kept between
/// frames so scrolling a long conversation doesn't re-run markdown + textwrap
/// over every message. Invalidated when the width, display flags, or session
/// change; new messages are appended incrementally.
#[derive(Default)]
pub(crate) struct HistoryCache {
    key: (Option<String>, usize, bool, bool, bool, bool, usize),
    msg_count: usize,
    lines: Vec<Line<'static>>,
    owner: Vec<Option<usize>>,
    code: Vec<Option<usize>>,
    blocks: Vec<String>,
    plain: Vec<String>,
    /// Maps rendered line index -> image path for click-to-open.
    pub image_at_line: Vec<Option<String>>,
}

pub(super) fn render_history(f: &mut Frame, app: &mut App, area: Rect) {
    // Borderless: the conversation fills the whole pane.
    let inner = area;
    if app.is_welcome() {
        app.max_scroll = 0;
        render_welcome(f, app, inner);
        return;
    }
    let width = inner.width.max(1) as usize;
    sync_cache(app, width);

    // The in-progress reply changes every frame (spinner, tokens); render it
    // fresh and splice it after the cached prefix.
    let mut tail: Vec<Line<'static>> = Vec::new();
    let mut tail_code: Vec<Option<usize>> = Vec::new();
    let mut tail_blocks: Vec<String> = Vec::new();
    if app.viewing_stream() {
        push_assistant_streaming(&mut tail, app, width, &mut tail_code, &mut tail_blocks);
        tail_code.resize(tail.len(), None);
    }

    let cache = &app.history_cache;
    let cached_lines = cache.lines.len();
    let total = cached_lines + tail.len();

    // Scroll: app.scroll counts lines scrolled UP from the bottom (0 = follow bottom).
    let height = inner.height as usize;
    let max_top = total.saturating_sub(height);
    app.max_scroll = max_top as u16; // let the event loop clamp scrolling

    // When the user has scrolled up during streaming, new tokens grow the tail
    // which pushes max_top up.  Keep the viewport pinned by raising scroll to
    // cancel out the growth.
    if app.scroll > 0 && app.viewing_stream() {
        let delta = total.saturating_sub(app.prev_total);
        if delta > 0 {
            app.scroll = app.scroll.saturating_add(delta as u16);
        }
    }
    app.prev_total = total;

    app.scroll = app.scroll.min(app.max_scroll);
    let top = max_top.saturating_sub(app.scroll as usize);

    // Snapshot the layout + plain text + per-line message owner so mouse
    // selection can map screen cells and scope a selection to one message.
    let cache = &app.history_cache;
    let mut plain = cache.plain.clone();
    plain.extend(tail.iter().map(line_text));
    let mut owner = cache.owner.clone();
    owner.resize(total, Some(app.messages.len()));
    let base = cache.blocks.len();
    let mut code = cache.code.clone();
    code.extend(tail_code.iter().map(|c| c.map(|id| id + base)));
    let mut blocks = cache.blocks.clone();
    blocks.append(&mut tail_blocks);
    app.sel
        .record_render(inner, top, plain, owner, code, blocks);

    // Paint the selection highlight over the visible slice.
    let cache = &app.history_cache;
    let line_at = |i: usize| {
        if i < cached_lines {
            &cache.lines[i]
        } else {
            &tail[i - cached_lines]
        }
    };
    let visible: Vec<Line> = (top..total.min(top + height))
        .map(|li| {
            app.sel
                .highlight(li, line_at(li))
                .unwrap_or_else(|| line_at(li).clone())
        })
        .collect();

    f.render_widget(Paragraph::new(visible), inner);
}

/// Bring the cached wrapped prefix up to date: reset on width/flag/session
/// change, then wrap only messages not yet cached. Stored messages are
/// append-only, so this is O(new messages) per frame instead of O(all).
fn sync_cache(app: &mut App, width: usize) {
    let key = (
        app.session.as_ref().map(|s| s.id.clone()),
        width,
        app.show_tool_detail,
        app.settings.show_reasoning,
        app.settings.hide_hints,
        app.settings.show_stats,
        app.theme_gen,
    );
    let c = &mut app.history_cache;
    if c.key != key || app.messages.len() < c.msg_count {
        *c = HistoryCache {
            key,
            ..Default::default()
        };
    }
    let theme = app.theme;
    for (i, m) in app.messages.iter().enumerate().skip(c.msg_count) {
        let start = c.lines.len();
        if m.role == "user" {
            let images_dir = app.space.images_dir(&app.active_space.name);
            render_markdown_images(
                &mut c.lines, &m.content, width, &theme, &images_dir,
                &mut c.image_at_line,
            );
            push_user(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "research_stage" {
            push_research_stage(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "error" {
            push_error(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "research_plan" {
            push_research_plan(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "session_link" {
            push_session_link(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "tool_call" {
            push_tool_call(
                &mut c.lines,
                &m.content,
                app.show_tool_detail,
                &app.settings,
                width,
                &theme,
            );
        } else {
            let images_dir = app.space.images_dir(&app.active_space.name);
            render_markdown_images(
                &mut c.lines, &m.content, width, &theme, &images_dir,
                &mut c.image_at_line,
            );
            push_assistant_stored(
                &mut c.lines,
                m,
                &app.settings,
                width,
                &mut c.code,
                &mut c.blocks,
                &theme,
            );
        }
        c.owner.resize(c.lines.len(), Some(i));
        c.code.resize(c.lines.len(), None);
        c.image_at_line.resize(c.lines.len(), None);
        let new_plain: Vec<String> = c.lines[start..].iter().map(line_text).collect();
        c.plain.extend(new_plain);
    }
    c.msg_count = app.messages.len();
}

/// The empty start screen: centered banner, a random greeting, and a live clock.
fn render_welcome(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for l in app.banner.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.greeting.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(dim(
        chrono::Local::now()
            .format("%A, %B %-d %Y · %H:%M:%S")
            .to_string(),
        &app.theme,
    )));

    // Center vertically.
    let pad = (area.height as usize).saturating_sub(lines.len()) / 2;
    let mut out = vec![Line::from(""); pad];
    out.extend(lines);

    f.render_widget(Paragraph::new(out).alignment(Alignment::Center), area);
}

/// A user message: `❯ ` prefix, wrapped with a 2-col hanging indent.
fn push_user(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let head = Span::styled(
        "❯ ",
        Style::default()
            .fg(theme.user_msg)
            .add_modifier(Modifier::BOLD),
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

/// A tool call block: a dim `⚒ name summary` one-liner; when tool detail is
/// on (Ctrl+T), the full arguments and result follow, reasoning-style.
fn push_tool_call(
    out: &mut Vec<Line<'static>>,
    content: &str,
    expanded: bool,
    settings: &crate::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let v: serde_json::Value = serde_json::from_str(content).unwrap_or_default();
    let field = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let (name, args, result) = (field("name"), field("arguments"), field("result"));
    let summary = crate::app::tool_call_summary(&name, &args, &result);
    let hint = if expanded || settings.hide_hints {
        ""
    } else {
        " — Ctrl+T for detail"
    };
    out.push(Line::from(vec![
        Span::styled("⚒ ", Style::default().fg(theme.tool_msg)),
        dim(format!("{summary}{hint}"), theme),
    ]));
    if expanded {
        for line in wrap_plain(&args, width.saturating_sub(2)) {
            out.push(Line::from(dim(format!("┆ {line}"), theme)));
        }
        for line in wrap_plain(&result, width.saturating_sub(2)) {
            out.push(Line::from(dim(format!("│ {line}"), theme)));
        }
    }
    out.push(Line::from(""));
}

/// A background-research progress line: a dim one-liner with a 🔎 marker,
/// no expand/collapse (unlike tool_call — there's no arguments/result pair,
/// just a phase label).
fn push_research_stage(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let mut first = true;
    for line in wrap_plain(content, width.saturating_sub(2)) {
        if first {
            out.push(Line::from(vec![
                Span::styled("🔎 ", Style::default().fg(theme.research_msg)),
                dim(line, theme),
            ]));
            first = false;
        } else {
            out.push(Line::from(dim(format!("  {line}"), theme)));
        }
    }
    out.push(Line::from(""));
}

/// A persistent request failure, kept in the transcript after the status bar
/// changes. Use the theme's error color and a hanging indent for long errors.
fn push_error(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let style = Style::default().fg(theme.error);
    let mut first = true;
    for line in wrap_plain(content, width.saturating_sub(2)) {
        if first {
            out.push(Line::from(vec![
                Span::styled("! ", style.add_modifier(Modifier::BOLD)),
                Span::styled(line, style),
            ]));
            first = false;
        } else {
            out.push(Line::from(Span::styled(format!("  {line}"), style)));
        }
    }
    if first {
        out.push(Line::from(Span::styled("! request failed", style)));
    }
    out.push(Line::from(""));
}

/// A pending plan-approval message: like `push_research_stage` but with a
/// distinct marker and full (non-dim) styling, since it's actionable —
/// [e]dit / Enter to continue — not passive progress.
fn push_research_plan(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let mut first = true;
    for line in wrap_plain(content, width.saturating_sub(2)) {
        if first {
            out.push(Line::from(vec![
                Span::styled("📋 ", Style::default().fg(theme.accent)),
                Span::styled(line, Style::default().fg(theme.accent)),
            ]));
            first = false;
        } else {
            out.push(Line::from(format!("  {line}")));
        }
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
    theme: &crate::theme::Theme,
) {
    // A `/swarm` persona's round reply gets a small header identifying it.
    if let Some(persona) = &msg.persona {
        let model = msg.model.as_deref().unwrap_or("");
        out.push(Line::from(vec![
            Span::styled(
                format!("**{persona}**"),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            dim(format!(" · {model}"), theme),
        ]));
    }
    // Completion line: "⏺ Vibed for 13.3 seconds" (dot green, text dim).
    match (&msg.phrase, msg.secs) {
        (Some(p), Some(secs)) => out.push(Line::from(vec![
            Span::styled(
                "⏺ ",
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            dim(format!("{p} for {secs:.1} seconds"), theme),
        ])),
        _ => out.push(dot(theme)),
    }

    if let Some(r) = &msg.reasoning {
        if settings.show_reasoning {
            out.push(Line::from(dim("▾ reasoning", theme)));
            for line in wrap_plain(r, width.saturating_sub(2)) {
                out.push(Line::from(dim(format!("┆ {line}"), theme)));
            }
        } else {
            let n = r.chars().count();
            let hint = if settings.hide_hints {
                ""
            } else {
                " — Ctrl+R to expand"
            };
            out.push(Line::from(dim(
                format!("▸ reasoning ({n} chars){hint}"),
                theme,
            )));
        }
    }

    let mut rendered = crate::markdown::render(&msg.content, width);
    rendered.lines = crate::citations::style_citations(rendered.lines, theme.accent);
    rendered.lines = crate::citations::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered);

    // Footer below the response.
    if let Some(m) = &msg.model {
        let mut footer = format!("· {m}");
        if settings.show_stats
            && let (Some(tok), Some(secs)) = (msg.tokens, msg.secs)
        {
            let tps = if secs > 0.0 { tok as f64 / secs } else { 0.0 };
            footer.push_str(&format!("  ·  {tps:.1} tok/s · ~{tok} tok · {secs:.2}s"));
        }
        out.push(Line::from(dim(footer, theme)));
    }
    out.push(Line::from(""));
}

/// A session switch link: renders as a styled box with arrows and the
/// linked session's name. Content format: `<target_sid>\n<label>`.
fn push_session_link(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let (sid, label) = match content.split_once('\n') {
        Some((sid, rest)) => (sid.to_string(), rest.trim().to_string()),
        None => (String::new(), content.to_string()),
    };
    let arrow = if label.starts_with("🔗") { "→" } else { "↩" };
    let color = theme.accent;
    let dim = Style::default().fg(theme.fg_dim);

    let w = width.min(60);
    let inner = w.saturating_sub(4);
    out.push(Line::from(Span::styled(
        format!("┌{}┐", "─".repeat(inner)),
        dim,
    )));
    out.push(Line::from(vec![
        Span::styled("│ ", dim),
        Span::styled(label.clone(), Style::default().fg(color)),
        Span::raw(" ".repeat(inner.saturating_sub(label.chars().count()))),
        Span::styled(" │", dim),
    ]));
    if !sid.is_empty() {
        let hint = format!("   {arrow} select text + Ctrl+O to switch");
        out.push(Line::from(vec![
            Span::styled("│ ", dim),
            Span::styled(hint.clone(), dim),
            Span::raw(" ".repeat(inner.saturating_sub(hint.chars().count().min(inner)))),
            Span::styled(" │", dim),
        ]));
    }
    out.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(inner)),
        dim,
    )));
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
        Span::styled(
            app.thinking_phrase().to_string(),
            Style::default().fg(color),
        ),
    ]));

    if let Some(t) = app.thinking_text() {
        for line in wrap_plain(t, width.saturating_sub(2)) {
            out.push(Line::from(dim(format!("┆ {line}"), &app.theme)));
        }
    }

    let buf = app.streaming.as_deref().unwrap_or("");
    let mut rendered = crate::markdown::render(buf, width);
    rendered.lines = crate::citations::style_citations(rendered.lines, app.theme.accent);
    rendered.lines = crate::citations::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered);
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

/// Scan content for markdown image references `![alt](file)` and render them
/// inline. For each match, resolve the file against `images_dir`, render it
/// with `image_to_halfblock_lines`, and track the lines in `image_at_line`.
fn render_markdown_images(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
    images_dir: &std::path::Path,
    image_at_line: &mut Vec<Option<String>>,
) {
    let mut rest = content;
    while let Some(start) = rest.find("![") {
        if let Some(end) = rest[start..].find(')') {
            let inner = &rest[start + 2..start + end];
            if let Some((_alt, file)) = inner.split_once("](") {
                let path = images_dir.join(file);
                let path_str = path.to_string_lossy().to_string();
                let half = image_to_halfblock_lines(&path_str, width);
                if half.len() <= 1 {
                    out.push(Line::from(dim(
                        format!("🖼 {_alt}"),
                        theme,
                    )));
                    image_at_line.push(Some(path_str));
                } else {
                    let img_start = out.len();
                    out.extend(half);
                    let img_end = out.len();
                    out.push(Line::from(""));
                    for _ in img_start..img_end {
                        image_at_line.push(Some(path_str.clone()));
                    }
                    image_at_line.push(None);
                }
            }
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
}

/// Max cell-rows a rendered image occupies (click-to-open encourages viewing
/// full size in an external viewer instead of eating the whole terminal).
const MAX_IMAGE_ROWS: usize = 20;

/// Render a PNG image as half-block ratatui lines for inline display in the
/// terminal. Falls back to a text marker if the image can't be loaded.
/// When the image is taller than `MAX_IMAGE_ROWS`, the last line says "🖼 image"
/// so the user knows to click to open the full version.
fn image_to_halfblock_lines(path: &str, max_width: usize) -> Vec<Line<'static>> {
    let img = match image::open(path) {
        Ok(img) => img,
        Err(_) => {
            return vec![Line::from(Span::raw("🖼 [image]"))];
        }
    };
    if max_width < 4 {
        return vec![Line::from(Span::raw("🖼"))];
    }
    let mut cell_w = max_width.min(img.width() as usize);
    let aspect = img.width() as f64 / img.height() as f64;
    let mut cell_h = (cell_w as f64 / aspect).round().max(1.0) as usize;
    let truncated = cell_h > MAX_IMAGE_ROWS;
    if truncated {
        cell_h = MAX_IMAGE_ROWS;
        // Recalculate width from capped height to maintain aspect
        let capped_w = (cell_h as f64 * aspect).round().max(1.0) as usize;
        cell_w = cell_w.min(capped_w);
    }
    let pixel_w = cell_w;
    let pixel_h = (cell_h * 2).max(2);
    let resized = img.resize_exact(pixel_w as u32, pixel_h as u32, image::imageops::FilterType::Lanczos3);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for y in (0..pixel_h).step_by(2) {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(pixel_w);
        for x in 0..pixel_w {
            let top = resized.get_pixel(x as u32, y as u32);
            let bottom = if y + 1 < pixel_h {
                resized.get_pixel(x as u32, (y + 1) as u32)
            } else {
                image::Rgba([0, 0, 0, 0])
            };
            let fg = ratatui::style::Color::Rgb(top[0], top[1], top[2]);
            let bg = ratatui::style::Color::Rgb(bottom[0], bottom[1], bottom[2]);
            spans.push(Span::styled("▀", Style::default().fg(fg).bg(bg)));
        }
        lines.push(Line::from(spans));
    }
    if truncated {
        lines.push(Line::from(Span::styled(
            "🖼 click to open in viewer",
            ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
        )));
    }
    lines
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, Message};

    fn msg(role: &str, content: &str) -> Message {
        Message {
            id: String::new(),
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: None,
            persona: None,
        }
    }

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let space = crate::space::Space {
            root: std::env::temp_dir().join(format!("nexus-hist-{}", uuid::Uuid::new_v4())),
        };
        App::new(db, Some("k".into()), space)
    }

    #[test]
    fn cache_appends_new_messages_and_resets_on_width_or_flag_change() {
        let mut a = test_app();
        a.messages.push(msg("user", "hello"));
        a.messages.push(msg("assistant", "**bold** answer"));
        sync_cache(&mut a, 80);
        let n = a.history_cache.lines.len();
        assert!(n > 0);
        assert_eq!(a.history_cache.msg_count, 2);
        assert_eq!(a.history_cache.plain.len(), n);

        // Same width: appending one message only grows the cache.
        a.messages.push(msg("user", "again"));
        sync_cache(&mut a, 80);
        assert!(a.history_cache.lines.len() > n);
        assert_eq!(a.history_cache.msg_count, 3);

        // Width change: full re-wrap.
        sync_cache(&mut a, 20);
        assert_eq!(a.history_cache.msg_count, 3);
        assert_eq!(a.history_cache.key.1, 20);

        // Ctrl+T flag change: re-wrap so tool-call detail shows.
        a.show_tool_detail = true;
        let before = a.history_cache.lines.len();
        a.messages.push(msg(
            "tool_call",
            r#"{"name":"skill","arguments":"{}","result":"ok"}"#,
        ));
        sync_cache(&mut a, 20);
        assert!(a.history_cache.lines.len() > before);

        // Fewer messages (session cleared): cache resets, no stale lines.
        a.messages.clear();
        sync_cache(&mut a, 20);
        assert_eq!(a.history_cache.lines.len(), 0);
        assert_eq!(a.history_cache.msg_count, 0);
    }
}
