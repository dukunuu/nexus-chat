//! The library of a space: every deck and doc, newest first, with the file
//! operations studio, the TUI and the CLI share. Native Open and Save dialogs
//! sit behind the [`dialogs::Dialogs`] trait so no test opens a real window.

pub mod dialogs;
pub mod files;
pub mod settings;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::deck::Deck;
use crate::deck::link::Kind;
use crate::doc::Doc;

/// Deepest folder nesting the library looks into under `decks/` and `docs/`.
const MAX_DEPTH: usize = 4;
/// More files than this are not listed (a library, not a file manager).
const MAX_ENTRIES: usize = 1000;
/// Width of a doc's page-one thumbnail.
const DOC_THUMB_WIDTH: u32 = 800;

/// One deck or doc of the space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub kind: Kind,
    /// Absolute path of the file.
    pub path: PathBuf,
    /// `decks/q3.deck.json`: the path inside the space files, `/` separated.
    pub relative: String,
    pub title: String,
    pub modified: DateTime<Utc>,
    /// Slides of a deck, blocks of a doc.
    pub count: usize,
    /// The design's name, or its slug when the design is gone.
    pub design: Option<String>,
    /// The revision the thumbnail is keyed by.
    pub revision: String,
    /// Where the thumbnail is (or will be) cached; `None` for an empty deck.
    /// [`ensure_thumbnail`] renders it on first use.
    pub thumbnail: Option<PathBuf>,
}

impl Entry {
    /// `slides` or `blocks`, singular for one.
    #[must_use]
    pub fn count_label(&self) -> String {
        let (one, many) = match self.kind {
            Kind::Deck => ("slide", "slides"),
            Kind::Doc => ("block", "blocks"),
        };
        format!(
            "{} {}",
            self.count,
            if self.count == 1 { one } else { many }
        )
    }
}

/// `<files>/<decks|docs>/.renders/<stem>`, the cache folder for one file.
fn render_folder(files: &Path, kind: Kind, path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    let (folder, suffix) = suffixes(kind);
    let stem = name.strip_suffix(suffix).unwrap_or(&name);
    Some(files.join(folder).join(".renders").join(stem))
}

fn suffixes(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Deck => ("decks", ".deck.json"),
        Kind::Doc => ("docs", ".doc.json"),
    }
}

/// The cache name `deck::render::render_cached` gives a slide's render.
fn deck_thumbnail(files: &Path, path: &Path, slide: &str, revision: &str) -> Option<PathBuf> {
    let key = slide.bytes().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    Some(render_folder(files, Kind::Deck, path)?.join(format!("{key}-{}.png", &revision[..8])))
}

fn doc_thumbnail(files: &Path, path: &Path, revision: &str) -> Option<PathBuf> {
    Some(render_folder(files, Kind::Doc, path)?.join(format!("thumb-{}.png", &revision[..8])))
}

/// Every deck and doc in the space, newest first. Hidden folders (`.history`,
/// `.renders`, `.trash`), `assets` folders, symlinks and files that do not
/// validate are left out.
#[must_use]
pub fn library(files: &Path) -> Vec<Entry> {
    let Ok(root) = files.canonicalize() else {
        return Vec::new();
    };
    let mut names: HashMap<String, Option<String>> = HashMap::new();
    let mut entries = Vec::new();
    for kind in [Kind::Deck, Kind::Doc] {
        let (folder, suffix) = suffixes(kind);
        let mut found = Vec::new();
        walk(&root.join(folder), suffix, 0, &mut found);
        for path in found {
            if entries.len() >= MAX_ENTRIES {
                break;
            }
            if let Some(entry) = read_entry(&root, kind, path, &mut names) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.relative.cmp(&b.relative))
    });
    entries
}

fn walk(dir: &Path, suffix: &str, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<_> = read.flatten().collect();
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let name = child.file_name().to_string_lossy().into_owned();
        let Ok(kind) = child.file_type() else {
            continue;
        };
        // `file_type` does not follow symlinks, so a link is neither.
        if kind.is_dir() {
            if depth < MAX_DEPTH && !name.starts_with('.') && name != "assets" {
                walk(&child.path(), suffix, depth + 1, out);
            }
        } else if kind.is_file() && name.ends_with(suffix) && !name.starts_with('.') {
            out.push(child.path());
        }
    }
}

fn read_entry(
    root: &Path,
    kind: Kind,
    path: PathBuf,
    names: &mut HashMap<String, Option<String>>,
) -> Option<Entry> {
    let metadata = std::fs::metadata(&path).ok()?;
    let source = std::fs::read_to_string(&path).ok()?;
    let (title, count, design, revision, thumbnail) = match kind {
        Kind::Deck => {
            let deck = Deck::check(&source).ok()?;
            let revision = deck.revision();
            let thumbnail = deck
                .slides
                .first()
                .and_then(|slide| deck_thumbnail(root, &path, &slide.id, &revision));
            (
                deck.title.clone(),
                deck.slides.len(),
                deck.design.clone(),
                revision,
                thumbnail,
            )
        }
        Kind::Doc => {
            let doc = Doc::check(&source).ok()?;
            let revision = doc.revision();
            let thumbnail = doc_thumbnail(root, &path, &revision);
            (
                doc.title.clone(),
                doc.blocks.len(),
                doc.design.clone(),
                revision,
                thumbnail,
            )
        }
    };
    let design = design.map(|slug| {
        names
            .entry(slug.clone())
            .or_insert_with(|| crate::design::load(root, &slug).ok().map(|d| d.name))
            .clone()
            .unwrap_or(slug)
    });
    let title = if title.trim().is_empty() {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .trim_end_matches(suffixes(kind).1)
            .to_string()
    } else {
        title
    };
    Some(Entry {
        kind,
        relative: path
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
        path,
        title,
        modified: metadata.modified().ok()?.into(),
        count,
        design,
        revision,
        thumbnail,
    })
}

/// The thumbnail of `entry`, rendered into the cache when it is not there yet:
/// the first slide of a deck (the render the `canvas` tool's `view` shares) or
/// page one of a doc. Old thumbnails of the same file are removed.
pub fn ensure_thumbnail(files: &Path, entry: &Entry) -> Result<PathBuf> {
    let target = entry.thumbnail.clone().context("nothing to draw yet")?;
    if target.is_file() {
        return Ok(target);
    }
    let root = files.canonicalize()?;
    match entry.kind {
        Kind::Deck => {
            let deck = Deck::check(&std::fs::read_to_string(&entry.path)?)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            ensure!(
                deck.revision() == entry.revision,
                "the deck changed; list the library again"
            );
            let first = deck.slides.first().context("the deck has no slides")?;
            let relative = crate::deck::render::render_cached(
                &deck,
                &entry.path,
                &root,
                std::slice::from_ref(&first.id),
                false,
            )?;
            Ok(root.join(relative))
        }
        Kind::Doc => {
            let doc = Doc::check(&std::fs::read_to_string(&entry.path)?)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            ensure!(
                doc.revision() == entry.revision,
                "the doc changed; list the library again"
            );
            let assets = entry.path.parent().context("doc folder")?.join("assets");
            let pages = crate::doc::layout::paginate(&doc, Some(&assets));
            let page = pages.first().context("the doc has no pages")?;
            let bytes = crate::doc::render::png(&doc, page, Some(&assets), DOC_THUMB_WIDTH)?;
            let folder = target.parent().context("render folder")?;
            ensure!(
                !folder.is_symlink() && !target.is_symlink(),
                "render cache must not contain symlinks"
            );
            crate::deck::write_atomic(&target, &bytes)?;
            let keep = target.file_name();
            for old in std::fs::read_dir(folder)?.flatten() {
                let name = old.file_name().to_string_lossy().into_owned();
                if name.starts_with("thumb-") && Some(old.file_name().as_os_str()) != keep {
                    let _ = std::fs::remove_file(old.path());
                }
            }
            Ok(target)
        }
    }
}

/// `3 minutes ago`, `yesterday`, `12 Mar`: how recently a file changed.
#[must_use]
pub fn relative_time(modified: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - modified).num_seconds();
    match seconds {
        ..60 => "just now".into(),
        60..3600 => plural(seconds / 60, "minute"),
        3600..86_400 => plural(seconds / 3600, "hour"),
        86_400..172_800 => "yesterday".into(),
        172_800..2_592_000 => plural(seconds / 86_400, "day"),
        _ => modified.format("%-d %b %Y").to_string(),
    }
}

fn plural(n: i64, unit: &str) -> String {
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}
