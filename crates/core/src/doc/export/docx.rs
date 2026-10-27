//! Deterministic, native `WordprocessingML`; no Office installation required.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
use super::super::{
    Block, Doc, Kind, ListItem,
    feedback::Thread,
    inline::{self, escape},
    layout,
};
use anyhow::{Context as _, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::Path,
};
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const PIC: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
fn part(root: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:{root} xmlns:w=\"{W}\" xmlns:r=\"{R}\" xmlns:a=\"{A}\" xmlns:wp=\"{WP}\" xmlns:pic=\"{PIC}\">{body}</w:{root}>"
    )
}
fn run(text: &str, properties: &str) -> String {
    format!(
        "<w:r><w:rPr>{properties}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>",
        escape(text)
    )
}
fn field(instruction: &str, cached: &str) -> String {
    format!(
        "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> {} </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>{}<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>",
        escape(instruction),
        run(cached, "")
    )
}
fn fields(text: &str, title: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&run(&rest[..start], ""));
        rest = &rest[start..];
        let Some(end) = rest.find('}') else { break };
        match &rest[..=end] {
            "{page}" => out.push_str(&field("PAGE", "1")),
            "{pages}" => out.push_str(&field("NUMPAGES", "1")),
            "{title}" => out.push_str(&run(title, "")),
            other => out.push_str(&run(other, "")),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(&run(rest, ""));
    out
}
#[derive(Default)]
struct Package {
    parts: BTreeMap<String, Vec<u8>>,
    types: BTreeMap<String, String>,
    rels: Vec<(String, String, String, bool)>,
    links: BTreeMap<String, String>,
    footnotes: BTreeMap<String, usize>,
    comments: Vec<(usize, String, usize, usize)>,
    starts: BTreeSet<usize>,
    ends: BTreeSet<usize>,
    offset: usize,
    list: usize,
    numbers: Vec<(usize, bool)>,
    drawing: usize,
}
impl Package {
    fn xml(&mut self, path: &str, kind: &str, xml: String) {
        self.parts.insert(path.into(), xml.into_bytes());
        self.types.insert(path.into(), kind.into());
    }
    fn rel(&mut self, kind: &str, target: &str, external: bool) -> String {
        let id = format!("rId{}", self.rels.len() + 1);
        self.rels
            .push((id.clone(), kind.into(), target.into(), external));
        id
    }
    fn markers(&mut self, block: &str, position: usize) -> String {
        let mut out = String::new();
        for (id, b, start, end) in &self.comments {
            if b != block {
                continue;
            }
            if position == *start && self.starts.insert(*id) {
                let _ = write!(out, "<w:commentRangeStart w:id=\"{id}\"/>");
            }
            if position == *end && self.ends.insert(*id) {
                let _ = write!(
                    out,
                    "<w:commentRangeEnd w:id=\"{id}\"/><w:r><w:commentReference w:id=\"{id}\"/></w:r>"
                );
            }
        }
        out
    }
    fn rich(&mut self, text: &str, literal: bool, block: &str) -> String {
        let runs = if literal {
            vec![inline::Run {
                text: text.into(),
                ..Default::default()
            }]
        } else {
            inline::runs(text)
        };
        let mut out = self.markers(block, self.offset);
        for span in runs {
            let mut properties = String::new();
            if span.bold {
                properties.push_str("<w:b/>");
            }
            if span.italic {
                properties.push_str("<w:i/>");
            }
            if span.strike {
                properties.push_str("<w:strike/>");
            }
            if span.code {
                properties.push_str("<w:rStyle w:val=\"CodeChar\"/>");
            }
            let link = span.link.as_ref().map(|url| {
                if let Some(id) = self.links.get(url) {
                    return id.clone();
                }
                let id = self.rel("hyperlink", url, true);
                self.links.insert(url.clone(), id.clone());
                id
            });
            if let Some(id) = &link {
                let _ = write!(out, "<w:hyperlink r:id=\"{id}\">");
                if !span.code {
                    properties.push_str("<w:rStyle w:val=\"Hyperlink\"/>");
                }
            }
            if let Some(id) = span
                .footnote
                .as_ref()
                .and_then(|key| self.footnotes.get(key))
            {
                let _ = write!(out, "<w:r><w:footnoteReference w:id=\"{id}\"/></w:r>");
                self.offset += span.text.chars().count();
            } else {
                // Split only at comment boundaries, retaining run formatting.
                let mut buffered = String::new();
                for ch in span.text.chars() {
                    if ch == '\n' {
                        out.push_str(&run(&buffered, &properties));
                        buffered.clear();
                        out.push_str("<w:r><w:br/></w:r>");
                    } else {
                        buffered.push(ch);
                    }
                    self.offset += 1;
                    let markers = self.markers(block, self.offset);
                    if !markers.is_empty() {
                        out.push_str(&run(&buffered, &properties));
                        buffered.clear();
                        out.push_str(&markers);
                    }
                }
                if !buffered.is_empty() {
                    out.push_str(&run(&buffered, &properties));
                }
            }
            if link.is_some() {
                out.push_str("</w:hyperlink>");
            }
        }
        out.push_str(&self.markers(block, self.offset));
        out
    }
    fn p(&mut self, block: &str, style: &str, text: &str, properties: &str) -> String {
        let content = self.rich(text, style == "Code", block);
        format!("<w:p><w:pPr><w:pStyle w:val=\"{style}\"/>{properties}</w:pPr>{content}</w:p>")
    }
    fn list(&mut self, block: &str, items: &[ListItem], depth: usize, number: usize) -> String {
        let mut out = String::new();
        for item in items {
            let text = item.checked.map_or_else(
                || item.text.clone(),
                |c| format!("{} {}", if c { "☑" } else { "☐" }, item.text),
            );
            let properties = if item.checked.is_none() {
                format!(
                    "<w:numPr><w:ilvl w:val=\"{depth}\"/><w:numId w:val=\"{number}\"/></w:numPr>"
                )
            } else {
                String::new()
            };
            out.push_str(&self.p(block, "ListParagraph", &text, &properties));
            self.offset += 1;
            out.push_str(&self.list(block, &item.children, depth + 1, number));
        }
        out
    }
    fn picture(&mut self, bytes: Vec<u8>, width: f64, height: f64, alt: &str) -> String {
        self.drawing += 1;
        let extension = if image::guess_format(&bytes).ok() == Some(image::ImageFormat::Jpeg) {
            "jpeg"
        } else {
            "png"
        };
        let target = format!("media/image{}.{extension}", self.drawing);
        self.parts.insert(format!("word/{target}"), bytes);
        let rel = self.rel("image", &target, false);
        let (cx, cy) = (
            (width * 12700.0).round() as u64,
            (height * 12700.0).round() as u64,
        );
        format!(
            "<w:r><w:drawing><wp:inline><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{}\" name=\"Picture {}\" descr=\"{}\"/><a:graphic><a:graphicData uri=\"{PIC}\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"Picture\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{rel}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>",
            self.drawing,
            self.drawing,
            escape(alt)
        )
    }
}
fn style_id(block: &Block, doc: &Doc) -> String {
    if let Some(style) = &block.style {
        return match style.as_str() {
            "title" => "Title".into(),
            "h1" => "Heading1".into(),
            "h2" => "Heading2".into(),
            "h3" => "Heading3".into(),
            "body" => "Normal".into(),
            "quote" => "Quote".into(),
            "caption" => "Caption".into(),
            "code" => "Code".into(),
            _ => format!(
                "DocStyle{}",
                doc.theme
                    .text_styles
                    .keys()
                    .position(|key| key == style)
                    .expect("checked style")
            ),
        };
    }
    match block.kind {
        Kind::Title { .. } => "Title",
        Kind::Heading { level: 1, .. } => "Heading1",
        Kind::Heading { level: 2, .. } => "Heading2",
        Kind::Heading { .. } => "Heading3",
        Kind::Quote { .. } => "Quote",
        Kind::Callout { .. } => "IntenseQuote",
        Kind::Code { .. } => "Code",
        _ => "Normal",
    }
    .into()
}
#[allow(clippy::too_many_lines)]
fn styles(doc: &Doc) -> String {
    let theme = inline::theme(doc);
    let mut out = String::new();
    let defaults = [
        ("Normal", "Normal", "body", None),
        ("Title", "Title", "title", None),
        ("Heading1", "heading 1", "h1", Some(0)),
        ("Heading2", "heading 2", "h2", Some(1)),
        ("Heading3", "heading 3", "h3", Some(2)),
        ("Quote", "Quote", "quote", None),
        ("Caption", "Caption", "caption", None),
        ("Code", "Code", "code", None),
        ("ListParagraph", "List Paragraph", "body", None),
        ("IntenseQuote", "Intense Quote", "body", None),
        ("Hyperlink", "Hyperlink", "body", None),
        ("CodeChar", "Code Char", "code", None),
    ];
    let mut styles = defaults
        .into_iter()
        .map(|(id, name, key, outline)| (id.to_string(), name.to_string(), key, outline))
        .collect::<Vec<_>>();
    for (index, key) in doc.theme.text_styles.keys().enumerate() {
        if !matches!(
            key.as_str(),
            "title" | "h1" | "h2" | "h3" | "body" | "quote" | "caption" | "code"
        ) {
            styles.push((format!("DocStyle{index}"), key.clone(), key.as_str(), None));
        }
    }
    for (id, name, key, outline) in styles {
        let id = id.as_str();
        let name = escape(&name);
        let font = theme.resolve_font(key, &crate::deck::Font::default());
        let family = theme.fonts.get(&font.family).unwrap_or(&font.family);
        let fg = crate::deck::text::color(
            &theme,
            if id == "Hyperlink" {
                "accent"
            } else {
                &font.color
            },
        );
        let properties = if matches!(id, "Hyperlink" | "CodeChar") {
            String::new()
        } else {
            let heading = outline.is_some() || id == "Title";
            let before = if heading {
                layout::LAYOUT.heading_above
            } else {
                0.0
            };
            let after = if heading {
                layout::LAYOUT.heading_below
            } else {
                layout::LAYOUT.block_gap
            };
            let hex = |role| {
                crate::deck::text::color(&theme, role)
                    .trim_start_matches('#')
                    .to_string()
            };
            let frame = match id {
                "Quote" => format!(
                    "<w:pBdr><w:left w:val=\"single\" w:sz=\"24\" w:space=\"8\" w:color=\"{}\"/></w:pBdr>",
                    hex("accent")
                ),
                "IntenseQuote" => format!(
                    "<w:pBdr><w:left w:val=\"single\" w:sz=\"24\" w:space=\"8\" w:color=\"{}\"/></w:pBdr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
                    hex("accent"),
                    hex("surface")
                ),
                _ => String::new(),
            };
            format!(
                "<w:pPr>{}{frame}<w:spacing w:before=\"{}\" w:after=\"{}\"/>{}</w:pPr>",
                if heading { "<w:keepNext/>" } else { "" },
                (before * 20.0) as u64,
                (after * 20.0) as u64,
                outline.map_or_else(String::new, |level| format!(
                    "<w:outlineLvl w:val=\"{level}\"/>"
                ))
            )
        };
        let _ = write!(
            out,
            "<w:style w:type=\"{}\" w:styleId=\"{id}\"{}><w:name w:val=\"{name}\"/>{properties}<w:rPr><w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\"/><w:color w:val=\"{}\"/><w:sz w:val=\"{}\"/>{}{}</w:rPr></w:style>",
            if matches!(id, "Hyperlink" | "CodeChar") {
                "character"
            } else {
                "paragraph"
            },
            if id == "Normal" {
                " w:default=\"1\""
            } else {
                ""
            },
            escape(family),
            escape(family),
            fg.trim_start_matches('#'),
            (font.size * 2.0).round() as u64,
            if font.weight >= 600 { "<w:b/>" } else { "" },
            if font.italic { "<w:i/>" } else { "" }
        );
    }
    part("styles", &out)
}
fn theme(doc: &Doc) -> String {
    let mut colors = String::new();
    for (id, role) in [
        ("dk1", "fg"),
        ("lt1", "bg"),
        ("dk2", "muted"),
        ("lt2", "surface"),
        ("accent1", "accent"),
        ("accent2", "accent2"),
        ("accent3", "accent3"),
        ("accent4", "accent4"),
        ("accent5", "accent5"),
        ("accent6", "accent6"),
        ("hlink", "accent"),
        ("folHlink", "muted"),
    ] {
        let value = doc
            .theme
            .colors
            .get(role)
            .or_else(|| doc.theme.colors.get("accent"))
            .map_or("000000", |s| s.trim_start_matches('#'));
        let _ = write!(colors, "<a:{id}><a:srgbClr val=\"{value}\"/></a:{id}>");
    }
    let fonts = |kind: &str, role: &str| {
        format!(
            "<a:{kind}Font><a:latin typeface=\"{}\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:{kind}Font>",
            escape(doc.theme.fonts.get(role).map_or("Arial", String::as_str))
        )
    };
    let fills = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>".repeat(3);
    let lines = "<a:ln w=\"12700\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:prstDash val=\"solid\"/></a:ln>".repeat(3);
    let effects = "<a:effectStyle><a:effectLst/></a:effectStyle>".repeat(3);
    format!(
        "<?xml version=\"1.0\"?><a:theme xmlns:a=\"{A}\" name=\"Document\"><a:themeElements><a:clrScheme name=\"Document\">{colors}</a:clrScheme><a:fontScheme name=\"Document\">{}{}</a:fontScheme><a:fmtScheme name=\"Document\"><a:fillStyleLst>{fills}</a:fillStyleLst><a:lnStyleLst>{lines}</a:lnStyleLst><a:effectStyleLst>{effects}</a:effectStyleLst><a:bgFillStyleLst>{fills}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>",
        fonts("major", "heading"),
        fonts("minor", "body")
    )
}
fn numbering(numbers: &[(usize, bool)]) -> String {
    let mut out = String::new();
    for (id, ordered) in [(1, true), (2, false)] {
        let _ = write!(
            out,
            "<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"multilevel\"/>"
        );
        for level in 0..4 {
            let _ = write!(
                out,
                "<w:lvl w:ilvl=\"{level}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{}\"/><w:lvlText w:val=\"{}\"/><w:pPr><w:ind w:left=\"{}\" w:hanging=\"240\"/></w:pPr></w:lvl>",
                if ordered { "decimal" } else { "bullet" },
                if ordered {
                    format!("%{}.", level + 1)
                } else {
                    "•".into()
                },
                (f64::from(level + 1) * layout::LAYOUT.list_indent * 20.0) as u64
            );
        }
        out.push_str("</w:abstractNum>");
    }
    for (id, ordered) in numbers {
        let _ = write!(
            out,
            "<w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"{}\"/></w:num>",
            if *ordered { 1 } else { 2 }
        );
    }
    part("numbering", &out)
}
fn rels(items: &[(String, String, String, bool)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (id, kind, target, external) in items {
        let _ = write!(
            out,
            "<Relationship Id=\"{id}\" Type=\"{R}/{kind}\" Target=\"{}\"{}/>",
            escape(target),
            if *external {
                " TargetMode=\"External\""
            } else {
                ""
            }
        );
    }
    out.push_str("</Relationships>");
    out
}
/// Export native paragraphs, tables, lists, fields, pictures and open comments.
#[allow(clippy::too_many_lines)]
pub fn export(
    doc: &Doc,
    assets: Option<&Path>,
    threads: &[Thread],
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let mut package = Package::default();
    let content = |kind: &str| {
        format!("application/vnd.openxmlformats-officedocument.wordprocessingml.{kind}+xml")
    };
    for (index, id) in doc.footnotes.keys().enumerate() {
        package.footnotes.insert(id.clone(), index + 1);
    }
    let open = threads
        .iter()
        .filter(|t| t.resolved.is_none())
        .collect::<Vec<_>>();
    let mut comments = String::new();
    for (id, thread) in open.iter().enumerate() {
        if let Some(block) = thread.anchor.block.as_ref().and_then(|id| doc.block(id)) {
            let range = thread
                .anchor
                .range
                .unwrap_or(super::super::feedback::Range {
                    start: 0,
                    end: block.plain_text().chars().count(),
                });
            package
                .comments
                .push((id, block.id.clone(), range.start, range.end));
        }
        let text = thread
            .messages
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.text))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = write!(
            comments,
            "<w:comment w:id=\"{id}\" w:author=\"nexus\" w:date=\"2000-01-01T00:00:00Z\"><w:p>{}</w:p></w:comment>",
            run(&text, "")
        );
    }
    let (w, h) = doc.page.size.points();
    let [top, right, bottom, left] = doc.page.margins_pt;
    let width = w - left - right;
    let pages = layout::heading_pages(&layout::paginate(doc, assets));
    let mut body = String::new();
    for block in &doc.blocks {
        package.offset = 0;
        match &block.kind {
            Kind::List { items, ordered } => {
                package.list += 1;
                let num = package.list;
                package.numbers.push((num, *ordered));
                body.push_str(&package.list(&block.id, items, 0, num));
            }
            Kind::Table {
                rows,
                header,
                align,
            } => {
                let border = doc
                    .theme
                    .colors
                    .get("border")
                    .map_or("D2D2D7", |s| s.trim_start_matches('#'));
                let _ = write!(body, "<w:tbl><w:tblPr><w:tblBorders>");
                for edge in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    let _ = write!(
                        body,
                        "<w:{edge} w:val=\"single\" w:sz=\"4\" w:color=\"{border}\"/>"
                    );
                }
                body.push_str("</w:tblBorders></w:tblPr><w:tblGrid>");
                let cell = (width * 20.0 / rows.first().map_or(1, |r| r.len().max(1)) as f64)
                    .round() as u64;
                for _ in 0..rows.first().map_or(0, Vec::len) {
                    let _ = write!(body, "<w:gridCol w:w=\"{cell}\"/>");
                }
                body.push_str("</w:tblGrid>");
                for (index, row) in rows.iter().enumerate() {
                    body.push_str(if index == 0 && *header {
                        "<w:tr><w:trPr><w:tblHeader/></w:trPr>"
                    } else {
                        "<w:tr>"
                    });
                    for (col, text) in row.iter().enumerate() {
                        let alignment = match align.get(col) {
                            Some(crate::deck::Align::Right) => "right",
                            Some(crate::deck::Align::Center) => "center",
                            _ => "left",
                        };
                        let _ = write!(
                            body,
                            "<w:tc><w:tcPr><w:tcW w:w=\"{cell}\" w:type=\"dxa\"/></w:tcPr>{}</w:tc>",
                            package.p(
                                &block.id,
                                "Normal",
                                text,
                                &format!("<w:jc w:val=\"{alignment}\"/>")
                            )
                        );
                        package.offset += if col + 1 < row.len() { 3 } else { 1 };
                    }
                    body.push_str("</w:tr>");
                }
                body.push_str("</w:tbl>");
            }
            Kind::Image {
                src,
                alt,
                caption,
                width: image_width,
            } => {
                if let Some(path) = crate::deck::layout::image_path(src, assets) {
                    let image = image::load_from_memory(&std::fs::read(&path)?)?;
                    let mut bytes = std::fs::read(path)?;
                    if image::guess_format(&bytes).ok() == Some(image::ImageFormat::WebP) {
                        let mut png = std::io::Cursor::new(Vec::new());
                        image.write_to(&mut png, image::ImageFormat::Png)?;
                        bytes = png.into_inner();
                    }
                    let fraction = match image_width {
                        super::super::model::ImageWidth::Inline => 0.5,
                        super::super::model::ImageWidth::Wide => 0.85,
                        super::super::model::ImageWidth::Full => 1.0,
                    };
                    let pw = width * fraction;
                    let ph = pw * f64::from(image.height()) / f64::from(image.width().max(1));
                    let picture = package.picture(bytes, pw, ph, alt);
                    let _ = write!(
                        body,
                        "<w:p>{}{}{}</w:p>",
                        package.markers(&block.id, 0),
                        picture,
                        package.markers(&block.id, block.plain_text().chars().count())
                    );
                } else {
                    warnings.push(format!(
                        "{}: image unavailable; exported alt text.",
                        block.id
                    ));
                    body.push_str(&package.p(&block.id, "Normal", alt, ""));
                }
                if !caption.is_empty() {
                    body.push_str(&package.p(&block.id, "Caption", caption, ""));
                }
            }
            Kind::Chart { chart } => {
                let theme = inline::theme(doc);
                let mut fragment = String::new();
                crate::deck::render::chart(
                    &mut fragment,
                    chart.chart,
                    &chart.labels,
                    &chart.series,
                    &theme,
                );
                let pixels = (width * 2.0).round() as u32;
                let pixel_height = (f64::from(pixels) * 0.5625).round() as u32;
                let svg = format!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{pixels}\" height=\"{pixel_height}\" viewBox=\"0 0 320 180\">{fragment}</svg>"
                );
                let options = resvg::usvg::Options {
                    fontdb: crate::deck::text::deck_fonts(&theme, assets),
                    ..Default::default()
                };
                let tree = resvg::usvg::Tree::from_str(&svg, &options)?;
                let mut pixmap =
                    resvg::tiny_skia::Pixmap::new(pixels, pixel_height).context("chart size")?;
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                let picture = package.picture(pixmap.encode_png()?, width, width * 0.5625, "Chart");
                let _ = write!(body, "<w:p>{picture}</w:p>");
            }
            Kind::Divider => body.push_str(&package.p(
                &block.id,
                "Normal",
                "",
                "<w:pBdr><w:bottom w:val=\"single\" w:sz=\"4\"/></w:pBdr>",
            )),
            Kind::PageBreak => body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>"),
            Kind::Toc => {
                body.push_str("<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> TOC \\o &quot;1-3&quot; \\h </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r></w:p>");
                for heading in doc
                    .blocks
                    .iter()
                    .filter(|b| matches!(b.kind, Kind::Heading { .. }))
                {
                    body.push_str(&package.p(
                        "",
                        "Normal",
                        &format!(
                            "{}\t{}",
                            heading.plain_text(),
                            pages.get(&heading.id).copied().unwrap_or(1)
                        ),
                        "",
                    ));
                }
                body.push_str("<w:p><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>");
            }
            Kind::Code { text, .. } => {
                let style = style_id(block, doc);
                let text = package.rich(text, true, &block.id);
                let _ = write!(
                    body,
                    "<w:p><w:pPr><w:pStyle w:val=\"{}\"/></w:pPr>{text}</w:p>",
                    escape(&style)
                );
            }
            _ => body.push_str(&package.p(
                &block.id,
                &style_id(block, doc),
                block.text().unwrap_or_default(),
                "",
            )),
        }
    }
    for (name, root, text) in [
        ("header1", "hdr", &doc.page.header),
        ("footer1", "ftr", &doc.page.footer),
    ] {
        package.xml(
            &format!("word/{name}.xml"),
            &content(if root == "hdr" { "header" } else { "footer" }),
            part(root, &format!("<w:p>{}</w:p>", fields(text, &doc.title))),
        );
    }
    let header = package.rel("header", "header1.xml", false);
    let footer = package.rel("footer", "footer1.xml", false);
    let _ = write!(
        body,
        "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"{header}\"/><w:footerReference w:type=\"default\" r:id=\"{footer}\"/><w:pgSz w:w=\"{}\" w:h=\"{}\"/><w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\"/></w:sectPr>",
        (w * 20.0).round() as u64,
        (h * 20.0).round() as u64,
        (top * 20.0).round() as u64,
        (right * 20.0).round() as u64,
        (bottom * 20.0).round() as u64,
        (left * 20.0).round() as u64
    );
    package.xml(
        "word/document.xml",
        &content("document.main"),
        part("document", &format!("<w:body>{body}</w:body>")),
    );
    package.xml("word/styles.xml", &content("styles"), styles(doc));
    package.rel("styles", "styles.xml", false);
    package.xml(
        "word/numbering.xml",
        &content("numbering"),
        numbering(&package.numbers),
    );
    package.rel("numbering", "numbering.xml", false);
    package.xml(
        "word/theme/theme1.xml",
        "application/vnd.openxmlformats-officedocument.theme+xml",
        theme(doc),
    );
    package.rel("theme", "theme/theme1.xml", false);
    if !doc.footnotes.is_empty() {
        let mut notes = String::from(
            "<w:footnote w:type=\"separator\" w:id=\"-1\"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type=\"continuationSeparator\" w:id=\"0\"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>",
        );
        for (id, text) in &doc.footnotes {
            let number = package.footnotes[id];
            let text = package.rich(text, false, "");
            let _ = write!(
                notes,
                "<w:footnote w:id=\"{number}\"><w:p>{text}</w:p></w:footnote>"
            );
        }
        package.xml(
            "word/footnotes.xml",
            &content("footnotes"),
            part("footnotes", &notes),
        );
        package.rel("footnotes", "footnotes.xml", false);
    }
    if !comments.is_empty() {
        package.xml(
            "word/comments.xml",
            &content("comments"),
            part("comments", &comments),
        );
        package.rel("comments", "comments.xml", false);
    }
    package.parts.insert(
        "word/_rels/document.xml.rels".into(),
        rels(&package.rels).into_bytes(),
    );
    if !doc.footnotes.is_empty() {
        let links = package
            .rels
            .iter()
            .filter(|(_, kind, _, _)| kind == "hyperlink")
            .cloned()
            .collect::<Vec<_>>();
        if !links.is_empty() {
            package.parts.insert(
                "word/_rels/footnotes.xml.rels".into(),
                rels(&links).into_bytes(),
            );
        }
    }
    package.xml("docProps/core.xml","application/vnd.openxmlformats-package.core-properties+xml",format!("<?xml version=\"1.0\"?><cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title><dc:creator>nexus</dc:creator></cp:coreProperties>",escape(&doc.title)));
    package.xml("docProps/app.xml","application/vnd.openxmlformats-officedocument.extended-properties+xml","<?xml version=\"1.0\"?><Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>nexus</Application></Properties>".into());
    package.parts.insert("_rels/.rels".into(),format!("<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"{R}/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>").into_bytes());
    let mut types = String::from(
        "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>",
    );
    for (path, kind) in package.types {
        let _ = write!(
            types,
            "<Override PartName=\"/{path}\" ContentType=\"{kind}\"/>"
        );
    }
    types.push_str("</Types>");
    package
        .parts
        .insert("[Content_Types].xml".into(), types.into_bytes());
    crate::deck::export::zip(package.parts.into_iter().collect())
}

#[cfg(test)]
mod tests;
