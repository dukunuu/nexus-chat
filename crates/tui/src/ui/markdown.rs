//! Styled markdown rendering for the history pane (`nexus_core::markdown`
//! holds the plain-text copy path and the GFM table splitter).
//! `tui_markdown` styles inline emphasis/code and highlights fenced code, but
//! leaves block markers as literal text (`# `, `- `, ```` ``` ````); we strip
//! those so the display — and anything copied from it — is clean text.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tui_markdown::{DefaultStyleSheet, Options, StyleSheet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use nexus_core::markdown::{TableAlign, TableSegment};

pub fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Terminal column width of one char (0 for combining marks, 2 for CJK/emoji).
fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// `tui_markdown`'s default inline-code style is white-on-black, which is
/// invisible against the (very common) black-background terminal. Everything
/// else defers to the library's own defaults.
#[derive(Clone, Copy, Debug, Default)]
struct NexusStyleSheet;

impl StyleSheet for NexusStyleSheet {
    fn heading(&self, level: u8) -> Style {
        DefaultStyleSheet.heading(level)
    }
    fn code(&self) -> Style {
        Style::new().fg(Color::Yellow).bg(Color::DarkGray)
    }
    fn link(&self) -> Style {
        DefaultStyleSheet.link()
    }
    fn blockquote(&self) -> Style {
        DefaultStyleSheet.blockquote()
    }
    fn heading_meta(&self) -> Style {
        DefaultStyleSheet.heading_meta()
    }
    fn metadata_block(&self) -> Style {
        DefaultStyleSheet.metadata_block()
    }
}

fn md_options() -> Options<NexusStyleSheet> {
    Options::new(NexusStyleSheet)
}

/// Rendered markdown: styled/wrapped `lines`, plus, per line, which fenced code
/// block it belongs to (`code[i]`), and the raw text of each block (`blocks`).
#[derive(Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub code: Vec<Option<usize>>,
    pub blocks: Vec<String>,
}

impl Rendered {
    fn push(&mut self, line: Line<'static>, code: Option<usize>) {
        self.lines.push(line);
        self.code.push(code);
    }
}

/// Render `content` to styled, width-wrapped lines. Fenced code blocks get a box
/// drawn around them and are tracked so a long-press can copy the raw code.
/// GFM pipe tables — which `tui_markdown` doesn't support (it just warns and
/// drops them) — are pulled out and rendered as a bordered, column-aligned
/// table before the rest of the content goes through the normal pipeline.
/// `heading` colors `#` headings (the theme accent).
pub fn render(content: &str, width: usize, heading: Color) -> Rendered {
    let mut r = Rendered::default();
    for seg in nexus_core::markdown::split_tables(content) {
        match seg {
            TableSegment::Table(rows, aligns) => render_table(&mut r, &rows, &aligns, width),
            TableSegment::Text(text) => render_text(&mut r, &text, width, heading),
        }
    }
    r
}

fn render_text(r: &mut Rendered, content: &str, width: usize, heading: Color) {
    let text = tui_markdown::from_str_with_options(content, &md_options());
    let mut in_code = false;
    let mut raw: Vec<String> = Vec::new();

    for line in &text.lines {
        let plain = line_text(line);
        let unstyled = line.spans.iter().all(|s| s.style == Style::default());

        // Fence line toggles a code block; the fence itself isn't shown.
        if unstyled && plain.trim_start().starts_with("```") {
            if in_code {
                in_code = false;
                push_code_border(r, width, false);
                r.blocks.push(raw.join("\n"));
            } else {
                in_code = true;
                raw.clear();
                push_code_border(r, width, true);
            }
            continue;
        }

        if in_code {
            raw.push(plain);
            push_code_content(r, line, width);
            continue;
        }

        let id = None;
        match classify(line, &plain, heading) {
            Block::Drop => {}
            Block::Header(body) | Block::List(body) => {
                for l in wrap_styled_line(&body, width) {
                    r.push(l, id);
                }
            }
            Block::Plain => {
                for l in wrap_styled_line(line, width) {
                    r.push(l, id);
                }
            }
        }
    }

    // Unterminated block (e.g. mid-stream): close it so metadata stays valid.
    if in_code {
        push_code_border(r, width, false);
        r.blocks.push(raw.join("\n"));
    }
}

fn border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Top (`top=true`) or bottom rule of a code box, tagged with the current block id.
fn push_code_border(r: &mut Rendered, width: usize, top: bool) {
    let id = Some(r.blocks.len());
    if width < 2 {
        r.push(Line::from(""), id);
        return;
    }
    let (l, rt) = if top { ('┌', '┐') } else { ('└', '┘') };
    let bar = format!("{l}{}{rt}", "─".repeat(width - 2));
    r.push(Line::from(Span::styled(bar, border_style())), id);
}

/// A code content line: wrapped to the box interior, framed with `│ … │`.
fn push_code_content(r: &mut Rendered, line: &Line, width: usize) {
    let id = Some(r.blocks.len());
    let interior = width.saturating_sub(4).max(1);
    for row in wrap_styled_line(line, interior) {
        let used: usize = row.spans.iter().map(|s| s.content.width()).sum();
        let pad = interior.saturating_sub(used);
        let mut spans: Vec<Span<'static>> = vec![Span::styled("│ ", border_style())];
        spans.extend(row.spans);
        spans.push(Span::styled(
            format!("{} │", " ".repeat(pad)),
            border_style(),
        ));
        r.push(Line::from(spans), id);
    }
}

/// Render a parsed table as a bordered, column-aligned box. Cell text still
/// gets inline styling (bold/italic/code) via `tui_markdown`, and wraps within
/// its column if the table doesn't fit `width`.
fn render_table(r: &mut Rendered, rows: &[Vec<String>], aligns: &[TableAlign], width: usize) {
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if ncols == 0 {
        return;
    }
    let mut colw = vec![1usize; ncols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            colw[i] = colw[i].max(cell.width().max(1));
        }
    }
    // Shrink the widest columns to fit `width` (border + " x " padding per
    // column), down to a 3-char floor so cells stay legible.
    let overhead = (ncols + 1) + 2 * ncols;
    let avail = width.saturating_sub(overhead);
    let min_w = 3;
    while colw.iter().sum::<usize>() > avail && colw.iter().any(|&w| w > min_w) {
        let idx = colw.iter().enumerate().max_by_key(|&(_, &w)| w).unwrap().0;
        colw[idx] -= 1;
    }

    let border = |l: char, mid: char, right: char| -> Line<'static> {
        let mut s = String::from(l);
        for (i, w) in colw.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push(if i + 1 == colw.len() { right } else { mid });
        }
        Line::from(Span::styled(s, border_style()))
    };

    r.push(border('┌', '┬', '┐'), None);
    for (ri, row) in rows.iter().enumerate() {
        push_table_row(r, row, aligns, &colw, ri == 0);
        if ri == 0 {
            r.push(border('├', '┼', '┤'), None);
        }
    }
    r.push(border('└', '┴', '┘'), None);
}

/// One logical table row, possibly wrapping to several physical lines if a
/// cell doesn't fit its column.
fn push_table_row(
    r: &mut Rendered,
    row: &[String],
    aligns: &[TableAlign],
    colw: &[usize],
    is_header: bool,
) {
    let wrapped: Vec<Vec<Line<'static>>> = (0..colw.len())
        .map(|i| {
            let mut spans = styled_cell(row.get(i).map_or("", String::as_str));
            if is_header {
                for s in &mut spans {
                    s.style = s.style.add_modifier(Modifier::BOLD);
                }
            }
            wrap_styled_line(&Line::from(spans), colw[i])
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);

    for li in 0..height {
        let mut spans: Vec<Span<'static>> = vec![Span::styled("│", border_style())];
        for (i, w) in colw.iter().enumerate() {
            let cell = wrapped[i].get(li);
            let used: usize = cell.map_or(0, |l| l.spans.iter().map(|s| s.content.width()).sum());
            let pad = w.saturating_sub(used);
            let (lpad, rpad) = match aligns.get(i).copied().unwrap_or(TableAlign::Left) {
                TableAlign::Left => (0, pad),
                TableAlign::Right => (pad, 0),
                TableAlign::Center => (pad / 2, pad - pad / 2),
            };
            spans.push(Span::raw(format!(" {}", " ".repeat(lpad))));
            if let Some(l) = cell {
                spans.extend(l.spans.clone());
            }
            spans.push(Span::raw(format!("{} ", " ".repeat(rpad))));
            spans.push(Span::styled("│", border_style()));
        }
        r.push(Line::from(spans), None);
    }
}

/// Inline-styled spans for one table cell (bold/italic/code), via
/// `tui_markdown`'s single-line rendering of the cell's own text.
fn styled_cell(text: &str) -> Vec<Span<'static>> {
    let rendered = tui_markdown::from_str_with_options(text, &md_options());
    rendered
        .lines
        .into_iter()
        .next()
        .map(|l| {
            l.spans
                .into_iter()
                .map(|s| Span::styled(s.content.into_owned(), s.style))
                .collect()
        })
        .unwrap_or_default()
}

enum Block {
    Drop,
    Header(Line<'static>),
    List(Line<'static>),
    Plain,
}

/// Decide how a rendered markdown line should be treated. Fenced code is
/// handled before this, so a styled line here is prose with inline
/// formatting (`code`, **bold**): its block marker is rewritten inside the
/// first span and the other spans keep their styles.
fn classify(line: &Line, plain: &str, heading: Color) -> Block {
    let trimmed = plain.trim_start();
    let unstyled = line.spans.iter().all(|s| s.style == Style::default());
    if unstyled && trimmed.starts_with("```") {
        return Block::Drop;
    }
    let indent = plain.len() - trimmed.len();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        let header_style = Style::default().fg(heading).add_modifier(Modifier::BOLD);
        if let Some(mut l) = replace_prefix(line, indent + hashes + 1, "") {
            // `##   Heading`: extra spaces after the marker aren't content.
            if let Some(first) = l.spans.first_mut() {
                first.content = first.content.trim_start().to_string().into();
            }
            for span in &mut l.spans {
                span.style = span.style.patch(header_style);
            }
            return Block::Header(l);
        }
    }
    for marker in ["- ", "* ", "+ "] {
        if trimmed.starts_with(marker) {
            let bullet = format!("{}• ", &plain[..indent]);
            if let Some(l) = replace_prefix(line, indent + marker.len(), &bullet) {
                return Block::List(l);
            }
        }
    }
    Block::Plain
}

/// `line` with its first `len` bytes (which must sit inside the first span)
/// replaced by `with`; `None` when the prefix straddles a span boundary.
fn replace_prefix(line: &Line, len: usize, with: &str) -> Option<Line<'static>> {
    let first = line.spans.first()?;
    let rest = first.content.get(len..)?;
    let mut spans = Vec::with_capacity(line.spans.len());
    let head = format!("{with}{rest}");
    if !head.is_empty() {
        spans.push(Span::styled(head, first.style));
    }
    spans.extend(
        line.spans[1..]
            .iter()
            .map(|s| Span::styled(s.content.to_string(), s.style)),
    );
    Some(Line::from(spans))
}

/// Word-wrap a styled `Line` to `width` terminal columns, preserving per-span
/// styling. Wraps by display width (CJK/emoji are 2 columns), not char count,
/// so wide-glyph content — like a Japanese vocab table — doesn't overflow its
/// budget. Mouse selection still maps by char index (`selection.rs`), a
/// close-enough approximation for wide glyphs.
fn wrap_styled_line(line: &Line, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|sp| sp.content.chars().map(|c| (c, sp.style)))
        .collect();

    let mut rows: Vec<Vec<(char, Style)>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    let mut word: Vec<(char, Style)> = Vec::new();

    for (c, st) in chars {
        if c == ' ' {
            place_word(&mut rows, &mut cur, &mut word, width);
            if !cur.is_empty() {
                if width_of(&cur) < width {
                    cur.push((' ', st));
                } else {
                    rows.push(std::mem::take(&mut cur));
                }
            }
        } else {
            word.push((c, st));
        }
    }
    place_word(&mut rows, &mut cur, &mut word, width);
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows.into_iter().map(row_to_line).collect()
}

fn width_of(v: &[(char, Style)]) -> usize {
    v.iter().map(|&(c, _)| char_width(c)).sum()
}

/// Flush the accumulated `word` into the current row, wrapping (and hard-breaking
/// over-long words) as needed. All measured in display columns.
fn place_word(
    rows: &mut Vec<Vec<(char, Style)>>,
    cur: &mut Vec<(char, Style)>,
    word: &mut Vec<(char, Style)>,
    width: usize,
) {
    if word.is_empty() {
        return;
    }
    let w = std::mem::take(word);
    if width_of(&w) > width {
        if !cur.is_empty() {
            rows.push(std::mem::take(cur));
        }
        let mut chunk = Vec::new();
        let mut chunk_w = 0;
        for ch in w {
            let cw = char_width(ch.0);
            if chunk_w + cw > width && !chunk.is_empty() {
                rows.push(std::mem::take(&mut chunk));
                chunk_w = 0;
            }
            chunk.push(ch);
            chunk_w += cw;
        }
        *cur = chunk;
    } else {
        if width_of(cur) + width_of(&w) > width {
            rows.push(std::mem::take(cur));
        }
        cur.extend(w);
    }
}

/// Rebuild an owned `Line` from a row of styled chars, merging same-style runs.
fn row_to_line(row: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur_style: Option<Style> = None;
    for (c, st) in row {
        if cur_style == Some(st) {
            buf.push(c);
        } else {
            if let Some(s) = cur_style {
                spans.push(Span::styled(std::mem::take(&mut buf), s));
            }
            buf.push(c);
            cur_style = Some(st);
        }
    }
    if let Some(s) = cur_style {
        spans.push(Span::styled(buf, s));
    }
    Line::from(spans)
}

#[cfg(test)]
mod inline_code_tests {
    use super::*;

    #[test]
    fn inline_code_is_visible_on_a_black_background_terminal() {
        let r = render("run `cargo test` now", 80, Color::Cyan);
        let code_span = r.lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "cargo test");
        let span = code_span.expect("inline code span not found");
        // Not the library default (white-on-black — invisible on a black bg).
        assert_ne!(
            span.style,
            Style::default().fg(Color::White).bg(Color::Black)
        );
        assert_eq!(span.style.bg, Some(Color::DarkGray));
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    const TABLE: &str = "| Name | Age |\n| --- | ---: |\n| Alice | 30 |\n| Bob | 7 |";

    #[test]
    fn cjk_columns_stay_aligned() {
        // Double-width glyphs must not desync the border from the content —
        // every row's rendered display width has to match the border's.
        let table = "| 単語 | 読み |\n| --- | --- |\n| 会う | あう |\n| 会社 | かいしゃ |";
        let r = render(table, 40, Color::Cyan);
        let widths: Vec<usize> = r
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.width()).sum())
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "row widths: {widths:?}"
        );
    }

    #[test]
    fn render_produces_a_bordered_box_with_header_separator() {
        let r = render(TABLE, 40, Color::Cyan);
        let text: Vec<String> = r.lines.iter().map(line_text).collect();
        // top border, header, header/body separator, 2 data rows, bottom border.
        assert_eq!(text.len(), 6);
        assert!(text[0].starts_with('┌') && text[0].ends_with('┐'));
        assert!(text[1].contains("Name") && text[1].contains("Age"));
        assert!(text[2].starts_with('├') && text[2].ends_with('┤'));
        assert!(text[5].starts_with('└') && text[5].ends_with('┘'));
    }

    /// Inline formatting used to block marker stripping: a bullet or heading
    /// containing `code` kept its raw `- ` / `## ` while plain ones didn't.
    #[test]
    fn markers_are_stripped_from_lines_with_inline_formatting() {
        let r = render(
            "- plain item\n- has `ip` inside\n\n## Use `nmcli` here",
            80,
            Color::Cyan,
        );
        let text: Vec<String> = r.lines.iter().map(line_text).collect();
        assert!(text.iter().any(|l| l == "• plain item"), "{text:?}");
        assert!(
            text.iter()
                .any(|l| l.starts_with("• has ") && l.contains("ip")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.starts_with("Use ") && l.contains("nmcli")),
            "{text:?}"
        );
        assert!(
            !text
                .iter()
                .any(|l| l.starts_with("- ") || l.starts_with('#')),
            "{text:?}"
        );
    }
}
