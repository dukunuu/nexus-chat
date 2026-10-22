//! Deck v2: bounded, UI-independent slide data, auto-layout and atomic
//! editing operations. Slides are 1920 x 1080 px frames holding a tree of
//! nodes; version-1 grid decks are migrated on load.

pub mod feedback;
pub mod layout;
pub mod link;
pub mod migrate;
pub mod model;
pub mod ops;
pub mod render;
pub mod suggest;
pub mod text;
pub mod validate;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, anyhow, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use layout::{Rect, layout, layout_at, layout_map};
pub use model::{
    Align, AutoLayout, Chart, Cross, Deck, Dim, Fit, Font, FontFile, Kind, Main, Mode, Node,
    Position, ResolvedFont, Series, Shape, Sizing, Slide, SlideSize, Stroke, Theme, Transform,
    VAlign,
};
pub use ops::{Op, apply};

/// Maximum UTF-8 source size, in bytes.
pub const MAX_SOURCE: usize = 2 * 1024 * 1024;
/// Version-1 canvas width in grid units (used only by migration).
pub const WIDTH: i32 = 32;
/// Version-1 canvas height in grid units (used only by migration).
pub const HEIGHT: i32 = 18;

/// A located validation failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckError {
    pub path: String,
    pub message: String,
}

impl Deck {
    /// SHA-256 of canonical JSON, independent of input object key order.
    #[must_use]
    pub fn revision(&self) -> String {
        hash(self)
    }

    #[must_use]
    pub fn slide(&self, id: &str) -> Option<&Slide> {
        self.slides.iter().find(|s| s.id == id)
    }

    /// A node by id, on any slide.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.slides.iter().find_map(|s| s.node(id))
    }

    /// The slide that holds node `id`.
    #[must_use]
    pub fn slide_of_node(&self, id: &str) -> Option<&Slide> {
        self.slides.iter().find(|s| s.node(id).is_some())
    }

    /// Every node id with the id of its slide.
    #[must_use]
    pub fn node_ids(&self) -> Vec<(String, String)> {
        self.slides
            .iter()
            .flat_map(|s| s.nodes().into_iter().map(|n| (s.id.clone(), n.id.clone())))
            .collect()
    }

    /// A fixed two-slide deck for tests: the migrated version-1 classic deck.
    #[cfg(test)]
    pub(crate) fn classic_fixture(title: &str) -> Self {
        let mut deck = Self::check(include_str!("../tests/fixtures/deck/classic.deck.json"))
            .expect("classic deck fixture");
        deck.title = title.into();
        if let Some(Kind::Text { text, .. }) = deck.slides[0]
            .children
            .first_mut()
            .map(|node| &mut node.kind)
        {
            *text = title.into();
        }
        deck
    }

    /// One title slide and nothing else: what the `deck` tool starts from.
    #[must_use]
    pub fn titled(title: &str) -> Self {
        Self::check(
            &json!({
                "version": 2, "title": title,
                "slides": [{"id": "s1", "layout": title_layout(), "children": [
                    {"id": "b1", "kind": "text", "text": title, "style": "title"}
                ]}]
            })
            .to_string(),
        )
        .expect("titled deck")
    }

    /// A ready-to-edit title and content slide.
    #[must_use]
    pub fn new_from_layouts(title: &str) -> Self {
        Self::check(
            &json!({
                "version": 2, "title": title,
                "slides": [
                    {"id": "s1", "layout": title_layout(), "children": [
                        {"id": "b_kicker", "kind": "text", "text": "A NEW PERSPECTIVE / 01",
                         "style": "caption", "font": {"color": "accent", "letter_spacing": 0.12}},
                        {"id": "b1", "kind": "text", "text": title, "style": "title"},
                        {"id": "b_subtitle", "kind": "text",
                         "text": "A little clarity. A bigger possibility.",
                         "font": {"color": "muted"}}
                    ]},
                    {"id": "s2", "notes": "Chart values are illustrative placeholders, not measured data.",
                     "layout": {"mode": "column", "gap": 56, "padding": [120, 160, 120, 160], "cross": "stretch"},
                     "children": [
                        {"id": "b2", "kind": "text", "text": "Make room for what matters.", "style": "heading"},
                        {"id": "b_row", "kind": "frame", "name": "Content", "sizing": {"h": "fill"},
                         "layout": {"mode": "row", "gap": 96, "cross": "stretch"}, "children": [
                            {"id": "b3", "kind": "text", "sizing": {"w": "fill"},
                             "text": "- Start with the story\n- Make it memorable\n- Leave room to breathe"},
                            {"id": "b_chart", "kind": "chart", "chart": "bar", "sizing": {"w": "fill"},
                             "labels": ["Start", "Build", "Grow"],
                             "series": [{"name": "Illustrative values", "values": [3, 5, 8]}]}
                        ]}
                     ]}
                ]
            })
            .to_string(),
        )
        .expect("starter deck")
    }
}

fn title_layout() -> Value {
    json!({"mode": "column", "gap": 32, "padding": [160, 160, 160, 160], "main": "center", "cross": "stretch"})
}

fn issue(errors: &mut Vec<CheckError>, path: &str, message: &str) {
    errors.push(CheckError {
        path: path.into(),
        message: message.into(),
    });
}

fn is_hex(color: &str) -> bool {
    color.strip_prefix('#').is_some_and(|digits| {
        matches!(digits.len(), 3 | 4 | 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// A relative image path within the deck assets directory. Backslashes and
/// URL escapes are rejected too, so interpretation cannot vary by platform.
#[must_use]
pub fn safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', ':', '%', '\0'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Whether two paths name one file, through symlinks and `..`.
#[must_use]
pub fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// Write `bytes` next to `path` and rename over it, so a reader (studio, the
/// model's tools, a sync) never sees a half-written file. Refuses to write
/// through a symlink.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("missing parent folder"))?;
    std::fs::create_dir_all(parent)?;
    ensure!(!path.is_symlink(), "refusing to overwrite a symlink");
    let temporary = parent.join(format!(".deck-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn hash(value: &impl Serialize) -> String {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(k, v)| (k, sorted(v)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            scalar => scalar,
        }
    }
    let value = sorted(serde_json::to_value(value).expect("deck JSON"));
    let bytes = serde_json::to_vec(&value).expect("canonical JSON");
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

#[cfg(test)]
mod tests;
