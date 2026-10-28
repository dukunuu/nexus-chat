//! Window-free command handling and the only filesystem boundary for studio.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};
use nexus_core::canvas::dialogs::Dialogs;
use nexus_core::deck::{self, Deck, Kind, Node, Op, same_file};
use nexus_core::remote_image::Fetcher;
use nexus_core::theme_colors::{self, ThemeColors};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod doc;
mod doc_comments;
mod exports;
mod feedback;
mod fonts;
mod history;
mod library;
pub use library::Answer;
mod templates;
use nexus_core::deck::link::{ActivityState, Message as LinkMessage};
use std::time::SystemTime;

/// All messages accepted from the presentation layer. `Open` and `ImportAsset`
/// are explicit user-entered paths; deck content can only name safe assets.
#[derive(Debug, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
pub enum Command {
    Load {},
    Save {
        deck: Deck,
    },
    Apply {
        ops: Vec<Op>,
    },
    Undo {},
    Redo {},
    NewDeck {
        title: String,
    },
    Open {
        path: PathBuf,
    },
    ImportAsset {
        path: PathBuf,
    },
    ListFonts {},
    ImportFont {
        path: PathBuf,
    },
    ImportFontData {
        name: String,
        data: String,
    },
    ImportTemplate {
        path: PathBuf,
    },
    ImportTemplateData {
        name: String,
        data: String,
    },
    Designs {},
    ApplyDesign {
        design: String,
    },
    ExportDeck {
        path: PathBuf,
    },
    Export {
        format: deck::export::Format,
        path: Option<PathBuf>,
        template: Option<String>,
    },
    Reveal {
        path: PathBuf,
    },
    Focus {
        slide: String,
        blocks: Vec<String>,
    },
    CommentCreate {
        anchor: nexus_core::deck::feedback::Anchor,
        text: String,
        send: bool,
    },
    CommentReply {
        thread: String,
        text: String,
        send: bool,
    },
    CommentResolve {
        thread: String,
    },
    CommentReopen {
        thread: String,
    },
    PreviewEdit {
        owner: String,
        edit: String,
    },
    ApplyEdit {
        owner: String,
        edit: String,
    },
    DismissEdit {
        owner: String,
        edit: String,
    },
    /// A new, empty doc in the space's `docs/` folder.
    NewDoc {
        title: String,
    },
    /// One undo step: a batch of doc operations (`doc::Op`), checked together.
    DocApply {
        ops: Vec<serde_json::Value>,
    },
    DocApplyDesign {
        design: Option<String>,
    },
    DocExport {
        format: String,
        path: Option<PathBuf>,
        template: Option<String>,
    },
    DocCommentCreate {
        anchor: nexus_core::doc::feedback::Anchor,
        text: String,
        send: bool,
    },
    /// Accept (one undo step) or reject every pending suggestion.
    DocResolveEdits {
        accept: bool,
    },
    /// Paginate and serve the pages of the open doc as `/pages/…` images.
    DocPages {},
    DocDesigns {},
    /// An image pasted or dropped: hex bytes, stored by content in the assets.
    DocImportAsset {
        name: String,
        data: String,
    },
    /// The block in view and the selected text.
    DocFocus {
        block: Option<String>,
        selection: Option<String>,
    },
    /// The decks and docs of the space, newest first.
    Library {},
    /// Open a library file by its path inside the space files.
    LibraryOpen {
        path: String,
    },
    LibraryReveal {
        path: String,
    },
    LibraryRename {
        path: String,
        title: String,
    },
    LibraryDuplicate {
        path: String,
    },
    LibraryDelete {
        path: String,
    },
    /// Export a library file through a native Save dialog.
    LibraryExport {
        path: String,
        format: String,
    },
    /// A native Open dialog: nexus files open, Office and Markdown files import.
    OpenDialog {},
    /// A native Save dialog for the open file in one of its export formats.
    ExportAs {
        format: String,
        template: Option<String>,
    },
    /// A native Save dialog for a copy of the open deck or doc with its assets.
    SaveCopy {},
    /// A native Open dialog for a `PowerPoint` or Word template.
    PickTemplate {},
    /// The kept versions of the open deck or doc.
    History {},
    /// One kept version, read-only.
    HistoryPreview {
        id: String,
    },
    /// Make a kept version the document again: one undo step, kept in history too.
    HistoryRestore {
        id: String,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreadActivity {
    #[serde(flatten)]
    state: ActivityState,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event")]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Deck {
        deck: Deck,
        colors: ThemeColors,
        revision: String,
        // The backend supplies preconditions rather than asking JS to
        // reproduce Rust's canonical number serialization.
        hashes: BTreeMap<String, String>,
        images: BTreeMap<String, String>,
        threads: Vec<nexus_core::deck::feedback::Thread>,
        linked: bool,
        activities: BTreeMap<String, ThreadActivity>,
        changed: Vec<String>,
        notice: Option<String>,
        /// Approximations made when a `PowerPoint` file was just imported.
        import_warnings: Vec<String>,
        template: bool,
        /// Studio opened with no file: the page shows the library.
        home: bool,
    },
    Feedback {
        threads: Vec<nexus_core::deck::feedback::Thread>,
        activities: BTreeMap<String, ThreadActivity>,
        notes: Vec<String>,
    },
    Focus {
        slide: String,
    },
    /// The open file is a doc.
    Doc {
        doc: nexus_core::doc::Doc,
        colors: ThemeColors,
        revision: String,
        /// `docs/q3.doc.json`, for display.
        relative: String,
        threads: Vec<nexus_core::doc::feedback::Thread>,
        linked: bool,
        activities: BTreeMap<String, ThreadActivity>,
        changed: Vec<String>,
        notice: Option<String>,
        /// Content hashes of the doc's images and fonts, by name.
        assets: BTreeMap<String, String>,
        design: doc::DesignState,
        can_undo: bool,
        can_redo: bool,
        home: bool,
    },
    DocFeedback {
        threads: Vec<nexus_core::doc::feedback::Thread>,
        activities: BTreeMap<String, ThreadActivity>,
        notes: Vec<String>,
    },
    /// What a suggestion would change, without changing it.
    DocPreview {
        owner: String,
        edit: String,
        title: Option<String>,
        changed: Vec<nexus_core::doc::Block>,
        removed: Vec<String>,
    },
    DocFocus {
        block: Option<String>,
    },
    Designs {
        available: Vec<doc::DesignInfo>,
    },
    TemplateImported {
        slug: String,
        name: String,
        warnings: Vec<String>,
    },
    Pages {
        revision: String,
        pages: Vec<doc::PageInfo>,
    },
    Theme {
        colors: ThemeColors,
    },
    Preview {
        deck: Deck,
    },
    Asset {
        src: String,
    },
    Fonts {
        families: Vec<deck::text::FontFamily>,
    },
    Exported {
        path: PathBuf,
        warnings: Vec<String>,
    },
    /// A copy of the open file, with the assets it uses, was written.
    Saved {
        path: PathBuf,
        assets: usize,
    },
    /// Every deck and doc of the space.
    Library {
        items: Vec<library::Item>,
        /// The library path of the open file, when it is inside the space.
        current: Option<String>,
        /// Studio opened with no file.
        home: bool,
        /// The file an operation just made, to highlight.
        selected: Option<String>,
        notice: Option<String>,
    },
    /// The kept versions, newest first.
    History {
        versions: Vec<history::Version>,
    },
    /// A kept version to show read-only; exactly one of `deck` and `doc` is set.
    HistoryPreview {
        id: String,
        deck: Option<Deck>,
        doc: Option<nexus_core::doc::Doc>,
    },
    /// A dialog was cancelled or the command changed nothing on screen.
    Idle {},
    Revealed {},
    Error {
        message: String,
    },
}

impl Event {
    pub fn script(&self) -> String {
        let json = serde_json::to_string(self).expect("event JSON");
        // Escaping these also works in older JS engines and prevents an event
        // from terminating a script if this transport is ever embedded in HTML.
        let json = json
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        format!("window.studioEvent({json});")
    }
}

pub struct Backend {
    pub deck: Deck,
    /// Set while a doc, not a deck, is open (`deck` is then an unused placeholder).
    doc: Option<doc::DocSession>,
    path: PathBuf,
    files_dir: PathBuf,
    recent: PathBuf,
    /// Revision of the deck as last written to, or read from, the file.
    known_revision: String,
    undo: Vec<Deck>,
    redo: Vec<Deck>,
    pub fetcher: Arc<Fetcher>,
    feedback: Vec<nexus_core::deck::feedback::Thread>,
    activities: BTreeMap<PathBuf, BTreeMap<String, ThreadActivity>>,
    pub linked: bool,
    pub outgoing: Vec<LinkMessage>,
    deck_stamp: Option<Stamp>,
    comments_stamp: Option<Stamp>,
    colors: ThemeColors,
    theme_stamp: String,
    /// Animation data dropped while reading the deck from disk, reported once.
    load_notes: Vec<String>,
    /// Approximations from importing a `.pptx`, reported once with the open.
    import_warnings: Vec<String>,
    /// Reveal is limited to files exported by this window, never deck content.
    exports: Vec<PathBuf>,
    /// Content hashes of deck assets by file, reused while the file's stamp
    /// is unchanged, so an edit doesn't re-read every image and font.
    asset_hashes: std::cell::RefCell<std::collections::HashMap<PathBuf, (Stamp, String)>>,
    /// Native Open and Save dialogs; tests script them.
    dialogs: Arc<dyn Dialogs>,
    /// Where the data root's small settings live (`studio-settings.json`).
    data_dir: PathBuf,
    /// Studio started with no file, and nothing has been opened or written since.
    home: std::cell::Cell<bool>,
    /// The last library listing, which thumbnail requests are answered from.
    listing: std::cell::RefCell<Vec<nexus_core::canvas::Entry>>,
    /// Approximations from importing a Word or Markdown file, reported once.
    doc_notice: std::cell::RefCell<Option<String>>,
}

/// Modified time and length: enough to notice another writer within one mtime tick.
type Stamp = (SystemTime, u64);

impl Backend {
    #[cfg(test)]
    pub fn start(path: Option<&Path>, files_dir: &Path, data_dir: &Path) -> Result<Self> {
        Self::open(path, files_dir, data_dir, false)
    }

    /// What the window does: with no file given the library shows, and a blank
    /// deck stays in memory until it is first edited, so starting studio never
    /// leaves an empty file in the library.
    pub fn launch(path: Option<&Path>, files_dir: &Path, data_dir: &Path) -> Result<Self> {
        Self::open(path, files_dir, data_dir, true)
    }

    fn open(path: Option<&Path>, files_dir: &Path, data_dir: &Path, defer: bool) -> Result<Self> {
        let recent = data_dir.join("studio-recent.json");
        let remembered = fs::read(&recent)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PathBuf>(&bytes).ok())
            .filter(|p| p.is_absolute() && p.is_file());
        let mut import_warnings = Vec::new();
        let no_path_given = path.is_none();
        let chosen = path.or(remembered.as_deref());
        let opens_doc = chosen.is_some_and(doc::is_doc_path);
        let path = match chosen {
            Some(path) if opens_doc => absolute(path)?,
            Some(path) if is_pptx(path) => {
                let imported =
                    deck::import::import_pptx_file(&absolute(path)?, &decks_dir(files_dir)?, None)?;
                import_warnings = imported.warnings;
                imported.path
            }
            Some(path) => absolute(path)?,
            None => new_path(files_dir)?,
        };
        let (deck, load_notes) = if path.exists() && !opens_doc {
            read_deck_noted(&path)?
        } else {
            (Deck::new_from_layouts("Untitled"), Vec::new())
        };
        let mut backend = Self {
            fetcher: Fetcher::new(asset_root(&path)?),
            deck,
            doc: None,
            path,
            files_dir: files_dir.into(),
            recent,
            known_revision: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            feedback: Vec::new(),
            activities: BTreeMap::new(),
            linked: false,
            outgoing: Vec::new(),
            deck_stamp: None,
            comments_stamp: None,
            colors: theme_colors::load().colors,
            theme_stamp: theme_colors::source_stamp(),
            load_notes,
            import_warnings,
            exports: Vec::new(),
            asset_hashes: std::cell::RefCell::default(),
            dialogs: default_dialogs(),
            data_dir: data_dir.into(),
            home: std::cell::Cell::new(no_path_given),
            listing: std::cell::RefCell::default(),
            doc_notice: std::cell::RefCell::default(),
        };
        if opens_doc {
            let doc = if backend.path.exists() {
                doc::read_doc(&backend.path)?
            } else {
                doc::create_doc_file(files_dir, &backend.path, "Untitled")?
            };
            let path = backend.path.clone();
            backend.enter_doc(path, doc)?;
            return Ok(backend);
        }
        let in_memory = defer && chosen.is_none();
        if !backend.path.exists() && !in_memory {
            backend.save()?;
        }
        backend.known_revision = backend.deck.revision();
        if !in_memory {
            backend.remember()?;
        }
        backend.load_feedback()?;
        backend.deck_stamp = stamp(&backend.path);
        Ok(backend)
    }

    pub fn event(&self) -> Event {
        if self.doc.is_some() {
            return self.doc_event();
        }
        Event::Deck {
            threads: self.feedback.clone(),
            linked: self.linked,
            activities: self.thread_activities(),
            changed: Vec::new(),
            notice: self.load_notice(),
            import_warnings: self.import_warnings.clone(),
            template: self
                .deck
                .design
                .as_deref()
                .and_then(|slug| nexus_core::design::load(&self.files_dir, slug).ok())
                .is_some_and(|d| d.source.is_some()),
            home: self.home.get(),
            deck: self.deck.clone(),
            colors: self.colors.clone(),
            revision: self.deck.revision(),
            hashes: self
                .deck
                .slides
                .iter()
                .flat_map(deck::Slide::nodes)
                .map(|n| (n.id.clone(), n.hash()))
                .collect(),
            images: self.asset_hashes(),
        }
    }

    fn deck_image_sources(&self) -> impl Iterator<Item = &str> {
        self.deck.slides.iter().flat_map(|slide| {
            slide
                .background
                .iter()
                .map(|background| background.image.as_str())
                .chain(
                    slide
                        .nodes()
                        .into_iter()
                        .filter_map(|node| match &node.kind {
                            Kind::Image { src, .. } => Some(src.as_str()),
                            _ => None,
                        }),
                )
        })
    }

    fn asset_hashes(&self) -> BTreeMap<String, String> {
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
        let mut images = BTreeMap::new();
        for src in self.deck_image_sources() {
            if src.starts_with("https://") {
                let route = image_hash(src);
                images.insert(src.to_owned(), route.clone());
                if let Some(path) = self.fetcher.cached(src)
                    && let Some(hash) =
                        self.cached_hash(&path, || read_bounded(&path, 8 * 1024 * 1024).ok())
                {
                    images.insert(format!("/asset/{route}"), hash);
                }
            } else if let Some(hash) = local(src) {
                images.insert(src.to_owned(), hash);
            }
        }
        for file in &self.deck.theme.font_files {
            if let Some(hash) = local(&file.file) {
                images.insert(file.file.clone(), hash);
            }
        }
        images
    }

    /// The content hash of `path`, read only when its stamp changed.
    fn cached_hash(&self, path: &Path, read: impl FnOnce() -> Option<Vec<u8>>) -> Option<String> {
        let current = stamp(path)?;
        if let Some((seen, hash)) = self.asset_hashes.borrow().get(path)
            && *seen == current
        {
            return Some(hash.clone());
        }
        let hash = hash_bytes(&read()?);
        self.asset_hashes
            .borrow_mut()
            .insert(path.to_path_buf(), (current, hash.clone()));
        Some(hash)
    }

    pub fn dispatch(&mut self, source: &str) -> Event {
        let result = (|| {
            ensure!(
                source.len() as u64 <= deck::import::MAX_PPTX_BYTES * 2 + 64 * 1024,
                "command too large"
            );
            let command: Command = serde_json::from_str(source).context("invalid command")?;
            ensure!(
                source.len() <= deck::MAX_SOURCE + 64 * 1024
                    || matches!(
                        command,
                        Command::ImportFontData { .. }
                            | Command::DocImportAsset { .. }
                            | Command::ImportTemplateData { .. }
                    ),
                "command too large"
            );
            // The file wins: when the deck changed under the page, show that
            // instead of applying an edit made against the old one.
            // Doc requests that only report (a focus, a comment) leave the
            // file check to the 500 ms poll, which delivers the reload with
            // its changed blocks.
            let guarded = self.doc.is_none()
                || Self::doc_mutates(&command)
                || matches!(command, Command::Load {});
            if guarded && let (Some(mut event), true) = self.sync()? {
                match &mut event {
                    Event::Deck { notice, .. } if !matches!(command, Command::Load {}) => {
                        *notice = Some("The deck changed on disk first, so your last action was not applied. Undo takes the change back.".into());
                    }
                    Event::Doc { notice, .. } if Self::doc_mutates(&command) => {
                        *notice = Some("The doc changed on disk first, so your last action was not applied. Undo takes the change back.".into());
                    }
                    _ => {}
                }
                return Ok(event);
            }
            // Dropped-animation notes are shown once, with the load that found them.
            if !matches!(command, Command::Load {} | Command::Open { .. }) {
                self.load_notes.clear();
            }
            // Import approximations are reported once, with the event that
            // opens or first loads the imported deck.
            let reports = matches!(command, Command::Load {} | Command::Open { .. });
            let event = self.execute(command);
            if reports {
                self.import_warnings.clear();
            }
            event
        })();
        result.unwrap_or_else(|e| Event::Error {
            message: e.to_string(),
        })
    }

    /// What was dropped while reading the deck, for the page's notice.
    fn load_notice(&self) -> Option<String> {
        (!self.load_notes.is_empty()).then(|| {
            format!(
                "Animation data that could not be read was dropped: {}",
                self.load_notes.join("; ")
            )
        })
    }

    #[allow(clippy::too_many_lines)]
    fn execute(&mut self, command: Command) -> Result<Event> {
        if let Some(event) = self.library_command(&command)? {
            return Ok(event);
        }
        if let Some(event) = self.switch_document(&command)? {
            return Ok(event);
        }
        match &command {
            Command::ImportTemplate { path } => return self.import_template(path),
            Command::ImportTemplateData { name, data } => {
                return self.import_template_data(name, data);
            }
            Command::Designs {} => return self.template_designs(),
            Command::ApplyDesign { design } => return self.use_design(design),
            Command::History {} => return self.history_event(),
            Command::HistoryPreview { id } => return self.history_preview(id),
            _ => {}
        }
        if self.doc.is_some() {
            return self.doc_execute(command);
        }
        Self::refuse_doc_command(&command)?;
        match command {
            Command::CommentCreate { anchor, text, send } => {
                return self.comment_create(anchor, text, send);
            }
            Command::CommentReply { thread, text, send } => {
                return self.comment_reply(&thread, text, send);
            }
            Command::CommentResolve { thread } => return self.comment_resolve(&thread, true),
            Command::CommentReopen { thread } => return self.comment_resolve(&thread, false),
            Command::Focus { slide, blocks } => {
                if self.linked {
                    self.outgoing.push(LinkMessage::Focus {
                        deck_path: self.path.clone(),
                        slide,
                        blocks,
                    });
                }
                return Ok(self.feedback_event(vec![]));
            }
            Command::PreviewEdit { owner, edit } => return self.preview_edit(&owner, &edit),
            Command::ApplyEdit { owner, edit } => self.apply_edit(&owner, &edit)?,
            Command::DismissEdit { owner, edit } => return self.dismiss_edit(&owner, &edit),
            Command::ListFonts {} => {
                return Ok(Event::Fonts {
                    families: deck::text::deck_font_info(
                        &self.deck.theme,
                        Some(&asset_root(&self.path)?),
                    ),
                });
            }
            Command::ImportFont { path } => self.import_font(&path)?,
            Command::ImportFontData { name, data } => {
                ensure!(
                    data.len() <= deck::text::MAX_FONT_FILE * 2 && data.len().is_multiple_of(2),
                    "font exceeds size limit or has invalid encoding"
                );
                let bytes: Result<Vec<u8>> = data
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
                    .collect();
                self.import_font_bytes(&name, &bytes?)?;
            }
            Command::Load {} => {}
            Command::Apply { ops } => {
                let next = deck::apply(&self.deck, &ops)?;
                self.replace(next)?;
            }
            Command::Save { deck } => {
                let next = checked(&deck)?;
                // A failed save must not change the authoritative document.
                self.write_deck(&next)?;
                self.replace(next)?;
                self.known_revision = self.deck.revision();
            }
            Command::Undo {} => {
                if let Some(previous) = self.undo.last() {
                    self.write_deck(previous)?;
                    self.redo.push(std::mem::replace(
                        &mut self.deck,
                        self.undo.pop().expect("undo"),
                    ));
                }
            }
            Command::Redo {} => {
                if let Some(next) = self.redo.last() {
                    self.write_deck(next)?;
                    self.undo.push(std::mem::replace(
                        &mut self.deck,
                        self.redo.pop().expect("redo"),
                    ));
                }
            }
            Command::NewDeck { title } => self.new_deck_file(&title)?,
            Command::HistoryRestore { id } => {
                let kept = self.history_deck(&id)?;
                self.replace(kept)?;
            }
            Command::Open { path } => {
                self.open_path(absolute(&path)?)?;
            }
            Command::ExportDeck { path } => {
                return self.export(deck::export::Format::Json, Some(&absolute(&path)?), None);
            }
            Command::Export {
                format,
                path,
                template,
            } => return self.export(format, path.as_deref(), template.as_deref()),
            Command::Reveal { path } => return self.reveal(&path),
            Command::ImportAsset { path } => {
                return Ok(Event::Asset {
                    src: self.import_asset(&path)?,
                });
            }
            // `refuse_doc_command` turned every doc-only command away above.
            _ => unreachable!("doc commands are refused without a doc"),
        }
        self.known_revision = self.deck.revision();
        self.deck_stamp = stamp(&self.path);
        self.recheck_feedback();
        self.persist_feedback()?;
        Ok(self.event())
    }

    fn new_deck_file(&mut self, title: &str) -> Result<()> {
        let deck = checked(&Deck::new_from_layouts(title))?;
        let path = new_path(&self.files_dir)?;
        atomic_write(&path, &serde_json::to_vec_pretty(&deck)?)?;
        self.open_document(path, deck)
    }

    fn replace(&mut self, next: Deck) -> Result<()> {
        if self.deck != next {
            self.write_deck(&next)?;
            self.undo.push(std::mem::replace(&mut self.deck, next));
            self.redo.clear();
        }
        Ok(())
    }

    /// Notice another writer of the deck or its comments file. One external
    /// change of the deck is one undo step, so Cmd+Z takes back the model's edit.
    pub fn poll(&mut self) -> Result<Option<Event>> {
        let mut theme_event = None;
        if !self.linked {
            let stamp = theme_colors::source_stamp();
            if stamp != self.theme_stamp {
                self.theme_stamp = stamp;
                self.colors = theme_colors::load().colors;
                theme_event = Some(Event::Theme {
                    colors: self.colors.clone(),
                });
            }
        }
        Ok(self.sync()?.0.or(theme_event))
    }

    /// The event for what changed on disk, and whether the deck itself was reloaded.
    fn sync(&mut self) -> Result<(Option<Event>, bool)> {
        if self.doc.is_some() {
            return self.doc_sync();
        }
        let deck_stamp = stamp(&self.path);
        let comments_stamp = stamp(&nexus_core::deck::feedback::sidecar(&self.path));
        let mut changed = Vec::new();
        let mut reloaded = false;
        if deck_stamp != self.deck_stamp {
            // Remember the stamp first: an unreadable file is reported once,
            // not on every tick.
            self.deck_stamp = deck_stamp;
            let (next, notes) = read_deck_noted(&self.path)?;
            if next.revision() != self.known_revision {
                self.load_notes = notes;
                changed = changed_nodes(&self.deck, &next);
                self.undo.push(std::mem::replace(&mut self.deck, next));
                self.redo.clear();
                self.known_revision = self.deck.revision();
                reloaded = true;
            }
        }
        let comments_changed = comments_stamp != self.comments_stamp;
        if comments_changed {
            self.comments_stamp = comments_stamp;
            self.load_feedback()?;
        }
        self.recheck_feedback();
        if !reloaded && !comments_changed {
            return Ok((None, false));
        }
        self.import_warnings.clear();
        let mut event = self.event();
        if let Event::Deck {
            changed: flash,
            notice,
            ..
        } = &mut event
        {
            *flash = changed;
            if reloaded {
                let dropped = self
                    .load_notice()
                    .map_or_else(String::new, |n| format!(" {n}"));
                *notice = Some(format!(
                    "The deck changed on disk and was reloaded. Undo takes the change back.{dropped}"
                ));
            }
        }
        Ok((Some(event), reloaded))
    }

    pub fn receive_link(&mut self, message: LinkMessage) -> Result<Event> {
        match message {
            LinkMessage::Activity {
                deck_path,
                thread_id,
                state,
                detail,
            } => {
                if state != ActivityState::Unknown {
                    let path = absolute(&deck_path)?;
                    self.activities
                        .entry(path)
                        .or_default()
                        .insert(thread_id, ThreadActivity { state, detail });
                }
                // Include a just-written deck in this event, so Undo is ready
                // even when completion beats the periodic filesystem poll.
                Ok(self
                    .sync()?
                    .0
                    .unwrap_or_else(|| self.feedback_event(Vec::new())))
            }
            LinkMessage::Theme { colors } => {
                self.colors = colors;
                Ok(Event::Theme {
                    colors: self.colors.clone(),
                })
            }
            LinkMessage::Open { deck_path } => {
                // Opening the document that is already open must not drop its undo history.
                if !same_file(&deck_path, &self.path) {
                    let path = absolute(&deck_path)?;
                    if self
                        .switch_document(&Command::Open { path: path.clone() })?
                        .is_none()
                    {
                        self.open_file(path)?;
                    }
                }
                Ok(self.event())
            }
            message @ LinkMessage::DocFocus { .. } if self.doc.is_some() => {
                Ok(self.doc_receive_link(&message))
            }
            LinkMessage::Focus {
                deck_path, slide, ..
            } if same_file(&deck_path, &self.path) => Ok(Event::Focus { slide }),
            LinkMessage::Bye => {
                self.linked = false;
                Ok(self.event())
            }
            _ => Ok(self.event()),
        }
    }

    pub fn hello(&mut self) {
        if self.linked {
            self.outgoing.push(LinkMessage::Hello {
                deck_path: self.path.clone(),
            });
        }
    }

    fn save(&self) -> Result<()> {
        self.write_deck(&self.deck)
    }

    /// Write the deck file, first keeping the version it replaces in the
    /// deck's history (`deck/history.rs`), so the model's `history` action and
    /// the user can both go back.
    fn write_deck(&self, deck: &Deck) -> Result<()> {
        self.home.set(false);
        // History is a safety net: failing to keep a version must not block a save.
        let _ = deck::history::record_before_write(
            &self.path,
            &deck.revision(),
            deck::history::Who::User,
        );
        atomic_write(&self.path, &serde_json::to_vec_pretty(deck)?)
    }

    fn remember(&self) -> Result<()> {
        atomic_write(&self.recent, &serde_json::to_vec(&self.path)?)
    }

    /// Read a deck from disk and make it the open document.
    /// Open a deck, or import a `PowerPoint` file and open the result.
    fn open_path(&mut self, path: PathBuf) -> Result<()> {
        if is_pptx(&path) {
            self.open_pptx(&path)
        } else {
            self.import_warnings.clear();
            self.open_file(path)
        }
    }

    /// A `PowerPoint` file becomes a new deck beside the others, then opens.
    fn open_pptx(&mut self, path: &Path) -> Result<()> {
        let imported = deck::import::import_pptx_file(path, &decks_dir(&self.files_dir)?, None)?;
        self.open_document(imported.path, imported.deck)?;
        self.load_notes.clear();
        self.import_warnings = imported.warnings;
        Ok(())
    }

    fn open_file(&mut self, path: PathBuf) -> Result<()> {
        let (deck, notes) = read_deck_noted(&path)?;
        self.open_document(path, deck)?;
        self.load_notes = notes;
        Ok(())
    }

    fn open_document(&mut self, path: PathBuf, deck: Deck) -> Result<()> {
        self.home.set(false);
        let root = asset_root(&path)?;
        atomic_write(&self.recent, &serde_json::to_vec(&path)?)?;
        self.path = path;
        self.deck = deck;
        self.fetcher = Fetcher::new(root);
        self.known_revision = self.deck.revision();
        self.undo.clear();
        self.redo.clear();
        self.deck_stamp = stamp(&self.path);
        self.load_feedback()?;
        self.hello();
        Ok(())
    }

    fn import_asset(&self, path: &Path) -> Result<String> {
        let path = absolute(path)?;
        let bytes = read_bounded(&path, 8 * 1024 * 1024)?;
        let mut reader =
            image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .context("expected a PNG, JPEG or WebP image")?;
        let image = if image.width().max(image.height()) > 1600 {
            image.thumbnail(1600, 1600)
        } else {
            image
        };
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png)?;
        let bytes = png.into_inner();
        let name = format!("{}.png", hash_bytes(&bytes));
        let root = asset_root(&self.path)?;
        atomic_write(&root.join(&name), &bytes)?;
        Ok(format!("assets/{name}"))
    }

    /// No caller-provided path is joined to a filesystem root before it has
    /// passed lexical validation, and canonical containment is checked again.
    pub fn serve(&self, uri: &str) -> Result<(&'static str, Vec<u8>)> {
        let path = protocol_path(uri)?;
        if let Some((mime, source)) = embedded(&path) {
            return Ok((mime, source.as_bytes().to_vec()));
        }
        if let Some(page) = path.strip_prefix("/pages/") {
            return self.serve_page(page);
        }
        if let Some(thumbnail) = path.strip_prefix("/library/") {
            return self.serve_thumbnail(thumbnail);
        }
        let relative = path.strip_prefix("/assets/").context("unknown resource")?;
        ensure!(deck::safe_asset_path(relative), "unsafe asset path");
        let root = asset_root(&self.path)?;
        let file = root.join(relative).canonicalize()?;
        ensure!(
            file.starts_with(&root) && file.is_file(),
            "asset escapes its folder"
        );
        let mime = match file.extension().and_then(|e| e.to_str()) {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            // Fonts imported into the deck (`assets/fonts/`).
            Some("ttf") => "font/ttf",
            Some("otf") => "font/otf",
            Some("woff2") => "font/woff2",
            _ => bail!("unsupported asset type"),
        };
        let max = if mime.starts_with("font/") {
            deck::text::MAX_FONT_FILE
        } else {
            8 * 1024 * 1024
        };
        Ok((mime, read_bounded(&file, max)?))
    }

    pub fn remote_image(&self, uri: &str) -> Result<(Arc<Fetcher>, String)> {
        let path = protocol_path(uri)?;
        let hash = path.strip_prefix("/asset/").context("unknown resource")?;
        let src = if self.doc.is_some() {
            self.doc_remote_source(hash)
        } else {
            self.deck_image_sources()
                .find(|src| src.starts_with("https://") && image_hash(src) == hash)
                .map(str::to_owned)
        }
        .context("unknown remote image")?;
        // Reject an assets symlink changed since the document was opened.
        asset_root(&self.path)?;
        Ok((Arc::clone(&self.fetcher), src))
    }
}

/// The operating system's dialogs, except in tests, which script their own: a
/// test must never open a window.
fn default_dialogs() -> Arc<dyn Dialogs> {
    #[cfg(test)]
    {
        Arc::new(nexus_core::canvas::dialogs::Scripted::new([]))
    }
    #[cfg(not(test))]
    {
        Arc::new(nexus_core::canvas::dialogs::Native)
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// A node without its children, so a frame counts as changed only when the
/// frame itself changed, not when something inside it did.
fn shallow(node: &Node) -> Node {
    let mut copy = node.clone();
    if let Some(children) = copy.children_mut() {
        children.clear();
    }
    copy
}

/// Nodes that are new or different in `next`; every node when the theme changed.
fn changed_nodes(before: &Deck, next: &Deck) -> Vec<String> {
    let old: Vec<Node> = before
        .slides
        .iter()
        .flat_map(deck::Slide::nodes)
        .map(shallow)
        .collect();
    next.slides
        .iter()
        .flat_map(deck::Slide::nodes)
        .filter(|n| before.theme != next.theme || !old.contains(&shallow(n)))
        .map(|n| n.id.clone())
        .collect()
}

fn checked(deck: &Deck) -> Result<Deck> {
    Deck::check(&serde_json::to_string(deck)?).map_err(|errors| {
        anyhow::anyhow!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "file exceeds size limit");
    Ok(bytes)
}

#[cfg(test)]
fn read_deck(path: &Path) -> Result<Deck> {
    read_deck_noted(path).map(|(deck, _)| deck)
}

fn read_deck_noted(path: &Path) -> Result<(Deck, Vec<String>)> {
    let bytes = read_bounded(path, deck::MAX_SOURCE)?;
    Deck::check_noted(std::str::from_utf8(&bytes)?).map_err(|errors| {
        anyhow::anyhow!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })
}

fn absolute(path: &Path) -> Result<PathBuf> {
    ensure!(!path.as_os_str().is_empty(), "empty path");
    ensure!(
        !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "parent path components are not allowed"
    );
    let path = std::path::absolute(path)?;
    ensure!(path.file_name().is_some(), "expected a file path");
    Ok(path)
}

/// Bytes from hex (the page cannot send a binary message), bounded after decoding.
fn decode_hex(data: &str, max: usize) -> Result<Vec<u8>> {
    ensure!(
        data.len() <= max * 2 && data.len().is_multiple_of(2),
        "file exceeds size limit or has invalid encoding"
    );
    data.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
        .collect()
}

fn is_pptx(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pptx"))
}

/// The space's `decks/` folder, created and checked to stay inside the space.
fn decks_dir(files_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(files_dir)?;
    let root = files_dir.canonicalize()?;
    let decks = root.join("decks");
    if decks.exists() {
        ensure!(
            decks.canonicalize()?.starts_with(&root),
            "decks directory escapes the space"
        );
    }
    fs::create_dir_all(&decks)?;
    Ok(decks)
}

fn new_path(files_dir: &Path) -> Result<PathBuf> {
    let decks = decks_dir(files_dir)?;
    let path = decks.join("untitled.deck.json");
    if path.exists() || path.is_symlink() {
        Ok(decks.join(format!("untitled-{}.deck.json", Uuid::new_v4())))
    } else {
        Ok(path)
    }
}

fn asset_root(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("missing deck folder")?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let root = parent.join("assets");
    // Reject even internal directory symlinks: the fetcher writes descendants.
    ensure!(!root.is_symlink(), "assets directory must not be a symlink");
    fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    ensure!(
        root.starts_with(&parent),
        "assets folder escapes deck folder"
    );
    let images = root.join("images");
    ensure!(!images.is_symlink(), "image cache must not be a symlink");
    Ok(root)
}

/// Sibling temporary file + rename: no partially written deck is observable.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    deck::write_atomic(path, bytes)
}

fn image_hash(src: &str) -> String {
    hash_bytes(src.as_bytes())
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

pub fn protocol_path(uri: &str) -> Result<String> {
    let path = uri
        .strip_prefix("nexus://localhost")
        .or_else(|| uri.strip_prefix("http://nexus.localhost"))
        .context("unknown protocol origin")?;
    let path = if path.is_empty() { "/" } else { path };
    let path = if let Some((path, query)) = path.split_once('?') {
        let version = query.strip_prefix("v=").context("unknown URL query")?;
        ensure!(
            version.len() == 64 && version.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid asset version"
        );
        path
    } else {
        path
    };
    // Webviews percent-encode spaces and non-ASCII filenames. Decode exactly
    // once, then apply the same containment rules to the decoded path.
    let mut bytes = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let digits = [
                input.next().context("invalid URL escape")?,
                input.next().context("invalid URL escape")?,
            ];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&digits)?, 16)?);
        } else {
            bytes.push(byte);
        }
    }
    let path = String::from_utf8(bytes)?;
    ensure!(
        path.starts_with('/') && !path.starts_with("//"),
        "invalid protocol path"
    );
    ensure!(
        path == "/" || deck::safe_asset_path(&path[1..]),
        "unsafe protocol path"
    );
    Ok(path)
}

/// Only byte-addressed asset responses may survive a deck switch or reload.
/// An unversioned download (including its first remote fetch) stays uncached.
pub fn cache_control(uri: &str, bytes: &[u8]) -> &'static str {
    if protocol_path(uri).is_ok_and(|p| p.starts_with("/assets/") || p.starts_with("/asset/"))
        && uri
            .split_once("?v=")
            .is_some_and(|(_, v)| v == hash_bytes(bytes))
    {
        "private, max-age=31536000, immutable"
    } else {
        "no-store"
    }
}

fn embedded(path: &str) -> Option<(&'static str, &'static str)> {
    Some(match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", include_str!("../ui/index.html")),
        "/studio.css" => ("text/css; charset=utf-8", include_str!("../ui/studio.css")),
        "/app.js" => ("text/javascript", include_str!("../ui/app.js")),
        "/local.js" => ("text/javascript", include_str!("../ui/local.js")),
        "/canvas.js" => ("text/javascript", include_str!("../ui/canvas.js")),
        "/strip.js" => ("text/javascript", include_str!("../ui/strip.js")),
        "/panel.js" => ("text/javascript", include_str!("../ui/panel.js")),
        "/render.js" => ("text/javascript", include_str!("../ui/render.js")),
        "/helpers.js" => ("text/javascript", include_str!("../ui/helpers.js")),
        "/icons.js" => ("text/javascript", include_str!("../ui/icons.js")),
        "/feedback.js" => ("text/javascript", include_str!("../ui/feedback.js")),
        "/editing.js" => ("text/javascript", include_str!("../ui/editing.js")),
        "/fields.js" => ("text/javascript", include_str!("../ui/fields.js")),
        "/fonts.js" => ("text/javascript", include_str!("../ui/fonts.js")),
        "/templates.js" => ("text/javascript", include_str!("../ui/templates.js")),
        "/library.js" => ("text/javascript", include_str!("../ui/library.js")),
        "/library.css" => ("text/css; charset=utf-8", include_str!("../ui/library.css")),
        "/guides.js" => ("text/javascript", include_str!("../ui/guides.js")),
        "/animations.js" => ("text/javascript", include_str!("../ui/animations.js")),
        "/playback.js" => ("text/javascript", include_str!("../ui/playback.js")),
        "/animate-panel.js" => ("text/javascript", include_str!("../ui/animate-panel.js")),
        "/doc/doc.css" => ("text/css; charset=utf-8", include_str!("../ui/doc/doc.css")),
        "/doc/doc.js" => ("text/javascript", include_str!("../ui/doc/doc.js")),
        "/doc/inline.js" => ("text/javascript", include_str!("../ui/doc/inline.js")),
        "/doc/model.js" => ("text/javascript", include_str!("../ui/doc/model.js")),
        "/doc/edits.js" => ("text/javascript", include_str!("../ui/doc/edits.js")),
        "/doc/dom.js" => ("text/javascript", include_str!("../ui/doc/dom.js")),
        "/doc/blocks.js" => ("text/javascript", include_str!("../ui/doc/blocks.js")),
        "/doc/editor.js" => ("text/javascript", include_str!("../ui/doc/editor.js")),
        "/doc/menus.js" => ("text/javascript", include_str!("../ui/doc/menus.js")),
        "/doc/toolbar.js" => ("text/javascript", include_str!("../ui/doc/toolbar.js")),
        "/doc/comments.js" => ("text/javascript", include_str!("../ui/doc/comments.js")),
        "/doc/paste.js" => ("text/javascript", include_str!("../ui/doc/paste.js")),
        "/doc/glyphs.js" => ("text/javascript", include_str!("../ui/doc/glyphs.js")),
        _ => return None,
    })
}

#[cfg(test)]
mod animation_tests;
#[cfg(test)]
mod background_tests;
#[cfg(test)]
mod doc_tests;
#[cfg(test)]
mod font_tests;
#[cfg(test)]
mod library_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod theme_tests;
#[cfg(test)]
mod timing_tests;
#[cfg(test)]
mod ui_tests;
