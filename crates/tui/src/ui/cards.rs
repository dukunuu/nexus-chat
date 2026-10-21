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
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

use super::style::{self, glyph};
use super::{dim, fmt_cost, to_color};
use crate::app_view::AppView;
use nexus_core::db::Message;

/// Non-text regions travel together through the card renderer.
pub(super) struct Regions<'a> {
    pub diagrams: &'a mut Vec<crate::diagrams::Placement>,
    pub widgets: &'a mut Vec<crate::interactive::Placement>,
}

/// Remove `![alt](file)` image references from text — the images themselves
/// are rendered inline by `render_markdown_images`, so the raw refs must not
/// also wrap into the body text. Unterminated refs (mid-stream) are kept
/// verbatim.
pub(super) fn strip_markdown_images(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for (range, _, _) in markdown_images(content) {
        out.push_str(&content[cursor..range.start]);
        cursor = range.end;
    }
    out.push_str(&content[cursor..]);
    out
}

/// Actual Markdown images only, never image-looking text inside a code fence.
fn markdown_images(content: &str) -> Vec<(std::ops::Range<usize>, String, String)> {
    let mut images = Vec::new();
    let mut current = None;
    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                current = Some((range, dest_url.into_string(), String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, alt)) = &mut current {
                    alt.push_str(&text);
                }
            }
            Event::End(TagEnd::Image) => {
                if let Some(image) = current.take() {
                    images.push(image);
                }
            }
            _ => {}
        }
    }
    images
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
        format!("{} you", glyph::YOU),
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
        spans.push(Span::styled(format!("{} ", glyph::RAIL), rail));
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

/// A foldable tool summary with type-specific detail behind a colored rail.
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
    push_round_preamble(
        out,
        &[field("reasoning_content"), field("thinking")],
        &field("assistant_content"),
        settings,
        width,
        theme,
    );
    let view = nexus_core::app::tool_call_view(&name, &args, &result);
    push_workspace_call(out, &view, expanded, settings, width, theme);
    if expanded {
        let args = serde_json::from_str(&args).unwrap_or_default();
        let body_width = width.saturating_sub(2).max(1);
        let body = expanded_tool_body(&name, &args, &view, &result, body_width, theme);
        let mut body = body
            .into_iter()
            .flat_map(|row| {
                super::diff::styled_rows(&row, body_width, Style::default().fg(theme.fg_dim))
            })
            .collect();
        cap_tool_body(&mut body, body_width, theme);
        let rail = Span::styled(
            format!("{} ", glyph::RAIL),
            Style::default().fg(if view.outcome == nexus_core::app::ToolOutcome::Ok {
                theme.accent2
            } else {
                theme.error
            }),
        );
        for mut row in body {
            row.spans.insert(0, rail.clone());
            out.push(row);
        }
        out.push(Line::default());
    }
}

/// What the model thought and said before a round of tool calls, drawn
/// above the round's first call so a tool loop reads in order: thinking,
/// calls, thinking, calls, answer. Reasoning folds like a reply's (Ctrl+R).
fn push_round_preamble(
    out: &mut Vec<Line<'static>>,
    reasoning: &[String],
    said: &str,
    settings: &nexus_core::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let reasoning = reasoning
        .iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty());
    if let Some(reasoning) = reasoning {
        // The same rail as a reply's reasoning, so thinking that streamed
        // live keeps its look when its round's calls land.
        let rail = Span::styled(
            format!("{} ", glyph::RAIL),
            Style::default().fg(theme.border_dim),
        );
        if settings.show_reasoning {
            out.push(Line::from(vec![
                rail.clone(),
                dim(format!("{} reasoning", glyph::EXPANDED), theme),
            ]));
            for line in wrap_plain(reasoning, width.saturating_sub(2)) {
                out.push(Line::from(vec![rail.clone(), dim(line, theme)]));
            }
        } else {
            let hint = if settings.hide_hints {
                ""
            } else {
                " — Ctrl+R to expand"
            };
            out.push(Line::from(vec![
                rail,
                dim(
                    format!(
                        "{} reasoning ({} chars){hint}",
                        glyph::SELECTED,
                        reasoning.chars().count()
                    ),
                    theme,
                ),
            ]));
        }
    }
    let said = said.trim();
    if !said.is_empty() {
        for line in wrap_plain(said, width) {
            out.push(Line::from(Span::styled(
                line,
                Style::default().fg(theme.fg),
            )));
        }
    }
}

fn push_workspace_call(
    out: &mut Vec<Line<'static>>,
    view: &nexus_core::app::ToolView,
    expanded: bool,
    settings: &nexus_core::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
) {
    use nexus_core::app::ToolOutcome;
    let failed = view.outcome != ToolOutcome::Ok;
    let dim = Style::default().fg(theme.fg_dim);
    let mut head = vec![Span::styled(
        format!("{} ", call_glyph(view, failed)),
        Style::default().fg(if failed { theme.error } else { theme.accent2 }),
    )];
    let mut append = |text: String, style: Style| {
        for line in super::diff::escaped_rows(&text, usize::MAX, style, dim) {
            head.extend(line.spans);
        }
    };
    append(
        format!("{} {}", view.verb, view.target)
            .trim_start()
            .to_string(),
        if view.verb.is_empty() {
            dim
        } else {
            Style::default().fg(theme.fg)
        },
    );
    if view.verb.is_empty() && !expanded && !settings.hide_hints {
        append(" — Ctrl+T for detail".to_string(), dim);
    }
    if let Some(added) = view.added {
        append(format!("  +{added}"), Style::default().fg(theme.success));
    }
    if let Some(deleted) = view.deleted {
        append(format!(" −{deleted}"), Style::default().fg(theme.error));
    }
    for stat in &view.stats {
        append(
            format!("{}{stat}", style::SEP),
            if failed
                && (stat.starts_with("exit")
                    || matches!(stat.as_str(), "failed" | "denied" | "blocked"))
            {
                Style::default().fg(theme.error)
            } else {
                dim
            },
        );
    }
    let mut header = Line::from(head);
    if header.width() + 2 <= width {
        header
            .spans
            .push(Span::raw(" ".repeat(width - header.width() - 1)));
        header.spans.push(Span::styled(
            if expanded {
                glyph::EXPANDED
            } else {
                glyph::SELECTED
            },
            dim,
        ));
    }
    out.push(header);
    if expanded {
        return;
    }
    let body_width = width.saturating_sub(3);
    let body = if let Some(diff) = &view.diff {
        super::diff::render(
            diff,
            body_width,
            theme,
            super::diff::Options {
                max_rows: Some(8),
                context: 1,
                hint: if settings.hide_hints {
                    ""
                } else {
                    " · Ctrl+T"
                },
            },
        )
    } else if let Some(output) = &view.output {
        let rows: Vec<_> = output
            .split('\n')
            .flat_map(|line| super::diff::escaped_rows(line, body_width, dim, dim))
            .collect();
        if view.verb == "$" && failed {
            let skip = rows.len().saturating_sub(5);
            rows.into_iter().skip(skip).collect()
        } else {
            Vec::new()
        }
    } else if !view.sources.is_empty() {
        source_domains(view, body_width, theme)
    } else {
        Vec::new()
    };
    for mut row in body {
        row.spans.insert(0, Span::raw("   "));
        out.push(row);
    }
}

/// ✗ for a failed call, ◎ for web tools, ⚒ for everything else.
fn call_glyph(view: &nexus_core::app::ToolView, failed: bool) -> &'static str {
    if failed {
        glyph::FAIL
    } else if is_web(view) {
        glyph::WEB
    } else {
        glyph::TOOL
    }
}

/// Web tools: searches of every mode and page fetches.
fn is_web(view: &nexus_core::app::ToolView) -> bool {
    matches!(
        view.verb.as_str(),
        "search" | "news" | "papers" | "discussions" | "fetch"
    )
}

/// Collapsed search card: the distinct source domains, in result order.
fn source_domains(
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut domains: Vec<&str> = Vec::new();
    for source in &view.sources {
        if !source.domain.is_empty() && !domains.contains(&source.domain.as_str()) {
            domains.push(&source.domain);
        }
    }
    let shown = domains.len().min(4);
    let mut text = domains[..shown].join(style::SEP);
    if domains.len() > shown {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("{}+{}", style::SEP, domains.len() - shown),
        );
    }
    let dim = Style::default().fg(theme.fg_dim);
    let mut rows = super::diff::escaped_rows(&text, usize::MAX, dim, dim);
    rows.truncate(1);
    if let Some(row) = rows.first_mut() {
        *row = super::popups::chrome::fit_line(row.clone(), width);
    }
    rows
}

/// Opened search card: each hit's title, domain and the start of its text.
fn sources_body(
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let mut rows = tool_rule("results", width, theme);
    let text_width = width.saturating_sub(5).max(1);
    for (index, source) in view.sources.iter().enumerate() {
        let number = format!("{:>3}  ", index + 1);
        let pad = " ".repeat(number.chars().count());
        for (row, line) in wrap_plain(&source.title, text_width)
            .into_iter()
            .enumerate()
        {
            let lead = if row == 0 {
                number.clone()
            } else {
                pad.clone()
            };
            let mut spans = vec![Span::styled(lead, dim)];
            for part in
                super::diff::escaped_rows(&line, usize::MAX, Style::default().fg(theme.fg), dim)
            {
                spans.extend(part.spans);
            }
            rows.push(Line::from(spans));
        }
        if !source.domain.is_empty() {
            let mut spans = vec![Span::raw(pad.clone())];
            for part in super::diff::escaped_rows(
                &source.domain,
                usize::MAX,
                Style::default().fg(theme.accent2),
                dim,
            ) {
                spans.extend(part.spans);
            }
            rows.push(Line::from(spans));
        }
        for line in wrap_plain(&source.snippet, text_width).into_iter().take(2) {
            let mut spans = vec![Span::raw(pad.clone())];
            for part in super::diff::escaped_rows(&line, usize::MAX, dim, dim) {
                spans.extend(part.spans);
            }
            rows.push(Line::from(spans));
        }
        if index + 1 < view.sources.len() {
            rows.push(Line::default());
        }
    }
    rows
}

fn tool_rule(label: &str, width: usize, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let rule = Style::default().fg(theme.border_dim);
    let dim = Style::default().fg(theme.fg_dim);
    let mut rows = super::diff::escaped_rows(&format!("── {label} ──"), width, rule, dim);
    if let Some(last) = rows.last_mut() {
        last.spans.push(Span::styled(
            "─".repeat(width.saturating_sub(last.width())),
            rule,
        ));
    }
    rows
}

#[allow(clippy::too_many_lines)] // one dispatch for the emitted tool formats
fn expanded_tool_body(
    name: &str,
    args: &serde_json::Value,
    view: &nexus_core::app::ToolView,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    if let Some(diff) = &view.diff {
        let mut rows = tool_rule("diff", width, theme);
        rows.extend(super::diff::expanded(diff, width, theme));
        return rows;
    }
    if !view.sources.is_empty() {
        return sources_body(view, width, theme);
    }
    let tool = name.split('_').next().unwrap_or(name);
    if matches!(tool, "shell" | "proc") && !view.verb.is_empty() {
        return command_body(args, view, result, width, theme);
    }
    if tool == "fs" && view.verb == "read" {
        return read_body(args["path"].as_str().unwrap_or(""), result, width, theme);
    }
    if tool == "fs" && view.verb == "grep" {
        return grep_body(args["pattern"].as_str().unwrap_or(""), result, width, theme);
    }
    if tool == "fs" && matches!(view.verb.as_str(), "glob" | "list") {
        return result
            .split('\n')
            .flat_map(|line| {
                let style = if line.ends_with('/') {
                    Style::default().fg(theme.accent)
                } else {
                    dim
                };
                super::diff::escaped_rows(line, width, style, dim)
            })
            .collect();
    }
    if name == "task" {
        let report = if result.starts_with("sub-agent report (") {
            result.split_once('\n').map_or("", |(_, report)| report)
        } else {
            result
        };
        return tool_markdown(report, width, theme);
    }
    let mut rows = Vec::new();
    if let Some(args) = args.as_object() {
        let keys: Vec<_> = args
            .keys()
            .map(|key| super::diff::escaped_rows(key, usize::MAX, dim, dim).remove(0))
            .collect();
        let key_width = keys.iter().map(Line::width).max().unwrap_or(0);
        for ((_, value), mut key) in args.iter().zip(keys) {
            let padding = key_width.saturating_sub(key.width()) + 2;
            key.spans.push(Span::raw(" ".repeat(padding)));
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            key.spans
                .push(Span::styled(value, Style::default().fg(theme.fg)));
            rows.extend(super::diff::styled_rows(&key, width, dim));
        }
    }
    rows.extend(tool_rule("result", width, theme));
    rows.extend(
        result
            .split('\n')
            .flat_map(|line| super::diff::escaped_rows(line, width, dim, dim)),
    );
    rows
}

fn command_body(
    args: &serde_json::Value,
    view: &nexus_core::app::ToolView,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let command = args["cmd"].as_str().map_or_else(
        || format!("{} {}", view.verb, view.target),
        |cmd| format!("$ {cmd}"),
    );
    let mut rows = super::diff::escaped_rows(
        &command,
        width,
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        dim,
    );
    if let Some(cwd) = args["cwd"].as_str() {
        let dir = super::diff::escaped_rows(&format!("in {cwd}"), width, dim, dim);
        let last = rows.last_mut().expect("command has a row");
        if dir.len() == 1 && last.width() + dir[0].width() < width {
            last.spans
                .push(Span::raw(" ".repeat(width - last.width() - dir[0].width())));
            last.spans.extend(dir[0].spans.clone());
        } else {
            rows.extend(dir);
        }
    }
    let output = view
        .output
        .as_deref()
        .unwrap_or(result)
        .trim_end_matches('\n');
    let count = if output.is_empty() {
        0
    } else {
        output.split('\n').count()
    };
    rows.extend(tool_rule(
        &format!("output{}{} lines", style::SEP, count),
        width,
        theme,
    ));
    if count > 0 {
        for line in output.split('\n') {
            let start = line.trim_start();
            let color = if ["error", "fatal", "FAILED", "panicked"]
                .iter()
                .any(|s| start.starts_with(s))
            {
                theme.error
            } else if start.starts_with("warning") {
                theme.warning
            } else {
                theme.fg_dim
            };
            rows.extend(super::diff::escaped_rows(
                line,
                width,
                Style::default().fg(color),
                dim,
            ));
        }
    }
    rows
}

fn grep_body(
    pattern: &str,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let matched = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut rows = Vec::new();
    for line in result.trim_end().split('\n') {
        if !line.starts_with(' ') && line.ends_with(':') {
            rows.extend(super::diff::escaped_rows(
                &line[..line.len() - 1],
                width,
                Style::default().fg(theme.accent),
                dim,
            ));
        } else if let Some((number, text)) = line.trim_start().split_once(": ")
            && number.parse::<usize>().is_ok()
        {
            let mut spans = vec![Span::styled(format!("{number:>4}  "), dim)];
            let mut cursor = 0;
            if !pattern.is_empty() {
                for (index, found) in text.match_indices(pattern) {
                    spans.push(Span::styled(text[cursor..index].to_string(), dim));
                    spans.push(Span::styled(found.to_string(), matched));
                    cursor = index + found.len();
                }
            }
            spans.push(Span::styled(text[cursor..].to_string(), dim));
            rows.extend(super::diff::styled_rows(&Line::from(spans), width, dim));
        } else {
            rows.extend(super::diff::escaped_rows(line, width, dim, dim));
        }
    }
    rows
}

fn read_body(
    path: &str,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");
    let lang = match extension {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "sh" | "bash" => "bash",
        "json" | "toml" | "yaml" | "yml" | "html" | "css" | "c" | "cpp" | "go" | "java" | "rb"
        | "sql" | "md" => extension,
        _ => "plain",
    };
    let mut rows = tool_rule(&format!("{path} ── {lang}"), width, theme);
    if !result
        .split('\n')
        .next()
        .unwrap_or_default()
        .contains(" (lines ")
    {
        let dim = Style::default().fg(theme.fg_dim);
        rows.extend(
            result
                .split('\n')
                .flat_map(|line| super::diff::escaped_rows(line, width, dim, dim)),
        );
        return rows;
    }
    let entries: Vec<_> = result
        .split('\n')
        .skip(1)
        .map(|line| {
            if let Some((number, text)) = line.trim_start().split_once(':')
                && number.parse::<usize>().is_ok()
            {
                let text = text
                    .split_once('\t')
                    .filter(|(hash, _)| {
                        hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit())
                    })
                    .map_or_else(|| text.strip_prefix(' ').unwrap_or(text), |(_, text)| text);
                (number, text)
            } else {
                ("", line)
            }
        })
        .collect();
    let source = entries
        .iter()
        .map(|(_, text)| *text)
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted = highlight_read(&source, lang, theme);
    let digits = entries.iter().map(|(n, _)| n.len()).max().unwrap_or(1);
    let dim = Style::default().fg(theme.fg_dim);
    for ((number, _), mut source) in entries.iter().zip(highlighted) {
        dim_tool_escapes(std::slice::from_mut(&mut source), dim, theme.border_dim);
        for (index, mut line) in
            super::diff::styled_rows(&source, width.saturating_sub(digits + 2), dim)
                .into_iter()
                .enumerate()
        {
            line.spans.insert(
                0,
                Span::styled(
                    if index == 0 {
                        format!("{number:>digits$}: ")
                    } else {
                        " ".repeat(digits + 2)
                    },
                    dim,
                ),
            );
            rows.extend(super::diff::styled_rows(&line, width, dim));
        }
    }
    rows
}

/// Highlight the whole file slice before adding gutters or wrapping its rows.
fn highlight_read(source: &str, lang: &str, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let safe = markdown_tool_text(source);
    if lang == "plain" {
        return safe
            .split('\n')
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.fg),
                ))
            })
            .collect();
    }
    let code_width = safe
        .split('\n')
        .map(unicode_width::UnicodeWidthStr::width)
        .max()
        .unwrap_or(1)
        .max(1);
    // A longer fence cannot be closed by backticks in the file itself.
    let fence = "`".repeat(
        safe.split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(3)
            + 1,
    );
    let rendered = crate::ui::markdown::render(
        &format!("{fence}{lang}\n{safe}\n{fence}"),
        code_width + 4,
        md_colors(theme),
    );
    // The existing fenced renderer supplies syntax spans inside its box.
    rendered
        .lines
        .into_iter()
        .skip(1)
        .take(safe.split('\n').count())
        .map(|line| {
            let count = line.spans.len().saturating_sub(2);
            Line::from(
                line.spans
                    .into_iter()
                    .skip(1)
                    .take(count)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Sanitize before Markdown parsing, which can otherwise discard controls.
fn markdown_tool_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' {
                "    ".to_string()
            } else if c.is_control() && c != '\n' {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Highlighting and wrapping may split escapes across spans or rows. Ignore
/// Markdown box/table chrome when locating their text, then dim every fragment.
fn dim_tool_escapes(lines: &mut [Line<'static>], dim: Style, rule: ratatui::style::Color) {
    let chrome = |span: &Span<'_>| {
        span.style.fg == Some(rule) && span.content.chars().all(|c| " │┌┐└┘─├┤┬┴┼".contains(c))
    };
    let text: String = lines
        .iter()
        .flat_map(|line| line.spans.iter().filter(|span| !chrome(span)))
        .map(|span| span.content.as_ref())
        .collect();
    let ranges: Vec<_> = text
        .match_indices("\\u{")
        .filter_map(|(start, _)| {
            let end = text[start + 3..].find('}')? + start + 3;
            text[start + 3..end]
                .chars()
                .all(|c| c.is_ascii_hexdigit())
                .then_some(start..end + 1)
        })
        .collect();
    if ranges.is_empty() {
        return;
    }
    let mut offset = 0;
    for line in lines {
        let mut spans = Vec::new();
        for span in std::mem::take(&mut line.spans) {
            if chrome(&span) {
                spans.push(span);
                continue;
            }
            for c in span.content.chars() {
                let style = if ranges.iter().any(|range| range.contains(&offset)) {
                    dim
                } else {
                    line.style.patch(span.style)
                };
                spans.push(Span::styled(c.to_string(), style));
                offset += c.len_utf8();
            }
        }
        line.spans = spans;
    }
}

fn tool_markdown(text: &str, width: usize, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let mut lines =
        crate::ui::markdown::render(&markdown_tool_text(text), width, md_colors(theme)).lines;
    dim_tool_escapes(&mut lines, dim, theme.border_dim);
    lines
        .into_iter()
        .flat_map(|line| super::diff::styled_rows(&line, width, dim))
        .collect()
}

fn cap_tool_body(rows: &mut Vec<Line<'static>>, width: usize, theme: &crate::theme::Theme) {
    const MAX_ROWS: usize = 400;
    if rows.len() > MAX_ROWS {
        let total = rows.len();
        let dim = Style::default().fg(theme.fg_dim);
        let mut keep = MAX_ROWS - 1;
        loop {
            let notice = super::diff::escaped_rows(
                &format!("… {} more lines", total - keep),
                width,
                dim,
                dim,
            );
            if keep + notice.len() <= MAX_ROWS {
                rows.truncate(keep);
                rows.extend(notice);
                break;
            }
            keep = MAX_ROWS.saturating_sub(notice.len());
        }
    }
}

/// A compaction digest at the compaction boundary: what was folded away is
/// visible in the transcript itself, dimmed so the live conversation leads.
pub(super) fn push_compaction(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let agent = Style::default().fg(theme.accent2);
    out.push(style::event_head(
        glyph::DIGEST,
        agent,
        "earlier messages, summarized".into(),
        agent.add_modifier(Modifier::BOLD),
    ));
    for line in wrap_plain(content, width.saturating_sub(2)) {
        out.push(style::event_body(&line, Style::default().fg(theme.fg_dim)));
    }
    out.push(Line::from(""));
}

/// A user-invoked skill: show its name and scope, never its instruction body.
pub(super) fn push_skill(
    out: &mut Vec<Line<'static>>,
    content: &str,
    _width: usize,
    theme: &crate::theme::Theme,
) {
    let name = content.lines().next().unwrap_or_default();
    let you = Style::default().fg(theme.accent);
    let label = format!("skill {name}{}active for this session", style::SEP);
    out.push(style::event_head(glyph::TOOL, you, label, you));
    out.push(Line::from(""));
}

/// A command the user ran with `!`: the command and how it ended, then the
/// head of its output (the full output stays in the row).
pub(super) fn push_user_command(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    const MAX_OUTPUT_LINES: usize = 12;
    let row: serde_json::Value = serde_json::from_str(content).unwrap_or_default();
    let cmd = row["cmd"].as_str().unwrap_or_default();
    let outcome = nexus_core::app::user_command_outcome(&row);
    let you = Style::default().fg(theme.accent);
    out.push(style::event_head(
        glyph::YOU,
        you,
        format!("$ {cmd}{}{outcome}", style::SEP),
        you,
    ));
    let output = row["output"].as_str().unwrap_or_default().trim_end();
    if !output.is_empty() {
        let meta = Style::default().fg(theme.fg_dim);
        let lines = wrap_plain(output, width.saturating_sub(2));
        for line in lines.iter().take(MAX_OUTPUT_LINES) {
            out.push(style::event_body(line, meta));
        }
        if lines.len() > MAX_OUTPUT_LINES {
            out.push(style::event_body(
                &format!("… {} more lines", lines.len() - MAX_OUTPUT_LINES),
                meta,
            ));
        }
    }
    out.push(Line::from(""));
}

/// A transient row while a compaction request runs. Deliberately not a
/// `Message`: failed or cancelled jobs must vanish without leaving a fake
/// conversation turn in the database.
pub(super) fn push_compaction_pending(
    out: &mut Vec<Line<'static>>,
    _width: usize,
    theme: &crate::theme::Theme,
) {
    let agent = Style::default().fg(theme.accent2);
    out.push(style::event_head(
        glyph::RUNNING,
        agent,
        "summarizing earlier messages…".into(),
        agent.add_modifier(Modifier::BOLD),
    ));
    out.push(Line::from(""));
}

/// A research progress row: the stage and its detail, dimmed.
pub(super) fn push_research_stage(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let meta = Style::default().fg(theme.fg_dim);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_default();
    out.push(style::event_head(
        glyph::RESEARCH,
        Style::default().fg(theme.accent2),
        head,
        meta,
    ));
    for line in lines {
        out.push(style::event_body(&line, meta));
    }
    out.push(Line::from(""));
}

/// A persistent request failure, kept in the transcript after the status bar
/// moves on.
pub(super) fn push_error(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let err = Style::default().fg(theme.error);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_else(|| "request failed".into());
    out.push(style::event_head(
        glyph::FAIL,
        err.add_modifier(Modifier::BOLD),
        head,
        err,
    ));
    for line in lines {
        out.push(style::event_body(&line, err));
    }
    out.push(Line::from(""));
}

/// Clarifying questions waiting on your chat answer: a question round in the
/// interactive accent, questions in full text, the guidance footer dimmed.
pub(super) fn push_survey_section(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let you = Style::default().fg(theme.accent);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_default();
    out.push(style::event_head(
        glyph::QUESTION,
        you.add_modifier(Modifier::BOLD),
        head,
        you.add_modifier(Modifier::BOLD),
    ));
    // The trailing guidance ("Answer in chat…") stays dim across its lines.
    let mut footer = false;
    for line in lines {
        footer |= line.starts_with("Answer in chat");
        let style = if footer {
            Style::default().fg(theme.fg_dim)
        } else {
            Style::default().fg(theme.fg)
        };
        out.push(style::event_body(&line, style));
    }
    out.push(Line::from(""));
}

/// A plan waiting on your approval: same shape as a question round — it's
/// actionable, so it takes the interactive accent.
pub(super) fn push_research_plan(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let you = Style::default().fg(theme.accent);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_default();
    out.push(style::event_head(
        glyph::PLAN,
        you.add_modifier(Modifier::BOLD),
        head,
        you.add_modifier(Modifier::BOLD),
    ));
    for line in lines {
        out.push(style::event_body(&line, Style::default().fg(theme.fg)));
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
    regions: Regions<'_>,
) {
    // Header: the assistant glyph + who answered (persona overrides the
    // model name; personas are agents, so they take the agent color).
    let mut head = vec![Span::styled(
        format!("{} ", glyph::ASSISTANT),
        Style::default()
            .fg(theme.accent2)
            .add_modifier(Modifier::BOLD),
    )];
    match (&msg.persona, msg.model.as_deref()) {
        (Some(p), Some(m)) => {
            head.push(Span::styled(
                p.clone(),
                Style::default()
                    .fg(theme.accent2)
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
                    .fg(theme.accent2)
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
    let rail = Span::styled(
        format!("{} ", glyph::RAIL),
        Style::default().fg(theme.border_dim),
    );
    if let Some(r) = &msg.reasoning {
        if settings.show_reasoning {
            out.push(Line::from(vec![
                rail.clone(),
                dim(format!("{} reasoning", glyph::EXPANDED), theme),
            ]));
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
                dim(
                    format!("{} reasoning ({n} chars){hint}", glyph::SELECTED),
                    theme,
                ),
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
    push_rendered(out, code, blocks, rendered, Some(rail), regions);

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

/// A link to another session: the target in the interactive accent, how to
/// follow it underneath. Content format: `<target_sid>\n<label>`.
pub(super) fn push_session_link(
    out: &mut Vec<Line<'static>>,
    content: &str,
    _width: usize,
    theme: &crate::theme::Theme,
) {
    let (sid, label) = match content.split_once('\n') {
        Some((sid, rest)) => (sid.to_string(), rest.trim().to_string()),
        None => (String::new(), content.to_string()),
    };
    // Older link rows lead with an emoji; the glyph replaces it.
    let label = label
        .trim_start_matches(['\u{1F517}', '↩', ' '])
        .to_string();
    let you = Style::default().fg(theme.accent);
    out.push(style::event_head(glyph::LINK, you, label, you));
    if !sid.is_empty() {
        out.push(style::event_body(
            "select this line + Ctrl+O to open",
            Style::default().fg(theme.fg_dim),
        ));
    }
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
    regions: Regions<'_>,
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

    let rail = Span::styled(format!("{} ", glyph::RAIL), Style::default().fg(color));
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
    push_rendered(out, code, blocks, rendered, Some(rail), regions);
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
    regions: Regions<'_>,
) {
    let Regions { diagrams, widgets } = regions;
    widgets.extend(r.widgets.into_iter().map(|mut placement| {
        placement.row += out.len();
        placement
    }));
    diagrams.extend(r.diagrams.into_iter().map(|mut placement| {
        placement.row += out.len();
        placement
    }));
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
    image_at_line.resize(out.len(), None);
    for (_, file, alt) in markdown_images(content) {
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
            let mut line = Line::from(dim(format!("{} {alt}", glyph::MEDIA), theme));
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
}

/// Max cell-rows a rendered image occupies (click-to-open encourages viewing
/// full size in an external viewer instead of eating the whole terminal).
const MAX_IMAGE_ROWS: usize = 20;

/// Render a PNG image as half-block ratatui lines for inline display in the
/// terminal. Falls back to a text marker if the image can't be loaded.
/// When the image is taller than `MAX_IMAGE_ROWS`, the last line says "▣ click to open"
/// so the user knows to click to open the full version.
pub(super) fn image_to_halfblock_lines(path: &str, max_width: usize) -> Vec<Line<'static>> {
    let Ok(img) = image::open(path) else {
        return vec![Line::from(Span::raw(format!("{} [image]", glyph::MEDIA)))];
    };
    if max_width < 4 {
        return vec![Line::from(Span::raw(glyph::MEDIA))];
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
            format!("{} click to open in viewer", glyph::MEDIA),
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
        // List items hang: continuation lines sit under the item's text,
        // not back at the margin.
        let hang = " ".repeat(list_hang(raw));
        let opts = textwrap::Options::new(w).subsequent_indent(&hang);
        for piece in textwrap::wrap(raw, opts) {
            out.push(piece.into_owned());
        }
    }
    out
}

/// Columns before a list item's text — indent plus a `1.`/`1)`/`-`/`*`/`+`/`•`
/// marker and its space — or the plain indent for a non-list line.
fn list_hang(line: &str) -> usize {
    let body = line.trim_start();
    let indent = line.len() - body.len();
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let marker = if digits > 0 && matches!(body[digits..].chars().next(), Some('.' | ')')) {
        digits + 1
    } else if body.starts_with(['-', '*', '+', '•']) {
        body.chars().next().map_or(0, char::len_utf8)
    } else {
        0
    };
    if marker > 0 && body[marker..].starts_with(' ') {
        // `•` is multi-byte but one column wide.
        indent + body[..marker].chars().count() + 1
    } else {
        indent
    }
}

/// The markdown palette for the transcript, from the theme.
fn md_colors(theme: &crate::theme::Theme) -> crate::ui::markdown::MdColors {
    crate::ui::markdown::MdColors {
        heading: theme.accent,
        rule: theme.border_dim,
        code: theme.warning,
    }
}

#[cfg(test)]
mod tool_card_tests {
    use super::*;
    use nexus_core::app::Settings;
    use serde_json::json;

    fn card(
        name: &str,
        args: serde_json::Value,
        result: &str,
        expanded: bool,
        hide_hints: bool,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let settings = Settings {
            hide_hints,
            ..Settings::default()
        };
        push_tool_call(
            &mut lines,
            &json!({"name":name,"arguments":args.to_string(),"result":result}).to_string(),
            expanded,
            &settings,
            80,
            &crate::theme::Theme::default(),
        );
        lines
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn expanded_diff_tints_full_rows_but_not_previews() {
        let result = "edited a (+1 −1)\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\x1b";
        let args = json!({"action":"edit","path":"a"});
        let theme = crate::theme::Theme::default();
        let expanded = card("fs", args.clone(), result, true, false);
        for (needle, bg) in [("− old", theme.diff_del_bg), ("+ new", theme.diff_add_bg)] {
            let row = expanded
                .iter()
                .find(|row| row.to_string().contains(needle))
                .unwrap();
            assert_eq!(row.width(), 80);
            assert_eq!(row.style.bg, Some(bg));
            assert_eq!(row.spans[0].style.fg, Some(theme.accent2));
            assert!(row.spans[1..].iter().all(|s| s.style.bg == Some(bg)));
        }
        assert!(card("fs", args, result, false, false).iter().all(|row| row.style.bg.is_none() && row.spans.iter().all(|s| s.style.bg.is_none())));
    }

    #[test]
    fn expanded_commands_color_output_and_align_cwd() {
        let theme = crate::theme::Theme::default();
        let lines = card(
            "shell",
            json!({"action":"run","cmd":"cargo build","cwd":"sub/dir"}),
            "exit 101 · 9.8s\nnormal\n  error[E0425]: missing\nwarning: unused\nfatal: broken\nFAILED test\npanicked here",
            true,
            false,
        );
        assert!(lines[1].to_string().starts_with("▎ $ cargo build"));
        assert!(lines[1].to_string().ends_with("in sub/dir"));
        assert_eq!(lines[1].width(), 80);
        assert!(lines[2].to_string().contains("output · 6 lines"));
        for line in &lines[1..lines.len() - 1] {
            assert_eq!(line.spans[0].style.fg, Some(theme.error));
        }
        for (needle, color) in [
            ("normal", theme.fg_dim),
            ("error[E", theme.error),
            ("warning:", theme.warning),
            ("fatal:", theme.error),
            ("FAILED", theme.error),
            ("panicked", theme.error),
        ] {
            let row = lines
                .iter()
                .find(|l| l.to_string().contains(needle))
                .unwrap();
            assert!(row.spans[1..].iter().any(|s| s.style.fg == Some(color)));
        }
    }

    #[test]
    fn grep_highlights_literal_case_sensitive_matches_and_paths() {
        let lines = card(
            "fs",
            json!({"action":"grep","path":"crates","pattern":"fn parse"}),
            "crates/a.rs:\n  12: pub fn parse() { fn parse(); FN PARSE(); }",
            true,
            false,
        );
        let theme = crate::theme::Theme::default();
        assert_eq!(lines[1].to_string(), "▎ crates/a.rs");
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.accent));
        let matched: Vec<_> = lines[2]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .collect();
        assert_eq!(matched.len(), 2);
        assert!(
            matched
                .iter()
                .all(|s| s.content == "fn parse" && s.style.fg == Some(theme.accent))
        );
        assert!(lines[2].to_string().contains("  12  pub"));
    }

    #[test]
    fn reads_highlight_code_with_dim_number_gutters_and_language() {
        let lines = card(
            "fs",
            json!({"action":"read","path":"src/a.rs"}),
            "src/a.rs (lines 40-42 of 42):\n   40:0123abcd\tlet value = 1;\n   41:abcdef01\t\n   42:abcdef02\tvalue + 2",
            true,
            false,
        );
        let theme = crate::theme::Theme::default();
        assert!(lines[1].to_string().contains("── src/a.rs ── rust ──"));
        assert!(
            lines[2].to_string().contains("40: let value = 1;"),
            "{:?}",
            text(&lines)
        );
        assert!(!text(&lines).join("\n").contains("0123abcd"));
        assert_eq!(lines[2].spans[1].style.fg, Some(theme.fg_dim));
        assert!(
            lines[2].spans[2..]
                .iter()
                .any(|s| s.style.fg.is_some_and(|c| c != theme.fg_dim))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.to_string().contains("42: value + 2"))
        );
    }

    #[test]
    fn read_wrapping_preserves_tabs_fences_and_later_line_numbers() {
        let theme = crate::theme::Theme::default();
        let rows = read_body(
            "notes.txt",
            "notes.txt (lines 1-3 of 3):\n1:     ````\n2: \tlong content that wraps\n3: end",
            16,
            &theme,
        );
        let shown = text(&rows).join("\n");
        assert!(shown.contains("plain"));
        assert!(shown.contains("1:     ````"), "{shown}");
        assert!(shown.contains("2:     long"), "{shown}");
        assert!(shown.contains("3: end"), "{shown}");
        assert!(rows.iter().all(|row| row.width() <= 16));
    }

    #[test]
    fn markdown_controls_stay_dim_even_when_the_escape_wraps() {
        let theme = crate::theme::Theme::default();
        for source in [
            "xxxxxxxxxxxxxxxxxxxxxxxx\x1b",
            "```text\nxxxxxxxxxxxxxxxxxxxxxxxx\x1b\n```",
        ] {
            let rows = tool_markdown(source, 10, &theme);
            assert!(rows.iter().all(|row| row.width() <= 10));
            let escapes: Vec<_> = rows
                .iter()
                .flat_map(|row| &row.spans)
                .filter(|s| s.content.chars().any(|c| "\\u{1b}".contains(c)))
                .collect();
            assert!(!escapes.is_empty());
            assert!(escapes.iter().all(|s| s.style.fg == Some(theme.fg_dim)));
        }
    }

    #[test]
    fn reports_use_markdown_and_generic_arguments_form_a_table() {
        let report = card(
            "task",
            json!({"prompt":"report"}),
            "sub-agent report (1 steps, 10 tokens):\n# Findings\n\n**important** text",
            true,
            false,
        );
        assert!(report.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.content.contains("important") && s.style.add_modifier.contains(Modifier::BOLD)
            })
        }));
        assert!(
            !report[1..]
                .iter()
                .any(|l| l.to_string().contains("sub-agent report"))
        );
        let lines = card(
            "search",
            json!({"query":"needle","limit":3}),
            "a result",
            true,
            false,
        );
        assert!(lines.iter().any(|l| l.to_string() == "▎ limit  3"));
        assert!(lines.iter().any(|l| l.to_string() == "▎ query  needle"));
        assert!(lines.iter().any(|l| l.to_string().contains("── result ──")));
        for action in ["glob", "list"] {
            let listing = card(
                "fs",
                json!({"action":action,"path":".","pattern":"*"}),
                "dir/\nfile",
                true,
                false,
            );
            assert_eq!(
                listing[1].spans[1].style.fg,
                Some(crate::theme::Theme::default().accent)
            );
        }
    }

    #[test]
    fn expanded_bodies_are_capped_and_controls_are_always_dim_escapes() {
        let result = (0..450)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = card("search", json!({}), &result, true, false);
        assert_eq!(lines.len(), 402); // header + 400 body rows + blank
        assert_eq!(lines[400].to_string(), "▎ … 52 more lines");
        let theme = crate::theme::Theme::default();
        for (name, args, result) in [
            (
                "fs",
                json!({"action":"edit","path":"a"}),
                "edited a (+1 −0)\n--- a/a\n+++ b/a\n@@ -0,0 +1 @@\n+x\x1b",
            ),
            (
                "shell",
                json!({"action":"run","cmd":"x\x1b","cwd":"a\x07"}),
                "exit 1\nerror: x\r",
            ),
            (
                "proc",
                json!({"action":"read","id":"x"}),
                "x · exited 1\npanicked\x1b",
            ),
            (
                "fs",
                json!({"action":"read","path":"a.rs"}),
                "a.rs (lines 1-1 of 1):\n1: let x = \"a\x1b\";\r",
            ),
            (
                "fs",
                json!({"action":"grep","pattern":"x","path":"."}),
                "a\x07:\n  1: x\x1b",
            ),
            (
                "fs",
                json!({"action":"glob","pattern":"*","path":"."}),
                "a\x1b/",
            ),
            ("fs", json!({"action":"list","path":"."}), "a\x1b/"),
            (
                "task",
                json!({"prompt":"report"}),
                "sub-agent report (1 steps, 1 tokens):\n**x\x1b**",
            ),
            ("search", json!({"query":"a\x07"}), "x\x1b"),
        ] {
            let lines = card(name, args, result, true, false);
            let body = text(&lines[1..]).join("\n");
            assert!(
                !body.chars().any(|c| c.is_control() && c != '\n'),
                "{name}: {body}"
            );
            assert!(body.contains("\\u{"), "{name}: {body}");
            for row in lines.iter().skip(1).take(lines.len() - 2) {
                assert!(row.to_string().starts_with("▎ "), "{name}: {row}");
                assert!(row.width() <= 80, "{name}: {row}");
                for span in &row.spans {
                    if span.content.contains("\\u{") || span.content == "\\" {
                        assert_eq!(span.style.fg, Some(theme.fg_dim), "{name}: {span:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_expanded_cards_wrap_gutters_without_losing_the_rail() {
        let theme = crate::theme::Theme::default();
        for (name, args, result) in [
            (
                "fs",
                json!({"action":"edit","path":"a"}),
                "edited a (+1 −1)\n--- a/a\n+++ b/a\n@@ -100 +100 @@\n-old\n+new",
            ),
            (
                "fs",
                json!({"action":"grep","path":".","pattern":"x"}),
                "a:\n  100: xxxxx",
            ),
        ] {
            let mut rows = Vec::new();
            push_tool_call(
                &mut rows,
                &json!({"name":name,"arguments":args.to_string(),"result":result}).to_string(),
                true,
                &Settings::default(),
                8,
                &theme,
            );
            for row in &rows[1..rows.len() - 1] {
                assert!(row.width() <= 8, "{row}");
                assert_eq!(row.spans[0].content, "▎ ");
            }
        }
    }

    #[test]
    fn fold_markers_fit_without_wrapping_long_headers() {
        for expanded in [false, true] {
            let short = card("search", json!({"query":"x"}), "ok", expanded, false);
            assert_eq!(short[0].width(), 80);
            assert!(short[0].to_string().ends_with(if expanded {
                glyph::EXPANDED
            } else {
                glyph::SELECTED
            }));
            let long = card(
                "shell",
                json!({"action":"run","cmd":"x".repeat(100)}),
                "exit 0",
                expanded,
                false,
            );
            assert!(!long[0].to_string().contains(['▸', '▾']));
        }
    }

    #[test]
    fn round_thinking_and_interim_text_sit_above_the_first_call() {
        let row = json!({
            "name": "fs",
            "arguments": json!({"action":"read","path":"src/a.rs"}).to_string(),
            "result": "src/a.rs (lines 1-2 of 2):\n1: fn a() {}\n2: fn b() {}",
            "reasoning_content": "check a first",
            "assistant_content": "Let me read a.rs.",
        })
        .to_string();
        for (show, expected) in [
            (false, "▎ ▸ reasoning (13 chars) — Ctrl+R to expand"),
            (true, "▎ ▾ reasoning"),
        ] {
            let mut lines = Vec::new();
            let settings = Settings {
                show_reasoning: show,
                ..Settings::default()
            };
            push_tool_call(
                &mut lines,
                &row,
                false,
                &settings,
                80,
                &crate::theme::Theme::default(),
            );
            let lines = text(&lines);
            assert_eq!(lines[0], expected);
            let said = lines.iter().position(|l| l == "Let me read a.rs.").unwrap();
            let call = lines
                .iter()
                .position(|l| l.contains("read src/a.rs"))
                .unwrap();
            assert!(said < call, "{lines:?}");
        }
    }

    #[test]
    fn ctrl_t_shows_the_result_of_reads_and_searches() {
        let result = "src/a.rs (lines 1-2 of 2):\n1: fn a() {}\n2: fn b() {}";
        let collapsed = text(&card(
            "fs",
            json!({"action":"read","path":"src/a.rs"}),
            result,
            false,
            false,
        ));
        assert_eq!(collapsed.len(), 1, "{collapsed:?}");
        let expanded = text(&card(
            "fs",
            json!({"action":"read","path":"src/a.rs"}),
            result,
            true,
            false,
        ));
        assert!(
            expanded.iter().any(|l| l.contains("2: fn b() {}")),
            "{expanded:?}"
        );
    }

    #[test]
    fn web_searches_show_their_query_sources_and_results() {
        let result = "[1] Budget passes\n    https://www.mongolia.substack.com/p/b\n    The 2026 budget passed on time.\n[2] World population\n    https://en.wikipedia.org/wiki/World_population\n    The total number of people.";
        let args = json!({"mode":"web","query":"Mongolia budget"});
        let collapsed = text(&card("search", args.clone(), result, false, false));
        assert!(
            collapsed[0].starts_with("◎ search \"Mongolia budget\" · 2 results"),
            "{collapsed:?}"
        );
        assert_eq!(
            collapsed[1].trim(),
            "mongolia.substack.com · en.wikipedia.org"
        );
        let open = text(&card("search", args, result, true, false));
        assert!(
            open.iter().any(|l| l.contains("1  Budget passes")),
            "{open:?}"
        );
        assert!(
            open.iter().any(|l| l
                .trim_start_matches(['▎', ' '])
                .starts_with("en.wikipedia.org")),
            "{open:?}"
        );
        assert!(
            open.iter()
                .any(|l| l.contains("The total number of people.")),
            "{open:?}"
        );
    }

    #[test]
    fn compact_edit_is_capped_and_expanded_diff_never_shows_arguments() {
        let diff = (1..=20)
            .map(|n| format!("+new line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = format!(
            "edited src/a (+20 −1)\n--- a/src/a\n+++ b/src/a\n@@ -1 +1,20 @@\n-old\n{diff}"
        );
        let args =
            json!({"action":"edit","path":"src/a","edits":[{"hash":"secret hash","new":"new"}]});
        let compact = card("fs", args.clone(), &result, false, false);
        assert_eq!(
            compact[0].to_string().trim_end_matches([' ', '▸']),
            "⚒ edit src/a  +20 −1"
        );
        assert_eq!(compact.len(), 9);
        assert!(
            compact
                .last()
                .unwrap()
                .to_string()
                .contains("more lines · Ctrl+T")
        );
        assert!(
            compact[1..]
                .iter()
                .all(|line| line.to_string().starts_with("   "))
        );
        let expanded = card("fs", args.clone(), &result, true, false);
        let text = expanded
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("new line 20"));
        assert!(!text.contains("secret hash"));
        assert!(!text.contains("\"action\""));
        assert_eq!(expanded.last().unwrap().to_string(), "");
        assert!(
            !card("fs", args, &result, false, true)
                .last()
                .unwrap()
                .to_string()
                .contains("Ctrl+T")
        );
    }

    #[test]
    fn failed_command_shows_only_output_tail_until_expanded() {
        let args = json!({"action":"run","cmd":"cargo build"});
        let result = "exit 101 · 9.8s\nfirst\nsecond\nthird\nfourth\nfifth\nsixth\nlast\x1b[2J";
        let compact = card("shell", args.clone(), result, false, false);
        assert_eq!(
            compact[0].to_string().trim_end_matches([' ', '▸']),
            "✗ $ cargo build · exit 101 · 9.8s"
        );
        assert_eq!(compact.len(), 6);
        assert_eq!(compact[1].to_string(), "   third");
        assert!(compact[5].to_string().contains("last\\u{1b}[2J"));
        let expanded = card("shell", args, result, true, false);
        assert!(expanded.iter().any(|line| line.to_string() == "▎ first"));
        assert!(
            !expanded
                .iter()
                .any(|line| line.to_string().contains("\"cmd\""))
        );
        assert_eq!(
            card(
                "shell",
                json!({"action":"run","cmd":"true"}),
                "exit 0\nok",
                false,
                false
            )
            .len(),
            1
        );
    }
}

#[cfg(test)]
mod image_reference_tests {
    use super::*;

    #[test]
    fn image_looking_code_is_not_removed_or_loaded() {
        let source = "```mermaid\nflowchart LR\nA[\"![label](private.png)\"] --> B\n```";
        assert_eq!(strip_markdown_images(source), source);
        assert!(markdown_images(source).is_empty());
        assert!(markdown_images("`![literal](private.png)`").is_empty());
    }

    #[test]
    fn real_images_are_removed_without_losing_surrounding_text() {
        let source = "Before ![plot](plot.png) after";
        assert_eq!(strip_markdown_images(source), "Before  after");
        let refs = markdown_images(source);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].1, "plot.png");
        assert_eq!(refs[0].2, "plot");
    }
}
