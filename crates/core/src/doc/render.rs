//! Positioned document pages to SVG and PNG; no network access.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
use super::{
    Doc, inline,
    layout::{self, Page, Paint},
};
use anyhow::{Context as _, Result, ensure};
use resvg::{tiny_skia, usvg};
use std::fmt::Write as _;
use std::path::Path;

/// Paint one page with the deck's colors, fonts, chart and image helpers.
#[must_use]
pub fn svg(doc: &Doc, page: &Page, assets: Option<&Path>) -> String {
    let theme = inline::theme(doc);
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\"><rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        page.width,
        page.height,
        page.width,
        page.height,
        crate::deck::text::color(&theme, "bg")
    );
    for item in &page.items {
        let (x, y, w, h) = (item.x, item.y, item.w, item.h);
        let _ = write!(out, "<g data-block=\"{}\">", inline::escape(&item.block));
        match &item.paint {
            Paint::Run(run) => {
                if let Some(link) = &run.link {
                    let _ = write!(out, "<a href=\"{}\">", inline::escape(link));
                }
                let _ = write!(
                    out,
                    "<text x=\"{x}\" y=\"{}\" font-family=\"{}\" font-size=\"{}\" font-weight=\"{}\" font-style=\"{}\" fill=\"{}\" text-decoration=\"{}\" xml:space=\"preserve\">{}</text>",
                    y + run.baseline,
                    inline::escape(&run.family),
                    run.size,
                    run.weight,
                    if run.italic { "italic" } else { "normal" },
                    run.color,
                    if run.strike {
                        "line-through"
                    } else if run.link.is_some() {
                        "underline"
                    } else {
                        "none"
                    },
                    inline::escape(&run.text)
                );
                if run.link.is_some() {
                    out.push_str("</a>");
                }
            }
            Paint::Box { fill, border } => {
                let _ = write!(
                    out,
                    "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{fill}\" stroke=\"{border}\" stroke-width=\"0.5\"/>"
                );
            }
            Paint::Line { color } => {
                let _ = write!(
                    out,
                    "<path d=\"M{x} {y}h{w}\" stroke=\"{color}\" stroke-width=\"{h}\"/>"
                );
            }
            Paint::Image { src, alt } => {
                if let Some(data) = crate::deck::render::image_data(src, assets) {
                    let _ = write!(
                        out,
                        "<image x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" href=\"data:image/png;base64,{data}\"/>"
                    );
                } else {
                    let _ = write!(
                        out,
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"#eee\"/><text x=\"{x}\" y=\"{}\" font-size=\"11\">{}</text>",
                        y + 14.0,
                        inline::escape(alt)
                    );
                }
            }
            Paint::Svg(body) => {
                let _ = write!(
                    out,
                    "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 320 180\">{body}</svg>"
                );
            }
        }
        out.push_str("</g>");
    }
    out.push_str("</svg>");
    out
}
/// Rasterize a page at the requested width, using the measurement font database.
pub fn png(doc: &Doc, page: &Page, assets: Option<&Path>, width: u32) -> Result<Vec<u8>> {
    raster(doc, &svg(doc, page, assets), assets, width)
}
fn raster(doc: &Doc, svg: &str, assets: Option<&Path>, width: u32) -> Result<Vec<u8>> {
    let options = usvg::Options {
        fontdb: crate::deck::text::deck_fonts(&inline::theme(doc), assets),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options)?;
    let scale = width as f32 / tree.size().width();
    let height = (tree.size().height() * scale).ceil() as u32;
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).context("invalid document render size")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}
/// Return page image references, or a contact sheet when no block filter is given.
pub fn view(doc: &Doc, path: &Path, files: &Path, blocks: &[String]) -> Result<String> {
    for id in blocks {
        ensure!(
            doc.block(id).is_some(),
            "unknown block {id:?}; read the outline"
        );
    }
    let assets = path.parent().context("doc folder")?.join("assets");
    let pages = layout::paginate(doc, Some(&assets));
    let selected = pages
        .iter()
        .enumerate()
        .filter(|(_, p)| blocks.is_empty() || p.items.iter().any(|i| blocks.contains(&i.block)))
        .collect::<Vec<_>>();
    let directory = crate::deck::render::render_dir(path, files)?;
    let revision = doc.revision();
    let mut out = String::new();
    let mut outputs = Vec::new();
    if blocks.is_empty() && pages.len() > 1 {
        let cols = pages.len().min(3);
        let w = pages[0].width + 20.0;
        let h = pages[0].height + 36.0;
        let mut sheet = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\"><rect width=\"100%\" height=\"100%\" fill=\"#ddd\"/>",
            w * cols as f64,
            h * pages.len().div_ceil(cols) as f64
        );
        for (index, page) in &selected {
            let _ = write!(
                sheet,
                "<g transform=\"translate({} {})\">{}<text x=\"10\" y=\"{}\" font-size=\"12\">Page {}</text></g>",
                (index % cols) as f64 * w + 10.0,
                (index / cols) as f64 * h + 10.0,
                svg(doc, page, Some(&assets)),
                page.height + 16.0,
                index + 1
            );
        }
        sheet.push_str("</svg>");
        outputs.push((
            "sheet".to_string(),
            "pages".to_string(),
            raster(doc, &sheet, Some(&assets), 1200)?,
        ));
    } else {
        for (index, page) in selected {
            outputs.push((
                format!("page-{}", index + 1),
                format!("page {}", index + 1),
                png(doc, page, Some(&assets), 1200)?,
            ));
        }
    }
    for (key, label, bytes) in outputs {
        let name = format!("{key}-{}.png", &revision[..8]);
        let output = directory.join(&name);
        ensure!(
            !output.is_symlink(),
            "render cache must not contain symlinks"
        );
        if !output.is_file() {
            crate::deck::write_atomic(&output, &bytes)?;
        }
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let old = entry.file_name().to_string_lossy().into_owned();
            if old.starts_with(&format!("{key}-"))
                && Path::new(&old)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
                && old != name
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        let _ = writeln!(
            out,
            "![{label}]({})",
            output
                .strip_prefix(files)?
                .to_string_lossy()
                .replace('\\', "/")
        );
    }
    Ok(out)
}
