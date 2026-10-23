//! Small PDF 1.4 writer: a 2× raster page plus invisible, searchable text.
//! Helvetica is never painted; `ToUnicode` maps the invisible character codes
//! back to the original Unicode, including text absent from Helvetica itself.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;

use super::super::{Deck, Kind, layout_at, render};

fn hex_unicode(text: &str) -> String {
    text.encode_utf16().fold(String::new(), |mut out, unit| {
        let _ = write!(out, "{unit:04X}");
        out
    })
}

fn stream(dict: &str, bytes: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", bytes.len()).into_bytes();
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\nendstream");
    out
}

fn text_for(node: &super::super::Node, deck: &Deck) -> Option<(String, f64)> {
    let (text, style, font) = match &node.kind {
        Kind::Text { text, style, font } => (super::plain(text), style, font),
        Kind::Code {
            source,
            style,
            font,
            ..
        } => (source.clone(), style, font),
        Kind::Table { rows, style, font } => (
            rows.iter()
                .map(|r| r.join("  "))
                .collect::<Vec<_>>()
                .join("\n"),
            style,
            font,
        ),
        _ => return None,
    };
    let font = deck.theme.resolve_font(style, font);
    Some((super::transform(&text, font.transform), font.size))
}

/// One aspect-correct page per slide, with no network or speaker notes.
#[allow(clippy::too_many_lines)]
pub fn export_pdf(deck: &Deck, assets: Option<&Path>) -> Result<Vec<u8>> {
    let texts = deck
        .slides
        .iter()
        .flat_map(super::visible_nodes)
        .filter_map(|node| text_for(node, deck))
        .collect::<Vec<_>>();
    let chars = texts
        .iter()
        .flat_map(|(s, _)| s.chars())
        .filter(|c| *c != '\n')
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let encoding = chars
        .iter()
        .enumerate()
        .map(|(i, c)| (*c, (i / 255, i % 255 + 1)))
        .collect::<BTreeMap<_, _>>();
    // Objects 1/2/3 are catalog, pages and document info. Fonts follow.
    let mut objects = vec![Vec::new(); 3];
    let mut font_ids = Vec::new();
    for chunk in chars.chunks(255) {
        let font_id = objects.len() + 1;
        let cmap_id = font_id + 1;
        font_ids.push(font_id);
        objects.push(format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode {cmap_id} 0 R >>").into_bytes());
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /NexusUnicode def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange\n",
        );
        // CMap sections may contain at most 100 mappings each.
        for (batch, units) in chunk.chunks(100).enumerate() {
            let _ = writeln!(cmap, "{} beginbfchar", units.len());
            for (i, c) in units.iter().enumerate() {
                let _ = writeln!(
                    cmap,
                    "<{:02X}> <{}>",
                    batch * 100 + i + 1,
                    hex_unicode(&c.to_string())
                );
            }
            cmap.push_str("endbfchar\n");
        }
        cmap.push_str("endcmap CMapName currentdict /CMap defineresource pop end end");
        objects.push(stream("", cmap.as_bytes()));
    }
    let size = deck.theme.size;
    let scale = 960.0 / size.w;
    let (w, h) = (960.0, size.h * scale);
    let mut pages = Vec::new();
    for slide in &deck.slides {
        let png = render::export_slide_at(deck, &slide.id, (size.w * 2.0).round() as u32, assets)?;
        let image = image::load_from_memory(&png)?.to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&image)?;
        let page = objects.len() + 1;
        let image_id = page + 1;
        let content_id = page + 2;
        pages.push(page);
        let fonts = font_ids
            .iter()
            .enumerate()
            .fold(String::new(), |mut out, (i, id)| {
                let _ = write!(out, "/F{i} {id} 0 R ");
                out
            });
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.4} {h:.4}] /Resources << /XObject << /Im {image_id} 0 R >> /Font << {fonts} >> >> /Contents {content_id} 0 R >>").into_bytes());
        objects.push(stream(&format!("/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", image.width(), image.height()), &jpeg));
        let mut content = format!("q {w:.4} 0 0 {h:.4} 0 0 cm /Im Do Q\n");
        let rects = layout_at(deck, slide, assets)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for node in super::visible_nodes(slide) {
            let Some((text, px)) = text_for(node, deck) else {
                continue;
            };
            let Some(rect) = rects.get(&node.id) else {
                continue;
            };
            let pt = px * scale;
            for (line, text) in text.split('\n').enumerate() {
                let _ = write!(
                    content,
                    "BT 3 Tr 1 0 0 1 {:.4} {:.4} Tm ",
                    rect.x * scale,
                    h - rect.y * scale - pt * (1.0 + line as f64 * 1.35)
                );
                let mut previous_font = None;
                for ch in text.chars() {
                    let &(font, code) = &encoding[&ch];
                    if previous_font != Some(font) {
                        if previous_font.is_some() {
                            content.push_str("> Tj ");
                        }
                        let _ = write!(content, "/F{font} {pt:.4} Tf <");
                        previous_font = Some(font);
                    }
                    let _ = write!(content, "{code:02X}");
                }
                if previous_font.is_some() {
                    content.push_str("> Tj ");
                }
                content.push_str("ET\n");
            }
        }
        objects.push(stream("", content.as_bytes()));
    }
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    objects[1] = format!(
        "<< /Type /Pages /Count {} /Kids [{}] >>",
        pages.len(),
        pages.iter().fold(String::new(), |mut out, id| {
            let _ = write!(out, "{id} 0 R ");
            out
        })
    )
    .into_bytes();
    objects[2] = format!(
        "<< /Title <FEFF{}> /Creator (nexus studio) >>",
        hex_unicode(&deck.title)
    )
    .into_bytes();
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Ok(out)
}
