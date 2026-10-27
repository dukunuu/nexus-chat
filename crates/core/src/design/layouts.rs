//! Built-in layouts and slot instantiation. Missing/empty slots disappear;
//! non-slot decoration remains. Slot values are Markdown text or image paths.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{Design, Spacing};
use crate::deck::{Deck, Kind, Node, Slide};

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
           "fit": "cover", "sizing": {"w": "fill", "h": "fill"}})
}

/// Eleven spacious, role-only layouts. All geometry uses the supplied rhythm.
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
                text("kicker", "caption"),
                text("title", "title"),
                text("subtitle", "body"),
            ],
            "section" => vec![
                text("number", "display"),
                text("title", "title"),
                text("subtitle", "body"),
            ],
            "quote" => vec![
                json!({"kind": "text", "text": "“", "style": "display"}),
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
                        panel["fill"] = json!("bg");
                        panel["stroke"] = json!({"color": "muted", "width": 1});
                        panel["radius"] = json!(spacing.gap / 2.0);
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
                text("kicker", "caption"),
                text("title", "heading"),
                text("items", "lead"),
            ],
            _ => vec![
                text("title", "heading"),
                text("body", "lead"),
                text("caption", "caption"),
            ],
        };
        let mut slide = json!({"id": name, "layout": {"mode": "column", "gap": spacing.gap,
            "padding": spacing.margin, "cross": "stretch",
            "main": if centered { "center" } else { "start" }}, "children": children});
        if matches!(name, "title" | "closing" | "section") {
            slide["children"].as_array_mut().expect("children").insert(0,
                json!({"kind": "shape", "fill": "accent", "sizing": {"w": spacing.margin, "h": spacing.gap / 4.0}}));
        }
        // Deck validation supplies stable ids to the non-slot frame decoration.
        let deck =
            Deck::check(&json!({"version": 2, "title": name, "slides": [slide]}).to_string())
                .expect("built-in layout");
        layouts.insert(name.into(), deck.slides.into_iter().next().expect("slide"));
    }
    layouts
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
    fn fill(nodes: &mut Vec<Node>, values: &BTreeMap<String, String>) {
        nodes.retain_mut(|node| {
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
            node.id.clear();
            node.slot.clear();
            if let Some(children) = node.children_mut() {
                fill(children, values);
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
    fill(&mut slide.children, values);
    Ok(slide)
}
