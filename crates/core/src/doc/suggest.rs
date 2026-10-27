//! Reviewable edit proposals, independently checked and bounded.
use super::{Doc, Kind, Op};
use crate::deck::CheckError;
use crate::design::Resolver;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Untrusted proposal.
#[derive(Clone, Debug, Deserialize)]
pub struct RawSuggestion {
    pub why: String,
    pub ops: Vec<Value>,
}
/// Checked proposal, using the same language as direct edits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub why: String,
    pub ops: Vec<Op>,
}

fn sources(doc: &Doc) -> BTreeSet<&str> {
    doc.blocks
        .iter()
        .filter_map(|b| match &b.kind {
            Kind::Image { src, .. } => Some(src.as_str()),
            _ => None,
        })
        .collect()
}

/// Prepare up to five independent suggestions, each at most 40 operations,
/// and reject any image source not already present, including transient ones.
#[must_use]
pub fn prepare(base: &Doc, raw: Vec<RawSuggestion>) -> (Vec<Suggestion>, Vec<CheckError>) {
    prepare_with_resolver(base, raw, None)
}

/// As `prepare`, with caller-owned design lookup.
#[must_use]
pub fn prepare_with_resolver(
    base: &Doc,
    raw: Vec<RawSuggestion>,
    resolver: Option<&Resolver<'_>>,
) -> (Vec<Suggestion>, Vec<CheckError>) {
    let mut valid = Vec::new();
    let mut errors = Vec::new();
    if raw.is_empty() || raw.len() > 5 {
        return (
            valid,
            vec![CheckError {
                path: "suggestions".into(),
                message: "expected 1 to 5 suggestions; fix: split the review".into(),
            }],
        );
    }
    for (i, raw) in raw.into_iter().enumerate() {
        let path = format!("suggestions[{i}]");
        if raw.ops.is_empty()
            || raw.ops.len() > 40
            || raw.why.chars().count() > 200
            || raw.why.contains(['\r', '\n'])
        {
            errors.push(CheckError { path, message: "expected 1 to 40 ops and a one-line reason up to 200 characters; fix: shorten the suggestion".into() });
            continue;
        }
        match super::direct::prepare_with_resolver(base, raw.ops, resolver) {
            Err(found) => errors.extend(found.into_iter().map(|mut e| {
                e.path = format!("{path}.{}", e.path);
                e
            })),
            Ok(prepared) => {
                let mut current = base.clone();
                let mut safe = true;
                let allowed = sources(base);
                for op in &prepared.ops {
                    // Every prefix was already validated by direct::prepare.
                    current =
                        super::apply_with_resolver(&current, std::slice::from_ref(op), resolver)
                            .expect("checked operation");
                    if sources(&current).iter().any(|src| !allowed.contains(src)) {
                        safe = false;
                        break;
                    }
                }
                if safe {
                    valid.push(Suggestion {
                        id: uuid::Uuid::new_v4().to_string(),
                        why: raw.why,
                        ops: prepared.ops,
                    });
                } else {
                    errors.push(CheckError { path: format!("{path}.ops"), message: "new image source in suggestion; fix: use an image src already in the document".into() });
                }
            }
        }
    }
    (valid, errors)
}
