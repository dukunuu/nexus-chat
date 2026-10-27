//! Replace example slides while retaining the template's masters and layouts verbatim.
use super::package::{self, OFFICE};
use crate::deck::{Deck, import::xml};
use anyhow::{Context as _, Result};
use std::fmt::Write as _;

fn scale_attribute(source: &str, key: &str, factor: f64) -> String {
    let needle = format!(" {key}=\"");
    let mut rest = source;
    let mut out = String::new();
    while let Some(at) = rest.find(&needle) {
        out.push_str(&rest[..at + needle.len()]);
        rest = &rest[at + needle.len()..];
        let Some(end) = rest.find('"') else {
            break;
        };
        if let Ok(value) = rest[..end].parse::<f64>() {
            let _ = write!(out, "{:.0}", value * factor);
        } else {
            out.push_str(&rest[..end]);
        }
        out.push('"');
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[allow(clippy::too_many_lines)] // One ordered OPC package rewrite.
pub(super) fn merge(
    template: &[u8],
    generated: &[u8],
    deck: &Deck,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let mut base = package::open(template)?;
    let generated = package::open(generated)?;
    let original_types = package::text(&base, "[Content_Types].xml")?.to_string();
    base.retain(|name, _| {
        !name.starts_with("ppt/slides/") && !name.starts_with("ppt/notesSlides/")
    });
    let mut presentation = package::text(&base, "ppt/presentation.xml")?.to_string();
    let width = xml::parse(&presentation)?
        .kid("sldSz")
        .and_then(|s| s.num("cx"))
        .unwrap_or(12_192_000.0);
    let mut rels = package::relationships(
        package::text(&base, "ppt/_rels/presentation.xml.rels")?,
        |rel| !rel.attr("Type").unwrap_or("").ends_with("/slide"),
    )?;
    let original_rels = xml::parse(&rels)?;
    let mut slide_ids = String::from("<p:sldIdLst>");
    let mut written = vec!["ppt/presentation.xml".to_string()];
    let mut generated_types = package::text(&generated, "[Content_Types].xml")?.to_string();
    let layouts: Vec<_> = base
        .keys()
        .filter(|n| {
            n.starts_with("ppt/slideLayouts/") && n.ends_with(".xml") && !n.contains("/_rels/")
        })
        .cloned()
        .collect();
    let blank = layouts
        .iter()
        .find(|name| {
            package::text(&base, name)
                .ok()
                .and_then(|s| xml::parse(s).ok())
                .is_some_and(|el| el.attr("type") == Some("blank") || el.find("ph").is_none())
        })
        .cloned();
    if blank.is_none() && deck.slides.iter().any(|s| s.source_layout.is_none()) {
        warnings.push("Template has no Blank or placeholder-free layout; freeform slides use its first layout.".into());
    }
    let blank = blank
        .or_else(|| layouts.first().cloned())
        .context("template has no slide layout")?;
    for (index, slide) in deck.slides.iter().enumerate() {
        let n = index + 1;
        let mut id = format!("nexusSlide{n}");
        while original_rels
            .kids_named("Relationship")
            .any(|r| r.attr("Id") == Some(&id))
        {
            id.push('x');
        }
        let _ = write!(slide_ids, "<p:sldId id=\"{}\" r:id=\"{id}\"/>", 256 + index);
        rels = package::append(
            &rels,
            &package::relationship(&id, "slide", &format!("slides/slide{n}.xml")),
        )?;
        let name = format!("ppt/slides/slide{n}.xml");
        let mut source = package::text(&generated, &name)?.to_string();
        let layout = slide
            .source_layout
            .as_ref()
            .filter(|name| base.contains_key(*name))
            .unwrap_or(&blank);
        if slide.source_layout.is_some() {
            source = package::replace(&source, "bg", "");
            // Exporter shape names are ids for pictures, and node names for text.
            let mut shapes = package::ranges(&source, "sp");
            shapes.extend(package::ranges(&source, "pic"));
            shapes.sort_by_key(|r| r.start);
            for range in shapes.into_iter().rev() {
                let raw = &source[range.clone()];
                let shape = xml::parse(raw)?;
                let name = shape.find("cNvPr").and_then(|p| p.attr("name"));
                if let Some(ph) = slide
                    .nodes()
                    .into_iter()
                    .find(|n| Some(n.id.as_str()) == name)
                    .and_then(|n| n.source_placeholder.as_ref())
                {
                    let identity = format!(
                        "<p:nvPr><p:ph type=\"{}\" idx=\"{}\"/></p:nvPr>",
                        crate::doc::inline::escape(&ph.kind),
                        crate::doc::inline::escape(&ph.idx)
                    );
                    let replaced = package::replace(raw, "nvPr", &identity);
                    source.replace_range(range, &replaced);
                }
            }
        }
        for key in [
            "x", "y", "cx", "cy", "sz", "spc", "marL", "indent", "lIns", "rIns", "tIns", "bIns",
        ] {
            source = scale_attribute(&source, key, width / 12_192_000.0);
        }
        base.insert(name.clone(), source.into_bytes());
        written.push(name);
        let rel_name = format!("ppt/slides/_rels/slide{n}.xml.rels");
        let rel_source = package::text(&generated, &rel_name)?;
        let mut relationships = package::relationships(rel_source, |r| {
            !r.attr("Type").unwrap_or("").ends_with("/notesSlide")
        })?;
        for range in package::ranges(&relationships.clone(), "Relationship")
            .into_iter()
            .rev()
        {
            let original = &relationships[range.clone()];
            let el = xml::parse(original)?;
            if el.attr("Type") == Some(&format!("{OFFICE}/slideLayout")) {
                let rel = package::relationship(
                    el.attr("Id").unwrap_or("rId1"),
                    "slideLayout",
                    &format!("/{layout}"),
                );
                relationships.replace_range(range, &rel);
            }
        }
        for (path, bytes) in &generated {
            if path.starts_with(&format!("ppt/media/slide{n}-")) {
                let filename = path.trim_start_matches("ppt/media/");
                let mut target = format!("nexus-{filename}");
                while base.contains_key(&format!("ppt/media/{target}")) {
                    target = format!("nexus-{target}");
                }
                relationships = relationships.replace(
                    &format!("../media/{filename}"),
                    &format!("../media/{target}"),
                );
                let new_path = format!("ppt/media/{target}");
                generated_types =
                    generated_types.replace(&format!("/{path}\""), &format!("/{new_path}\""));
                base.insert(new_path.clone(), bytes.clone());
                written.push(new_path);
            }
        }
        base.insert(rel_name, relationships.into_bytes());
    }
    slide_ids.push_str("</p:sldIdLst>");
    if package::ranges(&presentation, "sldIdLst").is_empty() {
        let at = presentation
            .find("<p:sldSz")
            .context("template slide size")?;
        presentation.insert_str(at, &slide_ids);
    } else {
        presentation = package::replace(&presentation, "sldIdLst", &slide_ids);
    }
    base.insert("ppt/presentation.xml".into(), presentation.into_bytes());
    base.insert("ppt/_rels/presentation.xml.rels".into(), rels.into_bytes());
    let types = package::content_types(&original_types, &generated_types, &base, &written)?
        .replace(
            "presentationml.template.main+xml",
            "presentationml.presentation.main+xml",
        );
    base.insert("[Content_Types].xml".into(), types.into_bytes());
    crate::deck::export::zip(base.into_iter().collect())
}
