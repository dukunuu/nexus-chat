//! GFM input via the workspace's existing pulldown-cmark parser. Inline
//! Markdown is canonicalized, never interpreted as HTML. Non-GFM structural
//! blocks use a lossless `nexus-block` fenced JSON extension on Markdown out.
use super::model::{Block, Doc, ImageWidth, Kind, ListItem, Tone};
use crate::deck::Align;
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM
}

#[derive(Debug)]
enum Tree<'a> {
    Group(Tag<'a>, Vec<Tree<'a>>),
    Leaf(Event<'a>),
}
fn trees<'a>(events: &mut impl Iterator<Item = Event<'a>>) -> Vec<Tree<'a>> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(tag) => out.push(Tree::Group(tag, trees(events))),
            Event::End(_) => break,
            event => out.push(Tree::Leaf(event)),
        }
    }
    out
}
fn escaped(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '[' | ']' | '`' | '|' | '<' | '>' | '#'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
fn inline(nodes: &[Tree<'_>]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Tree::Leaf(Event::Text(text) | Event::Html(text) | Event::InlineHtml(text)) => {
                out.push_str(&escaped(text));
            }
            Tree::Leaf(Event::Code(text)) => {
                let fence = "`".repeat(text.split('`').count().max(1));
                let pad = if text.starts_with('`') || text.ends_with('`') {
                    " "
                } else {
                    ""
                };
                let _ = write!(out, "{fence}{pad}{text}{pad}{fence}");
            }
            Tree::Leaf(Event::FootnoteReference(label)) => {
                let _ = write!(out, "[^{label}]");
            }
            Tree::Leaf(Event::SoftBreak | Event::HardBreak) => out.push('\n'),
            Tree::Group(tag, children) => {
                let text = inline(children);
                match tag {
                    Tag::Strong => {
                        let _ = write!(out, "**{text}**");
                    }
                    Tag::Emphasis => {
                        let _ = write!(out, "*{text}*");
                    }
                    Tag::Strikethrough => {
                        let _ = write!(out, "~~{text}~~");
                    }
                    Tag::Link {
                        dest_url, title, ..
                    } => {
                        let _ = write!(out, "[{text}]({dest_url}");
                        if !title.is_empty() {
                            let _ = write!(out, " \"{}\"", title.replace('"', "\\\""));
                        }
                        out.push(')');
                    }
                    Tag::Image {
                        dest_url, title, ..
                    } => {
                        let _ = write!(out, "![{text}]({dest_url}");
                        if !title.is_empty() {
                            let _ = write!(out, " \"{}\"", title.replace('"', "\\\""));
                        }
                        out.push(')');
                    }
                    _ => {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&text);
                    }
                }
            }
            Tree::Leaf(_) => {}
        }
    }
    out
}
fn code_text(nodes: &[Tree<'_>]) -> String {
    let text = nodes
        .iter()
        .map(|node| match node {
            Tree::Leaf(Event::Text(t)) => t.to_string(),
            _ => String::new(),
        })
        .collect::<String>();
    text.strip_suffix('\n').unwrap_or(&text).into()
}
fn list_items(nodes: &[Tree<'_>]) -> Vec<ListItem> {
    nodes
        .iter()
        .filter_map(|node| {
            let Tree::Group(Tag::Item, children) = node else {
                return None;
            };
            let mut item = ListItem {
                text: String::new(),
                checked: None,
                children: Vec::new(),
            };
            let mut text = Vec::new();
            for child in children {
                match child {
                    Tree::Group(Tag::List(_), nested) => item.children.extend(list_items(nested)),
                    Tree::Leaf(Event::TaskListMarker(checked)) => item.checked = Some(*checked),
                    Tree::Group(Tag::Paragraph, paragraph) => {
                        for node in paragraph {
                            if let Tree::Leaf(Event::TaskListMarker(checked)) = node {
                                item.checked = Some(*checked);
                            }
                        }
                        text.push(inline(paragraph));
                    }
                    _ => text.push(inline(std::slice::from_ref(child))),
                }
            }
            item.text = text.join("");
            Some(item)
        })
        .collect()
}

/// Parsed blocks plus footnote definitions, which live on the document rather
/// than masquerading as visible blocks. `first_h1_is_title` is opt-in.
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub blocks: Vec<Block>,
    pub footnotes: BTreeMap<String, String>,
}

/// Parse blocks only; use `parse_document` to retain footnote definitions.
#[must_use]
pub fn parse(source: &str) -> Vec<Block> {
    parse_document(source, false).blocks
}

/// Parse GFM, optionally treating the first H1 as the document's title block.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_document(source: &str, first_h1_is_title: bool) -> Parsed {
    let mut parsed = Parsed::default();
    let tree = trees(&mut Parser::new_ext(source, options()));
    for node in tree {
        let kind = match node {
            Tree::Group(Tag::Heading { level, .. }, children) => {
                let level = (level as u8).min(3);
                let text = inline(&children);
                if level == 1 && parsed.blocks.is_empty() && first_h1_is_title {
                    Kind::Title { text }
                } else {
                    Kind::Heading { level, text }
                }
            }
            Tree::Group(Tag::Paragraph, children) => {
                if let [
                    Tree::Group(
                        Tag::Image {
                            dest_url, title, ..
                        },
                        alt,
                    ),
                ] = children.as_slice()
                {
                    Kind::Image {
                        src: dest_url.to_string(),
                        alt: plain(&inline(alt)),
                        caption: title.to_string(),
                        width: ImageWidth::Full,
                    }
                } else {
                    Kind::Paragraph {
                        text: inline(&children),
                    }
                }
            }
            Tree::Group(Tag::BlockQuote(tone), children) => {
                let text = inline(&children);
                match tone {
                    None => Kind::Quote { text },
                    Some(tone) => Kind::Callout {
                        tone: match tone {
                            BlockQuoteKind::Warning | BlockQuoteKind::Caution => Tone::Warn,
                            BlockQuoteKind::Tip => Tone::Success,
                            BlockQuoteKind::Important => Tone::Info,
                            BlockQuoteKind::Note => Tone::Note,
                        },
                        text,
                    },
                }
            }
            Tree::Group(Tag::CodeBlock(code), children) => {
                let lang = match code {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let text = code_text(&children);
                if lang == "nexus-block"
                    && let Ok(block) = serde_json::from_str::<Block>(&text)
                {
                    parsed.blocks.push(block);
                    continue;
                }
                Kind::Code { lang, text }
            }
            Tree::Group(Tag::List(number), children) => Kind::List {
                ordered: number.is_some(),
                items: list_items(&children),
            },
            Tree::Group(Tag::Table(alignment), children) => {
                let align = alignment
                    .into_iter()
                    .map(|a| match a {
                        Alignment::Center => Align::Center,
                        Alignment::Right => Align::Right,
                        _ => Align::Left,
                    })
                    .collect();
                let rows = children
                    .into_iter()
                    .filter_map(|row| match row {
                        Tree::Group(Tag::TableHead | Tag::TableRow, cells) => Some(
                            cells
                                .into_iter()
                                .filter_map(|cell| match cell {
                                    Tree::Group(Tag::TableCell, text) => Some(inline(&text)),
                                    _ => None,
                                })
                                .collect(),
                        ),
                        _ => None,
                    })
                    .collect();
                Kind::Table {
                    header: true,
                    align,
                    rows,
                }
            }
            Tree::Group(Tag::FootnoteDefinition(label), children) => {
                parsed
                    .footnotes
                    .insert(label.to_string(), inline(&children));
                continue;
            }
            Tree::Leaf(Event::Rule) => Kind::Divider,
            Tree::Group(_, children) => Kind::Paragraph {
                text: inline(&children),
            },
            Tree::Leaf(event) => Kind::Paragraph {
                text: inline(&[Tree::Leaf(event)]),
            },
        };
        parsed.blocks.push(Block::new(kind));
    }
    parsed
}

fn render_items(items: &[ListItem], ordered: bool, depth: usize, out: &mut String) {
    for (index, item) in items.iter().enumerate() {
        let bullet = if ordered {
            format!("{}.", index + 1)
        } else {
            "-".into()
        };
        let check = item.checked.map_or("", |c| if c { "[x] " } else { "[ ] " });
        let indent = "    ".repeat(depth);
        let mut lines = item.text.lines();
        let _ = writeln!(
            out,
            "{indent}{bullet} {check}{}",
            lines.next().unwrap_or_default()
        );
        for line in lines {
            let _ = writeln!(out, "{indent}    {line}");
        }
        render_items(&item.children, false, depth + 1, out);
    }
}
fn extension(block: &Block) -> String {
    let source = serde_json::to_string(block).expect("block JSON");
    let fence = "`".repeat(source.split('`').count().max(3));
    format!("{fence}nexus-block\n{source}\n{fence}")
}

/// GFM for one complete block. Structural blocks without a Markdown equivalent
/// and explicit style/width overrides are losslessly encoded as fenced JSON.
#[must_use]
pub fn render_block(block: &Block) -> String {
    if block.style.is_some() {
        return extension(block);
    }
    match &block.kind {
        Kind::Title { text } => format!("# {text}"),
        Kind::Heading { level, text } => format!("{} {text}", "#".repeat(usize::from(*level))),
        Kind::Paragraph { text } => text.clone(),
        Kind::Quote { text } => text
            .lines()
            .map(|s| format!("> {s}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Kind::Callout { tone, text } => {
            let label = match tone {
                Tone::Note => "NOTE",
                Tone::Warn => "WARNING",
                Tone::Success => "TIP",
                Tone::Info => "IMPORTANT",
            };
            format!(
                "> [!{label}]\n{}",
                text.lines()
                    .map(|s| format!("> {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
        Kind::Code { lang, text } => {
            let fence = "`".repeat(text.split('`').count().max(3));
            format!("{fence}{lang}\n{text}\n{fence}")
        }
        Kind::List { ordered, items } => {
            let mut out = String::new();
            render_items(items, *ordered, 0, &mut out);
            out.trim_end().into()
        }
        Kind::Table {
            header: true,
            align,
            rows,
        } if !rows.is_empty() => {
            let row = |r: &[String]| format!("| {} |", r.join(" | "));
            let mut out = row(&rows[0]);
            out.push('\n');
            let separators = (0..rows[0].len())
                .map(|i| match align.get(i).unwrap_or(&Align::Left) {
                    Align::Left => ":---".into(),
                    Align::Center => ":---:".into(),
                    Align::Right => "---:".into(),
                })
                .collect::<Vec<_>>();
            out.push_str(&row(&separators));
            for r in &rows[1..] {
                out.push('\n');
                out.push_str(&row(r));
            }
            out
        }
        Kind::Image {
            src,
            alt,
            caption,
            width: ImageWidth::Full,
        } => format!(
            "![{}]({src}{})",
            escaped(alt),
            if caption.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", caption.replace('"', "\\\""))
            }
        ),
        Kind::Divider => "---".into(),
        _ => extension(block),
    }
}

/// GitHub-flavored Markdown, including footnote definitions.
#[must_use]
pub fn render(doc: &Doc) -> String {
    let mut out = doc
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            if index > 0 && matches!(block.kind, Kind::Title { .. }) {
                extension(block)
            } else {
                render_block(block)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    for (label, text) in &doc.footnotes {
        let _ = write!(out, "\n\n[^{label}]: {}", text.replace('\n', "\n    "));
    }
    out.push('\n');
    out
}

/// Clean GitHub Markdown for sharing, deliberately dropping presentation-only
/// overrides rather than emitting the lossless internal JSON extension.
#[must_use]
pub fn export(doc: &Doc) -> String {
    let mut headings = Vec::new();
    let mut slugs = BTreeMap::<String, usize>::new();
    for block in &doc.blocks {
        if let Kind::Heading { text, .. } | Kind::Title { text } = &block.kind {
            let title = plain(text);
            let slug: String = title
                .to_lowercase()
                .chars()
                .filter_map(|c| {
                    if c.is_alphanumeric() || matches!(c, '_' | '-') {
                        Some(c)
                    } else if c.is_whitespace() {
                        Some('-')
                    } else {
                        None
                    }
                })
                .collect();
            let count = slugs.entry(slug.clone()).or_default();
            let anchor = if *count == 0 {
                slug
            } else {
                format!("{slug}-{count}")
            };
            *count += 1;
            if let Kind::Heading { level, .. } = block.kind {
                headings.push((level, title, anchor));
            }
        }
    }
    let mut blocks = Vec::new();
    for block in &doc.blocks {
        let mut block = block.clone();
        block.style = None;
        match &mut block.kind {
            Kind::PageBreak => continue,
            Kind::Toc => {
                let base = headings
                    .iter()
                    .map(|(level, _, _)| *level)
                    .min()
                    .unwrap_or(1);
                blocks.push(
                    headings
                        .iter()
                        .map(|(level, title, anchor)| {
                            format!(
                                "{}- [{}](#{anchor})",
                                "  ".repeat(usize::from(level - base)),
                                escaped(title)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                continue;
            }
            Kind::Chart { chart } => {
                let mut rows = vec![
                    std::iter::once(String::from("Label"))
                        .chain(chart.series.iter().map(|s| s.name.clone()))
                        .collect(),
                ];
                for (i, label) in chart.labels.iter().enumerate() {
                    rows.push(
                        std::iter::once(label.clone())
                            .chain(chart.series.iter().map(|s| {
                                s.values.get(i).map_or(String::new(), ToString::to_string)
                            }))
                            .collect(),
                    );
                }
                block.kind = Kind::Table {
                    header: true,
                    align: Vec::new(),
                    rows,
                };
            }
            Kind::Image { width, .. } => *width = ImageWidth::Full,
            Kind::Table { header, rows, .. } => {
                if !*header {
                    rows.insert(0, vec![String::new(); rows.first().map_or(0, Vec::len)]);
                    *header = true;
                }
                if rows.is_empty() {
                    continue;
                }
            }
            _ => {}
        }
        blocks.push(render_block(&block));
    }
    let mut out = blocks.join("\n\n");
    for (label, text) in &doc.footnotes {
        let _ = write!(out, "\n\n[^{label}]: {}", text.replace('\n', "\n    "));
    }
    out.push('\n');
    out
}

/// Plain-text character offsets are measured after stripping inline markup.
#[must_use]
pub fn plain(source: &str) -> String {
    let mut out = String::new();
    for event in Parser::new_ext(source, options()) {
        match event {
            Event::Text(text) | Event::Code(text) => out.push_str(&text),
            Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph) => out.push('\n'),
            Event::FootnoteReference(label) => {
                let _ = write!(out, "[^{label}]");
            }
            _ => {}
        }
    }
    out.trim_end_matches('\n').into()
}
