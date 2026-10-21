//! Local comment threads and chat. Suggested edits never mutate a deck until applied.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, ensure};
use nexus_core::deck::feedback::{self, Anchor, Kind, Role, Severity};
use nexus_core::deck::suggest::{Selection, Suggestion};
use nexus_core::deck::{self, Deck, Op};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::jobs::Job;
use super::{Backend, Event, atomic_write, read_bounded};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditStatus {
    Pending,
    Applied,
    Dismissed,
    Outdated,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    #[serde(flatten)]
    pub proposal: Suggestion,
    pub status: EditStatus,
    pub guards: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub text: String,
    pub created: u64,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub anchor: Anchor,
    pub severity: Severity,
    pub created: u64,
    pub resolved: Option<u64>,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub threads: Vec<Thread>,
    pub chat: Vec<Message>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs()
}
fn message(role: Role, text: String, edits: Vec<Edit>) -> Message {
    Message {
        id: Uuid::new_v4().to_string(),
        role,
        text,
        created: now(),
        edits,
    }
}
fn context(messages: &[Message]) -> Vec<feedback::Message> {
    messages
        .iter()
        .map(|m| feedback::Message {
            role: m.role.clone(),
            text: m.text.clone(),
        })
        .collect()
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
fn proposal(base: &Deck, proposal: Suggestion, anchor: Option<&Anchor>) -> Edit {
    let mut keys = Vec::new();
    if let Some(anchor) = anchor {
        keys.push(
            anchor
                .block
                .clone()
                .unwrap_or_else(|| format!("slide:{}", anchor.slide)),
        );
    }
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
    Edit {
        guards: keys
            .into_iter()
            .map(|k| {
                let value = snapshot(base, &k);
                (k, value)
            })
            .collect(),
        proposal,
        status: EditStatus::Pending,
    }
}

impl State {
    fn edits_mut(&mut self) -> impl Iterator<Item = &mut Edit> {
        self.threads
            .iter_mut()
            .flat_map(|t| &mut t.messages)
            .chain(self.chat.iter_mut())
            .flat_map(|m| &mut m.edits)
    }
    fn edit(&self, owner: &str, id: &str) -> Option<&Edit> {
        self.threads
            .iter()
            .filter(|t| t.id == owner)
            .flat_map(|t| &t.messages)
            .chain(self.chat.iter().filter(|m| m.id == owner))
            .flat_map(|m| &m.edits)
            .find(|e| e.proposal.id == id)
    }
    fn prune(&mut self) {
        let cutoff = now().saturating_sub(30 * 24 * 60 * 60);
        self.threads
            .retain(|t| t.resolved.is_none_or(|time| time >= cutoff));
        if self.threads.len() > 200 {
            self.threads.drain(..self.threads.len() - 200);
        }
        if self.chat.len() > 100 {
            self.chat.drain(..self.chat.len() - 100);
        }
    }
}

impl Backend {
    pub(super) fn feedback_event(&self, notes: Vec<String>) -> Event {
        Event::Feedback {
            threads: self.feedback.threads.clone(),
            chat: self.feedback.chat.clone(),
            notes,
        }
    }
    pub(super) fn comment_create(
        &mut self,
        anchor: Anchor,
        text: String,
        send: bool,
    ) -> Result<Event> {
        anchor.check(&self.deck).map_err(anyhow::Error::msg)?;
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a comment (at most 2000 characters)"
        );
        if send {
            self.can_feedback()?;
        }
        self.feedback.prune();
        ensure!(
            self.feedback.threads.len() < 200,
            "This deck has 200 threads; resolve older threads first"
        );
        let id = Uuid::new_v4().to_string();
        self.feedback.threads.push(Thread {
            id: id.clone(),
            anchor,
            severity: Severity::Consider,
            created: now(),
            resolved: None,
            messages: vec![message(Role::You, text, vec![])],
        });
        self.persist_feedback()?;
        if send {
            self.ask_feedback(Kind::Thread, Some(id), Selection::default())
        } else {
            Ok(self.feedback_event(vec![]))
        }
    }
    pub(super) fn comment_reply(&mut self, id: &str, text: String, send: bool) -> Result<Event> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a reply (at most 2000 characters)"
        );
        if send {
            self.can_feedback()?;
        }
        let thread = self
            .feedback
            .threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?;
        thread
            .anchor
            .check(&self.deck)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            thread.messages.len() < 100,
            "Thread has reached 100 messages"
        );
        thread.messages.push(message(Role::You, text, vec![]));
        self.persist_feedback()?;
        if send {
            self.ask_feedback(Kind::Thread, Some(id.into()), Selection::default())
        } else {
            Ok(self.feedback_event(vec![]))
        }
    }
    pub(super) fn comment_resolve(&mut self, id: &str, resolved: bool) -> Result<Event> {
        self.feedback
            .threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?
            .resolved = resolved.then(now);
        self.persist_feedback()?;
        Ok(self.feedback_event(vec![]))
    }
    fn can_feedback(&self) -> Result<()> {
        ensure!(self.host.is_some(), "Host connection is not configured");
        ensure!(!self.suggesting, "Already asking nexus");
        Ok(())
    }
    pub(super) fn chat(&mut self, text: String, selection: Selection) -> Result<Event> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a message (at most 2000 characters)"
        );
        self.can_feedback()?;
        self.feedback.chat.push(message(Role::You, text, vec![]));
        self.persist_feedback()?;
        self.ask_feedback(Kind::Chat, None, selection)
    }
    pub(super) fn ask_feedback(
        &mut self,
        kind: Kind,
        thread: Option<String>,
        selection: Selection,
    ) -> Result<Event> {
        self.can_feedback()?;
        let anchored = thread
            .as_ref()
            .and_then(|id| self.feedback.threads.iter().find(|t| t.id == *id));
        let request = feedback::Request {
            kind,
            deck: self.deck.clone(),
            revision: self.deck.revision(),
            selection,
            anchor: anchored.map(|t| t.anchor.clone()),
            messages: context(
                anchored.map_or(self.feedback.chat.as_slice(), |t| t.messages.as_slice()),
            ),
            instruction: if kind == Kind::Review {
                "Review this deck using the commenting principles.".into()
            } else {
                String::new()
            },
            model: None,
        };
        ensure!(request.check().is_ok(), "Invalid feedback context");
        self.request_host(Job::Feedback {
            request: Box::new(request),
            thread,
        })
    }
    pub(super) fn receive_feedback(
        &mut self,
        request: &feedback::Request,
        target: Option<String>,
        response: feedback::Response,
    ) -> Result<Event> {
        ensure!(
            response.revision == request.revision,
            "Host returned a different revision"
        );
        let mut notes = response.notes;
        if let Some(id) = target {
            let thread = self
                .feedback
                .threads
                .iter_mut()
                .find(|t| t.id == id)
                .context("Thread no longer exists")?;
            let edits: Vec<_> = response
                .edit
                .into_iter()
                .map(|e| proposal(&request.deck, e, Some(&thread.anchor)))
                .collect();
            if !response.text.is_empty() || !edits.is_empty() {
                thread
                    .messages
                    .push(message(Role::Model, response.text, edits));
            }
        } else {
            let edits: Vec<_> = response
                .edit
                .into_iter()
                .chain(response.options)
                .map(|e| proposal(&request.deck, e, None))
                .collect();
            if !response.text.is_empty() || !edits.is_empty() {
                self.feedback
                    .chat
                    .push(message(Role::Model, response.text, edits));
            }
        }
        self.feedback.prune();
        for comment in response.comments.into_iter().take(8) {
            if self.feedback.threads.len() == 200 {
                notes.push("Thread limit reached; remaining review comments were not saved".into());
                break;
            }
            if comment.anchor.check(&self.deck).is_err() {
                notes.push(
                    "A reviewed layer was removed while asking nexus; its comment was not saved"
                        .into(),
                );
                continue;
            }
            let edits = comment
                .edit
                .into_iter()
                .map(|e| proposal(&request.deck, e, Some(&comment.anchor)))
                .collect();
            self.feedback.threads.push(Thread {
                id: Uuid::new_v4().to_string(),
                anchor: comment.anchor,
                severity: comment.severity,
                created: now(),
                resolved: None,
                messages: vec![message(Role::Model, comment.text, edits)],
            });
        }
        self.recheck_feedback();
        self.persist_feedback()?;
        Ok(self.feedback_event(notes))
    }
    pub(super) fn preview_edit(&self, owner: &str, id: &str) -> Result<Event> {
        let edit = self.feedback.edit(owner, id).context("Unknown edit")?;
        ensure!(edit.status == EditStatus::Pending, "outdated — ask again");
        Ok(Event::Preview {
            deck: deck::apply(&self.deck, &edit.proposal.ops)?,
        })
    }
    pub(super) fn apply_edit(&mut self, owner: &str, id: &str) -> Result<()> {
        let edit = self.feedback.edit(owner, id).context("Unknown edit")?;
        ensure!(edit.status == EditStatus::Pending, "outdated — ask again");
        let next = deck::apply(&self.deck, &edit.proposal.ops)?;
        self.feedback
            .edits_mut()
            .find(|e| e.proposal.id == id)
            .context("Unknown edit")?
            .status = EditStatus::Applied;
        self.replace(next);
        self.recheck_feedback();
        self.persist_feedback()
    }
    pub(super) fn dismiss_edit(&mut self, owner: &str, id: &str) -> Result<Event> {
        ensure!(self.feedback.edit(owner, id).is_some(), "Unknown edit");
        self.feedback
            .edits_mut()
            .find(|e| e.proposal.id == id)
            .context("Unknown edit")?
            .status = EditStatus::Dismissed;
        self.persist_feedback()?;
        Ok(self.feedback_event(vec![]))
    }
    pub(super) fn recheck_feedback(&mut self) {
        for edit in self.feedback.edits_mut() {
            if matches!(edit.status, EditStatus::Pending | EditStatus::Outdated) {
                edit.status = if edit
                    .guards
                    .iter()
                    .any(|(k, v)| snapshot(&self.deck, k) != *v)
                    || deck::apply(&self.deck, &edit.proposal.ops).is_err()
                {
                    EditStatus::Outdated
                } else {
                    EditStatus::Pending
                };
            }
        }
    }
    fn sidecar(&self, suffix: &str) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push(suffix);
        path.into()
    }
    pub(super) fn load_feedback(&mut self) -> Result<()> {
        let comments = self.sidecar(".comments.json");
        let chat = self.sidecar(".chat.json");
        self.feedback = State {
            threads: if comments.exists() {
                serde_json::from_slice(&read_bounded(&comments, 128 * 1024 * 1024)?)?
            } else {
                vec![]
            },
            chat: if chat.exists() {
                serde_json::from_slice(&read_bounded(&chat, 128 * 1024 * 1024)?)?
            } else {
                vec![]
            },
        };
        self.feedback.prune();
        self.recheck_feedback();
        if comments.exists() || chat.exists() {
            self.persist_feedback()?;
        }
        Ok(())
    }
    pub(super) fn persist_feedback(&mut self) -> Result<()> {
        self.feedback.prune();
        atomic_write(
            &self.sidecar(".comments.json"),
            &serde_json::to_vec_pretty(&self.feedback.threads)?,
        )?;
        atomic_write(
            &self.sidecar(".chat.json"),
            &serde_json::to_vec_pretty(&self.feedback.chat)?,
        )
    }
}

#[cfg(test)]
mod tests;
