//! Practice components: a highlighted code editor with line numbers, choice
//! lists that mark the right answer once picked, and quizzes that page
//! through questions. Every quiz reserves its tallest question so paging
//! never moves the transcript below it.

use super::{Control, Entry, branch_regions, control_style, display, glyph, wrapped_text};
use crate::theme::Theme;
use crossterm::event::KeyCode;
use nexus_core::interactive::{EditorKey, Node, Question, Session};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn height(node: &Node, session: &Session, width: u16) -> Option<u16> {
    Some(match node {
        Node::Editor { .. } | Node::Choice { .. } => node.rows(),
        Node::Quiz { items, .. } => {
            1 + items
                .iter()
                .map(|item| question_height(item, session, width))
                .max()
                .unwrap_or(0)
        }
        _ => return None,
    })
}

/// Prompt, answer and explanation lines, wrapped to `width`.
fn question_text(
    item: &Question,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let text = |template: &str| session.text(template).unwrap_or_default();
    let title = wrapped_text(
        &text(&item.title),
        width,
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    );
    let mut answer = Vec::new();
    if let Some(value) = &item.answer {
        answer.extend(wrapped_text(
            &text(value),
            width,
            Style::default().fg(theme.success),
        ));
    }
    if let Some(value) = &item.explain {
        answer.extend(wrapped_text(
            &text(value),
            width,
            Style::default().fg(theme.fg_dim),
        ));
    }
    (title, answer)
}

fn view_height(item: &Question, session: &Session, width: u16) -> u16 {
    branch_regions(session, &item.view, Rect::new(0, 0, width, u16::MAX))
        .last()
        .map_or(0, |area| area.bottom())
}

fn question_height(item: &Question, session: &Session, width: u16) -> u16 {
    let (title, answer) = question_text(item, session, width, &Theme::default());
    let view = view_height(item, session, width);
    let rows = |lines: &[Line]| u16::try_from(lines.len()).unwrap_or(u16::MAX);
    rows(&title)
        .saturating_add(if view > 0 { view + 1 } else { 0 })
        .saturating_add(if item.has_answer() {
            2 + rows(&answer)
        } else {
            0
        })
}

/// The current question's view and the area it is drawn in.
pub(super) fn quiz_view<'a>(
    node: &'a Node,
    session: &Session,
    region: Rect,
) -> Option<(&'a [Node], Rect)> {
    let Node::Quiz { items, .. } = node else {
        return None;
    };
    let item = items.get(session.question(session.document().node_index(node)))?;
    let (title, _) = question_text(item, session, region.width, &Theme::default());
    let top = 1 + u16::try_from(title.len()).unwrap_or(0) + 1;
    Some((
        &item.view,
        Rect::new(
            region.x,
            region.y + top,
            region.width,
            view_height(item, session, region.width),
        ),
    ))
}

pub(super) fn render(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    (focus, control, can_send): (Option<usize>, usize, bool),
    buffer: &mut Buffer,
) -> Option<Vec<(Rect, usize)>> {
    let index = session.document().node_index(node);
    Some(match node {
        Node::Editor {
            key, label, lang, ..
        } => {
            let selected = focus == Some(control);
            editor(
                (index, key, label, lang.as_deref()),
                session,
                area,
                theme,
                selected,
                buffer,
            );
            vec![(area, control)]
        }
        Node::Choice {
            key,
            label,
            options,
            answer,
        } => {
            Paragraph::new(display(session, label))
                .style(Style::default().fg(theme.fg_dim))
                .render(Rect::new(area.x, area.y, area.width, 1), buffer);
            let picked = session.value(key);
            let mut hits = Vec::new();
            for (option, text) in options.iter().enumerate() {
                let row = Rect::new(
                    area.x,
                    area.y + 1 + u16::try_from(option).unwrap_or(0),
                    area.width,
                    1,
                );
                let chosen = picked == *text;
                let (mark, color) = match answer {
                    Some(answer) if !picked.is_empty() && text == answer => {
                        (glyph::OK, Some(theme.success))
                    }
                    Some(_) if chosen => (glyph::FAIL, Some(theme.error)),
                    _ if chosen => (glyph::DOT, None),
                    _ => (glyph::RING, None),
                };
                let mut style = control_style(theme, focus == Some(control + option));
                if let Some(color) = color {
                    style = style.fg(color);
                }
                Paragraph::new(format!("{mark} {}", display(session, text)))
                    .style(style)
                    .render(row, buffer);
                hits.push((row, control + option));
            }
            hits
        }
        Node::Quiz { items, .. } => quiz(
            node,
            items,
            session,
            area,
            theme,
            (focus, control, can_send),
            buffer,
        ),
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
fn quiz(
    node: &Node,
    items: &[Question],
    session: &Session,
    area: Rect,
    theme: &Theme,
    (focus, control, can_send): (Option<usize>, usize, bool),
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let index = session.document().node_index(node);
    let current = session.question(index);
    let header = Rect::new(area.x, area.y, area.width, 1);
    Paragraph::new(format!(
        "{} Question {} of {} {}",
        glyph::PREV,
        current + 1,
        items.len(),
        glyph::NEXT
    ))
    .style(control_style(theme, focus == Some(control)).add_modifier(Modifier::BOLD))
    .render(header, buffer);
    if let Some((right, total)) = session.quiz_score(index) {
        Paragraph::new(format!("{right}/{total} right"))
            .style(Style::default().fg(theme.fg_dim))
            .right_aligned()
            .render(header, buffer);
    }
    let mut hits = vec![(header, control)];
    let Some(item) = items.get(current) else {
        return hits;
    };
    let (title, answer) = question_text(item, session, area.width, theme);
    let mut y = area.y + 1;
    let title_rows = u16::try_from(title.len()).unwrap_or(0);
    Paragraph::new(title).render(Rect::new(area.x, y, area.width, title_rows), buffer);
    y += title_rows;
    let mut next = control + 2;
    if let Some((view, view_area)) = quiz_view(node, session, area)
        && view_area.height > 0
    {
        let regions = branch_regions(session, view, view_area);
        hits.extend(Entry::branch(
            view, session, &regions, theme, focus, can_send, buffer, &mut next,
        ));
        y = view_area.bottom();
    }
    if item.has_answer() {
        let reveal = Rect::new(area.x, y + 1, area.width, 1);
        let shown = session.revealed(index);
        Paragraph::new(if shown {
            "[ Hide answer ]"
        } else {
            "[ Show answer ]"
        })
        .style(control_style(theme, focus == Some(control + 1)))
        .render(reveal, buffer);
        hits.push((reveal, control + 1));
        if shown {
            let rows = u16::try_from(answer.len()).unwrap_or(0);
            Paragraph::new(answer).render(
                Rect::new(
                    area.x,
                    reveal.y + 1,
                    area.width,
                    rows.min(area.bottom().saturating_sub(reveal.y + 1)),
                ),
                buffer,
            );
        }
    }
    hits
}

/// Highlighted lines of `source`, one per `\n`-separated line. Lines the
/// highlighter reshaped fall back to plain text so the caret stays aligned.
pub(super) fn highlighted(source: &str, lang: &str, theme: &Theme) -> Vec<Line<'static>> {
    let plain: Vec<&str> = source.split('\n').collect();
    let fence = "`".repeat(
        source
            .split(|ch| ch != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(3)
            + 1,
    );
    let lang = if lang.eq_ignore_ascii_case("nexus-ui") || lang.eq_ignore_ascii_case("mermaid") {
        "plain"
    } else {
        lang
    };
    let rendered = crate::ui::markdown::render(
        &format!("{fence}{lang}\n{source}\n{fence}"),
        plain
            .iter()
            .map(|line| line.width())
            .max()
            .unwrap_or(1)
            .max(1)
            .saturating_add(4),
        crate::ui::markdown::MdColors {
            heading: theme.accent,
            rule: theme.border_dim,
            code: theme.fg,
        },
    );
    let mut lines = rendered.lines.into_iter().skip(1);
    plain
        .iter()
        .map(|text| {
            let content = lines.next().map(|line| {
                let count = line.spans.len().saturating_sub(2);
                Line::from(
                    line.spans
                        .into_iter()
                        .skip(1)
                        .take(count)
                        .collect::<Vec<_>>(),
                )
            });
            match content {
                Some(line)
                    if crate::ui::markdown::line_text(&line).trim_end() == text.trim_end() =>
                {
                    line
                }
                _ => Line::styled((*text).to_string(), Style::default().fg(theme.fg)),
            }
        })
        .collect()
}

/// `line` without its first `skip` display columns, at most `take` wide.
fn columns(line: &Line<'static>, skip: usize, take: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut column = 0;
    for span in &line.spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let width = ch.width().unwrap_or(0);
            if column >= skip && column + width <= skip + take {
                text.push(ch);
            }
            column += width;
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
    }
    Line::from(spans)
}

fn gutter(text: &str, top: usize, rows: u16) -> u16 {
    let last = text.split('\n').count().max(top + usize::from(rows));
    u16::try_from(last.to_string().len() + 1).unwrap_or(4)
}

fn editor(
    (node, key, label, lang): (usize, &str, &str, Option<&str>),
    session: &Session,
    area: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    let state = session.editor(node);
    let editing = selected && state.editing;
    let text = session.value(key);
    let header = Rect::new(area.x, area.y, area.width, 1);
    Paragraph::new(display(session, label))
        .style(control_style(theme, selected).fg(if selected {
            theme.accent
        } else {
            theme.fg_dim
        }))
        .render(header, buffer);
    let hint = if editing {
        "Esc done · Tab indent".to_string()
    } else if selected {
        "Enter edit".to_string()
    } else {
        lang.unwrap_or_default().to_string()
    };
    Paragraph::new(hint)
        .style(Style::default().fg(theme.fg_dim))
        .right_aligned()
        .render(header, buffer);
    let rows = area.height.saturating_sub(1);
    let body = Rect::new(area.x, area.y + 1, area.width, rows);
    buffer.set_style(body, Style::default().bg(theme.raised));
    let gutter = gutter(&text, state.top, rows);
    let width = usize::from(area.width.saturating_sub(gutter));
    let (line, column) = nexus_core::interactive::line_col(&text, state.cursor);
    let caret_x = text
        .split('\n')
        .nth(line)
        .unwrap_or_default()
        .chars()
        .take(column)
        .map(|ch| ch.width().unwrap_or(0))
        .sum::<usize>();
    let left = if editing {
        caret_x.saturating_sub(width.saturating_sub(1))
    } else {
        0
    };
    let lines = highlighted(&text, lang.unwrap_or("plain"), theme);
    for row in 0..rows {
        let number = state.top + usize::from(row);
        let y = body.y + row;
        let Some(content) = lines.get(number) else {
            continue;
        };
        buffer.set_string(
            area.x,
            y,
            format!("{:>w$} ", number + 1, w = usize::from(gutter - 1)),
            Style::default().fg(theme.fg_dim).bg(theme.raised),
        );
        Paragraph::new(columns(content, left, width))
            .style(Style::default().bg(theme.raised))
            .render(
                Rect::new(area.x + gutter, y, u16::try_from(width).unwrap_or(0), 1),
                buffer,
            );
    }
    if editing && line >= state.top && line < state.top + usize::from(rows) {
        let x = area.x + gutter + u16::try_from(caret_x - left).unwrap_or(0);
        let y = body.y + u16::try_from(line - state.top).unwrap_or(0);
        if x < area.right() {
            let cell = &mut buffer[(x, y)];
            cell.set_style(Style::default().fg(theme.bg).bg(theme.accent));
        }
    }
}

/// Route a key to the focused editor while it is editing. Returns whether
/// the key was consumed.
pub(super) fn editor_key(entry: &mut Entry, node: usize, code: KeyCode) -> bool {
    let Ok(session) = &mut entry.session else {
        return false;
    };
    let key = match code {
        KeyCode::Esc => {
            entry.error = session
                .set_editing(node, false)
                .err()
                .map_or_else(String::new, |error| error.to_string());
            return true;
        }
        KeyCode::Enter => EditorKey::Newline,
        KeyCode::Tab => EditorKey::Indent,
        KeyCode::BackTab => EditorKey::Outdent,
        KeyCode::Backspace => EditorKey::Backspace,
        KeyCode::Delete => EditorKey::Delete,
        KeyCode::Left => EditorKey::Left,
        KeyCode::Right => EditorKey::Right,
        KeyCode::Up => EditorKey::Up,
        KeyCode::Down => EditorKey::Down,
        KeyCode::Home => EditorKey::Home,
        KeyCode::End => EditorKey::End,
        KeyCode::Char(ch) => EditorKey::Insert(ch.to_string()),
        _ => return false,
    };
    entry.error = session
        .edit_code(node, &key)
        .err()
        .map_or_else(String::new, |error| error.to_string());
    true
}

/// Whether the focused control is an editor that is taking keys.
pub(super) fn editing(entry: &Entry) -> Option<usize> {
    let Some(Control::Editor(node)) = entry.controls().get(entry.focus).cloned() else {
        return None;
    };
    entry
        .session
        .as_ref()
        .is_ok_and(|session| session.editor(node).editing)
        .then_some(node)
}

/// Clicks: an editor places its caret, a quiz header pages back on its
/// left arrow and forward elsewhere.
pub(super) fn click(
    entry: &mut Entry,
    control: &Control,
    pos: Position,
    area: Rect,
    row_offset: u16,
) -> bool {
    let Ok(session) = &mut entry.session else {
        return true;
    };
    let result = match control {
        Control::Editor(node) => {
            let Some(Node::Editor { key, rows, .. }) = session.document().node(*node) else {
                return true;
            };
            let row = (row_offset + pos.y - area.y).saturating_sub(1);
            let state = session.editor(*node);
            let text = session.value(key);
            let gutter = gutter(&text, state.top, *rows);
            let target = usize::from((pos.x - area.x).saturating_sub(gutter));
            let line = text
                .split('\n')
                .nth(state.top + usize::from(row))
                .unwrap_or_default();
            let mut used = 0;
            let column = line
                .chars()
                .take_while(|ch| {
                    used += ch.width().unwrap_or(0);
                    used <= target
                })
                .count();
            session.place_cursor(*node, usize::from(row), column)
        }
        Control::Quiz(node) => {
            let question = session.question(*node);
            if pos.x < area.x + 2 {
                session.select_question(*node, question.saturating_sub(1))
            } else {
                session.select_question(*node, question + 1)
            }
        }
        _ => return false,
    };
    entry.error = result
        .err()
        .map_or_else(String::new, |error| error.to_string());
    true
}
