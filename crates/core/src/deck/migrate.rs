//! Version 1 to version 2, on the JSON value, before validation.
//!
//! Rules (idempotent: version 2 input is left alone):
//! - grid units become px (x60), on a 1920 x 1080 slide;
//! - every block becomes an absolute, fixed-size child of a `mode: none`
//!   slide, in the same paint order, with its id kept;
//! - text size steps map to px through the old size table (rounded to whole
//!   px: 26, 42, 58, 81, 119) and `xl`/`xxl` keep their tighter line height;
//!   only values that differ from the default text style are written;
//! - `style.align` and `style.color` become `font.align` and `font.color`;
//! - code blocks use the `code` text style; text and tables the `body` style;
//! - shape `stroke` becomes `{color, width: 2.4}` (the old 2 px at 1600 wide);
//! - the slide's template name (`title`, `content`, ...) is dropped;
//! - v1's out-of-grid repair (clamping a block into 32 x 18) still applies.

use serde_json::{Map, Value, json};

use super::model::{GRID, number_value, round2};
use super::{HEIGHT, WIDTH};

/// Old size table, as px on the new 1920 wide slide.
const SIZES: [(&str, f64, Option<f64>); 5] = [
    ("s", 26.0, None),
    ("m", 42.0, None),
    ("l", 58.0, None),
    ("xl", 81.0, Some(1.08)),
    ("xxl", 119.0, Some(1.08)),
];

/// Width of the 2 px v1 shape border on the new slide.
const SHAPE_BORDER: f64 = 2.4;

/// Convert a version-1 deck value in place.
pub fn v1_to_v2(deck: &mut Value) {
    let Some(root) = deck.as_object_mut() else {
        return;
    };
    root.insert("version".into(), json!(2));
    let Some(slides) = root.get_mut("slides").and_then(Value::as_array_mut) else {
        return;
    };
    for slide in slides {
        if let Some(object) = slide.as_object_mut() {
            migrate_slide(object);
        }
    }
}

fn migrate_slide(slide: &mut Map<String, Value>) {
    slide.remove("layout");
    slide.insert("layout".into(), json!({"mode": "none"}));
    if let Some(blocks) = slide.remove("blocks") {
        let children = match blocks {
            Value::Array(blocks) => Value::Array(blocks.into_iter().map(migrate_block).collect()),
            other => other,
        };
        slide.insert("children".into(), children);
    }
}

/// v1 repair: position and size are clamped into the grid.
fn clamp_to_grid(block: &mut Map<String, Value>) {
    for (position, size, bound) in [("x", "w", WIDTH), ("y", "h", HEIGHT)] {
        if let (Some(p), Some(s)) = (
            block.get(position).and_then(Value::as_i64),
            block.get(size).and_then(Value::as_i64),
        ) {
            let p = p.clamp(0, i64::from(bound - 1));
            block.insert(position.into(), json!(p));
            block.insert(size.into(), json!(s.clamp(1, i64::from(bound) - p)));
        }
    }
}

fn px(value: Option<Value>) -> Option<Value> {
    value.map(|v| v.as_f64().map_or(v, |n| number_value(round2(n * GRID))))
}

fn migrate_block(block: Value) -> Value {
    let Value::Object(mut block) = block else {
        return block;
    };
    clamp_to_grid(&mut block);
    let mut node = Map::new();
    for key in ["id", "kind"] {
        if let Some(v) = block.remove(key) {
            node.insert(key.into(), v);
        }
    }
    let (x, y) = (px(block.remove("x")), px(block.remove("y")));
    let (w, h) = (px(block.remove("w")), px(block.remove("h")));
    if let (Some(w), Some(h)) = (&w, &h) {
        node.insert("sizing".into(), json!({"w": w, "h": h}));
    } else if w.is_some() || h.is_some() {
        node.insert(
            "sizing".into(),
            json!({"w": w.unwrap_or(Value::Null), "h": h.unwrap_or(Value::Null)}),
        );
    }
    if let (Some(x), Some(y)) = (x, y) {
        node.insert("position".into(), json!({"x": x, "y": y}));
    }
    let style = block.remove("style");
    let kind = node
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = kind.as_str();
    match kind {
        "text" | "table" | "code" => {
            node.insert(
                "style".into(),
                json!(if kind == "code" { "code" } else { "body" }),
            );
            let font = font_overrides(style.as_ref(), kind == "code");
            if !font.is_empty() {
                node.insert("font".into(), Value::Object(font));
            }
        }
        _ => {}
    }
    if kind == "shape"
        && let Some(stroke) = block.remove("stroke")
    {
        if stroke.is_string() {
            node.insert(
                "stroke".into(),
                json!({"color": stroke, "width": SHAPE_BORDER}),
            );
        } else {
            node.insert("stroke".into(), stroke);
        }
    }
    // Everything else keeps its name, so unknown fields still fail validation
    // where the author wrote them.
    for (key, value) in block {
        node.insert(key, value);
    }
    Value::Object(node)
}

/// Font fields that differ from the default text style.
fn font_overrides(style: Option<&Value>, code: bool) -> Map<String, Value> {
    let mut font = Map::new();
    let get = |key: &str| style.and_then(|s| s.get(key));
    let size = get("size").and_then(Value::as_str).unwrap_or("m");
    let default_size = if code { 36.0 } else { 42.0 };
    if let Some((_, px, line_height)) = SIZES.iter().find(|(name, ..)| *name == size) {
        if (*px - default_size).abs() > f64::EPSILON {
            font.insert("size".into(), number_value(*px));
        }
        if let Some(line_height) = line_height {
            font.insert("line_height".into(), json!(line_height));
        }
    } else if get("size").is_some() {
        // Unknown size token: keep it so validation reports it.
        font.insert("size".into(), get("size").cloned().unwrap_or(Value::Null));
    }
    if let Some(align) = get("align").filter(|a| a.as_str() != Some("left")) {
        font.insert("align".into(), align.clone());
    }
    if let Some(color) = get("color").filter(|c| !c.is_null()) {
        font.insert("color".into(), color.clone());
    }
    font
}
