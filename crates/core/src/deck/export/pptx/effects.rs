//! `DrawingML` equivalents of deck paints and outer shadows.
use std::fmt::Write as _;

use super::{Theme, integer, rgb};
use crate::deck::effects::{BLOB, BLOB_START, blob_outline, in_outline};
use crate::deck::{Color, Fill, Fit, Focus, Mask, Node, Rect};

fn color(theme: &Theme, color: &Color, opacity: f64) -> String {
    let (rgb, alpha) = rgb(theme, color.value());
    format!(
        "<a:srgbClr val=\"{rgb}\"><a:alpha val=\"{}\"/></a:srgbClr>",
        integer(alpha * color.opacity() * opacity * 100_000.0)
    )
}

pub(super) fn fill(theme: &Theme, fill: Option<&Fill>, opacity: f64) -> String {
    let Some(fill) = fill else {
        return "<a:noFill/>".into();
    };
    let (stops, geometry) = match fill {
        Fill::Solid(value) => return format!("<a:solidFill>{}</a:solidFill>", color(theme, value, opacity)),
        Fill::Linear { linear } => (&linear.stops, format!("<a:lin ang=\"{}\" scaled=\"0\"/>", integer((linear.angle - 90.0).rem_euclid(360.0) * 60_000.0))),
        Fill::Radial { radial } => (&radial.stops, "<a:path path=\"circle\"><a:fillToRect l=\"50000\" t=\"50000\" r=\"50000\" b=\"50000\"/></a:path>".into()),
    };
    let mut out = "<a:gradFill rotWithShape=\"1\"><a:gsLst>".to_string();
    for (at, value) in stops {
        let _ = write!(
            out,
            "<a:gs pos=\"{}\">{}</a:gs>",
            integer(at * 100_000.0),
            color(theme, value, opacity)
        );
    }
    let _ = write!(out, "</a:gsLst>{geometry}</a:gradFill>");
    out
}

pub(super) fn shadow(node: &Node, theme: &Theme, rect: Rect, scale: f64) -> String {
    let Some(spec) = node.shadow.as_ref().and_then(|s| s.resolve(theme)) else {
        return "<a:effectLst/>".into();
    };
    let direction = spec.y.atan2(spec.x).to_degrees().rem_euclid(360.0);
    format!(
        "<a:effectLst><a:outerShdw blurRad=\"{}\" dist=\"{}\" dir=\"{}\" sx=\"{}\" sy=\"{}\" algn=\"ctr\" rotWithShape=\"1\">{}</a:outerShdw></a:effectLst>",
        integer(spec.blur * scale),
        integer(spec.x.hypot(spec.y) * scale),
        integer(direction * 60_000.0),
        integer((1.0 + 2.0 * spec.spread / rect.w.max(1.0)).max(0.0) * 100_000.0),
        integer((1.0 + 2.0 * spec.spread / rect.h.max(1.0)).max(0.0) * 100_000.0),
        color(theme, &spec.color, node.opacity)
    )
}

pub(super) fn crop(ratio: f64, box_ratio: f64, focus: crate::deck::Focus) -> String {
    let (x, y) = if ratio > box_ratio {
        ((1.0 - box_ratio / ratio) * 100_000.0, 0.0)
    } else {
        (0.0, (1.0 - ratio / box_ratio) * 100_000.0)
    };
    format!(
        "<a:srcRect l=\"{}\" r=\"{}\" t=\"{}\" b=\"{}\"/>",
        integer(x * focus.x),
        integer(x * (1.0 - focus.x)),
        integer(y * focus.y),
        integer(y * (1.0 - focus.y))
    )
}

/// A mask `PowerPoint` and Google Slides both draw as a preset picture shape.
pub(super) fn preset_mask(mask: Mask, rect: Rect) -> Option<String> {
    let (preset, guides) = match mask {
        Mask::Circle | Mask::Ellipse => ("ellipse", String::new()),
        Mask::Pill => (
            "roundRect",
            "<a:gd name=\"adj\" fmla=\"val 50000\"/>".into(),
        ),
        // The preset's inset is a fraction of the short side; a quarter of
        // the width and points at mid-height match the deck's hexagon.
        Mask::Hexagon => (
            "hexagon",
            format!(
                "<a:gd name=\"adj\" fmla=\"val {}\"/><a:gd name=\"vf\" fmla=\"val 115470\"/>",
                integer(25_000.0 * rect.w / rect.w.min(rect.h).max(1.0))
            ),
        ),
        Mask::Arch | Mask::Blob => return None,
    };
    Some(format!(
        "<a:prstGeom prst=\"{preset}\"><a:avLst>{guides}</a:avLst></a:prstGeom>"
    ))
}

/// The exact outline of a mask without a preset, as custom geometry in
/// hundredths of a slide pixel. Used for shapes; pictures get the mask in
/// their alpha instead (see [`bake_mask`]), which every importer keeps.
pub(super) fn custom_mask(mask: Mask, rect: Rect) -> String {
    let unit = |value: f64| integer(value * 100.0);
    let (w, h) = (rect.w, rect.h);
    let pt = |x: f64, y: f64| format!("<a:pt x=\"{}\" y=\"{}\"/>", unit(x), unit(y));
    let mut path = String::new();
    if mask == Mask::Arch {
        let r = Mask::arch_radius(w, h);
        let arc = |start: i64| {
            format!(
                "<a:arcTo wR=\"{0}\" hR=\"{0}\" stAng=\"{start}\" swAng=\"5400000\"/>",
                unit(r)
            )
        };
        let _ = write!(
            path,
            "<a:moveTo>{}</a:moveTo><a:lnTo>{}</a:lnTo>{}<a:lnTo>{}</a:lnTo>{}<a:lnTo>{}</a:lnTo><a:close/>",
            pt(0.0, h),
            pt(0.0, r),
            arc(10_800_000),
            pt(w - r, 0.0),
            arc(16_200_000),
            pt(w, h)
        );
    } else {
        let (sx, sy) = BLOB_START;
        let _ = write!(path, "<a:moveTo>{}</a:moveTo>", pt(sx * w, sy * h));
        for [ax, ay, bx, by, ex, ey] in BLOB {
            let _ = write!(
                path,
                "<a:cubicBezTo>{}{}{}</a:cubicBezTo>",
                pt(ax * w, ay * h),
                pt(bx * w, by * h),
                pt(ex * w, ey * h)
            );
        }
        path.push_str("<a:close/>");
    }
    format!(
        "<a:custGeom><a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/><a:rect l=\"0\" t=\"0\" r=\"r\" b=\"b\"/><a:pathLst><a:path w=\"{}\" h=\"{}\">{path}</a:path></a:pathLst></a:custGeom>",
        unit(w),
        unit(h)
    )
}

/// The picture with everything outside `mask` made transparent, as PNG.
/// Pixels map to the box the way the picture is placed: scaled by `fit` and
/// offset by `focus`. Edges are 4× supersampled.
pub(super) fn bake_mask(
    png: &[u8],
    mask: Mask,
    rect: Rect,
    fit: Fit,
    focus: Focus,
) -> anyhow::Result<Vec<u8>> {
    let mut image = image::load_from_memory(png)?.to_rgba8();
    let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
    let (w, h) = (rect.w.max(1.0), rect.h.max(1.0));
    let scale = if fit == Fit::Cover {
        (w / iw).max(h / ih)
    } else {
        (w / iw).min(h / ih)
    };
    let (ox, oy) = ((w - iw * scale) * focus.x, (h - ih * scale) * focus.y);
    let outline = (mask == Mask::Blob).then(|| blob_outline(24));
    // The blob's outline is sampled once, not per pixel.
    let inside = |bx: f64, by: f64| match &outline {
        Some(points) => {
            (0.0..=w).contains(&bx) && (0.0..=h).contains(&by) && in_outline(points, bx / w, by / h)
        }
        None => mask.contains(w, h, bx, by),
    };
    for (px, py, pixel) in image.enumerate_pixels_mut() {
        let covered = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
            .iter()
            .filter(|(dx, dy)| {
                inside(
                    ox + (f64::from(px) + dx) * scale,
                    oy + (f64::from(py) + dy) * scale,
                )
            })
            .count();
        if covered < 4 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255
            let alpha = (f64::from(pixel[3]) * covered as f64 / 4.0).round() as u8;
            pixel[3] = alpha;
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}
