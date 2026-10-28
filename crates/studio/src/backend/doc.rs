//! Doc mode: the studio backend owns one `*.doc.json` the way it owns a deck.
//!
//! Every edit is a batch of doc operations (`doc::direct`), written to the file
//! at once and kept as one undo step; the model's edits reload as one undo step
//! too. Comments live in the shared sidecar (`doc::feedback`).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use anyhow::Context as _;
use nexus_core::deck::history::{self, Document as _};
use nexus_core::doc::{self, Doc, Kind as BlockKind, feedback as comments, layout};
use serde::Serialize;

use super::{
    Backend, Command, Event, LinkMessage, Path, PathBuf, Result, absolute, asset_root,
    atomic_write, bail, ensure, image_hash, read_bounded, stamp,
};

/// Pages are drawn this wide in pixels; the UI shows them at fit-width.
const PAGE_PNG_WIDTH: u32 = 1400;

/// One open document: its text, undo stacks and comment threads.
pub struct DocSession {
    pub doc: Doc,
    pub(super) undo: Vec<Doc>,
    pub(super) redo: Vec<Doc>,
    pub(super) threads: Vec<comments::Thread>,
    pages: RefCell<Option<PageCache>>,
}

/// Paginated pages of one revision, with PNGs drawn when first asked for.
struct PageCache {
    revision: String,
    pages: Vec<layout::Page>,
    png: HashMap<usize, Vec<u8>>,
}

impl DocSession {
    pub(super) fn new(doc: Doc) -> Self {
        Self {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            threads: Vec::new(),
            pages: RefCell::new(None),
        }
    }
}

/// The design a doc follows, for the Design panel.
#[derive(Debug, Serialize)]
pub struct DesignState {
    pub(super) slug: Option<String>,
    pub(super) name: String,
    pub(super) colors: BTreeMap<String, String>,
    pub(super) fonts: BTreeMap<String, String>,
    /// The design's revision moved since the doc copied it.
    pub(super) update_available: bool,
    pub(super) template: bool,
}

/// One design of the space, for switching.
#[derive(Debug, Serialize)]
pub struct DesignInfo {
    pub(super) slug: String,
    pub(super) name: String,
    pub(super) colors: BTreeMap<String, String>,
    pub(super) fonts: BTreeMap<String, String>,
    pub(super) current: bool,
}

#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub(super) n: usize,
    /// Points; the pages share one size, but each is told for safety.
    pub(super) width: f64,
    pub(super) height: f64,
    pub(super) src: String,
}

pub(super) fn is_doc_path(path: &Path) -> bool {
    nexus_core::deck::link::Kind::of_path(path) == nexus_core::deck::link::Kind::Doc
}

fn located(errors: &[nexus_core::deck::CheckError]) -> String {
    errors
        .iter()
        .map(|e| format!("{}: {}", e.path, e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn read_doc(path: &Path) -> Result<Doc> {
    let bytes = read_bounded(path, Doc::MAX_HISTORY_SOURCE)?;
    Doc::check(std::str::from_utf8(&bytes)?).map_err(|e| anyhow::anyhow!("{}", located(&e)))
}

fn checked(doc: &Doc) -> Result<Doc> {
    Doc::check(&serde_json::to_string(doc)?).map_err(|e| anyhow::anyhow!("{}", located(&e)))
}

/// Blocks that are new or different in `next`.
fn changed_blocks(before: &Doc, next: &Doc) -> Vec<String> {
    next.blocks
        .iter()
        .filter(|block| before.block(&block.id) != Some(block))
        .map(|block| block.id.clone())
        .collect()
}

fn new_doc_path(files_dir: &Path) -> Result<PathBuf> {
    let docs = docs_dir(files_dir)?;
    let path = docs.join("untitled.doc.json");
    if path.exists() || path.is_symlink() {
        Ok(docs.join(format!("untitled-{}.doc.json", uuid::Uuid::new_v4())))
    } else {
        Ok(path)
    }
}

/// The space's `docs/` folder, created and checked to stay inside the space.
pub(super) fn docs_dir(files_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(files_dir)?;
    let root = files_dir.canonicalize()?;
    let docs = root.join("docs");
    if docs.exists() {
        ensure!(
            docs.canonicalize()?.starts_with(&root),
            "docs directory escapes the space"
        );
    }
    std::fs::create_dir_all(&docs)?;
    Ok(docs)
}

/// A fresh doc following the space's default design.
fn blank_doc(files_dir: &Path, title: &str) -> Doc {
    let slug = nexus_core::design::default_slug(files_dir).unwrap_or_else(|_| "nexus".into());
    let design = nexus_core::design::load(files_dir, &slug).unwrap_or_default();
    let mut doc = Doc::from_design(title, &design);
    doc.design = Some(slug);
    doc
}

/// A new doc file for a path that does not exist yet.
pub(super) fn create_doc_file(files_dir: &Path, path: &Path, title: &str) -> Result<Doc> {
    let doc = checked(&blank_doc(files_dir, title))?;
    atomic_write(path, &serde_json::to_vec_pretty(&doc)?)?;
    Ok(doc)
}

impl Backend {
    /// Whether a doc, not a deck, is open.
    #[cfg(test)]
    pub fn is_doc(&self) -> bool {
        self.doc.is_some()
    }

    /// The open document's title.
    #[must_use]
    pub fn title(&self) -> &str {
        self.doc
            .as_ref()
            .map_or(self.deck.title.as_str(), |s| s.doc.title.as_str())
    }

    pub(super) fn session(&self) -> Result<&DocSession> {
        self.doc
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("open a doc first"))
    }

    fn session_mut(&mut self) -> Result<&mut DocSession> {
        self.doc
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("open a doc first"))
    }

    pub(super) fn doc_resolver(
        &self,
    ) -> impl Fn(&str) -> Result<nexus_core::design::Design> + use<> {
        let files = self.files_dir.clone();
        move |slug: &str| nexus_core::design::load(&files, slug)
    }

    fn design_state(&self, doc: &Doc) -> DesignState {
        let stored = doc
            .design
            .as_deref()
            .and_then(|slug| nexus_core::design::load(&self.files_dir, slug).ok());
        DesignState {
            slug: doc.design.clone(),
            template: stored.as_ref().is_some_and(|d| d.source.is_some()),
            name: stored.as_ref().map_or_else(
                || doc.design.clone().unwrap_or_else(|| "Custom".into()),
                |d| d.name.clone(),
            ),
            colors: doc.theme.colors.clone(),
            fonts: doc.theme.fonts.clone(),
            update_available: stored
                .as_ref()
                .is_some_and(|d| doc.design_rev.as_deref() != Some(d.revision().as_str())),
        }
    }

    /// `docs/q3.doc.json` for display; the file name outside the space.
    fn doc_relative(&self) -> String {
        self.files_dir
            .canonicalize()
            .ok()
            .and_then(|root| self.path.strip_prefix(root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| self.path.file_name().map(PathBuf::from).unwrap_or_default())
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub(super) fn doc_event(&self) -> Event {
        let session = self.doc.as_ref().expect("a doc is open");
        Event::Doc {
            doc: session.doc.clone(),
            colors: self.colors.clone(),
            revision: session.doc.revision(),
            relative: self.doc_relative(),
            threads: session.threads.clone(),
            linked: self.linked,
            activities: self.thread_activities(),
            changed: Vec::new(),
            notice: self.doc_notice.borrow_mut().take(),
            assets: self.doc_asset_hashes(&session.doc),
            design: self.design_state(&session.doc),
            can_undo: !session.undo.is_empty(),
            can_redo: !session.redo.is_empty(),
            home: self.home.get(),
        }
    }

    pub(super) fn doc_feedback_event(&self, notes: Vec<String>) -> Event {
        Event::DocFeedback {
            threads: self
                .doc
                .as_ref()
                .map(|s| s.threads.clone())
                .unwrap_or_default(),
            activities: self.thread_activities(),
            notes,
        }
    }

    /// Content hashes of the images and fonts the doc names, so the page can
    /// cache them by content.
    fn doc_asset_hashes(&self, doc: &Doc) -> BTreeMap<String, String> {
        let root = asset_root(&self.path).ok();
        let local = |src: &str| {
            let root = root.as_ref()?;
            let path = root.join(src.strip_prefix("assets/").unwrap_or(src));
            self.cached_hash(&path, || {
                self.serve(&format!("nexus://localhost/{src}"))
                    .ok()
                    .map(|(_, bytes)| bytes)
            })
        };
        let mut assets = BTreeMap::new();
        for block in &doc.blocks {
            if let BlockKind::Image { src, .. } = &block.kind {
                if src.starts_with("https://") {
                    let route = image_hash(src);
                    assets.insert(src.clone(), route.clone());
                    if let Some(path) = self.fetcher.cached(src)
                        && let Some(hash) =
                            self.cached_hash(&path, || read_bounded(&path, 8 * 1024 * 1024).ok())
                    {
                        assets.insert(format!("/asset/{route}"), hash);
                    }
                } else if let Some(hash) = local(src) {
                    assets.insert(src.clone(), hash);
                }
            }
        }
        for file in &doc.theme.font_files {
            if let Some(hash) = local(&file.file) {
                assets.insert(file.file.clone(), hash);
            }
        }
        assets
    }

    pub(super) fn doc_remote_source(&self, hash: &str) -> Option<String> {
        self.doc.as_ref()?.doc.blocks.iter().find_map(|b| {
            if let BlockKind::Image { src, .. } = &b.kind
                && src.starts_with("https://")
                && image_hash(src) == hash
            {
                Some(src.clone())
            } else {
                None
            }
        })
    }

    // -- opening ---------------------------------------------------------------

    /// Read a doc from disk and make it the open document.
    pub(super) fn open_doc_file(&mut self, path: PathBuf) -> Result<()> {
        let doc = if path.exists() {
            read_doc(&path)?
        } else {
            ensure!(
                path.parent().is_some_and(|p| !p.as_os_str().is_empty()),
                "expected a file path"
            );
            create_doc_file(&self.files_dir, &path, "Untitled")?
        };
        self.enter_doc(path, doc)
    }

    pub(super) fn enter_doc(&mut self, path: PathBuf, doc: Doc) -> Result<()> {
        self.home.set(false);
        let root = asset_root(&path)?;
        atomic_write(&self.recent, &serde_json::to_vec(&path)?)?;
        self.path = path;
        self.fetcher = nexus_core::remote_image::Fetcher::new(root);
        self.known_revision = doc.revision();
        self.deck_stamp = stamp(&self.path);
        self.load_notes.clear();
        self.import_warnings.clear();
        self.doc = Some(DocSession::new(doc));
        self.load_doc_feedback()?;
        self.hello();
        Ok(())
    }

    fn new_doc(&mut self, title: &str) -> Result<()> {
        let path = new_doc_path(&self.files_dir)?;
        let doc = create_doc_file(&self.files_dir, &path, title)?;
        self.enter_doc(path, doc)
    }

    fn load_doc_feedback(&mut self) -> Result<()> {
        let threads = comments::load(&self.path)?;
        self.comments_stamp = stamp(&comments::sidecar(&self.path));
        self.session_mut()?.threads = threads;
        self.reanchor_threads();
        Ok(())
    }

    /// Re-find anchors and recheck suggestions against the current text.
    pub(super) fn reanchor_threads(&mut self) {
        let resolver = self.doc_resolver();
        if let Some(session) = self.doc.as_mut() {
            comments::reanchor_with_resolver(&mut session.threads, &session.doc, Some(&resolver));
        }
    }

    pub(super) fn persist_doc_feedback(&mut self) -> Result<()> {
        let sidecar = comments::sidecar(&self.path);
        let threads = self.session()?.threads.clone();
        if threads.is_empty() && !sidecar.exists() {
            return Ok(());
        }
        comments::save(&self.path, &threads)?;
        self.comments_stamp = stamp(&sidecar);
        Ok(())
    }

    // -- writing ---------------------------------------------------------------

    /// Write the doc file, first keeping the version it replaces in history.
    fn write_doc(&self, doc: &Doc) -> Result<()> {
        self.home.set(false);
        // History is a safety net: failing to keep a version must not block a save.
        let _ = history::record_before_write_for::<Doc>(
            &self.path,
            &doc.revision(),
            history::Who::User,
        );
        atomic_write(&self.path, &serde_json::to_vec_pretty(doc)?)
    }

    /// Make `next` the document: one undo step, written at once.
    pub(super) fn replace_doc(&mut self, next: Doc) -> Result<()> {
        if self.session()?.doc != next {
            self.write_doc(&next)?;
            let session = self.session_mut()?;
            session.undo.push(std::mem::replace(&mut session.doc, next));
            session.redo.clear();
            self.reanchor_threads();
        }
        Ok(())
    }

    fn apply_ops(&mut self, raw: Vec<serde_json::Value>) -> Result<()> {
        let resolver = self.doc_resolver();
        let prepared =
            doc::direct::prepare_with_resolver(&self.session()?.doc, raw, Some(&resolver))
                .map_err(|e| anyhow::anyhow!("{}", located(&e)))?;
        self.replace_doc(prepared.next)
    }

    // -- commands --------------------------------------------------------------

    /// The doc half of `execute`. Commands that only make sense for decks fail
    /// with a plain sentence rather than touching the doc.
    #[allow(clippy::too_many_lines)]
    pub(super) fn doc_execute(&mut self, command: Command) -> Result<Event> {
        match command {
            Command::Load {} => {}
            Command::DocFocus { block, selection } => {
                if self.linked {
                    self.outgoing.push(LinkMessage::DocFocus {
                        deck_path: self.path.clone(),
                        block,
                        selection: selection
                            .map(|s| s.chars().take(1000).collect())
                            .filter(|s: &String| !s.is_empty()),
                    });
                }
                return Ok(self.doc_feedback_event(vec![]));
            }
            Command::DocApply { ops } => self.apply_ops(ops)?,
            Command::DocApplyDesign { design } => {
                self.apply_ops(vec![
                    serde_json::json!({"op": "apply_design", "design": design}),
                ])?;
            }
            Command::Undo {} => {
                if let Some(previous) = self.session()?.undo.last() {
                    self.write_doc(previous)?;
                    let session = self.session_mut()?;
                    let previous = session.undo.pop().expect("undo");
                    session
                        .redo
                        .push(std::mem::replace(&mut session.doc, previous));
                    self.reanchor_threads();
                }
            }
            Command::Redo {} => {
                if let Some(next) = self.session()?.redo.last() {
                    self.write_doc(next)?;
                    let session = self.session_mut()?;
                    let next = session.redo.pop().expect("redo");
                    session.undo.push(std::mem::replace(&mut session.doc, next));
                    self.reanchor_threads();
                }
            }
            Command::NewDoc { .. } | Command::NewDeck { .. } | Command::Open { .. } => {
                bail!("switching documents is handled before the open doc")
            }
            Command::DocExport {
                format,
                path,
                template,
            } => {
                return self.doc_export(&format, path.as_deref(), template.as_deref());
            }
            Command::Reveal { path } => return self.reveal(&path),
            Command::DocCommentCreate { anchor, text, send } => {
                return self.doc_comment_create(anchor, text, send);
            }
            Command::CommentReply { thread, text, send } => {
                return self.doc_comment_reply(&thread, text, send);
            }
            Command::CommentResolve { thread } => return self.doc_comment_resolve(&thread, true),
            Command::CommentReopen { thread } => return self.doc_comment_resolve(&thread, false),
            Command::PreviewEdit { owner, edit } => return self.doc_preview_edit(&owner, &edit),
            Command::ApplyEdit { owner, edit } => self.doc_apply_edit(&owner, &edit)?,
            Command::DismissEdit { owner, edit } => return self.doc_dismiss_edit(&owner, &edit),
            Command::DocResolveEdits { accept } => self.doc_resolve_edits(accept)?,
            Command::DocPages {} => return self.doc_pages(),
            Command::DocDesigns {} => return self.doc_designs(),
            Command::ImportAsset { path } => {
                let path = absolute(&path)?;
                let bytes = read_bounded(&path, 8 * 1024 * 1024)?;
                return Ok(Event::Asset {
                    src: self.doc_store_image(&bytes, &path.to_string_lossy())?,
                });
            }
            Command::DocImportAsset { name, data } => {
                let bytes = super::decode_hex(&data, 8 * 1024 * 1024)?;
                return Ok(Event::Asset {
                    src: self.doc_store_image(&bytes, &name)?,
                });
            }
            _ => bail!("that action is for decks; open a deck to use it"),
        }
        self.known_revision = self.session()?.doc.revision();
        self.deck_stamp = stamp(&self.path);
        self.persist_doc_feedback()?;
        Ok(self.doc_event())
    }

    /// Store an image by content next to the doc; the page gets its `assets/…` name.
    fn doc_store_image(&self, bytes: &[u8], label: &str) -> Result<String> {
        let stem = Path::new(label)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image");
        nexus_core::deck::assets::store_image_bytes(
            bytes,
            &Path::new(label)
                .file_name()
                .map_or_else(|| "image".into(), |n| n.to_string_lossy().into_owned()),
            stem,
            &asset_root(&self.path)?,
        )
    }

    // -- external changes ------------------------------------------------------

    /// The doc half of `sync`: one external change of the file is one undo step.
    pub(super) fn doc_sync(&mut self) -> Result<(Option<Event>, bool)> {
        let doc_stamp = stamp(&self.path);
        let comments_stamp = stamp(&comments::sidecar(&self.path));
        let mut changed = Vec::new();
        let mut reloaded = false;
        if doc_stamp != self.deck_stamp {
            // Remember the stamp first: an unreadable file is reported once.
            self.deck_stamp = doc_stamp;
            let next = read_doc(&self.path)?;
            if next.revision() != self.known_revision {
                let session = self.session_mut()?;
                changed = changed_blocks(&session.doc, &next);
                session.undo.push(std::mem::replace(&mut session.doc, next));
                session.redo.clear();
                self.known_revision = self.session()?.doc.revision();
                reloaded = true;
            }
        }
        let comments_changed = comments_stamp != self.comments_stamp;
        if comments_changed {
            self.comments_stamp = comments_stamp;
            self.load_doc_feedback()?;
        } else {
            self.reanchor_threads();
        }
        if !reloaded && !comments_changed {
            return Ok((None, false));
        }
        let mut event = self.doc_event();
        if let Event::Doc {
            changed: flash,
            notice,
            ..
        } = &mut event
        {
            *flash = changed;
            if reloaded {
                *notice = Some(
                    "The doc changed on disk and was reloaded. Undo takes the change back.".into(),
                );
            }
        }
        Ok((Some(event), reloaded))
    }

    // -- exports ---------------------------------------------------------------

    fn doc_export(
        &mut self,
        format: &str,
        path: Option<&Path>,
        template: Option<&str>,
    ) -> Result<Event> {
        let format: doc::export::Format = format.parse()?;
        let session = self.session()?;
        // Markdown names its images beside it, so they are copied into
        // `exports/assets/` by content, as the canvas tool does.
        let output = self.doc_output(
            &session.doc,
            &session.threads,
            &self.path,
            format,
            template,
            &self.files_dir.join("exports/assets"),
        )?;
        let path = nexus_core::deck::export::write_export(&output, &self.files_dir, path)?;
        self.exports.push(path.clone());
        Ok(Event::Exported {
            path,
            warnings: output.warnings,
        })
    }

    /// A doc as a file in `format`, not yet written anywhere. Markdown's images
    /// are copied by content into `md_images`.
    pub(super) fn doc_output(
        &self,
        doc: &Doc,
        threads: &[comments::Thread],
        path: &Path,
        format: doc::export::Format,
        template: Option<&str>,
        md_images: &Path,
    ) -> Result<nexus_core::deck::export::Export> {
        // The assets folder is checked (and made) the way every deck and doc
        // operation does before anything reads from it.
        asset_root(path)?;
        nexus_core::canvas::files::doc_output(
            &self.files_dir,
            doc,
            threads,
            path,
            format,
            template,
            md_images,
        )
    }

    // -- page preview ----------------------------------------------------------

    fn doc_pages(&self) -> Result<Event> {
        let session = self.session()?;
        let revision = session.doc.revision();
        let assets = asset_root(&self.path)?;
        let pages = layout::paginate(&session.doc, Some(&assets));
        let info = pages
            .iter()
            .enumerate()
            .map(|(index, page)| PageInfo {
                n: index + 1,
                width: page.width,
                height: page.height,
                src: format!("/pages/{}-{}.png", &revision[..8], index + 1),
            })
            .collect();
        *session.pages.borrow_mut() = Some(PageCache {
            revision: revision.clone(),
            pages,
            png: HashMap::new(),
        });
        Ok(Event::Pages {
            revision,
            pages: info,
        })
    }

    /// `/pages/<rev8>-<n>.png`: a page of the revision the preview showed.
    pub(super) fn serve_page(&self, name: &str) -> Result<(&'static str, Vec<u8>)> {
        let session = self.session()?;
        let (revision, number) = name
            .strip_suffix(".png")
            .and_then(|stem| stem.split_once('-'))
            .context("unknown page")?;
        let number: usize = number.parse().context("unknown page")?;
        let mut cache = session.pages.borrow_mut();
        let cache = cache.as_mut().context("no page preview is open")?;
        ensure!(
            cache.revision.starts_with(revision)
                && revision.len() == 8
                && cache.revision == session.doc.revision(),
            "the pages are out of date"
        );
        let index = number.checked_sub(1).context("unknown page")?;
        ensure!(index < cache.pages.len(), "unknown page");
        if !cache.png.contains_key(&index) {
            let assets = asset_root(&self.path)?;
            let bytes = doc::render::png(
                &session.doc,
                &cache.pages[index],
                Some(&assets),
                PAGE_PNG_WIDTH,
            )?;
            cache.png.insert(index, bytes);
        }
        Ok(("image/png", cache.png[&index].clone()))
    }

    // -- designs ---------------------------------------------------------------

    fn doc_designs(&self) -> Result<Event> {
        let current = self.session()?.doc.design.clone();
        let mut available = Vec::new();
        for slug in nexus_core::design::list(&self.files_dir)? {
            if let Ok(design) = nexus_core::design::load(&self.files_dir, &slug) {
                available.push(DesignInfo {
                    current: current.as_deref() == Some(slug.as_str()),
                    slug,
                    name: design.name,
                    colors: design.colors,
                    fonts: design.fonts,
                });
            }
        }
        Ok(Event::Designs { available })
    }

    // -- link ------------------------------------------------------------------

    pub(super) fn doc_receive_link(&self, message: &LinkMessage) -> Event {
        match message {
            LinkMessage::DocFocus {
                deck_path, block, ..
            } if nexus_core::deck::same_file(deck_path, &self.path) => Event::DocFocus {
                block: block.clone(),
            },
            _ => self.doc_event(),
        }
    }

    /// Commands that only make sense with a doc open fail in deck mode.
    pub(super) fn refuse_doc_command(command: &Command) -> Result<()> {
        ensure!(
            !matches!(
                command,
                Command::DocApply { .. }
                    | Command::DocApplyDesign { .. }
                    | Command::DocExport { .. }
                    | Command::DocCommentCreate { .. }
                    | Command::DocResolveEdits { .. }
                    | Command::DocPages {}
                    | Command::DocDesigns {}
                    | Command::DocImportAsset { .. }
                    | Command::DocFocus { .. }
                    | Command::NewDoc { .. }
            ),
            "open a doc first"
        );
        Ok(())
    }

    /// Whether a command changes the doc, so a file that moved first wins.
    pub(super) fn doc_mutates(command: &Command) -> bool {
        matches!(
            command,
            Command::DocApply { .. }
                | Command::DocApplyDesign { .. }
                | Command::ApplyDesign { .. }
                | Command::Undo {}
                | Command::Redo {}
                | Command::ApplyEdit { .. }
                | Command::DocResolveEdits { .. }
        )
    }

    /// Switch to another document (`Open`, `NewDoc`, `NewDeck`) from either mode.
    /// Returns `None` when the command is not a switch.
    pub(super) fn switch_document(&mut self, command: &Command) -> Result<Option<Event>> {
        match command {
            Command::Open { path } if is_doc_path(path) => {
                self.open_doc_file(absolute(path)?)?;
            }
            Command::NewDoc { title } => self.new_doc(title)?,
            Command::Open { .. } | Command::NewDeck { .. } if self.doc.is_some() => {
                // Leaving doc mode: the deck path re-enters deck mode with a clean slate.
                let leaving = self.doc.take();
                let result = match command {
                    Command::Open { path } => self.open_path(absolute(path)?),
                    Command::NewDeck { title } => self.new_deck_file(title),
                    _ => unreachable!("matched above"),
                };
                if let Err(error) = result {
                    self.doc = leaving;
                    return Err(error);
                }
            }
            _ => return Ok(None),
        }
        self.known_revision = if let Some(session) = &self.doc {
            session.doc.revision()
        } else {
            self.deck.revision()
        };
        Ok(Some(self.event()))
    }
}
