//! Pure parsing for report citations: the trailing `Sources:` (or
//! `## Sources`) list research/web-mode replies end with (see
//! `WRITER_PROMPT`/`SEARCHER_PROMPT` in `app/research.rs` and
//! `web_mode_clause` in `app/chat.rs`), and the `[n]` inline markers that
//! reference it. Parsing is data — it lives here in core. Styling the
//! markers into terminal colors is rendering — it lives in the TUI crate
//! (`ui/citations_style.rs`).

/// Parse a message's trailing citation list into `(n, url)` pairs, in the
/// order they're listed. Recognizes a `Sources:` line or a `Sources` heading
/// (any level), then reads `N. url` / `N) url` lines until a non-matching
/// line ends the section.
pub fn parse_citations(content: &str) -> Vec<(usize, String)> {
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
        let Some((num, rest)) = t.split_once(['.', ')']) else {
            break;
        };
        let Ok(n) = num.trim().parse::<usize>() else {
            break;
        };
        let url = rest.trim().to_string();
        if url.is_empty() {
            break;
        }
        out.push((n, url));
    }
    out
}

/// The first `[n]` (n = 1+ ascii digits) substring in `text`, if any.
pub fn citation_number_in(text: &str) -> Option<usize> {
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        rest = &rest[start + 1..];
        let end = rest.find(']')?;
        let inner = &rest[..end];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return inner.parse().ok();
        }
        rest = &rest[end + 1..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_citations_reads_a_sources_heading_section() {
        let content = "# Report\n\nBody text [1] and more [2].\n\n## Sources\n1. https://a.example\n2. https://b.example/page\n";
        assert_eq!(
            parse_citations(content),
            vec![
                (1, "https://a.example".into()),
                (2, "https://b.example/page".into())
            ]
        );
    }

    #[test]
    fn parse_citations_reads_a_plain_sources_colon_line() {
        let content = "findings text [1]\nSources:\n1. https://a.example\n";
        assert_eq!(
            parse_citations(content),
            vec![(1, "https://a.example".into())]
        );
    }

    #[test]
    fn parse_citations_returns_empty_when_no_sources_section() {
        assert!(parse_citations("just prose, no citations").is_empty());
    }

    #[test]
    fn citation_number_in_finds_first_bracketed_number() {
        assert_eq!(
            citation_number_in("supported by research [3] and also [4]"),
            Some(3)
        );
        assert_eq!(citation_number_in("no citation here"), None);
        assert_eq!(citation_number_in("[not a number] but [5] later"), Some(5));
        assert_eq!(citation_number_in("[not a number]"), None);
    }
}
