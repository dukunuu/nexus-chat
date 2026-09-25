//! Transcript card builders: user bubbles, assistant replies (stored and
//! streaming), tool calls, research/compaction/error rows, session links, and
//! inline images — each pushes wrapped `Line`s for the history pane.

// Casts here are on terminal-bounded values (u16/u32 dims, byte colors,
// glyph counts) — never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use image::GenericImageView;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

use super::{dim, fmt_cost, to_color};
use crate::app_view::AppView;
use nexus_core::db::Message;

/// Remove `![alt](file)` image references from text — the images themselves
/// are rendered inline by `render_markdown_images`, so the raw refs must not
/// also wrap into the body text. Unterminated refs (mid-stream) are kept
/// verbatim.
pub(super) fn strip_markdown_images(content: &str) -> String {
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
/// Its padding follows the configured UI surface background. The `❯ you`
/// header and time sit inside the bubble; images render inside at the bubble's
/// width.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_user_card(
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
    let bg_style = Style::default().bg(theme.raised);

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

    // Emit: left margin (pane bg) + a rail in the user color + content + pad
    // to card width. The rail makes the bubble read as a card even when the
    // raised shade is the terminal's own (transparent) background.
    let lead = width.saturating_sub(card_w);
    let rail = Style::default().fg(theme.user_msg).patch(bg_style);
    for (li, line) in card.into_iter().enumerate() {
        let len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        let is_image = card_img[li].is_some();
        let mut spans = Vec::with_capacity(4);
        if lead > 0 {
            spans.push(Span::raw(" ".repeat(lead)));
        }
        spans.push(Span::styled("▎ ", rail));
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
pub(super) fn push_tool_call(
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
        // Expanded detail gets breathing room; collapsed calls stack into
        // one compact block above the reply they fed.
        out.push(Line::from(""));
    }
}

/// A compaction-digest block: the digest of the earlier conversation, shown
/// right at the compaction boundary in the transcript — what was folded
/// away is visible in the chat itself, not only behind the context popup's
/// editor. Header in accent2, digest body dimmed so the live conversation
/// stays prominent.
pub(super) fn push_compaction(
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

/// A transient block shown in the transcript while a compaction request is
/// running. It is deliberately not a `Message`: failed or cancelled jobs must
/// disappear without leaving a fake conversation turn in the database.
pub(super) fn push_compaction_pending(
    out: &mut Vec<Line<'static>>,
    width: usize,
    theme: &crate::theme::Theme,
) {
    out.push(Line::from(vec![
        Span::styled("⟳ ", Style::default().fg(theme.accent2)),
        Span::styled(
            "compacting earlier messages…",
            Style::default()
                .fg(theme.accent2)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    for line in wrap_plain(
        "building a conversation digest — please wait",
        width.saturating_sub(2),
    ) {
        out.push(Line::from(dim(format!("  {line}"), theme)));
    }
    out.push(Line::from(""));
}

/// A background-research progress line: a dim one-liner with a 🔎 marker,
/// no expand/collapse (unlike `tool_call` — there's no arguments/result pair,
/// just a phase label).
pub(super) fn push_research_stage(
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
pub(super) fn push_error(
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
pub(super) fn push_survey_section(
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
pub(super) fn push_research_plan(
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
pub(super) fn push_assistant_stored(
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
            head.push(dim(
                format!(" · {}", crate::ui::short_model_label(m)),
                theme,
            ));
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
                crate::ui::short_model_label(m),
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

    // A quiet rail for stored replies; only the live reply's rail is bright.
    let rail = Span::styled("▎ ", Style::default().fg(theme.border_dim));
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

    let mut rendered = crate::ui::markdown::render(
        &strip_markdown_images(content),
        // Rail (2) plus a one-column margin before the scrollbar gutter.
        width.saturating_sub(3),
        md_colors(theme),
    );
    rendered.lines = crate::ui::citations_style::style_citations(rendered.lines, theme.accent);
    rendered.lines = crate::ui::citations_style::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered, Some(rail));

    // Footer below the response: the phrase, then stats, one separator style.
    let mut stats: Vec<String> = Vec::new();
    if settings.show_stats
        && let (Some(tok), Some(secs)) = (msg.tokens, msg.secs)
    {
        let tps = if secs > 0.0 { tok as f64 / secs } else { 0.0 };
        stats.push(format!("{tps:.1} tok/s · ~{tok} tok · {secs:.1}s"));
    }
    if settings.show_stats
        && let Some(cost) = msg.cost.filter(|c| *c > 0.0)
    {
        stats.push(fmt_cost(Some(cost)));
    }
    let mut footer: Vec<Span> = Vec::new();
    if let Some(p) = &msg.phrase {
        footer.push(Span::styled(
            p.clone(),
            Style::default()
                .fg(theme.fg_dim)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    if !stats.is_empty() {
        let sep = if footer.is_empty() { "" } else { " · " };
        footer.push(dim(format!("{sep}{}", stats.join(" · ")), theme));
    }
    if !footer.is_empty() {
        footer.insert(0, dim("· ", theme));
        out.push(Line::from(footer));
    }
    out.push(Line::from(""));
}

/// A session switch link: renders as a styled box with arrows and the
/// linked session's name. Content format: `<target_sid>\n<label>`.
pub(super) fn push_session_link(
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

/// The in-progress reply: a `⠹ <model> — <phrase>` header in the spinner
/// color, the thinking block when present, then the live markdown stream.
pub(super) fn push_assistant_streaming(
    out: &mut Vec<Line<'static>>,
    app: &AppView,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
) {
    let color = to_color(app.spinner_color());
    let name = app.active_chat_task().map_or_else(
        || "assistant".to_string(),
        |t| crate::ui::short_model_label(&t.model),
    );
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
    let mut rendered = crate::ui::markdown::render(
        &strip_markdown_images(buf),
        width.saturating_sub(3),
        md_colors(&app.theme),
    );
    rendered.lines = crate::ui::citations_style::style_citations(rendered.lines, app.theme.accent);
    rendered.lines = crate::ui::citations_style::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered, Some(rail));
    out.push(Line::from(""));
}

/// Splice a `markdown::Rendered` into the running line/code/block vecs, keeping
/// `code` aligned to `out` and offsetting local block ids to global ones. When
/// `rail` is given it prefixes every body line (the assistant left gutter).
pub(super) fn push_rendered(
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
pub(super) fn render_markdown_images(
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
pub(super) fn image_to_halfblock_lines(path: &str, max_width: usize) -> Vec<Line<'static>> {
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

pub(super) fn wrap_plain(content: &str, width: usize) -> Vec<String> {
    let w = width.max(1);
    let mut out = Vec::new();
    let content = crate::ui::markdown::terminal_safe(content);
    for raw in content.split('\n') {
        for piece in textwrap::wrap(raw, w) {
            out.push(piece.into_owned());
        }
    }
    out
}

/// The markdown palette for the transcript, from the theme.
fn md_colors(theme: &crate::theme::Theme) -> crate::ui::markdown::MdColors {
    crate::ui::markdown::MdColors {
        heading: theme.accent,
        rule: theme.border_dim,
        code: theme.warning,
    }
}
