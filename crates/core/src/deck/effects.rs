//! Optional deck paints and effects, shared by every export surface.
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

/// Image masks supported consistently across renderers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mask {
    Circle,
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
