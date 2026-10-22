//! The editing language: atomic transactions on a copy of the deck.
//!
//! Nodes are addressed by id alone (ids are unique across a deck). `parent`
//! names a slide or a frame. `update_node` and `update_slide` take a JSON
//! merge patch: objects merge key by key, `null` removes a key, anything else
//! replaces.

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::layout::{Rect, layout_map};
use super::model::{
    AutoLayout, Deck, Dim, Font, Kind, Mode, Node, Position, Slide, Theme, default_text_styles,
};

/// The complete editing language. Slide indices in `move_slide` are zero-based.
/// A missing `after` appends; a missing `index` appends to the parent.
// Operations are short-lived values (parsed, applied, dropped), so the size
// gap between a whole `Slide` or `Node` payload and a rename is harmless.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    AddSlide {
        after: Option<String>,
        slide: Slide,
    },
    DeleteSlide {
        id: String,
    },
    MoveSlide {
        id: String,
        to: usize,
    },
    SetNotes {
        slide: String,
        notes: String,
    },
    /// Change slide fields (`name`, `fill`, `transition`, `layout`) by merge patch.
    UpdateSlide {
        id: String,
        fields: Map<String, Value>,
    },
    InsertNode {
        parent: String,
        index: Option<usize>,
        node: Node,
    },
    UpdateNode {
        id: String,
        fields: Map<String, Value>,
        base_hash: String,
    },
    DeleteNode {
        id: String,
    },
    MoveNode {
        id: String,
        parent: String,
        index: Option<usize>,
    },
    /// Wrap sibling nodes in a new frame with this auto-layout.
    Wrap {
        ids: Vec<String>,
        layout: AutoLayout,
        /// The new frame's id; generated when absent.
        id: Option<String>,
    },
    /// Replace a frame by its children.
    Unwrap {
        id: String,
    },
    /// Set the auto-layout of a slide or frame, converting children's
    /// positions the way Figma does.
    SetLayout {
        target: String,
        layout: AutoLayout,
    },
    /// Merge `font` into the named text style, creating it from `body`.
    SetTextStyle {
        name: String,
        font: Font,
    },
    SetTheme {
        theme: Theme,
    },
}

/// Apply an entire transaction on a copy. Any invalid or stale operation
/// discards the copy; the original deck is never changed.
pub fn apply(deck: &Deck, ops: &[Op]) -> Result<Deck> {
    let mut next = deck.clone();
    for op in ops {
        step(deck, &mut next, op)?;
    }
    match Deck::check(&serde_json::to_string(&next)?) {
        Ok(deck) => Ok(deck),
        Err(errors) => bail!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

// -- tree helpers ----------------------------------------------------------------

fn slide_index(deck: &Deck, id: &str) -> Result<usize> {
    deck.slides
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| anyhow!("unknown slide {id}"))
}

fn find_mut<'a>(nodes: &'a mut [Node], id: &str) -> Option<&'a mut Node> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(kids) = node.children_mut()
            && let Some(found) = find_mut(kids, id)
        {
            return Some(found);
        }
    }
    None
}

fn node_mut<'a>(deck: &'a mut Deck, id: &str) -> Result<&'a mut Node> {
    deck.slides
        .iter_mut()
        .find_map(|s| find_mut(&mut s.children, id))
        .ok_or_else(|| anyhow!("unknown node {id}"))
}

/// The children of a slide or frame.
fn container_mut<'a>(deck: &'a mut Deck, id: &str) -> Result<&'a mut Vec<Node>> {
    if let Some(i) = deck.slides.iter().position(|s| s.id == id) {
        return Ok(&mut deck.slides[i].children);
    }
    let node = node_mut(deck, id)
        .map_err(|_| anyhow!("unknown parent {id}: expected a slide or frame id"))?;
    node.children_mut()
        .ok_or_else(|| anyhow!("{id} is not a frame, so it cannot have children"))
}

fn take(nodes: &mut Vec<Node>, id: &str, parent: &str) -> Option<(Node, String, usize)> {
    if let Some(i) = nodes.iter().position(|n| n.id == id) {
        return Some((nodes.remove(i), parent.into(), i));
    }
    for node in nodes.iter_mut() {
        let own = node.id.clone();
        if let Some(kids) = node.children_mut()
            && let Some(found) = take(kids, id, &own)
        {
            return Some(found);
        }
    }
    None
}

fn take_node(deck: &mut Deck, id: &str) -> Result<(Node, String, usize)> {
    for slide in &mut deck.slides {
        let own = slide.id.clone();
        if let Some(found) = take(&mut slide.children, id, &own) {
            return Ok(found);
        }
    }
    bail!("unknown node {id}")
}

fn contains(node: &Node, id: &str) -> bool {
    node.id == id || node.children().iter().any(|c| contains(c, id))
}

/// The layout of a slide or frame.
fn layout_of(deck: &Deck, id: &str) -> Result<AutoLayout> {
    if let Some(slide) = deck.slides.iter().find(|s| s.id == id) {
        return Ok(slide.layout.clone());
    }
    match deck.node(id).map(|n| &n.kind) {
        Some(Kind::Frame { layout, .. }) => Ok(layout.clone()),
        Some(_) => bail!("{id} is not a frame"),
        None => bail!("unknown parent {id}: expected a slide or frame id"),
    }
}

/// Index of the slide holding `id` (a node or the slide itself).
fn slide_of(deck: &Deck, id: &str) -> Result<usize> {
    deck.slides
        .iter()
        .position(|s| s.id == id || s.nodes().iter().any(|n| n.id == id))
        .ok_or_else(|| anyhow!("unknown node {id}"))
}

/// Absolute rectangles of the slide that holds `id`, plus the slide index.
fn rects(deck: &Deck, id: &str) -> Result<(usize, std::collections::HashMap<String, Rect>)> {
    let index = slide_of(deck, id)?;
    Ok((index, layout_map(deck, &deck.slides[index], None)))
}

fn origin_of(map: &std::collections::HashMap<String, Rect>, parent: &str) -> (f64, f64) {
    map.get(parent).map_or((0.0, 0.0), |r| (r.x, r.y))
}

fn merge_patch(target: &mut Value, patch: &Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = Value::Object(Map::new());
        }
        let map = target.as_object_mut().expect("object");
        for (key, value) in patch {
            if value.is_null() {
                map.remove(key);
            } else {
                merge_patch(map.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch.clone();
    }
}

fn no_structure(fields: &Map<String, Value>, what: &str) -> Result<()> {
    for key in ["id", "kind", "children"] {
        ensure!(
            !fields.contains_key(key),
            match key {
                "id" => format!("{what} ids are immutable"),
                "kind" => format!("a {what}'s kind is immutable; insert a new one"),
                _ => "children change through insert_node, delete_node and move_node".to_string(),
            }
        );
    }
    Ok(())
}

fn unique_ids(deck: &Deck, node: &Node) -> Result<()> {
    let mut all = Vec::new();
    node.walk(&mut all);
    for inner in all {
        ensure!(
            inner.id.is_empty()
                || (deck.node(&inner.id).is_none() && deck.slides.iter().all(|s| s.id != inner.id)),
            "id {} already exists",
            inner.id
        );
    }
    Ok(())
}

fn flow_axis(mode: Mode, rect: &Rect) -> f64 {
    if mode == Mode::Column { rect.y } else { rect.x }
}

// -- operations ------------------------------------------------------------------

fn step(original: &Deck, next: &mut Deck, op: &Op) -> Result<()> {
    match op {
        Op::AddSlide { after, slide } => {
            let index = after.as_ref().map_or(Ok(next.slides.len()), |id| {
                slide_index(next, id).map(|i| i + 1)
            })?;
            next.slides.insert(index, slide.clone());
        }
        Op::DeleteSlide { id } => {
            let index = slide_index(next, id)?;
            next.slides.remove(index);
        }
        Op::MoveSlide { id, to } => {
            ensure!(*to < next.slides.len(), "slide destination out of range");
            let index = slide_index(next, id)?;
            let slide = next.slides.remove(index);
            next.slides.insert(*to, slide);
        }
        Op::SetNotes { slide, notes } => {
            let index = slide_index(next, slide)?;
            next.slides[index].notes.clone_from(notes);
        }
        Op::UpdateSlide { id, fields } => {
            no_structure(fields, "slide")?;
            let index = slide_index(next, id)?;
            let mut value = serde_json::to_value(&next.slides[index])?;
            for (key, patch) in fields {
                if patch.is_null() {
                    value.as_object_mut().expect("slide").remove(key);
                } else {
                    merge_patch(&mut value[key], patch);
                }
            }
            next.slides[index] = serde_json::from_value(value)?;
        }
        Op::InsertNode {
            parent,
            index,
            node,
        } => {
            unique_ids(next, node)?;
            let children = container_mut(next, parent)?;
            let at = index.unwrap_or(children.len());
            ensure!(
                at <= children.len(),
                "index {at} is past the end of {parent}"
            );
            children.insert(at, node.clone());
        }
        Op::UpdateNode {
            id,
            fields,
            base_hash,
        } => update_node(original, next, id, fields, base_hash)?,
        Op::DeleteNode { id } => {
            take_node(next, id)?;
        }
        Op::MoveNode { id, parent, index } => move_node(next, id, parent, *index)?,
        Op::Wrap { ids, layout, id } => wrap(next, ids, layout, id.as_deref())?,
        Op::Unwrap { id } => unwrap(next, id)?,
        Op::SetLayout { target, layout } => set_layout(next, target, layout)?,
        Op::SetTextStyle { name, font } => {
            let base = next
                .theme
                .text_styles
                .get(name)
                .or_else(|| next.theme.text_styles.get("body"))
                .cloned()
                .unwrap_or_else(|| default_text_styles().remove("body").unwrap_or_default());
            next.theme
                .text_styles
                .insert(name.clone(), base.merged(font));
        }
        Op::SetTheme { theme } => next.theme.clone_from(theme),
    }
    Ok(())
}

fn update_node(
    original: &Deck,
    next: &mut Deck,
    id: &str,
    fields: &Map<String, Value>,
    base_hash: &str,
) -> Result<()> {
    no_structure(fields, "node")?;
    // The precondition names the node as the caller last saw it. Earlier
    // operations of the same transaction may already have changed it.
    let seen = original.node(id).map(Node::hash);
    let node = node_mut(next, id)?;
    ensure!(
        seen.as_deref() == Some(base_hash) || node.hash() == base_hash,
        "stale node {id}: base_hash mismatch"
    );
    let mut value = serde_json::to_value(&*node)?;
    for (key, patch) in fields {
        if patch.is_null() {
            value.as_object_mut().expect("node").remove(key);
        } else {
            merge_patch(&mut value[key], patch);
        }
    }
    *node = serde_json::from_value(value)?;
    Ok(())
}

fn move_node(deck: &mut Deck, id: &str, parent: &str, index: Option<usize>) -> Result<()> {
    let (_, map) = rects(deck, id)?;
    let base = rects(deck, parent).map_or((0.0, 0.0), |(_, m)| origin_of(&m, parent));
    let target_layout = layout_of(deck, parent)?;
    let (mut node, old_parent, _) = {
        let node = deck.node(id).context("unknown node")?;
        ensure!(!contains(node, parent), "cannot move {id} into itself");
        take_node(deck, id)?
    };
    let old_mode = layout_of(deck, &old_parent)
        .map(|l| l.mode)
        .unwrap_or_default();
    if target_layout.mode == Mode::None {
        if (node.position == Position::Auto || old_parent != parent)
            && let Some(rect) = map.get(id)
        {
            node.position = Position::Absolute {
                x: rect.x - base.0,
                y: rect.y - base.1,
            };
        }
    } else if old_mode == Mode::None && matches!(node.position, Position::Absolute { .. }) {
        node.position = Position::Auto;
    }
    let children = container_mut(deck, parent)?;
    let at = index.unwrap_or(children.len());
    ensure!(
        at <= children.len(),
        "index {at} is past the end of {parent}"
    );
    children.insert(at, node);
    Ok(())
}

/// The slide or frame that directly holds node `id`, on slide `si`.
fn parent_id(deck: &Deck, si: usize, id: &str) -> String {
    let slide = &deck.slides[si];
    let mut stack: Vec<(&str, &[Node])> = vec![(slide.id.as_str(), &slide.children)];
    while let Some((parent, nodes)) = stack.pop() {
        if nodes.iter().any(|n| n.id == id) {
            return parent.to_string();
        }
        for n in nodes {
            if matches!(n.kind, Kind::Frame { .. }) {
                stack.push((n.id.as_str(), n.children()));
            }
        }
    }
    slide.id.clone()
}

/// An id for a new frame that nothing else uses.
fn free_frame_id(deck: &Deck) -> String {
    (1..10_000_u32)
        .map(|n| format!("frame_{n}"))
        .find(|id| deck.node(id).is_none() && deck.slides.iter().all(|s| s.id != *id))
        .expect("an unused id")
}

/// The box around `taken`, as (left, top, right, bottom), in slide px.
fn bounds_of(
    taken: &[Node],
    map: &std::collections::HashMap<String, Rect>,
) -> (f64, f64, f64, f64) {
    let boxes: Vec<Rect> = taken
        .iter()
        .filter_map(|n| map.get(&n.id).copied())
        .collect();
    if boxes.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    (
        boxes.iter().map(|r| r.x).fold(f64::MAX, f64::min),
        boxes.iter().map(|r| r.y).fold(f64::MAX, f64::min),
        boxes.iter().map(|r| r.x + r.w).fold(f64::MIN, f64::max),
        boxes.iter().map(|r| r.y + r.h).fold(f64::MIN, f64::max),
    )
}

fn wrap(deck: &mut Deck, ids: &[String], layout: &AutoLayout, new_id: Option<&str>) -> Result<()> {
    ensure!(!ids.is_empty(), "wrap needs at least one node id");
    let (si, map) = rects(deck, &ids[0])?;
    let parent = parent_id(deck, si, &ids[0]);
    let parent_layout = layout_of(deck, &parent)?;
    let frame_id = new_id.map_or_else(|| free_frame_id(deck), str::to_string);
    ensure!(
        deck.node(&frame_id).is_none() && deck.slides.iter().all(|s| s.id != frame_id),
        "id {frame_id} already exists"
    );
    let children = container_mut(deck, &parent)?;
    let mut indices = Vec::new();
    for id in ids {
        let i = children
            .iter()
            .position(|n| n.id == *id)
            .ok_or_else(|| anyhow!("{id} is not a child of {parent}; wrap siblings only"))?;
        ensure!(!indices.contains(&i), "{id} is listed twice");
        indices.push(i);
    }
    indices.sort_unstable();
    let first_auto = children[indices[0]].position == Position::Auto;
    let mut taken: Vec<Node> = indices.iter().rev().map(|i| children.remove(*i)).collect();
    taken.reverse();
    let (left, top, right, bottom) = bounds_of(&taken, &map);
    if layout.mode == Mode::None {
        for node in &mut taken {
            if let Some(r) = map.get(&node.id) {
                node.position = Position::Absolute {
                    x: r.x - left,
                    y: r.y - top,
                };
            }
        }
    } else {
        taken.sort_by(|a, b| {
            let key = |n: &Node| map.get(&n.id).map_or(0.0, |r| flow_axis(layout.mode, r));
            key(a).total_cmp(&key(b))
        });
        for node in &mut taken {
            node.position = Position::Auto;
        }
    }
    let base = origin_of(&map, &parent);
    let mut frame = Node::new(
        &frame_id,
        Kind::Frame {
            layout: layout.clone(),
            fill: None,
            stroke: None,
            radius: 0.0,
            clip: false,
            children: taken,
        },
    );
    if !(parent_layout.mode != Mode::None && first_auto) {
        frame.position = Position::Absolute {
            x: left - base.0,
            y: top - base.1,
        };
    }
    if layout.mode == Mode::None {
        frame.sizing.w = Dim::Fixed((right - left).max(0.0));
        frame.sizing.h = Dim::Fixed((bottom - top).max(0.0));
    }
    container_mut(deck, &parent)?.insert(indices[0], frame);
    Ok(())
}

fn unwrap(deck: &mut Deck, id: &str) -> Result<()> {
    let (_, map) = rects(deck, id)?;
    let node = deck.node(id).context("unknown node")?;
    ensure!(
        matches!(node.kind, Kind::Frame { .. }),
        "{id} is not a frame; only frames can be unwrapped"
    );
    let (frame, parent, index) = take_node(deck, id)?;
    let parent_mode = layout_of(deck, &parent)?.mode;
    let base = origin_of(&map, &parent);
    let Kind::Frame { children, .. } = frame.kind else {
        unreachable!("checked frame");
    };
    let mut lifted = Vec::with_capacity(children.len());
    for mut child in children {
        if !(child.position == Position::Auto && parent_mode != Mode::None)
            && let Some(r) = map.get(&child.id)
        {
            child.position = Position::Absolute {
                x: r.x - base.0,
                y: r.y - base.1,
            };
        }
        lifted.push(child);
    }
    let siblings = container_mut(deck, &parent)?;
    for (offset, child) in lifted.into_iter().enumerate() {
        siblings.insert(index + offset, child);
    }
    Ok(())
}

fn set_layout(deck: &mut Deck, target: &str, layout: &AutoLayout) -> Result<()> {
    let (_, map) = rects(deck, target)?;
    let old = layout_of(deck, target)?;
    let base = origin_of(&map, target);
    let own = map.get(target).copied();
    if let Some(i) = deck.slides.iter().position(|s| s.id == target) {
        let slide = &mut deck.slides[i];
        slide.layout.clone_from(layout);
        convert_children(&mut slide.children, &old, layout, &map, base);
        return Ok(());
    }
    let node = node_mut(deck, target)?;
    let sizing = node.sizing.clone();
    let Kind::Frame {
        layout: current,
        children,
        ..
    } = &mut node.kind
    else {
        bail!("{target} is not a slide or frame");
    };
    current.clone_from(layout);
    convert_children(children, &old, layout, &map, base);
    if layout.mode == Mode::None
        && let Some(rect) = own
    {
        // Without auto-layout a frame cannot hug; keep the size it had.
        if sizing.w == Dim::Hug {
            node.sizing.w = Dim::Fixed(rect.w);
        }
        if sizing.h == Dim::Hug {
            node.sizing.h = Dim::Fixed(rect.h);
        }
    }
    Ok(())
}

fn convert_children(
    children: &mut Vec<Node>,
    old: &AutoLayout,
    new: &AutoLayout,
    map: &std::collections::HashMap<String, Rect>,
    base: (f64, f64),
) {
    if old.mode == Mode::None && new.mode != Mode::None {
        children.sort_by(|a, b| {
            let key = |n: &Node| map.get(&n.id).map_or(0.0, |r| flow_axis(new.mode, r));
            key(a).total_cmp(&key(b))
        });
        for child in children {
            child.position = Position::Auto;
        }
    } else if old.mode != Mode::None && new.mode == Mode::None {
        for child in children {
            if let Some(r) = map.get(&child.id) {
                child.position = Position::Absolute {
                    x: r.x - base.0,
                    y: r.y - base.1,
                };
            }
        }
    }
}
