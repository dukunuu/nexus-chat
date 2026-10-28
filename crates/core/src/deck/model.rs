//! Deck v2 data model: a node tree on a pixel canvas with Figma-style
//! auto-layout, numeric typography and named text styles.
//!
//! Everything here is plain serde data. Layout lives in `layout.rs`, editing
//! in `ops.rs`, validation in `validate.rs`.

// `serialize_with` and `skip_serializing_if` helpers must take references, and
// `default` functions must return the field type.
#![allow(
    clippy::cast_possible_truncation,
    clippy::ref_option,
    clippy::trivially_copy_pass_by_ref,
    clippy::unnecessary_wraps
)]

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::effects::{Background, Dash, Fill, Focus, Mask, Overlay, Shadow, ShadowSpec};

/// Default slide width in px.
pub const SLIDE_WIDTH: f64 = 1920.0;
/// Default slide height in px.
pub const SLIDE_HEIGHT: f64 = 1080.0;
/// One version-1 grid unit in px (`1920 / 32`).
pub const GRID: f64 = 60.0;

/// Serialize whole numbers as integers so decks stay readable.
pub(super) fn num<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if value.fract() == 0.0 && value.abs() < 1e12 {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

fn num_opt<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(v) => num(v, serializer),
        None => serializer.serialize_none(),
    }
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn is_one(v: &f64) -> bool {
    (*v - 1.0).abs() < f64::EPSILON
}
pub(super) fn one() -> f64 {
    1.0
}
fn body() -> String {
    "body".into()
}
fn code_style() -> String {
    "code".into()
}
fn accent() -> String {
    "accent".into()
}

/// Round to two decimals; used by migration and layout output.
#[must_use]
pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// -- sizing and position -------------------------------------------------------

/// One axis of a node's size: a pixel length, hug content, or fill the parent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Dim {
    Fixed(f64),
    #[default]
    Hug,
    Fill,
}

impl Serialize for Dim {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fixed(n) => num(n, serializer),
            Self::Hug => serializer.serialize_str("hug"),
            Self::Fill => serializer.serialize_str("fill"),
        }
    }
}

impl<'de> Deserialize<'de> for Dim {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Number(n) => n
                .as_f64()
                .map(Self::Fixed)
                .ok_or_else(|| D::Error::custom("expected a pixel length")),
            Value::String(s) if s == "hug" => Ok(Self::Hug),
            Value::String(s) if s == "fill" => Ok(Self::Fill),
            _ => Err(D::Error::custom(
                "expected a pixel number, \"hug\" or \"fill\"",
            )),
        }
    }
}

/// Width, height and their limits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sizing {
    pub w: Dim,
    pub h: Dim,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "num_opt")]
    pub min_w: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "num_opt")]
    pub max_w: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "num_opt")]
    pub min_h: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "num_opt")]
    pub max_h: Option<f64>,
}

impl Sizing {
    #[must_use]
    pub fn fixed(w: f64, h: f64) -> Self {
        Self {
            w: Dim::Fixed(w),
            h: Dim::Fixed(h),
            ..Self::default()
        }
    }
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `auto` follows the parent's auto-layout; `absolute` ignores it (and every
/// child of a `mode: none` parent is absolute).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Position {
    #[default]
    Auto,
    Absolute {
        x: f64,
        y: f64,
    },
}

impl Serialize for Position {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Absolute { x, y } => {
                let mut map = Map::new();
                map.insert("x".into(), number_value(*x));
                map.insert("y".into(), number_value(*y));
                Value::Object(map).serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match &value {
            Value::String(s) if s == "auto" => Ok(Self::Auto),
            Value::Object(map) => {
                if let Some(key) = map.keys().find(|k| !matches!(k.as_str(), "x" | "y")) {
                    return Err(D::Error::custom(format!("unknown position field {key}")));
                }
                let get = |k: &str| {
                    map.get(k)
                        .and_then(Value::as_f64)
                        .ok_or_else(|| D::Error::custom(format!("position.{k} must be a number")))
                };
                Ok(Self::Absolute {
                    x: get("x")?,
                    y: get("y")?,
                })
            }
            _ => Err(D::Error::custom(
                "expected \"auto\" or {\"x\": px, \"y\": px}",
            )),
        }
    }
}

/// A JSON number that is an integer when it can be.
#[must_use]
pub fn number_value(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e12 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

// -- auto-layout ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    None,
    Row,
    Column,
}

/// Distribution along the layout direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Main {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

/// Alignment across the layout direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cross {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

fn padding_de<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[f64; 4], D::Error> {
    let numbers =
        |v: &Value| -> Option<Vec<f64>> { v.as_array()?.iter().map(Value::as_f64).collect() };
    let value = Value::deserialize(deserializer)?;
    if let Some(n) = value.as_f64() {
        return Ok([n; 4]);
    }
    match numbers(&value).as_deref() {
        Some([a]) => Ok([*a; 4]),
        Some([v, h]) => Ok([*v, *h, *v, *h]),
        Some([t, r, b, l]) => Ok([*t, *r, *b, *l]),
        _ => Err(D::Error::custom(
            "padding is a number or [top, right, bottom, left]",
        )),
    }
}

fn padding_ser<S: Serializer>(value: &[f64; 4], serializer: S) -> Result<S::Ok, S::Error> {
    Value::Array(value.iter().map(|v| number_value(*v)).collect()).serialize(serializer)
}

fn zero_padding(v: &[f64; 4]) -> bool {
    v.iter().all(|p| *p == 0.0)
}

/// Auto-layout of a slide or frame (Figma semantics).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoLayout {
    pub mode: Mode,
    #[serde(skip_serializing_if = "is_false")]
    pub wrap: bool,
    #[serde(skip_serializing_if = "is_zero", serialize_with = "num")]
    pub gap: f64,
    /// Gap between wrapped lines; defaults to `gap`.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "num_opt")]
    pub gap_cross: Option<f64>,
    #[serde(
        deserialize_with = "padding_de",
        serialize_with = "padding_ser",
        skip_serializing_if = "zero_padding"
    )]
    pub padding: [f64; 4],
    #[serde(skip_serializing_if = "is_start_main")]
    pub main: Main,
    #[serde(skip_serializing_if = "is_start_cross")]
    pub cross: Cross,
}

fn is_start_main(v: &Main) -> bool {
    *v == Main::Start
}
fn is_start_cross(v: &Cross) -> bool {
    *v == Cross::Start
}

impl AutoLayout {
    #[must_use]
    pub fn flow(mode: Mode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }
}

// -- typography ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

/// Numeric typography. In a text style every field normally has a value; on a
/// node only the overridden fields are present.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// An installed family, a deck font, or a theme role (`heading`, `body`, `mono`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Size in px.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "num_opt")]
    pub size: Option<f64>,
    /// 100 to 900.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// Multiplier of the size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f64>,
    /// In em.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f64>,
    /// A theme color role or a hex color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valign: Option<VAlign>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
}

impl Font {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// `self` with every field that `over` sets replaced.
    #[must_use]
    pub fn merged(&self, over: &Self) -> Self {
        Self {
            family: over.family.clone().or_else(|| self.family.clone()),
            size: over.size.or(self.size),
            weight: over.weight.or(self.weight),
            italic: over.italic.or(self.italic),
            line_height: over.line_height.or(self.line_height),
            letter_spacing: over.letter_spacing.or(self.letter_spacing),
            color: over.color.clone().or_else(|| self.color.clone()),
            align: over.align.or(self.align),
            valign: over.valign.or(self.valign),
            transform: over.transform.or(self.transform),
        }
    }
}

/// A font with every field decided.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFont {
    /// A theme role or family name; see [`Theme::family`].
    pub family: String,
    pub size: f64,
    pub weight: u16,
    pub italic: bool,
    pub line_height: f64,
    pub letter_spacing: f64,
    pub color: String,
    pub align: Align,
    pub valign: VAlign,
    pub transform: Transform,
}

// -- shapes, charts ------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    #[default]
    Contain,
    Cover,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Rect,
    Ellipse,
    Line,
    Arrow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Chart {
    #[default]
    Bar,
    Line,
    Pie,
}

/// A named numeric chart series, aligned with the chart labels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

/// A border. It is drawn inside the box and never takes layout space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    #[serde(default, skip_serializing_if = "Dash::is_solid")]
    pub dash: Dash,
    pub color: String,
    #[serde(serialize_with = "num")]
    pub width: f64,
}

// -- animation -----------------------------------------------------------------

/// Direction of a transition or build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Left,
    Right,
    Up,
    Down,
}

/// Playback timing curve; spring is a bounded overshoot curve in studio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    Linear,
    Ease,
    EaseIn,
    #[default]
    EaseOut,
    EaseInOut,
    Spring,
}

/// How a slide enters from its predecessor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionType {
    #[default]
    None,
    Fade,
    Push,
    Slide,
    Cover,
    Zoom,
    Smart,
}

fn duration() -> f64 {
    400.0
}

/// A slide transition, played on entry to the slide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Transition {
    #[serde(rename = "type")]
    pub kind: TransitionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    #[serde(serialize_with = "num")]
    pub duration_ms: f64,
    pub easing: Easing,
}

impl Default for Transition {
    fn default() -> Self {
        Self {
            kind: TransitionType::None,
            direction: None,
            duration_ms: duration(),
            easing: Easing::default(),
        }
    }
}

/// A node build's visual effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Appear,
    #[default]
    Fade,
    Rise,
    Fly,
    Scale,
    Wipe,
    Blur,
    Typewriter,
    Pulse,
    Shake,
}

/// Start on a click, alongside the previous build, or after it finishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    #[default]
    Click,
    WithPrevious,
    AfterPrevious,
}

/// Playback-only text segmentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildBy {
    #[default]
    Whole,
    Word,
    Line,
    Item,
}

/// The lifecycle slot occupied by a build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationSlot {
    BuildIn,
    Emphasis,
    BuildOut,
}

impl AnimationSlot {
    /// The slot's JSON key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::BuildIn => "build_in",
            Self::Emphasis => "emphasis",
            Self::BuildOut => "build_out",
        }
    }
}

/// One timed build. Durations are milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Build {
    pub effect: Effect,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    #[serde(serialize_with = "num")]
    pub duration_ms: f64,
    #[serde(serialize_with = "num")]
    pub delay_ms: f64,
    pub easing: Easing,
    pub trigger: Trigger,
    pub by: BuildBy,
    #[serde(serialize_with = "num_opt", skip_serializing_if = "Option::is_none")]
    pub stagger_ms: Option<f64>,
}

impl Default for Build {
    fn default() -> Self {
        Self {
            effect: Effect::default(),
            direction: None,
            duration_ms: duration(),
            delay_ms: 0.0,
            easing: Easing::default(),
            trigger: Trigger::default(),
            by: BuildBy::default(),
            stagger_ms: None,
        }
    }
}

/// A node's entry, emphasis and exit builds, in lifecycle order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Animation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_in: Option<Build>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<Build>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_out: Option<Build>,
}

impl Animation {
    /// No slot is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.build_in.is_none() && self.emphasis.is_none() && self.build_out.is_none()
    }

    /// Builds in lifecycle order, without empty slots.
    pub fn sequence(&self) -> impl Iterator<Item = (AnimationSlot, &Build)> {
        [
            (AnimationSlot::BuildIn, self.build_in.as_ref()),
            (AnimationSlot::Emphasis, self.emphasis.as_ref()),
            (AnimationSlot::BuildOut, self.build_out.as_ref()),
        ]
        .into_iter()
        .filter_map(|(slot, build)| build.map(|b| (slot, b)))
    }
}

// -- nodes ---------------------------------------------------------------------

/// Kind-specific payload, flattened into each node's JSON object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Kind {
    Frame {
        #[serde(default)]
        layout: AutoLayout,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Fill>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
        #[serde(default, skip_serializing_if = "is_zero", serialize_with = "num")]
        radius: f64,
        #[serde(default, skip_serializing_if = "is_false")]
        clip: bool,
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        /// A Markdown subset: headings, bullets, numbered lists, bold,
        /// italic, inline code, links.
        text: String,
        #[serde(default = "body")]
        style: String,
        #[serde(default, skip_serializing_if = "Font::is_empty")]
        font: Font,
    },
    Image {
        src: String,
        #[serde(default)]
        fit: Fit,
    },
    Shape {
        #[serde(default)]
        shape: Shape,
        #[serde(default = "accent_opt", skip_serializing_if = "Option::is_none")]
        fill: Option<Fill>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<Stroke>,
        #[serde(default, skip_serializing_if = "is_zero", serialize_with = "num")]
        radius: f64,
    },
    Table {
        rows: Vec<Vec<String>>,
        #[serde(default = "body")]
        style: String,
        #[serde(default, skip_serializing_if = "Font::is_empty")]
        font: Font,
    },
    Code {
        #[serde(default)]
        language: String,
        source: String,
        #[serde(default = "code_style")]
        style: String,
        #[serde(default, skip_serializing_if = "Font::is_empty")]
        font: Font,
    },
    Chart {
        chart: Chart,
        labels: Vec<String>,
        series: Vec<Series>,
    },
}

fn accent_opt() -> Option<Fill> {
    Some(accent().into())
}

impl Kind {
    /// The lowercase kind name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Frame { .. } => "frame",
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::Shape { .. } => "shape",
            Self::Table { .. } => "table",
            Self::Code { .. } => "code",
            Self::Chart { .. } => "chart",
        }
    }
}

/// Original template placeholder identity, retained after slot instantiation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placeholder {
    pub kind: String,
    pub idx: String,
}

/// One element of a slide's tree.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Node {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Named placeholder in a design layout; removed when instantiated.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub slot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_placeholder: Option<Placeholder>,
    /// Inherited artwork: rendered locally, not duplicated into template slides.
    #[serde(default, skip_serializing_if = "is_false")]
    pub template_background: bool,
    #[serde(default, skip_serializing_if = "Sizing::is_default")]
    pub sizing: Sizing,
    #[serde(default, skip_serializing_if = "is_auto")]
    pub position: Position,
    #[serde(
        default = "one",
        skip_serializing_if = "is_one",
        serialize_with = "num"
    )]
    pub opacity: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    /// Hidden nodes take no layout space and are not drawn.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Entry, emphasis and exit builds; played in present mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<Animation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<Focus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Mask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
    /// Image radius; frame and shape radii remain in their kind payload.
    #[serde(
        rename = "radius",
        skip_serializing_if = "Option::is_none",
        serialize_with = "num_opt"
    )]
    pub image_radius: Option<f64>,
    #[serde(flatten)]
    pub kind: Kind,
}

fn is_auto(p: &Position) -> bool {
    *p == Position::Auto
}

impl Node {
    /// A node with default sizing and position.
    #[must_use]
    pub fn new(id: &str, kind: Kind) -> Self {
        Self {
            id: id.into(),
            name: None,
            slot: String::new(),
            source_placeholder: None,
            template_background: false,
            sizing: Sizing::default(),
            position: Position::Auto,
            opacity: 1.0,
            rotation: None,
            hidden: false,
            locked: false,
            animation: None,
            shadow: None,
            focus: None,
            mask: None,
            overlay: None,
            image_radius: None,
            kind,
        }
    }

    /// SHA-256 of canonical node JSON (children included); an update precondition.
    #[must_use]
    pub fn hash(&self) -> String {
        super::hash(self)
    }

    /// Children of a frame, empty for other kinds.
    #[must_use]
    pub fn children(&self) -> &[Node] {
        match &self.kind {
            Kind::Frame { children, .. } => children,
            _ => &[],
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<Node>> {
        match &mut self.kind {
            Kind::Frame { children, .. } => Some(children),
            _ => None,
        }
    }

    /// This node and every descendant, depth first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for child in self.children() {
            child.walk(out);
        }
    }

    /// The text a node carries, for labels.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            Kind::Text { text, .. } => Some(text),
            Kind::Code { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A slide: a frame of the theme's size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slide {
    #[serde(default)]
    pub id: String,
    /// Original OPC layout part, used when exporting into a template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_layout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Background: a theme role or hex color; the theme's `bg` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Fill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,
    #[serde(default)]
    pub layout: AutoLayout,
    #[serde(default)]
    pub children: Vec<Node>,
    #[serde(default)]
    pub notes: String,
    /// How this slide enters from the previous one; played in present mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    /// Explicit node build order; repaired and completed in tree order on load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builds: Vec<String>,
}

impl Slide {
    /// The slide's builds in playback order: each node's slots in lifecycle
    /// order, nodes in `builds` order (which validation keeps complete).
    #[must_use]
    pub fn build_sequence(&self) -> Vec<(&Node, AnimationSlot, &Build)> {
        self.builds
            .iter()
            .filter_map(|id| self.node(id))
            .flat_map(|node| {
                node.animation
                    .iter()
                    .flat_map(Animation::sequence)
                    .map(move |(slot, build)| (node, slot, build))
            })
            .collect()
    }

    #[must_use]
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            source_layout: None,
            name: None,
            fill: None,
            background: None,
            layout: AutoLayout::default(),
            children: Vec::new(),
            notes: String::new(),
            transition: None,
            builds: Vec::new(),
        }
    }

    /// Every node on the slide, depth first.
    #[must_use]
    pub fn nodes(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        for child in &self.children {
            child.walk(&mut out);
        }
        out
    }

    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes().into_iter().find(|n| n.id == id)
    }
}

// -- theme ---------------------------------------------------------------------

/// Slide size in px.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlideSize {
    #[serde(serialize_with = "num")]
    pub w: f64,
    #[serde(serialize_with = "num")]
    pub h: f64,
}

impl Default for SlideSize {
    fn default() -> Self {
        Self {
            w: SLIDE_WIDTH,
            h: SLIDE_HEIGHT,
        }
    }
}

/// A font file imported into the deck, under `assets/fonts/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontFile {
    /// The family name nodes use for this file.
    pub family: String,
    /// `assets/fonts/name.ttf` (also `.otf`, `.ttc`, `.woff2`).
    pub file: String,
    #[serde(default = "regular")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
    /// Families tried after this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,
}

fn regular() -> u16 {
    400
}

/// Theme tokens, shared by all slides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub colors: BTreeMap<String, String>,
    /// Font roles (`heading`, `body`, `mono`) to family names.
    pub fonts: BTreeMap<String, String>,
    pub size: SlideSize,
    pub text_styles: BTreeMap<String, Font>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub font_files: Vec<FontFile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shadows: BTreeMap<String, ShadowSpec>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: [
                ("bg", "#faf9f6"),
                ("fg", "#282923"),
                ("accent", "#ae593e"),
                ("muted", "#76766b"),
                ("accent2", "#284c49"),
                ("accent3", "#d4a25c"),
                ("surface", "#f1ebe3"),
                ("border", "#ded8cf"),
            ]
            .map(|(k, v)| (k.into(), v.into()))
            .into(),
            fonts: [
                ("heading", "Inter"),
                ("body", "Inter"),
                ("mono", "JetBrains Mono"),
            ]
            .map(|(k, v)| (k.into(), v.into()))
            .into(),
            size: SlideSize::default(),
            text_styles: default_text_styles(),
            font_files: Vec::new(),
            shadows: super::effects::presets().clone(),
        }
    }
}

fn style(family: &str, size: f64, weight: u16, line_height: f64, color: Option<&str>) -> Font {
    Font {
        family: Some(family.into()),
        size: Some(size),
        weight: Some(weight),
        italic: Some(false),
        line_height: Some(line_height),
        letter_spacing: Some(0.0),
        color: Some(color.unwrap_or("fg").into()),
        align: Some(Align::Left),
        valign: Some(VAlign::Top),
        transform: Some(Transform::None),
    }
}

/// The named styles every deck starts with.
#[must_use]
pub fn default_text_styles() -> BTreeMap<String, Font> {
    [
        (
            "display",
            style("heading", 312.93, 800, 1.0, Some("accent")),
        ),
        ("quote", style("heading", 81.0, 500, 1.15, None)),
        ("lead", style("body", 58.0, 400, 1.4, None)),
        ("title", style("heading", 120.0, 700, 1.08, None)),
        ("heading", style("heading", 81.0, 700, 1.08, None)),
        ("subheading", style("heading", 58.0, 600, 1.2, None)),
        ("body", style("body", 42.0, 400, 1.35, None)),
        ("caption", style("body", 26.0, 400, 1.35, Some("muted"))),
        ("code", style("mono", 36.0, 400, 1.35, None)),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .into()
}

impl Theme {
    /// The concrete family name for a role or family string.
    #[must_use]
    pub fn family<'a>(&'a self, family: &'a str) -> &'a str {
        self.fonts.get(family).map_or(family, String::as_str)
    }

    /// The named style (or `body`) with `over` applied, over hard defaults.
    #[must_use]
    pub fn resolve_font(&self, style: &str, over: &Font) -> ResolvedFont {
        let base = self
            .text_styles
            .get(style)
            .or_else(|| self.text_styles.get("body"))
            .cloned()
            .unwrap_or_default();
        let font = base.merged(over);
        ResolvedFont {
            family: font.family.unwrap_or_else(|| "body".into()),
            size: font.size.unwrap_or(42.0),
            weight: font.weight.unwrap_or(400),
            italic: font.italic.unwrap_or(false),
            line_height: font.line_height.unwrap_or(1.35),
            letter_spacing: font.letter_spacing.unwrap_or(0.0),
            color: font.color.unwrap_or_else(|| "fg".into()),
            align: font.align.unwrap_or_default(),
            valign: font.valign.unwrap_or_default(),
            transform: font.transform.unwrap_or_default(),
        }
    }
}

/// A complete presentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    pub version: u8,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_rev: Option<String>,
    #[serde(default)]
    pub theme: Theme,
    pub slides: Vec<Slide>,
}
