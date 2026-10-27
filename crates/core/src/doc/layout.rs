//! Point-based document pagination, measured with the deck renderer's fonts.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
use super::{Block, Doc, Kind, ListItem, inline};
use crate::deck::text::{Face, Typesetter, color, font_stack};
use crate::deck::{Align, Font, ResolvedFont};
use std::collections::BTreeMap;
use std::path::Path;

/// All document spacing, in points (also consumed by HTML print styling).
#[derive(Clone, Copy, Debug)]
pub struct LayoutSpec {
    pub block_gap: f64,
    pub heading_above: f64,
    pub heading_below: f64,
    pub list_indent: f64,
    pub quote_padding: f64,
    pub callout_padding: f64,
    pub cell_padding: f64,
    pub code_padding: f64,
    pub caption_gap: f64,
    pub list_gap: f64,
    pub bar_width: f64,
}
/// The single spacing table for flowing documents.
pub const LAYOUT: LayoutSpec = LayoutSpec {
    block_gap: 8.0,
    heading_above: 16.0,
    heading_below: 6.0,
    list_indent: 18.0,
    quote_padding: 8.0,
    callout_padding: 12.0,
    cell_padding: 5.0,
    code_padding: 10.0,
    caption_gap: 5.0,
    list_gap: 3.0,
    bar_width: 3.0,
};

/// A painted text run. Coordinates on its item describe its line box.
#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub family: String,
    pub size: f64,
    pub weight: u16,
    pub italic: bool,
    pub strike: bool,
    pub link: Option<String>,
    pub color: String,
    pub baseline: f64,
}
/// Page primitives, in paint order.
#[derive(Clone, Debug)]
pub enum Paint {
    Run(Run),
    Box { fill: String, border: String },
    Line { color: String },
    Image { src: String, alt: String },
    Svg(String),
}
/// One positioned primitive; every item retains its source block id.
#[derive(Clone, Debug)]
pub struct Item {
    pub block: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub paint: Paint,
}
/// A physical page. Its items and size are in points.
#[derive(Clone, Debug)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub items: Vec<Item>,
}
#[derive(Clone)]
struct Line {
    items: Vec<Item>,
    height: f64,
}
// Independent composition constraints, not mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
struct Flow {
    block: String,
    lines: Vec<Line>,
    above: f64,
    below: f64,
    padding: f64,
    whole: bool,
    widows: bool,
    heading: bool,
    fill: Option<String>,
    bar: Option<String>,
    repeat: Option<Vec<Line>>,
    page_break: bool,
}
impl Flow {
    fn height(&self) -> f64 {
        self.lines.iter().map(|l| l.height).sum::<f64>() + self.padding * 2.0
    }
}
struct Composer<'a, 'b> {
    doc: &'a Doc,
    ts: Typesetter<'b>,
    width: f64,
    height: f64,
    assets: Option<&'a Path>,
    heading_pages: &'a BTreeMap<String, usize>,
}
impl Composer<'_, '_> {
    fn font(&self, name: &str) -> ResolvedFont {
        self.ts.theme.resolve_font(name, &Font::default())
    }
    #[allow(clippy::too_many_lines)]
    fn text(&mut self, source: &str, style: &str, x: f64, width: f64, literal: bool) -> Vec<Line> {
        let font = self.font(style);
        let runs = if literal {
            vec![inline::Run {
                text: source.into(),
                ..Default::default()
            }]
        } else {
            inline::runs(source)
        };
        let mut lines = vec![Line {
            items: Vec::new(),
            height: font.size * font.line_height,
        }];
        let mut used = 0.0;
        for mut run in runs {
            if let Some(id) = &run.footnote {
                let number = self
                    .doc
                    .footnotes
                    .keys()
                    .position(|key| key == id)
                    .map_or_else(|| id.clone(), |i| (i + 1).to_string());
                run.text = format!("[{number}]");
            }
            let face = Face {
                family: font_stack(self.ts.theme, if run.code { "mono" } else { &font.family }),
                weight: if run.bold {
                    font.weight.max(700)
                } else {
                    font.weight
                },
                italic: font.italic || run.italic,
                spacing: font.letter_spacing * font.size,
            };
            for part in run.text.split_inclusive(['\n', ' ']) {
                let newline = part.ends_with('\n');
                let text = part.trim_end_matches('\n');
                let advance = self.ts.advance(&face, font.size, text);
                if used > 0.0 && used + self.ts.advance(&face, font.size, text.trim_end()) > width {
                    lines.push(Line {
                        items: Vec::new(),
                        height: font.size * font.line_height,
                    });
                    used = 0.0;
                }
                let fragments = if advance > width {
                    text.chars().map(|c| c.to_string()).collect::<Vec<_>>()
                } else {
                    vec![text.to_string()]
                };
                for text in fragments {
                    let w = self.ts.advance(&face, font.size, &text);
                    if used > 0.0 && used + w > width {
                        lines.push(Line {
                            items: Vec::new(),
                            height: font.size * font.line_height,
                        });
                        used = 0.0;
                    }
                    let (ascent, descent) = self.ts.ascent_descent(&face, font.size);
                    let baseline = (font.size * font.line_height - ascent - descent) / 2.0 + ascent;
                    lines.last_mut().expect("line").items.push(Item {
                        block: String::new(),
                        x: x + used,
                        y: 0.0,
                        w,
                        h: font.size * font.line_height,
                        paint: Paint::Run(Run {
                            text,
                            family: face.family.clone(),
                            size: font.size,
                            weight: face.weight,
                            italic: face.italic,
                            strike: run.strike,
                            link: run.link.clone(),
                            color: color(
                                self.ts.theme,
                                if run.link.is_some() {
                                    "accent"
                                } else {
                                    &font.color
                                },
                            ),
                            baseline,
                        }),
                    });
                    used += w;
                }
                if newline {
                    lines.push(Line {
                        items: Vec::new(),
                        height: font.size * font.line_height,
                    });
                    used = 0.0;
                }
            }
        }
        for line in &mut lines {
            let used = line.items.last().map_or(0.0, |i| i.x + i.w - x);
            let offset = match font.align {
                Align::Left => 0.0,
                Align::Center => (width - used) / 2.0,
                Align::Right => width - used,
            };
            for item in &mut line.items {
                item.x += offset;
            }
        }
        lines
    }
    fn flow(block: &Block) -> Flow {
        Flow {
            block: block.id.clone(),
            lines: Vec::new(),
            above: 0.0,
            below: LAYOUT.block_gap,
            padding: 0.0,
            whole: false,
            widows: true,
            heading: false,
            fill: None,
            bar: None,
            repeat: None,
            page_break: false,
        }
    }
    fn list(
        &mut self,
        block: &Block,
        items: &[ListItem],
        ordered: bool,
        depth: usize,
        out: &mut Vec<Flow>,
    ) {
        for (index, item) in items.iter().enumerate() {
            let mut flow = Self::flow(block);
            flow.below = LAYOUT.list_gap;
            let indent = (depth + 1) as f64 * LAYOUT.list_indent;
            flow.lines = self.text(
                &item.text,
                inline::style(block),
                indent,
                (self.width - indent).max(1.0),
                false,
            );
            let marker = item.checked.map_or_else(
                || {
                    if ordered {
                        format!("{}.", index + 1)
                    } else {
                        "•".into()
                    }
                },
                |checked| if checked { "☑".into() } else { "☐".into() },
            );
            let mut label = self.text(
                &marker,
                inline::style(block),
                indent - LAYOUT.list_indent,
                LAYOUT.list_indent,
                true,
            );
            flow.lines[0].items.append(&mut label[0].items);
            out.push(flow);
            self.list(block, &item.children, ordered, depth + 1, out);
        }
    }
    fn row(&mut self, block: &Block, cells: &[String], align: &[Align], header: bool) -> Vec<Line> {
        let cell_width = self.width / cells.len().max(1) as f64;
        let mut columns = Vec::new();
        for (index, text) in cells.iter().enumerate() {
            let mut lines = self.text(
                text,
                inline::style(block),
                index as f64 * cell_width + LAYOUT.cell_padding,
                (cell_width - 2.0 * LAYOUT.cell_padding).max(1.0),
                false,
            );
            for line in &mut lines {
                let used = line.items.iter().map(|i| i.w).sum::<f64>();
                let slack = (cell_width - 2.0 * LAYOUT.cell_padding - used).max(0.0);
                let shift = match align.get(index).copied().unwrap_or(Align::Left) {
                    Align::Left => 0.0,
                    Align::Center => slack / 2.0,
                    Align::Right => slack,
                };
                for item in &mut line.items {
                    item.x += shift;
                    if header && let Paint::Run(run) = &mut item.paint {
                        run.weight = run.weight.max(700);
                    }
                }
            }
            columns.push(lines);
        }
        let count = columns.iter().map(Vec::len).max().unwrap_or(0);
        (0..count)
            .map(|index| {
                let padding = LAYOUT.cell_padding;
                let height = columns
                    .iter()
                    .filter_map(|c| c.get(index))
                    .map(|l| l.height)
                    .fold(0.0_f64, f64::max)
                    + 2.0 * padding;
                let mut items = (0..cells.len())
                    .map(|i| Item {
                        block: block.id.clone(),
                        x: i as f64 * cell_width,
                        y: 0.0,
                        w: cell_width,
                        h: height,
                        paint: Paint::Box {
                            fill: if header {
                                color(self.ts.theme, "surface")
                            } else {
                                "none".into()
                            },
                            border: color(self.ts.theme, "border"),
                        },
                    })
                    .collect::<Vec<_>>();
                for column in &columns {
                    if let Some(line) = column.get(index) {
                        items.extend(line.items.iter().cloned().map(|mut i| {
                            i.y += padding;
                            i
                        }));
                    }
                }
                Line { items, height }
            })
            .collect()
    }
    #[allow(clippy::too_many_lines)]
    fn blocks(&mut self) -> Vec<Flow> {
        let mut out = Vec::new();
        for block in &self.doc.blocks {
            let mut flow = Self::flow(block);
            match &block.kind {
                Kind::List { items, ordered } => {
                    self.list(block, items, *ordered, 0, &mut out);
                    if let Some(last) = out.last_mut() {
                        last.below = LAYOUT.block_gap;
                    }
                    continue;
                }
                Kind::Table {
                    rows,
                    header,
                    align,
                } => {
                    let repeat = rows
                        .first()
                        .filter(|_| *header)
                        .map(|r| self.row(block, r, align, true));
                    for (index, row) in rows.iter().enumerate() {
                        let mut flow = Self::flow(block);
                        flow.lines = self.row(block, row, align, *header && index == 0);
                        flow.whole = true;
                        flow.widows = false;
                        flow.below = if index + 1 == rows.len() {
                            LAYOUT.block_gap
                        } else {
                            0.0
                        };
                        if index > 0 {
                            flow.repeat.clone_from(&repeat);
                        }
                        // Keep the first data row beside its header.
                        flow.heading = *header && index == 0;
                        out.push(flow);
                    }
                    continue;
                }
                Kind::Image {
                    src,
                    alt,
                    caption,
                    width,
                } => {
                    let ratio = crate::deck::layout::image_path(src, self.assets)
                        .and_then(|p| image::image_dimensions(p).ok())
                        .map_or(0.6, |(w, h)| f64::from(h) / f64::from(w.max(1)));
                    let fraction = match width {
                        super::model::ImageWidth::Inline => 0.5,
                        super::model::ImageWidth::Wide => 0.85,
                        super::model::ImageWidth::Full => 1.0,
                    };
                    let w = self.width * fraction;
                    let captions = if caption.is_empty() {
                        vec![]
                    } else {
                        self.text(caption, "caption", 0.0, self.width, false)
                    };
                    let caption_height = captions.iter().map(|l| l.height).sum::<f64>()
                        + if captions.is_empty() {
                            0.0
                        } else {
                            LAYOUT.caption_gap
                        };
                    let h = (w * ratio).min((self.height - caption_height).max(1.0));
                    let w = w.min(h / ratio);
                    flow.lines.push(Line {
                        height: h,
                        items: vec![Item {
                            block: block.id.clone(),
                            x: (self.width - w) / 2.0,
                            y: 0.0,
                            w,
                            h,
                            paint: Paint::Image {
                                src: src.clone(),
                                alt: alt.clone(),
                            },
                        }],
                    });
                    if !captions.is_empty() {
                        flow.lines.push(Line {
                            items: vec![],
                            height: LAYOUT.caption_gap,
                        });
                        flow.lines.extend(captions);
                    }
                    flow.whole = true;
                }
                Kind::Chart { chart } => {
                    // The chart renderer's labels are sized for a 320 px box, so a
                    // 70% column keeps them near body size.
                    let w = self.width * 0.7;
                    let h = (w * 0.5625).min(self.height);
                    let mut svg = String::new();
                    crate::deck::render::chart(
                        &mut svg,
                        chart.chart,
                        &chart.labels,
                        &chart.series,
                        self.ts.theme,
                    );
                    flow.lines = vec![Line {
                        height: h,
                        items: vec![Item {
                            block: block.id.clone(),
                            x: (self.width - w) / 2.0,
                            y: 0.0,
                            w,
                            h,
                            paint: Paint::Svg(svg),
                        }],
                    }];
                    flow.whole = true;
                }
                Kind::Divider => flow.lines.push(Line {
                    height: 1.0,
                    items: vec![Item {
                        block: block.id.clone(),
                        x: 0.0,
                        y: 0.0,
                        w: self.width,
                        h: 0.5,
                        paint: Paint::Line {
                            color: color(self.ts.theme, "border"),
                        },
                    }],
                }),
                Kind::PageBreak => flow.page_break = true,
                Kind::Toc => {
                    for heading in self
                        .doc
                        .blocks
                        .iter()
                        .filter(|b| matches!(b.kind, Kind::Heading { .. }))
                    {
                        let number = self.heading_pages.get(&heading.id).copied().unwrap_or(0);
                        let mut lines = self.text(
                            heading.text().unwrap_or_default(),
                            "body",
                            0.0,
                            self.width - 36.0,
                            false,
                        );
                        let mut label =
                            self.text(&number.to_string(), "body", self.width - 25.0, 25.0, true);
                        lines[0].items.append(&mut label[0].items);
                        flow.lines.extend(lines);
                    }
                }
                _ => {
                    let padding = match block.kind {
                        Kind::Quote { .. } => LAYOUT.quote_padding,
                        Kind::Callout { .. } => LAYOUT.callout_padding,
                        Kind::Code { .. } => LAYOUT.code_padding,
                        _ => 0.0,
                    };
                    flow.padding = padding;
                    flow.whole = matches!(block.kind, Kind::Code { .. } | Kind::Callout { .. });
                    flow.widows = !matches!(block.kind, Kind::Code { .. });
                    match &block.kind {
                        Kind::Quote { .. } => flow.bar = Some(color(self.ts.theme, "accent")),
                        Kind::Callout { tone, .. } => {
                            flow.fill = Some(color(self.ts.theme, "surface"));
                            flow.bar = Some(tone_color(self.ts.theme, *tone));
                        }
                        Kind::Code { .. } => flow.fill = Some(color(self.ts.theme, "surface")),
                        _ => {}
                    }
                    flow.heading = matches!(block.kind, Kind::Heading { .. } | Kind::Title { .. });
                    if flow.heading {
                        flow.above = LAYOUT.heading_above;
                        flow.below = if matches!(block.kind, Kind::Title { .. }) {
                            LAYOUT.heading_below * 2.0
                        } else {
                            LAYOUT.heading_below
                        };
                    }
                    let bar = if flow.bar.is_some() {
                        LAYOUT.bar_width
                    } else {
                        0.0
                    };
                    flow.lines = self.text(
                        block.text().unwrap_or_default(),
                        inline::style(block),
                        padding + bar,
                        (self.width - 2.0 * padding - bar).max(1.0),
                        matches!(block.kind, Kind::Code { .. }),
                    );
                }
            }
            out.push(flow);
        }
        for (index, (id, text)) in self.doc.footnotes.iter().enumerate() {
            let block = Block {
                id: format!("@footnote-{id}"),
                style: Some("caption".into()),
                kind: Kind::Paragraph {
                    text: format!("[{}] {text}", index + 1),
                },
            };
            let mut flow = Self::flow(&block);
            flow.lines = self.text(
                block.text().unwrap_or_default(),
                "caption",
                0.0,
                self.width,
                false,
            );
            out.push(flow);
        }
        out
    }
}
/// The accent for a callout's left bar, from color roles so templates recolor it.
pub(crate) fn tone_color(theme: &crate::deck::Theme, tone: super::model::Tone) -> String {
    use super::model::Tone;
    let role = match tone {
        Tone::Info => "accent",
        Tone::Warn => "accent2",
        Tone::Success => "accent3",
        Tone::Note => "muted",
    };
    if theme.colors.contains_key(role) {
        color(theme, role)
    } else if tone == Tone::Success {
        "#2e7d32".into()
    } else {
        color(theme, "accent")
    }
}
fn next_page(pages: &mut Vec<Page>, size: (f64, f64)) {
    pages.push(Page {
        width: size.0,
        height: size.1,
        items: vec![],
    });
}
fn place(page: &mut Page, line: &Line, block: &str, left: f64, top: f64) {
    page.items.extend(line.items.iter().cloned().map(|mut i| {
        i.block = block.into();
        i.x += left;
        i.y += top;
        i
    }));
}
#[allow(clippy::too_many_lines)]
fn pass(doc: &Doc, assets: Option<&Path>, heading_pages: &BTreeMap<String, usize>) -> Vec<Page> {
    let theme = inline::theme(doc);
    let size = doc.page.size.points();
    let [top, right, bottom, left] = doc.page.margins_pt;
    let height = size.1 - top - bottom;
    let width = size.0 - left - right;
    let mut composer = Composer {
        doc,
        ts: Typesetter::new(&theme, assets),
        width,
        height,
        assets,
        heading_pages,
    };
    let flows = composer.blocks();
    let mut pages = Vec::new();
    next_page(&mut pages, size);
    let mut y = top;
    for (index, flow) in flows.iter().enumerate() {
        if flow.page_break {
            next_page(&mut pages, size);
            y = top;
            continue;
        }
        let keep = if flow.heading {
            flows.get(index + 1).map_or(0.0, |next| {
                next.lines.iter().take(2).map(|l| l.height).sum::<f64>() + next.padding * 2.0
            })
        } else {
            0.0
        };
        let above = if y > top { flow.above } else { 0.0 };
        let total = flow.height();
        if y > top
            && ((flow.whole && total <= height && y + above + total > size.1 - bottom)
                || (flow.heading && y + above + total + flow.below + keep > size.1 - bottom))
        {
            next_page(&mut pages, size);
            y = top;
            if let Some(header) = &flow.repeat {
                for line in header {
                    place(pages.last_mut().expect("page"), line, &flow.block, left, y);
                    y += line.height;
                }
            }
        }
        if y > top {
            y += flow.above;
        }
        let mut start = 0;
        while start < flow.lines.len() {
            let available = size.1 - bottom - y - flow.padding * 2.0;
            let mut used = 0.0;
            let mut count = 0;
            for line in &flow.lines[start..] {
                if used + line.height > available {
                    break;
                }
                used += line.height;
                count += 1;
            }
            let remaining = flow.lines.len() - start;
            if flow.widows && count < remaining {
                if remaining - count == 1 {
                    count = count.saturating_sub(1);
                }
                if count < 2 {
                    count = 0;
                }
            }
            let fresh = top
                + flow
                    .repeat
                    .as_ref()
                    .map_or(0.0, |header| header.iter().map(|l| l.height).sum::<f64>());
            if count == 0 && y > fresh {
                next_page(&mut pages, size);
                y = top;
                if let Some(header) = &flow.repeat {
                    for line in header {
                        place(pages.last_mut().expect("page"), line, &flow.block, left, y);
                        y += line.height;
                    }
                }
                continue;
            }
            // A line taller than the content box still makes progress.
            count = count.max(1);
            let painted_height = flow.lines[start..start + count]
                .iter()
                .map(|l| l.height)
                .sum::<f64>()
                + flow.padding * 2.0;
            let page = pages.last_mut().expect("page");
            if let Some(fill) = &flow.fill {
                page.items.push(Item {
                    block: flow.block.clone(),
                    x: left,
                    y,
                    w: width,
                    h: painted_height,
                    paint: Paint::Box {
                        fill: fill.clone(),
                        border: "none".into(),
                    },
                });
            }
            if let Some(bar) = &flow.bar {
                page.items.push(Item {
                    block: flow.block.clone(),
                    x: left,
                    y,
                    w: LAYOUT.bar_width,
                    h: painted_height,
                    paint: Paint::Box {
                        fill: bar.clone(),
                        border: "none".into(),
                    },
                });
            }
            y += flow.padding;
            for line in &flow.lines[start..start + count] {
                place(page, line, &flow.block, left, y);
                y += line.height;
            }
            y += flow.padding;
            start += count;
            if start < flow.lines.len() {
                next_page(&mut pages, size);
                y = top;
                if let Some(header) = &flow.repeat {
                    for line in header {
                        place(pages.last_mut().expect("page"), line, &flow.block, left, y);
                        y += line.height;
                    }
                }
            }
        }
        y += flow.below;
    }
    let count = pages.len();
    for (index, page) in pages.iter_mut().enumerate() {
        for (field, at, id) in [
            (&doc.page.header, top / 2.0, "@header"),
            (&doc.page.footer, size.1 - bottom / 2.0, "@footer"),
        ] {
            if field.is_empty() {
                continue;
            }
            let text = field
                .replace("{pages}", &count.to_string())
                .replace("{page}", &(index + 1).to_string())
                .replace("{title}", &doc.title);
            for line in composer.text(&text, "caption", 0.0, width, true) {
                place(page, &line, id, left, at);
            }
        }
    }
    pages
}
/// Layout twice: reserve TOC geometry, then fill heading page numbers.
#[must_use]
pub fn paginate(doc: &Doc, assets: Option<&Path>) -> Vec<Page> {
    let first = pass(doc, assets, &BTreeMap::new());
    pass(doc, assets, &heading_pages(&first))
}
/// First page containing each block, one-based (also used by cached DOCX TOCs).
#[must_use]
pub fn heading_pages(pages: &[Page]) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    for (index, page) in pages.iter().enumerate() {
        for item in &page.items {
            result.entry(item.block.clone()).or_insert(index + 1);
        }
    }
    result
}

#[cfg(test)]
mod tests;
