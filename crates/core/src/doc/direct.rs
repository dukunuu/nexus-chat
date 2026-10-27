//! Checked direct edits: at most 500 operations, new images permitted.
use super::{Doc, Op};
use crate::deck::CheckError;
use crate::design::Resolver;
use serde_json::Value;

/// A validated transaction; writing is deliberately a caller responsibility.
pub struct Prepared {
    pub ops: Vec<Op>,
    pub next: Doc,
}

/// Prepare with the built-in design.
pub fn prepare(base: &Doc, raw: Vec<Value>) -> Result<Prepared, Vec<CheckError>> {
    prepare_with_resolver(base, raw, None)
}

/// Parse, apply and check against the state left by each earlier operation.
pub fn prepare_with_resolver(
    base: &Doc,
    raw: Vec<Value>,
    resolver: Option<&Resolver<'_>>,
) -> Result<Prepared, Vec<CheckError>> {
    if raw.is_empty() || raw.len() > 500 {
        return Err(vec![CheckError {
            path: "ops".into(),
            message: "expected 1 to 500 operations; fix: split the edit into bounded transactions"
                .into(),
        }]);
    }
    let mut ops = Vec::new();
    for (i, value) in raw.into_iter().enumerate() {
        ops.push(serde_json::from_value::<Op>(value).map_err(|e| {
            vec![CheckError {
                path: format!("ops[{i}]"),
                message: format!("{e}; fix: use the documented operation fields"),
            }]
        })?);
    }
    let next = super::apply_with_resolver(base, &ops, resolver).map_err(|e| {
        let text = e.to_string();
        let (path, message) = text.split_once(": ").unwrap_or(("ops", &text));
        vec![CheckError {
            path: path.into(),
            message: message.into(),
        }]
    })?;
    Ok(Prepared { ops, next })
}
