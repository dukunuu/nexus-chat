use crossterm::event::{KeyCode, KeyEvent};
use nexus_core::app::AppCommand;
use nexus_core::tools::Decision;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;

use super::chrome;
use crate::ui::diff::{self, escaped_rows};
use crate::ui::style;

fn request_lines(
    tool: &str,
    action: &str,
    summary: &str,
    class: Option<&str>,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let normal = Style::default().fg(theme.fg);
    let (evidence, summary) = summary
        .split_once('\n')
        .filter(|(head, _)| head.starts_with("in_scope ") || head.starts_with("risk: "))
        .unwrap_or(("", summary));
    let (head, body) = summary.split_once('\n').unwrap_or((summary, ""));
    let is_diff = body.starts_with("--- ") || body.starts_with("diff --git ");
    let parsed_counts = head.rsplit_once(" (+").and_then(|(label, counts)| {
        let (added, deleted) = counts.strip_suffix(')')?.split_once(" −")?;
        Some((
            label,
            added.parse::<usize>().ok()?,
            deleted.parse::<usize>().ok()?,
        ))
    });
    let label = if tool == "fs" {
        parsed_counts
            .map_or(head, |(label, _, _)| label)
            .to_string()
    } else {
        format!("{tool}{}{action}", style::SEP)
    };
    let mut header = escaped_rows(&label, width, normal, dim);
    if is_diff || parsed_counts.is_some() {
        let (added, deleted) = parsed_counts.map_or_else(
            || diff::counts(body),
            |(_, added, deleted)| (added, deleted),
        );
        let line = header.last_mut().expect("request has a header");
        line.spans.push(Span::styled(
            format!("  +{added}"),
            Style::default().fg(theme.success),
        ));
        line.spans.push(Span::styled(
            format!(" −{deleted}"),
            Style::default().fg(theme.error),
        ));
    }
    if let Some(class) = class {
        let badge_style = match class {
            "runs a command" => Style::default().fg(theme.accent2),
            "destructive" => Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
            "reaches outside" => Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
            _ => dim,
        };
        let badge = escaped_rows(class, width, badge_style, dim).remove(0);
        let line = header.last_mut().expect("request has a header");
        *line = chrome::fit_line(line.clone(), width.saturating_sub(badge.width() + 1));
        let padding = width.saturating_sub(line.width() + badge.width()).max(1);
        line.spans.push(Span::raw(" ".repeat(padding)));
        line.spans.extend(badge.spans);
    }
    header.push(Line::default());
    if !evidence.is_empty() {
        header.extend(escaped_rows(evidence, width, dim, dim));
    }
    if is_diff {
        header.extend(diff::render(
            body,
            width,
            theme,
            diff::Options {
                max_rows: None,
                context: 3,
                hint: "",
            },
        ));
    } else {
        let text = if tool == "fs" { body } else { summary };
        for line in text.split('\n').filter(|line| !line.is_empty()) {
            let line = line.strip_prefix("background: ").unwrap_or(line);
            let color = if line.starts_with("could not parse safely:") {
                Style::default().fg(theme.warning)
            } else if line.starts_with("$ ") {
                normal.add_modifier(Modifier::BOLD)
            } else if line.starts_with("in ") {
                dim
            } else {
                normal
            };
            header.extend(escaped_rows(line, width, color, dim));
        }
    }
    header
}

fn wave_lines(summary: &str, width: usize, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let mut sections: Vec<(&str, &str, &str, String)> = Vec::new();
    for line in summary.split('\n').skip(1) {
        if let Some((number, call)) = line
            .split_once(". ")
            .filter(|(number, _)| number.parse::<usize>().is_ok())
        {
            let (tool, action) = call.split_once(' ').unwrap_or((call, ""));
            sections.push((number, tool, action, String::new()));
        } else if let Some((_, _, _, body)) = sections.last_mut() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(line.strip_prefix("  ").unwrap_or(line));
        }
    }
    let mut lines = Vec::new();
    for (number, tool, action, body) in sections {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut section =
            request_lines(tool, action, &body, None, width.saturating_sub(5), theme).into_iter();
        let Some(mut head) = section.next() else {
            continue;
        };
        let section_style = style::section(theme, "", 0).spans[0].style;
        for span in &mut head.spans {
            if span.style.fg == Some(theme.fg) {
                span.style = section_style;
            }
        }
        head.spans.insert(
            0,
            Span::styled(
                format!("{} {number}  ", style::glyph::SECTION),
                section_style,
            ),
        );
        lines.push(head);
        lines.extend(section);
        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }
    }
    lines
}

/// One plain sentence saying what the agent is asking for, above the preview.
fn explain(tool: &str, action: &str, text: &str) -> String {
    let text = text
        .split_once('\n')
        .filter(|(head, _)| head.starts_with("in_scope ") || head.starts_with("risk: "))
        .map_or(text, |(_, rest)| rest);
    let (head, body) = text.split_once('\n').unwrap_or((text, ""));
    let (label, counts) = head.rsplit_once(" (+").map_or((head, ""), |(l, c)| (l, c));
    let path = label.split_once(' ').map_or(label, |(_, path)| path);
    let (added, deleted) = counts
        .strip_suffix(')')
        .and_then(|counts| counts.split_once(" −"))
        .map_or_else(
            || {
                let (added, deleted) = diff::counts(body);
                (added.to_string(), deleted.to_string())
            },
            |(added, deleted)| (added.to_string(), deleted.to_string()),
        );
    let (added, deleted) = (added.as_str(), deleted.as_str());
    let lines = |n: &str| {
        if n == "1" {
            "1 line".to_string()
        } else {
            format!("{n} lines")
        }
    };
    match (tool, action) {
        ("wave", _) => format!(
            "The agent wants to make these {action} together. Each is shown below; \
             y approves all of them, r lets you decide one by one."
        ),
        ("fs", "edit") => format!(
            "The agent wants to change {path}: {} added, {} removed.",
            lines(added),
            lines(deleted)
        ),
        ("fs", "write") if body.contains("@@ -0,0 ") || !body.contains("\n-") => {
            format!("The agent wants to create {path} with {}.", lines(added))
        }
        ("fs", "write") => format!(
            "The agent wants to replace the contents of {path}: {} added, {} removed.",
            lines(added),
            lines(deleted)
        ),
        ("fs", "delete") => format!("The agent wants to delete {path} ({}).", lines(deleted)),
        ("fs", "move") => format!("The agent wants to {head}."),
        ("shell", _) => "The agent wants to run this command:".to_string(),
        ("proc", _) => "The agent wants to start this in the background:".to_string(),
        ("task", _) => {
            "The agent wants a sub-agent to edit files in its own git worktree:".to_string()
        }
        _ => format!("The agent wants to use {tool}{}{action}:", style::SEP),
    }
}

/// Put the plain-language explanation under the header (first, for a wave).
fn insert_intro(
    lines: &mut Vec<Line<'static>>,
    request: &nexus_core::tools::ApprovalRequest,
    text: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let wave = request.tool == "wave";
    let at = if wave {
        0
    } else {
        lines
            .iter()
            .position(|line| line.width() == 0)
            .unwrap_or(lines.len())
    };
    // Word-wrapped (it's a sentence), then escaped: paths come from the model.
    let dim = Style::default().fg(theme.fg_dim);
    let mut intro: Vec<Line<'static>> =
        crate::ui::cards::wrap_plain(&explain(&request.tool, &request.action, text), width)
            .iter()
            .flat_map(|row| escaped_rows(row, width, Style::default().fg(theme.fg), dim))
            .collect();
    if wave {
        intro.push(Line::default());
    } else {
        intro.insert(0, Line::default());
    }
    lines.splice(at..at, intro);
}

/// The pinned key row: what each key does for this request.
fn legend(app: &AppView, request: &nexus_core::tools::ApprovalRequest) -> String {
    let wave = request.tool == "wave";
    let session = match request.tool.as_str() {
        "fs" => "a allow all edits this session",
        "shell" | "proc" => "a allow all commands this session",
        "wave" => "a + session",
        _ => "a allow for this session",
    };
    if shell_unread(app) {
        let deny = if wave { "n deny all" } else { "n deny" };
        let review = if wave { " · r one by one" } else { "" };
        format!("scroll to the end to enable y{review} · {deny}")
    } else if wave {
        let session = if request.allow_session {
            format!(" · {session}")
        } else {
            String::new()
        };
        // Short enough for an 80-column terminal; the sentence above the
        // preview spells out what y and r do.
        format!("y approve all{session} · r one by one · n deny all")
    } else if request.allow_session {
        format!("y allow once · {session} · n deny")
    } else {
        "y allow once · n deny".to_string()
    }
}

fn shell_unread(app: &AppView) -> bool {
    app.approval_request.as_ref().is_some_and(|request| {
        request.tool == "shell"
            || (request.tool == "wave"
                && request.summary.lines().next().is_some_and(|counts| {
                    counts
                        .split(", ")
                        .any(|count| count.ends_with(" command") || count.ends_with(" commands"))
                }))
    }) && app.approval_max_scroll.get() > 0
        && !app.approval_seen_end.get()
}

/// The request's preview lines (explanation, header, diff or command) at
/// the panel's inner `width`.
fn content_lines(app: &AppView, width: usize) -> Option<Vec<Line<'static>>> {
    let request = app.approval_request.as_ref()?;
    let text = request.detail.as_deref().unwrap_or(&request.summary);
    let mut lines = if request.tool == "wave" {
        wave_lines(text, width, &app.theme)
    } else {
        request_lines(
            &request.tool,
            &request.action,
            text,
            request.class.as_deref(),
            width,
            &app.theme,
        )
    };
    insert_intro(&mut lines, request, text, width, &app.theme);
    Some(lines)
}

fn panel_block(app: &AppView) -> ratatui::widgets::Block<'static> {
    chrome::popup_block_focused("", &app.theme, true, chrome::Tone::Normal)
}

/// How tall the approval panel docked in the composer's place wants to be
/// in `column`: the whole request when it fits, else about half the screen
/// (the preview scrolls), never less than 8 rows.
#[must_use]
pub fn docked_height(app: &AppView, column: Rect) -> u16 {
    let frame = panel_block(app).inner(Rect::new(0, 0, column.width, 100));
    let chrome_rows = 100 - frame.height;
    let Some(lines) = content_lines(app, usize::from(frame.width)) else {
        return 0;
    };
    // Preview + a spacer + the key row + the border.
    let wanted = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .saturating_add(chrome_rows);
    let cap = (column.height.saturating_mul(55) / 100).max(8);
    wanted.min(cap).min(column.height.saturating_sub(2)).max(4)
}

/// Draw the approval docked at the bottom of the reading column — the
/// composer's place — as the main layout does (tests render it alone).
#[cfg(test)]
pub fn render(f: &mut Frame, app: &AppView) {
    let column = crate::ui::reading_column(f.area());
    let height = docked_height(app, column);
    let area = Rect {
        y: column.bottom().saturating_sub(height.saturating_add(1)),
        height,
        ..column
    };
    render_in(f, app, area);
}

/// Draw the approval panel in `area` (where the composer normally is).
pub fn render_in(f: &mut Frame, app: &AppView, area: Rect) {
    let Some(request) = &app.approval_request else {
        return;
    };
    app.approval_area.set(Some(area));
    let wave = request.tool == "wave";
    let dim = Style::default().fg(app.theme.fg_dim);
    let width = usize::from(panel_block(app).inner(area).width);
    let Some(lines) = content_lines(app, width) else {
        return;
    };
    let inner = panel_block(app).inner(area);
    // The bottom inner row is the key legend; the preview scrolls above it.
    let height = usize::from(inner.height).saturating_sub(2);
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
    let title = if wave {
        format!(
            " approve {}{}{} ",
            request.action,
            style::SEP,
            request.summary.lines().next().unwrap_or_default()
        )
    } else if queued > 1 {
        format!(" approve{}1 of {queued} ", crate::ui::style::SEP)
    } else {
        " approve ".to_string()
    };
    let legend = legend(app, request);
    chrome::render_hinted(
        f,
        area,
        escaped_rows(
            &title,
            usize::MAX,
            Style::default().fg(app.theme.accent),
            dim,
        )
        .remove(0),
        "",
        app,
        true,
        chrome::Tone::Normal,
    );
    render_footer(f, app, area, (offset, height, lines.len()), max_scroll);
    render_legend(f, app, inner, &legend);
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
    let body = ratatui::layout::Rect {
        height: u16::try_from(height).unwrap_or(u16::MAX),
        ..inner
    };
    f.render_widget(Paragraph::new(visible), body);
}

/// The pinned key row at the bottom of the popup, dim during the open delay
/// (when the keys don't work yet).
fn render_legend(f: &mut Frame, app: &AppView, inner: ratatui::layout::Rect, legend: &str) {
    if inner.height < 2 {
        return;
    }
    let delayed = app
        .approval_opened_at
        .is_some_and(|opened| opened.elapsed() < app.approval_delay);
    let style = if delayed {
        Style::default()
            .fg(app.theme.fg_dim)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(app.theme.accent)
    };
    let row = ratatui::layout::Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            chrome::truncate(legend, usize::from(inner.width)),
            style,
        ))),
        row,
    );
}

/// The border's position readout when the preview scrolls.
fn render_footer(
    f: &mut Frame,
    app: &AppView,
    area: ratatui::layout::Rect,
    (offset, height, total): (usize, usize, usize),
    max_scroll: usize,
) {
    let hint = if max_scroll > 0 {
        format!(
            "lines {}–{} of {total} · ↑↓ PgUp PgDn Home End or wheel",
            offset + 1,
            (offset + height).min(total),
        )
    } else {
        String::new()
    };
    let hint = hint.as_str();
    let percentage = offset
        .saturating_mul(100)
        .checked_div(max_scroll)
        .map_or_else(String::new, |percent| format!(" · {percent}%"));
    let hint_width = usize::from(area.width.saturating_sub(4));
    let hint = if app.settings.hide_hints { "" } else { hint };
    let footer = format!(
        " {}{} ",
        chrome::truncate(
            hint,
            hint_width.saturating_sub(percentage.chars().count() + 2)
        ),
        percentage
    );
    let delayed = app
        .approval_opened_at
        .is_some_and(|opened| opened.elapsed() < app.approval_delay);
    let footer_style = if delayed {
        Style::default()
            .fg(app.theme.fg_dim)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(app.theme.fg)
    };
    if !hint.is_empty() || max_scroll > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(footer, footer_style))).right_aligned(),
            ratatui::layout::Rect::new(
                area.x + 2,
                area.bottom().saturating_sub(1),
                area.width.saturating_sub(4),
                1,
            ),
        );
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> anyhow::Result<()> {
    let page = app.approval_page_height.get();
    let (up, amount) = match key.code {
        KeyCode::Up | KeyCode::Char('k') if !super::is_chord(key) => (true, 1),
        KeyCode::Down | KeyCode::Char('j') if !super::is_chord(key) => (false, 1),
        KeyCode::PageUp => (true, page),
        KeyCode::PageDown | KeyCode::Char(' ') => (false, page),
        KeyCode::Home | KeyCode::Char('g') if !super::is_chord(key) => (true, usize::MAX),
        KeyCode::End | KeyCode::Char('G') if !super::is_chord(key) => (false, usize::MAX),
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
        KeyCode::Char('a')
            if !super::is_chord(key)
                && app
                    .approval_request
                    .as_ref()
                    .is_some_and(|request| request.allow_session) =>
        {
            Decision::AllowForSession
        }
        KeyCode::Char('n') if !super::is_chord(key) => Decision::Deny,
        KeyCode::Char('r')
            if !super::is_chord(key)
                && app
                    .approval_request
                    .as_ref()
                    .is_some_and(|request| request.tool == "wave") =>
        {
            Decision::ReviewEach
        }
        _ => return Ok(()),
    };
    if decision != Decision::Deny
        && ((decision != Decision::ReviewEach && shell_unread(app))
            || app
                .approval_opened_at
                .is_some_and(|opened| opened.elapsed() < app.approval_delay))
    {
        return Ok(());
    }
    app.execute(AppCommand::Approve { decision })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::{App, Popup};
    use nexus_core::db::Db;
    use nexus_core::space::Space;
    use nexus_core::tools::ApprovalRequest;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::{Duration, Instant};

    fn wave_app(summary: String) -> (AppView, tokio::sync::oneshot::Receiver<Decision>) {
        let root = std::env::temp_dir().join(format!("nexus-wave-popup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("k"),
            Space { root },
        ));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app.pending_approval = Some(sender);
        app.pending_approval_request = Some(ApprovalRequest {
            tool: "wave".into(),
            class: None,
            detail: None,
            action: "2 calls".into(),
            summary,
            allow_session: true,
        });
        app.sync_approval_popup();
        (app, receiver)
    }

    fn draw(app: &AppView) {
        Terminal::new(TestBackend::new(120, 24))
            .unwrap()
            .draw(|f| render(f, app))
            .unwrap();
    }

    #[test]
    fn one_time_wave_hides_and_ignores_session_approval() {
        let (mut app, mut receiver) = wave_app("1 delete".into());
        app.approval_request.as_mut().unwrap().allow_session = false;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!screen.contains("a this session"));
        handle_key(&mut app, KeyCode::Char('a').into()).unwrap();
        assert!(receiver.try_recv().is_err());
        handle_key(&mut app, KeyCode::Char('y').into()).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), Decision::Allow);
        let root = app.space.root.clone();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wave_keys_map_to_combined_decisions_and_keep_the_open_delay() {
        for (key, expected) in [
            (KeyCode::Char('y'), Decision::Allow),
            (KeyCode::Char('a'), Decision::AllowForSession),
            (KeyCode::Char('r'), Decision::ReviewEach),
            (KeyCode::Char('n'), Decision::Deny),
            (KeyCode::Esc, Decision::Deny),
        ] {
            let (mut app, mut receiver) = wave_app(
                "1 edit, 1 command\n1. fs write\n  write one\n2. shell run\n  $ true".into(),
            );
            let root = app.space.root.clone();
            draw(&app);
            app.approval_delay = Duration::from_millis(400);
            app.approval_opened_at = Some(Instant::now() + Duration::from_secs(1));
            handle_key(&mut app, key.into()).unwrap();
            if expected != Decision::Deny {
                assert!(receiver.try_recv().is_err());
                assert_eq!(app.popup, Popup::Approval);
                app.approval_opened_at = Some(Instant::now() - Duration::from_secs(1));
                handle_key(&mut app, key.into()).unwrap();
            }
            assert_eq!(receiver.try_recv().unwrap(), expected);
            assert_eq!(app.popup, Popup::None);
            assert!(app.pending_approval.is_none());
            drop(app);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn combined_shell_requires_scrolling_for_allow_but_not_review_or_denial() {
        for (key, expected) in [
            (KeyCode::Char('y'), Decision::Allow),
            (KeyCode::Char('a'), Decision::AllowForSession),
            (KeyCode::Char('r'), Decision::ReviewEach),
            (KeyCode::Esc, Decision::Deny),
        ] {
            let summary = format!(
                "1 edit, 1 command\n1. fs edit\n{}\n2. shell run\n  $ echo LAST_COMMAND",
                "  +a line of diff\n".repeat(50)
            );
            let (mut app, mut receiver) = wave_app(summary);
            let root = app.space.root.clone();
            draw(&app);
            assert!(app.approval_max_scroll.get() > 0);
            assert!(!app.approval_seen_end.get());
            handle_key(&mut app, key.into()).unwrap();
            if matches!(expected, Decision::Allow | Decision::AllowForSession) {
                assert!(receiver.try_recv().is_err());
                while app.approval_scroll.get() < app.approval_max_scroll.get() {
                    handle_key(&mut app, KeyCode::PageDown.into()).unwrap();
                    draw(&app);
                }
                assert!(app.approval_seen_end.get());
                handle_key(&mut app, key.into()).unwrap();
            }
            assert_eq!(receiver.try_recv().unwrap(), expected);
            drop(app);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
