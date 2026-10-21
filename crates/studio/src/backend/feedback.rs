//! Local comments share the model tool's sidecar; Send uses only the link.

use anyhow::{Context as _, Result, ensure};
use nexus_core::deck;
use nexus_core::deck::feedback::{self, Anchor, Edit, EditStatus, Message, Role, Severity, Thread};
use nexus_core::deck::link::Message as LinkMessage;
use uuid::Uuid;

use super::{Backend, Event, stamp};

impl Backend {
    pub(super) fn feedback_event(&self, notes: Vec<String>) -> Event {
        Event::Feedback {
            threads: self.feedback.clone(),
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
        ensure!(
            !send || self.linked,
            "Not linked to a conversation — open it from nexus with /studio"
        );
        ensure!(
            self.feedback.len() < 200,
            "This deck has 200 threads; resolve older threads first"
        );
        let id = Uuid::new_v4().to_string();
        self.feedback.push(Thread {
            id: id.clone(),
            anchor: anchor.clone(),
            severity: Severity::Consider,
            created: feedback::now(),
            resolved: None,
            messages: vec![Message::new(Role::You, text.clone(), vec![])],
        });
        self.persist_feedback()?;
        if send {
            self.outgoing.push(LinkMessage::Comment {
                deck_path: self.path.clone(),
                thread_id: id,
                slide: anchor.slide,
                block: anchor.block,
                point: (anchor.x, anchor.y),
                text,
            });
        }
        Ok(self.feedback_event(vec![]))
    }
    pub(super) fn comment_reply(&mut self, id: &str, text: String, send: bool) -> Result<Event> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a reply (at most 2000 characters)"
        );
        ensure!(
            !send || self.linked,
            "Not linked to a conversation — open it from nexus with /studio"
        );
        let thread = self
            .feedback
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?;
        ensure!(
            thread.messages.len() < 100,
            "Thread has reached 100 messages"
        );
        thread
            .messages
            .push(Message::new(Role::You, text.clone(), vec![]));
        self.persist_feedback()?;
        if send {
            self.outgoing.push(LinkMessage::Reply {
                deck_path: self.path.clone(),
                thread_id: id.into(),
                text,
            });
        }
        Ok(self.feedback_event(vec![]))
    }
    pub(super) fn comment_resolve(&mut self, id: &str, resolved: bool) -> Result<Event> {
        self.feedback
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?
            .resolved = resolved.then(feedback::now);
        self.persist_feedback()?;
        Ok(self.feedback_event(vec![]))
    }
    fn edit(&self, owner: &str, id: &str) -> Result<&Edit> {
        self.feedback
            .iter()
            .filter(|t| t.id == owner)
            .flat_map(|t| &t.messages)
            .flat_map(|m| &m.edits)
            .find(|e| e.proposal.id == id)
            .context("Unknown edit")
    }
    pub(super) fn preview_edit(&self, owner: &str, id: &str) -> Result<Event> {
        let edit = self.edit(owner, id)?;
        ensure!(
            edit.status == EditStatus::Pending,
            "Outdated — ask again in nexus"
        );
        Ok(Event::Preview {
            deck: deck::apply(&self.deck, &edit.proposal.ops)?,
        })
    }
    pub(super) fn apply_edit(&mut self, owner: &str, id: &str) -> Result<()> {
        let edit = self.edit(owner, id)?;
        ensure!(
            edit.status == EditStatus::Pending,
            "Outdated — ask again in nexus"
        );
        let next = deck::apply(&self.deck, &edit.proposal.ops)?;
        self.replace(next)?;
        self.edit_status(id, EditStatus::Applied);
        Ok(())
    }
    fn edit_status(&mut self, id: &str, status: EditStatus) {
        for edit in self
            .feedback
            .iter_mut()
            .flat_map(|t| &mut t.messages)
            .flat_map(|m| &mut m.edits)
        {
            if edit.proposal.id == id {
                edit.status = status;
            }
        }
    }
    pub(super) fn dismiss_edit(&mut self, owner: &str, id: &str) -> Result<Event> {
        self.edit(owner, id)?;
        self.edit_status(id, EditStatus::Dismissed);
        self.persist_feedback()?;
        Ok(self.feedback_event(vec![]))
    }
    pub(super) fn recheck_feedback(&mut self) {
        for edit in self
            .feedback
            .iter_mut()
            .flat_map(|t| &mut t.messages)
            .flat_map(|m| &mut m.edits)
        {
            edit.recheck(&self.deck);
        }
    }
    pub(super) fn load_feedback(&mut self) -> Result<()> {
        self.feedback = feedback::load(&self.path)?;
        self.comments_stamp = stamp(&feedback::sidecar(&self.path));
        self.recheck_feedback();
        Ok(())
    }
    pub(super) fn persist_feedback(&mut self) -> Result<()> {
        feedback::save(&self.path, &self.feedback)?;
        self.comments_stamp = stamp(&feedback::sidecar(&self.path));
        Ok(())
    }
}
