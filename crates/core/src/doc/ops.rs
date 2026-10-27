//! Ordered atomic operations. A missing `after` appends. List items use
//! one-based dotted paths. `occurrence` is one-based; absent replaces all exact
//! matches in the addressed source string, not its rendered plain text.
use super::model::{Block, Doc, Kind, ListItem};
use crate::design::{Resolver, resolve};
use anyhow::{Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Document edit language shared by direct edits and suggestions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    InsertBlocks {
        after: Option<String>,
        blocks: Vec<Block>,
    },
    InsertMarkdown {
        after: Option<String>,
        markdown: String,
    },
    UpdateBlock {
        id: String,
        patch: Value,
    },
    DeleteBlocks {
        ids: Vec<String>,
    },
    MoveBlocks {
        ids: Vec<String>,
        after: Option<String>,
    },
    ReplaceText {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
        find: String,
        replace: String,
        occurrence: Option<usize>,
    },
    SetPage {
        patch: Value,
    },
    SetTitle {
        title: String,
    },
    ApplyDesign {
        design: Option<String>,
    },
}

pub(crate) fn merge_patch(target: &mut Value, patch: &Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = serde_json::json!({});
        }
        let target = target.as_object_mut().expect("object");
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch.clone();
    }
}

/// Apply atomically with the built-in design resolver.
pub fn apply(base: &Doc, ops: &[Op]) -> Result<Doc> {
    apply_with_resolver(base, ops, None)
}

/// Apply and validate each step on a copy; no caller state or file is changed.
pub fn apply_with_resolver(base: &Doc, ops: &[Op], resolver: Option<&Resolver<'_>>) -> Result<Doc> {
    let mut working = base.clone();
    for (index, op) in ops.iter().enumerate() {
        step(&mut working, op, resolver).map_err(|e| anyhow!("ops[{index}].{e}"))?;
        working = Doc::check(&serde_json::to_string(&working)?).map_err(|errors| {
            anyhow!(
                "{}",
                errors
                    .iter()
                    .map(|e| format!(
                        "ops[{index}]{}: {}; fix: correct this field before retrying",
                        e.path.trim_start_matches('$'),
                        e.message
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    }
    Ok(working)
}
fn index(doc: &Doc, id: &str) -> Result<usize> {
    doc.blocks
        .iter()
        .position(|b| b.id == id)
        .ok_or_else(|| anyhow!("id: no block {id:?}; fix: read the outline for current block ids"))
}
fn after_index(doc: &Doc, after: Option<&str>) -> Result<usize> {
    after.map_or(Ok(doc.blocks.len()), |id| index(doc, id).map(|i| i + 1))
}
fn selection(doc: &Doc, ids: &[String]) -> Result<()> {
    ensure!(
        !ids.is_empty(),
        "ids: expected block ids; fix: supply at least one id from the outline"
    );
    let mut used = BTreeSet::new();
    for id in ids {
        index(doc, id)?;
        ensure!(
            used.insert(id),
            "ids: duplicate {id:?}; fix: list each block once"
        );
    }
    Ok(())
}
fn insert(doc: &mut Doc, after: Option<&str>, blocks: &[Block]) -> Result<()> {
    let at = after_index(doc, after)?;
    let mut ids: BTreeSet<&str> = doc.blocks.iter().map(|b| b.id.as_str()).collect();
    for block in blocks {
        ensure!(
            block.id.is_empty() || ids.insert(&block.id),
            "blocks.id: id {:?} already exists; fix: omit insertion ids or use unused ids",
            block.id
        );
    }
    doc.blocks.splice(at..at, blocks.iter().cloned());
    Ok(())
}
fn item_text<'a>(items: &'a mut [ListItem], path: &str) -> Result<&'a mut String> {
    let (first, rest) = path
        .split_once('.')
        .map_or((path, None), |(a, b)| (a, Some(b)));
    let position = first.parse::<usize>().ok().and_then(|n| n.checked_sub(1));
    let item = position.and_then(|i| items.get_mut(i)).ok_or_else(|| {
        anyhow!("item: no list item {path:?}; fix: use a one-based dotted path such as 2.1")
    })?;
    if let Some(rest) = rest {
        item_text(&mut item.children, rest)
    } else {
        Ok(&mut item.text)
    }
}
fn replace_text(
    doc: &mut Doc,
    id: &str,
    item: Option<&str>,
    find: &str,
    replace: &str,
    occurrence: Option<usize>,
) -> Result<()> {
    ensure!(
        !find.is_empty(),
        "find: cannot be empty; fix: copy an exact source substring"
    );
    let at = index(doc, id)?;
    let block = &mut doc.blocks[at];
    let text = if let Some(item) = item {
        let Kind::List { items, .. } = &mut block.kind else {
            bail!("item: {id} is not a list; fix: omit item for a text block");
        };
        item_text(items, item)?
    } else {
        block.text_mut().ok_or_else(|| anyhow!("id: {id} has no source text; fix: for a list supply item, otherwise use update_block"))?
    };
    let found = text.match_indices(find).map(|(i, _)| i).collect::<Vec<_>>();
    ensure!(
        !found.is_empty(),
        "find: {find:?} not in {id}; fix: copy an exact substring from the block's source text"
    );
    if let Some(n) = occurrence {
        let at = n.checked_sub(1).and_then(|n| found.get(n)).ok_or_else(|| {
            anyhow!(
                "occurrence: out of range; fix: use 1 to {} or omit to replace all",
                found.len()
            )
        })?;
        text.replace_range(*at..*at + find.len(), replace);
    } else {
        *text = text.replace(find, replace);
    }
    Ok(())
}
#[allow(clippy::too_many_lines)]
fn step(doc: &mut Doc, op: &Op, resolver: Option<&Resolver<'_>>) -> Result<()> {
    match op {
        Op::InsertBlocks { after, blocks } => insert(doc, after.as_deref(), blocks)?,
        Op::InsertMarkdown { after, markdown } => {
            let parsed = super::markdown::parse_document(markdown, false);
            insert(doc, after.as_deref(), &parsed.blocks)?;
            doc.footnotes.extend(parsed.footnotes);
        }
        Op::UpdateBlock { id, patch } => {
            ensure!(
                patch.is_object(),
                "patch: expected object; fix: supply fields to merge"
            );
            ensure!(
                patch.get("id").is_none() && patch.get("kind").is_none(),
                "patch: id and kind are immutable; fix: delete and insert to change kind"
            );
            let at = index(doc, id)?;
            let mut value = serde_json::to_value(&doc.blocks[at])?;
            merge_patch(&mut value, patch);
            doc.blocks[at] = serde_json::from_value(value)
                .map_err(|e| anyhow!("patch: {e}; fix: use fields for this block kind"))?;
        }
        Op::DeleteBlocks { ids } => {
            selection(doc, ids)?;
            doc.blocks.retain(|b| !ids.contains(&b.id));
        }
        Op::MoveBlocks { ids, after } => {
            selection(doc, ids)?;
            ensure!(
                !after.as_ref().is_some_and(|id| ids.contains(id)),
                "after: destination is a moved block; fix: choose an unmoved block or omit after to append"
            );
            if let Some(after) = after {
                index(doc, after)?;
            }
            let moved = doc
                .blocks
                .iter()
                .filter(|b| ids.contains(&b.id))
                .cloned()
                .collect::<Vec<_>>();
            doc.blocks.retain(|b| !ids.contains(&b.id));
            let at = after_index(doc, after.as_deref())?;
            doc.blocks.splice(at..at, moved);
        }
        Op::ReplaceText {
            id,
            item,
            find,
            replace,
            occurrence,
        } => replace_text(doc, id, item.as_deref(), find, replace, *occurrence)?,
        Op::SetPage { patch } => {
            ensure!(
                patch.is_object(),
                "patch: expected object; fix: supply page fields to merge"
            );
            let mut value = serde_json::to_value(&doc.page)?;
            merge_patch(&mut value, patch);
            doc.page = serde_json::from_value(value)
                .map_err(|e| anyhow!("patch: {e}; fix: use size, margins_pt, header or footer"))?;
        }
        Op::SetTitle { title } => doc.title.clone_from(title),
        Op::ApplyDesign { design } => {
            let slug = design
                .as_deref()
                .or(doc.design.as_deref())
                .unwrap_or("nexus")
                .to_string();
            let design = resolve(&slug, resolver)
                .map_err(|e| anyhow!("design: {e}; fix: choose a stored design or nexus"))?;
            doc.theme = super::Theme::from_design(&design);
            doc.page = design.doc.page.clone();
            doc.design = Some(slug);
            doc.design_rev = Some(design.revision());
        }
    }
    Ok(())
}
