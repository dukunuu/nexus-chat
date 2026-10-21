//! Independently validated edit proposals shared by the feedback flow.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CheckError, Content, Deck, Op, apply};

/// Slides and (slide, block) pairs; empty means the whole deck.
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
                .any(|s| s.id == *slide && s.blocks.iter().any(|b| b.id == *block))
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
    let mut valid = Vec::new();
    let mut errors = Vec::new();
    if raw.is_empty() || raw.len() > 5 {
        super::issue(&mut errors, "suggestions", "expected 1–5 suggestions");
        return (valid, errors);
    }
    for (i, raw) in raw.into_iter().enumerate() {
        let path = format!("suggestions[{i}]");
        match prepare_one(base, raw, i) {
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

fn prepare_one(base: &Deck, raw: RawSuggestion, index: usize) -> Result<Suggestion, CheckError> {
    if raw.why.chars().count() > 200 || raw.why.contains(['\r', '\n']) {
        return Err(failure(".why", "expected one line, at most 200 characters"));
    }
    if raw.ops.len() > 40 {
        return Err(failure(".ops", "at most 40 operations"));
    }
    let mut reserved = super::reserved_ids(&serde_json::to_value(base).expect("deck JSON"));
    for value in &raw.ops {
        if value["op"] == "add_slide" {
            let wrapper = json!({"slides":[value["slide"]]});
            reserved.extend(super::reserved_ids(&wrapper));
        } else if value["op"] == "insert_block" {
            super::reserve_id(&value["block"], &mut reserved);
        }
    }
    let mut used = super::reserved_ids(&serde_json::to_value(base).expect("deck JSON"));
    let mut ops = Vec::new();
    for (j, mut value) in raw.ops.into_iter().enumerate() {
        let path = format!(".ops[{j}]");
        if value["op"] == "update_block" {
            let slide = value["slide"].as_str().unwrap_or_default();
            let id = value["id"].as_str().unwrap_or_default();
            let block = base
                .slides
                .iter()
                .find(|s| s.id == slide)
                .and_then(|s| s.blocks.iter().find(|b| b.id == id))
                .ok_or_else(|| {
                    failure(
                        &format!("{path}.id"),
                        &format!("no block {id:?} on slide {slide:?}"),
                    )
                })?;
            value["base_hash"] = json!(block.hash());
        }
        if value["op"] == "add_slide" {
            insertion_id(
                &mut value["slide"],
                &format!("s_suggest_{index}_{j}"),
                &reserved,
                &mut used,
            )
            .map_err(|e| failure(&format!("{path}.slide.id"), e))?;
            if let Some(blocks) = value
                .get_mut("slide")
                .and_then(|slide| slide.get_mut("blocks"))
                .and_then(Value::as_array_mut)
            {
                for (k, block) in blocks.iter_mut().enumerate() {
                    insertion_id(
                        block,
                        &format!("b_suggest_{index}_{j}_{k}"),
                        &reserved,
                        &mut used,
                    )
                    .map_err(|e| failure(&format!("{path}.slide.blocks[{k}].id"), e))?;
                }
            }
        } else if value["op"] == "insert_block" {
            insertion_id(
                &mut value["block"],
                &format!("b_suggest_{index}_{j}"),
                &reserved,
                &mut used,
            )
            .map_err(|e| failure(&format!("{path}.block.id"), e))?;
        }
        ops.push(serde_json::from_value::<Op>(value).map_err(|e| failure(&path, &e))?);
    }
    let next = apply(base, &ops).map_err(|error| {
        let j = (0..ops.len())
            .find(|j| apply(base, &ops[..=*j]).is_err())
            .unwrap_or(0);
        failure(&format!(".ops[{j}]"), &error)
    })?;
    let sources: BTreeSet<_> = base
        .slides
        .iter()
        .flat_map(|s| &s.blocks)
        .filter_map(|b| {
            if let Content::Image { src, .. } = &b.content {
                Some(src)
            } else {
                None
            }
        })
        .collect();
    for block in next.slides.iter().flat_map(|s| &s.blocks) {
        if let Content::Image { src, .. } = &block.content
            && !sources.contains(src)
        {
            return Err(failure(".ops", "image src must already occur in the deck"));
        }
    }
    Ok(Suggestion {
        id: uuid::Uuid::new_v4().to_string(),
        why: raw.why,
        ops,
    })
}

fn insertion_id(
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
        super::fill_id(value, stem, reserved, used);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
