//! Shared, file-backed anchored comment threads. No inference or chat transport.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::suggest::Suggestion;
use super::{Deck, HEIGHT, Op, WIDTH};

/// A point on a slide, optionally attached to a block.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub slide: String,
    pub block: Option<String>,
    pub x: i32,
    pub y: i32,
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
            .is_some_and(|id| !slide.blocks.iter().any(|b| b.id == *id))
        {
            return Err("unknown block on anchored slide".into());
        }
        if !(0..WIDTH).contains(&self.x) || !(0..HEIGHT).contains(&self.y) {
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
pub struct Message {
    pub id: String,
    pub role: Role,
    pub text: String,
    pub created: u64,
    pub edits: Vec<Edit>,
}

/// The shared comments sidecar is a JSON array of these threads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub anchor: Anchor,
    pub severity: Severity,
    pub created: u64,
    pub resolved: Option<u64>,
    pub messages: Vec<Message>,
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
    /// Construct a local or model reply.
    #[must_use]
    pub fn new(role: Role, text: String, edits: Vec<Edit>) -> Self {
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
    serde_json::to_value(
        deck.slides
            .iter()
            .flat_map(|s| &s.blocks)
            .find(|b| b.id == key),
    )
    .expect("block JSON")
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
        for op in &proposal.ops {
            match op {
                Op::UpdateBlock { id, .. } | Op::DeleteBlock { id, .. } => keys.push(id.clone()),
                Op::Restyle { target, .. } => {
                    if target == "deck" {
                        keys.extend(base.slides.iter().map(|s| format!("slide:{}", s.id)));
                    } else if base.slides.iter().any(|s| s.id == *target) {
                        keys.push(format!("slide:{target}"));
                    } else {
                        keys.push(target.clone());
                    }
                }
                Op::SetTheme { .. } => keys.push("theme".into()),
                Op::SetNotes { slide, .. }
                | Op::SetLayout { slide, .. }
                | Op::InsertBlock { slide, .. } => keys.push(format!("slide:{slide}")),
                Op::DeleteSlide { id } => keys.push(format!("slide:{id}")),
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
    let path = sidecar(path);
    if !path.exists() {
        return Ok(vec![]);
    }
    ensure!(!path.is_symlink(), "comments file must not be a symlink");
    serde_json::from_slice(&std::fs::read(path)?).context("invalid comments file")
}

/// Persist an entire comments transaction atomically.
pub fn save(path: &Path, threads: &[Thread]) -> Result<()> {
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
        }
    }

    #[test]
    fn anchors_require_a_real_slide_block_and_grid_point() {
        let deck = Deck::classic_fixture("Anchors");
        assert!(anchor("s1", Some("b1"), 3, 6).check(&deck).is_ok());
        assert!(anchor("s1", None, 0, 0).check(&deck).is_ok());
        assert!(anchor("s1", None, 31, 17).check(&deck).is_ok());
        for bad in [
            anchor("nope", None, 1, 1),
            anchor("s1", Some("nope"), 1, 1),
            anchor("s1", Some("b2"), 1, 1),
            anchor("s1", None, 32, 1),
            anchor("s1", None, 1, 18),
            anchor("s1", None, -1, 1),
        ] {
            assert!(bad.check(&deck).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn suggested_edits_go_outdated_when_a_block_they_touch_changes() {
        let deck = Deck::classic_fixture("Edits");
        let hash = deck.slides[0].blocks[0].hash();
        let raw = RawSuggestion {
            why: "Sharper".into(),
            ops: vec![
                json!({"op": "update_block", "slide": "s1", "id": "b1", "fields": {"text": "Sharper"}, "base_hash": hash}),
            ],
        };
        let (mut valid, errors) = crate::deck::suggest::prepare(&deck, vec![raw]);
        assert!(errors.is_empty());
        let mut edit = Edit::new(&deck, valid.pop().unwrap(), &anchor("s1", Some("b1"), 3, 6));
        edit.recheck(&deck);
        assert_eq!(edit.status, EditStatus::Pending);
        // An unrelated change keeps it applicable; a change to its block does not.
        let mut other = deck.clone();
        other.slides[1].blocks[0].x = 4;
        edit.recheck(&other);
        assert_eq!(edit.status, EditStatus::Pending);
        let mut touched = deck.clone();
        touched.slides[0].blocks[0].w = 20;
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
