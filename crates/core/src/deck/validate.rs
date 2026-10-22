//! Parsing and validation: forgiving where the intent is clear (missing or
//! duplicate ids are filled, version 1 is migrated, missing theme tokens and
//! text styles are added), strict and located everywhere else.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::model::{
    Animation, AnimationSlot, AutoLayout, Build, BuildBy, Deck, Effect, Font, Kind, Node, Position,
    Slide, Theme, Transition,
};
use super::{CheckError, MAX_SOURCE, is_hex, issue, migrate, safe_asset_path};

/// Slides per deck.
pub const MAX_SLIDES: usize = 200;
/// Nodes on one slide, containers included.
pub const MAX_NODES: usize = 200;
/// Frames nested inside frames.
pub const MAX_DEPTH: usize = 8;
const MAX_TEXT: usize = 8 * 1024;
const MAX_LENGTH: f64 = 20_000.0;

impl Deck {
    /// Parse and normalize a deck. Version 1 decks are migrated to version 2
    /// first (see `migrate.rs`). Missing/duplicate ids are repaired; other
    /// failures carry JSON paths. Limits apply before deserializing the model.
    pub fn check(source: &str) -> std::result::Result<Self, Vec<CheckError>> {
        Self::check_noted(source).map(|(deck, _)| deck)
    }

    /// Like [`Deck::check`], also returning load notes: animation data that
    /// could not be parsed (an old placeholder, a typo) is dropped rather than
    /// failing the deck, and each drop is reported here.
    pub fn check_noted(source: &str) -> std::result::Result<(Self, Vec<String>), Vec<CheckError>> {
        let mut errors = Vec::new();
        let mut notes = Vec::new();
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
        match value.get("version") {
            Some(v) if *v == json!(1) => migrate::v1_to_v2(&mut value),
            Some(v) if *v == json!(2) => {}
            _ => issue(&mut errors, "$.version", "expected deck version 1 or 2"),
        }
        let theme = normalize_theme(&mut value, &mut errors);
        validate_theme(&theme, &mut errors);
        let reserved = reserved_ids(&value);
        let mut used = BTreeSet::new();
        if let Some(slides) = value["slides"].as_array_mut() {
            if slides.len() > MAX_SLIDES {
                issue(
                    &mut errors,
                    "$.slides",
                    &format!("at most {MAX_SLIDES} slides"),
                );
            }
            for (s, slide) in slides.iter_mut().enumerate() {
                let mut walker = Walker {
                    theme: &theme,
                    errors: &mut errors,
                    notes: &mut notes,
                    reserved: &reserved,
                    used: &mut used,
                    slide: s + 1,
                    count: 0,
                };
                walker.slide(slide, &format!("$.slides[{s}]"));
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
        if errors.is_empty() {
            Ok((deck.expect("checked deck"), notes))
        } else {
            Err(errors)
        }
    }
}

fn normalize_theme(value: &mut Value, errors: &mut Vec<CheckError>) -> Theme {
    let defaults = serde_json::to_value(Theme::default()).expect("theme JSON");
    if value.get("theme").is_none() {
        value["theme"] = defaults.clone();
    } else if value["theme"].is_object() {
        for group in ["colors", "fonts", "text_styles"] {
            if value["theme"].get(group).is_none() {
                value["theme"][group] = defaults[group].clone();
            } else if let Some(map) = value["theme"][group].as_object_mut() {
                for (key, val) in defaults[group].as_object().expect("token map") {
                    map.entry(key).or_insert_with(|| val.clone());
                }
            }
        }
    }
    match serde_json::from_value(value["theme"].clone()) {
        Ok(theme) => theme,
        Err(e) => {
            issue(errors, "$.theme", &e.to_string());
            Theme::default()
        }
    }
}

fn valid_family(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 80
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\'' | '\\' | ';' | '<' | '>' | '{' | '}'))
}

fn finite(v: f64, low: f64, high: f64) -> bool {
    v.is_finite() && (low..=high).contains(&v)
}

fn check_font(font: &Font, theme: &Theme, path: &str, errors: &mut Vec<CheckError>) {
    let color_ok = |c: &str| is_hex(c) || theme.colors.contains_key(c);
    if font.family.as_deref().is_some_and(|f| !valid_family(f)) {
        issue(
            errors,
            &format!("{path}.family"),
            "invalid font family name",
        );
    }
    if font.size.is_some_and(|v| !finite(v, 1.0, 2000.0)) {
        issue(errors, &format!("{path}.size"), "expected 1 to 2000 px");
    }
    if font.weight.is_some_and(|v| !(1..=1000).contains(&v)) {
        issue(errors, &format!("{path}.weight"), "expected 100 to 900");
    }
    if font.line_height.is_some_and(|v| !finite(v, 0.5, 5.0)) {
        issue(errors, &format!("{path}.line_height"), "expected 0.5 to 5");
    }
    if font.letter_spacing.is_some_and(|v| !finite(v, -0.5, 2.0)) {
        issue(
            errors,
            &format!("{path}.letter_spacing"),
            "expected -0.5 to 2 em",
        );
    }
    if font.color.as_deref().is_some_and(|c| !color_ok(c)) {
        issue(
            errors,
            &format!("{path}.color"),
            "expected theme role or hex color",
        );
    }
}

fn validate_theme(theme: &Theme, errors: &mut Vec<CheckError>) {
    for (role, color) in &theme.colors {
        if !is_hex(color) {
            issue(
                errors,
                &format!("$.theme.colors.{role}"),
                "expected hex color",
            );
        }
    }
    for (role, family) in &theme.fonts {
        if !valid_family(family) {
            issue(
                errors,
                &format!("$.theme.fonts.{role}"),
                "invalid font family name",
            );
        }
    }
    if !finite(theme.size.w, 320.0, 7680.0) || !finite(theme.size.h, 180.0, 4320.0) {
        issue(
            errors,
            "$.theme.size",
            "expected 320 to 7680 by 180 to 4320 px",
        );
    }
    for (name, font) in &theme.text_styles {
        let path = format!("$.theme.text_styles.{name}");
        if name.is_empty()
            || name.len() > 32
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
        {
            issue(
                errors,
                &path,
                "style names are lowercase letters, digits, - and _",
            );
        }
        check_font(font, theme, &path, errors);
    }
    if theme.font_files.len() > 16 {
        issue(errors, "$.theme.font_files", "at most 16 font files");
    }
    for (i, file) in theme.font_files.iter().enumerate() {
        let path = format!("$.theme.font_files[{i}]");
        if !valid_family(&file.family) {
            issue(
                errors,
                &format!("{path}.family"),
                "invalid font family name",
            );
        }
        let ext_ok = std::path::Path::new(&file.file)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "ttf" | "otf" | "ttc" | "woff2"
                )
            });
        if !file.file.starts_with("assets/fonts/") || !safe_asset_path(&file.file) || !ext_ok {
            issue(
                errors,
                &format!("{path}.file"),
                "expected assets/fonts/<name>.ttf, .otf, .ttc or .woff2",
            );
        }
        if !(1..=1000).contains(&file.weight) {
            issue(errors, &format!("{path}.weight"), "expected 100 to 900");
        }
        if file.fallback.len() > 8 || file.fallback.iter().any(|f| !valid_family(f)) {
            issue(
                errors,
                &format!("{path}.fallback"),
                "up to 8 valid family names",
            );
        }
    }
}

pub(super) fn reserved_ids(value: &Value) -> BTreeSet<String> {
    fn nodes(value: &Value, reserved: &mut BTreeSet<String>) {
        reserve_id(value, reserved);
        if let Some(children) = value["children"].as_array() {
            for child in children {
                nodes(child, reserved);
            }
        }
    }
    let mut reserved = BTreeSet::new();
    if let Some(slides) = value["slides"].as_array() {
        for slide in slides {
            nodes(slide, &mut reserved);
        }
    }
    reserved
}

pub(super) fn reserve_id(value: &Value, reserved: &mut BTreeSet<String>) {
    if let Some(id) = value["id"].as_str().filter(|id| !id.is_empty()) {
        reserved.insert(id.into());
    }
}

pub(super) fn fill_id(
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

/// Complete and repair a slide's build order against the animated nodes left
/// in its tree: unknown and repeated ids are dropped, animated nodes missing
/// from the list follow in tree order. A malformed list is dropped with a note.
fn repair_builds(slide: &mut Value, path: &str, notes: &mut Vec<String>) {
    fn animated(value: &Value, ids: &mut Vec<String>) {
        if value.get("animation").is_some_and(|a| !a.is_null())
            && let Some(id) = value["id"].as_str()
        {
            ids.push(id.into());
        }
        if let Some(children) = value["children"].as_array() {
            for child in children {
                animated(child, ids);
            }
        }
    }
    let Some(object) = slide.as_object_mut() else {
        return;
    };
    let mut listed = Vec::new();
    match object.remove("builds") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                if let Value::String(id) = item {
                    listed.push(id);
                } else {
                    notes.push(format!("{path}.builds: dropped a non-string entry"));
                }
            }
        }
        Some(_) => notes.push(format!(
            "{path}.builds: dropped (expected an array of node ids)"
        )),
    }
    let mut ids = Vec::new();
    animated(slide, &mut ids);
    let mut order: Vec<String> = Vec::new();
    for id in listed.into_iter().chain(ids.iter().cloned()) {
        if ids.contains(&id) && !order.contains(&id) {
            order.push(id);
        }
    }
    if !order.is_empty() {
        slide["builds"] = json!(order);
    }
}

/// Located range check of a slide transition.
pub(super) fn check_transition(transition: &Transition, path: &str, errors: &mut Vec<CheckError>) {
    if !finite(transition.duration_ms, 0.0, 5000.0) {
        issue(errors, &format!("{path}.duration_ms"), "expected 0 to 5000");
    }
}

/// Located range and compatibility check of one build in `slot`.
pub(super) fn check_build(
    build: &Build,
    slot: AnimationSlot,
    text: bool,
    path: &str,
    errors: &mut Vec<CheckError>,
) {
    for (key, value, max) in [
        ("duration_ms", build.duration_ms, 5000.0),
        ("delay_ms", build.delay_ms, 10000.0),
        ("stagger_ms", build.stagger_ms.unwrap_or(0.0), 10000.0),
    ] {
        if !finite(value, 0.0, max) {
            issue(
                errors,
                &format!("{path}.{key}"),
                &format!("expected 0 to {max}"),
            );
        }
    }
    let emphasis_effect = matches!(build.effect, Effect::Pulse | Effect::Shake);
    if emphasis_effect && slot != AnimationSlot::Emphasis {
        issue(
            errors,
            &format!("{path}.effect"),
            "pulse and shake are emphasis effects, not entry or exit",
        );
    }
    if !emphasis_effect && slot == AnimationSlot::Emphasis {
        issue(
            errors,
            &format!("{path}.effect"),
            "emphasis uses pulse or shake",
        );
    }
    if !text && build.effect == Effect::Typewriter {
        issue(
            errors,
            &format!("{path}.effect"),
            "typewriter needs a text node",
        );
    }
    if !text && build.by != BuildBy::Whole {
        issue(
            errors,
            &format!("{path}.by"),
            "word, line and item builds need a text node",
        );
    }
}

struct Walker<'a> {
    theme: &'a Theme,
    errors: &'a mut Vec<CheckError>,
    notes: &'a mut Vec<String>,
    reserved: &'a BTreeSet<String>,
    used: &'a mut BTreeSet<String>,
    slide: usize,
    count: usize,
}

impl Walker<'_> {
    fn color_ok(&self, color: &str) -> bool {
        is_hex(color) || self.theme.colors.contains_key(color)
    }

    fn slide(&mut self, slide: &mut Value, path: &str) {
        fill_id(slide, &format!("s{}", self.slide), self.reserved, self.used);
        self.transition(slide, path);
        let mut shell = slide.clone();
        let children = shell.as_object_mut().and_then(|o| o.remove("children"));
        // Build order is repaired below, once the animated nodes are known.
        if let Some(object) = shell.as_object_mut() {
            object.remove("builds");
        }
        match serde_json::from_value::<Slide>(shell) {
            Ok(typed) => {
                self.layout(&typed.layout, &format!("{path}.layout"));
                if typed.fill.as_deref().is_some_and(|c| !self.color_ok(c)) {
                    issue(
                        self.errors,
                        &format!("{path}.fill"),
                        "expected theme role or hex color",
                    );
                }
            }
            Err(e) => issue(self.errors, path, &e.to_string()),
        }
        match children {
            None => {}
            Some(Value::Array(_)) => {
                if let Some(children) = slide.get_mut("children") {
                    self.nodes(children, &format!("{path}.children"), 0);
                }
            }
            Some(_) => issue(self.errors, &format!("{path}.children"), "expected array"),
        }
        repair_builds(slide, path, self.notes);
    }

    /// Typed transition: unparseable data is dropped with a note, ranges are errors.
    fn transition(&mut self, slide: &mut Value, path: &str) {
        let path = format!("{path}.transition");
        let Some(object) = slide.as_object_mut() else {
            return;
        };
        match object.get("transition").cloned() {
            None => {}
            Some(Value::Null) => {
                object.remove("transition");
            }
            Some(raw) => match serde_json::from_value::<Transition>(raw) {
                Ok(transition) => check_transition(&transition, &path, self.errors),
                Err(e) => {
                    object.remove("transition");
                    self.notes.push(format!("{path}: dropped ({e})"));
                }
            },
        }
    }

    /// Typed animation, like [`Self::transition`]; an animation with no builds is removed.
    fn animation(&mut self, node: &mut Value, path: &str) {
        let path = format!("{path}.animation");
        let text = node["kind"] == "text";
        let Some(object) = node.as_object_mut() else {
            return;
        };
        match object.get("animation").cloned() {
            None => {}
            Some(Value::Null) => {
                object.remove("animation");
            }
            Some(raw) => match serde_json::from_value::<Animation>(raw) {
                Ok(animation) => {
                    for (slot, build) in animation.sequence() {
                        let at = format!("{path}.{}", slot.key());
                        check_build(build, slot, text, &at, self.errors);
                    }
                    if animation.is_empty() {
                        object.remove("animation");
                    }
                }
                Err(e) => {
                    object.remove("animation");
                    self.notes.push(format!("{path}: dropped ({e})"));
                }
            },
        }
    }

    fn layout(&mut self, layout: &AutoLayout, path: &str) {
        if !finite(layout.gap, 0.0, 2000.0)
            || layout.gap_cross.is_some_and(|g| !finite(g, 0.0, 2000.0))
        {
            issue(self.errors, &format!("{path}.gap"), "expected 0 to 2000 px");
        }
        if layout.padding.iter().any(|p| !finite(*p, 0.0, 2000.0)) {
            issue(
                self.errors,
                &format!("{path}.padding"),
                "expected 0 to 2000 px",
            );
        }
    }

    fn nodes(&mut self, nodes: &mut Value, path: &str, depth: usize) {
        let Some(items) = nodes.as_array_mut() else {
            return;
        };
        for (i, node) in items.iter_mut().enumerate() {
            let path = format!("{path}[{i}]");
            self.count += 1;
            if self.count == MAX_NODES + 1 {
                issue(
                    self.errors,
                    &path,
                    &format!("at most {MAX_NODES} nodes per slide"),
                );
            }
            self.node(node, &path, depth, i);
        }
    }

    fn node(&mut self, node: &mut Value, path: &str, depth: usize, index: usize) {
        if !node.is_object() {
            issue(self.errors, path, "expected node object");
            return;
        }
        fill_id(
            node,
            &format!("n{}_{}_{}", self.slide, depth + 1, index + 1),
            self.reserved,
            self.used,
        );
        self.animation(node, path);
        let is_frame = node["kind"] == "frame";
        let mut shell = node.clone();
        let children = if is_frame {
            shell.as_object_mut().and_then(|o| o.remove("children"))
        } else {
            None
        };
        match serde_json::from_value::<Node>(shell) {
            Ok(typed) => self.typed(&typed, path),
            Err(e) => {
                let field = if matches!(
                    node["kind"].as_str(),
                    Some("frame" | "text" | "image" | "shape" | "table" | "code" | "chart")
                ) {
                    ""
                } else {
                    ".kind"
                };
                issue(self.errors, &format!("{path}{field}"), &e.to_string());
            }
        }
        match children {
            None => {}
            Some(Value::Array(_)) => {
                if depth + 1 >= MAX_DEPTH {
                    issue(
                        self.errors,
                        &format!("{path}.children"),
                        &format!("frames nest at most {MAX_DEPTH} deep"),
                    );
                } else if let Some(children) = node.get_mut("children") {
                    self.nodes(children, &format!("{path}.children"), depth + 1);
                }
            }
            Some(_) => issue(self.errors, &format!("{path}.children"), "expected array"),
        }
    }

    fn style(&mut self, style: &str, path: &str) {
        if !self.theme.text_styles.contains_key(style) {
            let known = self
                .theme
                .text_styles
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            issue(
                self.errors,
                &format!("{path}.style"),
                &format!("unknown text style {style:?}; styles are: {known}"),
            );
        }
    }

    fn typed(&mut self, node: &Node, path: &str) {
        let sizing = &node.sizing;
        for (name, dim) in [("w", sizing.w), ("h", sizing.h)] {
            if let super::model::Dim::Fixed(n) = dim
                && !finite(n, 0.0, MAX_LENGTH)
            {
                issue(
                    self.errors,
                    &format!("{path}.sizing.{name}"),
                    "expected 0 to 20000 px, \"hug\" or \"fill\"",
                );
            }
        }
        for (name, limit) in [
            ("min_w", sizing.min_w),
            ("max_w", sizing.max_w),
            ("min_h", sizing.min_h),
            ("max_h", sizing.max_h),
        ] {
            if limit.is_some_and(|v| !finite(v, 0.0, MAX_LENGTH)) {
                issue(
                    self.errors,
                    &format!("{path}.sizing.{name}"),
                    "expected 0 to 20000 px",
                );
            }
        }
        if let (Some(low), Some(high)) = (sizing.min_w, sizing.max_w)
            && low > high
        {
            issue(
                self.errors,
                &format!("{path}.sizing"),
                "min_w exceeds max_w",
            );
        }
        if let (Some(low), Some(high)) = (sizing.min_h, sizing.max_h)
            && low > high
        {
            issue(
                self.errors,
                &format!("{path}.sizing"),
                "min_h exceeds max_h",
            );
        }
        if let Position::Absolute { x, y } = node.position
            && (!finite(x, -MAX_LENGTH, MAX_LENGTH) || !finite(y, -MAX_LENGTH, MAX_LENGTH))
        {
            issue(
                self.errors,
                &format!("{path}.position"),
                "expected -20000 to 20000 px",
            );
        }
        if !finite(node.opacity, 0.0, 1.0) {
            issue(self.errors, &format!("{path}.opacity"), "expected 0 to 1");
        }
        if node.rotation.is_some_and(|r| !r.is_finite()) {
            issue(self.errors, &format!("{path}.rotation"), "expected degrees");
        }
        if node.name.as_ref().is_some_and(|n| n.chars().count() > 80) {
            issue(
                self.errors,
                &format!("{path}.name"),
                "at most 80 characters",
            );
        }
        self.kind(&node.kind, path);
    }

    // One arm per node kind.
    #[allow(clippy::too_many_lines)]
    fn kind(&mut self, kind: &Kind, path: &str) {
        let theme = self.theme;
        let check_color = |color: &str, field: &str, errors: &mut Vec<CheckError>| {
            if !(is_hex(color) || theme.colors.contains_key(color)) {
                issue(
                    errors,
                    &format!("{path}.{field}"),
                    "expected theme role or hex color",
                );
            }
        };
        match kind {
            Kind::Frame {
                layout,
                fill,
                stroke,
                radius,
                ..
            } => {
                self.layout(layout, &format!("{path}.layout"));
                if let Some(fill) = fill {
                    check_color(fill, "fill", self.errors);
                }
                if let Some(stroke) = stroke {
                    check_color(&stroke.color, "stroke.color", self.errors);
                    if !finite(stroke.width, 0.0, 200.0) {
                        issue(
                            self.errors,
                            &format!("{path}.stroke.width"),
                            "expected 0 to 200 px",
                        );
                    }
                }
                if !finite(*radius, 0.0, 5000.0) {
                    issue(
                        self.errors,
                        &format!("{path}.radius"),
                        "expected 0 to 5000 px",
                    );
                }
            }
            Kind::Text { text, style, font } => {
                if text.len() > MAX_TEXT {
                    issue(self.errors, path, "node text exceeds 8 KB");
                }
                self.style(style, path);
                check_font(font, theme, &format!("{path}.font"), self.errors);
            }
            Kind::Code {
                language,
                source,
                style,
                font,
            } => {
                if language.len() + source.len() > MAX_TEXT {
                    issue(self.errors, path, "node text exceeds 8 KB");
                }
                self.style(style, path);
                check_font(font, theme, &format!("{path}.font"), self.errors);
            }
            Kind::Table { rows, style, font } => {
                if rows.iter().flatten().map(String::len).sum::<usize>() > MAX_TEXT {
                    issue(self.errors, path, "node text exceeds 8 KB");
                }
                self.style(style, path);
                check_font(font, theme, &format!("{path}.font"), self.errors);
            }
            Kind::Chart { labels, series, .. } => {
                if series.iter().any(|s| {
                    s.values.len() != labels.len() || s.values.iter().any(|v| !v.is_finite())
                }) {
                    issue(
                        self.errors,
                        &format!("{path}.series"),
                        "series must have one finite value per label",
                    );
                }
                let bytes = labels.iter().map(String::len).sum::<usize>()
                    + series.iter().map(|s| s.name.len()).sum::<usize>();
                if bytes > MAX_TEXT {
                    issue(self.errors, path, "node text exceeds 8 KB");
                }
            }
            Kind::Image { src, .. } => {
                if src.starts_with("https://") {
                    if let Err(e) = crate::remote_image::validate_url(src) {
                        issue(self.errors, &format!("{path}.src"), &e.to_string());
                    }
                } else if !safe_asset_path(src) {
                    issue(
                        self.errors,
                        &format!("{path}.src"),
                        "expected HTTPS or a relative assets path without '..'",
                    );
                }
            }
            Kind::Shape {
                fill,
                stroke,
                radius,
                ..
            } => {
                if let Some(fill) = fill {
                    check_color(fill, "fill", self.errors);
                }
                if let Some(stroke) = stroke {
                    check_color(&stroke.color, "stroke.color", self.errors);
                    if !finite(stroke.width, 0.0, 200.0) {
                        issue(
                            self.errors,
                            &format!("{path}.stroke.width"),
                            "expected 0 to 200 px",
                        );
                    }
                }
                if !finite(*radius, 0.0, 5000.0) {
                    issue(
                        self.errors,
                        &format!("{path}.radius"),
                        "expected 0 to 5000 px",
                    );
                }
            }
        }
    }
}
