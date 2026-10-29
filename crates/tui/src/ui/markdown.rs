//! Styled markdown rendering for the history pane (`nexus_core::markdown`
//! holds the plain-text copy path and the GFM table splitter).
//! `tui_markdown` styles inline emphasis/code and highlights fenced code, but
//! leaves block markers as literal text (`# `, `- `, ```` ``` ````); we strip
//! those so the display — and anything copied from it — is clean text.

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tui_markdown::{DefaultStyleSheet, Options, StyleSheet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use nexus_core::markdown::{TableAlign, TableSegment};

/// Glues a site badge to the URL after it, so wrapping keeps them together.
pub(crate) const BADGE_GAP: char = '\u{a0}';

/// The badge letter for an `https://` URL: its host's first letter or digit,
/// ignoring a leading `www.`.
pub(crate) fn badge_letter(url: &str) -> Option<char> {
    let host = url
        .strip_prefix("https://")?
        .split(['/', '?', '#', ':', ')', ']', '>', '"', '\''])
        .next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    host.chars()
        .next()
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_uppercase())
}

/// Put a site badge before every `https://` URL that starts a word: the
/// host's letter in reversed link color, then [`BADGE_GAP`]. The history view
/// swaps the badge for the site's icon once one is cached.
fn badge_links(line: &Line, colors: MdColors) -> Line<'static> {
    let badge = Style::default()
        .fg(colors.heading)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD);
    let mut spans = Vec::new();
    let mut previous: Option<char> = None;
    for span in &line.spans {
        let text = span.content.as_ref();
        let mut rest = 0;
        for (at, _) in text.match_indices("https://") {
            let before = text[..at].chars().next_back().or(previous);
            if before.is_some_and(|ch| !(ch.is_whitespace() || "([<\"'".contains(ch))) {
                continue;
            }
            let Some(letter) = badge_letter(&text[at..]) else {
                continue;
            };
            if at > rest {
                spans.push(Span::styled(text[rest..at].to_string(), span.style));
            }
            spans.push(Span::styled(format!("{letter}{BADGE_GAP}"), badge));
            rest = at;
        }
        if rest < text.len() {
            spans.push(Span::styled(text[rest..].to_string(), span.style));
        }
        previous = text.chars().next_back().or(previous);
    }
    let mut out = Line::from(spans);
    out.style = line.style;
    out.alignment = line.alignment;
    out
}

pub fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Terminal column width of one char (0 for combining marks, 2 for CJK/emoji).
fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Inline styles in theme colors: code is tinted text rather than a heavy
/// background block (the library default is white-on-black, invisible on a
/// dark terminal), links are underlined accent. Everything else defers to
/// the library's own defaults.
#[derive(Clone, Copy, Debug)]
struct NexusStyleSheet {
    code: Color,
    link: Color,
}

impl StyleSheet for NexusStyleSheet {
    fn heading(&self, level: u8) -> Style {
        DefaultStyleSheet.heading(level)
    }
    fn code(&self) -> Style {
        Style::new().fg(self.code)
    }
    fn link(&self) -> Style {
        Style::new()
            .fg(self.link)
            .add_modifier(Modifier::UNDERLINED)
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

fn md_options(colors: MdColors) -> Options<NexusStyleSheet> {
    Options::new(NexusStyleSheet {
        code: colors.code,
        link: colors.heading,
    })
}

/// Rendered markdown: styled/wrapped `lines`, plus, per line, which fenced code
/// block it belongs to (`code[i]`), and the raw text of each block (`blocks`).
#[derive(Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub code: Vec<Option<usize>>,
    pub blocks: Vec<String>,
    /// Fixed-height Mermaid regions, painted by the history view after text.
    pub diagrams: Vec<crate::diagrams::Placement>,
    /// Model-authored UI regions, rendered and hit-tested separately from text.
    pub widgets: Vec<crate::interactive::Placement>,
    ui_count: usize,
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
/// `s` without emoji-presentation selectors (U+FE0F). `⚠️` is `⚠` + VS16:
/// width tables call it one column, but terminals draw the emoji form two
/// wide, shifting every table row and wrapped line it sits in. Without the
/// selector the base glyph renders in its one-column text form everywhere.
pub fn terminal_safe(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\u{FE0F}') {
        s.replace('\u{FE0F}', "").into()
    } else {
        s.into()
    }
}

/// Theme colors the renderer paints with.
#[derive(Clone, Copy)]
pub struct MdColors {
    /// `#` headings.
    pub heading: Color,
    /// Code-box and table rules.
    pub rule: Color,
    /// Inline `code`.
    pub code: Color,
}

impl Default for MdColors {
    fn default() -> Self {
        Self {
            heading: Color::Cyan,
            rule: Color::DarkGray,
            code: Color::Yellow,
        }
    }
}

pub fn render(content: &str, width: usize, colors: MdColors) -> Rendered {
    let mut r = Rendered::default();
    let mut cursor = 0;
    let mut diagram: Option<(std::ops::Range<usize>, String, bool)> = None;
    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next().is_some_and(|lang| {
                    lang.eq_ignore_ascii_case("mermaid") || lang.eq_ignore_ascii_case("nexus-ui")
                }) =>
            {
                let ui = info
                    .split_whitespace()
                    .next()
                    .is_some_and(|lang| lang.eq_ignore_ascii_case("nexus-ui"));
                diagram = Some((range, String::new(), ui));
            }
            Event::Text(text) => {
                if let Some((_, source, _)) = &mut diagram {
                    source.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((range, source, ui)) = diagram.take() {
                    render_normal(&mut r, &content[cursor..range.start], width, colors);
                    let raw = &content[range.clone()];
                    let fence = raw.chars().next().unwrap_or('`');
                    let length = raw.chars().take_while(|&ch| ch == fence).count();
                    let closed = raw.lines().last().is_some_and(|line| {
                        let line = line.trim_start_matches([' ', '\t', '>']);
                        let count = line.chars().take_while(|&ch| ch == fence).count();
                        count >= length && line[count..].trim().is_empty()
                    });
                    let source = source.strip_suffix('\n').unwrap_or(&source);
                    if ui {
                        render_ui(&mut r, source, closed, width, colors);
                    } else {
                        render_diagram(&mut r, source, closed, width, colors);
                    }
                    cursor = range.end;
                }
            }
            _ => {}
        }
    }
    render_normal(&mut r, &content[cursor..], width, colors);
    r
}

fn render_normal(r: &mut Rendered, content: &str, width: usize, colors: MdColors) {
    let content = terminal_safe(content);
    for segment in nexus_core::markdown::split_tables(&content) {
        match segment {
            TableSegment::Table(rows, aligns) => render_table(r, &rows, &aligns, width, colors),
            TableSegment::Text(text) => render_text(r, &text, width, colors),
        }
    }
}

#[cfg(test)]
mod interactive_tests;

/// Model-written text shown verbatim, with every control character made
/// visible (`\u{1b}`) so nothing can drive the terminal; tabs become spaces.
fn visible_controls(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\t' => "    ".to_string(),
            ch if ch.is_control() => ch.escape_unicode().to_string(),
            ch => ch.to_string(),
        })
        .collect()
}

fn render_ui(r: &mut Rendered, source: &str, complete: bool, width: usize, colors: MdColors) {
    let ordinal = r.ui_count;
    r.ui_count += 1;
    let document = nexus_core::interactive::Document::check(source);
    let patch = nexus_core::interactive::edit_patch(source).is_some();
    let fallback = width < 8 || (complete && document.is_err() && !patch);
    if fallback {
        // A finished block that doesn't parse: show up to three errors, then show
        // the source as written — no card placeholder rows, and only control
        // characters escaped (escaping quotes made valid JSON look broken).
        let label = if width < 8 {
            "interactive UI"
        } else {
            "interactive UI · couldn't be shown"
        };
        push_code_border(r, width, Some(label), colors.rule);
        if let Err(errors) = &document {
            for error in errors.iter().take(3) {
                let reason = format!("{}: {}", error.path, error.message);
                push_code_content(
                    r,
                    &Line::from(Span::styled(
                        visible_controls(&reason),
                        Style::default().fg(colors.code),
                    )),
                    width,
                    colors.rule,
                );
            }
        }
        for line in source.lines() {
            push_code_content(r, &Line::from(visible_controls(line)), width, colors.rule);
        }
        push_code_border(r, width, None, colors.rule);
        r.blocks.push(source.into());
        return;
    }
    let card_width = u16::try_from(width).unwrap_or(u16::MAX);
    let height = crate::interactive::source_height(source, card_width);
    {
        r.widgets.push(crate::interactive::Placement {
            row: r.lines.len(),
            width: card_width,
            owner: 0,
            ordinal,
            height,
            source: source.into(),
            complete,
        });
        for row in 0..height {
            let text = if row == 0 {
                if complete {
                    "interactive output…"
                } else {
                    "generating UI…"
                }
            } else {
                ""
            };
            let text: String = text.chars().take(width).collect();
            r.push(Line::from(text), Some(r.blocks.len()));
        }
    }
    r.blocks.push(source.into());
}

fn render_diagram(r: &mut Rendered, source: &str, complete: bool, width: usize, colors: MdColors) {
    push_code_border(r, width, Some("mermaid"), colors.rule);
    if width >= 8 {
        let interior = width.saturating_sub(4);
        r.diagrams.push(crate::diagrams::Placement {
            row: r.lines.len(),
            width: u16::try_from(interior).unwrap_or(u16::MAX),
            source: source.to_string(),
            complete,
        });
        let label = if complete {
            "rendering diagram…"
        } else {
            "streaming diagram…"
        };
        let label: String = label.chars().take(interior).collect();
        for row in 0..crate::diagrams::PREVIEW_ROWS {
            let line = Line::from(if row == 0 {
                label.clone()
            } else {
                String::new()
            });
            push_code_content(r, &line, width, colors.rule);
        }
    }
    // Keep the raw source visible and copyable even after successful rendering.
    for line in source.split('\n') {
        let visible: String = terminal_safe(line)
            .chars()
            .map(|ch| {
                if ch == '\t' {
                    "    ".to_string()
                } else if ch.is_control() {
                    ch.escape_default().to_string()
                } else {
                    ch.to_string()
                }
            })
            .collect();
        push_code_content(r, &Line::from(visible), width, colors.rule);
    }
    push_code_border(r, width, None, colors.rule);
    r.blocks.push(source.to_string());
}

fn render_text(r: &mut Rendered, content: &str, width: usize, colors: MdColors) {
    let text = tui_markdown::from_str_with_options(content, &md_options(colors));
    let mut in_code = false;
    let mut raw: Vec<String> = Vec::new();

    for line in &text.lines {
        let plain = line_text(line);
        let unstyled = line.spans.iter().all(|s| s.style == Style::default());

        // Fence line toggles a code block; the fence itself isn't shown.
        if unstyled && plain.trim_start().starts_with("```") {
            if in_code {
                in_code = false;
                push_code_border(r, width, None, colors.rule);
                r.blocks.push(raw.join("\n"));
            } else {
                in_code = true;
                raw.clear();
                // ```rust → the box's top rule is labelled `rust`.
                let lang = plain.trim_start().trim_start_matches('`').trim();
                push_code_border(r, width, Some(lang), colors.rule);
            }
            continue;
        }

        if in_code {
            raw.push(plain);
            push_code_content(r, line, width, colors.rule);
            continue;
        }

        let id = None;
        match classify(line, &plain, colors.heading) {
            Block::Drop => {}
            Block::Header(body) | Block::List(body) => {
                for l in wrap_styled_line(&badge_links(&body, colors), width) {
                    r.push(l, id);
                }
            }
            Block::Plain => {
                for l in wrap_styled_line(&badge_links(line, colors), width) {
                    r.push(l, id);
                }
            }
        }
    }

    // Unterminated block (e.g. mid-stream): close it so metadata stays valid.
    if in_code {
        push_code_border(r, width, None, colors.rule);
        r.blocks.push(raw.join("\n"));
    }
}

/// Top (`lang` given, maybe empty) or bottom rule of a code box, tagged with
/// the current block id. A non-empty `lang` labels the top rule: `┌─ rust ─┐`.
fn push_code_border(r: &mut Rendered, width: usize, lang: Option<&str>, rule: Color) {
    let id = Some(r.blocks.len());
    if width < 2 {
        r.push(Line::from(""), id);
        return;
    }
    let style = Style::default().fg(rule);
    let line = match lang {
        Some(lang) if !lang.is_empty() && width > crate::ui::style::width(lang) + 6 => {
            let fill = width - crate::ui::style::width(lang) - 5;
            Line::from(vec![
                Span::styled("┌─ ", style),
                Span::styled(lang.to_string(), style.add_modifier(Modifier::ITALIC)),
                Span::styled(format!(" {}┐", "─".repeat(fill)), style),
            ])
        }
        Some(_) => Line::from(Span::styled(format!("┌{}┐", "─".repeat(width - 2)), style)),
        None => Line::from(Span::styled(format!("└{}┘", "─".repeat(width - 2)), style)),
    };
    r.push(line, id);
}

/// A code content line: wrapped to the box interior, framed with `│ … │`.
fn push_code_content(r: &mut Rendered, line: &Line, width: usize, rule: Color) {
    let border_style = || Style::default().fg(rule);
    let id = Some(r.blocks.len());
    let interior = width.saturating_sub(4).max(1);
    for row in wrap_code_line(line, interior) {
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
fn render_table(
    r: &mut Rendered,
    rows: &[Vec<String>],
    aligns: &[TableAlign],
    width: usize,
    colors: MdColors,
) {
    let border_style = || Style::default().fg(colors.rule);
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

    // When any body row wraps onto several lines, rule between every body
    // row so wrapped cells can't run into the next row visually.
    let wraps = rows.iter().skip(1).any(|row| {
        row.iter().zip(&colw).any(|(cell, &w)| {
            styled_cell(cell, colors)
                .iter()
                .map(Span::width)
                .sum::<usize>()
                > w
        })
    });
    r.push(border('┌', '┬', '┐'), None);
    for (ri, row) in rows.iter().enumerate() {
        push_table_row(r, row, aligns, &colw, ri == 0, colors);
        if ri == 0 || (wraps && ri + 1 < rows.len()) {
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
    colors: MdColors,
) {
    let border_style = || Style::default().fg(colors.rule);
    let wrapped: Vec<Vec<Line<'static>>> = (0..colw.len())
        .map(|i| {
            let mut spans = styled_cell(row.get(i).map_or("", String::as_str), colors);
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
fn styled_cell(text: &str, colors: MdColors) -> Vec<Span<'static>> {
    let rendered = tui_markdown::from_str_with_options(text, &md_options(colors));
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
/// Hard-wrap a code line at `width` display columns, keeping every
/// character — unlike prose wrapping, leading indentation and runs of
/// spaces are content here.
fn wrap_code_line(line: &Line, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut used = 0;
    for (c, st) in line
        .spans
        .iter()
        .flat_map(|sp| sp.content.chars().map(move |c| (c, sp.style)))
    {
        let cw = char_width(c);
        if used + cw > width && used > 0 {
            rows.push(Vec::new());
            used = 0;
        }
        rows.last_mut().expect("never empty").push((c, st));
        used += cw;
    }
    rows.into_iter().map(row_to_line).collect()
}

pub(crate) fn wrap_styled_line(line: &Line, width: usize) -> Vec<Line<'static>> {
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
mod diagram_tests {
    use super::*;

    #[test]
    fn diagrams_keep_prose_order_and_raw_source_copy_metadata() {
        let source = "flowchart LR\n    A[⚠\u{FE0F}] -->|next| B";
        let r = render(
            &format!("Before\n\n```mermaid\n{source}\n```\n\nAfter"),
            60,
            MdColors::default(),
        );
        assert_eq!(r.diagrams.len(), 1);
        assert!(r.diagrams[0].complete);
        assert_eq!(r.diagrams[0].source, source);
        assert_eq!(r.blocks, [source]);
        assert_eq!(r.code.len(), r.lines.len());
        let placement = &r.diagrams[0];
        assert!(
            r.code[placement.row..placement.row + usize::from(crate::diagrams::PREVIEW_ROWS)]
                .iter()
                .all(|id| *id == Some(0))
        );
        assert!(line_text(&r.lines[0]).contains("Before"));
        assert!(line_text(r.lines.last().expect("after")).contains("After"));
    }

    #[test]
    fn incomplete_diagrams_reserve_the_same_geometry_without_rendering() {
        let partial = "```mermaid\nflowchart LR\nA-->B";
        let pending = render(partial, 60, MdColors::default());
        let complete = render(&format!("{partial}\n```"), 60, MdColors::default());
        assert!(!pending.diagrams[0].complete);
        assert!(complete.diagrams[0].complete);
        assert_eq!(pending.lines.len(), complete.lines.len());
        assert_eq!(pending.blocks, complete.blocks);
    }

    #[test]
    fn diagram_source_cannot_emit_terminal_control_sequences() {
        let source = "flowchart LR\nA[\u{1b}[31m] --> B\n\tB --> C";
        let r = render(
            &format!("```mermaid\n{source}\n```"),
            60,
            MdColors::default(),
        );
        let displayed = r.lines.iter().map(line_text).collect::<String>();
        assert!(!displayed.contains('\u{1b}'));
        assert!(!displayed.contains('\t'));
        assert_eq!(r.blocks, [source]);
    }

    #[test]
    fn tilde_fences_and_pipe_labels_do_not_become_tables() {
        for source in [
            "~~~MERMAID\nflowchart LR\nA -->|label| B\n~~~",
            "  ```mermaid\n  flowchart LR\n  A -->|label| B\n  ```",
            "> ```mermaid\n> flowchart LR\n> A -->|label| B\n> ```",
            "1. Diagram:\n\n   ```mermaid\n   flowchart LR\n   A -->|label| B\n   ```",
        ] {
            let r = render(source, 60, MdColors::default());
            assert_eq!(r.diagrams.len(), 1);
            assert!(r.diagrams[0].complete, "{source}");
            assert_eq!(r.blocks, ["flowchart LR\nA -->|label| B"]);
        }
    }

    #[test]
    fn literal_nested_fences_and_inline_mentions_do_not_render() {
        for content in [
            "````markdown\n```mermaid\nflowchart LR\nA-->B\n```\n````",
            "Use `mermaid` or ` ```mermaid ` here.",
            "```rust\n// mermaid\n```",
        ] {
            assert!(render(content, 60, MdColors::default()).diagrams.is_empty());
        }
    }

    #[test]
    fn narrow_views_keep_source_without_an_image_slot() {
        for width in 0..8 {
            let r = render(
                "```mermaid\nflowchart LR\nA-->B\n```",
                width,
                MdColors::default(),
            );
            assert!(r.diagrams.is_empty());
            assert_eq!(r.blocks, ["flowchart LR\nA-->B"]);
            assert_eq!(r.code.len(), r.lines.len());
        }
    }
}

#[cfg(test)]
mod inline_code_tests {
    use super::*;

    #[test]
    fn inline_code_is_visible_on_a_black_background_terminal() {
        let r = render("run `cargo test` now", 80, MdColors::default());
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
        assert_eq!(span.style.fg, Some(MdColors::default().code));
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
        let r = render(table, 40, MdColors::default());
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
        let r = render(TABLE, 40, MdColors::default());
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
            MdColors::default(),
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

    #[test]
    fn code_boxes_label_their_language() {
        let r = render("```rust\nlet x = 1;\n```", 40, MdColors::default());
        let top = line_text(&r.lines[0]);
        assert!(top.starts_with("┌─ rust ─") && top.ends_with('┐'), "{top}");
        assert_eq!(top.chars().count(), 40);
        let bare = render("```\nx\n```", 20, MdColors::default());
        assert_eq!(line_text(&bare.lines[0]), format!("┌{}┐", "─".repeat(18)));
    }

    #[test]
    fn code_keeps_its_indentation() {
        let r = render("```python\nf(\n    x=1,\n)\n```", 40, MdColors::default());
        let text: Vec<String> = r.lines.iter().map(line_text).collect();
        assert!(
            text.iter().any(|l| l.starts_with("│     x=1,")),
            "{text:#?}"
        );
    }

    #[test]
    fn emoji_selectors_cannot_skew_table_rows() {
        let md = "| a | b |\n|---|---|\n| ⚠\u{FE0F} warn | ✅ ok |\n| x | y |";
        let r = render(md, 60, MdColors::default());
        let text: Vec<String> = r.lines.iter().map(line_text).collect();
        let w = text[0].width();
        assert!(text.iter().all(|l| l.width() == w), "{text:#?}");
        assert!(!text.concat().contains('\u{FE0F}'));
    }

    #[test]
    fn wrapped_table_rows_are_ruled_apart() {
        let long = "word ".repeat(20);
        let md = format!("| k | v |\n|---|---|\n| a | {long} |\n| b | short |");
        let r = render(&md, 40, MdColors::default());
        let rules = r
            .lines
            .iter()
            .map(line_text)
            .filter(|l| l.starts_with('├'))
            .count();
        assert_eq!(rules, 2, "header rule plus one between the two body rows");
    }
}
