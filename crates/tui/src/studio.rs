//! The terminal's half of the studio link: spawn `nexus-studio --attach`,
//! read what the canvas says, tell it what the model changed, and turn its
//! comments into ordinary user messages.
//!
//! All of this is view state. Core only knows the shared `deck_link` context
//! (what studio shows, which decks the model changed), so the headless CLI and
//! the host are unaffected.

use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use nexus_core::deck::link::{ActivityState, Kind as FileKind, Line, Message, WaitingReason};
use nexus_core::deck::{Deck, Kind, feedback, render, same_file};
use nexus_core::doc::{self, Doc};
use tokio::sync::{Notify, mpsc};

use crate::app_view::AppView;

/// How much of a block's text names it in a comment message.
const SUMMARY_CHARS: usize = 60;
/// Characters of the anchored node's text a comment message carries.
const NODE_TEXT_CHARS: usize = 300;

/// A running studio window.
struct Canvas {
    child: Option<Child>,
    input: ChildStdin,
    messages: mpsc::UnboundedReceiver<Message>,
    deck: PathBuf,
    /// Set when the window speaks another link version ("update nexus-studio").
    problem: Arc<Mutex<Option<String>>>,
}

impl Canvas {
    /// Best effort: a closed window is noticed through its `Bye`.
    fn send(&mut self, message: Message) {
        if let Ok(line) = Line::encode(message) {
            let _ = self
                .input
                .write_all(line.as_bytes())
                .and_then(|()| self.input.flush());
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        self.send(Message::Bye);
        // Reap off-thread; studio also exits when its stdin closes.
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// What studio reported it is looking at.
#[derive(Clone, PartialEq, Eq)]
struct Focus {
    deck: PathBuf,
    /// A slide id, or for a doc the block in view.
    slide: String,
    /// Selected nodes of a slide, or for a doc the selected text (at most one entry).
    blocks: Vec<String>,
    doc: bool,
}

#[derive(Clone)]
struct StudioComment {
    deck: PathBuf,
    thread: String,
    reason: Option<WaitingReason>,
}

struct CommentTurn {
    comment: StudioComment,
    task: nexus_core::app::ChatTaskId,
    slides: Vec<(String, String)>,
    edited: BTreeSet<usize>,
    /// For a doc: each block with its JSON, and the ids that changed in this turn.
    blocks: Vec<(String, String)>,
    edited_blocks: BTreeSet<String>,
    replies: usize,
    replied: bool,
}

impl CommentTurn {
    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.edited_blocks.is_empty() {
            let ids = self.edited_blocks.iter().cloned().collect::<Vec<_>>();
            parts.push(if ids.len() <= 8 {
                format!(
                    "edited block{} {}",
                    if ids.len() == 1 { "" } else { "s" },
                    ids.join(", ")
                )
            } else {
                format!("edited {} blocks", ids.len())
            });
        }
        if !self.edited.is_empty() {
            let numbers = self
                .edited
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "edited slide{} {numbers}",
                if self.edited.len() == 1 { "" } else { "s" }
            ));
        }
        if self.replied {
            parts.push("replied in this thread".into());
        }
        parts.join("; ")
    }
}

fn slide_revisions(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|source| Deck::check(&source).ok())
        .map(|deck| {
            deck.slides
                .iter()
                .map(|slide| {
                    (
                        slide.id.clone(),
                        serde_json::to_string(slide).expect("slide JSON"),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Each block of a doc file with its JSON, to tell which ones a turn edited.
fn block_revisions(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|source| Doc::check(&source).ok())
        .map(|doc| {
            doc.blocks
                .iter()
                .map(|block| {
                    (
                        block.id.clone(),
                        serde_json::to_string(block).expect("block JSON"),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_doc(path: &Path) -> bool {
    FileKind::of_path(path) == FileKind::Doc
}

fn model_replies(comment: &StudioComment) -> usize {
    let model = |role: &feedback::Role| matches!(role, feedback::Role::Model);
    if is_doc(&comment.deck) {
        return doc::feedback::load(&comment.deck)
            .unwrap_or_default()
            .iter()
            .filter(|thread| thread.id == comment.thread)
            .flat_map(|thread| &thread.messages)
            .filter(|message| model(&message.role))
            .count();
    }
    feedback::load(&comment.deck)
        .unwrap_or_default()
        .iter()
        .filter(|thread| thread.id == comment.thread)
        .flat_map(|thread| &thread.messages)
        .filter(|message| model(&message.role))
        .count()
}

/// `paragraph`, `heading 2`, `list`… for a block, as the comment message names it.
fn block_kind(block: &doc::Block) -> String {
    match &block.kind {
        doc::Kind::Heading { level, .. } => format!("heading {level}"),
        doc::Kind::PageBreak => "page_break".into(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value["kind"].as_str().map(str::to_string))
            .unwrap_or_else(|| "block".into()),
    }
}

/// Terminal-side link state; lives on `AppView`.
#[derive(Default)]
pub struct Studio {
    canvas: Option<Canvas>,
    /// Comments waiting for a free composer and no streaming reply.
    queued: VecDeque<String>,
    comments: VecDeque<StudioComment>,
    active: Option<CommentTurn>,
    #[cfg(test)]
    activities: Vec<Message>,
    last_deck: Option<PathBuf>,
    focus: Option<Focus>,
    /// What the model was last told, so it hears about a change once.
    told: Option<Focus>,
    warned_draft: bool,
}

impl Studio {
    /// Whether a studio window is attached.
    #[must_use]
    pub fn linked(&self) -> bool {
        self.canvas.is_some()
    }

    /// Whether waiting comments or an active comment turn need polling.
    #[must_use]
    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty() || self.active.is_some()
    }
}

impl AppView {
    fn files_dir(&self) -> PathBuf {
        self.space.files_dir(&self.active_space.name)
    }

    /// Wakes the event loop when the model changes a deck or studio speaks.
    pub fn studio_wake(&self) -> Arc<Notify> {
        Arc::clone(&self.deck_link.lock().expect("deck link").notify)
    }

    /// Open or focus the studio linked to this terminal, on `deck` or on the
    /// most recent one.
    pub fn open_studio(&mut self, deck: Option<&str>) -> Result<()> {
        if self.incognito {
            bail!("studio isn't available in incognito mode");
        }
        let files = self.files_dir();
        std::fs::create_dir_all(files.join("decks"))?;
        std::fs::create_dir_all(files.join("docs"))?;
        let path = match deck {
            Some(deck) if Path::new(deck).is_absolute() => PathBuf::from(deck),
            Some(deck) => files.join(deck),
            None => self.default_deck(&files),
        };
        let path = std::path::absolute(path)?;
        self.studio.last_deck = Some(path.clone());
        if let Some(canvas) = &mut self.studio.canvas {
            canvas.deck.clone_from(&path);
            canvas.send(Message::Open { deck_path: path });
            self.push_status("studio focused");
            return Ok(());
        }
        let wake = self.studio_wake();
        self.studio.canvas = Some(spawn(&path, wake)?);
        self.send_studio_theme();
        self.refresh_tool_cards();
        self.push_status("studio linked to this conversation");
        Ok(())
    }

    /// Send the current terminal palette on attach and after a theme switch.
    pub fn send_studio_theme(&mut self) {
        if let Some(canvas) = &mut self.studio.canvas {
            canvas.send(Message::Theme {
                colors: self.theme.colors(),
            });
        }
    }

    /// The file for `/studio` without an argument: the last one the model
    /// touched or studio showed, else the newest deck or doc, else a new
    /// untitled deck.
    fn default_deck(&self, files: &Path) -> PathBuf {
        if let Some(last) = &self.studio.last_deck {
            return last.clone();
        }
        ["decks", "docs"]
            .iter()
            .flat_map(|folder| {
                std::fs::read_dir(files.join(folder))
                    .into_iter()
                    .flatten()
                    .flatten()
            })
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.ends_with(".deck.json") || name.ends_with(".doc.json")
            })
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .max()
            .map_or_else(|| files.join("decks/untitled.deck.json"), |(_, path)| path)
    }

    /// Tool cards hint at `/studio` only while no window is attached, so a
    /// change of link state re-lays them out.
    fn refresh_tool_cards(&mut self) {
        self.tool_open_gen = self.tool_open_gen.wrapping_add(1);
    }

    /// Apply one message from studio.
    pub fn handle_studio_message(&mut self, message: Message) -> Result<()> {
        match message {
            message @ (Message::DocFocus { .. } | Message::DocComment { .. }) => {
                return self.handle_doc_message(message);
            }
            message @ Message::Reply { .. } if message.path().is_some_and(is_doc) => {
                return self.handle_doc_message(message);
            }
            Message::Hello { deck_path } => self.studio.last_deck = Some(deck_path),
            Message::Focus {
                deck_path,
                slide,
                blocks,
            } => {
                self.deck_link.lock().expect("deck link").focus =
                    Some((deck_path.clone(), slide.clone(), blocks.clone()));
                self.studio.focus = Some(Focus {
                    deck: deck_path,
                    slide,
                    blocks,
                    doc: false,
                });
            }
            Message::Comment {
                deck_path,
                thread_id,
                slide,
                block,
                text,
                ..
            } => {
                self.queue_studio_comment(deck_path, thread_id, &slide, block.as_deref(), &text)?;
            }
            Message::Reply {
                deck_path,
                thread_id,
                text,
            } => {
                let threads = match feedback::load(&deck_path) {
                    Ok(threads) => threads,
                    Err(error) => {
                        self.studio_failure(deck_path, thread_id);
                        return Err(error);
                    }
                };
                let Some(thread) = threads.iter().find(|t| t.id == thread_id) else {
                    self.studio_failure(deck_path, thread_id);
                    bail!("studio replied in a thread that no longer exists");
                };
                let (slide, block) = (thread.anchor.slide.clone(), thread.anchor.block.clone());
                self.queue_studio_comment(deck_path, thread_id, &slide, block.as_deref(), &text)?;
            }
            Message::Bye => {
                if self.studio.canvas.take().is_some() {
                    self.refresh_tool_cards();
                    self.push_status("studio closed");
                }
            }
            Message::Open { .. } | Message::Theme { .. } | Message::Activity { .. } => {}
        }
        Ok(())
    }

    /// What a doc window says: the block in view, and comments and replies on text.
    fn handle_doc_message(&mut self, message: Message) -> Result<()> {
        match message {
            Message::DocFocus {
                deck_path,
                block,
                selection,
            } => {
                let block = block.unwrap_or_default();
                let selected: Vec<String> = selection.into_iter().collect();
                self.deck_link.lock().expect("deck link").focus =
                    Some((deck_path.clone(), block.clone(), selected.clone()));
                self.studio.focus = Some(Focus {
                    deck: deck_path,
                    slide: block,
                    blocks: selected,
                    doc: true,
                });
            }
            Message::DocComment {
                deck_path,
                thread_id,
                block,
                quote,
                text,
                ..
            } => {
                let built = self.doc_comment_text(&deck_path, &thread_id, &block, &quote, &text);
                self.enqueue_studio_comment(deck_path, thread_id, built)?;
            }
            Message::Reply {
                deck_path,
                thread_id,
                text,
            } if is_doc(&deck_path) => {
                let threads = match doc::feedback::load(&deck_path) {
                    Ok(threads) => threads,
                    Err(error) => {
                        self.studio_failure(deck_path, thread_id);
                        return Err(error);
                    }
                };
                let Some(thread) = threads.iter().find(|t| t.id == thread_id) else {
                    self.studio_failure(deck_path, thread_id);
                    bail!("studio replied in a thread that no longer exists");
                };
                let (block, quote) = (
                    thread.anchor.block.clone().unwrap_or_default(),
                    thread.anchor.quote.clone(),
                );
                let built = self.doc_comment_text(&deck_path, &thread_id, &block, &quote, &text);
                self.enqueue_studio_comment(deck_path, thread_id, built)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn studio_activity(
        &mut self,
        comment: &StudioComment,
        state: ActivityState,
        detail: Option<String>,
    ) {
        let message = Message::Activity {
            deck_path: comment.deck.clone(),
            thread_id: comment.thread.clone(),
            state,
            detail: detail.map(|text| text.chars().take(1024).collect()),
        };
        #[cfg(test)]
        self.studio.activities.push(message.clone());
        if let Some(canvas) = &mut self.studio.canvas {
            canvas.send(message);
        }
    }

    fn studio_failure(&mut self, deck: PathBuf, thread: String) {
        self.studio_activity(
            &StudioComment {
                deck,
                thread,
                reason: None,
            },
            ActivityState::Failed,
            Some("Could not send this comment; see nexus for details".into()),
        );
    }

    fn queue_studio_comment(
        &mut self,
        deck: PathBuf,
        thread: String,
        slide: &str,
        block: Option<&str>,
        text: &str,
    ) -> Result<()> {
        let built = self.comment_text(&deck, &thread, slide, block, text);
        self.enqueue_studio_comment(deck, thread, built)
    }

    /// Queue a comment's message for the conversation, or report that it could not be built.
    fn enqueue_studio_comment(
        &mut self,
        deck: PathBuf,
        thread: String,
        built: Result<String>,
    ) -> Result<()> {
        let text = match built {
            Ok(text) => text,
            Err(error) => {
                self.studio_failure(deck, thread);
                return Err(error);
            }
        };
        let reason = self
            .studio_wait_reason()
            .or_else(|| (!self.studio.queued.is_empty()).then_some(WaitingReason::Streaming));
        let comment = StudioComment {
            deck,
            thread,
            reason,
        };
        if let Some(reason) = comment.reason {
            self.studio_activity(&comment, ActivityState::Queued { reason }, None);
        }
        self.studio.queued.push_back(text);
        self.studio.comments.push_back(comment);
        self.studio.warned_draft = false;
        Ok(())
    }

    fn studio_wait_reason(&self) -> Option<WaitingReason> {
        if !self.input_text().trim().is_empty() {
            Some(WaitingReason::Draft)
        } else if self.is_streaming() || self.studio.active.is_some() {
            Some(WaitingReason::Streaming)
        } else if self
            .current_model
            .as_deref()
            .is_none_or(|model| self.resolve_model_backend(model).is_none())
        {
            Some(WaitingReason::NoModel)
        } else {
            None
        }
    }

    /// `Comment on decks/a.deck.json · slide 2 (b4: Make room…): text`, the
    /// thread id the model replies with, and the slide as an image. The image
    /// is a durable snapshot, because the message is replayed every turn.
    fn comment_text(
        &self,
        path: &Path,
        thread: &str,
        slide: &str,
        block: Option<&str>,
        text: &str,
    ) -> Result<String> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("studio deck {} is gone", path.display()))?;
        let deck = Deck::check(&source)
            .map_err(|_| anyhow::anyhow!("studio deck {} is invalid", path.display()))?;
        let index = deck
            .slides
            .iter()
            .position(|s| s.id == slide)
            .context("the commented slide no longer exists")?;
        let summary = block
            .and_then(|id| deck.slides[index].node(id))
            .map_or_else(
                || "slide".to_string(),
                |b| match &b.kind {
                    Kind::Text { text, .. } => {
                        let clip: String = text
                            .chars()
                            .map(|c| if c == '\n' { ' ' } else { c })
                            .take(SUMMARY_CHARS)
                            .collect();
                        format!("{}: {clip}", b.id)
                    }
                    _ => b.id.clone(),
                },
            );
        let files = self.files_dir();
        let image = render::render_snapshot(&deck, path, &files, slide)?;
        let shown = path.strip_prefix(&files).unwrap_or(path).display();
        let number = index + 1;
        // The anchored node and the revision, so the model can edit at once
        // instead of reading the whole deck first.
        let assets = path.parent().map(|dir| dir.join("assets"));
        let anchored = block
            .and_then(|id| {
                nexus_core::deck::brief::node_brief(
                    &deck,
                    slide,
                    id,
                    assets.as_deref(),
                    NODE_TEXT_CHARS,
                )
            })
            .map_or_else(String::new, |line| format!("node {line}\n"));
        let revision = deck.revision();
        Ok(format!(
            "Comment on {shown} · slide {number} ({summary}): {text}\nthread {thread} · revision {}\n{anchored}![slide {number}]({image})",
            &revision[..12]
        ))
    }

    /// `Comment on docs/q3.doc.json · b4 "Ship the docs canvas": text`, the thread id
    /// the model replies with, the doc's revision, and the anchored block's text, so
    /// the model can edit without a `read`. No page image: the text is enough.
    fn doc_comment_text(
        &self,
        path: &Path,
        thread: &str,
        block: &str,
        quote: &str,
        text: &str,
    ) -> Result<String> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("studio doc {} is gone", path.display()))?;
        let doc = Doc::check(&source)
            .map_err(|_| anyhow::anyhow!("studio doc {} is invalid", path.display()))?;
        let found = doc
            .block(block)
            .context("the commented block no longer exists")?;
        let flat = |text: &str, limit: usize| -> String {
            text.chars()
                .map(|c| if c == '\n' { ' ' } else { c })
                .take(limit)
                .collect()
        };
        let plain = found.plain_text();
        let named = if quote.is_empty() { &plain } else { quote };
        let shown = path
            .strip_prefix(self.files_dir())
            .unwrap_or(path)
            .display();
        let revision = doc.revision();
        Ok(format!(
            "Comment on {shown} · {block} \"{}\": {text}\nthread {thread} · revision {}\nblock {block} {}: \"{}\"",
            flat(named, SUMMARY_CHARS),
            &revision[..12],
            block_kind(found),
            flat(&plain, NODE_TEXT_CHARS),
        ))
    }

    /// Run from the event loop: apply studio's messages, tell it what the
    /// model changed, and send a queued comment once nothing is in the way.
    pub fn poll_studio(&mut self) {
        let mut received = Vec::new();
        let mut problem = None;
        if let Some(canvas) = &mut self.studio.canvas {
            while let Ok(message) = canvas.messages.try_recv() {
                received.push(message);
            }
            problem = canvas.problem.lock().expect("studio problem").take();
        }
        if let Some(problem) = problem {
            self.push_status(format!("studio: {problem}"));
        }
        for message in received {
            if let Err(error) = self.handle_studio_message(message) {
                self.push_status(format!("studio: {error:#}"));
            }
        }
        let changed = std::mem::take(&mut self.deck_link.lock().expect("deck link").changed);
        for (path, slide) in changed {
            self.studio_progress(&path, &slide);
            self.studio.last_deck = Some(path.clone());
            if let Some(canvas) = &mut self.studio.canvas {
                if !same_file(&canvas.deck, &path) {
                    canvas.deck.clone_from(&path);
                    canvas.send(Message::Open {
                        deck_path: path.clone(),
                    });
                }
                canvas.send(if is_doc(&path) {
                    Message::DocFocus {
                        deck_path: path,
                        block: Some(slide).filter(|block| !block.is_empty()),
                        selection: None,
                    }
                } else {
                    Message::Focus {
                        deck_path: path,
                        slide,
                        blocks: Vec::new(),
                    }
                });
            }
        }
        self.finish_studio_turn();
        self.send_queued_comment();
    }

    fn studio_progress(&mut self, path: &Path, slide: &str) {
        let Some(turn) = &mut self.studio.active else {
            return;
        };
        if !same_file(path, &turn.comment.deck) {
            return;
        }
        if is_doc(path) {
            let next = block_revisions(path);
            let before = turn.edited_blocks.clone();
            for (id, revision) in &next {
                if !turn
                    .blocks
                    .iter()
                    .any(|(old, rev)| old == id && rev == revision)
                {
                    turn.edited_blocks.insert(id.clone());
                }
            }
            for (id, _) in &turn.blocks {
                if !next.iter().any(|(next_id, _)| next_id == id) {
                    turn.edited_blocks.insert(id.clone());
                }
            }
            // A change that touches no block (the title, the design) still counts.
            if turn.edited_blocks == before && !slide.is_empty() {
                turn.edited_blocks.insert(slide.to_string());
            }
            turn.blocks = next;
            if turn.edited_blocks != before {
                let (comment, summary) = (turn.comment.clone(), turn.summary());
                self.studio_activity(&comment, ActivityState::Progress, Some(summary));
            }
            return;
        }
        let next = slide_revisions(path);
        let before = turn.edited.clone();
        for (index, revision) in next.iter().enumerate() {
            if !turn.slides.contains(revision) {
                turn.edited.insert(index + 1);
            }
        }
        for (index, (id, _)) in turn.slides.iter().enumerate() {
            if !next.iter().any(|(next_id, _)| next_id == id) {
                turn.edited.insert(index + 1);
            }
        }
        // Deck-wide changes (for example a theme) need not alter slide JSON.
        if turn.edited == before
            && let Some(index) = next.iter().position(|(id, _)| id == slide)
        {
            turn.edited.insert(index + 1);
        }
        turn.slides = next;
        if turn.edited != before {
            let (comment, summary) = (turn.comment.clone(), turn.summary());
            self.studio_activity(&comment, ActivityState::Progress, Some(summary));
        }
    }

    fn finish_studio_turn(&mut self) {
        let Some(mut turn) = self.studio.active.take() else {
            return;
        };
        if !turn.replied && model_replies(&turn.comment) > turn.replies {
            turn.replied = true;
            self.studio_activity(
                &turn.comment,
                ActivityState::Progress,
                Some("replied in this thread".into()),
            );
        }
        if self.chat_tasks.contains_key(&turn.task) {
            self.studio.active = Some(turn);
            return;
        }
        let outcome = self
            .deck_link
            .lock()
            .expect("deck link")
            .turns
            .remove(&turn.task)
            .flatten();
        let (state, detail) = outcome.map_or_else(
            || (ActivityState::Stopped, "Response stopped".into()),
            |outcome| {
                let summary = turn.summary();
                let detail = if outcome.state == ActivityState::Done && !summary.is_empty() {
                    summary
                } else if outcome.detail.trim().is_empty() {
                    "no changes".into()
                } else {
                    outcome.detail
                };
                (outcome.state, detail)
            },
        );
        self.studio_activity(&turn.comment, state, Some(detail));
    }

    fn update_studio_waiting(&mut self) -> Option<WaitingReason> {
        let reason = self.studio_wait_reason();
        let mut updates = Vec::new();
        for comment in &mut self.studio.comments {
            if reason.is_some() && comment.reason != reason {
                comment.reason = reason;
                if let Some(reason) = reason {
                    updates.push((comment.clone(), reason));
                }
            }
        }
        for (comment, reason) in updates {
            self.studio_activity(&comment, ActivityState::Queued { reason }, None);
        }
        reason
    }

    /// One queued comment per call, and only when it cannot disturb anything:
    /// no reply is streaming, the composer holds no draft, a model is ready.
    fn send_queued_comment(&mut self) {
        if self.studio.queued.is_empty() {
            return;
        }
        let reason = self.update_studio_waiting();
        if reason == Some(WaitingReason::Draft) {
            if !self.studio.warned_draft {
                self.studio.warned_draft = true;
                self.push_status(
                    "studio comment waiting — it sends once your draft is sent or cleared",
                );
            }
            return;
        }
        if reason.is_some() {
            return;
        }
        let text = self.studio.queued.pop_front().expect("a queued comment");
        let comment = self.studio.comments.pop_front().expect("queued identity");
        let (slides, blocks) = if is_doc(&comment.deck) {
            (Vec::new(), block_revisions(&comment.deck))
        } else {
            (slide_revisions(&comment.deck), Vec::new())
        };
        let replies = model_replies(&comment);
        let text = self.with_studio_context(text);
        if let Err(error) = self.core.send_message(text) {
            self.studio_failure(comment.deck, comment.thread);
            self.push_status(format!("studio comment not sent: {error:#}"));
        } else if let Some(task) = self.active_chat_task().map(|task| task.id) {
            self.deck_link
                .lock()
                .expect("deck link")
                .turns
                .insert(task, None);
            self.studio_activity(&comment, ActivityState::Working, None);
            self.studio.active = Some(CommentTurn {
                comment,
                task,
                slides,
                edited: BTreeSet::new(),
                blocks,
                edited_blocks: BTreeSet::new(),
                replies,
                replied: false,
            });
            self.push_status("studio comment sent");
        } else {
            self.studio_failure(comment.deck, comment.thread);
        }
        self.update_studio_waiting();
    }

    /// Append `studio: viewing slide 3, selected b4` to an outgoing message,
    /// once per change of what studio shows.
    pub fn with_studio_context(&mut self, text: String) -> String {
        let Some(focus) = self.studio.focus.clone().filter(|_| self.studio.linked()) else {
            return text;
        };
        if self.studio.told.as_ref() == Some(&focus) {
            return text;
        }
        if focus.doc {
            let line = match (focus.slide.is_empty(), focus.blocks.first()) {
                (true, _) => return text,
                (false, None) => format!("studio: viewing {}", focus.slide),
                (false, Some(selected)) => format!(
                    "studio: viewing {}, selected {:?}",
                    focus.slide,
                    selected.chars().take(NODE_TEXT_CHARS).collect::<String>()
                ),
            };
            self.studio.told = Some(focus);
            return format!("{text}\n\n{line}");
        }
        let number = std::fs::read_to_string(&focus.deck)
            .ok()
            .and_then(|source| Deck::check(&source).ok())
            .and_then(|deck| deck.slides.iter().position(|s| s.id == focus.slide))
            .map_or_else(|| focus.slide.clone(), |index| (index + 1).to_string());
        let selected = if focus.blocks.is_empty() {
            "nothing".to_string()
        } else {
            focus.blocks.join(", ")
        };
        let line = format!("studio: viewing slide {number}, selected {selected}");
        self.studio.told = Some(focus);
        format!("{text}\n\n{line}")
    }
}

/// `nexus-studio --attach <deck>` with piped stdio, found beside this binary
/// first and on `PATH` second, plus a reader thread that forwards its lines
/// and wakes the event loop. EOF becomes a `Bye`.
fn spawn(deck: &Path, wake: Arc<Notify>) -> Result<Canvas> {
    let name = if cfg!(windows) {
        "nexus-studio.exe"
    } else {
        "nexus-studio"
    };
    let beside = std::env::current_exe()?.with_file_name(name);
    let mut child = None;
    for program in [beside, PathBuf::from(name)] {
        match Command::new(&program)
            .arg("--attach")
            .arg(deck)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(started) => {
                child = Some(started);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let child =
        child.context("nexus-studio isn't installed — cargo install --path crates/studio")?;
    attach(child, deck, wake)
}

/// Wrap a child with piped stdin and stdout as the canvas.
fn attach(mut child: Child, deck: &Path, wake: Arc<Notify>) -> Result<Canvas> {
    let input = child.stdin.take().context("studio stdin")?;
    let output = child.stdout.take().context("studio stdout")?;
    let (sender, messages) = mpsc::unbounded_channel();
    let problem = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&problem);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            match Line::decode(&line) {
                Ok(message) => {
                    if sender.send(message).is_err() {
                        return;
                    }
                    wake.notify_one();
                }
                // A window on another link version says so once, instead of
                // seeming to ignore comments.
                Err(error) if error.to_string().contains("update nexus") => {
                    *seen.lock().expect("studio problem") = Some(error.to_string());
                    wake.notify_one();
                }
                Err(_) => {}
            }
        }
        let _ = sender.send(Message::Bye);
        wake.notify_one();
    });
    Ok(Canvas {
        child: Some(child),
        input,
        messages,
        deck: deck.to_path_buf(),
        problem,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::{app::App, db::Db, space::Space};

    fn view(root: &Path) -> AppView {
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("k"),
            Space {
                root: root.to_path_buf(),
            },
        ))
    }

    fn deck_file(app: &AppView) -> PathBuf {
        let path = app.files_dir().join("decks/test.deck.json");
        let deck = Deck::new_from_layouts("Test");
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&deck).unwrap()).unwrap();
        path
    }

    fn comment(path: PathBuf, text: &str) -> Message {
        Message::Comment {
            deck_path: path,
            thread_id: "t1".into(),
            slide: "s1".into(),
            block: Some("b1".into()),
            point: (210, 390),
            text: text.into(),
        }
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp() -> Dir {
        Dir(std::env::temp_dir().join(format!("nexus-studio-link-{}", uuid::Uuid::new_v4())))
    }

    fn ready(app: &mut AppView) {
        app.current_model = Some("a/one".into());
        // No metadata/memory jobs; the request task is aborted before this
        // current-thread test runtime can poll it, so no network is touched.
        app.incognito = true;
    }

    fn start_comment_turn(app: &mut AppView) -> nexus_core::app::ChatTaskId {
        app.poll_studio();
        let task = app.active_chat_task().expect("comment started");
        task.abort.abort();
        task.id
    }

    fn activities<'a>(app: &'a AppView, thread: &str) -> Vec<(ActivityState, Option<&'a str>)> {
        app.studio
            .activities
            .iter()
            .filter_map(|message| match message {
                Message::Activity {
                    thread_id,
                    state,
                    detail,
                    ..
                } if thread_id == thread => Some((*state, detail.as_deref())),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn comment_activity_waits_works_and_reports_every_edited_slide() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        ready(&mut app);
        app.set_input("my draft");
        app.handle_studio_message(comment(path.clone(), "Clarify this"))
            .unwrap();
        assert_eq!(
            activities(&app, "t1"),
            vec![(
                ActivityState::Queued {
                    reason: WaitingReason::Draft
                },
                None
            )]
        );
        app.clear_input();
        let task = start_comment_turn(&mut app);
        let mut deck = Deck::check(&std::fs::read_to_string(&path).unwrap()).unwrap();
        deck.slides[0].notes = "Changed".into();
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&deck).unwrap()).unwrap();
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path.clone(), "s1".into()));
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Progress, Some("edited slide 1")))
        );
        deck.slides[1].notes = "Also changed".into();
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&deck).unwrap()).unwrap();
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path, "s2".into()));
        app.poll_studio();
        app.core
            .on_chat_event(task, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1"),
            vec![
                (
                    ActivityState::Queued {
                        reason: WaitingReason::Draft
                    },
                    None
                ),
                (ActivityState::Working, None),
                (ActivityState::Progress, Some("edited slide 1")),
                (ActivityState::Progress, Some("edited slides 1, 2")),
                (ActivityState::Done, Some("edited slides 1, 2")),
            ]
        );
        assert!(app.deck_link.lock().unwrap().turns.is_empty());
    }

    #[tokio::test]
    async fn comment_activity_single_slide_done_and_discarded_turn_are_observable() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        ready(&mut app);
        app.set_input("draft");
        app.handle_studio_message(comment(path.clone(), "Fix slide one"))
            .unwrap();
        app.clear_input();
        let task = start_comment_turn(&mut app);
        let mut deck = Deck::check(&std::fs::read_to_string(&path).unwrap()).unwrap();
        deck.slides[0].notes = "Fixed".into();
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&deck).unwrap()).unwrap();
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path.clone(), "s1".into()));
        app.core
            .on_chat_event(task, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1"),
            vec![
                (
                    ActivityState::Queued {
                        reason: WaitingReason::Draft
                    },
                    None
                ),
                (ActivityState::Working, None),
                (ActivityState::Progress, Some("edited slide 1")),
                (ActivityState::Done, Some("edited slide 1")),
            ]
        );
        app.handle_studio_message(comment(path, "Try again"))
            .unwrap();
        start_comment_turn(&mut app);
        assert!(
            app.studio.has_queued(),
            "active turns keep the bridge polling"
        );
        let session = app.session.as_ref().unwrap().id.clone();
        app.core.discard_chat_task(&session);
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Stopped, Some("Response stopped")))
        );
        assert!(!app.studio.has_queued());
    }

    #[tokio::test]
    async fn comment_activity_failure_is_capped_scrubbed_and_cancel_is_stopped() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        ready(&mut app);
        app.handle_studio_message(comment(path.clone(), "Try this"))
            .unwrap();
        let task = start_comment_turn(&mut app);
        let error = format!("Bearer sk-123456789012345678901234 {}", "é".repeat(300));
        app.core
            .on_chat_event(task, nexus_core::provider::StreamEvent::Error(error))
            .unwrap();
        app.poll_studio();
        let (state, detail) = activities(&app, "t1").pop().unwrap();
        assert_eq!(state, ActivityState::Failed);
        let detail = detail.unwrap();
        assert!(detail.contains("[REDACTED]"));
        assert!(!detail.contains("sk-123"));
        assert_eq!(detail.chars().count(), 240);
        app.handle_studio_message(comment(path, "Again")).unwrap();
        let task = start_comment_turn(&mut app);
        app.core.cancel_chat_task(task).unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Stopped, Some("Response stopped")))
        );
    }

    #[tokio::test]
    async fn two_comments_have_ordered_independent_turns_and_plain_answer_excerpts() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        ready(&mut app);
        app.set_input("draft");
        app.handle_studio_message(comment(path.clone(), "First"))
            .unwrap();
        let mut second = comment(path, "Second");
        if let Message::Comment { thread_id, .. } = &mut second {
            *thread_id = "t2".into();
        }
        app.handle_studio_message(second).unwrap();
        app.clear_input();
        let first = start_comment_turn(&mut app);
        app.poll_studio();
        assert_eq!(
            activities(&app, "t2").last(),
            Some(&(
                ActivityState::Queued {
                    reason: WaitingReason::Streaming
                },
                None
            ))
        );
        app.core
            .on_chat_event(
                first,
                nexus_core::provider::StreamEvent::Token("**Which** audience?".into()),
            )
            .unwrap();
        app.core
            .on_chat_event(first, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        let second = start_comment_turn(&mut app);
        assert_ne!(first, second);
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Done, Some("Which audience?")))
        );
        app.core
            .on_chat_event(second, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t2"),
            vec![
                (
                    ActivityState::Queued {
                        reason: WaitingReason::Draft
                    },
                    None
                ),
                (
                    ActivityState::Queued {
                        reason: WaitingReason::Streaming
                    },
                    None
                ),
                (ActivityState::Working, None),
                (ActivityState::Done, Some("no changes")),
            ]
        );
    }

    #[tokio::test]
    async fn comment_activity_tracks_model_replies_in_its_own_thread() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        ready(&mut app);
        app.handle_studio_message(comment(path.clone(), "Explain this"))
            .unwrap();
        let task = start_comment_turn(&mut app);
        let thread = feedback::Thread {
            id: "t1".into(),
            anchor: feedback::Anchor {
                slide: "s1".into(),
                block: None,
                x: 0,
                y: 0,
                px: true,
            },
            severity: feedback::Severity::Consider,
            created: feedback::now(),
            resolved: None,
            messages: vec![feedback::Message::new(
                feedback::Role::Model,
                "Explained".into(),
                vec![],
            )],
        };
        feedback::save(&path, &[thread]).unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Progress, Some("replied in this thread")))
        );
        app.core
            .on_chat_event(task, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Done, Some("replied in this thread")))
        );
    }

    #[tokio::test]
    async fn comment_activity_reason_changes_and_invalid_send_fails() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        app.handle_studio_message(comment(path.clone(), "Wait"))
            .unwrap();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(
                ActivityState::Queued {
                    reason: WaitingReason::NoModel
                },
                None
            ))
        );
        app.set_input("draft");
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(
                ActivityState::Queued {
                    reason: WaitingReason::Draft
                },
                None
            ))
        );
        std::fs::remove_file(&path).unwrap();
        assert!(app.handle_studio_message(comment(path, "Gone")).is_err());
        assert_eq!(
            activities(&app, "t1").last().unwrap().0,
            ActivityState::Failed
        );
    }

    #[tokio::test]
    async fn a_comment_becomes_a_queued_message_with_the_slide_image() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        app.handle_studio_message(comment(path, "Clarify this"))
            .unwrap();
        assert!(app.studio.has_queued());
        let queued = app.studio.queued[0].clone();
        assert!(
            queued
                .starts_with("Comment on decks/test.deck.json · slide 1 (b1: Test): Clarify this"),
            "{queued}"
        );
        assert!(queued.contains("thread t1 · revision "), "{queued}");
        // The anchored node rides along (id, kind, rect, style, text), so the
        // model can edit at once; the deck's revision is the 12 characters
        // `read` and `edit` print.
        let deck = Deck::check(&std::fs::read_to_string(deck_file(&app)).unwrap()).unwrap();
        assert!(
            queued.contains(&format!("revision {}\n", &deck.revision()[..12])),
            "{queued}"
        );
        let node = queued
            .lines()
            .find(|line| line.starts_with("node b1 text "))
            .unwrap_or_else(|| panic!("{queued}"));
        assert!(
            node.contains(" style ") && node.ends_with(": \"Test\""),
            "{node}"
        );
        let image = queued
            .split("![slide 1](")
            .nth(1)
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or_else(|| panic!("{queued}"));
        assert!(image.starts_with("decks/.renders/test/snap-"), "{image}");
        assert!(app.files_dir().join(image).is_file());
    }

    #[tokio::test]
    async fn a_typed_draft_is_never_overwritten_and_streaming_keeps_the_comment_queued() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        app.handle_studio_message(comment(path, "Clarify this"))
            .unwrap();

        app.set_input("keep my draft");
        app.poll_studio();
        assert_eq!(app.input_text(), "keep my draft");
        assert!(app.studio.has_queued(), "a draft keeps the comment waiting");
        assert!(app.studio.warned_draft);

        // An empty composer is not enough while a reply is still streaming.
        app.clear_input();
        app.current_model = Some("a/one".into());
        let abort = tokio::spawn(async {}).abort_handle();
        app.core.chat_tasks.insert(
            1,
            nexus_core::app::ChatTask {
                id: 1,
                session_id: "s".into(),
                session_title: "streaming".into(),
                space_id: "space".into(),
                model: "a/one".into(),
                model_id: "one".into(),
                backend: nexus_core::provider::BackendTag::OpenRouter,
                incognito: false,
                buffer: "partial".into(),
                thinking: String::new(),
                tool_status: None,
                usage: None,
                usage_row_id: None,
                started: std::time::Instant::now(),
                thinking_idx: 0,
                spinner_color: nexus_core::app::SpinnerColor::Green,
                abort,
            },
        );
        app.poll_studio();
        assert!(app.studio.has_queued() && app.messages.is_empty());

        // With no model chosen there is nothing to send to either.
        app.core.chat_tasks.clear();
        app.current_model = None;
        app.poll_studio();
        assert!(app.studio.has_queued() && app.messages.is_empty());
        assert_eq!(app.input_text(), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn focus_is_told_to_the_model_once_per_change_while_a_window_is_attached() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        let focus = |slide: &str, blocks: &[&str]| Message::Focus {
            deck_path: path.clone(),
            slide: slide.into(),
            blocks: blocks.iter().map(|b| (*b).into()).collect(),
        };
        app.handle_studio_message(focus("s2", &["b3"])).unwrap();
        // The deck tool reads the same focus.
        let shared = app.deck_link.lock().unwrap().focus.clone();
        assert_eq!(shared.map(|f| f.1), Some("s2".into()));
        // No window attached: the model hears nothing about it.
        assert_eq!(app.with_studio_context("hi".into()), "hi");

        // `cat` stands in for a window: it accepts lines and echoes them back.
        let cat = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        app.studio.canvas = Some(attach(cat, &path, app.studio_wake()).unwrap());
        assert!(app.studio.linked());
        assert_eq!(
            app.with_studio_context("hi".into()),
            "hi\n\nstudio: viewing slide 2, selected b3"
        );
        assert_eq!(
            app.with_studio_context("again".into()),
            "again",
            "unchanged focus is not repeated"
        );
        app.handle_studio_message(focus("s1", &[])).unwrap();
        assert_eq!(
            app.with_studio_context("next".into()),
            "next\n\nstudio: viewing slide 1, selected nothing"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_model_changing_a_deck_moves_the_attached_window_and_a_closed_window_is_noticed() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        let cat = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let wake = app.studio_wake();
        app.studio.canvas = Some(attach(cat, &path, Arc::clone(&wake)).unwrap());
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path.clone(), "s2".into()));
        app.poll_studio();
        // `cat` echoes what the terminal sent, so the Focus comes back to us.
        tokio::time::timeout(std::time::Duration::from_secs(5), wake.notified())
            .await
            .expect("the window echoed the focus");
        app.poll_studio();
        let focus = app.deck_link.lock().unwrap().focus.clone().unwrap();
        assert_eq!(focus.1, "s2");
        // Dropping the canvas says Bye; a window that closes on its own does too.
        app.handle_studio_message(Message::Bye).unwrap();
        assert!(!app.studio.linked());
    }

    #[tokio::test]
    async fn model_changes_are_drained_without_a_window() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path.clone(), "s2".into()));
        app.poll_studio();
        assert!(app.deck_link.lock().unwrap().changed.is_empty());
        assert_eq!(app.studio.last_deck, Some(path));
    }
    // -- docs ---------------------------------------------------------------------------

    fn doc_file(app: &AppView) -> PathBuf {
        let path = app.files_dir().join("docs/test.doc.json");
        let mut doc = Doc::from_design("Test", &nexus_core::design::Design::default());
        doc.blocks = doc::markdown::parse_document(
            "# Test\n\nShip the docs canvas this quarter.\n\n- one\n- two\n",
            true,
        )
        .blocks;
        let doc = Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap();
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&doc).unwrap()).unwrap();
        path
    }

    fn doc_comment(path: PathBuf, text: &str) -> Message {
        Message::DocComment {
            deck_path: path,
            thread_id: "t1".into(),
            block: "b2".into(),
            range: Some(doc::feedback::Range { start: 0, end: 4 }),
            quote: "Ship".into(),
            text: text.into(),
        }
    }

    #[tokio::test]
    async fn a_doc_comment_becomes_a_queued_message_with_the_block_text_and_no_image() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = doc_file(&app);
        app.handle_studio_message(doc_comment(path.clone(), "Say which quarter"))
            .unwrap();
        assert!(app.studio.has_queued());
        let queued = app.studio.queued[0].clone();
        assert!(
            queued.starts_with("Comment on docs/test.doc.json · b2 \"Ship\": Say which quarter\n"),
            "{queued}"
        );
        let doc = Doc::check(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            queued.contains(&format!("thread t1 · revision {}\n", &doc.revision()[..12])),
            "{queued}"
        );
        assert!(
            queued.ends_with("block b2 paragraph: \"Ship the docs canvas this quarter.\""),
            "{queued}"
        );
        assert!(
            !queued.contains("!["),
            "no page image rides along: {queued}"
        );
        // With no quote (a whole-block comment) the block's own text names it, clipped.
        let mut whole = doc_comment(path.clone(), "Tighten");
        if let Message::DocComment { quote, range, .. } = &mut whole {
            quote.clear();
            *range = None;
        }
        app.studio.queued.clear();
        app.handle_studio_message(whole).unwrap();
        assert!(
            app.studio.queued[0].starts_with(
                "Comment on docs/test.doc.json · b2 \"Ship the docs canvas this quarter.\": Tighten"
            ),
            "{}",
            app.studio.queued[0]
        );
        // The block text is bounded, and a missing block fails the comment.
        let long = "word ".repeat(200);
        let mut doc = doc;
        doc.blocks[1] = doc::Block {
            id: "b2".into(),
            style: None,
            kind: doc::Kind::Paragraph { text: long },
        };
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&doc).unwrap()).unwrap();
        app.studio.queued.clear();
        app.handle_studio_message(doc_comment(path.clone(), "Long"))
            .unwrap();
        let line = app.studio.queued[0].lines().last().unwrap().to_string();
        assert!(
            line.chars().count() <= "block b2 paragraph: \"\"".chars().count() + 300,
            "{line}"
        );
        let mut gone = doc_comment(path, "Where");
        if let Message::DocComment { block, .. } = &mut gone {
            *block = "b99".into();
        }
        assert!(app.handle_studio_message(gone).is_err());
        assert_eq!(
            activities(&app, "t1").last().unwrap().0,
            ActivityState::Failed
        );
    }

    #[tokio::test]
    async fn a_reply_in_a_doc_thread_reads_the_doc_sidecar() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = doc_file(&app);
        let thread = doc::feedback::Thread {
            id: "t1".into(),
            anchor: doc::feedback::Anchor {
                block: Some("b2".into()),
                range: Some(doc::feedback::Range { start: 0, end: 4 }),
                quote: "Ship".into(),
            },
            severity: feedback::Severity::Consider,
            created: feedback::now(),
            resolved: None,
            messages: vec![doc::feedback::Message::new_for(
                feedback::Role::You,
                "First".into(),
                vec![],
            )],
        };
        doc::feedback::save(&path, &[thread]).unwrap();
        app.handle_studio_message(Message::Reply {
            deck_path: path.clone(),
            thread_id: "t1".into(),
            text: "And the next one?".into(),
        })
        .unwrap();
        assert!(
            app.studio.queued[0]
                .starts_with("Comment on docs/test.doc.json · b2 \"Ship\": And the next one?"),
            "{}",
            app.studio.queued[0]
        );
        assert!(
            app.handle_studio_message(Message::Reply {
                deck_path: path,
                thread_id: "ghost".into(),
                text: "x".into(),
            })
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_doc_focus_is_told_once_per_change_with_the_selection() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = doc_file(&app);
        let focus = |block: Option<&str>, selection: Option<&str>| Message::DocFocus {
            deck_path: path.clone(),
            block: block.map(Into::into),
            selection: selection.map(Into::into),
        };
        app.handle_studio_message(focus(Some("b2"), Some("docs canvas")))
            .unwrap();
        let shared = app.deck_link.lock().unwrap().focus.clone();
        assert_eq!(
            shared,
            Some((path.clone(), "b2".into(), vec!["docs canvas".into()]))
        );
        assert_eq!(
            app.with_studio_context("hi".into()),
            "hi",
            "no window, no news"
        );
        let cat = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        app.studio.canvas = Some(attach(cat, &path, app.studio_wake()).unwrap());
        assert_eq!(
            app.with_studio_context("hi".into()),
            "hi\n\nstudio: viewing b2, selected \"docs canvas\""
        );
        assert_eq!(app.with_studio_context("again".into()), "again");
        app.handle_studio_message(focus(Some("b4"), None)).unwrap();
        assert_eq!(
            app.with_studio_context("next".into()),
            "next\n\nstudio: viewing b4"
        );
        app.handle_studio_message(focus(None, None)).unwrap();
        assert_eq!(app.with_studio_context("quiet".into()), "quiet");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_model_editing_a_doc_opens_it_and_focuses_the_first_changed_block() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = doc_file(&app);
        let other = deck_file(&app);
        let cat = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let wake = app.studio_wake();
        // The window is showing a deck, so the doc edit must open the doc first.
        app.studio.canvas = Some(attach(cat, &other, Arc::clone(&wake)).unwrap());
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path.clone(), "b2".into()));
        app.poll_studio();
        assert_eq!(app.studio.last_deck, Some(path.clone()));
        // `cat` echoes what was sent: the doc focus comes back as a `DocFocus`.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                wake.notified().await;
                app.poll_studio();
                if app.studio.focus.as_ref().is_some_and(|f| f.doc) {
                    break;
                }
            }
        })
        .await
        .expect("the window echoed the focus");
        let focus = app.deck_link.lock().unwrap().focus.clone().unwrap();
        assert_eq!((focus.0, focus.1), (path, "b2".into()));
        assert!(
            app.studio
                .canvas
                .as_ref()
                .unwrap()
                .deck
                .ends_with("test.doc.json")
        );
    }

    #[tokio::test]
    async fn doc_comment_activity_reports_the_blocks_the_model_edited() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = doc_file(&app);
        ready(&mut app);
        app.handle_studio_message(doc_comment(path.clone(), "Fix this"))
            .unwrap();
        let task = start_comment_turn(&mut app);
        let mut doc = Doc::check(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc.blocks[1] = doc::Block {
            id: "b2".into(),
            style: None,
            kind: doc::Kind::Paragraph {
                text: "Ship the docs canvas in Q3.".into(),
            },
        };
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&doc).unwrap()).unwrap();
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path.clone(), "b2".into()));
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Progress, Some("edited block b2")))
        );
        doc.blocks[2] = doc::Block {
            id: "b3".into(),
            style: None,
            kind: doc::Kind::Divider,
        };
        nexus_core::deck::write_atomic(&path, &serde_json::to_vec(&doc).unwrap()).unwrap();
        app.deck_link
            .lock()
            .unwrap()
            .changed
            .push((path, "b3".into()));
        app.poll_studio();
        app.core
            .on_chat_event(task, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        app.poll_studio();
        assert_eq!(
            activities(&app, "t1").last(),
            Some(&(ActivityState::Done, Some("edited blocks b2, b3")))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_window_on_another_link_version_is_reported_once() {
        let dir = temp();
        let mut app = view(&dir.0);
        let path = deck_file(&app);
        let old = Command::new("sh")
            .arg("-c")
            .arg("echo '{\"version\":1,\"type\":\"Hello\",\"deck_path\":\"a\"}'; cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let wake = app.studio_wake();
        app.studio.canvas = Some(attach(old, &path, Arc::clone(&wake)).unwrap());
        tokio::time::timeout(std::time::Duration::from_secs(5), wake.notified())
            .await
            .expect("the old window spoke");
        app.poll_studio();
        let status = app.core.last_status();
        assert!(
            status.contains("update nexus-studio"),
            "the status line tells the user to update nexus-studio: {status}"
        );
        assert!(
            app.studio.last_deck.is_none(),
            "the old Hello was not believed"
        );
    }
}
