//! SVG paint servers, outer-shadow filters and image masks.
use std::fmt::Write as _;
use std::path::Path;

use super::super::text::{color, escape};
use super::super::{Color, Dash, Fill, Fit, Focus, Node, Rect, Theme};

pub(super) fn rgba(theme: &Theme, value: &Color) -> (String, f64) {
    let raw = color(theme, value.value());
    let hex = raw.trim_start_matches('#');
    let hex = if matches!(hex.len(), 3 | 4) {
        hex.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        hex.to_string()
    };
    let alpha = if hex.len() == 8 {
        f64::from(u8::from_str_radix(&hex[6..], 16).unwrap_or(255)) / 255.0
    } else {
        1.0
    };
    (format!("#{}", &hex[..6]), alpha * value.opacity())
}

pub(super) fn paint(
    out: &mut String,
    fill: &Fill,
    theme: &Theme,
    rect: Rect,
    next: &mut u32,
) -> String {
    let (stops, kind, attrs) = match fill {
        Fill::Solid(color) => {
            let (color, alpha) = rgba(theme, color);
            return format!("fill=\"{color}\" fill-opacity=\"{alpha}\"");
        }
        Fill::Linear { linear } => {
            let radians = linear.angle.to_radians();
            let (dx, dy) = (radians.sin(), -radians.cos());
            let length = rect.w * dx.abs() + rect.h * dy.abs();
            let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            (
                &linear.stops,
                "linearGradient",
                format!(
                    "gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"",
                    cx - dx * length / 2.0,
                    cy - dy * length / 2.0,
                    cx + dx * length / 2.0,
                    cy + dy * length / 2.0
                ),
            )
        }
        Fill::Radial { radial } => (
            &radial.stops,
            "radialGradient",
            "cx=\".5\" cy=\".5\" r=\".5\"".into(),
        ),
    };
    *next += 1;
    let id = *next;
    let _ = write!(out, "<defs><{kind} id=\"paint{id}\" {attrs}>");
    for (offset, color) in stops {
        let (color, alpha) = rgba(theme, color);
        let _ = write!(
            out,
            "<stop offset=\"{offset}\" stop-color=\"{color}\" stop-opacity=\"{alpha}\"/>"
        );
    }
    let _ = write!(out, "</{kind}></defs>");
    format!("fill=\"url(#paint{id})\"")
}

pub(super) fn dash(dash: Dash, width: f64) -> String {
    match dash {
        Dash::Solid => String::new(),
        Dash::Dash => format!(" stroke-dasharray=\"{} {}\"", width * 3.0, width * 2.0),
        Dash::Dot => format!(" stroke-dasharray=\"{width} {}\"", width * 2.0),
    }
}

pub(super) fn shadow(
    out: &mut String,
    node: &Node,
    theme: &Theme,
    next: &mut u32,
    source: bool,
) -> bool {
    let Some(spec) = node.shadow.as_ref().and_then(|s| s.resolve(theme)) else {
        return false;
    };
    *next += 1;
    let id = *next;
    let (color, alpha) = rgba(theme, &spec.color);
    let _ = write!(
        out,
        "<defs><filter id=\"shadow{id}\" x=\"-100%\" y=\"-100%\" width=\"300%\" height=\"300%\" color-interpolation-filters=\"sRGB\">"
    );
    if spec.spread != 0.0 {
        let _ = write!(
            out,
            "<feMorphology in=\"SourceAlpha\" operator=\"{}\" radius=\"{}\"/>",
            if spec.spread > 0.0 { "dilate" } else { "erode" },
            spec.spread.abs()
        );
    }
    let _ = write!(
        out,
        "<feGaussianBlur stdDeviation=\"{}\"/><feOffset dx=\"{}\" dy=\"{}\" result=\"offset\"/><feFlood flood-color=\"{color}\" flood-opacity=\"{alpha}\"/><feComposite in2=\"offset\" operator=\"in\"/><feMerge><feMergeNode/>{}</feMerge></filter></defs><g filter=\"url(#shadow{id})\">",
        spec.blur / super::super::effects::SHADOW_BLUR_DIVISOR,
        spec.x,
        spec.y,
        if source {
            "<feMergeNode in=\"SourceGraphic\"/>"
        } else {
            ""
        }
    );
    true
}

pub(super) fn image(
    out: &mut String,
    node: &Node,
    rect: Rect,
    theme: &Theme,
    assets: Option<&Path>,
    next: &mut u32,
) {
    let super::super::Kind::Image { src, fit } = &node.kind else {
        return;
    };
    *next += 1;
    let id = *next;
    let Rect { x, y, w, h } = rect;
    let _ = write!(out, "<defs><clipPath id=\"image{id}\">");
    if let Some(mask) = node.mask {
        let _ = write!(out, "<path d=\"{}\"/>", mask.path(x, y, w, h));
    } else {
        let _ = write!(
            out,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"{}\"/>",
            node.image_radius.unwrap_or(0.0)
        );
    }
    let _ = write!(out, "</clipPath></defs><g clip-path=\"url(#image{id})\">");
    if let Some(data) = super::image_data(src, assets) {
        let dimensions = super::super::layout::image_path(src, assets)
            .and_then(|p| image::image_dimensions(p).ok())
            .unwrap_or((640, 360));
        let (iw, ih) = (f64::from(dimensions.0), f64::from(dimensions.1));
        let scale = if *fit == Fit::Cover {
            (w / iw).max(h / ih)
        } else {
            (w / iw).min(h / ih)
        };
        let Focus { x: fx, y: fy } = node.focus.unwrap_or_default();
        let _ = write!(
            out,
            "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" href=\"data:image/png;base64,{data}\"/>",
            x + (w - iw * scale) * fx,
            y + (h - ih * scale) * fy,
            iw * scale,
            ih * scale
        );
    } else {
        let _ = write!(
            out,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"#dddcd5\"/><text x=\"{}\" y=\"{}\" font-size=\"28\" fill=\"#6f6d66\">{}</text>",
            x + 14.0,
            y + 38.0,
            escape("Image not available")
        );
    }
    if let Some(overlay) = &node.overlay {
        let fill = paint(out, &overlay.fill, theme, rect, next);
        let _ = write!(
            out,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" {fill}/>"
        );
    }
    out.push_str("</g>");
}
