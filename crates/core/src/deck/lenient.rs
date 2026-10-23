//! Lenient input for the `deck` tool: the small, predictable mistakes a model
//! makes (a font given as a string, `kind: "ellipse"`, a text style it never
//! defined, `"58"` for 58, letter spacing in px) are repaired on the JSON
//! value before validation, and each repair is reported so the model learns
//! the real shape. Nothing here guesses at meaning: anything not covered still
//! fails validation, with a hint from [`hint`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::model::number_value;
use super::{Deck, Kind};

/// Fields that hold a number; a numeric string there is converted.
const NUMBER_KEYS: &[&str] = &[
    "size",
    "weight",
    "line_height",
    "letter_spacing",
    "gap",
    "gap_cross",
    "radius",
    "opacity",
    "rotation",
    "x",
    "y",
    "w",
    "h",
    "min_w",
    "max_w",
    "min_h",
    "max_h",
    "width",
    "duration_ms",
    "delay_ms",
    "stagger_ms",
    "to",
    "index",
];
/// Fields that must be whole numbers.
const INTEGER_KEYS: &[&str] = &["weight", "to", "index"];
const MAX_NOTES: usize = 12;

/// What the normalizer needs to know about the deck the input is meant for.
pub struct Context {
    /// Known text style names with their sizes in px.
    styles: BTreeMap<String, f64>,
    /// Resolved font size of each text-bearing node, for letter-spacing fixes.
    node_sizes: BTreeMap<String, f64>,
    default_size: f64,
    notes: Vec<String>,
}

impl Context {
    /// For input aimed at a new deck: the default theme's styles.
    #[must_use]
    pub fn for_new_deck() -> Self {
        Self::for_deck(&Deck::titled("x"))
    }

    #[must_use]
    pub fn for_deck(deck: &Deck) -> Self {
        let styles = deck
            .theme
            .text_styles
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    deck.theme.resolve_font(name, &super::Font::default()).size,
                )
            })
            .collect();
        let mut node_sizes = BTreeMap::new();
        for slide in &deck.slides {
            for node in slide.nodes() {
                let font = match &node.kind {
                    Kind::Text { style, font, .. }
                    | Kind::Table { style, font, .. }
                    | Kind::Code { style, font, .. } => deck.theme.resolve_font(style, font),
                    _ => continue,
                };
                node_sizes.insert(node.id.clone(), font.size);
            }
        }
        Self {
            styles,
            node_sizes,
            default_size: deck
                .theme
                .resolve_font("body", &super::Font::default())
                .size,
            notes: Vec::new(),
        }
    }

    /// The `normalized: …` lines collected so far.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut unique: Vec<String> = self
            .notes
            .iter()
            .filter(|note| seen.insert((*note).clone()))
            .cloned()
            .collect();
        if unique.len() > MAX_NOTES {
            let extra = unique.len() - MAX_NOTES;
            unique.truncate(MAX_NOTES);
            unique.push(format!("{extra} more repairs of the same kinds"));
        }
        unique
    }

    fn note(&mut self, text: String) {
        self.notes.push(text);
    }

    /// Declare text styles defined by the same call, so they are not remapped.
    pub fn define_style(&mut self, name: &str, size: Option<f64>) {
        let size = size.unwrap_or(self.default_size);
        self.styles.entry(name.to_string()).or_insert(size);
    }

    /// Normalize a list of slide objects (the `slides` of a create).
    pub fn slides(&mut self, slides: &mut Value) {
        if let Some(items) = slides.as_array_mut() {
            for slide in items {
                self.node(slide, None);
            }
        }
    }

    /// Normalize a `text_styles` object given beside the slides or ops.
    pub fn text_styles(&mut self, styles: &mut Value) {
        let Some(map) = styles.as_object_mut() else {
            return;
        };
        for (name, font) in map.iter_mut() {
            self.font(font, None, Some(name));
            let size = font.get("size").and_then(Value::as_f64);
            self.define_style(name, size);
        }
    }

    /// Normalize every operation of an edit or comment.
    pub fn ops(&mut self, ops: &mut [Value]) {
        // Styles defined anywhere in the call are known to every operation.
        for op in ops.iter() {
            match op["op"].as_str() {
                Some("set_text_style") => {
                    if let Some(name) = op["name"].as_str() {
                        let size = op["font"]["size"].as_f64();
                        self.define_style(name, size);
                    }
                }
                Some("set_theme") => {
                    if let Some(styles) = op["theme"]["text_styles"].as_object() {
                        for (name, font) in styles {
                            self.define_style(name, font["size"].as_f64());
                        }
                    }
                }
                _ => {}
            }
        }
        for op in ops.iter_mut() {
            self.op(op);
        }
    }

    fn op(&mut self, op: &mut Value) {
        let Some(kind) = op["op"].as_str().map(str::to_string) else {
            return;
        };
        for key in ["to", "index"] {
            self.number(op, key);
        }
        match kind.as_str() {
            "add_slide" => self.node(&mut op["slide"], None),
            "insert_node" => self.node(&mut op["node"], None),
            "update_node" | "update_slide" => {
                let id = op["id"].as_str().map(str::to_string);
                self.node(&mut op["fields"], id.as_deref());
            }
            "set_text_style" => {
                let name = op["name"].as_str().map(str::to_string);
                self.font(&mut op["font"], None, name.as_deref());
            }
            "set_theme" => {
                if let Some(styles) = op["theme"].get_mut("text_styles") {
                    self.text_styles(styles);
                }
            }
            "wrap" | "set_layout" => self.layout(&mut op["layout"]),
            "set_transition" => self.numbers_in(&mut op["transition"]),
            "set_animation" => self.numbers_in(&mut op["build"]),
            _ => {}
        }
    }

    /// A slide, node or partial node (`fields` of an update), recursively.
    fn node(&mut self, value: &mut Value, id: Option<&str>) {
        let Some(map) = value.as_object_mut() else {
            return;
        };
        let own_id = map
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| id.map(str::to_string));
        let label = own_id.clone().unwrap_or_else(|| "a new node".into());
        let mut kind = map.get("kind").and_then(Value::as_str).map(str::to_string);
        if let Some(alias) = kind.as_deref().and_then(shape_alias) {
            map.insert("kind".into(), json!("shape"));
            map.entry("shape").or_insert_with(|| json!(alias));
            self.note(format!(
                "kind {:?} → shape with shape {alias:?}",
                kind.as_deref().unwrap_or_default()
            ));
            kind = Some("shape".into());
        }
        let size = self.size_for(map, own_id.as_deref());
        // The node's own font, with its size as the em reference.
        if map.contains_key("font") {
            let mut font = map.remove("font").unwrap_or(Value::Null);
            self.font(&mut font, Some(size), None);
            map.insert("font".into(), font);
        }
        if let Some(style) = map.get("style").and_then(Value::as_str).map(str::to_string)
            && !self.styles.contains_key(&style)
        {
            let fallback = if kind.as_deref() == Some("code") {
                "code"
            } else {
                "body"
            };
            self.note(format!(
                "text style {style:?} on {label} is not defined → {fallback} (define it with set_text_style first, or use: {})",
                self.styles.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
            map.insert("style".into(), json!(fallback));
        }
        self.numbers_in_map(map);
        if let Some(layout) = map.get_mut("layout") {
            self.layout(layout);
        }
        if let Some(rows) = map.get_mut("rows").and_then(Value::as_array_mut) {
            let mut changed = false;
            for cell in rows.iter_mut().filter_map(Value::as_array_mut).flatten() {
                if cell.is_number() {
                    *cell = json!(cell.to_string());
                    changed = true;
                }
            }
            if changed {
                self.note(format!("table cells on {label}: numbers → text"));
            }
        }
        if let Some(series) = map.get_mut("series").and_then(Value::as_array_mut) {
            for one in series {
                if let Some(values) = one.get_mut("values").and_then(Value::as_array_mut) {
                    for v in values {
                        if let Some(n) = v.as_str().and_then(parse_number) {
                            *v = number_value(n);
                        }
                    }
                }
            }
        }
        if let Some(children) = map.get_mut("children").and_then(Value::as_array_mut) {
            for child in children {
                self.node(child, None);
            }
        }
    }

    fn size_for(&self, map: &Map<String, Value>, id: Option<&str>) -> f64 {
        map.get("font")
            .and_then(|f| f.get("size"))
            .and_then(Value::as_f64)
            .or_else(|| {
                map.get("style")
                    .and_then(Value::as_str)
                    .and_then(|s| self.styles.get(s).copied())
            })
            .or_else(|| id.and_then(|id| self.node_sizes.get(id).copied()))
            .unwrap_or(self.default_size)
    }

    /// A `font` value: a string becomes `{family}`; numbers are coerced;
    /// letter spacing beyond 2 is taken to be px and converted to em.
    fn font(&mut self, font: &mut Value, size: Option<f64>, owner: Option<&str>) {
        if let Some(family) = font.as_str() {
            let family = family.to_string();
            self.note(format!(
                "font {family:?} → {{\"family\":{family:?}}}{}",
                owner.map_or(String::new(), |o| format!(" on {o}"))
            ));
            *font = json!({ "family": family });
        }
        let Some(map) = font.as_object_mut() else {
            return;
        };
        if let Some(word) = map
            .get("weight")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            && let Some(weight) = weight_word(&word)
        {
            map.insert("weight".into(), json!(weight));
            self.note(format!("weight {word:?} → {weight}"));
        }
        self.numbers_in_map(map);
        let reference = map
            .get("size")
            .and_then(Value::as_f64)
            .or(size)
            .unwrap_or(self.default_size);
        if let Some(spacing) = map.get("letter_spacing").and_then(Value::as_f64)
            && spacing.abs() > 2.0
        {
            let em = (spacing / reference * 1000.0).round() / 1000.0;
            map.insert("letter_spacing".into(), json!(em));
            self.note(format!(
                "letter_spacing {spacing} is em, not px → {em} (= {spacing} px at {reference} px)"
            ));
        }
    }

    fn layout(&mut self, layout: &mut Value) {
        if let Some(map) = layout.as_object_mut() {
            self.numbers_in_map(map);
        }
    }

    fn numbers_in(&mut self, value: &mut Value) {
        if let Some(map) = value.as_object_mut() {
            self.numbers_in_map(map);
        }
    }

    fn number(&mut self, value: &mut Value, key: &str) {
        if let Some(map) = value.as_object_mut() {
            self.number_in(map, key);
        }
    }

    /// Coerce the numeric fields of `map`, descending into the sub-objects
    /// that hold numbers (sizing, position, stroke, animation and so on).
    fn numbers_in_map(&mut self, map: &mut Map<String, Value>) {
        for key in NUMBER_KEYS {
            self.number_in(map, key);
        }
        if let Some(padding) = map.get_mut("padding") {
            match padding {
                Value::String(s) => {
                    if let Some(n) = parse_number(s) {
                        *padding = number_value(n);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        if let Some(n) = item.as_str().and_then(parse_number) {
                            *item = number_value(n);
                        }
                    }
                }
                _ => {}
            }
        }
        for key in [
            "sizing",
            "position",
            "stroke",
            "animation",
            "build_in",
            "emphasis",
            "build_out",
            "transition",
        ] {
            if let Some(Value::Object(inner)) = map.get_mut(key) {
                self.numbers_in_map(inner);
            }
        }
    }

    fn number_in(&mut self, map: &mut Map<String, Value>, key: &str) {
        let Some(value) = map.get(key) else {
            return;
        };
        let integer = INTEGER_KEYS.contains(&key);
        let replacement = match value {
            Value::String(text) => parse_number(text).map(|n| (n, true)),
            Value::Number(n) if integer && n.as_i64().is_none() && n.as_u64().is_none() => {
                n.as_f64().map(|f| (f, false))
            }
            _ => None,
        };
        let Some((number, from_text)) = replacement else {
            return;
        };
        if integer && number.fract() != 0.0 {
            return;
        }
        let fixed = if integer {
            json!(to_integer(number))
        } else {
            number_value(number)
        };
        let before = value.to_string();
        map.insert(key.to_string(), fixed.clone());
        if from_text {
            self.note(format!("{key} {before} → {fixed}"));
        } else {
            self.note(format!("{key} {before} → {fixed} (whole number)"));
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn to_integer(number: f64) -> i64 {
    number as i64
}

fn parse_number(text: &str) -> Option<f64> {
    let text = text.trim();
    let text = text.strip_suffix("px").unwrap_or(text).trim();
    text.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn shape_alias(kind: &str) -> Option<&'static str> {
    match kind {
        "rect" | "rectangle" | "square" => Some("rect"),
        "ellipse" | "circle" | "oval" => Some("ellipse"),
        "line" => Some("line"),
        "arrow" => Some("arrow"),
        _ => None,
    }
}

fn weight_word(word: &str) -> Option<u16> {
    Some(match word {
        "thin" => 100,
        "light" => 300,
        "normal" | "regular" => 400,
        "medium" => 500,
        "semibold" | "semi-bold" => 600,
        "bold" => 700,
        "extrabold" | "extra-bold" => 800,
        "black" | "heavy" => 900,
        _ => return None,
    })
}

/// `name`, `name.deck.json` and `decks/name` become `decks/name.deck.json`.
/// Returns `None` when the path needs no change or is not a recognizable
/// short form (a folder other than `decks/`, an absolute path), which the
/// confinement check then judges.
#[must_use]
pub fn normalize_path(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.contains("..") {
        return None;
    }
    let rest = match trimmed.strip_prefix("decks/") {
        Some(rest) => rest,
        None if trimmed.contains('/') => return None,
        None => trimmed,
    };
    let (folder, file) = rest.rsplit_once('/').map_or(("", rest), |(a, b)| (a, b));
    let stem = [".deck.json", ".json", ".deck"]
        .iter()
        .find_map(|suffix| file.strip_suffix(suffix))
        .unwrap_or(file);
    if stem.is_empty() || stem.starts_with('.') {
        return None;
    }
    let folder = if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    };
    let fixed = format!("decks/{folder}{stem}.deck.json");
    (fixed != input).then_some(fixed)
}

/// A one-line fix for a validation message, when the message is a known
/// shape of mistake. `path` is the JSON path the message is located at.
#[must_use]
pub fn hint(path: &str, message: &str) -> Option<&'static str> {
    let field = path.rsplit('.').next().unwrap_or(path);
    let m = message;
    Some(if m.contains("expected struct Font") {
        "font must be an object, e.g. {\"family\":\"Avenir\"}"
    } else if m.contains("unknown text style") {
        "define it with set_text_style first, or use one of the listed styles"
    } else if field == "letter_spacing" {
        "letter_spacing is in em: 0.05 is 5% of the font size, not px"
    } else if m.contains("unknown variant") && path.ends_with("kind") {
        "kind is one of frame, text, image, shape, table, code, chart (ellipse and line are shape kinds: use kind \"shape\" with shape)"
    } else if m.contains("unknown field") {
        "remove that field, or check its name and nesting against the schema"
    } else if m.contains("missing field `kind`") || m.contains("missing field `type`") {
        "every node needs a kind: frame, text, image, shape, table, code or chart"
    } else if m.contains("expected a pixel number") {
        "sizing w and h are a px number, \"hug\" or \"fill\""
    } else if m.contains("expected theme role or hex color") {
        "use a theme role (bg fg accent muted ...) or a hex color like #1a2b3c"
    } else if m.contains("expected HTTPS or a relative assets path") {
        "image src is an https URL, a space file such as us_1.jpg, or assets/<file>"
    } else if m.contains("padding is a number") {
        "padding is a number or [top, right, bottom, left] in px"
    } else if field == "size" && m.contains("1 to 2000") {
        "font size is in px, 1 to 2000"
    } else if m.contains("unknown position field") || m.contains("expected \"auto\" or") {
        "position is \"auto\" or {\"x\": px, \"y\": px}"
    } else if m.contains("frames nest at most") {
        "flatten the tree: put siblings in one frame instead of nesting deeper"
    } else if m.contains("at most 200 nodes") {
        "split the content over more slides"
    } else if m.contains("node text exceeds 8 KB") {
        "split the text over several nodes"
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests;
