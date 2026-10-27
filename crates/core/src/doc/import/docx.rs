//! Faithful-first Word import using the deck's bounded ZIP and XML readers.
use super::{
    super::{
        Block, Doc, Kind, ListItem,
        feedback::{self, Anchor, Range, Role, Severity},
    },
    Imported,
};
use crate::deck::import::{
    pptx::package::{Package, Rels},
    xml::El,
};
use anyhow::{Context as _, Result, ensure};
use std::{collections::BTreeMap, path::Path};
fn attr<'a>(el: &'a El, name: &str) -> Option<&'a str> {
    el.attr_local(name)
}
fn val(el: &El) -> &str {
    attr(el, "val").unwrap_or_default()
}
fn child<'a>(el: &'a El, name: &str) -> Option<&'a str> {
    el.kid(name).map(val)
}
fn number(el: &El, name: &str) -> Option<f64> {
    attr(el, name)?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}
fn all_text(el: &El) -> String {
    if matches!(el.name.as_str(), "t" | "instrText") {
        return el.text.clone();
    }
    el.kids.iter().map(all_text).collect()
}
fn literal(el: &El) -> String {
    match el.name.as_str() {
        "t" => el.text.clone(),
        "br" => "\n".into(),
        "tab" => "\t".into(),
        "footnoteReference" => format!("[^{}]", attr(el, "id").unwrap_or("1")),
        _ => el.kids.iter().map(literal).collect(),
    }
}
fn markdown(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
fn rich(el: &El, rels: &Rels) -> String {
    match el.name.as_str() {
        "t" => markdown(&el.text),
        "tab" => "\t".into(),
        "br" => {
            if attr(el, "type") == Some("page") {
                "\u{000c}".into()
            } else {
                "\n".into()
            }
        }
        "footnoteReference" => format!("[^{}]", attr(el, "id").unwrap_or("1")),
        "r" => {
            let mut text = el
                .kids
                .iter()
                .filter(|k| k.name != "rPr")
                .map(|k| rich(k, rels))
                .collect::<String>();
            if let Some(props) = el.kid("rPr") {
                let on = |key| {
                    props
                        .kid(key)
                        .is_some_and(|e| !matches!(attr(e, "val"), Some("0" | "false" | "off")))
                };
                if child(props, "rStyle").is_some_and(|s| s.to_lowercase().contains("code")) {
                    text = format!("`{}`", all_text(el));
                }
                if on("b") {
                    text = format!("**{text}**");
                }
                if on("i") {
                    text = format!("*{text}*");
                }
                if on("strike") || on("dstrike") {
                    text = format!("~~{text}~~");
                }
            }
            text
        }
        "hyperlink" => {
            let text = el.kids.iter().map(|k| rich(k, rels)).collect::<String>();
            let target = el
                .rel("id")
                .and_then(|id| rels.get(id))
                .map(|r| r.target.clone())
                .or_else(|| attr(el, "anchor").map(|s| format!("#{s}")));
            target.map_or_else(|| text.clone(), |url| format!("[{text}]({url})"))
        }
        "instrText" | "fldChar" | "drawing" | "pPr" | "rPr" => String::new(),
        _ => el.kids.iter().map(|k| rich(k, rels)).collect(),
    }
}
#[derive(Clone, Default)]
struct Style {
    name: String,
    outline: Option<u8>,
    parent: String,
    numbering: Option<(String, usize)>,
}
fn styles(root: &El) -> BTreeMap<String, Style> {
    root.kids_named("style")
        .map(|el| {
            let p = el.kid("pPr");
            (
                attr(el, "styleId").unwrap_or_default().into(),
                Style {
                    name: child(el, "name").unwrap_or_default().to_lowercase(),
                    outline: p
                        .and_then(|e| e.kid("outlineLvl"))
                        .and_then(|e| val(e).parse().ok()),
                    parent: child(el, "basedOn").unwrap_or_default().into(),
                    numbering: p.and_then(num_pr),
                },
            )
        })
        .collect()
}
fn num_pr(p: &El) -> Option<(String, usize)> {
    let num = p.kid("numPr")?;
    let id = child(num, "numId")?;
    if id == "0" {
        return None;
    }
    Some((
        id.into(),
        child(num, "ilvl").and_then(|s| s.parse().ok()).unwrap_or(0),
    ))
}
fn resolved_style<'a>(id: &str, styles: &'a BTreeMap<String, Style>) -> Vec<&'a Style> {
    let mut out = Vec::new();
    let mut current = id;
    for _ in 0..20 {
        let Some(style) = styles.get(current) else {
            break;
        };
        out.push(style);
        current = &style.parent;
    }
    out
}
fn paragraph_kind(el: &El, styles: &BTreeMap<String, Style>, text: String) -> Kind {
    let p = el.kid("pPr");
    let id = p.and_then(|p| child(p, "pStyle")).unwrap_or_default();
    let chain = resolved_style(id, styles);
    let names = std::iter::once(id.to_lowercase())
        .chain(chain.iter().map(|s| s.name.clone()))
        .collect::<Vec<_>>();
    let outline = p
        .and_then(|p| p.kid("outlineLvl"))
        .and_then(|e| val(e).parse::<u8>().ok())
        .or_else(|| chain.iter().find_map(|s| s.outline));
    if names.iter().any(|n| n == "title") {
        return Kind::Title { text };
    }
    let level = names.iter().find_map(|n| {
        n.replace(' ', "")
            .strip_prefix("heading")
            .and_then(|s| s.parse::<u8>().ok())
    });
    if let Some(level) = level.or_else(|| outline.filter(|n| *n < 9).map(|n| n + 1)) {
        return Kind::Heading {
            level: level.clamp(1, 3),
            text,
        };
    }
    if names.iter().any(|n| n.replace(' ', "") == "intensequote") {
        return Kind::Callout {
            tone: super::super::model::Tone::Note,
            text,
        };
    }
    if names.iter().any(|n| n == "quote") {
        return Kind::Quote { text };
    }
    if names.iter().any(|n| {
        matches!(
            n.as_str(),
            "code" | "html preformatted" | "htmlpreformatted"
        )
    }) {
        return Kind::Code {
            lang: String::new(),
            text: literal(el),
        };
    }
    Kind::Paragraph { text }
}
fn numbering(root: &El) -> BTreeMap<(String, usize), bool> {
    let mut abstracts = BTreeMap::new();
    for el in root.kids_named("abstractNum") {
        for level in el.kids_named("lvl") {
            let index = attr(level, "ilvl")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            abstracts.insert(
                (
                    attr(el, "abstractNumId").unwrap_or_default().to_string(),
                    index,
                ),
                child(level, "numFmt") != Some("bullet"),
            );
        }
    }
    let mut result = BTreeMap::new();
    for el in root.kids_named("num") {
        let id = attr(el, "numId").unwrap_or_default();
        let abstract_id = child(el, "abstractNumId").unwrap_or_default();
        for level in 0..4 {
            result.insert(
                (id.into(), level),
                abstracts
                    .get(&(abstract_id.into(), level))
                    .copied()
                    .unwrap_or(false),
            );
        }
    }
    result
}
fn push_item(items: &mut Vec<ListItem>, item: ListItem, level: usize) {
    if level == 0 || items.is_empty() {
        items.push(item);
    } else {
        push_item(
            &mut items.last_mut().expect("item").children,
            item,
            level - 1,
        );
    }
}
fn header_text(el: &El) -> String {
    fn walk(el: &El, out: &mut String, field: &mut bool) {
        if el.name == "instrText" {
            let name = el.text.split_whitespace().next().unwrap_or_default();
            match name {
                "PAGE" => out.push_str("{page}"),
                "NUMPAGES" => out.push_str("{pages}"),
                "TITLE" => out.push_str("{title}"),
                _ => {}
            }
            *field = true;
        } else if el.name == "fldChar" && attr(el, "fldCharType") == Some("end") {
            *field = false;
        } else if el.name == "t" && !*field {
            out.push_str(&el.text);
        }
        for kid in &el.kids {
            walk(kid, out, field);
        }
    }
    let mut out = String::new();
    let mut field = false;
    walk(el, &mut out, &mut field);
    out
}
fn comment_ranges(
    el: &El,
    index: usize,
    position: &mut usize,
    anchors: &mut BTreeMap<String, (usize, usize, usize)>,
    warnings: &mut Vec<String>,
) {
    match el.name.as_str() {
        "commentRangeStart" => {
            anchors.insert(
                attr(el, "id").unwrap_or_default().into(),
                (index, *position, *position),
            );
        }
        "commentRangeEnd" => {
            if let Some(anchor) = anchors.get_mut(attr(el, "id").unwrap_or_default()) {
                if anchor.0 == index {
                    anchor.2 = *position;
                } else {
                    anchor.2 = usize::MAX;
                    warnings
                        .push("Cross-block comments were anchored to their first block.".into());
                }
            }
        }
        "t" | "tab" | "br" | "footnoteReference" => *position += literal(el).chars().count(),
        "drawing" | "instrText" | "rPr" | "pPr" => {}
        _ => {
            for child in &el.kids {
                comment_ranges(child, index, position, anchors, warnings);
            }
        }
    }
}
fn image(
    el: &El,
    package: &mut Package,
    rels: &Rels,
    assets: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<Block>> {
    let Some(blip) = el.find("blip") else {
        if el.find("chart").is_some() {
            warnings.push("Native Word charts were replaced with a placeholder.".into());
            return Ok(Some(Block::new(Kind::Paragraph {
                text: "[Chart not imported]".into(),
            })));
        }
        return Ok(None);
    };
    let alt = el
        .find("docPr")
        .and_then(|e| attr(e, "descr"))
        .unwrap_or_default()
        .to_string();
    let rel = blip
        .rel("embed")
        .and_then(|id| rels.get(id))
        .filter(|r| !r.external);
    if let Some(rel) = rel {
        let bytes = package.media(&rel.target)?;
        match crate::deck::assets::store_image_bytes(&bytes, &rel.target, "image", assets) {
            Ok(src) => {
                return Ok(Some(Block::new(Kind::Image {
                    src,
                    alt,
                    caption: String::new(),
                    width: super::super::model::ImageWidth::default(),
                })));
            }
            Err(error) => warnings.push(format!(
                "{}: unsupported image; placeholder ({error}).",
                rel.target
            )),
        }
    } else {
        warnings.push(
            "Linked image was replaced with a placeholder; external media is never fetched.".into(),
        );
    }
    Ok(Some(Block::new(Kind::Paragraph {
        text: format!("[Image not imported: {alt}]"),
    })))
}
fn table(el: &El, rels: &Rels, warnings: &mut Vec<String>) -> Block {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut merged = false;
    let mut align = Vec::new();
    let mut previous: Vec<String> = Vec::new();
    for row in el.kids_named("tr").take(200) {
        let mut cells = Vec::new();
        for cell in row.kids_named("tc") {
            let text = cell
                .kids_named("p")
                .map(|p| rich(p, rels))
                .collect::<Vec<_>>()
                .join("\n");
            let props = cell.kid("tcPr");
            let span = props
                .and_then(|p| child(p, "gridSpan"))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 20);
            let continuation = props
                .and_then(|p| p.kid("vMerge"))
                .is_some_and(|m| attr(m, "val") != Some("restart"));
            if span > 1 || props.and_then(|p| p.kid("vMerge")).is_some() {
                merged = true;
            }
            for _ in 0..span {
                if cells.len() == 20 {
                    break;
                }
                let text = if continuation {
                    previous
                        .get(cells.len())
                        .cloned()
                        .unwrap_or_else(|| text.clone())
                } else {
                    text.clone()
                };
                if rows.is_empty() {
                    align.push(match cell.find("jc").map(val) {
                        Some("right") => crate::deck::Align::Right,
                        Some("center") => crate::deck::Align::Center,
                        _ => crate::deck::Align::Left,
                    });
                }
                cells.push(text);
            }
        }
        previous.clone_from(&cells);
        rows.push(cells);
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(columns, String::new());
    }
    align.resize(columns, crate::deck::Align::Left);
    if merged {
        warnings.push("Merged table cells were repeated into ordinary cells.".into());
    }
    if el.kids_named("tr").count() > 200 {
        warnings.push("Table truncated to 200 rows.".into());
    }
    Block::new(Kind::Table {
        header: el
            .kid("tr")
            .and_then(|r| r.kid("trPr"))
            .and_then(|p| p.kid("tblHeader"))
            .is_some(),
        align,
        rows,
    })
}
/// Read a DOCX/DOTX package, never following external relationships.
#[allow(clippy::too_many_lines)]
pub fn import(bytes: &[u8], assets: &Path) -> Result<Imported> {
    ensure!(
        bytes.len() as u64 <= super::MAX_BYTES,
        "document exceeds the 250 MB import limit"
    );
    let mut package = Package::open(bytes)?;
    ensure!(
        !package.has("word/vbaProject.bin"),
        "macro-enabled Word packages are refused"
    );
    if package.has("[Content_Types].xml") {
        let types = package.xml("[Content_Types].xml")?;
        ensure!(
            !types
                .kids
                .iter()
                .any(|t| attr(t, "ContentType").is_some_and(|s| s.contains("macroEnabled"))),
            "macro-enabled Word packages are refused"
        );
    }
    let root_rels = package.rels("");
    let main = root_rels
        .first("/officeDocument")
        .filter(|r| !r.external)
        .map_or("word/document.xml", |r| r.target.as_str())
        .to_string();
    ensure!(
        package.has(&main),
        "not a Word document: missing document.xml"
    );
    let root = package.xml(&main)?;
    let body = root.kid("body").context("Word document has no body")?;
    let rels = package.rels(&main);
    let load = |package: &mut Package, kind: &str| -> Result<El> {
        if let Some(rel) = rels.first(kind).filter(|r| !r.external) {
            package.xml(&rel.target)
        } else {
            Ok(El::default())
        }
    };
    let styles = styles(&load(&mut package, "/styles")?);
    let number_part = load(&mut package, "/numbering")?;
    let numbers = numbering(&number_part);
    let mut doc = Doc::from_design("Imported document", &crate::design::Design::default());
    doc.design = None;
    doc.design_rev = None;
    let core = if let Some(rel) = root_rels.first("/core-properties").filter(|r| !r.external) {
        package.xml(&rel.target)?
    } else if package.has("docProps/core.xml") {
        package.xml("docProps/core.xml")?
    } else {
        El::default()
    };
    if let Some(title) = core.kid("title").map(|t| &t.text).filter(|s| !s.is_empty()) {
        doc.title.clone_from(title);
    }
    let mut warnings = Vec::new();
    let mut formats = Vec::new();
    number_part.descendants("numFmt", &mut formats);
    if formats
        .iter()
        .any(|e| !matches!(val(e), "bullet" | "decimal"))
    {
        warnings.push("Non-decimal numbering was approximated as decimal lists.".into());
    }
    let mut comments = BTreeMap::new();
    let mut anchors = BTreeMap::<String, (usize, usize, usize)>::new();
    let comment_root = load(&mut package, "/comments")?;
    for comment in comment_root.kids_named("comment") {
        comments.insert(
            attr(comment, "id").unwrap_or_default().to_string(),
            all_text(comment),
        );
    }
    let notes = load(&mut package, "/footnotes")?;
    let note_rels = rels
        .first("/footnotes")
        .filter(|r| !r.external)
        .map(|r| package.rels(&r.target))
        .unwrap_or_default();
    for note in notes.kids_named("footnote") {
        let id = attr(note, "id").unwrap_or_default();
        if id.parse::<i64>().is_ok_and(|n| n > 0) {
            doc.footnotes.insert(
                id.into(),
                note.kids_named("p")
                    .map(|p| rich(p, &note_rels))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    let mut toc = false;
    let mut last_num: Option<String> = None;
    for el in &body.kids {
        if doc.blocks.len() >= 2000 {
            warnings.push("Document truncated to 2000 blocks.".into());
            break;
        }
        if el.name == "tbl" {
            doc.blocks.push(table(el, &rels, &mut warnings));
            last_num = None;
            continue;
        }
        if el.name != "p" {
            if !matches!(el.name.as_str(), "sectPr" | "bookmarkStart" | "bookmarkEnd") {
                warnings.push(format!("Unsupported {} content was omitted.", el.name));
            }
            continue;
        }
        let mut instructions = Vec::new();
        el.descendants("instrText", &mut instructions);
        if instructions
            .iter()
            .any(|e| e.text.trim_start().starts_with("TOC"))
        {
            doc.blocks.push(Block::new(Kind::Toc));
            toc = true;
            last_num = None;
        }
        if toc {
            let mut fields = Vec::new();
            el.descendants("fldChar", &mut fields);
            if fields.iter().any(|e| attr(e, "fldCharType") == Some("end")) {
                toc = false;
            }
            continue;
        }
        if el.find("rPr").is_some_and(|p| {
            p.kids
                .iter()
                .any(|k| matches!(k.name.as_str(), "sz" | "color" | "rFonts"))
        }) {
            warnings.push(
                "Explicit run fonts, sizes and colors were replaced with document text styles."
                    .into(),
            );
        }
        if el.find("pict").is_some() {
            warnings.push("Legacy drawing content was omitted.".into());
        }
        let p = el.kid("pPr");
        let style_id = p.and_then(|p| child(p, "pStyle")).unwrap_or_default();
        let caption = style_id.eq_ignore_ascii_case("caption")
            || resolved_style(style_id, &styles)
                .iter()
                .any(|s| s.name == "caption");
        let text = rich(el, &rels);
        if caption
            && let Some(Block {
                kind: Kind::Image { caption, .. },
                ..
            }) = doc.blocks.last_mut()
        {
            *caption = text;
            continue;
        }
        let paragraph = paragraph_kind(el, &styles, text.clone());
        let num = p
            .and_then(num_pr)
            .or_else(|| {
                resolved_style(style_id, &styles)
                    .iter()
                    .find_map(|s| s.numbering.clone())
            })
            .or_else(|| text.starts_with("☐ ").then(|| ("checkbox".into(), 0)))
            .or_else(|| text.starts_with("☑ ").then(|| ("checkbox".into(), 0)));
        let mut block_index = doc.blocks.len();
        let mut paragraph_offset = 0;
        if let Some((id, level)) = num {
            let ordered = numbers
                .get(&(id.clone(), level.min(3)))
                .copied()
                .unwrap_or(false);
            if level > 3 {
                warnings.push("List nesting deeper than four levels was clamped.".into());
            }
            let mut item = ListItem {
                text,
                checked: None,
                children: vec![],
            };
            for (prefix, checked) in [("☐ ", false), ("☑ ", true)] {
                if let Some(rest) = item.text.strip_prefix(prefix) {
                    item.text = rest.into();
                    item.checked = Some(checked);
                    break;
                }
            }
            if last_num.as_ref() == Some(&id)
                && let Some(Block {
                    kind: Kind::List { items, .. },
                    ..
                }) = doc.blocks.last_mut()
            {
                let previous = Block::new(Kind::List {
                    ordered,
                    items: items.clone(),
                });
                paragraph_offset = previous.plain_text().chars().count() + 1;
                push_item(items, item, level.min(3));
                block_index = doc.blocks.len() - 1;
            } else {
                doc.blocks.push(Block::new(Kind::List {
                    ordered,
                    items: vec![item],
                }));
            }
            last_num = Some(id);
        } else {
            last_num = None;
            let segments = text.split('\u{000c}').collect::<Vec<_>>();
            if p.and_then(|p| p.kid("pageBreakBefore")).is_some() {
                doc.blocks.push(Block::new(Kind::PageBreak));
                block_index += 1;
            }
            for (index, segment) in segments.iter().enumerate() {
                if index > 0 {
                    doc.blocks.push(Block::new(Kind::PageBreak));
                }
                if !segment.is_empty() {
                    let kind = if segments.len() == 1 {
                        paragraph.clone()
                    } else {
                        paragraph_kind(el, &styles, (*segment).into())
                    };
                    doc.blocks.push(Block::new(kind));
                }
            }
            if p.and_then(|p| p.kid("pBdr"))
                .and_then(|b| b.kid("bottom"))
                .is_some()
                && text.is_empty()
            {
                doc.blocks.push(Block::new(Kind::Divider));
            }
        }
        // Inline media is kept immediately after its paragraph.
        let mut drawings = Vec::new();
        el.descendants("drawing", &mut drawings);
        for drawing in drawings {
            if let Some(block) = image(drawing, &mut package, &rels, assets, &mut warnings)? {
                doc.blocks.push(block);
            }
        }
        // Track character ranges from the same plain text used by doc anchors.
        comment_ranges(
            el,
            block_index,
            &mut paragraph_offset,
            &mut anchors,
            &mut warnings,
        );
        if el.find("object").is_some() {
            warnings.push(
                "Embedded objects were omitted (fallback pictures, if present, were kept).".into(),
            );
        }
        if el.find("txbxContent").is_some() {
            warnings.push("Text boxes were flattened into paragraph text.".into());
        }
    }
    let section = body.kid("sectPr").or_else(|| body.find("sectPr"));
    if let Some(section) = section {
        if let Some(size) = section.kid("pgSz")
            && let (Some(w), Some(h)) = (number(size, "w"), number(size, "h"))
        {
            doc.page.size = super::super::model::Paper::Custom {
                w: w / 20.0,
                h: h / 20.0,
            };
        }
        if let Some(margins) = section.kid("pgMar") {
            for (index, key) in ["top", "right", "bottom", "left"].iter().enumerate() {
                if let Some(n) = number(margins, key) {
                    doc.page.margins_pt[index] = n / 20.0;
                }
            }
        }
        for (name, header) in [("headerReference", true), ("footerReference", false)] {
            if let Some(reference) = section
                .kids_named(name)
                .find(|e| attr(e, "type") == Some("default"))
                && let Some(rel) = reference
                    .rel("id")
                    .and_then(|id| rels.get(id))
                    .filter(|r| !r.external)
            {
                let text = header_text(&package.xml(&rel.target)?);
                if header {
                    doc.page.header = text;
                } else {
                    doc.page.footer = text;
                }
            }
        }
    }
    if doc.title == "Imported document"
        && let Some(title) = doc.blocks.iter().find_map(|b| {
            if let Kind::Title { text } = &b.kind {
                Some(super::super::markdown::plain(text))
            } else {
                None
            }
        })
    {
        doc.title = title;
    }
    if !super::super::validate::check_page(&doc.page).is_empty() {
        warnings.push("Invalid page geometry replaced with A4 defaults.".into());
        doc.page = super::super::Page::default();
    }
    for block in &mut doc.blocks {
        if serde_json::to_vec(block)?.len() > 32 * 1024 {
            warnings.push(
                "Oversized blocks were truncated to 32 KB; oversized lists/tables were flattened."
                    .into(),
            );
            if block.text().is_none() {
                block.kind = Kind::Paragraph {
                    text: block.plain_text(),
                };
            }
            while serde_json::to_vec(block)?.len() > 32 * 1024 {
                let text = block.text_mut().expect("text block");
                *text = text.chars().take(text.chars().count() / 2).collect();
            }
        }
    }
    warnings.sort();
    warnings.dedup();
    let doc = Doc::check(&serde_json::to_string(&doc)?).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut threads = Vec::new();
    for (id, text) in comments {
        let anchor = anchors
            .get(&id)
            .and_then(|(index, start, end)| {
                doc.blocks.get(*index).map(|b| {
                    let plain = b.plain_text();
                    let end = (*end).min(plain.chars().count());
                    let range = (*start < end).then_some(Range { start: *start, end });
                    Anchor {
                        block: Some(b.id.clone()),
                        range,
                        quote: range.map_or_else(String::new, |r| {
                            plain.chars().skip(r.start).take(r.end - r.start).collect()
                        }),
                    }
                })
            })
            .unwrap_or(Anchor {
                block: None,
                range: None,
                quote: String::new(),
            });
        threads.push(feedback::Thread {
            id: format!("word-comment-{id}"),
            anchor,
            severity: Severity::Consider,
            created: 946_684_800,
            resolved: None,
            messages: vec![feedback::Message {
                id: format!("word-message-{id}"),
                role: Role::You,
                text,
                created: 946_684_800,
                edits: vec![],
            }],
        });
    }
    Ok(Imported {
        doc,
        threads,
        warnings,
    })
}

#[cfg(test)]
mod tests;
