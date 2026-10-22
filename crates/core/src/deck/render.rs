//! Deck → SVG → PNG, so the model (and the terminal) can see slides.
//!
//! The studio webview draws slides with HTML and CSS; this module draws the
//! same slide with SVG and rasterizes it with `resvg`, placing every node
//! where [`super::layout`] puts it and measuring text with the very font
//! database `resvg` renders with. Both renderers follow the numbers in
//! [`LAYOUT`]; a studio test checks that the CSS and JS still use them.
//!
//! Deliberate differences from the HTML renderer:
//! - font fallbacks: `system-ui` and `ui-serif` are CSS-only, so the SVG
//!   stacks name the platform's equivalents instead;
//! - table borders are 1 CSS pixel in HTML (they stay thin as the slide
//!   scales) and 1 slide pixel here (they scale);
//! - the inline-code tint is the light-mode tint, because the HTML tint
//!   follows the viewer's light or dark mode, not the deck's theme;
//! - text is placed with real font ascent and descent, so a baseline can sit
//!   a pixel off a browser's rounding of the same metrics;
//! - `woff2` deck fonts cannot be loaded here (the font database reads
//!   `ttf`, `otf` and `ttc`), so text set in one falls back to its next family.
//!
//! Remote images are only drawn when `remote_image::Fetcher` already holds
//! them; nothing here touches the network.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use base64::Engine as _;
use resvg::{tiny_skia, usvg};

use super::layout::{Rect, image_path, layout_with};
use super::model::{Chart, Deck, Fit, Kind, Node, Shape, Slide, Stroke, Theme, VAlign};
pub use super::text::LAYOUT;
use super::text::{Typesetter, color, escape, font_stack};

const SHEET_THUMB_WIDTH: f64 = 520.0;
const SHEET_GAP: f64 = 16.0;
const SHEET_LABEL: f64 = 30.0;
const CACHE_WIDTH: u32 = 1200;

// -- non-text nodes ------------------------------------------------------------

fn chart(
    out: &mut String,
    chart: Chart,
    labels: &[String],
    series: &[super::Series],
    theme: &Theme,
) {
    let palette = [
        color(theme, "accent"),
        color(theme, "fg"),
        color(theme, "muted"),
    ];
    let max = series
        .iter()
        .flat_map(|s| &s.values)
        .fold(1.0_f64, |m, v| m.max(v.abs()));
    let count = labels.len().max(1) as f64;
    if chart == Chart::Pie {
        let data = series.first().map_or(&[][..], |s| s.values.as_slice());
        let total = data.iter().map(|v| v.max(0.0)).sum::<f64>().max(1.0);
        let mut angle = -std::f64::consts::FRAC_PI_2;
        for (index, value) in data.iter().enumerate() {
            let value = value.max(0.0);
            let end = angle + value / total * std::f64::consts::TAU;
            let fill = &palette[index % palette.len()];
            if value / total >= 0.999_999 {
                let _ = write!(
                    out,
                    "<circle cx=\"160\" cy=\"80\" r=\"65\" fill=\"{fill}\"/>"
                );
            } else if value > 0.0 {
                let _ = write!(
                    out,
                    "<path d=\"M160 80 L{} {} A65 65 0 {} 1 {} {} Z\" fill=\"{fill}\"/>",
                    160.0 + 65.0 * angle.cos(),
                    80.0 + 65.0 * angle.sin(),
                    u8::from(end - angle > std::f64::consts::PI),
                    160.0 + 65.0 * end.cos(),
                    80.0 + 65.0 * end.sin(),
                );
            }
            angle = end;
        }
    } else {
        let per_series = series.len().max(1) as f64;
        for (s, one) in series.iter().enumerate() {
            let fill = &palette[s % palette.len()];
            let mut points = String::new();
            for (index, value) in one.values.iter().enumerate() {
                let x = 10.0 + (index as f64 + 0.5) * 300.0 / count;
                let y = 140.0 - value / max * 120.0;
                let _ = write!(points, "{x},{y} ");
                if chart == Chart::Bar {
                    let _ = write!(
                        out,
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\"/>",
                        10.0 + index as f64 * 300.0 / count + s as f64 * 260.0 / count / per_series,
                        y.min(140.0),
                        250.0 / count / per_series,
                        (140.0 - y).abs(),
                    );
                }
            }
            if chart == Chart::Line {
                let _ = write!(
                    out,
                    "<polyline points=\"{points}\" fill=\"none\" stroke=\"{fill}\" stroke-width=\"3\"/>"
                );
            }
        }
    }
    let fg = color(theme, "fg");
    let family = escape(&font_stack(theme, "body"));
    for (index, label) in labels.iter().enumerate() {
        let _ = write!(
            out,
            "<text x=\"{}\" y=\"170\" text-anchor=\"middle\" font-size=\"10\" font-family=\"{family}\" fill=\"{fg}\">{}</text>",
            10.0 + (index as f64 + 0.5) * 300.0 / count,
            escape(label),
        );
    }
}

/// The deck image as a PNG data URL, from the deck's own `assets` folder or
/// the remote-image cache. Never fetches.
fn image_data(src: &str, assets: Option<&Path>) -> Option<String> {
    let path = image_path(src, assets)?;
    let image = image::load_from_memory(&std::fs::read(path).ok()?).ok()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(png.into_inner()))
}

fn stroke_attrs(stroke: Option<&Stroke>, theme: &Theme) -> String {
    stroke.filter(|s| s.width > 0.0).map_or_else(
        || "stroke=\"none\"".into(),
        |s| {
            format!(
                "stroke=\"{}\" stroke-width=\"{}\"",
                color(theme, &s.color),
                s.width
            )
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn shape(
    out: &mut String,
    kind: Shape,
    rect: Rect,
    fill: Option<&str>,
    stroke: Option<&Stroke>,
    radius: f64,
    theme: &Theme,
) {
    let Rect { x, y, w, h } = rect;
    let fill_color = fill.map_or_else(|| "none".to_string(), |f| color(theme, f));
    match kind {
        Shape::Rect | Shape::Ellipse => {
            // Strokes are drawn inside the box, like the editor's inset border.
            let width = stroke.map_or(0.0, |s| s.width.max(0.0));
            let (ix, iy, iw, ih) = (
                x + width / 2.0,
                y + width / 2.0,
                (w - width).max(0.0),
                (h - width).max(0.0),
            );
            let attrs = stroke_attrs(stroke, theme);
            if kind == Shape::Rect {
                let _ = write!(
                    out,
                    "<rect x=\"{ix}\" y=\"{iy}\" width=\"{iw}\" height=\"{ih}\" rx=\"{radius}\" fill=\"{fill_color}\" {attrs}/>"
                );
            } else {
                let _ = write!(
                    out,
                    "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" fill=\"{fill_color}\" {attrs}/>",
                    x + w / 2.0,
                    y + h / 2.0,
                    iw / 2.0,
                    ih / 2.0,
                );
            }
        }
        Shape::Line | Shape::Arrow => {
            let (line_color, width) = stroke.map_or_else(
                || {
                    (
                        fill.map_or_else(|| color(theme, "accent"), |f| color(theme, f)),
                        4.0,
                    )
                },
                |s| (color(theme, &s.color), s.width.max(1.0)),
            );
            let cy = y + h / 2.0;
            let mut path = format!("M{} {cy} H{}", x + 2.0, x + w - 2.0);
            if kind == Shape::Arrow {
                let head = (w * 0.15).min(h.max(width * 6.0)).min(48.0);
                let tip = x + w - 2.0;
                let _ = write!(
                    path,
                    " M{} {} L{tip} {cy} L{} {}",
                    tip - head,
                    cy - head * 0.6,
                    tip - head,
                    cy + head * 0.6
                );
            }
            let _ = write!(
                out,
                "<path d=\"{path}\" fill=\"none\" stroke=\"{line_color}\" stroke-width=\"{width}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            );
        }
    }
}

struct Painter<'d, 't, 'x> {
    deck: &'d Deck,
    assets: Option<&'d Path>,
    ts: &'t mut Typesetter<'x>,
    rects: HashMap<String, Rect>,
    clips: &'t mut u32,
}

impl Painter<'_, '_, '_> {
    // One arm per node kind.
    #[allow(clippy::too_many_lines)]
    fn node(&mut self, out: &mut String, node: &Node) {
        if node.hidden {
            return;
        }
        let Some(rect) = self.rects.get(&node.id).copied() else {
            return;
        };
        let theme = &self.deck.theme;
        let wrapped = node.opacity < 1.0 || node.rotation.is_some_and(|r| r != 0.0);
        if wrapped {
            let _ = write!(out, "<g opacity=\"{}\"", node.opacity);
            if let Some(r) = node.rotation.filter(|r| *r != 0.0) {
                let _ = write!(
                    out,
                    " transform=\"rotate({r} {} {})\"",
                    rect.x + rect.w / 2.0,
                    rect.y + rect.h / 2.0
                );
            }
            out.push('>');
        }
        let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
        match &node.kind {
            Kind::Frame {
                fill,
                stroke,
                radius,
                clip,
                children,
                ..
            } => {
                if let Some(fill) = fill {
                    let _ = write!(
                        out,
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"{radius}\" fill=\"{}\"/>",
                        color(theme, fill)
                    );
                }
                if let Some(stroke) = stroke.as_ref().filter(|s| s.width > 0.0) {
                    let half = stroke.width / 2.0;
                    let _ = write!(
                        out,
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{}\"/>",
                        x + half,
                        y + half,
                        (w - stroke.width).max(0.0),
                        (h - stroke.width).max(0.0),
                        (radius - half).max(0.0),
                        color(theme, &stroke.color),
                        stroke.width
                    );
                }
                if *clip {
                    *self.clips += 1;
                    let id = *self.clips;
                    let _ = write!(
                        out,
                        "<clipPath id=\"clip{id}\"><rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"{radius}\"/></clipPath><g clip-path=\"url(#clip{id})\">"
                    );
                }
                for child in children {
                    self.node(out, child);
                }
                if *clip {
                    out.push_str("</g>");
                }
            }
            Kind::Text { text, style, font } => {
                let font = theme.resolve_font(style, font);
                let fill = color(theme, &font.color);
                let valign = font.valign;
                let style = self.ts.text_style(font, false);
                let mut dy = 0.0;
                if valign != VAlign::Top {
                    let (height, _) =
                        self.ts
                            .markdown(&mut String::new(), &style, text, (x, y, w), &fill);
                    dy = ((h - height).max(0.0)) * if valign == VAlign::Middle { 0.5 } else { 1.0 };
                }
                let _ = write!(out, "<g fill=\"{fill}\">");
                self.ts.markdown(out, &style, text, (x, y + dy, w), &fill);
                out.push_str("</g>");
            }
            Kind::Code {
                source,
                style,
                font,
                ..
            } => {
                let font = theme.resolve_font(style, font);
                let fill = color(theme, &font.color);
                let style = self.ts.text_style(font, true);
                let _ = write!(out, "<g fill=\"{fill}\">");
                self.ts.code(out, &style, source, (x, y, w), h, &fill);
                out.push_str("</g>");
            }
            Kind::Table { rows, style, font } => {
                let font = theme.resolve_font(style, font);
                let fill = color(theme, &font.color);
                let style = self.ts.text_style(font, false);
                let _ = write!(out, "<g fill=\"{fill}\">");
                self.ts.table(out, &style, rows, (x, y, w), &fill);
                out.push_str("</g>");
            }
            Kind::Shape {
                shape: kind,
                fill,
                stroke,
                radius,
            } => shape(
                out,
                *kind,
                rect,
                fill.as_deref(),
                stroke.as_ref(),
                *radius,
                theme,
            ),
            Kind::Chart {
                chart: kind,
                labels,
                series,
            } => {
                let _ = write!(
                    out,
                    "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {} {}\" preserveAspectRatio=\"none\">",
                    LAYOUT.chart_width, LAYOUT.chart_height
                );
                chart(out, *kind, labels, series, theme);
                out.push_str("</svg>");
            }
            Kind::Image { src, fit } => {
                if let Some(data) = image_data(src, self.assets) {
                    let _ = write!(
                        out,
                        "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\"><image width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"xMidYMid {}\" href=\"data:image/png;base64,{data}\"/></svg>",
                        if *fit == Fit::Cover { "slice" } else { "meet" },
                    );
                } else {
                    let _ = write!(
                        out,
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"#dddcd5\"/><text x=\"{}\" y=\"{}\" font-size=\"28\" fill=\"#6f6d66\">Image not available</text>",
                        x + 14.0,
                        y + 38.0,
                    );
                }
            }
        }
        if wrapped {
            out.push_str("</g>");
        }
    }
}

fn slide_body(
    deck: &Deck,
    slide: &Slide,
    assets: Option<&Path>,
    ts: &mut Typesetter<'_>,
    clips: &mut u32,
) -> String {
    let theme = &deck.theme;
    let size = theme.size;
    let rects = layout_with(ts, deck, slide, assets).into_iter().collect();
    let mut out = format!(
        "<rect width=\"{}\" height=\"{}\" fill=\"{}\"/>",
        size.w,
        size.h,
        color(theme, slide.fill.as_deref().unwrap_or("bg"))
    );
    let mut painter = Painter {
        deck,
        assets,
        ts,
        rects,
        clips,
    };
    for node in &slide.children {
        painter.node(&mut out, node);
    }
    out
}

fn raster(
    body: &str,
    width: u32,
    height: u32,
    view: (f64, f64),
    ts: &Typesetter<'_>,
) -> Result<Vec<u8>> {
    let source = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {} {}\">{body}</svg>",
        view.0, view.1
    );
    let tree = usvg::Tree::from_str(&source, ts.options())?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("invalid render size")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}

/// Render a slide to a PNG `width_px` wide. Local images and imported fonts
/// are skipped; use [`render_slide_at`] to draw a deck's own assets.
pub fn render_slide(deck: &Deck, slide_id: &str, width_px: u32) -> Result<Vec<u8>> {
    render_slide_at(deck, slide_id, width_px, None)
}

/// Render using the deck's already-local `assets` folder. Never downloads.
pub fn render_slide_at(
    deck: &Deck,
    slide_id: &str,
    width_px: u32,
    assets: Option<&Path>,
) -> Result<Vec<u8>> {
    ensure!(
        (32..=3200).contains(&width_px),
        "render width must be 32 to 3200"
    );
    let slide = deck
        .slides
        .iter()
        .find(|s| s.id == slide_id)
        .context("unknown slide")?;
    let size = deck.theme.size;
    let mut ts = Typesetter::new(&deck.theme, assets);
    let body = slide_body(deck, slide, assets, &mut ts, &mut 0);
    let height = (f64::from(width_px) * size.h / size.w).round() as u32;
    raster(&body, width_px, height, (size.w, size.h), &ts)
}

/// A grid of numbered thumbnails, in the requested order.
pub fn render_deck_sheet(deck: &Deck, slides: &[String], cols: usize) -> Result<Vec<u8>> {
    sheet_at(deck, slides, cols, None)
}

fn sheet_at(deck: &Deck, slides: &[String], cols: usize, assets: Option<&Path>) -> Result<Vec<u8>> {
    ensure!(
        !slides.is_empty() && slides.len() <= 200 && (1..=12).contains(&cols),
        "a contact sheet needs 1 to 200 slides and 1 to 12 columns"
    );
    let cols = cols.min(slides.len());
    let rows = slides.len().div_ceil(cols);
    let size = deck.theme.size;
    let thumb_height = SHEET_THUMB_WIDTH * size.h / size.w;
    let cell_w = SHEET_THUMB_WIDTH + SHEET_GAP;
    let cell_h = thumb_height + SHEET_LABEL + SHEET_GAP;
    let mut ts = Typesetter::new(&deck.theme, assets);
    let mut clips = 0;
    let mut body = String::from("<rect width=\"100%\" height=\"100%\" fill=\"#eeede7\"/>");
    for (index, id) in slides.iter().enumerate() {
        let (number, slide) = deck
            .slides
            .iter()
            .enumerate()
            .find(|(_, s)| s.id == *id)
            .with_context(|| format!("unknown slide {id}"))?;
        let x = (index % cols) as f64 * cell_w + SHEET_GAP;
        let y = (index / cols) as f64 * cell_h + SHEET_GAP;
        let _ = write!(
            body,
            "<svg x=\"{x}\" y=\"{y}\" width=\"{SHEET_THUMB_WIDTH}\" height=\"{thumb_height}\" viewBox=\"0 0 {} {}\">{}</svg>\
             <rect x=\"{x}\" y=\"{y}\" width=\"{SHEET_THUMB_WIDTH}\" height=\"{thumb_height}\" fill=\"none\" stroke=\"#b9b7ac\"/>\
             <text x=\"{x}\" y=\"{}\" fill=\"#282923\" font-size=\"18\" font-family=\"'Helvetica Neue', 'Segoe UI', sans-serif\">Slide {}</text>",
            size.w,
            size.h,
            slide_body(deck, slide, assets, &mut ts, &mut clips),
            y + thumb_height + 22.0,
            number + 1,
        );
    }
    let width = (cols as f64 * cell_w + SHEET_GAP).ceil();
    let height = (rows as f64 * cell_h + SHEET_GAP).ceil();
    raster(&body, width as u32, height as u32, (width, height), &ts)
}

// -- render cache ----------------------------------------------------------------

/// Hex-encode a slide id or sheet key so deck data is never a path component.
fn file_key(key: &str) -> String {
    key.bytes().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// The cache folder for one deck, `<files>/decks/.renders/<stem>`, created on
/// demand. Refuses a path that passes through a symlink.
fn render_dir(deck_path: &Path, files: &Path) -> Result<PathBuf> {
    let name = deck_path
        .file_name()
        .context("deck filename")?
        .to_string_lossy();
    let stem = name.strip_suffix(".deck.json").unwrap_or(&name);
    let directory = files.join("decks/.renders").join(stem);
    let mut ancestor = directory.as_path();
    while ancestor.starts_with(files) && ancestor != files {
        ensure!(
            !ancestor.is_symlink(),
            "render cache must not contain symlinks"
        );
        ancestor = ancestor.parent().context("render cache root")?;
    }
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn relative(files: &Path, output: &Path) -> Result<String> {
    Ok(output
        .strip_prefix(files)?
        .to_string_lossy()
        .replace('\\', "/"))
}

/// Write a render to `<files>/decks/.renders/<deck-stem>/<slide-id>-<revision8>.png`
/// (a contact sheet when `sheet`, covering `ids`) and return its path relative
/// to `files`, ready for a `![alt](path)` reference. Older revisions of the
/// same slide are removed. The folder is a device-local cache.
pub fn render_cached(
    deck: &Deck,
    deck_path: &Path,
    files: &Path,
    ids: &[String],
    sheet: bool,
) -> Result<String> {
    ensure!(!ids.is_empty(), "no slides to render");
    let directory = render_dir(deck_path, files)?;
    let key = file_key(&if sheet {
        format!("sheet-{}", &super::hash(&ids)[..8])
    } else {
        ids[0].clone()
    });
    let name = format!("{key}-{}.png", &deck.revision()[..8]);
    let output = directory.join(&name);
    if !output.is_file() {
        let assets = deck_path.parent().context("deck folder")?.join("assets");
        let bytes = if sheet {
            sheet_at(deck, ids, 3, Some(&assets))?
        } else {
            render_slide_at(deck, &ids[0], CACHE_WIDTH, Some(&assets))?
        };
        super::write_atomic(&output, &bytes)?;
    }
    for entry in std::fs::read_dir(&directory)?.flatten() {
        let old = entry.file_name().to_string_lossy().into_owned();
        if old.starts_with(&format!("{key}-"))
            && Path::new(&old).extension().is_some_and(|ext| ext == "png")
            && old != name
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    relative(files, &output)
}

/// A durable render of one slide for a conversation message. Unlike
/// [`render_cached`] it is never pruned, because a past message keeps
/// referring to it.
pub fn render_snapshot(
    deck: &Deck,
    deck_path: &Path,
    files: &Path,
    slide_id: &str,
) -> Result<String> {
    let directory = render_dir(deck_path, files)?;
    let name = format!("snap-{}-{}.png", file_key(slide_id), &deck.revision()[..8]);
    let output = directory.join(&name);
    if !output.is_file() {
        let assets = deck_path.parent().context("deck folder")?.join("assets");
        let bytes = render_slide_at(deck, slide_id, CACHE_WIDTH, Some(&assets))?;
        super::write_atomic(&output, &bytes)?;
    }
    relative(files, &output)
}

#[cfg(test)]
mod tests;
