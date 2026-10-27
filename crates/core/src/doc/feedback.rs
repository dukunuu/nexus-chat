//! Document anchors and proposal guards on the shared comments sidecar.
use super::{Doc, Op, suggest::Suggestion};
pub use crate::deck::feedback::{EditStatus, Role, Severity, now, sidecar};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Plain-text, character-based half-open range (never UTF-8 byte offsets).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

/// `block: null` is a document-level thread after its block disappears.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub block: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    pub quote: String,
}
impl Anchor {
    /// Reject invalid block/range selections when a comment is first created.
    pub fn check(&self, doc: &Doc) -> Result<(), String> {
        let Some(id) = &self.block else {
            return if self.range.is_none() {
                Ok(())
            } else {
                Err("document anchor cannot have a range".into())
            };
        };
        let block = doc.block(id).ok_or("unknown block")?;
        if let Some(range) = self.range {
            let text = block.plain_text();
            if range.start >= range.end
                || range.end > text.chars().count()
                || text
                    .chars()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .collect::<String>()
                    != self.quote
            {
                return Err("range must select quote in block plain text".into());
            }
        }
        Ok(())
    }
    /// Re-find the quote within its block, otherwise fall back to the block,
    /// or the document when the block disappeared. No cross-block guessing.
    pub fn reanchor(&mut self, doc: &Doc) {
        let Some(block) = self.block.as_deref().and_then(|id| doc.block(id)) else {
            self.block = None;
            self.range = None;
            return;
        };
        self.range = if self.quote.is_empty() {
            None
        } else {
            let text = block.plain_text();
            text.find(&self.quote).map(|byte| {
                let start = text[..byte].chars().count();
                Range {
                    start,
                    end: start + self.quote.chars().count(),
                }
            })
        };
    }
}

/// A guarded doc edit with the deck thread lifecycle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    #[serde(flatten)]
    pub proposal: Suggestion,
    pub status: EditStatus,
    pub guards: BTreeMap<String, Value>,
}
pub type Message = crate::deck::feedback::Message<Edit>;
pub type Thread = crate::deck::feedback::Thread<Anchor, Edit>;

fn snapshot(doc: &Doc, key: &str) -> Value {
    match key {
        "@order" => json!(doc.blocks.iter().map(|b| &b.id).collect::<Vec<_>>()),
        "@page" => json!(doc.page),
        "@theme" => json!([doc.theme, doc.design, doc.design_rev]),
        "@title" => json!(doc.title),
        _ => json!(doc.block(key)),
    }
}
impl Edit {
    /// Capture only affected blocks and document fields, not unrelated text.
    #[must_use]
    pub fn new(base: &Doc, proposal: Suggestion, anchor: &Anchor) -> Self {
        let mut keys = Vec::new();
        if let Some(block) = &anchor.block {
            keys.push(block.clone());
        }
        for op in &proposal.ops {
            match op {
                Op::UpdateBlock { id, .. } | Op::ReplaceText { id, .. } => keys.push(id.clone()),
                Op::DeleteBlocks { ids } | Op::MoveBlocks { ids, .. } => {
                    keys.extend(ids.iter().cloned());
                    keys.push("@order".into());
                }
                Op::InsertBlocks { .. } | Op::InsertMarkdown { .. } => keys.push("@order".into()),
                Op::SetPage { .. } => keys.push("@page".into()),
                Op::SetTitle { .. } => keys.push("@title".into()),
                Op::ApplyDesign { .. } => {
                    keys.push("@theme".into());
                    keys.push("@page".into());
                }
            }
        }
        Self {
            guards: keys
                .into_iter()
                .map(|k| {
                    let v = snapshot(base, &k);
                    (k, v)
                })
                .collect(),
            proposal,
            status: EditStatus::Pending,
        }
    }
    /// Recheck a proposal after an edit; settled edits stay settled.
    pub fn recheck(&mut self, doc: &Doc) {
        if matches!(self.status, EditStatus::Pending | EditStatus::Outdated) {
            self.status = if self.guards.iter().any(|(k, v)| snapshot(doc, k) != *v)
                || super::apply(doc, &self.proposal.ops).is_err()
            {
                EditStatus::Outdated
            } else {
                EditStatus::Pending
            };
        }
    }
}

/// Load the same thread model as decks, without deck point migration.
pub fn load(path: &Path) -> Result<Vec<Thread>> {
    crate::deck::feedback::load_for(path)
}
/// Save the same atomic comments sidecar as decks.
pub fn save(path: &Path, threads: &[Thread]) -> Result<()> {
    crate::deck::feedback::save_for(path, threads)
}
/// Update anchors and proposal statuses after a successful document edit.
pub fn reanchor(threads: &mut [Thread], doc: &Doc) {
    for thread in threads {
        thread.anchor.reanchor(doc);
        for message in &mut thread.messages {
            for edit in &mut message.edits {
                edit.recheck(doc);
            }
        }
    }
}
