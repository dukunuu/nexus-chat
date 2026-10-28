//! `DrawingML` equivalents of deck paints and outer shadows.
use std::fmt::Write as _;

use super::{Theme, integer, rgb};
use crate::deck::{Color, Fill, Node, Rect};

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
