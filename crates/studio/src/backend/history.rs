//! Version history for the open deck or doc: the kept versions (`.history/`),
//! a read-only preview of one, and restoring one as a new write.

use anyhow::{Context as _, Result};
use nexus_core::deck::{self, Deck, history};
use nexus_core::doc::Doc;
use serde::Serialize;

use super::{Backend, Event};

/// One kept version, newest first on the wire. Each is the document as it was
/// just before an edit; `who` made that edit and `summary` says what it changed.
#[derive(Debug, Serialize)]
pub struct Version {
    /// The kept file's stem, unique within the document's history.
    pub id: String,
    /// When the edit that replaced this version was made, RFC 3339 UTC.
    pub at: String,
    /// `user` or `model`.
    pub who: String,
    pub summary: String,
}

fn id_of(entry: &history::Entry) -> String {
    entry
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `20261028T103105123` as `2026-10-28T10:31:05Z`.
fn rfc3339(stamp: &str) -> String {
    if stamp.len() >= 15 && stamp.is_ascii() {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &stamp[0..4],
            &stamp[4..6],
            &stamp[6..8],
            &stamp[9..11],
            &stamp[11..13],
            &stamp[13..15]
        )
    } else {
        stamp.to_string()
    }
}

/// What differs between two versions of a doc, in a few words.
fn describe_doc_change(from: &Doc, to: &Doc) -> String {
    let mut parts = Vec::new();
    if from.title != to.title {
        parts.push("renamed it".to_string());
    }
    let added = to
        .blocks
        .iter()
        .filter(|b| !from.blocks.iter().any(|o| o.id == b.id))
        .count();
    let removed = from
        .blocks
        .iter()
        .filter(|b| !to.blocks.iter().any(|n| n.id == b.id))
        .count();
    let edited = to
        .blocks
        .iter()
        .filter(|b| from.blocks.iter().any(|o| o.id == b.id && o != *b))
        .count();
    let blocks =
        |n: usize, verb: &str| format!("{verb} {n} block{}", if n == 1 { "" } else { "s" });
    if edited > 0 {
        parts.push(blocks(edited, "edited"));
    }
    if added > 0 {
        parts.push(blocks(added, "added"));
    }
    if removed > 0 {
        parts.push(blocks(removed, "removed"));
    }
    if parts.is_empty() && from.theme != to.theme {
        parts.push("changed the design".into());
    }
    if parts.is_empty() {
        "small changes".into()
    } else {
        let mut text = parts.join(", ");
        if let Some(first) = text.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        text
    }
}

impl Backend {
    fn history_entries(&self) -> Result<Vec<history::Entry>> {
        history::list(&self.path)
    }

    fn history_entry(&self, id: &str) -> Result<history::Entry> {
        self.history_entries()?
            .into_iter()
            .find(|entry| id_of(entry) == id)
            .context("that version is no longer kept")
    }

    /// The kept versions, newest first, each with what the edit after it changed.
    pub(super) fn history_event(&self) -> Result<Event> {
        let entries = self.history_entries()?;
        let mut versions = Vec::with_capacity(entries.len());
        if let Some(session) = &self.doc {
            let loaded: Vec<Option<Doc>> =
                entries.iter().map(|e| e.load_for::<Doc>().ok()).collect();
            for (i, entry) in entries.iter().enumerate() {
                let next = loaded
                    .get(i + 1)
                    .and_then(Option::as_ref)
                    .unwrap_or(&session.doc);
                let summary = loaded[i].as_ref().map_or_else(
                    || "unreadable version".into(),
                    |kept| describe_doc_change(kept, next),
                );
                versions.push(Version {
                    id: id_of(entry),
                    at: rfc3339(&entry.stamp),
                    who: entry.who.clone(),
                    summary,
                });
            }
        } else {
            let loaded: Vec<Option<Deck>> = entries.iter().map(|e| e.load().ok()).collect();
            for (i, entry) in entries.iter().enumerate() {
                let next = loaded
                    .get(i + 1)
                    .and_then(Option::as_ref)
                    .unwrap_or(&self.deck);
                let summary = loaded[i].as_ref().map_or_else(
                    || "unreadable version".into(),
                    |kept| {
                        let text = deck::history::describe_change(kept, next);
                        let mut text = text.trim().to_string();
                        if let Some(first) = text.get_mut(0..1) {
                            first.make_ascii_uppercase();
                        }
                        text
                    },
                );
                versions.push(Version {
                    id: id_of(entry),
                    at: rfc3339(&entry.stamp),
                    who: entry.who.clone(),
                    summary,
                });
            }
        }
        versions.reverse();
        Ok(Event::History { versions })
    }

    /// One kept version, read-only, for the page to show in place of the current one.
    pub(super) fn history_preview(&self, id: &str) -> Result<Event> {
        let entry = self.history_entry(id)?;
        if self.doc.is_some() {
            Ok(Event::HistoryPreview {
                id: id.into(),
                deck: None,
                doc: Some(entry.load_for::<Doc>()?),
            })
        } else {
            Ok(Event::HistoryPreview {
                id: id.into(),
                deck: Some(entry.load()?),
                doc: None,
            })
        }
    }

    /// The kept deck to restore, checked like any edit.
    pub(super) fn history_deck(&self, id: &str) -> Result<Deck> {
        self.history_entry(id)?.load()
    }

    /// The kept doc to restore.
    pub(super) fn history_doc(&self, id: &str) -> Result<Doc> {
        self.history_entry(id)?.load_for::<Doc>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_read_as_utc_times() {
        assert_eq!(rfc3339("20261028T103105123"), "2026-10-28T10:31:05Z");
        assert_eq!(rfc3339("odd"), "odd");
    }
}
