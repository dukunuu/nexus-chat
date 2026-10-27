//! Deck history: the way back after an edit that went wrong.
//!
//! Every write of a deck file, by the `canvas` tool and by studio, first keeps
//! the version it is about to replace in `<decks folder>/.history/<deck
//! stem>/<timestamp>-<rev8>-<who>.json`, where `rev8` is the first eight
//! characters of that version's revision and `who` (`model` or `user`) is the
//! writer that replaced it. A version identical to the newest kept one is
//! skipped, and only the last [`KEEP`] versions stay. Restoring a version is
//! a normal write of its content, so it is itself undoable.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};

use super::{Deck, Kind, MAX_SOURCE, write_atomic};

/// The data-specific half of history; storage and filenames stay shared.
pub trait Document: serde::Serialize + Sized {
    const MAX_HISTORY_SOURCE: usize;
    fn check_history(source: &str) -> Option<Self>;
    fn history_revision(&self) -> String;
}
impl Document for Deck {
    const MAX_HISTORY_SOURCE: usize = MAX_SOURCE;
    fn check_history(source: &str) -> Option<Self> {
        Self::check(source).ok()
    }
    fn history_revision(&self) -> String {
        self.revision()
    }
}
impl Document for crate::doc::Doc {
    const MAX_HISTORY_SOURCE: usize = 2000 * 32 * 1024 + MAX_SOURCE;
    fn check_history(source: &str) -> Option<Self> {
        Self::check(source).ok()
    }
    fn history_revision(&self) -> String {
        self.revision()
    }
}

/// Versions kept per deck.
pub const KEEP: usize = 50;

/// Who made the write that replaced a kept version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Model,
    User,
}

impl Who {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::User => "user",
        }
    }
}

/// One kept version.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    /// `YYYYMMDDTHHMMSSmmm`, UTC.
    pub stamp: String,
    pub rev8: String,
    pub who: String,
}

impl Entry {
    /// `2026-10-23 14:05:11 UTC`.
    #[must_use]
    pub fn time(&self) -> String {
        let s = &self.stamp;
        if s.len() >= 15 && s.is_ascii() {
            format!(
                "{}-{}-{} {}:{}:{} UTC",
                &s[0..4],
                &s[4..6],
                &s[6..8],
                &s[9..11],
                &s[11..13],
                &s[13..15]
            )
        } else {
            s.clone()
        }
    }

    /// The kept deck.
    ///
    /// # Errors
    /// When the file is unreadable or no longer a valid deck.
    pub fn load(&self) -> Result<Deck> {
        let bytes = std::fs::read(&self.path)?;
        ensure!(bytes.len() <= MAX_SOURCE, "kept version is too large");
        Deck::check(&String::from_utf8(bytes)?)
            .map_err(|e| anyhow::anyhow!("kept version is invalid: {}", e[0].message))
    }

    /// Load a kept deck or doc with its own validator.
    pub fn load_for<D: Document>(&self) -> Result<D> {
        ensure!(
            !self.path.is_symlink(),
            "kept version must not be a symlink"
        );
        let bytes = std::fs::read(&self.path)?;
        ensure!(
            bytes.len() <= D::MAX_HISTORY_SOURCE,
            "kept version is too large"
        );
        D::check_history(&String::from_utf8(bytes)?).context("kept version is invalid")
    }
}

/// `<folder>/.history/<stem>` for a deck file.
fn directory(deck_path: &Path) -> Result<PathBuf> {
    let name = deck_path
        .file_name()
        .context("deck filename")?
        .to_string_lossy()
        .into_owned();
    let stem = name
        .strip_suffix(".deck.json")
        .or_else(|| name.strip_suffix(".doc.json"))
        .unwrap_or(&name)
        .to_string();
    let parent = deck_path.parent().context("deck folder")?;
    let folder = if name.ends_with(".doc.json") {
        parent
            .ancestors()
            .find(|p| p.file_name().is_some_and(|n| n == "docs"))
            .unwrap_or(parent)
    } else {
        parent
    };
    let root = folder.join(".history");
    let directory = root.join(stem);
    if name.ends_with(".doc.json") {
        ensure!(
            !root.is_symlink() && !directory.is_symlink(),
            "history folder must not be a symlink"
        );
    }
    Ok(directory)
}

fn parse(path: PathBuf) -> Option<Entry> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stem = name.strip_suffix(".json")?;
    let mut parts = stem.splitn(3, '-');
    let (stamp, rev8, who) = (parts.next()?, parts.next()?, parts.next()?);
    Some(Entry {
        stamp: stamp.into(),
        rev8: rev8.into(),
        who: who.into(),
        path,
    })
}

/// Kept versions of a deck, oldest first. Empty when there is no history.
///
/// # Errors
/// When the folder exists but cannot be listed.
pub fn list(deck_path: &Path) -> Result<Vec<Entry>> {
    let dir = directory(deck_path)?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<Entry> = std::fs::read_dir(&dir)?
        .flatten()
        .filter_map(|e| parse(e.path()))
        .collect();
    if deck_path.to_string_lossy().ends_with(".doc.json") {
        ensure!(
            entries.iter().all(|entry| !entry.path.is_symlink()),
            "kept version must not be a symlink"
        );
    }
    entries.sort_by(|a, b| a.stamp.cmp(&b.stamp).then_with(|| a.rev8.cmp(&b.rev8)));
    Ok(entries)
}

/// Keep the version of `deck_path` that is about to be replaced by a write
/// whose result has revision `next_revision`. Does nothing when the file does
/// not exist, is not a valid deck, equals the result, or equals the newest
/// kept version.
///
/// # Errors
/// When the history folder cannot be written.
pub fn record_before_write(deck_path: &Path, next_revision: &str, who: Who) -> Result<()> {
    record_before_write_for::<Deck>(deck_path, next_revision, who)
}

/// Keep a version of either document kind, preserving deck filenames/bytes.
pub fn record_before_write_for<D: Document>(
    deck_path: &Path,
    next_revision: &str,
    who: Who,
) -> Result<()> {
    let Ok(bytes) = std::fs::read(deck_path) else {
        return Ok(());
    };
    if bytes.len() > D::MAX_HISTORY_SOURCE {
        return Ok(());
    }
    let Ok(source) = String::from_utf8(bytes) else {
        return Ok(());
    };
    let Some(previous) = D::check_history(&source) else {
        return Ok(());
    };
    let revision = previous.history_revision();
    if revision == next_revision {
        return Ok(());
    }
    let rev8 = &revision[..8];
    let existing = list(deck_path)?;
    if existing.last().is_some_and(|last| last.rev8 == rev8) {
        return Ok(());
    }
    let dir = directory(deck_path)?;
    let root = dir.parent().context("history folder")?;
    ensure!(!root.is_symlink(), "history folder must not be a symlink");
    std::fs::create_dir_all(&dir)?;
    ensure!(!dir.is_symlink(), "history folder must not be a symlink");
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3f");
    let file = dir.join(format!("{stamp}-{rev8}-{}.json", who.name()));
    write_atomic(&file, &serde_json::to_vec_pretty(&previous)?)?;
    let kept = existing.len() + 1;
    if kept > KEEP {
        for old in existing.iter().take(kept - KEEP) {
            let _ = std::fs::remove_file(&old.path);
        }
    }
    Ok(())
}

/// The kept version whose revision starts with `prefix` (at least 4 characters).
///
/// # Errors
/// When no kept version, or more than one, matches.
pub fn find(deck_path: &Path, prefix: &str) -> Result<Entry> {
    let prefix = prefix.trim().to_lowercase();
    ensure!(
        prefix.len() >= 4 && prefix.chars().all(|c| c.is_ascii_hexdigit()),
        "revision is the hex id shown by history, at least 4 characters"
    );
    let matches: Vec<Entry> = list(deck_path)?
        .into_iter()
        .filter(|e| e.rev8.starts_with(&prefix) || prefix.starts_with(&e.rev8))
        .collect();
    match matches.as_slice() {
        [] => {
            bail!("no kept version starts with {prefix}; run history without revision to list them")
        }
        [one] => Ok(one.clone()),
        many => {
            // The same content kept twice is one version; take the newest.
            if many.iter().all(|e| e.rev8 == many[0].rev8) {
                Ok(many[many.len() - 1].clone())
            } else {
                bail!("{prefix} matches several versions; use more characters")
            }
        }
    }
}

/// Image nodes in a deck.
#[must_use]
pub fn image_count(deck: &Deck) -> usize {
    deck.slides
        .iter()
        .flat_map(super::Slide::nodes)
        .filter(|n| matches!(n.kind, Kind::Image { .. }))
        .count()
}

/// One line on what differs between two versions of a deck.
#[must_use]
pub fn describe_change(from: &Deck, to: &Deck) -> String {
    let mut parts = Vec::new();
    if from.title != to.title {
        parts.push("title".to_string());
    }
    if from.theme != to.theme {
        parts.push("theme".to_string());
    }
    let removed = from
        .slides
        .iter()
        .filter(|s| to.slide(&s.id).is_none())
        .count();
    let added = to
        .slides
        .iter()
        .filter(|s| from.slide(&s.id).is_none())
        .count();
    let changed: Vec<String> = to
        .slides
        .iter()
        .enumerate()
        .filter(|(_, s)| from.slide(&s.id).is_some_and(|old| old != *s))
        .map(|(i, _)| (i + 1).to_string())
        .collect();
    if !changed.is_empty() {
        parts.push(format!(
            "slide{} {} changed",
            if changed.len() == 1 { "" } else { "s" },
            changed.join(", ")
        ));
    }
    let order_changed = removed == 0
        && added == 0
        && from
            .slides
            .iter()
            .map(|s| &s.id)
            .ne(to.slides.iter().map(|s| &s.id));
    if order_changed {
        parts.push("slides reordered".into());
    }
    if added > 0 {
        parts.push(format!(
            "{added} slide{} added",
            if added == 1 { "" } else { "s" }
        ));
    }
    if removed > 0 {
        parts.push(format!(
            "{removed} slide{} removed",
            if removed == 1 { "" } else { "s" }
        ));
    }
    let (a, b) = (image_count(from), image_count(to));
    if a != b {
        parts.push(format!("images {a} → {b}"));
    }
    if parts.is_empty() {
        "no change".into()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests;
