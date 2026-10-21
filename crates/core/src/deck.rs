//! Deck v1: bounded, UI-independent slide data and atomic editing operations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Maximum UTF-8 source size, in bytes.
pub const MAX_SOURCE: usize = 2 * 1024 * 1024;
/// Canvas width in grid units.
pub const WIDTH: i32 = 32;
/// Canvas height in grid units.
pub const HEIGHT: i32 = 18;

/// A located validation failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckError {
    pub path: String,
    pub message: String,
}

/// A complete version-one presentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    pub version: u8,
    pub title: String,
    #[serde(default)]
    pub theme: Theme,
    pub slides: Vec<Slide>,
}

/// Theme tokens, shared by all slides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub colors: BTreeMap<String, String>,
    pub fonts: BTreeMap<String, String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: [
                ("bg", "#0f1115"),
                ("fg", "#e8e6e3"),
                ("accent", "#7aa2f7"),
                ("muted", "#8b8f98"),
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
        }
    }
}

/// A starting arrangement; subsequent edits own their geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    Title,
    Section,
    #[default]
    Content,
    TwoColumn,
    ImageText,
    Quote,
    Blank,
}

/// A single slide with speaker notes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slide {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub notes: String,
}

/// Text size tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    S,
    #[default]
    M,
    L,
    Xl,
    Xxl,
}

/// Horizontal text alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Block styling. Colors are theme roles or hex literals, never CSS.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub align: Align,
    pub size: TextSize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Image fitting mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    #[default]
    Contain,
    Cover,
}

/// Primitive shape geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Rect,
    Ellipse,
    Line,
    Arrow,
}

/// Chart geometry.
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

/// Kind-specific payload, flattened into each block's JSON object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        src: String,
        #[serde(default)]
        fit: Fit,
    },
    Shape {
        #[serde(default)]
        shape: Shape,
        #[serde(default = "accent")]
        fill: String,
        #[serde(default = "accent")]
        stroke: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    Code {
        #[serde(default)]
        language: String,
        source: String,
    },
    Chart {
        chart: Chart,
        labels: Vec<String>,
        series: Vec<Series>,
    },
}

fn accent() -> String {
    "accent".into()
}

/// An absolutely positioned block on the 32 by 18 canvas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    #[serde(default)]
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(default)]
    pub style: Style,
    #[serde(flatten)]
    pub content: Content,
}

impl Block {
    /// SHA-256 of canonical block JSON, used as an update precondition.
    #[must_use]
    pub fn hash(&self) -> String {
        hash(self)
    }
}

/// The complete editing language. Slide indices in `move_slide` are zero-based.
/// A missing `after` appends. Restyle targets a block id, slide id, or `deck`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    AddSlide {
        after: Option<String>,
        slide: Slide,
    },
    DeleteSlide {
        id: String,
    },
    MoveSlide {
        id: String,
        to: usize,
    },
    SetLayout {
        slide: String,
        layout: Layout,
    },
    SetNotes {
        slide: String,
        notes: String,
    },
    InsertBlock {
        slide: String,
        block: Block,
    },
    UpdateBlock {
        slide: String,
        id: String,
        fields: Map<String, Value>,
        base_hash: String,
    },
    DeleteBlock {
        slide: String,
        id: String,
    },
    Restyle {
        target: String,
        style: Style,
    },
    SetTheme {
        theme: Theme,
    },
}

impl Deck {
    /// Parse and normalize a deck, preserving intent where unambiguous.
    /// Missing/duplicate ids and out-of-grid rectangles are repaired; other
    /// failures carry JSON paths. Limits apply before deserializing the model.
    pub fn check(source: &str) -> std::result::Result<Self, Vec<CheckError>> {
        let mut errors = Vec::new();
        if source.len() > MAX_SOURCE {
            issue(&mut errors, "$", "source exceeds 2 MB");
            return Err(errors);
        }
        let mut value: Value = serde_json::from_str(source).map_err(|e| {
            vec![CheckError {
                path: "$".into(),
                message: e.to_string(),
            }]
        })?;
        if !value.is_object() {
            issue(&mut errors, "$", "expected deck object");
            return Err(errors);
        }
        if value.get("version") != Some(&json!(1)) {
            issue(&mut errors, "$.version", "expected deck version 1");
        }
        let theme = normalize_theme(&mut value);
        let reserved = reserved_ids(&value);
        let mut used = BTreeSet::new();
        if let Some(slides) = value["slides"].as_array_mut() {
            if slides.len() > 200 {
                issue(&mut errors, "$.slides", "at most 200 slides");
            }
            for (s, slide) in slides.iter_mut().enumerate() {
                let path = format!("$.slides[{s}]");
                fill_id(slide, &format!("s{}", s + 1), &reserved, &mut used);
                if let Some(layout) = slide.get("layout")
                    && serde_json::from_value::<Layout>(layout.clone()).is_err()
                {
                    issue(&mut errors, &format!("{path}.layout"), "unknown layout");
                }
                if let Some(blocks) = slide.get_mut("blocks").and_then(Value::as_array_mut) {
                    if blocks.len() > 40 {
                        issue(
                            &mut errors,
                            &format!("{path}.blocks"),
                            "at most 40 blocks per slide",
                        );
                    }
                    for (b, block) in blocks.iter_mut().enumerate() {
                        let path = format!("{path}.blocks[{b}]");
                        fill_id(
                            block,
                            &format!("b{}_{}", s + 1, b + 1),
                            &reserved,
                            &mut used,
                        );
                        normalize_rect(block);
                        match serde_json::from_value::<Block>(block.clone()) {
                            Ok(block) => validate_block(&block, &theme, &path, &mut errors),
                            Err(e) => {
                                let field = if matches!(
                                    block["kind"].as_str(),
                                    Some("text" | "image" | "shape" | "table" | "code" | "chart")
                                ) {
                                    ""
                                } else {
                                    ".kind"
                                };
                                issue(&mut errors, &format!("{path}{field}"), &e.to_string());
                            }
                        }
                    }
                } else if slide.get("blocks").is_some() {
                    issue(&mut errors, &format!("{path}.blocks"), "expected array");
                }
            }
        } else {
            issue(&mut errors, "$.slides", "expected array");
        }
        let deck = match serde_json::from_value::<Self>(value) {
            Ok(deck) => Some(deck),
            Err(e) => {
                issue(&mut errors, "$", &e.to_string());
                None
            }
        };
        if let Some(deck) = &deck {
            for (role, color) in &deck.theme.colors {
                if !is_hex(color) {
                    issue(
                        &mut errors,
                        &format!("$.theme.colors.{role}"),
                        "expected hex color",
                    );
                }
            }
        }
        if errors.is_empty() {
            Ok(deck.expect("checked deck"))
        } else {
            Err(errors)
        }
    }

    /// SHA-256 of canonical JSON, independent of input object key order.
    #[must_use]
    pub fn revision(&self) -> String {
        hash(self)
    }

    /// A ready-to-edit title and content slide with the default dark theme.
    #[must_use]
    pub fn new_from_layouts(title: &str) -> Self {
        Self {
            version: 1,
            title: title.into(),
            theme: Theme::default(),
            slides: vec![
                Slide {
                    id: "s1".into(),
                    layout: Layout::Title,
                    notes: String::new(),
                    blocks: vec![text_block("b1", title, (2, 6, 28, 5), TextSize::Xxl)],
                },
                Slide {
                    id: "s2".into(),
                    layout: Layout::Content,
                    notes: String::new(),
                    blocks: vec![
                        text_block("b2", "Your next big idea", (2, 2, 28, 3), TextSize::Xl),
                        text_block(
                            "b3",
                            "- Start with the story\n- Make it memorable\n- Leave room to breathe",
                            (2, 6, 28, 10),
                            TextSize::L,
                        ),
                    ],
                },
            ],
        }
    }
}

fn normalize_theme(value: &mut Value) -> Theme {
    let defaults = serde_json::to_value(Theme::default()).expect("theme JSON");
    if value.get("theme").is_none() {
        value["theme"] = defaults.clone();
    } else if value["theme"].is_object() {
        for group in ["colors", "fonts"] {
            if value["theme"].get(group).is_none() {
                value["theme"][group] = defaults[group].clone();
            } else if let Some(map) = value["theme"][group].as_object_mut() {
                for (key, val) in defaults[group].as_object().expect("token map") {
                    map.entry(key).or_insert_with(|| val.clone());
                }
            }
        }
    }
    serde_json::from_value(value["theme"].clone()).unwrap_or_default()
}

fn text_block(id: &str, text: &str, rect: (i32, i32, i32, i32), size: TextSize) -> Block {
    Block {
        id: id.into(),
        x: rect.0,
        y: rect.1,
        w: rect.2,
        h: rect.3,
        style: Style {
            size,
            ..Style::default()
        },
        content: Content::Text { text: text.into() },
    }
}

fn issue(errors: &mut Vec<CheckError>, path: &str, message: &str) {
    errors.push(CheckError {
        path: path.into(),
        message: message.into(),
    });
}

fn reserved_ids(value: &Value) -> BTreeSet<String> {
    let mut reserved = BTreeSet::new();
    if let Some(slides) = value["slides"].as_array() {
        for slide in slides {
            reserve_id(slide, &mut reserved);
            if let Some(blocks) = slide["blocks"].as_array() {
                for block in blocks {
                    reserve_id(block, &mut reserved);
                }
            }
        }
    }
    reserved
}

fn reserve_id(value: &Value, reserved: &mut BTreeSet<String>) {
    if let Some(id) = value["id"].as_str().filter(|id| !id.is_empty()) {
        reserved.insert(id.into());
    }
}

fn fill_id(
    value: &mut Value,
    stem: &str,
    reserved: &BTreeSet<String>,
    used: &mut BTreeSet<String>,
) {
    if !value.is_object() {
        return;
    }
    if let Some(id) = value["id"].as_str().filter(|id| !id.is_empty())
        && used.insert(id.into())
    {
        return;
    }
    // Non-string ids are real type errors, not missing ids.
    if value.get("id").is_some_and(|id| !id.is_string()) {
        return;
    }
    let mut id = stem.to_string();
    let mut suffix = 1;
    while reserved.contains(&id) || used.contains(&id) {
        id = format!("{stem}_{suffix}");
        suffix += 1;
    }
    used.insert(id.clone());
    value["id"] = json!(id);
}

fn normalize_rect(block: &mut Value) {
    for (position, size, bound) in [("x", "w", WIDTH), ("y", "h", HEIGHT)] {
        if let (Some(p), Some(s)) = (block[position].as_i64(), block[size].as_i64()) {
            let p = p.clamp(0, i64::from(bound - 1));
            block[position] = json!(p);
            block[size] = json!(s.clamp(1, i64::from(bound) - p));
        }
    }
}

fn is_hex(color: &str) -> bool {
    color.strip_prefix('#').is_some_and(|digits| {
        matches!(digits.len(), 3 | 4 | 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn validate_block(block: &Block, theme: &Theme, path: &str, errors: &mut Vec<CheckError>) {
    let valid_color = |color: &str| is_hex(color) || theme.colors.contains_key(color);
    if block
        .style
        .color
        .as_deref()
        .is_some_and(|c| !valid_color(c))
    {
        issue(
            errors,
            &format!("{path}.style.color"),
            "expected theme role or hex color",
        );
    }
    let text_bytes = match &block.content {
        Content::Text { text } => text.len(),
        Content::Code { language, source } => language.len() + source.len(),
        Content::Table { rows } => rows.iter().flatten().map(String::len).sum(),
        Content::Chart { labels, series, .. } => {
            if series
                .iter()
                .any(|s| s.values.len() != labels.len() || s.values.iter().any(|v| !v.is_finite()))
            {
                issue(
                    errors,
                    &format!("{path}.series"),
                    "series must have one finite value per label",
                );
            }
            labels.iter().map(String::len).sum::<usize>()
                + series.iter().map(|s| s.name.len()).sum::<usize>()
        }
        Content::Image { src, .. } => {
            if src.starts_with("https://") {
                if let Err(e) = crate::remote_image::validate_url(src) {
                    issue(errors, &format!("{path}.src"), &e.to_string());
                }
            } else if !safe_asset_path(src) {
                issue(
                    errors,
                    &format!("{path}.src"),
                    "expected HTTPS or a relative assets path without '..'",
                );
            }
            0
        }
        Content::Shape { fill, stroke, .. } => {
            if !valid_color(fill) || !valid_color(stroke) {
                issue(errors, path, "shape colors must be theme roles or hex");
            }
            0
        }
    };
    if text_bytes > 8 * 1024 {
        issue(errors, path, "block text exceeds 8 KB");
    }
}

/// A relative image path within the deck assets directory. Backslashes and
/// URL escapes are rejected too, so interpretation cannot vary by platform.
#[must_use]
pub fn safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', ':', '%', '\0'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn hash(value: &impl Serialize) -> String {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(k, v)| (k, sorted(v)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            scalar => scalar,
        }
    }
    let value = sorted(serde_json::to_value(value).expect("deck JSON"));
    let bytes = serde_json::to_vec(&value).expect("canonical JSON");
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

/// Apply an entire transaction on a copy. Any invalid or stale operation
/// discards the copy; the original deck is never changed.
pub fn apply(deck: &Deck, ops: &[Op]) -> Result<Deck> {
    let mut next = deck.clone();
    for op in ops {
        match op {
            Op::AddSlide { after, slide } => {
                let index = after.as_ref().map_or(Ok(next.slides.len()), |id| {
                    slide_index(&next, id).map(|i| i + 1)
                })?;
                next.slides.insert(index, slide.clone());
            }
            Op::DeleteSlide { id } => {
                let index = slide_index(&next, id)?;
                next.slides.remove(index);
            }
            Op::MoveSlide { id, to } => {
                ensure!(*to < next.slides.len(), "slide destination out of range");
                let index = slide_index(&next, id)?;
                let slide = next.slides.remove(index);
                next.slides.insert(*to, slide);
            }
            Op::SetLayout { slide, layout } => slide_mut(&mut next, slide)?.layout = *layout,
            Op::SetNotes { slide, notes } => slide_mut(&mut next, slide)?.notes.clone_from(notes),
            Op::InsertBlock { slide, block } => {
                slide_mut(&mut next, slide)?.blocks.push(block.clone());
            }
            Op::UpdateBlock {
                slide,
                id,
                fields,
                base_hash,
            } => {
                let block = slide_mut(&mut next, slide)?
                    .blocks
                    .iter_mut()
                    .find(|b| b.id == *id)
                    .ok_or_else(|| anyhow!("unknown block {id}"))?;
                ensure!(
                    block.hash() == *base_hash,
                    "stale block {id}: base_hash mismatch"
                );
                ensure!(!fields.contains_key("id"), "block ids are immutable");
                let mut value = serde_json::to_value(&*block)?;
                for (key, val) in fields {
                    value[key] = val.clone();
                }
                *block = serde_json::from_value(value)?;
            }
            Op::DeleteBlock { slide, id } => {
                let blocks = &mut slide_mut(&mut next, slide)?.blocks;
                let index = blocks
                    .iter()
                    .position(|b| b.id == *id)
                    .ok_or_else(|| anyhow!("unknown block {id}"))?;
                blocks.remove(index);
            }
            Op::Restyle { target, style } => {
                let mut found = false;
                for slide in &mut next.slides {
                    for block in &mut slide.blocks {
                        if target == "deck" || *target == slide.id || *target == block.id {
                            block.style.clone_from(style);
                            found = true;
                        }
                    }
                    found |= *target == slide.id;
                }
                ensure!(found || target == "deck", "unknown style target {target}");
            }
            Op::SetTheme { theme } => next.theme.clone_from(theme),
        }
    }
    match Deck::check(&serde_json::to_string(&next)?) {
        Ok(deck) => Ok(deck),
        Err(errors) => bail!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

fn slide_index(deck: &Deck, id: &str) -> Result<usize> {
    deck.slides
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| anyhow!("unknown slide {id}"))
}

fn slide_mut<'a>(deck: &'a mut Deck, id: &str) -> Result<&'a mut Slide> {
    let index = slide_index(deck, id)?;
    Ok(&mut deck.slides[index])
}

#[cfg(test)]
mod tests;
