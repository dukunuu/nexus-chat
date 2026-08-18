// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use image::GenericImageView;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use std::collections::HashMap;
use std::fmt::Write as _;

use super::{dim, fmt_cost, to_color};
use crate::app_view::AppView;
use crate::history_cache::HistoryCache;
use crate::ui::markdown::line_text;
use nexus_core::db::Message;

#[allow(clippy::too_many_lines)] // whole conversation view: header, day dividers, cards, stats
pub(super) fn render_history(f: &mut Frame, app: &mut AppView, area: Rect) {
    // Borderless: the conversation fills the whole pane minus the rightmost
    // column, which is the scrollbar gutter (always reserved so lines don't
    // shift when the scrollbar appears).
    let inner = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(1).max(1),
        height: area.height,
    };
    if app.is_welcome() {
        app.max_scroll = 0;
        // The welcome screen renders no history lines — record an empty
        // layout so a click here can never map onto the previous session's
        // stale snapshot and open one of its URLs (or images) instead of
        // doing nothing.
        app.sel.record_render(
            Rect::default(),
            0,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        render_welcome(f, app, area);
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
    app.max_scroll = max_top; // let the event loop clamp scrolling

    // The rendered line count can change under the viewport: the streaming
    // tail grows, or a display-flag toggle (Ctrl+R reasoning, Ctrl+T tool
    // detail) re-wraps the whole cache. Keep the viewport where the user is
    // reading instead of letting it jump.
    //
    // A raw line-count delta only pins correctly when every new line landed
    // BELOW the viewport. Growth above it (reasoning streaming while the
    // user reads the answer below it, a tool-call message landing in the
    // cache, the tail re-wrapping) shifts the content without shifting the
    // line index, so delta compensation scrolls the wrong text under the
    // viewport. Instead, re-find the exact line the viewport was pinned to:
    // the tail re-renders every frame, but a surviving line's text is a
    // prefix of the line at its new position (only the frontier lines grow),
    // so the previous frame's viewport-top line can be matched by prefix in
    // the newly-rendered region and the viewport pinned back onto it.
    let prev_tail = std::mem::take(&mut app.prev_tail);
    let prev_top = app
        .prev_total
        .saturating_sub(height)
        .saturating_sub(app.scroll);
    let anchor: Option<&str> = if app.prev_total > 0 && !prev_tail.is_empty() {
        let prev_cached = app.prev_total.saturating_sub(prev_tail.len());
        prev_top
            .checked_sub(prev_cached)
            .and_then(|offset| prev_tail.get(offset).map(String::as_str))
    } else {
        None
    };

    let explicit_toggle = app.pin_viewport_top;
    app.pin_viewport_top = false;
    // Pin only when the user has actually scrolled up, or just toggled a
    // display flag. While following the bottom (scroll == 0) the viewport
    // tracks the newest lines — including across the tail→cache transition
    // when a reply finishes, where delta-compensating would leave the
    // viewport short of the end of the content (the stored card renders a
    // couple of lines taller than the streaming tail, so the reply's last
    // lines would end up hidden below the pane).
    let pin_top = app.prev_total > 0 && (explicit_toggle || app.scroll > 0);
    // A display-flag toggle while following the bottom keeps following the
    // bottom: the re-wrap's growth belongs above the viewport, so pinning
    // the top line would push the end of the conversation below the pane
    // ("the view moves way past the messages").
    let pin = pin_top && !(explicit_toggle && app.scroll == 0);
    if pin
        && !explicit_toggle
        && let Some(anchor) = anchor
        && anchor.chars().count() >= 12
    {
        // Scrolled up with the viewport inside the streaming tail: pin to
        // the content line, not the line index.
        // A very short anchor is a frontier line still being typed, where
        // the delta path below is exact anyway (all growth is below it) —
        // and a short prefix could match unrelated lines below the anchor.
        let prefix: String = anchor.chars().take(24).collect();
        // The anchor can only have moved into lines rendered since the last
        // frame: the tail plus any messages appended to the cache.
        let search_from = app.prev_total.saturating_sub(prev_tail.len()).min(total);
        let mut hit: Option<usize> = None;
        let mut i = total;
        while i > search_from && hit.is_none() {
            i -= 1;
            let text = if i < cached_lines {
                line_text(&cache.lines[i])
            } else {
                line_text(&tail[i - cached_lines])
            };
            if text.starts_with(&prefix) {
                hit = Some(i);
                break;
            }
        }
        if let Some(hit) = hit {
            app.scroll = max_top.saturating_sub(hit);
        } else {
            let delta = total as i64 - app.prev_total as i64;
            if delta != 0 {
                app.scroll = (app.scroll as i64 + delta).clamp(0, app.max_scroll as i64) as usize;
            }
        }
    } else if pin && explicit_toggle {
        // Display-flag toggle (Ctrl+R reasoning, Ctrl+T tool detail) while
        // scrolled up: the cache re-wrap preserves every message — only
        // reasoning / tool-detail blocks inside them appear or disappear —
        // so pin the viewport to the message the previous frame's top line
        // belonged to, at the same offset within it. Delta-compensating the
        // absolute line index would instead land on content shifted by all
        // the growth above the viewport ("the view jumps way past the
        // messages").
        let mut pinned: Option<usize> = None;
        if let Some(owner) = app.sel.owner_at(prev_top) {
            // The top line's offset within its message block in the
            // previous render (walk up to the block start).
            let mut old_start = prev_top;
            while old_start > 0 && app.sel.owner_at(old_start - 1) == Some(owner) {
                old_start -= 1;
            }
            let offset = prev_top - old_start;
            // The same message's block in the new render: the cache prefix
            // for stored messages, the streaming tail for the in-flight
            // reply (its owner id is the message count at record time).
            let (new_start, block_len) = if owner == app.messages.len() {
                (Some(cached_lines), tail.len())
            } else {
                let start = cache.owner.iter().position(|o| *o == Some(owner));
                let len = start.map_or(0, |s| {
                    cache.owner[s..]
                        .iter()
                        .take_while(|o| **o == Some(owner))
                        .count()
                });
                (start, len)
            };
            if let Some(new_start) = new_start {
                pinned = Some(new_start + offset.min(block_len.saturating_sub(1)));
            }
        }
        if pinned.is_none() {
            // Fallback when the message block can't be located (e.g. the
            // session changed between frames): re-find the previous top
            // line's text, trying nearby lines first — the top is often a
            // blank card row or a "▸ reasoning" header whose text the
            // toggle itself changes.
            'candidates: for off in [0, 1, 2, 3, 4, 5, -1, -2, -3, -4, -5] {
                let li = if off >= 0 {
                    prev_top + off as usize
                } else {
                    prev_top.saturating_sub((-off) as usize)
                };
                let Some(text) = app.sel.line_at(li) else {
                    continue;
                };
                if text.chars().count() < 12 {
                    continue;
                }
                // Exact text survives the re-wrap; fall back to a prefix
                // for streaming frontier lines that kept growing.
                if let Some(hit) = find_line(&cache.lines, &tail, |l| l == text) {
                    pinned = Some((hit as i64 - off).max(0) as usize);
                    break 'candidates;
                }
                let prefix: String = text.chars().take(24).collect();
                if let Some(hit) = find_line(&cache.lines, &tail, |l| l.starts_with(&prefix)) {
                    pinned = Some((hit as i64 - off).max(0) as usize);
                    break 'candidates;
                }
            }
        }
        if let Some(pinned) = pinned {
            app.scroll = max_top.saturating_sub(pinned);
        } else {
            let delta = total as i64 - app.prev_total as i64;
            if delta != 0 {
                app.scroll = (app.scroll as i64 + delta).clamp(0, app.max_scroll as i64) as usize;
            }
        }
    } else if pin {
        let delta = total as i64 - app.prev_total as i64;
        if delta != 0 {
            app.scroll = (app.scroll as i64 + delta).clamp(0, app.max_scroll as i64) as usize;
        }
    }
    app.prev_total = total;
    app.prev_tail = tail.iter().map(line_text).collect();

    app.scroll = app.scroll.min(app.max_scroll);
    let top = max_top.saturating_sub(app.scroll);

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

    // Scrollbar in the gutter: only when the conversation overflows. Drawn
    // directly rather than with ratatui's `Scrollbar` widget — that widget
    // proportions the thumb against `content_length - 1 + viewport`, which
    // inflates it when the content barely overflows, re-rounds its size and
    // position on every streamed line (the thumb visibly jumps while
    // thinking or tool output grows), and never maps it flush to the track
    // bottom — following the stream leaves a dead gap under the thumb.
    // Here the thumb is the true viewport fraction and its travel maps
    // linearly onto the track: flush at the top when scrolled to the start,
    // flush at the bottom when following.
    if total > height {
        render_scrollbar(f, area, total, top, &app.theme);
    }
}

/// Draw the history scrollbar into the 1-column gutter at `area`'s right
/// edge. `total` is the rendered line count and `top` the first visible
/// line. The thumb covers the viewport's fraction of the content and maps
/// linearly onto the track, so it sits flush against both track ends at the
/// scroll extremes — no dead gap under the thumb while following the stream.
fn render_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    top: usize,
    theme: &crate::theme::Theme,
) {
    let track = area.height as usize;
    if track == 0 {
        return;
    }
    // Viewport fraction of the content, at least one cell.
    let thumb_len = (track * track / total).clamp(1, track);
    // `top` ranges 0..=total-track; map it onto the thumb's travel range
    // 0..=track-thumb_len so both scroll extremes land flush. (The caller
    // only renders the scrollbar when `total > height`, so `room` is never
    // zero — the `unwrap_or(0)` is just a safety net.)
    let room = total.saturating_sub(track);
    let thumb_top = (top * (track - thumb_len)).checked_div(room).unwrap_or(0);
    let gutter = Rect {
        x: area.x + area.width.saturating_sub(1),
        y: area.y,
        width: 1,
        height: area.height,
    };
    let thumb_style = Style::default().fg(theme.accent);
    let track_style = Style::default().fg(theme.border_dim);
    let lines: Vec<Line> = (0..track)
        .map(|y| {
            let (symbol, style) = if y >= thumb_top && y < thumb_top + thumb_len {
                ("█", thumb_style)
            } else {
                ("║", track_style)
            };
            Line::from(Span::styled(symbol, style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), gutter);
}

/// Scan the rendered lines (cache prefix + streaming tail) from the bottom
/// for the last line matching `pred` — used to re-find a pinned viewport
/// line by text after a cache re-wrap.
fn find_line(
    cache: &[Line<'static>],
    tail: &[Line<'static>],
    pred: impl Fn(&str) -> bool,
) -> Option<usize> {
    let total = cache.len() + tail.len();
    let mut i = total;
    while i > 0 {
        i -= 1;
        let text = if i < cache.len() {
            line_text(&cache[i])
        } else {
            line_text(&tail[i - cache.len()])
        };
        if pred(&text) {
            return Some(i);
        }
    }
    None
}

// Long by design (cache sync).
#[allow(clippy::too_many_lines)]
/// Bring the cached wrapped prefix up to date: reset on width/flag/session
/// change, then wrap only messages not yet cached. Stored messages are
/// append-only, so this is O(new messages) per frame instead of O(all).
fn sync_cache(app: &mut AppView, width: usize) {
    let key = (
        app.session.as_ref().map(|s| s.id.clone()),
        width,
        app.show_tool_detail,
        app.settings.show_reasoning,
        app.settings.hide_hints,
        app.settings.show_stats,
        app.theme_gen,
    );
    // Work on a detached cache: the wrap pass reads the domain half
    // (messages, settings, space) through the `AppView` deref, which borrows
    // the whole view — it can't overlap a borrow of a view field.
    let mut cache = std::mem::take(&mut app.history_cache);
    let c = &mut cache;
    if c.key != key || app.messages.len() < c.msg_count {
        if c.key.0 != key.0 {
            // Session changed — save old cache for later reuse.
            if let Some(sid) = &c.key.0 {
                app.session_caches.insert(sid.clone(), std::mem::take(c));
            }
            // Restore cache for the new session if available.
            if let Some(sid) = &key.0
                && let Some(cached) = app.session_caches.remove(sid)
                && cached.key == key
                && cached.msg_count == app.messages.len()
            {
                *c = cached;
                return;
            }
            // New session or stale cache — full reset.
            *c = HistoryCache {
                key,
                ..Default::default()
            };
        } else if app.messages.len() < c.msg_count || c.key.1 != key.1 || c.key.6 != key.6 {
            // Width or theme changed — full reset (drops image cache too).
            *c = HistoryCache {
                key,
                ..Default::default()
            };
        } else {
            // Only display flags changed — keep image cache, re-wrap messages.
            let ic = std::mem::take(&mut c.image_cache);
            *c = HistoryCache {
                key,
                ..Default::default()
            };
            c.image_cache = ic;
        }
    }
    let theme = app.theme;
    for (i, m) in app.messages.iter().enumerate().skip(c.msg_count) {
        let start = c.lines.len();
        // Day divider when the conversation crosses a midnight boundary.
        if let Some(day) = m.created_at.as_deref().map(day_of)
            && c.last_day.as_deref() != Some(day)
        {
            if c.last_day.is_some() {
                rule_line(&mut c.lines, &day_label(day), width, &theme);
            }
            c.last_day = Some(day.to_string());
        }
        if m.role == "user" || m.role == "gate_reply" {
            let images_dir = app.space.files_dir(&app.active_space.name);
            push_user_card(
                &mut c.lines,
                &mut c.image_at_line,
                &m.content,
                width,
                &theme,
                m.created_at.as_deref(),
                &images_dir,
                &mut c.image_cache,
            );
        } else if m.role == "compaction" {
            push_compaction(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "research_stage" {
            push_research_stage(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "error" {
            push_error(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "survey" {
            push_survey_section(&mut c.lines, &m.content, width, &theme);
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
            push_assistant_stored(
                &mut c.lines,
                &mut c.image_at_line,
                &m.content,
                m,
                &app.settings,
                width,
                &mut c.code,
                &mut c.blocks,
                &theme,
                &app.space.files_dir(&app.active_space.name),
                &mut c.image_cache,
            );
        }
        c.owner.resize(c.lines.len(), Some(i));
        c.code.resize(c.lines.len(), None);
        c.image_at_line.resize(c.lines.len(), None);
        let new_plain: Vec<String> = c.lines[start..].iter().map(line_text).collect();
        c.plain.extend(new_plain);
    }
    c.msg_count = app.messages.len();
    app.history_cache = cache;
}

/// The empty start screen: a rounded panel holding the gradient banner, a
/// random greeting, a live clock, quick-start chips, and the most recent
/// sessions.
fn render_welcome(f: &mut Frame, app: &AppView, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    // Per-line gradient across the accent ramp: accent -> accent2.
    let banner_lines: Vec<&str> = app.banner.lines().collect();
    let n = banner_lines.len().max(1);
    for (i, l) in banner_lines.into_iter().enumerate() {
        let t = if n > 1 {
            i as f32 / (n - 1) as f32
        } else {
            0.0
        };
        let color = ramp(app.theme.accent, app.theme.accent2, t);
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
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
    if !app.settings.hide_hints {
        lines.push(Line::from(""));
        lines.push(chip_row(
            &["/research", "/swarm", "/model", "/help"],
            &app.theme,
        ));
    }
    // Most recent sessions across this space, as a quick-jump list.
    if let Ok(sessions) = app.db.list_sessions(&app.active_space.id) {
        let recent: Vec<_> = sessions.into_iter().take(4).collect();
        if !recent.is_empty() {
            lines.push(Line::from(""));
            let inner_w = area.width.saturating_sub(4) as usize;
            rule_line(&mut lines, "recent", inner_w, &app.theme);
            for s in &recent {
                let when = super::fmt_created(&s.created_at);
                lines.push(Line::from(vec![
                    Span::styled(
                        "▸ ",
                        Style::default()
                            .fg(app.theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(s.title.clone(), Style::default().fg(app.theme.fg)),
                    Span::styled(format!("  {when}"), Style::default().fg(app.theme.fg_dim)),
                ]));
            }
        }
    }

    // Frame it in a rounded panel, centered.
    let panel_w = area.width.min(86);
    let panel_h = (lines.len() + 2).min(area.height as usize) as u16;
    let panel = Rect {
        x: area.x + area.width.saturating_sub(panel_w) / 2,
        y: area.y + area.height.saturating_sub(panel_h) / 2,
        width: panel_w,
        height: panel_h,
    };
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.border_dim));
    let inner = block.inner(panel);
    f.render_widget(block, panel);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

/// Linear blend between two colors at `t` in 0.0..=1.0.
fn ramp(a: Color, b: Color, t: f32) -> Color {
    let mix = |x: u8, y: u8| {
        let (xf, yf) = (f32::from(x), f32::from(y));
        (xf + (yf - xf) * t).round() as u8
    };
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => a,
    }
}

/// `[ /cmd ]` chips in a dim bracket style, separated by two spaces.
fn chip_row(cmds: &[&str], theme: &crate::theme::Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, cmd) in cmds.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled("[ ", Style::default().fg(theme.border_dim)));
        spans.push(Span::styled(
            cmd.to_string(),
            Style::default().fg(theme.accent),
        ));
        spans.push(Span::styled(" ]", Style::default().fg(theme.border_dim)));
    }
    Line::from(spans)
}

/// A centered `── label ──` rule, used for day dividers and the welcome
/// screen's recent-sessions header.
fn rule_line(out: &mut Vec<Line<'static>>, label: &str, width: usize, theme: &crate::theme::Theme) {
    let label = format!(" {label} ");
    let dashes = width.saturating_sub(label.chars().count());
    let line = format!(
        "{}{}{}",
        "─".repeat(dashes / 2),
        label,
        "─".repeat(dashes - dashes / 2)
    );
    out.push(Line::from(Span::styled(
        line,
        Style::default().fg(theme.fg_dim),
    )));
}

/// `2026-08-08T14:32:00Z` -> `2026-08-08` (`created_at` is ISO-8601; the
/// first 10 bytes of an RFC3339 timestamp are always an ASCII date).
fn day_of(rfc3339: &str) -> &str {
    rfc3339.get(0..10).unwrap_or(rfc3339)
}

/// Human label for a "YYYY-MM-DD" day: Today / Yesterday / weekday+date.
fn day_label(day: &str) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if day == today {
        return "Today".into();
    }
    let yesterday = (chrono::Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    if day == yesterday {
        return "Yesterday".into();
    }
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_or_else(|_| day.to_string(), |d| d.format("%A, %B %-d").to_string())
}

/// Remove `![alt](file)` image references from text — the images themselves
/// are rendered inline by `render_markdown_images`, so the raw refs must not
/// also wrap into the body text. Unterminated refs (mid-stream) are kept
/// verbatim.
fn strip_markdown_images(content: &str) -> String {
    let mut rest = content;
    let mut out = String::with_capacity(content.len());
    loop {
        let Some(start) = rest.find("![") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find(')') {
            Some(end) if after[..end].contains("](") => {
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str("![");
                out.push_str(after);
                break;
            }
        }
    }
    out
}

/// A user message card: a right-aligned bubble capped at ~60% of the pane.
/// Its padding explicitly resets to the terminal's default background so a
/// terminal-level transparent background remains visible. The `❯ you` header
/// and time sit inside the bubble; images render inside at the bubble's width.
#[allow(clippy::too_many_arguments)]
fn push_user_card(
    out: &mut Vec<Line<'static>>,
    image_at_line: &mut Vec<Option<String>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
    created_at: Option<&str>,
    images_dir: &std::path::Path,
    image_cache: &mut HashMap<(String, usize), Vec<Line<'static>>>,
) {
    let card_w = (width * 3 / 5)
        .clamp(24, 64)
        .min(width.saturating_sub(6).max(24));
    let inner = card_w.saturating_sub(4);
    // Terminal colors have no alpha channel. Resetting the background lets
    // the terminal itself decide whether its default background is opaque or
    // transparent instead of baking an opaque RGB tint into every card cell.
    let bg_style = Style::default().bg(Color::Reset);

    let mut card: Vec<Line<'static>> = Vec::new();
    let mut card_img: Vec<Option<String>> = Vec::new();

    let mut head = vec![Span::styled(
        "❯ you",
        Style::default()
            .fg(theme.user_msg)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(t) = created_at {
        let time = super::fmt_created(t);
        let used: usize = head.iter().map(|s| s.content.chars().count()).sum();
        let pad = inner.saturating_sub(used + 1 + time.chars().count());
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(time, Style::default().fg(theme.fg_dim)));
    }
    card.push(Line::from(head));
    card_img.push(None);

    render_markdown_images(
        &mut card,
        content,
        inner,
        theme,
        images_dir,
        &mut card_img,
        image_cache,
        None,
    );
    for line in wrap_plain(&strip_markdown_images(content), inner) {
        card.push(Line::from(Span::raw(line)));
        card_img.push(None);
    }

    // Emit: left margin (pane bg) + 2-col pad + content + pad to card width.
    let lead = width.saturating_sub(card_w);
    for (li, line) in card.into_iter().enumerate() {
        let len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        let is_image = card_img[li].is_some();
        let mut spans = Vec::with_capacity(4);
        if lead > 0 {
            spans.push(Span::raw(" ".repeat(lead)));
        }
        spans.push(Span::styled("  ", bg_style));
        for sp in line.spans {
            // Image rows carry their own per-pixel backgrounds — the card
            // tint must not override them.
            if is_image {
                spans.push(sp);
            } else {
                spans.push(Span::styled(sp.content, sp.style.patch(bg_style)));
            }
        }
        spans.push(Span::styled(
            " ".repeat(card_w.saturating_sub(len + 2)),
            bg_style,
        ));
        out.push(Line::from(spans));
        image_at_line.push(card_img[li].clone());
    }
    out.push(Line::from(""));
    image_at_line.push(None);
}

/// A tool call block: a dim `⚒ name summary` one-liner; when tool detail is
/// on (Ctrl+T), the full arguments and result follow, reasoning-style.
fn push_tool_call(
    out: &mut Vec<Line<'static>>,
    content: &str,
    expanded: bool,
    settings: &nexus_core::app::Settings,
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
    let summary = nexus_core::app::tool_call_summary(&name, &args, &result);
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

/// A compaction-digest block: the digest of the earlier conversation, shown
/// right at the compaction boundary in the transcript — what was folded
/// away is visible in the chat itself, not only behind the context popup's
/// editor. Header in accent2, digest body dimmed so the live conversation
/// stays prominent.
fn push_compaction(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    out.push(Line::from(vec![
        Span::styled("📄 ", Style::default().fg(theme.accent2)),
        Span::styled(
            "conversation compacted — earlier messages summarized:",
            Style::default()
                .fg(theme.accent2)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    for line in wrap_plain(content, width.saturating_sub(2)) {
        out.push(Line::from(dim(format!("  {line}"), theme)));
    }
    out.push(Line::from(""));
}

/// A background-research progress line: a dim one-liner with a 🔎 marker,
/// no expand/collapse (unlike `tool_call` — there's no arguments/result pair,
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

/// A pending research-survey section: the scoping agent's clarifying
/// questions, awaiting a chat answer. Same family as `push_research_plan` —
/// distinct ❓ marker, accent header line, questions plain, the guidance
/// footer dimmed (it's the only passive part).
fn push_survey_section(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let mut first = true;
    for line in wrap_plain(content, width.saturating_sub(2)) {
        if first {
            out.push(Line::from(vec![
                Span::styled("❓ ", Style::default().fg(theme.accent)),
                Span::styled(
                    line,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            first = false;
        } else if line.starts_with("Answer in chat") {
            out.push(Line::from(dim(line, theme)));
        } else {
            out.push(Line::from(format!("  {line}")));
        }
    }
    out.push(Line::from(""));
}

/// A pending plan-approval message: like `push_research_stage` but with a
/// distinct marker and full (non-dim) styling, since it's actionable —
/// reply in chat to approve or change it — not passive progress.
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

/// A stored assistant reply: a `✦ <model>` header (persona name in accent
/// for swarm turns) with the completion time right-aligned, the collapsible
/// reasoning, inline images, the markdown answer, then a dim stats/phrase
/// footer. Everything below the header carries the `▎` left rail.
#[allow(clippy::too_many_lines)] // one card shape per content kind
#[allow(clippy::too_many_arguments)]
fn push_assistant_stored(
    out: &mut Vec<Line<'static>>,
    image_at_line: &mut Vec<Option<String>>,
    content: &str,
    msg: &Message,
    settings: &nexus_core::app::Settings,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
    theme: &crate::theme::Theme,
    images_dir: &std::path::Path,
    image_cache: &mut HashMap<(String, usize), Vec<Line<'static>>>,
) {
    // Header: ✦ + who answered (persona overrides the model name).
    let mut head = vec![Span::styled(
        "✦ ",
        Style::default()
            .fg(theme.accent2)
            .add_modifier(Modifier::BOLD),
    )];
    match (&msg.persona, msg.model.as_deref()) {
        (Some(p), Some(m)) => {
            head.push(Span::styled(
                p.clone(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
            head.push(dim(format!(" · {m}"), theme));
        }
        (Some(p), None) => {
            head.push(Span::styled(
                p.clone(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        (None, Some(m)) => {
            head.push(Span::styled(
                m.to_string(),
                Style::default()
                    .fg(theme.assistant_msg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        (None, None) => {
            head.push(Span::styled(
                "assistant",
                Style::default()
                    .fg(theme.assistant_msg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    if let Some(t) = &msg.created_at {
        let time = super::fmt_created(t);
        let head_len: usize = head.iter().map(|s| s.content.chars().count()).sum();
        let pad = width.saturating_sub(head_len + 1 + time.chars().count());
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(time, Style::default().fg(theme.fg_dim)));
    }
    out.push(Line::from(head));

    let rail = Span::styled("▎ ", Style::default().fg(theme.accent2));
    if let Some(r) = &msg.reasoning {
        if settings.show_reasoning {
            out.push(Line::from(vec![rail.clone(), dim("▾ reasoning", theme)]));
            for line in wrap_plain(r, width.saturating_sub(2)) {
                out.push(Line::from(vec![rail.clone(), dim(line, theme)]));
            }
        } else {
            let n = r.chars().count();
            let hint = if settings.hide_hints {
                ""
            } else {
                " — Ctrl+R to expand"
            };
            out.push(Line::from(vec![
                rail.clone(),
                dim(format!("▸ reasoning ({n} chars){hint}"), theme),
            ]));
        }
    }

    // Inline images slot between the reasoning block and the answer body.
    render_markdown_images(
        out,
        content,
        width.saturating_sub(2),
        theme,
        images_dir,
        image_at_line,
        image_cache,
        Some(&rail),
    );

    let mut rendered =
        crate::ui::markdown::render(&strip_markdown_images(content), width.saturating_sub(2));
    rendered.lines = crate::ui::citations_style::style_citations(rendered.lines, theme.accent);
    rendered.lines = crate::ui::citations_style::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered, Some(rail));

    // Footer below the response: phrase + stats (the model lives in the
    // header now).
    let mut footer = String::new();
    if let Some(p) = &msg.phrase {
        footer.push_str("· ");
        footer.push_str(p);
    }
    if settings.show_stats
        && let (Some(tok), Some(secs)) = (msg.tokens, msg.secs)
    {
        let tps = if secs > 0.0 { tok as f64 / secs } else { 0.0 };
        let _ = write!(footer, "  ·  {tps:.1} tok/s · ~{tok} tok · {secs:.2}s");
    }
    if settings.show_stats
        && let Some(cost) = msg.cost.filter(|c| *c > 0.0)
    {
        let _ = write!(footer, "  ·  {}", fmt_cost(Some(cost)));
    }
    if !footer.is_empty() {
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
    let arrow = if label.starts_with("🔗") {
        "→"
    } else {
        "↩"
    };
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
/// The in-progress reply: a `⠹ <model> — <phrase>` header in the spinner
/// color, the thinking block when present, then the live markdown stream.
fn push_assistant_streaming(
    out: &mut Vec<Line<'static>>,
    app: &AppView,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
) {
    let color = to_color(app.spinner_color());
    let name = app
        .active_chat_task()
        .map_or("assistant", |t| t.model.as_str());
    let mut head = vec![
        Span::styled(
            format!("{} ", app.spinner_char()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{name} — {}", app.thinking_phrase()),
            Style::default().fg(color),
        ),
    ];
    // Live elapsed time, right-aligned.
    if let Some(t) = app.active_chat_task() {
        let secs = t.started.elapsed().as_secs();
        let time = format!("{}:{:02}", secs / 60, secs % 60);
        let used: usize = head.iter().map(|s| s.content.chars().count()).sum();
        let pad = width.saturating_sub(used + 1 + time.chars().count());
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(time, Style::default().fg(app.theme.fg_dim)));
    }
    out.push(Line::from(head));

    let rail = Span::styled("▎ ", Style::default().fg(color));
    if let Some(t) = app.thinking_text() {
        for line in wrap_plain(t, width.saturating_sub(2)) {
            out.push(Line::from(vec![rail.clone(), dim(line, &app.theme)]));
        }
    }

    let buf = app.active_streaming_text().unwrap_or("");
    let mut rendered =
        crate::ui::markdown::render(&strip_markdown_images(buf), width.saturating_sub(2));
    rendered.lines = crate::ui::citations_style::style_citations(rendered.lines, app.theme.accent);
    rendered.lines = crate::ui::citations_style::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered, Some(rail));
    out.push(Line::from(""));
}

/// Splice a `markdown::Rendered` into the running line/code/block vecs, keeping
/// `code` aligned to `out` and offsetting local block ids to global ones. When
/// `rail` is given it prefixes every body line (the assistant left gutter).
fn push_rendered(
    out: &mut Vec<Line<'static>>,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
    r: crate::ui::markdown::Rendered,
    rail: Option<Span<'static>>,
) {
    code.resize(out.len(), None); // align past any dot/reasoning lines
    let base = blocks.len();
    code.extend(r.code.iter().map(|c| c.map(|id| id + base)));
    blocks.extend(r.blocks);
    if let Some(rail) = rail {
        for line in r.lines {
            let mut line = line;
            line.spans.insert(0, rail.clone());
            out.push(line);
        }
    } else {
        out.extend(r.lines);
    }
}

/// Scan content for markdown image references `![alt](file)` and render them
/// inline. For each match, resolve the file against `images_dir`, render it
/// with `image_to_halfblock_lines`, and track the lines in `image_at_line`.
/// Results are cached by (path, width) to avoid re-decoding every frame. When
/// `prefix` is given (the assistant rail) every pushed line — image rows and
/// the trailing blank alike — carries it, so the gutter stays continuous.
#[allow(clippy::too_many_arguments)]
fn render_markdown_images(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
    images_dir: &std::path::Path,
    image_at_line: &mut Vec<Option<String>>,
    image_cache: &mut HashMap<(String, usize), Vec<Line<'static>>>,
    prefix: Option<&Span<'static>>,
) {
    let mut rest = content;
    while let Some(start) = rest.find("![") {
        if let Some(end) = rest[start..].find(')') {
            let inner = &rest[start + 2..start + end];
            if let Some((alt, file)) = inner.split_once("](") {
                let path = images_dir.join(file);
                let path_str = path.to_string_lossy().to_string();
                let key = (path_str.clone(), width);
                let half = image_cache
                    .entry(key)
                    .or_insert_with(|| image_to_halfblock_lines(&path_str, width));
                if half.len() <= 1
                    && half
                        .first()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_default()
                        .contains("[image]")
                {
                    let mut line = Line::from(dim(format!("🖼 {alt}"), theme));
                    if let Some(p) = prefix {
                        line.spans.insert(0, p.clone());
                    }
                    out.push(line);
                    image_at_line.push(Some(path_str));
                } else {
                    let img_start = out.len();
                    for l in half.clone() {
                        let mut l = l;
                        if let Some(p) = prefix {
                            l.spans.insert(0, p.clone());
                        }
                        out.push(l);
                    }
                    let img_end = out.len();
                    let mut blank = Line::from("");
                    if let Some(p) = prefix {
                        blank.spans.insert(0, p.clone());
                    }
                    out.push(blank);
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
    let Ok(img) = image::open(path) else {
        return vec![Line::from(Span::raw("🖼 [image]"))];
    };
    if max_width < 4 {
        return vec![Line::from(Span::raw("🖼"))];
    }
    let mut cell_w = max_width.min(img.width() as usize);
    let aspect = f64::from(img.width()) / f64::from(img.height());
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
    let resized = img.resize_exact(
        pixel_w as u32,
        pixel_h as u32,
        image::imageops::FilterType::Lanczos3,
    );
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
    use nexus_core::app::App;
    use nexus_core::db::{Db, Message};

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        }
    }

    fn test_app() -> AppView {
        let db = Db::open_in_memory().unwrap();
        let space = nexus_core::space::Space {
            root: std::env::temp_dir().join(format!("nexus-hist-{}", uuid::Uuid::new_v4())),
        };
        AppView::new(App::new(db, Some("k"), space))
    }

    /// Build an app with a session and a manually-inserted streaming task.
    fn streaming_app(thinking: &str, buffer: &str) -> AppView {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let s =
            a.db.create_session("stream test", "a/one", &space, "chat")
                .unwrap();
        let session = a.db.get_session(&s.id).unwrap().unwrap();
        a.session = Some(session);
        a.settings.show_reasoning = true;
        a.show_tool_detail = true;
        // Stored history so the transcript overflows the pane.
        for i in 0..8 {
            a.messages.push(msg(
                "user",
                &format!("earlier user message number {i} with a decent amount of body text"),
            ));
            a.messages.push(msg(
                "assistant",
                &format!(
                    "earlier assistant reply {i} with a longer body so it wraps over a few lines"
                ),
            ));
        }
        let abort = tokio::spawn(async {}).abort_handle();
        let task = nexus_core::app::ChatTask {
            id: 1,
            session_id: s.id.clone(),
            session_title: "stream test".into(),
            space_id: space,
            model: "a/one".into(),
            model_id: "one".into(),
            backend: nexus_core::provider::BackendTag::OpenRouter,
            incognito: false,
            buffer: buffer.into(),
            thinking: thinking.into(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: nexus_core::app::SpinnerColor::Green,
            abort,
        };
        a.chat_tasks.insert(1, task);
        a
    }

    /// The rendered text of the first three rows of the history pane — the
    /// ground truth of what the user sees at the viewport top.
    fn visible_snapshot(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..3 {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
            }
            out.push('|');
        }
        out
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

    #[test]
    fn survey_section_renders_marker_header_and_dimmed_footer() {
        let mut a = test_app();
        a.messages.push(msg(
            "survey",
            "For \"fine-tuning LLMs\":\n 1. Depth or breadth?\n\nAnswer in chat — then say \"I approve\".",
        ));
        sync_cache(&mut a, 80);
        let text = a
            .history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("❓"), "{text}");
        assert!(text.contains("fine-tuning LLMs"), "{text}");
        assert!(text.contains("1. Depth or breadth?"), "{text}");
        assert!(text.contains("Answer in chat"), "{text}");
        // The survey row is never replayed to the model — the role filter in
        // build_history skips it (covered in app/chat.rs tests); here we just
        // verify the renderer picked it up.
        assert!(
            a.history_cache
                .plain
                .iter()
                .any(|l| l.contains("Depth or breadth?"))
        );
    }

    #[test]
    fn compaction_digest_renders_with_header_and_body() {
        let mut a = test_app();
        a.messages.push(msg("user", "long conversation…"));
        a.messages
            .push(msg("compaction", "digest line one\ndigest line two"));
        sync_cache(&mut a, 80);
        let text = a
            .history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("📄"), "{text}");
        assert!(text.contains("conversation compacted"), "{text}");
        assert!(text.contains("digest line one"), "{text}");
        assert!(text.contains("digest line two"), "{text}");
        // The digest appears in the copy/plain view too — it's visible
        // transcript content, not a hidden sidecar.
        assert!(
            a.history_cache
                .plain
                .iter()
                .any(|l| l.contains("digest line one")),
            "{text}"
        );
    }

    /// A conversation tall enough to overflow the history pane, with one
    /// tool-call block whose detail toggle produces a large line delta.
    fn tall_tool_call_app() -> AppView {
        let mut a = test_app();
        for i in 0..60 {
            a.messages.push(msg(
                "user",
                &format!("message number {i} with some body text"),
            ));
        }
        let tool_content = serde_json::json!({
            "name": "search",
            "arguments": r#"{"mode":"web","query":"q"}"#,
            "result": "line one\n".repeat(40),
        })
        .to_string();
        a.messages.push(msg("tool_call", &tool_content));
        a
    }

    #[tokio::test]
    async fn streaming_reasoning_growth_keeps_viewport_on_reasoning() {
        use std::fmt::Write as _;
        let reasoning: String = (0..60)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "The answer body.\nSecond answer line.");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        // Scroll up into the middle of the reasoning block.
        a.scroll = 8;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the reasoning, got: {before}"
        );

        // Reasoning keeps streaming below the viewport.
        let task = a.chat_tasks.get_mut(&1).unwrap();
        for i in 0..20 {
            write!(
                task.thinking,
                "\nfresh reasoning token {i} some more words to wrap"
            )
            .unwrap();
        }
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped while reading reasoning: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn streaming_reasoning_growth_keeps_viewport_on_answer() {
        use std::fmt::Write as _;
        let reasoning: String = (0..50)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let answer: String = (0..40)
            .map(|i| format!("answer paragraph line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, &answer);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        // Scroll up into the answer region (bottom of the streaming tail).
        assert!(a.max_scroll > 0, "needs an overflowing transcript");
        a.scroll = 12;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("answer"),
            "viewport should be in the answer, got: {before}"
        );

        // Reasoning keeps streaming above the viewport; answer grows a bit below.
        let task = a.chat_tasks.get_mut(&1).unwrap();
        for i in 0..30 {
            write!(
                task.thinking,
                "\nmore reasoning token {i} with some words to wrap"
            )
            .unwrap();
        }
        task.buffer.push_str(" more answer tokens here.");
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn streaming_tail_to_cache_transition_does_not_jump() {
        let mut a = streaming_app(
            "some reasoning text here\nmore of it",
            &"A final answer body with enough text to wrap over several lines. ".repeat(6),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = 3;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);

        // Stream finishes: the tail is replaced by the cached stored message.
        a.on_chat_event(1, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "tail->cache transition jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn streaming_tail_to_cache_transition_pins_inside_the_tail() {
        let reasoning: String = (0..40)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "A final answer body line one.\nBody line two.");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        // Scroll up so the viewport top is inside the reasoning block.
        a.scroll = 5;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the reasoning, got: {before}"
        );

        a.on_chat_event(1, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "tail->cache transition inside the tail jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn tool_call_landing_above_viewport_keeps_position() {
        let reasoning: String = (0..60)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "answer body line one\nanswer body line two");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        // Scroll up into the reasoning block (well above where the tool
        // message will land in the cache).
        a.scroll = 10;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the reasoning, got: {before}"
        );

        // A tool call completes: with tool detail on, its block is long and
        // lands in the cache ABOVE the streaming tail.
        a.on_chat_event(
            1,
            nexus_core::provider::StreamEvent::ToolCall {
                name: "web_search".into(),
                arguments: r#"{"query":"some search query text"}"#.into(),
                result: "result line one\nresult line two\nresult line three".into(),
            },
        )
        .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "tool call landing above the viewport jumped: before={before:?} after={after:?}"
        );
    }

    #[test]
    fn flag_toggle_pins_viewport_when_scrolled_up() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total1 = a.history_cache.lines.len();
        assert!(a.max_scroll > 10, "conversation should overflow the pane");

        // User scrolls up a little, then Ctrl+T expands the tool block.
        a.scroll = 5;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 5);
        a.show_tool_detail = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total2 = a.history_cache.lines.len();
        let delta = total2 as i64 - total1 as i64;
        assert!(delta > 20, "tool detail should add many lines");
        // Scroll compensated by the delta: the top line stayed put.
        assert_eq!(
            a.scroll,
            5 + delta as usize,
            "viewport top must stay pinned"
        );

        // Ctrl+T again collapses: the viewport returns to the same spot.
        a.show_tool_detail = false;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 5, "collapse must restore the pinned position");
    }

    #[test]
    fn flag_toggle_while_following_bottom_stays_at_bottom() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total1 = a.history_cache.lines.len();
        assert_eq!(a.scroll, 0, "starts following the bottom");

        // Ctrl+T while following the bottom: the block expands in place and
        // the viewport keeps following the bottom — the end of the
        // conversation stays visible instead of the expansion pushing it
        // below the pane.
        a.show_tool_detail = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total2 = a.history_cache.lines.len();
        assert!(total2 > total1, "tool detail should add lines");
        assert_eq!(a.scroll, 0, "must keep following the bottom");
    }

    /// A tall conversation with reasoning traces on many assistant messages,
    /// so a show-reasoning toggle re-wraps content above and below the
    /// viewport, not just below it.
    fn tall_reasoning_app() -> AppView {
        let mut a = test_app();
        for i in 0..40 {
            a.messages.push(msg(
                "user",
                &format!("question number {i} with some body text"),
            ));
            let mut m = msg(
                "assistant",
                &format!("answer number {i} with a decent amount of body text"),
            );
            m.reasoning = Some(format!(
                "thinking trace {i} line one\nthinking trace {i} line two\nthinking trace {i} line three"
            ));
            m.model = Some("a/one".into());
            m.phrase = Some("working".into());
            a.messages.push(m);
        }
        a.settings.show_reasoning = false;
        a
    }

    #[test]
    fn flag_toggle_reasoning_while_scrolled_up_pins_content() {
        let mut a = tall_reasoning_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total1 = a.history_cache.lines.len();
        assert!(a.max_scroll > 10, "conversation should overflow the pane");

        // Scroll up into the middle of the conversation.
        a.scroll = 25;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("answer number 34"),
            "viewport should be in the middle of the conversation, got: {before}"
        );

        // Ctrl+R expands reasoning across the whole conversation — most of
        // the growth lands ABOVE the viewport. The viewport must stay on
        // the same content, not jump to older messages.
        a.settings.show_reasoning = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total2 = a.history_cache.lines.len();
        assert!(total2 > total1, "reasoning should add lines");
        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped on reasoning toggle: before={before:?} after={after:?}"
        );

        // Collapsing again returns to the same content.
        a.settings.show_reasoning = false;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let after2 = visible_snapshot(&terminal);
        assert!(
            after2.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped on reasoning collapse: before={before:?} after={after2:?}"
        );
    }

    #[test]
    fn flag_toggle_reasoning_while_following_bottom_stays_at_bottom() {
        let mut a = tall_reasoning_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "starts following the bottom");

        // Ctrl+R while at the bottom: the end of the conversation stays at
        // the bottom — the last message's reasoning is visible instead of
        // being pushed below the pane.
        a.settings.show_reasoning = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "must keep following the bottom");
        let buf = terminal.backend().buffer();
        let all: String = (0..HISTORY_H)
            .flat_map(|y| {
                (0..buf.area.width - 1).map(move |x| buf[(x, y as u16)].symbol().to_string())
            })
            .collect();
        assert!(
            all.contains("thinking trace 39"),
            "the last message's reasoning must be visible at the bottom"
        );
    }

    #[tokio::test]
    async fn flag_toggle_while_streaming_and_scrolled_into_tail_stays_pinned() {
        let reasoning: String = (0..60)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "The answer body.\nSecond answer line.");
        // One stored tool call that the Ctrl+T toggle will expand.
        a.messages.push(msg(
            "tool_call",
            r#"{"name":"search","arguments":"{}","result":"line one\nline two"}"#,
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        // Scroll into the middle of the reasoning (inside the streaming tail).
        a.scroll = 8;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the tail, got: {before}"
        );

        // Ctrl+T re-wraps the cache (tool detail expands ABOVE the tail)
        // while the user reads the streaming reply: the tail content must
        // stay put.
        a.show_tool_detail = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport jumped on toggle while streaming: before={before:?} after={after:?}"
        );
    }

    /// The scrollbar gutter column (x = width - 1): (symbol, fg) per row.
    fn gutter_snapshot(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    ) -> Vec<(String, ratatui::style::Color)> {
        let buf = terminal.backend().buffer();
        let x = buf.area.width - 1;
        (0..buf.area.height)
            .map(|y| (buf[(x, y)].symbol().to_string(), buf[(x, y)].fg))
            .collect()
    }

    /// Height of the history pane on an 80x24 terminal with an empty
    /// composer: 24 minus 3 input rows (1 content + 2 border) minus 1 status.
    const HISTORY_H: usize = 20;

    #[test]
    fn scrollbar_thumb_sits_flush_at_bottom_when_following() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "starts following the bottom");
        let gutter = gutter_snapshot(&terminal);
        assert_eq!(
            gutter[HISTORY_H - 1].1,
            a.theme.accent,
            "thumb must sit flush at the track bottom — no dead gap"
        );
        // The rest of the gutter above it is the dim track.
        assert_eq!(gutter[0].1, a.theme.border_dim);
    }

    #[test]
    fn scrollbar_thumb_sits_flush_at_top_when_scrolled_to_start() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = a.max_scroll;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let gutter = gutter_snapshot(&terminal);
        assert_eq!(
            gutter[0].1, a.theme.accent,
            "thumb must sit flush at the track top when scrolled to the start"
        );
    }

    #[test]
    fn scrollbar_thumb_tracks_viewport_proportionally() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total = a.history_cache.lines.len();
        assert!(total > HISTORY_H, "conversation should overflow the pane");
        // Halfway up the conversation: the thumb must sit at the halfway
        // point of its travel range, with the proportional length.
        a.scroll = a.max_scroll / 2;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let top = a.max_scroll - a.scroll;
        let thumb_len = (HISTORY_H * HISTORY_H / total).clamp(1, HISTORY_H);
        let expect_top = top * (HISTORY_H - thumb_len) / (total - HISTORY_H);
        let gutter = gutter_snapshot(&terminal);
        let accent_rows: Vec<usize> = (0..HISTORY_H)
            .filter(|&y| gutter[y].1 == a.theme.accent)
            .collect();
        assert_eq!(
            accent_rows.len(),
            thumb_len,
            "thumb length is the viewport fraction"
        );
        assert_eq!(
            accent_rows.first(),
            Some(&expect_top),
            "thumb top maps to the viewport"
        );
    }

    #[tokio::test]
    async fn stream_end_while_following_bottom_stays_at_bottom() {
        let mut a = streaming_app(
            "some reasoning text here\nmore of it",
            "A short final answer.",
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "follows the bottom while streaming");

        // The reply finishes: the tail moves into the cache as a stored card
        // that renders taller than the streaming tail (reasoning header +
        // phrase footer). Following the bottom must still show the very end
        // of the conversation — not a viewport that stops short of it.
        a.on_chat_event(1, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(
            a.scroll, 0,
            "must keep following the bottom after the stream ends"
        );
        // The pane's bottom row shows the last rendered line of the message
        // (its trailing blank card row) — nothing is cut off below it.
        let buf = terminal.backend().buffer();
        let bottom_row: String = (0..buf.area.width - 1)
            .map(|x| buf[(x, HISTORY_H as u16 - 1)].symbol().to_string())
            .collect();
        assert!(
            bottom_row.trim().is_empty(),
            "the message's trailing blank row must sit at the pane bottom, got: {bottom_row:?}"
        );
    }
}

#[cfg(test)]
mod card_background_tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::{Db, Message};
    use nexus_core::space::Space;

    #[test]
    fn user_card_keeps_terminal_bg_and_stays_right_aligned() {
        let db = Db::open_in_memory().unwrap();
        let space = Space {
            root: std::env::temp_dir().join(format!("nexus-tint-{}", uuid::Uuid::new_v4())),
        };
        let mut app = AppView::new(App::new(db, Some("k"), space));
        app.messages.push(Message {
            role: "user".into(),
            content: "a short note".into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: Some("2026-08-08T10:00:00Z".into()),
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 10)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // The card occupies the right side: find the first "a" of
        // "a short note" and verify that it uses the terminal background.
        let mut body_start = None;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "a" {
                    body_start = Some((x, cell.bg));
                    break;
                }
            }
            if body_start.is_some() {
                break;
            }
        }
        let (body_start, body_bg) = body_start.expect("user message should be rendered");
        assert_eq!(
            body_bg,
            Color::Reset,
            "card should not paint an opaque RGB bg"
        );
        // Right-aligned: the body starts well past the pane's midpoint.
        assert!(
            body_start >= 30,
            "card should be right-aligned, got body start at {body_start}"
        );
    }
}
