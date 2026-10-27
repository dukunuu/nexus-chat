//! Direct edits: the model changing a deck on the author's behalf.
//!
//! These are validated like [`super::suggest::prepare`] (ids and `base_hash`
//! are filled in, every operation is checked against the deck as the earlier
//! operations left it) but without the rules that exist for comment-attached
//! suggestions, which an author reviews before applying: a direct edit may
//! carry up to [`MAX_OPS`] operations and may introduce new images.
//!
//! Errors are collected for every operation rather than stopping at the
//! first, each located as `ops[3].id`.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::ops::{apply_step, apply_step_with_resolver, resolve_layout_request, validate_deck};
use super::suggest::{insertion_id, insertion_tree};
use super::validate::{reserve_id, reserved_ids};
use super::{CheckError, Deck, Kind, Op};

/// Operations in one direct edit.
pub const MAX_OPS: usize = 500;
/// Located errors reported at once.
const MAX_ERRORS: usize = 12;

/// A checked direct edit.
pub struct Prepared {
    /// The operations as applied: hashes and insertion ids filled in, and
    /// `children` patches expanded into deletes and inserts.
    pub ops: Vec<Op>,
    /// The deck after the edit, already validated.
    pub next: Deck,
    /// Repairs made to the input, such as replaced children.
    pub notes: Vec<String>,
}

// Call sites hand over errors and formatted strings alike; taking them by
// value keeps those calls short.
#[allow(clippy::needless_pass_by_value)]
fn failure(path: String, message: impl ToString) -> CheckError {
    CheckError {
        path,
        message: message.to_string(),
    }
}

/// Every explicit id the operations bring, so generated ids avoid them.
fn explicit_ids(ops: &[Value]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for op in ops {
        match op["op"].as_str() {
            Some("add_slide") => {
                if op.get("layout").is_some() {
                    reserve_id(op, &mut ids);
                } else {
                    ids.extend(reserved_ids(&json!({"slides": [op["slide"]]})));
                }
            }
            Some("insert_node") => {
                ids.extend(reserved_ids(
                    &json!({"slides": [{"children": [op["node"]]}]}),
                ));
            }
            Some("wrap") => reserve_id(op, &mut ids),
            Some("update_node" | "update_slide") => {
                if let Some(children) = op["fields"]["children"].as_array() {
                    ids.extend(reserved_ids(&json!({"slides": [{"children": children}]})));
                }
            }
            _ => {}
        }
    }
    ids
}

/// Ids of a slide or node and everything under it, as seen in `deck`.
fn subtree_ids(deck: &Deck, id: &str) -> Vec<String> {
    if let Some(slide) = deck.slide(id) {
        let mut ids = vec![slide.id.clone()];
        ids.extend(slide.nodes().iter().map(|n| n.id.clone()));
        return ids;
    }
    deck.node(id).map_or_else(Vec::new, |node| {
        let mut all = Vec::new();
        node.walk(&mut all);
        all.iter().map(|n| n.id.clone()).collect()
    })
}

/// A `children` patch on `update_node`/`update_slide` replaces the children:
/// the rest of the patch, then a delete per old child and an insert per new
/// one. Other operations pass through.
fn expand(
    working: &Deck,
    mut op: Value,
    index: usize,
    notes: &mut Vec<String>,
) -> Result<Vec<Value>, CheckError> {
    let which = op["op"].as_str().unwrap_or_default().to_string();
    if !matches!(which.as_str(), "update_node" | "update_slide")
        || !op["fields"]
            .as_object()
            .is_some_and(|f| f.contains_key("children"))
    {
        return Ok(vec![op]);
    }
    let id = op["id"].as_str().unwrap_or_default().to_string();
    let path = format!("ops[{index}].fields.children");
    let Value::Array(new_children) = op["fields"]["children"].take() else {
        return Err(failure(path, "children must be an array of nodes"));
    };
    if let Some(fields) = op["fields"].as_object_mut() {
        fields.remove("children");
    }
    let old: Vec<String> = if which == "update_slide" {
        working
            .slide(&id)
            .ok_or_else(|| failure(format!("ops[{index}].id"), format!("no slide {id:?}")))?
            .children
            .iter()
            .map(|n| n.id.clone())
            .collect()
    } else {
        let node = working
            .node(&id)
            .ok_or_else(|| failure(format!("ops[{index}].id"), format!("no node {id:?}")))?;
        if !matches!(node.kind, Kind::Frame { .. }) {
            return Err(failure(
                path,
                format!(
                    "{id} is a {} node, only a frame has children",
                    node.kind.name()
                ),
            ));
        }
        node.children().iter().map(|n| n.id.clone()).collect()
    };
    notes.push(format!(
        "children of {id} replaced: {} removed, {} inserted (fields.children is a shortcut for delete_node and insert_node)",
        old.len(),
        new_children.len()
    ));
    let mut out = Vec::new();
    if op["fields"].as_object().is_some_and(|f| !f.is_empty()) {
        out.push(op);
    }
    for child in old {
        out.push(json!({"op": "delete_node", "id": child}));
    }
    for (k, child) in new_children.into_iter().enumerate() {
        out.push(json!({"op": "insert_node", "parent": id, "index": k, "node": child}));
    }
    Ok(out)
}

/// Check and apply `raw` to `base` for a direct edit.
///
/// # Errors
/// Every located problem found, or the whole-deck check failures of the
/// result. Nothing is written either way.
pub fn prepare(base: &Deck, raw: Vec<Value>) -> Result<Prepared, Vec<CheckError>> {
    prepare_with_resolver(base, raw, None)
}

/// Prepare with caller-owned design lookup; layout requests become concrete
/// slides before the existing insertion/validation machinery runs.
pub fn prepare_with_resolver(
    base: &Deck,
    raw: Vec<Value>,
    resolver: Option<&crate::design::Resolver<'_>>,
) -> Result<Prepared, Vec<CheckError>> {
    if raw.is_empty() || raw.len() > MAX_OPS {
        return Err(vec![failure(
            "ops".into(),
            format!("expected 1 to {MAX_OPS} operations, got {}", raw.len()),
        )]);
    }
    let mut reserved = reserved_ids(&serde_json::to_value(base).expect("deck JSON"));
    let mut used = reserved.clone();
    reserved.extend(explicit_ids(&raw));
    let mut working = base.clone();
    let mut ops = Vec::new();
    let mut errors = Vec::new();
    let mut notes = Vec::new();
    for (j, value) in raw.into_iter().enumerate() {
        if errors.len() >= MAX_ERRORS {
            break;
        }
        let expanded = match expand(&working, value, j, &mut notes) {
            Ok(expanded) => expanded,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        for mut value in expanded {
            match one(&mut working, &mut value, j, &reserved, &mut used, resolver) {
                Ok(op) => ops.push(op),
                Err(error) => errors.push(error),
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    match validate_deck(&working) {
        Ok(next) => Ok(Prepared { ops, next, notes }),
        Err(errors) => Err(errors),
    }
}

/// Fill, parse and apply one operation to the working copy.
fn one(
    working: &mut Deck,
    value: &mut Value,
    j: usize,
    reserved: &BTreeSet<String>,
    used: &mut BTreeSet<String>,
    resolver: Option<&crate::design::Resolver<'_>>,
) -> Result<Op, CheckError> {
    let path = format!("ops[{j}]");
    resolve_layout_request(working, value, resolver).map_err(|e| failure(path.clone(), e))?;
    let kind = value["op"].as_str().unwrap_or_default().to_string();
    match kind.as_str() {
        "update_node" => {
            let id = value["id"].as_str().unwrap_or_default();
            let node = working.node(id).ok_or_else(|| {
                failure(
                    format!("{path}.id"),
                    format!("no node {id:?} in the deck (read the slide with slides:[…] for current ids)"),
                )
            })?;
            value["base_hash"] = json!(node.hash());
        }
        "add_slide" => {
            insertion_tree(&mut value["slide"], &format!("s_new_{j}"), reserved, used)
                .map_err(|(p, e)| failure(format!("{path}.slide{p}.id"), e))?;
        }
        "insert_node" => {
            insertion_tree(&mut value["node"], &format!("n_new_{j}"), reserved, used)
                .map_err(|(p, e)| failure(format!("{path}.node{p}.id"), e))?;
        }
        "wrap" => {
            insertion_id(value, &format!("f_new_{j}"), reserved, used)
                .map_err(|e| failure(format!("{path}.id"), e))?;
        }
        "delete_node" | "delete_slide" => {
            let id = value["id"].as_str().unwrap_or_default().to_string();
            for gone in subtree_ids(working, &id) {
                used.remove(&gone);
            }
        }
        _ => {}
    }
    let op = serde_json::from_value::<Op>(value.clone()).map_err(|e| failure(path.clone(), e))?;
    if resolver.is_some() {
        apply_step_with_resolver(working, &op, resolver)
    } else {
        apply_step(working, &op)
    }
    .map_err(|e| failure(path, format!("{e:#}")))?;
    Ok(op)
}

#[cfg(test)]
mod tests;
