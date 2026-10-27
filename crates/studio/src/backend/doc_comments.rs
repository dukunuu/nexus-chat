//! Doc comments share the model tool's sidecar; Send uses only the link.
//! Suggestions are `replace_text` and other operations the reader applies,
//! previews or dismisses, each guarded against edits made since.

use anyhow::{Context as _, Result, ensure};
use nexus_core::deck::link::Message as LinkMessage;
use nexus_core::doc::{self, Block, feedback as comments};
use uuid::Uuid;

use super::{Backend, Event};

impl Backend {
    pub(super) fn doc_comment_create(
        &mut self,
        anchor: comments::Anchor,
        text: String,
        send: bool,
    ) -> Result<Event> {
        let block = anchor
            .block
            .clone()
            .context("Select text or a block to comment on")?;
        anchor
            .check(&self.session()?.doc)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a comment (at most 2000 characters)"
        );
        ensure!(
            !send || self.linked,
            "Not linked to a conversation — open it from nexus with /studio"
        );
        ensure!(
            self.session()?.threads.len() < 200,
            "This doc has 200 threads; resolve older threads first"
        );
        let id = Uuid::new_v4().to_string();
        self.session_mut_threads()?.push(comments::Thread {
            id: id.clone(),
            anchor: anchor.clone(),
            severity: comments::Severity::Consider,
            created: comments::now(),
            resolved: None,
            messages: vec![comments::Message::new_for(
                comments::Role::You,
                text.clone(),
                vec![],
            )],
        });
        self.persist_doc_feedback()?;
        if send {
            self.outgoing.push(LinkMessage::DocComment {
                deck_path: self.path.clone(),
                thread_id: id,
                block,
                range: anchor.range,
                quote: anchor.quote,
                text,
            });
        }
        Ok(self.doc_feedback_event(vec![]))
    }

    pub(super) fn doc_comment_reply(
        &mut self,
        id: &str,
        text: String,
        send: bool,
    ) -> Result<Event> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a reply (at most 2000 characters)"
        );
        ensure!(
            !send || self.linked,
            "Not linked to a conversation — open it from nexus with /studio"
        );
        let thread = self
            .session_mut_threads()?
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?;
        ensure!(
            thread.messages.len() < 100,
            "Thread has reached 100 messages"
        );
        thread.messages.push(comments::Message::new_for(
            comments::Role::You,
            text.clone(),
            vec![],
        ));
        self.persist_doc_feedback()?;
        if send {
            self.outgoing.push(LinkMessage::Reply {
                deck_path: self.path.clone(),
                thread_id: id.into(),
                text,
            });
        }
        Ok(self.doc_feedback_event(vec![]))
    }

    pub(super) fn doc_comment_resolve(&mut self, id: &str, resolved: bool) -> Result<Event> {
        self.session_mut_threads()?
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?
            .resolved = resolved.then(comments::now);
        self.persist_doc_feedback()?;
        Ok(self.doc_feedback_event(vec![]))
    }

    fn doc_edit(&self, owner: &str, id: &str) -> Result<&comments::Edit> {
        self.session()?
            .threads
            .iter()
            .filter(|t| t.id == owner)
            .flat_map(|t| &t.messages)
            .flat_map(|m| &m.edits)
            .find(|e| e.proposal.id == id)
            .context("Unknown edit")
    }

    fn set_doc_edit_status(&mut self, ids: &[String], status: comments::EditStatus) -> Result<()> {
        for edit in self
            .session_mut_threads()?
            .iter_mut()
            .flat_map(|t| &mut t.messages)
            .flat_map(|m| &mut m.edits)
        {
            if ids.contains(&edit.proposal.id) {
                edit.status = status;
            }
        }
        Ok(())
    }

    /// What an edit would change: the blocks that would be new or different,
    /// and the ones that would go. Nothing is written.
    pub(super) fn doc_preview_edit(&self, owner: &str, id: &str) -> Result<Event> {
        let edit = self.doc_edit(owner, id)?;
        ensure!(
            edit.status == comments::EditStatus::Pending,
            "Outdated — ask again in nexus"
        );
        let resolver = self.doc_resolver();
        let before = &self.session()?.doc;
        let next = doc::apply_with_resolver(before, &edit.proposal.ops, Some(&resolver))?;
        let changed: Vec<Block> = next
            .blocks
            .iter()
            .filter(|b| before.block(&b.id) != Some(b))
            .cloned()
            .collect();
        let removed = before
            .blocks
            .iter()
            .filter(|b| next.block(&b.id).is_none())
            .map(|b| b.id.clone())
            .collect();
        Ok(Event::DocPreview {
            owner: owner.into(),
            edit: id.into(),
            title: (next.title != before.title).then(|| next.title.clone()),
            changed,
            removed,
        })
    }

    pub(super) fn doc_apply_edit(&mut self, owner: &str, id: &str) -> Result<()> {
        let edit = self.doc_edit(owner, id)?;
        ensure!(
            edit.status == comments::EditStatus::Pending,
            "Outdated — ask again in nexus"
        );
        let resolver = self.doc_resolver();
        let next =
            doc::apply_with_resolver(&self.session()?.doc, &edit.proposal.ops, Some(&resolver))?;
        self.replace_doc(next)?;
        self.set_doc_edit_status(&[id.into()], comments::EditStatus::Applied)
    }

    pub(super) fn doc_dismiss_edit(&mut self, owner: &str, id: &str) -> Result<Event> {
        self.doc_edit(owner, id)?;
        self.set_doc_edit_status(&[id.into()], comments::EditStatus::Dismissed)?;
        self.persist_doc_feedback()?;
        Ok(self.doc_feedback_event(vec![]))
    }

    /// Accept or reject every pending suggestion at once; accepting is one undo
    /// step. A suggestion that no longer applies is marked outdated.
    pub(super) fn doc_resolve_edits(&mut self, accept: bool) -> Result<()> {
        let pending: Vec<(String, Vec<doc::Op>)> = self
            .session()?
            .threads
            .iter()
            .flat_map(|t| &t.messages)
            .flat_map(|m| &m.edits)
            .filter(|e| e.status == comments::EditStatus::Pending)
            .map(|e| (e.proposal.id.clone(), e.proposal.ops.clone()))
            .collect();
        if !accept {
            let ids: Vec<String> = pending.into_iter().map(|(id, _)| id).collect();
            return self.set_doc_edit_status(&ids, comments::EditStatus::Dismissed);
        }
        let resolver = self.doc_resolver();
        let mut next = self.session()?.doc.clone();
        let (mut applied, mut stale) = (Vec::new(), Vec::new());
        for (id, ops) in pending {
            match doc::apply_with_resolver(&next, &ops, Some(&resolver)) {
                Ok(done) => {
                    next = done;
                    applied.push(id);
                }
                Err(_) => stale.push(id),
            }
        }
        self.replace_doc(next)?;
        self.set_doc_edit_status(&applied, comments::EditStatus::Applied)?;
        self.set_doc_edit_status(&stale, comments::EditStatus::Outdated)
    }

    fn session_mut_threads(&mut self) -> Result<&mut Vec<comments::Thread>> {
        Ok(&mut self.doc.as_mut().context("open a doc first")?.threads)
    }
}
