//! Import other presentation formats as decks. Today that is `PowerPoint`
//! (`.pptx`), the inverse of [`super::export`]'s writer.

pub mod pptx;
mod xml;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};

use super::{Deck, write_atomic};

pub use pptx::import_pptx;

/// An imported deck and every approximation made on the way.
#[derive(Debug)]
pub struct Imported {
    pub deck: Deck,
    pub warnings: Vec<String>,
}

/// An imported deck written to disk.
#[derive(Debug)]
pub struct ImportedFile {
    /// The `*.deck.json` file written (never an existing file).
    pub path: PathBuf,
    pub deck: Deck,
    pub warnings: Vec<String>,
}

/// Largest `.pptx` read.
pub const MAX_PPTX_BYTES: u64 = 250 * 1024 * 1024;

fn slug(text: &str) -> String {
    let words = text
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("-");
    if words.is_empty() {
        "imported".into()
    } else {
        words.chars().take(60).collect()
    }
}

/// Read and import the `.pptx` at `source`, copying pictures into
/// `decks_dir/assets/`. `title` replaces the presentation's own title.
fn read_and_import(source: &Path, decks_dir: &Path, title: Option<&str>) -> Result<Imported> {
    ensure!(
        source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pptx")),
        "expected a .pptx file"
    );
    let length = std::fs::metadata(source)
        .with_context(|| format!("could not read {}", source.display()))?
        .len();
    ensure!(
        length <= MAX_PPTX_BYTES,
        "the file is {} MB; at most {} MB can be imported",
        length.div_ceil(1024 * 1024),
        MAX_PPTX_BYTES / (1024 * 1024)
    );
    let bytes =
        std::fs::read(source).with_context(|| format!("could not read {}", source.display()))?;
    ensure!(
        !decks_dir.is_symlink(),
        "the decks folder must not be a symlink"
    );
    std::fs::create_dir_all(decks_dir)?;
    let mut imported = import_pptx(&bytes, &decks_dir.join("assets"))?;
    if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        imported.deck.title = title.chars().take(120).collect();
    }
    Ok(imported)
}

/// Write `deck` to `path`, which must not exist yet.
fn write_new(path: &Path, deck: &Deck) -> Result<()> {
    let contents = serde_json::to_vec_pretty(deck)?;
    // Reserve the name first so two imports never share it.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    drop(file);
    write_atomic(path, &contents).inspect_err(|_| {
        let _ = std::fs::remove_file(path);
    })
}

/// Import the `.pptx` at `source` as the new deck `deck_path` (which must not
/// exist), with pictures under `<deck folder>/assets/`.
///
/// # Errors
/// When the file cannot be read, is not a `.pptx`, or the deck cannot be
/// written.
pub fn import_pptx_to(source: &Path, deck_path: &Path, title: Option<&str>) -> Result<Imported> {
    let dir = deck_path.parent().context("missing deck folder")?;
    let imported = read_and_import(source, dir, title)?;
    write_new(deck_path, &imported.deck)?;
    Ok(imported)
}

/// Import the `.pptx` at `source` into `decks_dir` as `<slug>.deck.json`,
/// with its pictures under `decks_dir/assets/`. An existing deck is never
/// overwritten: the name gets `-2`, `-3`, … instead. `title` replaces the
/// presentation's own title (and names the file) when given.
///
/// # Errors
/// When the file cannot be read, is not a `.pptx`, or cannot be written.
pub fn import_pptx_file(
    source: &Path,
    decks_dir: &Path,
    title: Option<&str>,
) -> Result<ImportedFile> {
    let imported = read_and_import(source, decks_dir, title)?;
    let stem = title.map_or_else(
        || {
            source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        str::to_string,
    );
    let base = slug(&stem);
    for index in 1..1000 {
        let name = if index == 1 {
            format!("{base}.deck.json")
        } else {
            format!("{base}-{index}.deck.json")
        };
        let path = decks_dir.join(name);
        match write_new(&path, &imported.deck) {
            Ok(()) => {
                return Ok(ImportedFile {
                    path,
                    deck: imported.deck,
                    warnings: imported.warnings,
                });
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) => {}
            Err(error) => return Err(error),
        }
    }
    bail!("could not find a free name for the imported deck")
}

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;
