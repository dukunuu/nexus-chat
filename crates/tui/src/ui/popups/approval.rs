use crossterm::event::{KeyCode, KeyEvent};
use nexus_core::app::AppCommand;
use nexus_core::tools::Decision;
use ratatui::Frame;
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

pub fn render(f: &mut Frame, app: &AppView) {
    let Some(request) = &app.approval_request else {
        return;
    };
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::WIDE.1);
    let inner = chrome::popup_block_focused("", &app.theme, true, chrome::Tone::Normal).inner(area);
    let width = usize::from(inner.width);
    let height = usize::from(inner.height);
    let dim = Style::default().fg(app.theme.fg_dim);
    let wave = request.tool == "wave";
    let lines = if wave {
        wave_lines(&request.summary, width, &app.theme)
    } else {
        request_lines(
            &request.tool,
            &request.action,
            &request.summary,
            request.class.as_deref(),
            width,
            &app.theme,
        )
    };
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
    let session = match request.tool.as_str() {
        "fs" => "a allow edits this session",
        "shell" | "proc" => "a allow commands this session",
        _ => "a allow for this session",
    };
    let scroll_hint = format!("↑↓ scroll · y allow · {session} · n deny");
    let session_hint = format!("y allow · {session} · n deny");
    let hint = if wave && shell_unread(app) {
        // Short enough to fit the popup's footer on an 80-column terminal.
        "↓ read to the end to approve · r review each · n deny all"
    } else if wave && !request.allow_session {
        "y approve all · r review each · n deny all"
    } else if wave {
        "y approve all · a this session · r review each · n deny all"
    } else if shell_unread(app) {
        "↑↓ scroll · scroll to the end to allow · n deny"
    } else if !request.allow_session && max_scroll > 0 {
        "↑↓ scroll · y allow · n deny"
    } else if !request.allow_session {
        "y allow · n deny"
    } else if max_scroll > 0 {
        &scroll_hint
    } else {
        &session_hint
    };
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
    render_footer(f, app, area, hint, offset, max_scroll);
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

fn render_footer(
    f: &mut Frame,
    app: &AppView,
    area: ratatui::layout::Rect,
    hint: &str,
    offset: usize,
    max_scroll: usize,
) {
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
