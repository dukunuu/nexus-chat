//! Conservative byte-token windows for `EmbeddingGemma` 2, retaining source lines.

/// Each UTF-8 byte is counted as one token (a conservative upper bound for
/// byte-fallback tokenizers). Reserve the actual document prefix plus 32
/// special tokens inside a 1,000-token target, far below the 8,192 budget.
/// Windows overlap by up to 100 bytes, including when one line is very long.
#[must_use]
pub fn chunk_text(text: &str, filename: &str) -> Vec<(String, String)> {
    let prefix = format!("title: {filename} | text: ");
    let budget = 1000_usize.saturating_sub(prefix.len() + 32).max(4);
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut first_line = 1;
    while start < text.len() {
        let mut end = (start + budget).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end < text.len()
            && let Some(newline) = text[start..end].rfind('\n')
            && newline > budget / 2
        {
            end = start + newline + 1;
        }
        let chunk = &text[start..end];
        let last_line = first_line
            + chunk
                .trim_end_matches('\n')
                .bytes()
                .filter(|b| *b == b'\n')
                .count();
        if !chunk.trim().is_empty() {
            chunks.push((format!("lines {first_line}-{last_line}"), chunk.to_string()));
        }
        if end == text.len() {
            break;
        }
        let mut next = end.saturating_sub(100).max(start + 1);
        while !text.is_char_boundary(next) {
            next += 1;
        }
        first_line += text[start..next].bytes().filter(|b| *b == b'\n').count();
        start = next;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_bound_long_lines_unicode_and_prefix_with_overlap() {
        for text in [
            "word ".repeat(6000),
            "漢字🙂".repeat(3000),
            "line\n".repeat(4000),
        ] {
            let chunks = chunk_text(&text, "notes.txt");
            assert!(chunks.len() > 1);
            for (loc, chunk) in &chunks {
                let input = format!("title: notes.txt | text: {chunk}");
                assert!(input.len() + 32 <= 1000);
                assert!(input.len() + 32 < 8192);
                assert!(loc.starts_with("lines "));
                assert!(text.contains(chunk));
            }
            for pair in chunks.windows(2) {
                let previous = &pair[0].1;
                let next = &pair[1].1;
                assert!(
                    (1..=100).any(|n| previous.is_char_boundary(previous.len() - n)
                        && next.starts_with(&previous[previous.len() - n..]))
                );
            }
            assert!(text.ends_with(&chunks.last().unwrap().1));
        }
        assert!(chunk_text(" \n", "empty.txt").is_empty());
        assert_eq!(chunk_text("first\nsecond", "a")[0].0, "lines 1-2");
    }
}
