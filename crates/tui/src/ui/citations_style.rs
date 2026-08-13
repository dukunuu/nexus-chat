//! Styled rendering for report citations: re-styling the `[n]` inline
//! markers (and `‹low›`/`‹med›` confidence tags) across already-rendered
//! ratatui lines. The parsing half of citations lives in core
//! (`nexus_core::citations`); this is the terminal-colors half.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Re-style every `[n]` citation marker across already-rendered `lines`
/// with `accent`; everything else keeps its existing style. Splits spans as
/// needed, so a citation embedded mid-span still gets its own styled piece.
pub fn style_citations(lines: Vec<Line<'static>>, accent: Color) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let alignment = line.alignment;
            let style = line.style;
            let mut spans = Vec::new();
            for span in line.spans {
                spans.extend(split_citation_span(&span, accent));
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
fn split_citation_span(span: &Span<'static>, accent: Color) -> Vec<Span<'static>> {
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
pub fn style_confidence_tags(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let alignment = line.alignment;
            let style = line.style;
            let mut spans = Vec::new();
            for span in line.spans {
                spans.extend(split_confidence_span(&span));
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
fn split_confidence_span(span: &Span<'static>) -> Vec<Span<'static>> {
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
        out.push(Span::styled(
            rest[start..end].to_string(),
            span.style.add_modifier(Modifier::DIM),
        ));
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
    fn style_citations_restyles_bracketed_numbers_and_preserves_the_rest() {
        let lines = vec![Line::from(vec![
            Span::raw("supported by "),
            Span::raw("[1]"),
            Span::raw(" evidence"),
        ])];
        let out = style_citations(lines, Color::Cyan);
        assert_eq!(out.len(), 1);
        let has_accent = out[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "[1]" && s.style.fg == Some(Color::Cyan));
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
        let lines = vec![Line::from(
            "Some claim [1] \u{2039}low\u{203a}. Another [2] \u{2039}med\u{203a}.",
        )];
        let styled = style_confidence_tags(lines);
        let tag_spans: Vec<_> = styled[0]
            .spans
            .iter()
            .filter(|s| s.content.contains('\u{2039}'))
            .collect();
        assert_eq!(tag_spans.len(), 2);
        assert!(
            tag_spans
                .iter()
                .all(|s| s.style.add_modifier.contains(ratatui::style::Modifier::DIM))
        );
    }

    #[test]
    fn style_confidence_tags_leaves_lines_without_tags_untouched() {
        use ratatui::text::Line;
        let lines = vec![Line::from("Plain claim, high confidence, no tag.")];
        let styled = style_confidence_tags(lines.clone());
        assert_eq!(styled[0].spans.len(), lines[0].spans.len());
    }
}
