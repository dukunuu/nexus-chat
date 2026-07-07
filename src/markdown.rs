//! Markdown rendering for the history pane. `tui_markdown` styles inline
//! emphasis/code and highlights fenced code, but leaves block markers as literal
//! text (`# `, `- `, ```` ``` ````). We strip those so the display — and anything
//! copied from it — is clean text.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tui_markdown::{DefaultStyleSheet, Options, StyleSheet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::line_text;

/// Terminal column width of one char (0 for combining marks, 2 for CJK/emoji).
fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// tui_markdown's default inline-code style is white-on-black, which is
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
pub fn render(content: &str, width: usize) -> Rendered {
    let mut r = Rendered::default();
    for seg in split_tables(content) {
        match seg {
            Segment::Table(rows, aligns) => render_table(&mut r, &rows, &aligns, width),
            Segment::Text(text) => render_text(&mut r, &text, width),
        }
    }
    r
}

fn render_text(r: &mut Rendered, content: &str, width: usize) {
    let text = tui_markdown::from_str_with_options(content, &md_options());
    let mut in_code = false;
    let mut raw: Vec<String> = Vec::new();

    for line in &text.lines {
        let plain = line_text(line);
        let unstyled = line.spans.iter().all(|s| s.style == Style::default());

        // Fence line toggles a code block; the fence itself isn't shown.
        if unstyled && plain.trim_start().starts_with("```") {
            if !in_code {
                in_code = true;
                raw.clear();
                push_code_border(r, width, true);
            } else {
                in_code = false;
                push_code_border(r, width, false);
                r.blocks.push(raw.join("\n"));
            }
            continue;
        }

        if in_code {
            raw.push(plain);
            push_code_content(r, line, width);
            continue;
        }

        let id = None;
        match classify(line, &plain) {
            Block::Drop => {}
            Block::Header(txt) => {
                let styled = Span::styled(
                    txt,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                for l in wrap_styled_line(&Line::from(styled), width) {
                    r.push(l, id);
                }
            }
            Block::List(txt) => {
                for l in wrap_styled_line(&Line::from(txt), width) {
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

/// Plain text of `content` with markdown markers stripped (for clipboard copy).
/// Tables are reconstructed as clean GFM markdown rather than run through
/// `tui_markdown` (which doesn't support them).
pub fn to_plain(content: &str) -> String {
    split_tables(content)
        .into_iter()
        .map(|seg| match seg {
            Segment::Table(rows, aligns) => plain_table(&rows, &aligns),
            Segment::Text(text) => plain_text_segment(&text),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plain_text_segment(content: &str) -> String {
    let text = tui_markdown::from_str(content);
    let mut lines: Vec<String> = Vec::new();
    for line in &text.lines {
        let plain = line_text(line);
        match classify(line, &plain) {
            Block::Drop => {}
            Block::Header(r) | Block::List(r) => lines.push(r),
            Block::Plain => lines.push(plain),
        }
    }
    lines.join("\n")
}

// --- GFM pipe tables ---
//
// `tui_markdown` doesn't support tables (it warns and drops them), so we pull
// table blocks out of the raw source ourselves — skipping anything inside a
// fenced code block — and render/copy them directly from the parsed cells.

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

enum Segment {
    Text(String),
    Table(Vec<Vec<String>>, Vec<Align>),
}

/// Split `content` into text and table segments. A table starts at a header
/// row immediately followed by a valid delimiter row (`| --- | :---: |`), and
/// continues while subsequent lines still look like table rows.
fn split_tables(content: &str) -> Vec<Segment> {
    let lines: Vec<&str> = content.lines().collect();
    let mut segments = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            buf.push(line);
            i += 1;
            continue;
        }
        if !in_fence && is_delimiter_row(lines.get(i + 1)) && looks_like_row(line) {
            if !buf.is_empty() {
                segments.push(Segment::Text(std::mem::take(&mut buf).join("\n")));
            }
            let aligns = parse_aligns(lines[i + 1]);
            let mut rows = vec![split_cells(line)];
            let mut j = i + 2;
            while j < lines.len() && looks_like_row(lines[j]) {
                rows.push(split_cells(lines[j]));
                j += 1;
            }
            segments.push(Segment::Table(rows, aligns));
            i = j;
            continue;
        }
        buf.push(line);
        i += 1;
    }
    if !buf.is_empty() {
        segments.push(Segment::Text(buf.join("\n")));
    }
    segments
}

/// A plausible table row: non-empty and contains at least one `|`.
fn looks_like_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.contains('|')
}

/// The GFM delimiter row: cells of only `-`/`:`, at least one `-` each.
fn is_delimiter_row(line: Option<&&str>) -> bool {
    let Some(line) = line else { return false };
    if !looks_like_row(line) {
        return false;
    }
    let cells = split_cells(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim_matches(':');
            !c.is_empty() && c.chars().all(|ch| ch == '-')
        })
}

fn parse_aligns(delim_line: &str) -> Vec<Align> {
    split_cells(delim_line)
        .iter()
        .map(|c| {
            let c = c.trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

/// `| a | b |` -> `["a", "b"]`. Doesn't handle escaped `\|` — out of scope for
/// chat-message tables.
fn split_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// Reconstruct a clean, standard GFM table from parsed cells — nicer to paste
/// elsewhere than whatever mangled text `tui_markdown` would have produced.
fn plain_table(rows: &[Vec<String>], aligns: &[Align]) -> String {
    let Some(header) = rows.first() else {
        return String::new();
    };
    let mut out = vec![format!("| {} |", header.join(" | "))];
    let delim: Vec<&str> = aligns
        .iter()
        .map(|a| match a {
            Align::Left => "---",
            Align::Right => "---:",
            Align::Center => ":---:",
        })
        .collect();
    out.push(format!("| {} |", delim.join(" | ")));
    for row in &rows[1..] {
        out.push(format!("| {} |", row.join(" | ")));
    }
    out.join("\n")
}

/// Render a parsed table as a bordered, column-aligned box. Cell text still
/// gets inline styling (bold/italic/code) via `tui_markdown`, and wraps within
/// its column if the table doesn't fit `width`.
fn render_table(r: &mut Rendered, rows: &[Vec<String>], aligns: &[Align], width: usize) {
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
    aligns: &[Align],
    colw: &[usize],
    is_header: bool,
) {
    let wrapped: Vec<Vec<Line<'static>>> = (0..colw.len())
        .map(|i| {
            let mut spans = styled_cell(row.get(i).map(String::as_str).unwrap_or(""));
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
            let used: usize = cell
                .map(|l| l.spans.iter().map(|s| s.content.width()).sum())
                .unwrap_or(0);
            let pad = w.saturating_sub(used);
            let (lpad, rpad) = match aligns.get(i).copied().unwrap_or(Align::Left) {
                Align::Left => (0, pad),
                Align::Right => (pad, 0),
                Align::Center => (pad / 2, pad - pad / 2),
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
    Header(String),
    List(String),
    Plain,
}

/// Decide how a rendered markdown line should be treated. Only *unstyled* lines
/// are candidates for block-marker stripping — styled ones are inline-formatted
/// or syntax-highlighted code, which we leave untouched.
fn classify(line: &Line, plain: &str) -> Block {
    let unstyled = line.spans.iter().all(|s| s.style == Style::default());
    if !unstyled {
        return Block::Plain;
    }
    let trimmed = plain.trim_start();
    if trimmed.starts_with("```") {
        return Block::Drop;
    }
    if let Some(rest) = header_rest(trimmed) {
        return Block::Header(rest);
    }
    if let Some(rest) = list_rest(plain) {
        return Block::List(rest);
    }
    Block::Plain
}

/// `## Heading` -> `Heading` (1–6 `#` then a space).
fn header_rest(trimmed: &str) -> Option<String> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        Some(trimmed[hashes..].trim_start().to_string())
    } else {
        None
    }
}

/// `- item` / `* item` / `+ item` -> `• item`, preserving indentation.
fn list_rest(plain: &str) -> Option<String> {
    let indent_len = plain.len() - plain.trim_start().len();
    let (indent, s) = plain.split_at(indent_len);
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return Some(format!("{indent}• {rest}"));
        }
    }
    None
}

/// Word-wrap a styled `Line` to `width` terminal columns, preserving per-span
/// styling. Wraps by display width (CJK/emoji are 2 columns), not char count,
/// so wide-glyph content — like a Japanese vocab table — doesn't overflow its
/// budget. Mouse selection still maps by char index (`selection.rs`), which
/// stays a close-enough approximation for wide glyphs, same as before.
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

/// Total display width of a run of styled chars.
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
        let r = render("run `cargo test` now", 80);
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
        let r = render(table, 40);
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
    fn detects_table_and_leaves_surrounding_text_alone() {
        let content = format!("before\n\n{TABLE}\n\nafter");
        let segs = split_tables(&content);
        assert_eq!(segs.len(), 3);
        assert!(matches!(&segs[0], Segment::Text(t) if t.trim() == "before"));
        match &segs[1] {
            Segment::Table(rows, aligns) => {
                assert_eq!(
                    rows,
                    &[
                        vec!["Name".to_string(), "Age".to_string()],
                        vec!["Alice".to_string(), "30".to_string()],
                        vec!["Bob".to_string(), "7".to_string()],
                    ]
                );
                assert!(matches!(aligns[0], Align::Left));
                assert!(matches!(aligns[1], Align::Right));
            }
            _ => panic!("expected a table segment"),
        }
        assert!(matches!(&segs[2], Segment::Text(t) if t.trim() == "after"));
    }

    #[test]
    fn pipes_inside_a_fenced_code_block_are_not_a_table() {
        let content = "```\n| not | a | table |\n| --- | --- |\n```";
        let segs = split_tables(content);
        assert_eq!(segs.len(), 1);
        assert!(matches!(&segs[0], Segment::Text(_)));
    }

    #[test]
    fn render_produces_a_bordered_box_with_header_separator() {
        let r = render(TABLE, 40);
        let text: Vec<String> = r.lines.iter().map(line_text).collect();
        // top border, header, header/body separator, 2 data rows, bottom border.
        assert_eq!(text.len(), 6);
        assert!(text[0].starts_with('┌') && text[0].ends_with('┐'));
        assert!(text[1].contains("Name") && text[1].contains("Age"));
        assert!(text[2].starts_with('├') && text[2].ends_with('┤'));
        assert!(text[5].starts_with('└') && text[5].ends_with('┘'));
    }

    #[test]
    fn to_plain_reconstructs_clean_markdown_table() {
        let plain = to_plain(TABLE);
        assert_eq!(
            plain,
            "| Name | Age |\n| --- | ---: |\n| Alice | 30 |\n| Bob | 7 |"
        );
    }
}
