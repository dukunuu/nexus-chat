//! Optional deck paints and effects, shared by every export surface.
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::{CheckError, Fit, Theme, is_hex, issue};

/// CSS shadow blur radii are twice a Gaussian filter's standard deviation.
pub const SHADOW_BLUR_DIVISOR: f64 = 2.0;

/// A color role or hex value, optionally with independent opacity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Color {
    Solid(String),
    Alpha { color: String, opacity: f64 },
}
impl Color {
    /// The underlying role or hex color.
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::Solid(value) | Self::Alpha { color: value, .. } => value,
        }
    }

    /// Independent opacity, multiplied by any hex alpha.
    #[must_use]
    pub fn opacity(&self) -> f64 {
        match self {
            Self::Solid(_) => 1.0,
            Self::Alpha { opacity, .. } => *opacity,
        }
    }
}

/// Stops use normalized offsets from zero to one, in ascending order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Linear {
    pub angle: f64,
    pub stops: Vec<(f64, Color)>,
}

/// An elliptical gradient centered on the box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radial {
    pub stops: Vec<(f64, Color)>,
}

/// Solid paints retain their original string representation in existing decks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Fill {
    Solid(Color),
    Linear { linear: Linear },
    Radial { radial: Radial },
}
// Compatibility for color-only callers: gradients have their first stop as
// a solid fallback. Renderers must match Fill rather than discard its stops.
impl std::ops::Deref for Fill {
    type Target = str;

    fn deref(&self) -> &str {
        match self {
            Self::Solid(color) => color.value(),
            Self::Linear { linear } => linear.stops.first().map_or("", |(_, c)| c.value()),
            Self::Radial { radial } => radial.stops.first().map_or("", |(_, c)| c.value()),
        }
    }
}
impl std::fmt::Display for Fill {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Solid(Color::Solid(value)) => formatter.write_str(value),
            _ => formatter.write_str(&serde_json::to_string(self).map_err(|_| std::fmt::Error)?),
        }
    }
}
impl From<String> for Fill {
    fn from(value: String) -> Self {
        Self::Solid(Color::Solid(value))
    }
}
impl From<&str> for Fill {
    fn from(value: &str) -> Self {
        value.to_string().into()
    }
}
impl Fill {
    /// Colors contributing to this paint, for validation and design lint.
    #[must_use]
    pub fn colors(&self) -> Vec<&Color> {
        match self {
            Self::Solid(color) => vec![color],
            Self::Linear { linear } => linear.stops.iter().map(|(_, c)| c).collect(),
            Self::Radial { radial } => radial.stops.iter().map(|(_, c)| c).collect(),
        }
    }

    pub(crate) fn check(&self, theme: &Theme, path: &str, errors: &mut Vec<CheckError>) {
        let stops = match self {
            Self::Solid(color) => {
                check_color(color, theme, path, errors);
                return;
            }
            Self::Linear { linear } => {
                if !linear.angle.is_finite() {
                    issue(errors, &format!("{path}.linear.angle"), "expected degrees");
                }
                (&linear.stops, "linear")
            }
            Self::Radial { radial } => (&radial.stops, "radial"),
        };
        let path = format!("{path}.{}.stops", stops.1);
        if !(2..=8).contains(&stops.0.len()) {
            issue(errors, &path, "expected 2 to 8 gradient stops");
        }
        let mut previous = 0.0;
        for (i, (offset, color)) in stops.0.iter().enumerate() {
            if !(previous..=1.0).contains(offset) {
                issue(
                    errors,
                    &format!("{path}[{i}][0]"),
                    "expected ascending offsets from 0 to 1",
                );
            }
            previous = *offset;
            check_color(color, theme, &format!("{path}[{i}][1]"), errors);
        }
    }
}

pub(crate) fn check_color(color: &Color, theme: &Theme, path: &str, errors: &mut Vec<CheckError>) {
    if !(is_hex(color.value()) || theme.colors.contains_key(color.value())) {
        issue(errors, path, "expected theme role or hex color");
    }
    if !(0.0..=1.0).contains(&color.opacity()) {
        issue(errors, &format!("{path}.opacity"), "expected 0 to 1");
    }
}

/// Resolved outer shadow in slide pixels. Blur uses the CSS blur radius.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowSpec {
    pub x: f64,
    pub y: f64,
    pub blur: f64,
    pub spread: f64,
    pub color: Color,
}

/// Presets are copied into self-contained theme tokens by the design.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Shadow {
    Preset(String),
    Custom(ShadowSpec),
}
impl Shadow {
    pub(crate) fn resolve<'a>(&'a self, theme: &'a Theme) -> Option<&'a ShadowSpec> {
        match self {
            Self::Preset(name) => theme.shadows.get(name).or_else(|| presets().get(name)),
            Self::Custom(spec) => Some(spec),
        }
    }

    pub(crate) fn check(&self, theme: &Theme, path: &str, errors: &mut Vec<CheckError>) {
        let Some(spec) = self.resolve(theme) else {
            issue(
                errors,
                path,
                "unknown shadow preset; expected sm, md or lg from the design",
            );
            return;
        };
        for (key, value, min, max) in [
            ("x", spec.x, -2000.0, 2000.0),
            ("y", spec.y, -2000.0, 2000.0),
            ("blur", spec.blur, 0.0, 200.0),
            ("spread", spec.spread, -200.0, 200.0),
        ] {
            if !(min..=max).contains(&value) {
                issue(
                    errors,
                    &format!("{path}.{key}"),
                    &format!("expected {min} to {max} px"),
                );
            }
        }
        check_color(&spec.color, theme, &format!("{path}.color"), errors);
    }
}

/// The built-in design's restrained shadow scale. Explicit theme tokens override it.
#[must_use]
pub fn presets() -> &'static std::collections::BTreeMap<String, ShadowSpec> {
    static PRESETS: std::sync::LazyLock<std::collections::BTreeMap<String, ShadowSpec>> =
        std::sync::LazyLock::new(|| {
            [
                ("sm", 4.0, 12.0, 0.12),
                ("md", 12.0, 32.0, 0.16),
                ("lg", 24.0, 64.0, 0.20),
            ]
            .map(|(name, y, blur, opacity)| {
                (
                    name.into(),
                    ShadowSpec {
                        x: 0.0,
                        y,
                        blur,
                        spread: 0.0,
                        color: Color::Alpha {
                            color: "fg".into(),
                            opacity,
                        },
                    },
                )
            })
            .into()
        });
    &PRESETS
}

/// Normalized image focal point; (0,0) is top left, (1,1) bottom right.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Focus {
    pub x: f64,
    pub y: f64,
}
impl Default for Focus {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5 }
    }
}

/// A scrim painted over an image, inside its mask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub fill: Fill,
}

/// Full-bleed slide photograph, independent of slide layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Background {
    pub image: String,
    #[serde(default)]
    pub fit: Fit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
}

/// Image masks supported consistently across renderers. `circle` is the
/// largest centered circle; the others fill the image box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mask {
    Circle,
    Ellipse,
    /// Fully rounded ends: a rounded rectangle with radius half the short side.
    Pill,
    /// Round across the top, square at the bottom; see [`Mask::arch_radius`].
    Arch,
    /// Flat top and bottom, points at the middle of the left and right sides.
    Hexagon,
    /// An organic outline: [`BLOB`] scaled to the box.
    Blob,
}

/// The blob outline in a unit square: a start point, then cubic Bézier
/// segments `[c1x, c1y, c2x, c2y, x, y]`. Studio's `render.js` repeats these
/// numbers; the joins are smooth.
pub const BLOB_START: (f64, f64) = (0.52, 0.02);
pub const BLOB: [[f64; 6]; 4] = [
    [0.78, 0.0, 0.99, 0.18, 0.97, 0.46],
    [0.95, 0.74, 0.80, 0.99, 0.50, 0.98],
    [0.22, 0.97, 0.02, 0.80, 0.03, 0.52],
    [0.04, 0.22, 0.26, 0.04, 0.52, 0.02],
];

impl Mask {
    /// The arch's top corner radius: half the width, or the height when the
    /// box is wider than twice its height.
    #[must_use]
    pub fn arch_radius(w: f64, h: f64) -> f64 {
        (w / 2.0).min(h).max(0.0)
    }

    /// The outline as SVG path data for a box at (x, y) of size w × h.
    #[must_use]
    pub fn path(self, x: f64, y: f64, w: f64, h: f64) -> String {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        match self {
            Self::Circle | Self::Ellipse => {
                let (rx, ry) = if self == Self::Circle {
                    let radius = w.min(h) / 2.0;
                    (radius, radius)
                } else {
                    (w / 2.0, h / 2.0)
                };
                format!(
                    "M{} {cy}A{rx} {ry} 0 1 0 {} {cy}A{rx} {ry} 0 1 0 {} {cy}Z",
                    cx - rx,
                    cx + rx,
                    cx - rx
                )
            }
            Self::Pill => {
                let radius = w.min(h) / 2.0;
                format!(
                    "M{} {y}H{}A{radius} {radius} 0 0 1 {} {}V{}A{radius} {radius} 0 0 1 {} {}H{}A{radius} {radius} 0 0 1 {x} {}V{}A{radius} {radius} 0 0 1 {} {y}Z",
                    x + radius,
                    x + w - radius,
                    x + w,
                    y + radius,
                    y + h - radius,
                    x + w - radius,
                    y + h,
                    x + radius,
                    y + h - radius,
                    y + radius,
                    x + radius
                )
            }
            Self::Arch => {
                let radius = Self::arch_radius(w, h);
                format!(
                    "M{x} {}V{}A{radius} {radius} 0 0 1 {} {y}H{}A{radius} {radius} 0 0 1 {} {}V{}Z",
                    y + h,
                    y + radius,
                    x + radius,
                    x + w - radius,
                    x + w,
                    y + radius,
                    y + h
                )
            }
            Self::Hexagon => format!(
                "M{} {y}H{}L{} {cy}L{} {}H{}L{x} {cy}Z",
                x + w / 4.0,
                x + w * 0.75,
                x + w,
                x + w * 0.75,
                y + h,
                x + w / 4.0
            ),
            Self::Blob => {
                let at = |px: f64, py: f64| format!("{} {}", x + px * w, y + py * h);
                let mut data = format!("M{}", at(BLOB_START.0, BLOB_START.1));
                for [ax, ay, bx, by, ex, ey] in BLOB {
                    let _ = write!(data, "C{} {} {}", at(ax, ay), at(bx, by), at(ex, ey));
                }
                data.push('Z');
                data
            }
        }
    }

    /// Whether a point in box coordinates (0..w, 0..h) is inside the mask.
    #[must_use]
    pub fn contains(self, w: f64, h: f64, px: f64, py: f64) -> bool {
        if px < 0.0 || py < 0.0 || px > w || py > h {
            return false;
        }
        let in_corner = |r: f64, cx: f64, cy: f64| (px - cx).powi(2) + (py - cy).powi(2) <= r * r;
        match self {
            Self::Circle => in_corner(w.min(h) / 2.0, w / 2.0, h / 2.0),
            Self::Ellipse => {
                let (dx, dy) = ((px - w / 2.0) / (w / 2.0), (py - h / 2.0) / (h / 2.0));
                dx * dx + dy * dy <= 1.0
            }
            Self::Pill => {
                let r = w.min(h) / 2.0;
                let (cx, cy) = (px.clamp(r, w - r), py.clamp(r, h - r));
                in_corner(r, cx, cy)
            }
            Self::Arch => {
                let r = Self::arch_radius(w, h);
                py >= r || in_corner(r, px.clamp(r, w - r), r)
            }
            Self::Hexagon => {
                let edge = w / 4.0 * ((py - h / 2.0).abs() / (h / 2.0));
                px >= edge && px <= w - edge
            }
            Self::Blob => in_outline(&blob_outline(24), px / w, py / h),
        }
    }
}

/// Whether a point is inside a closed polygon (even-odd rule).
#[must_use]
pub fn in_outline(points: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut inside = false;
    for (i, &(ax, ay)) in points.iter().enumerate() {
        let (bx, by) = points[(i + 1) % points.len()];
        if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
            inside = !inside;
        }
    }
    inside
}

/// The blob outline sampled as a polygon in the unit square, `steps` points
/// per segment.
#[must_use]
pub fn blob_outline(steps: usize) -> Vec<(f64, f64)> {
    let mut points = Vec::with_capacity(BLOB.len() * steps);
    let mut from = BLOB_START;
    for [ax, ay, bx, by, ex, ey] in BLOB {
        for step in 0..steps {
            #[allow(clippy::cast_precision_loss)] // a handful of steps
            let along = step as f64 / steps as f64;
            let rest = 1.0 - along;
            // Bernstein weights of the cubic.
            let weights = [
                rest * rest * rest,
                3.0 * rest * rest * along,
                3.0 * rest * along * along,
                along * along * along,
            ];
            points.push((
                weights[0] * from.0 + weights[1] * ax + weights[2] * bx + weights[3] * ex,
                weights[0] * from.1 + weights[1] * ay + weights[2] * by + weights[3] * ey,
            ));
        }
        from = (ex, ey);
    }
    points
}

/// Repair the compact two-color gradient spelling before typed validation.
pub(crate) fn normalize(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object_mut() {
        if let Some(stops) = object
            .get("gradient")
            .and_then(serde_json::Value::as_array)
            .filter(|s| s.len() == 2)
            .cloned()
        {
            object.remove("gradient");
            object.insert("fill".into(), serde_json::json!({"linear": {"angle": 180, "stops": [[0, stops[0]], [1, stops[1]]]}}));
        }
        if let Some(fill) = object.get_mut("fill")
            && let Some(stops) = fill
                .get("gradient")
                .and_then(serde_json::Value::as_array)
                .filter(|s| s.len() == 2)
                .cloned()
        {
            *fill = serde_json::json!({"linear": {"angle": 180, "stops": [[0, stops[0]], [1, stops[1]]]}});
        }
        for value in object.values_mut() {
            normalize(value);
        }
    } else if let Some(items) = value.as_array_mut() {
        for item in items {
            normalize(item);
        }
    }
}

/// Stroke patterns, independent of the stroke width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dash {
    #[default]
    Solid,
    Dash,
    Dot,
}
impl Dash {
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde skip predicate
    pub(crate) fn is_solid(&self) -> bool {
        *self == Self::Solid
    }
}
