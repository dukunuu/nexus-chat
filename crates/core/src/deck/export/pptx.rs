//! Editable `DrawingML` shapes in a deterministic, hand-written OOXML package.
//! Layout is flattened to slide-space coordinates; fonts are referenced, not embedded.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use super::super::text::{LAYOUT, Mark, Row, classify, color, escape, inline};
use super::super::{
    Align, Deck, Fit, Kind, Mask, Node, Rect, ResolvedFont, Shape, Slide, Stroke, Theme, VAlign,
    layout_at,
};
use anyhow::Result;

mod effects;
mod package;
mod timing;

const NS: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";
const WIDTH: f64 = 12_192_000.0;
/// Points across the slide (12700 EMU per point): 960 pt, so 1920 px text
/// is exported at half its pixel size.
const WIDTH_PT: f64 = WIDTH / 12_700.0;

/// Portable families available in Google Slides; other families (including
/// Google Fonts) retain their author-selected name.
pub(crate) fn google_font(family: &str) -> &str {
    let first = family
        .split(',')
        .next()
        .unwrap_or(family)
        .trim()
        .trim_matches(['\'', '"']);
    match first.to_ascii_lowercase().as_str() {
        "system-ui" | "-apple-system" | "sf pro" | "sf pro display" | "sf pro text"
        | "helvetica neue" | "segoe ui" | "calibri" | "aptos" | "sans-serif" => "Inter",
        "menlo" | "sf mono" | "sfmono-regular" | "ui-monospace" | "monospace" | "consolas" => {
            "JetBrains Mono"
        }
        "ui-serif" | "serif" | "cambria" | "new york" => "Georgia",
        "helvetica" => "Arial",
        "times" => "Times New Roman",
        _ => first,
    }
}

fn integer(value: f64) -> i64 {
    value.round() as i64
}

fn rgb(theme: &Theme, value: &str) -> (String, f64) {
    let hex = color(theme, value).trim_start_matches('#').to_string();
    let hex = if hex.len() <= 4 {
        hex.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        hex
    };
    let alpha = if hex.len() == 8 {
        f64::from(u8::from_str_radix(&hex[6..], 16).unwrap_or(255)) / 255.0
    } else {
        1.0
    };
    (hex[..6].to_uppercase(), alpha)
}

fn fill(theme: &Theme, value: Option<&str>, opacity: f64) -> String {
    value.map_or_else(|| "<a:noFill/>".into(), |value| {
        let (rgb, alpha) = rgb(theme, value);
        format!("<a:solidFill><a:srgbClr val=\"{rgb}\"><a:alpha val=\"{}\"/></a:srgbClr></a:solidFill>", integer(alpha * opacity * 100_000.0))
    })
}

fn line(theme: &Theme, stroke: Option<&Stroke>, scale: f64, opacity: f64, arrow: bool) -> String {
    stroke.map_or_else(
        || "<a:ln><a:noFill/></a:ln>".into(),
        |s| {
            format!(
                "<a:ln w=\"{}\">{}{}{}</a:ln>",
                integer(s.width * scale),
                fill(theme, Some(&s.color), opacity),
                format_args!(
                    "<a:prstDash val=\"{}\"/>",
                    match s.dash {
                        super::super::Dash::Solid => "solid",
                        super::super::Dash::Dash => "dash",
                        super::super::Dash::Dot => "dot",
                    }
                ),
                if arrow {
                    "<a:tailEnd type=\"triangle\" w=\"med\" len=\"med\"/>"
                } else {
                    ""
                }
            )
        },
    )
}

fn xfrm(rect: Rect, scale: f64, rotation: Option<f64>, group: bool) -> String {
    let rot = integer(rotation.unwrap_or(0.0) * 60_000.0);
    let (x, y, w, h) = (
        integer(rect.x * scale),
        integer(rect.y * scale),
        integer(rect.w * scale),
        integer(rect.h * scale),
    );
    format!(
        "<a:xfrm rot=\"{rot}\"><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/>{}</a:xfrm>",
        if group {
            format!("<a:chOff x=\"{x}\" y=\"{y}\"/><a:chExt cx=\"{w}\" cy=\"{h}\"/>")
        } else {
            String::new()
        }
    )
}

fn geometry(shape: Shape, radius: f64, rect: Rect) -> String {
    let kind = match shape {
        Shape::Rect if radius > 0.0 => "roundRect",
        Shape::Rect => "rect",
        Shape::Ellipse => "ellipse",
        Shape::Line | Shape::Arrow => "line",
    };
    let adj = if kind == "roundRect" {
        format!(
            "<a:gd name=\"adj\" fmla=\"val {}\"/>",
            integer((radius / rect.w.min(rect.h).max(1.0) * 100_000.0).clamp(0.0, 50_000.0))
        )
    } else {
        String::new()
    };
    format!("<a:prstGeom prst=\"{kind}\"><a:avLst>{adj}</a:avLst></a:prstGeom>")
}

fn nv(
    id: u32,
    name: &str,
    text: bool,
    placeholder: Option<&super::super::model::Placeholder>,
) -> String {
    format!(
        "<p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{}\"/><p:cNvSpPr{} />{}</p:nvSpPr>",
        escape(name),
        if text && placeholder.is_none() {
            " txBox=\"1\""
        } else {
            ""
        },
        placeholder.map_or_else(
            || "<p:nvPr/>".into(),
            crate::design::template::placeholder_xml
        )
    )
}

fn run(theme: &Theme, font: &ResolvedFont, text: &str, mark: Mark, opacity: f64) -> String {
    let family = escape(google_font(theme.family(if mark == Mark::Code {
        "mono"
    } else {
        &font.family
    })));
    let bold = font.weight >= 600 || mark == Mark::Bold;
    let italic = font.italic || mark == Mark::Italic;
    let pt = font.size * WIDTH_PT / theme.size.w;
    format!(
        "<a:r><a:rPr lang=\"en-US\" sz=\"{}\" b=\"{}\" i=\"{}\" spc=\"{}\">{}<a:latin typeface=\"{family}\"/><a:ea typeface=\"{family}\"/><a:cs typeface=\"{family}\"/></a:rPr><a:t xml:space=\"preserve\">{}</a:t></a:r>",
        integer(pt * 100.0),
        u8::from(bold),
        u8::from(italic),
        integer(font.letter_spacing * pt * 100.0),
        fill(theme, Some(&font.color), opacity),
        escape(&super::transform(text, font.transform))
    )
}

fn paragraphs(
    theme: &Theme,
    font: &ResolvedFont,
    source: &str,
    markdown: bool,
    opacity: f64,
) -> String {
    let final_index = source.matches('\n').count();
    source.split('\n').enumerate().fold(String::new(), |mut out, (index, source)| {
        let mut font = font.clone();
        let row = if markdown { classify(source) } else { Row::Paragraph(source) };
        let (text, bullet, spacing) = match row {
            Row::Heading(level, text) => {
                font.size *= LAYOUT.heading_scale[level - 1];
                font.weight = LAYOUT.heading_weight;
                font.line_height = LAYOUT.heading_line_height;
                if font.family == "body" { font.family = "heading".into(); }
                (text, "<a:buNone/>", LAYOUT.heading_gap)
            }
            Row::Bullet(text) => (text, "<a:buChar char=\"&#8226;\"/>", LAYOUT.list_item_padding),
            Row::Numbered(text) => (text, "<a:buAutoNum type=\"arabicPeriod\"/>", LAYOUT.list_item_padding),
            Row::Paragraph(text) => (text, "<a:buNone/>", if font.line_height < LAYOUT.tight_line_height { 0.0 } else { LAYOUT.paragraph_gap }),
        };
        let align = match font.align { Align::Left => "l", Align::Center => "ctr", Align::Right => "r" };
        let indent = if matches!(row, Row::Bullet(_) | Row::Numbered(_)) { format!(" marL=\"{}\" indent=\"{}\"", integer(font.size * LAYOUT.list_indent * WIDTH / theme.size.w), -integer(font.size * 0.6 * WIDTH / theme.size.w)) } else { String::new() };
        let runs = if markdown { inline(text).iter().map(|r| run(theme, &font, &r.text, r.mark, opacity)).collect::<String>() } else { run(theme, &font, text, Mark::Plain, opacity) };
        let list = matches!(row, Row::Bullet(_) | Row::Numbered(_));
        let pt = font.size * WIDTH_PT / theme.size.w * 100.0;
        let before = if list { LAYOUT.list_item_padding } else { 0.0 };
        let after = if index == final_index && !list { 0.0 } else { spacing };
        let bullet_font = if list { format!("<a:buFont typeface=\"{}\"/>", escape(google_font(theme.family(&font.family)))) } else { String::new() };
        let _ = write!(out, "<a:p><a:pPr algn=\"{align}\"{indent}><a:lnSpc><a:spcPts val=\"{}\"/></a:lnSpc><a:spcBef><a:spcPts val=\"{}\"/></a:spcBef><a:spcAft><a:spcPts val=\"{}\"/></a:spcAft>{bullet_font}{bullet}</a:pPr>{runs}<a:endParaRPr lang=\"en-US\" sz=\"{}\"/></a:p>", integer(font.line_height * pt), integer(before * pt), integer(after * pt), integer(pt));
        out
    })
}

fn body(theme: &Theme, font: &ResolvedFont, text: &str, markdown: bool, opacity: f64) -> String {
    let anchor = match font.valign {
        VAlign::Top => "t",
        VAlign::Middle => "ctr",
        VAlign::Bottom => "b",
    };
    format!(
        "<p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" anchor=\"{anchor}\"><a:noAutofit/></a:bodyPr><a:lstStyle/>{}</p:txBody>",
        paragraphs(theme, font, text, markdown, opacity)
    )
}

struct Painter<'a> {
    deck: &'a Deck,
    assets: Option<&'a Path>,
    scale: f64,
    rects: BTreeMap<String, Rect>,
    ids: BTreeMap<String, timing::Target>,
    next: u32,
    media: Vec<Vec<u8>>,
    warnings: &'a mut Vec<String>,
    title: Option<String>,
}

impl Painter<'_> {
    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    fn text(
        &self,
        node: &Node,
        id: u32,
        rect: Rect,
        font: &ResolvedFont,
        source: &str,
        code: bool,
    ) -> String {
        let title_placeholder = super::super::model::Placeholder {
            kind: "title".into(),
            idx: String::new(),
        };
        let background = if code {
            fill(
                &self.deck.theme,
                Some(super::super::text::CODE_TINT),
                node.opacity * 0.1,
            )
        } else {
            "<a:noFill/>".into()
        };
        format!(
            "<p:sp>{}<p:spPr>{}{}{background}<a:ln><a:noFill/></a:ln></p:spPr>{}</p:sp>",
            nv(
                id,
                node.name.as_deref().unwrap_or(&node.id),
                true,
                node.source_placeholder
                    .as_ref()
                    .or_else(
                        || (self.title.as_deref() == Some(&node.id)).then_some(&title_placeholder)
                    )
            ),
            xfrm(rect, self.scale, node.rotation, false),
            geometry(Shape::Rect, 0.0, rect),
            body(&self.deck.theme, font, source, !code, node.opacity)
        )
    }

    fn picture(
        &mut self,
        node: &Node,
        id: u32,
        rect: Rect,
        png: Vec<u8>,
        fit: Fit,
    ) -> Result<String> {
        let image = image::load_from_memory(&png)?;
        let ratio = f64::from(image.width()) / f64::from(image.height());
        let box_ratio = rect.w / rect.h.max(1.0);
        let focus = node.focus.unwrap_or_default();
        let crop = if fit == Fit::Cover {
            effects::crop(ratio, box_ratio, focus)
        } else {
            String::new()
        };
        let (x, y) = if fit == Fit::Contain {
            if ratio > box_ratio {
                (0.0, (1.0 - box_ratio / ratio) * 100_000.0)
            } else {
                ((1.0 - ratio / box_ratio) * 100_000.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        };
        let inset = format!(
            "<a:fillRect l=\"{}\" r=\"{}\" t=\"{}\" b=\"{}\"/>",
            integer(x * focus.x),
            integer(x * (1.0 - focus.x)),
            integer(y * focus.y),
            integer(y * (1.0 - focus.y))
        );
        if node.mask == Some(Mask::Circle) && (rect.w - rect.h).abs() > 0.01 {
            self.warnings.push(format!("{}: a non-square circle mask becomes an ellipse in PowerPoint; use a square image box for a circle.", node.id));
        }
        // A mask without a preset travels in the picture's alpha; its scrim
        // gets the exact outline as custom geometry.
        let preset = node.mask.and_then(|mask| effects::preset_mask(mask, rect));
        let (png, geom, overlay_geom) = match (node.mask, preset) {
            (Some(_), Some(preset)) => (png, preset.clone(), preset),
            (Some(mask), None) => (
                effects::bake_mask(&png, mask, rect, fit, focus)?,
                geometry(Shape::Rect, 0.0, rect),
                effects::custom_mask(mask, rect),
            ),
            (None, _) => {
                let geom = geometry(Shape::Rect, node.image_radius.unwrap_or(0.0), rect);
                (png, geom.clone(), geom)
            }
        };
        self.media.push(png);
        let relationship = self.media.len() + 1;
        let shadow = effects::shadow(node, &self.deck.theme, rect, self.scale);
        let mut picture = format!(
            "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"{}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId{relationship}\"><a:alphaModFix amt=\"{}\"/></a:blip>{crop}<a:stretch>{inset}</a:stretch></p:blipFill><p:spPr>{}{geom}{shadow}</p:spPr></p:pic>",
            escape(&node.id),
            integer(node.opacity * 100_000.0),
            xfrm(rect, self.scale, node.rotation, false)
        );
        if let Some(overlay) = &node.overlay {
            let overlay_id = self.id();
            let _ = write!(
                picture,
                "<p:sp>{}<p:spPr>{}{overlay_geom}{}<a:ln><a:noFill/></a:ln></p:spPr></p:sp>",
                nv(overlay_id, "Image overlay", false, None),
                xfrm(rect, self.scale, node.rotation, false),
                effects::fill(&self.deck.theme, Some(&overlay.fill), node.opacity)
            );
        }
        Ok(picture)
    }

    fn background(&mut self, slide: &Slide) -> Result<(String, String)> {
        let default_fill = super::super::Fill::from("bg");
        let fallback = effects::fill(
            &self.deck.theme,
            Some(slide.fill.as_ref().unwrap_or(&default_fill)),
            1.0,
        );
        let Some(background) = &slide.background else {
            return Ok((fallback, String::new()));
        };
        let image = super::super::layout::image_path(&background.image, self.assets)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| image::load_from_memory(&bytes).ok());
        let Some(image) = image else {
            self.warnings.push(format!(
                "{}: background image unavailable ({})",
                slide.id, background.image
            ));
            return Ok((fallback, String::new()));
        };
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png)?;
        self.media.push(png.into_inner());
        let relationship = self.media.len() + 1;
        let ratio = f64::from(image.width()) / f64::from(image.height());
        let size = self.deck.theme.size;
        let crop = if background.fit == Fit::Cover {
            effects::crop(ratio, size.w / size.h, super::super::Focus::default())
        } else {
            String::new()
        };
        let (x, y) = if background.fit == Fit::Contain {
            if ratio > size.w / size.h {
                (0, integer((1.0 - size.w / ratio / size.h) * 50_000.0))
            } else {
                (integer((1.0 - size.h * ratio / size.w) * 50_000.0), 0)
            }
        } else {
            (0, 0)
        };
        let fill = format!(
            "<a:blipFill><a:blip r:embed=\"rId{relationship}\"/>{crop}<a:stretch><a:fillRect l=\"{x}\" r=\"{x}\" t=\"{y}\" b=\"{y}\"/></a:stretch></a:blipFill>"
        );
        let overlay = background
            .overlay
            .as_ref()
            .map_or_else(String::new, |overlay| {
                let id = self.id();
                let rect = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: size.w,
                    h: size.h,
                };
                format!(
                    "<p:sp>{}<p:spPr>{}{}{}<a:ln><a:noFill/></a:ln></p:spPr></p:sp>",
                    nv(id, "Background overlay", false, None),
                    xfrm(rect, self.scale, None, false),
                    geometry(Shape::Rect, 0.0, rect),
                    effects::fill(&self.deck.theme, Some(&overlay.fill), 1.0)
                )
            });
        Ok((fill, overlay))
    }

    fn table(
        &self,
        node: &Node,
        id: u32,
        rect: Rect,
        rows: &[Vec<String>],
        font: &ResolvedFont,
    ) -> String {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let width = integer(rect.w * self.scale / columns as f64);
        let height = integer(rect.h * self.scale / rows.len().max(1) as f64);
        let grid = format!("<a:gridCol w=\"{width}\"/>").repeat(columns);
        let mut cells = String::new();
        for (r, row) in rows.iter().enumerate() {
            let _ = write!(cells, "<a:tr h=\"{height}\">");
            for c in 0..columns {
                let mut font = font.clone();
                if r == 0 {
                    font.weight = 700;
                }
                let p = paragraphs(
                    &self.deck.theme,
                    &font,
                    row.get(c).map_or("", String::as_str),
                    false,
                    node.opacity,
                );
                let border = line(
                    &self.deck.theme,
                    Some(&Stroke {
                        color: font.color.clone(),
                        width: 1.0,
                        dash: super::super::Dash::Solid,
                    }),
                    self.scale,
                    node.opacity,
                    false,
                );
                let border = ["lnL", "lnR", "lnT", "lnB"]
                    .map(|name| border.replace("a:ln", &format!("a:{name}")))
                    .join("");
                let _ = write!(
                    cells,
                    "<a:tc><a:txBody><a:bodyPr/><a:lstStyle/>{p}</a:txBody><a:tcPr marL=\"0\" marR=\"0\" marT=\"0\" marB=\"0\">{border}<a:noFill/></a:tcPr></a:tc>"
                );
            }
            cells.push_str("</a:tr>");
        }
        format!(
            "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"{id}\" name=\"{}\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm rot=\"{}\"><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/table\"><a:tbl><a:tblPr firstRow=\"1\" bandRow=\"0\"/><a:tblGrid>{grid}</a:tblGrid>{cells}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>",
            escape(&node.id),
            integer(node.rotation.unwrap_or(0.0) * 60_000.0),
            integer(rect.x * self.scale),
            integer(rect.y * self.scale),
            integer(rect.w * self.scale),
            integer(rect.h * self.scale)
        )
    }

    #[allow(clippy::too_many_lines)]
    fn node(
        &mut self,
        node: &Node,
        inherited_opacity: f64,
        rotations: &[(Rect, f64)],
    ) -> Result<String> {
        if node.hidden {
            return Ok(String::new());
        }
        let Some(original_rect) = self.rects.get(&node.id).copied() else {
            return Ok(String::new());
        };
        let mut rect = original_rect;
        for (parent, angle) in rotations.iter().rev() {
            let (sin, cos) = angle.to_radians().sin_cos();
            let (cx, cy) = (parent.x + parent.w / 2.0, parent.y + parent.h / 2.0);
            let (dx, dy) = (rect.x + rect.w / 2.0 - cx, rect.y + rect.h / 2.0 - cy);
            rect.x = cx + dx * cos - dy * sin - rect.w / 2.0;
            rect.y = cy + dx * sin + dy * cos - rect.h / 2.0;
        }
        let id = self.id();
        let (shape, paragraphs) = match &node.kind {
            Kind::Text { text, .. } | Kind::Code { source: text, .. } => {
                (true, Some(text.split('\n').count()))
            }
            Kind::Shape { .. } | Kind::Frame { .. } => (true, None),
            _ => (false, None),
        };
        self.ids.insert(
            node.id.clone(),
            timing::Target {
                id,
                shape,
                paragraphs,
            },
        );
        let mut child_rotations = rotations.to_vec();
        if let Some(angle) = node.rotation.filter(|angle| *angle != 0.0) {
            child_rotations.push((original_rect, angle));
        }
        let mut node = node.clone();
        node.opacity *= inherited_opacity;
        node.rotation = Some(
            node.rotation.unwrap_or(0.0) + rotations.iter().map(|(_, angle)| angle).sum::<f64>(),
        );
        let theme = &self.deck.theme;
        Ok(match &node.kind {
            Kind::Frame {
                fill: bg,
                stroke,
                radius,
                clip,
                children,
                ..
            } => {
                if node.animation.is_some() && !children.is_empty() {
                    self.warnings.push(format!(
                        "{}: flattened frame builds animate its background, not its descendants.",
                        node.id
                    ));
                }
                if *clip && !children.is_empty() {
                    self.warnings.push(format!(
                        "{}: PowerPoint export does not clip flattened frame children.",
                        node.id
                    ));
                }
                let mut out = format!(
                    "<p:sp>{}<p:spPr>{}{}{}{}{}</p:spPr></p:sp>",
                    nv(id, node.name.as_deref().unwrap_or(&node.id), false, None),
                    xfrm(rect, self.scale, node.rotation, false),
                    geometry(Shape::Rect, *radius, rect),
                    effects::fill(theme, bg.as_ref(), node.opacity),
                    line(theme, stroke.as_ref(), self.scale, node.opacity, false),
                    effects::shadow(&node, theme, rect, self.scale)
                );
                for child in children {
                    out.push_str(&self.node(child, node.opacity, &child_rotations)?);
                }
                out
            }
            Kind::Text { text, style, font } => self.text(
                &node,
                id,
                rect,
                &theme.resolve_font(style, font),
                text,
                false,
            ),
            Kind::Code {
                source,
                style,
                font,
                ..
            } => {
                let mut font = theme.resolve_font(style, font);
                font.family = "mono".into();
                self.text(&node, id, rect, &font, source, true)
            }
            Kind::Table { rows, style, font } => {
                self.table(&node, id, rect, rows, &theme.resolve_font(style, font))
            }
            Kind::Shape {
                shape,
                fill: bg,
                stroke,
                radius,
            } => {
                let fallback = Stroke {
                    color: bg
                        .as_ref()
                        .and_then(|f| f.colors().first().copied())
                        .map_or_else(|| "accent".into(), |c| c.value().to_string()),
                    width: 4.0,
                    dash: super::super::Dash::Solid,
                };
                let stroke = if matches!(shape, Shape::Line | Shape::Arrow) {
                    Some(stroke.as_ref().unwrap_or(&fallback))
                } else {
                    stroke.as_ref()
                };
                let mut rect = rect;
                if matches!(shape, Shape::Line | Shape::Arrow) {
                    rect.y += rect.h / 2.0;
                    rect.h = 0.0;
                }
                format!(
                    "<p:sp>{}<p:spPr>{}{}{}{}{}</p:spPr></p:sp>",
                    nv(id, &node.id, false, None),
                    xfrm(rect, self.scale, node.rotation, false),
                    geometry(*shape, *radius, rect),
                    effects::fill(theme, bg.as_ref(), node.opacity),
                    line(
                        theme,
                        stroke,
                        self.scale,
                        node.opacity,
                        *shape == Shape::Arrow
                    ),
                    effects::shadow(&node, theme, rect, self.scale)
                )
            }
            Kind::Image { src, fit } => {
                let image = super::super::layout::image_path(src, self.assets)
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|b| image::load_from_memory(&b).ok());
                if let Some(image) = image {
                    let mut png = std::io::Cursor::new(Vec::new());
                    image.write_to(&mut png, image::ImageFormat::Png)?;
                    self.picture(&node, id, rect, png.into_inner(), *fit)?
                } else {
                    self.warnings.push(format!(
                        "{}: image unavailable; exported a labelled placeholder ({src}).",
                        node.id
                    ));
                    let font = theme.resolve_font("caption", &super::super::Font::default());
                    self.text(&node, id, rect, &font, "Image not available", false)
                }
            }
            Kind::Chart { .. } => {
                self.warnings.push(format!(
                    "{}: chart exported as a 2× PNG, not a native editable chart.",
                    node.id
                ));
                let mut deck = self.deck.clone();
                deck.theme.size = super::super::SlideSize {
                    w: rect.w.max(1.0),
                    h: rect.h.max(1.0),
                };
                deck.theme.colors.insert("bg".into(), "#00000000".into());
                let mut chart = node.clone();
                chart.position = super::super::Position::Absolute { x: 0.0, y: 0.0 };
                chart.sizing = super::super::Sizing::fixed(rect.w, rect.h);
                chart.opacity = 1.0;
                chart.rotation = None;
                let mut slide = Slide::new("chart");
                slide.children.push(chart);
                deck.slides = vec![slide];
                let png = super::super::render::export_slide_at(
                    &deck,
                    "chart",
                    (rect.w.max(1.0) * 2.0).round() as u32,
                    self.assets,
                )?;
                self.picture(&node, id, rect, png, Fit::Contain)?
            }
        })
    }
}

/// Export editable `PowerPoint` shapes. Use [`super::export`] to collect warnings.
pub fn export_pptx(deck: &Deck, assets: Option<&Path>) -> Result<Vec<u8>> {
    with_warnings(deck, assets, &mut Vec::new())
}

pub(super) fn with_warnings(
    deck: &Deck,
    assets: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    for font in &deck.theme.font_files {
        warnings.push(format!("Install font '{}' from {}; PowerPoint references fonts by family, without embedding them.", font.family, font.file));
    }
    let mut package = package::Package::new(deck);
    for (i, slide) in deck.slides.iter().enumerate() {
        let title = super::visible_nodes(slide).into_iter().find(|node| node.source_placeholder.as_ref().is_some_and(|ph| matches!(ph.kind.as_str(), "title" | "ctrTitle")))
            .or_else(|| super::visible_nodes(slide).into_iter().find(|n| matches!(&n.kind, Kind::Text { text, .. } if text.lines().any(|l| matches!(classify(l), Row::Heading(..))))))
            .or_else(|| super::visible_nodes(slide).into_iter().find(|n| matches!(&n.kind, Kind::Text { style, .. } if matches!(style.as_str(), "title" | "heading"))))
            .or_else(|| super::visible_nodes(slide).into_iter().find(|n| matches!(n.kind, Kind::Text { .. })));
        let mut painter = Painter {
            deck,
            assets,
            scale: WIDTH / deck.theme.size.w,
            rects: layout_at(deck, slide, assets).into_iter().collect(),
            ids: BTreeMap::new(),
            next: 1,
            media: Vec::new(),
            warnings,
            title: title.map(|n| n.id.clone()),
        };
        let (background, mut shapes) = painter.background(slide)?;
        for node in &slide.children {
            shapes.push_str(&painter.node(node, 1.0, &[])?);
        }
        let transition = timing::transition(slide, painter.warnings);
        let timing = timing::builds(slide, &painter.ids, painter.warnings);
        let xml = format!(
            "<p:sld {NS}><p:cSld name=\"{}\"><p:bg><p:bgPr>{}<a:effectLst/></p:bgPr></p:bg><p:spTree>{}{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>{transition}{timing}</p:sld>",
            escape(
                slide
                    .name
                    .as_deref()
                    .or_else(|| title.and_then(Node::text))
                    .unwrap_or(&deck.title)
            ),
            background,
            package::root_group()
        );
        let layout = slide
            .source_layout
            .as_deref()
            .and_then(|name| name.strip_prefix("ppt/slideLayouts/slideLayout"))
            .and_then(|name| name.strip_suffix(".xml"))
            .and_then(|number| number.parse::<usize>().ok())
            .filter(|index| (1..=5).contains(index))
            .unwrap_or(if title.is_some() { 5 } else { 1 });
        package.slide(i + 1, &xml, &slide.notes, painter.media, layout);
    }
    package.finish()
}
