//! Pure parsing/styling for report citations: the trailing `Sources:` (or
//! `## Sources`) list research/web-mode replies end with (see
//! `WRITER_PROMPT`/`SEARCHER_PROMPT` in `app/research.rs` and
//! `web_mode_clause` in `app/chat.rs`), and the `[n]` inline markers that
//! reference it.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Parse a message's trailing citation list into `(n, url)` pairs, in the
/// order they're listed. Recognizes a `Sources:` line or a `Sources` heading
/// (any level), then reads `N. url` / `N) url` lines until a non-matching
/// line ends the section.
pub(crate) fn parse_citations(content: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in content.lines() {
        let t = line.trim();
        if !in_section {
            let heading = t.trim_start_matches('#').trim();
            if t.eq_ignore_ascii_case("Sources:") || heading.eq_ignore_ascii_case("Sources") {
                in_section = true;
            }
            continue;
        }
        if t.is_empty() {
            continue;
        }
        let Some((num, rest)) = t.split_once(['.', ')']) else { break };
        let Ok(n) = num.trim().parse::<usize>() else { break };
        let url = rest.trim().to_string();
        if url.is_empty() {
            break;
        }
        out.push((n, url));
    }
    out
}

/// The first `[n]` (n = 1+ ascii digits) substring in `text`, if any.
pub(crate) fn citation_number_in(text: &str) -> Option<usize> {
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find(']') else { return None };
        let inner = &rest[..end];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return inner.parse().ok();
        }
        rest = &rest[end + 1..];
    }
    None
}

/// Re-style every `[n]` citation marker across already-rendered `lines`
/// with `accent`; everything else keeps its existing style. Splits spans as
/// needed, so a citation embedded mid-span still gets its own styled piece.
pub(crate) fn style_citations(lines: Vec<Line<'static>>, accent: Color) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let alignment = line.alignment;
            let style = line.style;
            let mut spans = Vec::new();
            for span in line.spans {
                spans.extend(split_citation_span(span, accent));
            }
            let mut out = Line::from(spans);
            out.alignment = alignment;
            out.style = style;
            out
        })
        .collect()
}

/// Split one span so each `[n]` substring becomes its own accent-styled
/// span; everything else keeps the original span's style.
fn split_citation_span(span: Span<'static>, accent: Color) -> Vec<Span<'static>> {
    let text = span.content.to_string();
    let mut out = Vec::new();
    let mut rest = text.as_str();
    loop {
        let Some(start) = rest.find('[') else {
            if !rest.is_empty() {
                out.push(Span::styled(rest.to_string(), span.style));
            }
            break;
        };
        let Some(end_rel) = rest[start + 1..].find(']') else {
            out.push(Span::styled(rest.to_string(), span.style));
            break;
        };
        let end = start + 1 + end_rel;
        let inner = &rest[start + 1..end];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            if start > 0 {
                out.push(Span::styled(rest[..start].to_string(), span.style));
            }
            out.push(Span::styled(
                rest[start..=end].to_string(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ));
            rest = &rest[end + 1..];
        } else {
            out.push(Span::styled(rest[..=start].to_string(), span.style));
            rest = &rest[start + 1..];
        }
    }
    out
}

/// Re-style every `‹low›`/`‹med›` confidence tag the Verifier stage emits
/// (see `VERIFIER_PROMPT` in `app/research.rs`) with a dim modifier;
/// everything else keeps its existing style. High confidence is the
/// default (unmarked), so there's nothing to style for it.
pub(crate) fn style_confidence_tags(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let alignment = line.alignment;
            let style = line.style;
            let mut spans = Vec::new();
            for span in line.spans {
                spans.extend(split_confidence_span(span));
            }
            let mut out = Line::from(spans);
            out.alignment = alignment;
            out.style = style;
            out
        })
        .collect()
}

/// Split one span so each `‹...›` confidence tag becomes its own
/// dim-styled span; everything else keeps the original span's style.
fn split_confidence_span(span: Span<'static>) -> Vec<Span<'static>> {
    let text = span.content.to_string();
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('\u{2039}') {
        if start > 0 {
            out.push(Span::styled(rest[..start].to_string(), span.style));
        }
        let Some(end_rel) = rest[start..].find('\u{203a}') else {
            out.push(Span::styled(rest[start..].to_string(), span.style));
            return out;
        };
        let end = start + end_rel + '\u{203a}'.len_utf8();
        out.push(Span::styled(rest[start..end].to_string(), span.style.add_modifier(Modifier::DIM)));
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        out.push(Span::styled(rest.to_string(), span.style));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_citations_reads_a_sources_heading_section() {
        let content =
            "# Report\n\nBody text [1] and more [2].\n\n## Sources\n1. https://a.example\n2. https://b.example/page\n";
        assert_eq!(
            parse_citations(content),
            vec![(1, "https://a.example".into()), (2, "https://b.example/page".into())]
        );
    }

    #[test]
    fn parse_citations_reads_a_plain_sources_colon_line() {
        let content = "findings text [1]\nSources:\n1. https://a.example\n";
        assert_eq!(parse_citations(content), vec![(1, "https://a.example".into())]);
    }

    #[test]
    fn parse_citations_returns_empty_when_no_sources_section() {
        assert!(parse_citations("just prose, no citations").is_empty());
    }

    #[test]
    fn citation_number_in_finds_first_bracketed_number() {
        assert_eq!(citation_number_in("supported by research [3] and also [4]"), Some(3));
        assert_eq!(citation_number_in("no citation here"), None);
        assert_eq!(citation_number_in("[not a number] but [5] later"), Some(5));
        assert_eq!(citation_number_in("[not a number]"), None);
    }

    #[test]
    fn style_citations_restyles_bracketed_numbers_and_preserves_the_rest() {
        let lines = vec![Line::from(vec![Span::raw("supported by "), Span::raw("[1]"), Span::raw(" evidence")])];
        let out = style_citations(lines, Color::Cyan);
        assert_eq!(out.len(), 1);
        let has_accent =
            out[0].spans.iter().any(|s| s.content.as_ref() == "[1]" && s.style.fg == Some(Color::Cyan));
        assert!(has_accent);
        let plain: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, "supported by [1] evidence");
    }

    #[test]
    fn style_citations_ignores_non_numeric_brackets() {
        let lines = vec![Line::from(Span::raw("a [note] here"))];
        let out = style_citations(lines, Color::Cyan);
        assert!(out[0].spans.iter().all(|s| s.style.fg != Some(Color::Cyan)));
        let plain: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, "a [note] here");
    }

    #[test]
    fn style_confidence_tags_dims_low_and_med_tags() {
        use ratatui::text::Line;
        let lines = vec![Line::from("Some claim [1] \u{2039}low\u{203a}. Another [2] \u{2039}med\u{203a}.")];
        let styled = style_confidence_tags(lines);
        let tag_spans: Vec<_> = styled[0].spans.iter().filter(|s| s.content.contains('\u{2039}')).collect();
        assert_eq!(tag_spans.len(), 2);
        assert!(tag_spans.iter().all(|s| s.style.add_modifier.contains(ratatui::style::Modifier::DIM)));
    }

    #[test]
    fn style_confidence_tags_leaves_lines_without_tags_untouched() {
        use ratatui::text::Line;
        let lines = vec![Line::from("Plain claim, high confidence, no tag.")];
        let styled = style_confidence_tags(lines.clone());
        assert_eq!(styled[0].spans.len(), lines[0].spans.len());
    }
}
