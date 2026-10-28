//! File operations on the decks and docs of a space: the library's rename,
//! duplicate and delete, saving a copy somewhere else, and writing an export to
//! a place the user chose in a dialog.
//!
//! Every function takes the space's files folder and a path *inside* it, as
//! the library lists them (`decks/q3.deck.json`), and refuses anything else.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};

use crate::deck::history::Who;
use crate::deck::link::Kind;
use crate::deck::{self, Deck};
use crate::doc::{self, Doc};

/// The longest title a rename accepts.
const MAX_TITLE: usize = 200;

/// The file a library path names: a plain, visible `decks/…/x.deck.json` or
/// `docs/…/x.doc.json` inside `files`, reached without symlinks.
pub fn resolve(files: &Path, relative: &str) -> Result<(Kind, PathBuf)> {
    let kind = if relative.starts_with("decks/") && relative.ends_with(".deck.json") {
        Kind::Deck
    } else if relative.starts_with("docs/") && relative.ends_with(".doc.json") {
        Kind::Doc
    } else {
        bail!("that is not a deck or doc of this space");
    };
    ensure!(relative.len() <= 400, "path too long");
    let mut path = files.canonicalize()?;
    let root = path.clone();
    for part in relative.split('/') {
        ensure!(
            !part.is_empty()
                && !part.starts_with('.')
                && !part.contains(['\\', '\0'])
                && part != "assets",
            "that is not a deck or doc of this space"
        );
        path.push(part);
        ensure!(!path.is_symlink(), "path must not contain symlinks");
    }
    ensure!(
        path.is_file() && path.starts_with(&root),
        "no such deck or doc"
    );
    Ok((kind, path))
}

fn title_ok(title: &str) -> Result<String> {
    let title = title.trim();
    ensure!(!title.is_empty(), "the title is empty");
    ensure!(
        title.chars().count() <= MAX_TITLE && !title.chars().any(char::is_control),
        "the title is too long or has control characters"
    );
    Ok(title.into())
}

/// Backslash-escape what the doc's inline Markdown would read as marks.
fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '<' | '&' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A doc with a new title; the title block follows when it showed the old one.
fn retitled_doc(doc: &Doc, title: &str) -> Result<Doc> {
    let mut ops = vec![serde_json::json!({"op": "set_title", "title": title})];
    if let Some(block) = doc
        .blocks
        .iter()
        .find(|b| matches!(b.kind, doc::Kind::Title { .. }))
        && let doc::Kind::Title { text } = &block.kind
        && doc::markdown::plain(text) == doc.title
    {
        ops.push(serde_json::json!({
            "op": "update_block", "id": block.id, "patch": {"text": escape_inline(title)}
        }));
    }
    doc::direct::prepare(doc, ops)
        .map(|prepared| prepared.next)
        .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))
}

fn read_deck(path: &Path) -> Result<Deck> {
    Deck::check(&std::fs::read_to_string(path)?)
        .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))
}

fn read_doc(path: &Path) -> Result<Doc> {
    Doc::check(&std::fs::read_to_string(path)?)
        .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))
}

/// Write a deck or doc over its own file: the version it replaces is kept in
/// the file's history, and a doc's comment anchors follow the new text.
fn write_over(files: &Path, path: &Path, kind: Kind, source: &Edited) -> Result<()> {
    match (kind, source) {
        (Kind::Deck, Edited::Deck(next)) => {
            let next = Deck::check(&serde_json::to_string(next)?)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            let _ = deck::history::record_before_write(path, &next.revision(), Who::User);
            deck::write_atomic(path, &serde_json::to_vec_pretty(&next)?)
        }
        (Kind::Doc, Edited::Doc(next)) => {
            let next = Doc::check(&serde_json::to_string(next)?)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            let mut threads = doc::feedback::load(path)?;
            let resolver = |slug: &str| crate::design::load(files, slug);
            doc::feedback::reanchor_with_resolver(&mut threads, &next, Some(&resolver));
            let _ =
                deck::history::record_before_write_for::<Doc>(path, &next.revision(), Who::User);
            deck::write_atomic(path, &serde_json::to_vec_pretty(&next)?)?;
            if !threads.is_empty() {
                doc::feedback::save(path, &threads)?;
            }
            Ok(())
        }
        _ => bail!("mismatched document kind"),
    }
}

enum Edited {
    Deck(Deck),
    Doc(Doc),
}

/// Give a deck or doc a new title. The file keeps its name, so links and the
/// model's references to it keep working.
pub fn rename(files: &Path, relative: &str, title: &str) -> Result<()> {
    let title = title_ok(title)?;
    let (kind, path) = resolve(files, relative)?;
    let next = match kind {
        Kind::Deck => {
            let mut deck = read_deck(&path)?;
            deck.title = title;
            Edited::Deck(deck)
        }
        Kind::Doc => Edited::Doc(retitled_doc(&read_doc(&path)?, &title)?),
    };
    write_over(files, &path, kind, &next)
}

/// A copy beside the original, titled `<title> copy`, named `<stem>-copy`.
/// Comments and history stay with the original. Returns the copy's library path.
pub fn duplicate(files: &Path, relative: &str) -> Result<String> {
    let (kind, path) = resolve(files, relative)?;
    let suffix = match kind {
        Kind::Deck => ".deck.json",
        Kind::Doc => ".doc.json",
    };
    let copy_title =
        |title: &str| title_ok(&format!("{title} copy")).unwrap_or_else(|_| "Untitled copy".into());
    let bytes = match kind {
        Kind::Deck => {
            let mut deck = read_deck(&path)?;
            deck.title = copy_title(&deck.title);
            let deck = Deck::check(&serde_json::to_string(&deck)?)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            serde_json::to_vec_pretty(&deck)?
        }
        Kind::Doc => {
            let doc = read_doc(&path)?;
            serde_json::to_vec_pretty(&retitled_doc(&doc, &copy_title(&doc.title))?)?
        }
    };
    let name = path.file_name().context("file name")?.to_string_lossy();
    let stem = name.strip_suffix(suffix).unwrap_or(&name);
    let folder = path.parent().context("folder")?;
    for index in 1.. {
        let candidate = if index == 1 {
            format!("{stem}-copy{suffix}")
        } else {
            format!("{stem}-copy-{index}{suffix}")
        };
        let target = folder.join(&candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(file) => {
                drop(file);
                if let Err(error) = deck::write_atomic(&target, &bytes) {
                    let _ = std::fs::remove_file(&target);
                    return Err(error);
                }
                let root = files.canonicalize()?;
                return Ok(target
                    .strip_prefix(&root)?
                    .to_string_lossy()
                    .replace('\\', "/"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("unbounded file name sequence")
}

/// `<folder>/.history/<stem>` of a deck or doc (docs keep theirs under `docs/`).
fn history_folder(kind: Kind, path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    let (stem, folder) = match kind {
        Kind::Deck => (
            name.strip_suffix(".deck.json")?,
            path.parent()?.to_path_buf(),
        ),
        Kind::Doc => (
            name.strip_suffix(".doc.json")?,
            path.ancestors()
                .find(|p| p.file_name().is_some_and(|n| n == "docs"))?
                .to_path_buf(),
        ),
    };
    Some(folder.join(".history").join(stem))
}

/// Move a deck or doc out of the library into `<files>/.trash/<when>/` with its
/// comments and kept versions, so deleting is undone by moving it back. Returns
/// the folder it went to. (The operating system's trash is not reachable with
/// the standard library alone.)
pub fn delete(files: &Path, relative: &str) -> Result<PathBuf> {
    let (kind, path) = resolve(files, relative)?;
    let root = files.canonicalize()?;
    let trash = root.join(".trash");
    ensure!(
        !trash.is_symlink(),
        "the trash folder must not be a symlink"
    );
    let when = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let folder = trash.join(format!("{when}-{}", &uuid::Uuid::new_v4().to_string()[..4]));
    std::fs::create_dir_all(&folder)?;
    let name = path.file_name().context("file name")?.to_owned();
    std::fs::rename(&path, folder.join(&name))?;
    let sidecar = deck::feedback::sidecar(&path);
    if sidecar.is_file() {
        let mut moved = name.clone();
        moved.push(".comments.json");
        std::fs::rename(&sidecar, folder.join(moved))?;
    }
    if let Some(history) = history_folder(kind, &path).filter(|h| h.is_dir() && !h.is_symlink()) {
        let _ = std::fs::rename(&history, folder.join("history"));
    }
    let renders = match kind {
        Kind::Deck => "decks",
        Kind::Doc => "docs",
    };
    let stem = name.to_string_lossy();
    let stem = stem
        .strip_suffix(".deck.json")
        .or_else(|| stem.strip_suffix(".doc.json"))
        .unwrap_or(&stem);
    let cache = root.join(renders).join(".renders").join(stem);
    if cache.is_dir() && !cache.is_symlink() {
        let _ = std::fs::remove_dir_all(cache);
    }
    Ok(folder)
}

/// Strip what file systems reject from a title, for a Save dialog's file name.
#[must_use]
pub fn file_stem_for(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        "Untitled".into()
    } else {
        cleaned.chars().take(100).collect()
    }
}

/// `<title>.<extension>`: what an export's Save dialog is pre-filled with.
#[must_use]
pub fn export_file_name(title: &str, extension: &str) -> String {
    format!("{}.{extension}", file_stem_for(title))
}

/// A path from a Save dialog: the extension the format needs is added when the
/// user left it off.
#[must_use]
pub fn with_extension(path: PathBuf, extension: &str) -> PathBuf {
    if path.extension().is_some() {
        return path;
    }
    let mut name = path.into_os_string();
    name.push(format!(".{extension}"));
    name.into()
}

/// Write an export where a Save dialog said. The dialog already asked about
/// replacing a file, so an existing file is replaced (atomically). Returns the
/// canonical path written.
pub fn write_chosen(bytes: &[u8], destination: &Path) -> Result<PathBuf> {
    ensure!(
        destination.is_absolute(),
        "the destination must be an absolute path"
    );
    ensure!(
        !destination
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "parent path components are not allowed"
    );
    ensure!(!destination.is_symlink(), "the destination is a symlink");
    ensure!(!destination.is_dir(), "the destination is a folder");
    let parent = destination.parent().context("missing folder")?;
    ensure!(parent.is_dir(), "the destination folder does not exist");
    let target = parent
        .canonicalize()?
        .join(destination.file_name().context("expected a file name")?);
    deck::write_atomic(&target, bytes)?;
    Ok(target)
}

/// Local assets (`assets/…`) a deck or doc refers to, in source order.
fn asset_references(kind: Kind, source: &str) -> Result<Vec<String>> {
    let mut refs = Vec::new();
    let mut add = |src: &str| {
        if !src.starts_with("https://") && !refs.iter().any(|r| r == src) {
            refs.push(src.to_string());
        }
    };
    match kind {
        Kind::Deck => {
            let deck = Deck::check(source)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            for node in deck.slides.iter().flat_map(deck::Slide::nodes) {
                if let deck::Kind::Image { src, .. } = &node.kind {
                    add(src);
                }
            }
            for font in &deck.theme.font_files {
                add(&font.file);
            }
        }
        Kind::Doc => {
            let doc = Doc::check(source)
                .map_err(|e| anyhow::anyhow!("{}: {}", e[0].path, e[0].message))?;
            for block in &doc.blocks {
                if let doc::Kind::Image { src, .. } = &block.kind {
                    add(src);
                }
            }
            for font in &doc.theme.font_files {
                add(&font.file);
            }
        }
    }
    Ok(refs)
}

/// What [`save_copy`] wrote.
#[derive(Debug, PartialEq, Eq)]
pub struct SavedCopy {
    pub path: PathBuf,
    /// Asset files copied into the `assets/` folder beside the copy.
    pub assets: usize,
}

/// Write `source` (a deck or doc file) to `destination` with the assets it
/// uses in an `assets/` folder beside it. The extension is `.deck.json` or
/// `.doc.json` whatever the dialog returned. A copy never overwrites an asset
/// that is already there.
pub fn save_copy(source: &Path, destination: &Path) -> Result<SavedCopy> {
    let kind = Kind::of_path(source);
    let suffix = match kind {
        Kind::Deck => ".deck.json",
        Kind::Doc => ".doc.json",
    };
    let name = destination
        .file_name()
        .context("expected a file name")?
        .to_string_lossy()
        .into_owned();
    let name = if name.ends_with(suffix) {
        name
    } else {
        format!("{}{suffix}", name.strip_suffix(".json").unwrap_or(&name))
    };
    let destination = destination.with_file_name(name);
    ensure!(
        destination.is_absolute(),
        "the destination must be an absolute path"
    );
    ensure!(
        !deck::same_file(source, &destination),
        "choose a different file than the one you are editing"
    );
    let bytes = std::fs::read(source)?;
    let source_text = std::str::from_utf8(&bytes)?;
    let from = source.parent().context("source folder")?;
    let to = destination
        .parent()
        .context("missing folder")?
        .canonicalize()
        .context("the destination folder does not exist")?;
    let mut assets = 0;
    if !deck::same_file(from, &to) {
        for reference in asset_references(kind, source_text)? {
            if !reference.starts_with("assets/") || !deck::safe_asset_path(&reference) {
                continue;
            }
            let (origin, copy) = (from.join(&reference), to.join(&reference));
            if !origin.is_file() || origin.is_symlink() || copy.exists() {
                continue;
            }
            let folder = copy.parent().context("asset folder")?;
            ensure!(
                !to.join("assets").is_symlink() && !folder.is_symlink(),
                "assets folders must not be symlinks"
            );
            std::fs::create_dir_all(folder)?;
            std::fs::copy(&origin, &copy)?;
            assets += 1;
        }
    }
    let path = write_chosen(
        &bytes,
        &to.join(destination.file_name().context("file name")?),
    )?;
    Ok(SavedCopy { path, assets })
}

/// A deck as a file in `format` (`pptx`, `pdf`, `png`, `html` or `json`), not
/// yet written anywhere. `template: Some("none")` writes a plain package.
pub fn deck_output(
    files: &Path,
    deck: &Deck,
    path: &Path,
    format: deck::export::Format,
    template: Option<&str>,
) -> Result<deck::export::Export> {
    let assets = path.parent().map(|folder| folder.join("assets"));
    crate::design::template::export_deck(deck, assets.as_deref(), files, format, template)
}

/// A doc as a file in `format`, not yet written anywhere. Markdown names its
/// images in an `assets/` folder beside the file, so they are copied by content
/// into `md_images`.
pub fn doc_output(
    files: &Path,
    doc: &Doc,
    threads: &[doc::feedback::Thread],
    path: &Path,
    format: doc::export::Format,
    template: Option<&str>,
    md_images: &Path,
) -> Result<deck::export::Export> {
    let assets = path.parent().context("doc folder")?.join("assets");
    if format != doc::export::Format::Md {
        return crate::design::template::export_doc(
            doc,
            Some(&assets),
            threads,
            files,
            format,
            template,
        );
    }
    let mut exported = doc.clone();
    for block in &mut exported.blocks {
        if let doc::Kind::Image { src, .. } = &mut block.kind
            && !src.starts_with("https://")
        {
            let from = assets.join(src.strip_prefix("assets/").unwrap_or(src));
            ensure!(
                std::fs::metadata(&from)?.len() <= deck::assets::MAX_IMAGE_BYTES,
                "image exceeds 16 MB"
            );
            *src =
                deck::assets::store_image_bytes(&std::fs::read(&from)?, src, "image", md_images)?;
        }
    }
    let mut output = doc::export::export(doc, Some(&assets), threads, format)?;
    output.bytes = doc::markdown::export(&exported).into_bytes();
    Ok(output)
}

/// The file extension an export of `kind` in `word` gets (`deck.json` for a
/// deck's own JSON).
pub fn export_extension(kind: Kind, word: &str) -> Result<&'static str> {
    Ok(match kind {
        Kind::Deck => word.parse::<deck::export::Format>()?.extension(),
        Kind::Doc => word.parse::<doc::export::Format>()?.extension(),
    })
}

/// Export a library file, read from disk, in `word` format.
pub fn export_file(
    files: &Path,
    relative: &str,
    word: &str,
    template: Option<&str>,
    md_images: &Path,
) -> Result<deck::export::Export> {
    let (kind, path) = resolve(files, relative)?;
    match kind {
        Kind::Deck => deck_output(files, &read_deck(&path)?, &path, word.parse()?, template),
        Kind::Doc => doc_output(
            files,
            &read_doc(&path)?,
            &doc::feedback::load(&path)?,
            &path,
            word.parse()?,
            template,
            md_images,
        ),
    }
}
