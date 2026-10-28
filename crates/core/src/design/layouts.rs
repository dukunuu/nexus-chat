//! Built-in layouts and slot instantiation. Missing/empty slots disappear;
//! non-slot decoration remains. Slot values are Markdown text or image paths.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{Design, Spacing};
use crate::deck::{Deck, Kind, Node, Slide};

mod rich;
#[cfg(test)]
mod tests;

fn text(slot: &str, style: &str) -> Value {
    json!({"id": slot, "kind": "text", "slot": slot, "text": slot,
           "style": style, "sizing": {"w": "fill"}})
}
fn column(children: &[Value], spacing: Spacing) -> Value {
    json!({"kind": "frame", "sizing": {"w": "fill", "h": "fill"},
           "layout": {"mode": "column", "gap": spacing.gap, "main": "center", "cross": "stretch"}, "children": children})
}
fn row(children: &[Value], spacing: Spacing) -> Value {
    json!({"kind": "frame", "sizing": {"w": "fill", "h": "fill"},
           "layout": {"mode": "row", "gap": spacing.gap * 2.0, "cross": "stretch"}, "children": children})
}
fn image() -> Value {
    json!({"id": "image", "kind": "image", "slot": "image", "src": "assets/placeholder.png",
           "fit": "cover", "radius": 32, "shadow": "md", "sizing": {"w": "fill", "h": "fill"}})
}

/// Twenty-one role-only layouts, from spacious copy to editorial photo stories.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn builtin(spacing: Spacing) -> BTreeMap<String, Slide> {
    let mut layouts = BTreeMap::new();
    for name in [
        "title",
        "section",
        "content",
        "two_column",
        "image_left",
        "image_right",
        "quote",
        "big_number",
        "comparison",
        "agenda",
        "closing",
    ] {
        let centered = matches!(
            name,
            "title" | "section" | "quote" | "big_number" | "closing" | "content" | "agenda"
        );
        let children = match name {
            "title" | "closing" => vec![
                text("kicker", "kicker"),
                text("title", "title"),
                text("subtitle", "body"),
            ],
            "section" => vec![
                text("number", "display"),
                text("title", "title"),
                text("subtitle", "body"),
            ],
            "quote" => vec![
                json!({"kind": "text", "text": "“", "style": "title", "font":{"color":"accent"}}),
                text("quote", "quote"),
                json!({"kind": "frame", "layout": {"mode": "row", "gap": spacing.gap, "cross": "center"},
                    "children": [
                        {"kind": "shape", "fill": "accent", "sizing": {"w": spacing.gap * 2.0, "h": spacing.gap / 8.0}},
                        text("attribution", "caption")]}),
            ],
            "big_number" => vec![
                text("number", "display"),
                text("title", "subheading"),
                text("body", "body"),
            ],
            "two_column" | "comparison" => {
                let mut left = column(
                    &[text("left_title", "subheading"), text("left", "lead")],
                    spacing,
                );
                let mut right = column(
                    &[text("right_title", "subheading"), text("right", "lead")],
                    spacing,
                );
                if name == "comparison" {
                    for panel in [&mut left, &mut right] {
                        panel["fill"] =
                            json!({"linear": {"angle": 145, "stops": [[0, "surface"], [1, "bg"]]}});
                        panel["shadow"] = json!("sm");
                        panel["radius"] = json!(spacing.gap);
                        panel["layout"]["padding"] = json!(vec![spacing.gap; 4]);
                    }
                }
                vec![text("title", "heading"), row(&[left, right], spacing)]
            }
            "image_left" | "image_right" => {
                let copy = column(
                    &[
                        text("title", "heading"),
                        text("body", "body"),
                        text("caption", "caption"),
                    ],
                    spacing,
                );
                let sides = if name == "image_left" {
                    vec![image(), copy]
                } else {
                    vec![copy, image()]
                };
                vec![row(&sides, spacing)]
            }
            "agenda" => vec![
                text("kicker", "kicker"),
                text("title", "heading"),
                text("items", "lead"),
            ],
            _ => vec![
                text("title", "heading"),
                json!({"kind":"frame", "fill":{"linear":{"angle":100,"stops":[[0,"surface"],[1,"bg"]]}}, "radius":24, "sizing":{"w":"fill"}, "layout":{"mode":"column","padding":48,"cross":"stretch"}, "children":[text("body", "lead")]}),
                text("caption", "caption"),
            ],
        };
        let mut slide = json!({"id": name, "layout": {"mode": "column", "gap": spacing.gap,
            "padding": spacing.margin, "cross": "stretch",
            "main": if centered { "center" } else { "start" }}, "children": children});
        if matches!(name, "title" | "closing" | "section" | "agenda") {
            slide["children"].as_array_mut().expect("children").insert(0,
                json!({"kind": "shape", "fill": "accent", "sizing": {"w": spacing.margin, "h": spacing.gap / 4.0}}));
        }
        if matches!(name, "section" | "closing") {
            slide["fill"] = json!({"linear": {"angle":120, "stops":[[0,"accent2"],[1,"fg"]]}});
            for child in slide["children"].as_array_mut().expect("children") {
                if child["kind"] == "text" {
                    child["font"] = json!({"color":"bg"});
                }
            }
        }
        if matches!(name, "title" | "closing" | "section" | "big_number") {
            let decoration = json!({"kind":"shape", "shape":"ellipse", "fill":{"radial":{"stops":[[0,{"color":"accent2","opacity":0.16}],[1,{"color":"accent2","opacity":0}]]}}, "position":{"x":1220,"y":-160}, "sizing":{"w":1040,"h":1040}, "locked":true});
            slide["children"]
                .as_array_mut()
                .expect("children")
                .insert(0, decoration);
            for child in slide["children"].as_array_mut().expect("children") {
                if child["slot"] == "title" || child["slot"] == "subtitle" {
                    child["sizing"]["max_w"] = json!(1280);
                }
            }
        }
        corners(&mut slide, spacing.radius / 32.0);
        // Deck validation supplies stable ids to the non-slot frame decoration.
        let deck =
            Deck::check(&json!({"version": 2, "title": name, "slides": [slide]}).to_string())
                .expect("built-in layout");
        layouts.insert(name.into(), deck.slides.into_iter().next().expect("slide"));
    }
    for (name, mut slide) in rich::builtin(spacing) {
        slide["id"] = json!(name);
        corners(&mut slide, spacing.radius / 32.0);
        let deck = Deck::check(&json!({"version":2,"title":name,"slides":[slide]}).to_string())
            .expect("built-in layout");
        layouts.insert(name.into(), deck.slides.into_iter().next().expect("slide"));
    }
    layouts
}

/// Scale the layouts' card and photo corners (32 px and under at the default
/// radius) to the design's; circles drawn with larger radii stay round.
fn corners(value: &mut Value, scale: f64) {
    if let Some(object) = value.as_object_mut() {
        if let Some(radius) = object
            .get("radius")
            .and_then(Value::as_f64)
            .filter(|r| *r <= 32.0)
        {
            object.insert("radius".into(), json!((radius * scale).round()));
        }
        for child in object.values_mut() {
            corners(child, scale);
        }
    } else if let Some(items) = value.as_array_mut() {
        for item in items {
            corners(item, scale);
        }
    }
}

/// Valid slot names, sorted for deterministic hints and galleries.
#[must_use]
pub fn slots(slide: &Slide) -> BTreeSet<String> {
    slide
        .nodes()
        .iter()
        .filter(|node| !node.slot.is_empty())
        .map(|node| node.slot.clone())
        .collect()
}

/// Instantiate a pre-resolved layout. Inserted ids are assigned by the caller's
/// normal insertion machinery, never copied from the template.
pub fn instantiate(
    design: &Design,
    layout: &str,
    values: &BTreeMap<String, String>,
) -> Result<Slide> {
    fn fill(nodes: &mut Vec<Node>, values: &BTreeMap<String, String>, title_slide: bool) {
        nodes.retain_mut(|node| {
            if !node.children().is_empty() {
                let mut descendants = Vec::new();
                node.walk(&mut descendants);
                let slots: Vec<_> = descendants
                    .iter()
                    .filter(|child| !child.slot.is_empty())
                    .collect();
                if !slots.is_empty()
                    && !slots.iter().any(|child| {
                        values
                            .get(&child.slot)
                            .is_some_and(|value| !value.is_empty())
                    })
                {
                    return false;
                }
            }
            if !node.slot.is_empty() {
                let Some(value) = values.get(&node.slot).filter(|s| !s.is_empty()) else {
                    return false;
                };
                match &mut node.kind {
                    Kind::Text { text, .. } => text.clone_from(value),
                    Kind::Image { src, .. } => src.clone_from(value),
                    _ => return false,
                }
            }
            if node.source_placeholder.is_none() && matches!(node.kind, Kind::Text { .. }) {
                node.source_placeholder = match node.slot.as_str() {
                    "title" => Some(crate::deck::model::Placeholder {
                        kind: if title_slide { "ctrTitle" } else { "title" }.into(),
                        idx: "0".into(),
                    }),
                    "subtitle" if title_slide => Some(crate::deck::model::Placeholder {
                        kind: "subTitle".into(),
                        idx: "1".into(),
                    }),
                    "body" => Some(crate::deck::model::Placeholder {
                        kind: "body".into(),
                        idx: "1".into(),
                    }),
                    _ => None,
                };
            }
            node.id.clear();
            node.slot.clear();
            if let Some(children) = node.children_mut() {
                fill(children, values, title_slide);
            }
            true
        });
    }
    let template = design.slide.layouts.get(layout).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown layout {layout:?}; fix: layouts are {}",
            design
                .slide
                .layouts
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let valid = slots(template);
    for key in values.keys() {
        ensure!(
            valid.contains(key),
            "unknown slot {key:?}; fix: slots are {}",
            valid.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    let mut slide = template.clone();
    slide.id.clear();
    slide.builds.clear();
    if slide.source_layout.is_none() {
        let index = match layout {
            "title" | "closing" | "cover_image" | "closing_image" => 2,
            "section" | "section_color" => 4,
            "content" | "image_left" | "image_right" | "big_number" | "chart_focus" => 3,
            _ => 5,
        };
        slide.source_layout = Some(format!("ppt/slideLayouts/slideLayout{index}.xml"));
    }
    let title_slide = slide.source_layout.as_deref() == Some("ppt/slideLayouts/slideLayout2.xml");
    fill(&mut slide.children, values, title_slide);
    Ok(slide)
}
