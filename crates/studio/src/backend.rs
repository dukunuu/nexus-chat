//! Window-free command handling and the only filesystem boundary for studio.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};
use nexus_core::deck::{self, Content, Deck, Op, same_file};
use nexus_core::remote_image::Fetcher;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod feedback;
use nexus_core::deck::link::Message as LinkMessage;
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
    ExportDeck {
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
}

#[derive(Debug, Serialize)]
#[serde(tag = "event")]
pub enum Event {
    Deck {
        deck: Deck,
        revision: String,
        // The backend supplies preconditions rather than asking JS to
        // reproduce Rust's canonical number serialization.
        hashes: BTreeMap<String, String>,
        images: BTreeMap<String, String>,
        threads: Vec<nexus_core::deck::feedback::Thread>,
        linked: bool,
        changed: Vec<String>,
        notice: Option<String>,
    },
    Feedback {
        threads: Vec<nexus_core::deck::feedback::Thread>,
        notes: Vec<String>,
    },
    Focus {
        slide: String,
    },
    Preview {
        deck: Deck,
    },
    Asset {
        src: String,
    },
    Exported {},
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
    path: PathBuf,
    files_dir: PathBuf,
    recent: PathBuf,
    /// Revision of the deck as last written to, or read from, the file.
    known_revision: String,
    undo: Vec<Deck>,
    redo: Vec<Deck>,
    pub fetcher: Arc<Fetcher>,
    feedback: Vec<nexus_core::deck::feedback::Thread>,
    pub linked: bool,
    pub outgoing: Vec<LinkMessage>,
    deck_stamp: Option<Stamp>,
    comments_stamp: Option<Stamp>,
}

/// Modified time and length: enough to notice another writer within one mtime tick.
type Stamp = (SystemTime, u64);

impl Backend {
    pub fn start(path: Option<&Path>, files_dir: &Path, data_dir: &Path) -> Result<Self> {
        let recent = data_dir.join("studio-recent.json");
        let remembered = fs::read(&recent)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PathBuf>(&bytes).ok())
            .filter(|p| p.is_absolute() && p.is_file());
        let path = match path.or(remembered.as_deref()) {
            Some(path) => absolute(path)?,
            None => new_path(files_dir)?,
        };
        let deck = if path.exists() {
            read_deck(&path)?
        } else {
            Deck::new_from_layouts("Untitled")
        };
        let mut backend = Self {
            fetcher: Fetcher::new(asset_root(&path)?),
            deck,
            path,
            files_dir: files_dir.into(),
            recent,
            known_revision: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            feedback: Vec::new(),
            linked: false,
            outgoing: Vec::new(),
            deck_stamp: None,
            comments_stamp: None,
        };
        if !backend.path.exists() {
            backend.save()?;
        }
        backend.known_revision = backend.deck.revision();
        backend.remember()?;
        backend.load_feedback()?;
        backend.deck_stamp = stamp(&backend.path);
        Ok(backend)
    }

    pub fn event(&self) -> Event {
        Event::Deck {
            threads: self.feedback.clone(),
            linked: self.linked,
            changed: Vec::new(),
            notice: None,
            deck: self.deck.clone(),
            revision: self.deck.revision(),
            hashes: self
                .deck
                .slides
                .iter()
                .flat_map(|s| &s.blocks)
                .map(|b| (b.id.clone(), b.hash()))
                .collect(),
            images: self
                .deck
                .slides
                .iter()
                .flat_map(|s| &s.blocks)
                .filter_map(|b| {
                    if let Content::Image { src, .. } = &b.content
                        && src.starts_with("https://")
                    {
                        Some((src.clone(), image_hash(src)))
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }

    pub fn dispatch(&mut self, source: &str) -> Event {
        let result = (|| {
            ensure!(
                source.len() <= deck::MAX_SOURCE + 64 * 1024,
                "command too large"
            );
            let command: Command = serde_json::from_str(source).context("invalid command")?;
            // The file wins: when the deck changed under the page, show that
            // instead of applying an edit made against the old one.
            if let (Some(mut event), true) = self.sync()? {
                if let (Event::Deck { notice, .. }, false) =
                    (&mut event, matches!(command, Command::Load {}))
                {
                    *notice = Some("The deck changed on disk first, so your last action was not applied. Undo takes the change back.".into());
                }
                return Ok(event);
            }
            self.execute(command)
        })();
        result.unwrap_or_else(|e| Event::Error {
            message: e.to_string(),
        })
    }

    fn execute(&mut self, command: Command) -> Result<Event> {
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
            Command::Load {} => {}
            Command::Apply { ops } => {
                let next = deck::apply(&self.deck, &ops)?;
                self.replace(next)?;
            }
            Command::Save { deck } => {
                let next = checked(&deck)?;
                // A failed save must not change the authoritative document.
                atomic_write(&self.path, &serde_json::to_vec_pretty(&next)?)?;
                self.replace(next)?;
                self.known_revision = self.deck.revision();
            }
            Command::Undo {} => {
                if let Some(previous) = self.undo.last() {
                    atomic_write(&self.path, &serde_json::to_vec_pretty(previous)?)?;
                    self.redo.push(std::mem::replace(
                        &mut self.deck,
                        self.undo.pop().expect("undo"),
                    ));
                }
            }
            Command::Redo {} => {
                if let Some(next) = self.redo.last() {
                    atomic_write(&self.path, &serde_json::to_vec_pretty(next)?)?;
                    self.undo.push(std::mem::replace(
                        &mut self.deck,
                        self.redo.pop().expect("redo"),
                    ));
                }
            }
            Command::NewDeck { title } => {
                let deck = checked(&Deck::new_from_layouts(&title))?;
                let path = new_path(&self.files_dir)?;
                atomic_write(&path, &serde_json::to_vec_pretty(&deck)?)?;
                self.open_document(path, deck)?;
            }
            Command::Open { path } => {
                let path = absolute(&path)?;
                let deck = read_deck(&path)?;
                self.open_document(path, deck)?;
            }
            Command::ExportDeck { path } => {
                atomic_write(&absolute(&path)?, &serde_json::to_vec_pretty(&self.deck)?)?;
                return Ok(Event::Exported {});
            }
            Command::ImportAsset { path } => {
                return Ok(Event::Asset {
                    src: self.import_asset(&path)?,
                });
            }
        }
        self.known_revision = self.deck.revision();
        self.deck_stamp = stamp(&self.path);
        self.recheck_feedback();
        self.persist_feedback()?;
        Ok(self.event())
    }

    fn replace(&mut self, next: Deck) -> Result<()> {
        if self.deck != next {
            atomic_write(&self.path, &serde_json::to_vec_pretty(&next)?)?;
            self.undo.push(std::mem::replace(&mut self.deck, next));
            self.redo.clear();
        }
        Ok(())
    }

    /// Notice another writer of the deck or its comments file. One external
    /// change of the deck is one undo step, so Cmd+Z takes back the model's edit.
    pub fn poll(&mut self) -> Result<Option<Event>> {
        Ok(self.sync()?.0)
    }

    /// The event for what changed on disk, and whether the deck itself was reloaded.
    fn sync(&mut self) -> Result<(Option<Event>, bool)> {
        let deck_stamp = stamp(&self.path);
        let comments_stamp = stamp(&nexus_core::deck::feedback::sidecar(&self.path));
        let mut changed = Vec::new();
        let mut reloaded = false;
        if deck_stamp != self.deck_stamp {
            // Remember the stamp first: an unreadable file is reported once,
            // not on every tick.
            self.deck_stamp = deck_stamp;
            let next = read_deck(&self.path)?;
            if next.revision() != self.known_revision {
                changed = changed_blocks(&self.deck, &next);
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
        let mut event = self.event();
        if let Event::Deck {
            changed: flash,
            notice,
            ..
        } = &mut event
        {
            *flash = changed;
            if reloaded {
                *notice = Some(
                    "The deck changed on disk and was reloaded. Undo takes the change back.".into(),
                );
            }
        }
        Ok((Some(event), reloaded))
    }

    pub fn receive_link(&mut self, message: LinkMessage) -> Result<Event> {
        match message {
            LinkMessage::Open { deck_path } => {
                // Opening the deck that is already open must not drop its undo history.
                if !same_file(&deck_path, &self.path) {
                    let path = absolute(&deck_path)?;
                    let deck = read_deck(&path)?;
                    self.open_document(path, deck)?;
                }
                Ok(self.event())
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
        atomic_write(&self.path, &serde_json::to_vec_pretty(&self.deck)?)
    }

    fn remember(&self) -> Result<()> {
        atomic_write(&self.recent, &serde_json::to_vec(&self.path)?)
    }

    fn open_document(&mut self, path: PathBuf, deck: Deck) -> Result<()> {
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
            _ => bail!("unsupported asset type"),
        };
        Ok((mime, read_bounded(&file, 8 * 1024 * 1024)?))
    }

    pub fn remote_image(&self, uri: &str) -> Result<(Arc<Fetcher>, String)> {
        let path = protocol_path(uri)?;
        let hash = path.strip_prefix("/asset/").context("unknown resource")?;
        let src = self
            .deck
            .slides
            .iter()
            .flat_map(|s| &s.blocks)
            .find_map(|b| {
                if let Content::Image { src, .. } = &b.content
                    && src.starts_with("https://")
                    && image_hash(src) == hash
                {
                    Some(src.clone())
                } else {
                    None
                }
            })
            .context("unknown remote image")?;
        // Reject an assets symlink changed since the document was opened.
        asset_root(&self.path)?;
        Ok((Arc::clone(&self.fetcher), src))
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Blocks that are new or different in `next`; every block when the theme changed.
fn changed_blocks(before: &Deck, next: &Deck) -> Vec<String> {
    let old: Vec<_> = before.slides.iter().flat_map(|s| &s.blocks).collect();
    next.slides
        .iter()
        .flat_map(|s| &s.blocks)
        .filter(|b| before.theme != next.theme || !old.iter().any(|o| o == b))
        .map(|b| b.id.clone())
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

fn read_deck(path: &Path) -> Result<Deck> {
    let bytes = read_bounded(path, deck::MAX_SOURCE)?;
    Deck::check(std::str::from_utf8(&bytes)?).map_err(|errors| {
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

fn new_path(files_dir: &Path) -> Result<PathBuf> {
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

fn embedded(path: &str) -> Option<(&'static str, &'static str)> {
    Some(match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", include_str!("../ui/index.html")),
        "/studio.css" => ("text/css; charset=utf-8", include_str!("../ui/studio.css")),
        "/app.js" => ("text/javascript", include_str!("../ui/app.js")),
        "/canvas.js" => ("text/javascript", include_str!("../ui/canvas.js")),
        "/strip.js" => ("text/javascript", include_str!("../ui/strip.js")),
        "/panel.js" => ("text/javascript", include_str!("../ui/panel.js")),
        "/render.js" => ("text/javascript", include_str!("../ui/render.js")),
        "/helpers.js" => ("text/javascript", include_str!("../ui/helpers.js")),
        "/icons.js" => ("text/javascript", include_str!("../ui/icons.js")),
        "/feedback.js" => ("text/javascript", include_str!("../ui/feedback.js")),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
