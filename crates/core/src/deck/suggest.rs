//! Independently validated edit proposals shared by the feedback flow.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::validate::{fill_id, reserve_id, reserved_ids};
use super::{CheckError, Deck, Kind, Op, Slide, apply};

/// Slides and (slide, node) pairs; empty means the whole deck.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Selection {
    pub slides: Vec<String>,
    pub blocks: Vec<(String, String)>,
}

impl Selection {
    /// Report every slide or block that the deck does not contain.
    pub fn check(&self, deck: &Deck, errors: &mut Vec<CheckError>) {
        for (i, id) in self.slides.iter().enumerate() {
            if !deck.slides.iter().any(|s| s.id == *id) {
                super::issue(errors, &format!("selection.slides[{i}]"), "unknown slide");
            }
        }
        for (i, (slide, block)) in self.blocks.iter().enumerate() {
            if !deck
                .slides
                .iter()
                .any(|s| s.id == *slide && s.node(block).is_some())
            {
                super::issue(errors, &format!("selection.blocks[{i}]"), "unknown block");
            }
        }
    }
}

/// Untrusted operations stay JSON until each suggestion is checked separately.
#[derive(Clone, Debug, Deserialize)]
pub struct RawSuggestion {
    pub why: String,
    pub ops: Vec<Value>,
}

/// An applicable proposal. Hashes and missing insertion ids are host supplied.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub why: String,
    pub ops: Vec<Op>,
}

/// Fill trusted preconditions and ids, then apply each proposal to the base.
/// Invalid proposals do not prevent independent proposals from surviving.
#[must_use]
pub fn prepare(base: &Deck, raw: Vec<RawSuggestion>) -> (Vec<Suggestion>, Vec<CheckError>) {
    prepare_with_resolver(base, raw, None)
}

/// Prepare proposals with caller-owned design lookup.
#[must_use]
pub fn prepare_with_resolver(
    base: &Deck,
    raw: Vec<RawSuggestion>,
    resolver: Option<&crate::design::Resolver<'_>>,
) -> (Vec<Suggestion>, Vec<CheckError>) {
    let mut valid = Vec::new();
    let mut errors = Vec::new();
    if raw.is_empty() || raw.len() > 5 {
        super::issue(&mut errors, "suggestions", "expected 1–5 suggestions");
        return (valid, errors);
    }
    for (i, raw) in raw.into_iter().enumerate() {
        let path = format!("suggestions[{i}]");
        match prepare_one(base, raw, i, resolver) {
            Ok(suggestion) => valid.push(suggestion),
            Err(error) => errors.push(CheckError {
                path: format!("{path}{}", error.path),
                message: error.message,
            }),
        }
    }
    (valid, errors)
}

fn failure(path: &str, message: &(impl ToString + ?Sized)) -> CheckError {
    CheckError {
        path: path.into(),
        message: message.to_string(),
    }
}

#[allow(clippy::too_many_lines)] // One pass over the existing operation language.
fn prepare_one(
    base: &Deck,
    raw: RawSuggestion,
    index: usize,
    resolver: Option<&crate::design::Resolver<'_>>,
) -> Result<Suggestion, CheckError> {
    if raw.why.chars().count() > 200 || raw.why.contains(['\r', '\n']) {
        return Err(failure(".why", "expected one line, at most 200 characters"));
    }
    if raw.ops.len() > 40 {
        return Err(failure(".ops", "at most 40 operations"));
    }
    let mut reserved = reserved_ids(&serde_json::to_value(base).expect("deck JSON"));
    for value in &raw.ops {
        if value["op"] == "add_slide" {
            reserve_id(value, &mut reserved);
            reserved.extend(reserved_ids(&json!({"slides": [value["slide"]]})));
        } else if value["op"] == "insert_node" {
            reserved.extend(reserved_ids(
                &json!({"slides": [{"children": [value["node"]]}]}),
            ));
        } else if value["op"] == "wrap" {
            reserve_id(value, &mut reserved);
        }
    }
    let mut used = reserved_ids(&serde_json::to_value(base).expect("deck JSON"));
    let mut ops = Vec::new();
    let mut layout_base = base.clone();
    for (j, mut value) in raw.ops.into_iter().enumerate() {
        let path = format!(".ops[{j}]");
        super::ops::resolve_layout_request(&layout_base, &mut value, resolver)
            .map_err(|e| failure(&path, &e))?;
        if value["op"] == "apply_design" {
            layout_base.design = value["design"]
                .as_str()
                .map(str::to_string)
                .or(layout_base.design);
        }
        if value["op"] == "update_node" {
            let id = value["id"].as_str().unwrap_or_default();
            let node = base.node(id).ok_or_else(|| {
                failure(
                    &format!("{path}.id"),
                    &format!("no node {id:?} in the deck"),
                )
            })?;
            value["base_hash"] = json!(node.hash());
        }
        if value["op"] == "add_slide" {
            insertion_tree(
                &mut value["slide"],
                &format!("s_suggest_{index}_{j}"),
                &reserved,
                &mut used,
            )
            .map_err(|(p, e)| failure(&format!("{path}.slide{p}.id"), e))?;
        } else if value["op"] == "insert_node" {
            insertion_tree(
                &mut value["node"],
                &format!("n_suggest_{index}_{j}"),
                &reserved,
                &mut used,
            )
            .map_err(|(p, e)| failure(&format!("{path}.node{p}.id"), e))?;
        } else if value["op"] == "wrap" {
            insertion_id(
                &mut value,
                &format!("f_suggest_{index}_{j}"),
                &reserved,
                &mut used,
            )
            .map_err(|e| failure(&format!("{path}.id"), e))?;
        }
        ops.push(serde_json::from_value::<Op>(value).map_err(|e| failure(&path, &e))?);
    }
    let apply_resolved = |ops: &[Op]| {
        if resolver.is_some() {
            super::ops::apply_with_resolver(base, ops, resolver)
        } else {
            apply(base, ops)
        }
    };
    let next = apply_resolved(&ops).map_err(|error| {
        let j = (0..ops.len())
            .find(|j| apply_resolved(&ops[..=*j]).is_err())
            .unwrap_or(0);
        failure(&format!(".ops[{j}]"), &error)
    })?;
    let image_sources = |deck: &Deck| -> BTreeSet<String> {
        deck.slides
            .iter()
            .flat_map(Slide::nodes)
            .filter_map(|n| {
                if let Kind::Image { src, .. } = &n.kind {
                    Some(src.clone())
                } else {
                    None
                }
            })
            .collect()
    };
    let sources = image_sources(base);
    if image_sources(&next)
        .iter()
        .any(|src| !sources.contains(src))
    {
        return Err(failure(".ops", "image src must already occur in the deck"));
    }
    Ok(Suggestion {
        id: uuid::Uuid::new_v4().to_string(),
        why: raw.why,
        ops,
    })
}

pub(super) fn insertion_id(
    value: &mut Value,
    stem: &str,
    reserved: &BTreeSet<String>,
    used: &mut BTreeSet<String>,
) -> Result<(), &'static str> {
    if let Some(id) = value["id"].as_str().filter(|id| !id.is_empty()) {
        if !used.insert(id.into()) {
            return Err("id already exists");
        }
    } else {
        fill_id(value, stem, reserved, used);
    }
    Ok(())
}

/// Give a slide or node, and every node under it, an id. On a clash the
/// error carries the JSON path of the node, such as `.children[1]`.
pub(super) fn insertion_tree(
    value: &mut Value,
    stem: &str,
    reserved: &BTreeSet<String>,
    used: &mut BTreeSet<String>,
) -> Result<(), (String, &'static str)> {
    insertion_id(value, stem, reserved, used).map_err(|e| (String::new(), e))?;
    if let Some(children) = value.get_mut("children").and_then(Value::as_array_mut) {
        for (k, child) in children.iter_mut().enumerate() {
            insertion_tree(child, &format!("{stem}_{k}"), reserved, used)
                .map_err(|(p, e)| (format!(".children[{k}]{p}"), e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
