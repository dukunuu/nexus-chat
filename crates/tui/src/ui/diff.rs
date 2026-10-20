//! Safe, shared rendering of unified diffs and untrusted tool text.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar as _;

use crate::theme::Theme;

#[derive(Clone, Copy)]
pub(super) struct Options<'a> {
    pub max_rows: Option<usize>,
    pub context: usize,
    pub hint: &'a str,
}

/// Escape controls rather than interpreting or hiding them. Newlines are
/// escaped too; callers split actual rows before invoking this function.
pub(super) fn escaped_rows(
    text: &str,
    width: usize,
    normal: Style,
    dim: Style,
) -> Vec<Line<'static>> {
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
            let row = rows.last_mut().expect("escaped text has a row");
            match row.spans.last_mut() {
                Some(span) if span.style == style => span.content.to_mut().push(c),
                _ => row.spans.push(Span::styled(c.to_string(), style)),
            }
            used += cells;
        }
    }
    rows
}

struct Row<'a> {
    number: usize,
    kind: char,
    text: &'a str,
    hunk: usize,
}

fn rows(diff: &str) -> Vec<Row<'_>> {
    let (mut old, mut new, mut hunk) = (0_usize, 0_usize, 0_usize);
    let mut rows = Vec::new();
    for line in diff.split_terminator('\n') {
        if line.starts_with("@@ ") {
            let mut parts = line.split_whitespace();
            let number =
                |part: Option<&str>| part?.get(1..)?.split(',').next()?.parse::<usize>().ok();
            parts.next();
            if let (Some(a), Some(b)) = (number(parts.next()), number(parts.next())) {
                old = a;
                new = b;
                hunk += 1;
            }
            continue;
        }
        if hunk == 0 {
            continue;
        }
        let (kind, number) = match line.chars().next() {
            Some('+') => ('+', new),
            Some('-') => ('−', old),
            Some(' ') => (' ', new),
            _ => ('…', 0),
        };
        rows.push(Row {
            number,
            kind,
            text: if kind == '…' { line } else { &line[1..] },
            hunk,
        });
        old = old.saturating_add(usize::from(matches!(kind, '−' | ' ')));
        new = new.saturating_add(usize::from(matches!(kind, '+' | ' ')));
    }
    rows
}

pub(super) fn counts(diff: &str) -> (usize, usize) {
    rows(diff).iter().fold((0, 0), |(added, deleted), row| {
        (
            added + usize::from(row.kind == '+'),
            deleted + usize::from(row.kind == '−'),
        )
    })
}

pub(super) fn render(
    diff: &str,
    width: usize,
    theme: &Theme,
    options: Options<'_>,
) -> Vec<Line<'static>> {
    let rows = rows(diff);
    let digits = rows
        .iter()
        .map(|row| row.number)
        .max()
        .unwrap_or(0)
        .to_string()
        .len();
    let dim = Style::default().fg(theme.fg_dim);
    let mut lines = Vec::new();
    let mut omitted = 0;
    let mut previous = None;
    for (index, row) in rows.iter().enumerate() {
        if row.kind == ' '
            && !rows.iter().enumerate().any(|(other, change)| {
                change.hunk == row.hunk
                    && matches!(change.kind, '+' | '−')
                    && index.abs_diff(other) <= options.context
            })
        {
            omitted += 1;
            continue;
        }
        if previous.is_some_and(|(last, hunk)| last + 1 != index || hunk != row.hunk) {
            lines.push(Line::from(Span::styled("┄", dim)));
        }
        previous = Some((index, row.hunk));
        let color = match row.kind {
            '+' => theme.success,
            '−' => theme.error,
            _ => theme.fg_dim,
        };
        let style = Style::default().fg(color);
        let gutter = if row.number == 0 {
            " ".repeat(digits)
        } else {
            format!("{:>digits$}", row.number)
        };
        let prefix_width = digits + 3;
        for (i, mut line) in escaped_rows(row.text, width.saturating_sub(prefix_width), style, dim)
            .into_iter()
            .enumerate()
        {
            let mut spans = if i == 0 {
                vec![
                    Span::styled(format!("{gutter} "), dim),
                    Span::styled(
                        format!("{} ", if row.kind == '…' { ' ' } else { row.kind }),
                        style,
                    ),
                ]
            } else {
                vec![Span::styled(" ".repeat(prefix_width), dim)]
            };
            spans.append(&mut line.spans);
            lines.push(Line::from(spans));
        }
    }
    if let Some(max) = options.max_rows {
        if lines.len() > max || omitted > 0 {
            let keep = max.saturating_sub(1).min(lines.len());
            omitted += lines.len() - keep;
            lines.truncate(keep);
            lines.push(Line::from(Span::styled(
                format!("… {omitted} more lines{}", options.hint),
                dim,
            )));
        }
    } else if omitted > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {omitted} more lines"),
            dim,
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget};

    fn render_diff(
        text: &str,
        width: usize,
        max_rows: Option<usize>,
        context: usize,
    ) -> Vec<Line<'static>> {
        render(
            text,
            width,
            &Theme::default(),
            Options {
                max_rows,
                context,
                hint: " · Ctrl+T",
            },
        )
    }

    #[test]
    fn numbers_hunks_colors_wraps_and_controls() {
        let source = "--- a/x\n+++ b/x\n@@ -41,2 +41,2 @@\n context\n-old\n+new\x1b[2J\n@@ -118 +120 @@\n-last\n+abcdefghijklmnop";
        let theme = Theme::default();
        let lines = render_diff(source, 15, None, 3);
        let text: Vec<_> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(text[0], " 41   context");
        assert!(text.iter().any(|line| line == "┄"));
        assert!(text.iter().any(|line| line.starts_with("118 − last")));
        assert!(text.iter().any(|line| line.starts_with("120 + ")));
        assert!(text.iter().any(|line| line == "      jklmnop"));
        assert!(!text.join("\n").contains('\x1b'));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 30));
        Paragraph::new(render_diff(source, 40, None, 3)).render(buffer.area, &mut buffer);
        assert_eq!(buffer[(6, 0)].fg, theme.fg_dim);
        assert_eq!(buffer[(4, 1)].fg, theme.error);
        assert_eq!(buffer[(6, 2)].fg, theme.success);
        assert_eq!(buffer[(9, 2)].symbol(), "\\");
        assert_eq!(buffer[(9, 2)].fg, theme.fg_dim);
        assert_eq!(buffer[(0, 1)].fg, theme.fg_dim);
        assert_eq!(counts(source), (2, 2));
    }

    #[test]
    fn context_and_row_limits_report_hidden_lines() {
        let source = "@@ -1,8 +1,8 @@\n a\n b\n c\n-old\n+new\n d\n e\n f\n g";
        let lines = render_diff(source, 60, Some(8), 1);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("1   a"));
        assert!(text.contains("3   c"));
        assert!(text.contains("… 5 more lines · Ctrl+T"));
        let limited = render_diff(source, 60, Some(3), 3);
        assert_eq!(limited.len(), 3);
        assert!(limited[2].to_string().contains("more lines · Ctrl+T"));
        let tabs = escaped_rows(
            "a\tb\r",
            80,
            Style::default(),
            Style::new().fg(Theme::default().fg_dim),
        );
        assert_eq!(tabs[0].to_string(), "a    b\\u{d}");
        let controls = render_diff("@@ -0,0 +1 @@\n+hello\r\n", 80, None, 3);
        assert_eq!(controls[0].to_string(), "1 + hello\\u{d}");
    }
}
