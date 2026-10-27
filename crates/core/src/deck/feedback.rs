//! Shared, file-backed anchored comment threads. No inference or chat transport.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::suggest::Suggestion;
use super::{Deck, Op};

/// A point on a slide, in slide pixels, optionally attached to a node (the
/// field is still called `block`, as in version 1).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub slide: String,
    pub block: Option<String>,
    pub x: i32,
    pub y: i32,
    /// False only in sidecars written for version-1 grid decks, whose points
    /// were grid cells; [`load`] converts those.
    #[serde(default)]
    pub px: bool,
}

impl Anchor {
    /// Reject dangling anchors rather than silently moving a comment.
    pub fn check(&self, deck: &Deck) -> Result<(), String> {
        let slide = deck
            .slides
            .iter()
            .find(|s| s.id == self.slide)
            .ok_or("unknown slide")?;
        if self
            .block
            .as_ref()
            .is_some_and(|id| slide.node(id).is_none())
        {
            return Err("unknown block on anchored slide".into());
        }
        let size = deck.theme.size;
        if !self.px
            || f64::from(self.x) < 0.0
            || f64::from(self.x) >= size.w
            || f64::from(self.y) < 0.0
            || f64::from(self.y) >= size.h
        {
            return Err("anchor point outside slide".into());
        }
        Ok(())
    }
}

/// Priority is visible on both the pin and thread.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Must,
    Should,
    #[default]
    Consider,
}

/// Authors are never system messages.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    You,
    Model,
}

/// Lifecycle of a suggested edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditStatus {
    Pending,
    Applied,
    Dismissed,
    Outdated,
}

/// An independently applicable proposal, guarded against external edits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    #[serde(flatten)]
    pub proposal: Suggestion,
    pub status: EditStatus,
    pub guards: BTreeMap<String, Value>,
}

/// One reply, optionally carrying an edit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message<E = Edit> {
    pub id: String,
    pub role: Role,
    pub text: String,
    pub created: u64,
    pub edits: Vec<E>,
}

/// The shared comments sidecar is a JSON array of these threads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread<A = Anchor, E = Edit> {
    pub id: String,
    pub anchor: A,
    pub severity: Severity,
    pub created: u64,
    pub resolved: Option<u64>,
    pub messages: Vec<Message<E>>,
}

/// Seconds since epoch, for local comment timestamps.
#[must_use]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Message {
    /// Construct a deck reply, preserving the original default type.
    #[must_use]
    pub fn new(role: Role, text: String, edits: Vec<Edit>) -> Self {
        Self::new_for(role, text, edits)
    }
}

impl<E> Message<E> {
    /// Construct a reply for either document kind.
    #[must_use]
    pub fn new_for(role: Role, text: String, edits: Vec<E>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            role,
            text,
            created: now(),
            edits,
        }
    }
}

fn snapshot(deck: &Deck, key: &str) -> Value {
    if key == "theme" {
        return serde_json::to_value(&deck.theme).expect("theme JSON");
    }
    if key == "order" {
        return serde_json::json!(deck.slides.iter().map(|s| &s.id).collect::<Vec<_>>());
    }
    if let Some(id) = key.strip_prefix("slide:") {
        return serde_json::to_value(deck.slides.iter().find(|s| s.id == id)).expect("slide JSON");
    }
    serde_json::to_value(deck.node(key)).expect("node JSON")
}

impl Edit {
    /// Capture only touched objects, allowing unrelated edits to coexist.
    #[must_use]
    pub fn new(base: &Deck, proposal: Suggestion, anchor: &Anchor) -> Self {
        let mut keys = vec![
            anchor
                .block
                .clone()
                .unwrap_or_else(|| format!("slide:{}", anchor.slide)),
        ];
        let target = |id: &String| {
            if base.slides.iter().any(|s| s.id == *id) {
                format!("slide:{id}")
            } else {
                id.clone()
            }
        };
        for op in &proposal.ops {
            match op {
                Op::UpdateNode { id, .. }
                | Op::DeleteNode { id }
                | Op::Unwrap { id }
                | Op::SetAnimation { id, .. } => {
                    keys.push(id.clone());
                }
                Op::MoveNode { id, parent, .. } => {
                    keys.push(id.clone());
                    keys.push(target(parent));
                }
                Op::InsertNode { parent, .. } => keys.push(target(parent)),
                Op::Wrap { ids, .. } => keys.extend(ids.iter().cloned()),
                Op::SetLayout { target: id, .. } => keys.push(target(id)),
                Op::SetTheme { .. } | Op::SetTextStyle { .. } | Op::ApplyDesign { .. } => {
                    keys.push("theme".into());
                }
                Op::SetNotes { slide, .. }
                | Op::SetTransition { slide, .. }
                | Op::ReorderBuilds { slide, .. } => keys.push(format!("slide:{slide}")),
                Op::UpdateSlide { id, .. } | Op::DeleteSlide { id } => {
                    keys.push(format!("slide:{id}"));
                }
                Op::AddSlide { .. } | Op::MoveSlide { .. } => keys.push("order".into()),
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

    /// Re-evaluate pending edits after a file or local change.
    pub fn recheck(&mut self, deck: &Deck) {
        if matches!(self.status, EditStatus::Pending | EditStatus::Outdated) {
            self.status = if self.guards.iter().any(|(k, v)| snapshot(deck, k) != *v)
                || super::apply(deck, &self.proposal.ops).is_err()
            {
                EditStatus::Outdated
            } else {
                EditStatus::Pending
            };
        }
    }
}

/// Path shared by the model tool and canvas.
#[must_use]
pub fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".comments.json");
    name.into()
}

/// Load threads; an absent file is an empty conversation.
pub fn load(path: &Path) -> Result<Vec<Thread>> {
    let mut threads: Vec<Thread> = load_for(path)?;
    for thread in &mut threads {
        // Version-1 anchors were grid cells of 60 px (on the 1920 px slide).
        if !thread.anchor.px {
            thread.anchor.x = thread.anchor.x * 60 + 30;
            thread.anchor.y = thread.anchor.y * 60 + 30;
            thread.anchor.px = true;
        }
    }
    Ok(threads)
}

/// Persist an entire comments transaction atomically.
pub fn save(path: &Path, threads: &[Thread]) -> Result<()> {
    save_for(path, threads)
}

/// Shared sidecar reader; data-specific anchor migration stays with its kind.
pub fn load_for<A: serde::de::DeserializeOwned, E: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Vec<Thread<A, E>>> {
    let path = sidecar(path);
    ensure!(!path.is_symlink(), "comments file must not be a symlink");
    if !path.exists() {
        return Ok(vec![]);
    }
    serde_json::from_slice(&std::fs::read(path)?).context("invalid comments file")
}

/// Shared atomic writer; default type parameters keep deck sidecars unchanged.
pub fn save_for<A: Serialize, E: Serialize>(path: &Path, threads: &[Thread<A, E>]) -> Result<()> {
    super::write_atomic(&sidecar(path), &serde_json::to_vec_pretty(threads)?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::deck::suggest::RawSuggestion;

    fn anchor(slide: &str, block: Option<&str>, x: i32, y: i32) -> Anchor {
        Anchor {
            slide: slide.into(),
            block: block.map(str::to_string),
            x,
            y,
            px: true,
        }
    }

    #[test]
    fn anchors_require_a_real_slide_node_and_pixel_point() {
        let deck = Deck::classic_fixture("Anchors");
        assert!(anchor("s1", Some("b1"), 180, 360).check(&deck).is_ok());
        assert!(anchor("s1", None, 0, 0).check(&deck).is_ok());
        assert!(anchor("s1", None, 1919, 1079).check(&deck).is_ok());
        for bad in [
            anchor("nope", None, 1, 1),
            anchor("s1", Some("nope"), 1, 1),
            anchor("s1", Some("b2"), 1, 1),
            anchor("s1", None, 1920, 1),
            anchor("s1", None, 1, 1080),
            anchor("s1", None, -1, 1),
        ] {
            assert!(bad.check(&deck).is_err(), "{bad:?}");
        }
        let mut grid = anchor("s1", None, 1, 1);
        grid.px = false;
        assert!(
            grid.check(&deck).is_err(),
            "unconverted grid anchors are refused"
        );
    }

    #[test]
    fn suggested_edits_go_outdated_when_a_block_they_touch_changes() {
        let deck = Deck::classic_fixture("Edits");
        let hash = deck.node("b1").unwrap().hash();
        let raw = RawSuggestion {
            why: "Sharper".into(),
            ops: vec![
                json!({"op": "update_node", "id": "b1", "fields": {"text": "Sharper"}, "base_hash": hash}),
            ],
        };
        let (mut valid, errors) = crate::deck::suggest::prepare(&deck, vec![raw]);
        assert!(errors.is_empty());
        let mut edit = Edit::new(
            &deck,
            valid.pop().unwrap(),
            &anchor("s1", Some("b1"), 180, 360),
        );
        edit.recheck(&deck);
        assert_eq!(edit.status, EditStatus::Pending);
        // An unrelated change keeps it applicable; a change to its block does not.
        let mut other = deck.clone();
        other.slides[1].children[0].opacity = 0.5;
        edit.recheck(&other);
        assert_eq!(edit.status, EditStatus::Pending);
        let mut touched = deck.clone();
        touched.slides[0].children[0].opacity = 0.2;
        edit.recheck(&touched);
        assert_eq!(edit.status, EditStatus::Outdated);
        // And it can come back when the file returns to what the edit guarded.
        edit.recheck(&deck);
        assert_eq!(edit.status, EditStatus::Pending);
        edit.status = EditStatus::Applied;
        edit.recheck(&touched);
        assert_eq!(
            edit.status,
            EditStatus::Applied,
            "a settled edit stays settled"
        );
    }

    #[test]
    fn version_one_grid_anchors_become_pixels_on_load() {
        let dir = std::env::temp_dir().join(format!("nexus-anchor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.deck.json");
        std::fs::write(
            sidecar(&path),
            r#"[{"id":"t","anchor":{"slide":"s1","block":"b1","x":3,"y":6},"severity":"must","created":1,"resolved":null,"messages":[]}]"#,
        )
        .unwrap();
        let threads = load(&path).unwrap();
        assert_eq!((threads[0].anchor.x, threads[0].anchor.y), (210, 390));
        assert!(threads[0].anchor.px);
        save(&path, &threads).unwrap();
        assert_eq!(load(&path).unwrap()[0].anchor.x, 210, "converted once");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_sidecar_round_trips_beside_the_deck_and_refuses_symlinks() {
        let dir = std::env::temp_dir().join(format!("nexus-feedback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.deck.json");
        assert!(load(&path).unwrap().is_empty());
        let thread = Thread {
            id: "t".into(),
            anchor: anchor("s1", None, 2, 2),
            severity: Severity::Must,
            created: 1,
            resolved: None,
            messages: vec![Message::new(Role::Model, "Fix this.".into(), vec![])],
        };
        save(&path, std::slice::from_ref(&thread)).unwrap();
        assert_eq!(sidecar(&path), dir.join("a.deck.json.comments.json"));
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].messages[0].text, "Fix this.");
        std::fs::write(sidecar(&path), "not json").unwrap();
        assert!(load(&path).is_err());
        #[cfg(unix)]
        {
            std::fs::remove_file(sidecar(&path)).unwrap();
            std::os::unix::fs::symlink(dir.join("elsewhere"), sidecar(&path)).unwrap();
            std::fs::write(dir.join("elsewhere"), "[]").unwrap();
            assert!(load(&path).is_err());
            assert!(save(&path, &[thread]).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
