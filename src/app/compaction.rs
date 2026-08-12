// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;
use tokio::sync::mpsc;

use super::{App, ContextBreakdown};
use crate::db::Message;
use crate::provider::ChatMessage;

impl App {
    // --- auto-compaction ---

    /// Rows that must never reach a model — neither in the raw history
    /// (`build_history`) nor in a compaction digest: background-job scratch
    /// (research stage/plan/survey rows), transport failures, session links,
    /// per-persona swarm round replies (the turn's synthesis carries the
    /// context), gate replies — the survey/plan sections they answer are
    /// excluded too, so a bare "drop Q2" must not leak into later turns via
    /// a digest — and the compaction digest row itself (the digest is
    /// already fed to the model via `compact_summary`, so a transcript row
    /// must never be sent twice).
    pub(super) fn excluded_from_model_history(m: &Message) -> bool {
        m.role == "compaction"
            || m.role == "research_stage"
            || m.role == "research_plan"
            || m.role == "survey"
            || m.role == "gate_reply"
            || m.role == "session_link"
            || m.role == "error"
            || m.persona.is_some()
    }

    /// The messages actually sent on the next turn: everything after the
    /// session's compaction boundary, or all of them if it hasn't compacted
    /// (yet). The full, uncompacted history stays in `self.messages`/the db
    /// for scrollback — only what's sent shrinks.
    pub(super) fn effective_messages(&self) -> &[Message] {
        let through = self
            .session
            .as_ref()
            .map_or(0, |s| s.compact_through as usize)
            .min(self.messages.len());
        &self.messages[through..]
    }

    /// After a reply, auto-compact once context usage crosses the configured
    /// threshold (0 disables it).
    pub(super) fn maybe_compact(&mut self) {
        if self.settings.compact_threshold == 0 || self.compact_rx.is_some() {
            return;
        }
        let Some(limit) = self.context_limit() else {
            return;
        };
        let used = self.context_used();
        let pct = used
            .checked_mul(100)
            .and_then(|v| v.checked_div(limit))
            .unwrap_or(0);
        if pct < u64::from(self.settings.compact_threshold) {
            return;
        }
        self.start_compaction(pct);
    }

    /// Manually trigger compaction right now (`/compact`), ignoring the
    /// threshold. No-ops with a status message if there's nothing to compact.
    pub fn force_compact(&mut self) {
        if self.compact_rx.is_some() {
            self.status = "already compacting…".to_string();
            return;
        }
        if self.is_streaming() {
            self.status = "wait for the current response to finish".to_string();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            self.status = "no active session to compact".to_string();
            return;
        };
        if session.compact_through as usize >= self.messages.len() {
            self.status = "nothing new to compact".to_string();
            return;
        }
        let pct = self.context_limit().filter(|&l| l > 0).map_or(0, |l| {
            self.context_used()
                .checked_mul(100)
                .and_then(|v| v.checked_div(l))
                .unwrap_or(0)
        });
        self.start_compaction(pct);
    }

    /// Kick off the background compaction job on the memory model (falling
    /// back to the session model), same pattern as memory extraction.
    /// `before_pct` is only used to report the before/after status on completion.
    fn start_compaction(&mut self, before_pct: u64) {
        let model = if self.memory_model.trim().is_empty() {
            if let Some(m) = self.current_model.clone() {
                m
            } else {
                self.status = "pick a model first with /model".to_string();
                return;
            }
        } else {
            self.memory_model.clone()
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.status = format!("model backend unavailable: {model} — pick another with /model");
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let through = session.compact_through as usize;
        if through >= self.messages.len() {
            return; // nothing new since the last compaction to fold in
        }
        let prior_summary = session.compact_summary.clone();
        let tail = compaction_tail(&self.messages, through);
        let session_id = session.id.clone();
        let new_through = self.messages.len() as i64;
        let (tx, rx) = mpsc::unbounded_channel();
        self.compact_rx = Some(rx);
        // No status write here: the input bar's "⟳ compacting…" hint (driven
        // by `compact_rx`) is the progress indicator — a status message would
        // be overwritten by the next event and never seen again.
        tokio::spawn(async move {
            let mut prompt = String::new();
            if let Some(s) = &prior_summary {
                prompt.push_str("Existing summary of earlier conversation:\n");
                prompt.push_str(s);
                prompt.push_str("\n\n");
            }
            prompt.push_str("New messages since that summary:\n");
            prompt.push_str(&tail);
            prompt.push_str(
                "\n\nCompress ALL of the above into one ultra-dense technical digest: cut \
                 every pleasantry, filler word, and repeated explanation, but keep every \
                 decision, fact, file/function name, code snippet, number, and open thread — \
                 nothing substantive may be lost. Terse fragments are fine. No headers, no \
                 meta-commentary about summarizing. Reply with ONLY the digest.",
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(summary) = provider.complete(&raw_model, msgs).await {
                let summary = summary.trim().to_string();
                if !summary.is_empty() {
                    let _ = tx.send((session_id, summary, new_through, before_pct));
                }
            }
        });
    }

    /// Apply a compaction digest to the matching session (in memory + db):
    /// the digest itself becomes a visible `compaction` transcript row at the
    /// compaction boundary, so what was folded away is shown in the chat
    /// instead of being reachable only through the context popup's editor.
    /// A later compaction updates that row in place (one digest row per
    /// session, at the same boundary). Clears the exact usage total — it
    /// reflects the pre-compaction request, so `context_used` should fall
    /// back to the (now accurate) estimate until the next real response
    /// reports fresh usage.
    pub fn on_compact_result(&mut self, result: Option<(String, String, i64, u64)>) {
        self.compact_rx = None;
        let Some((id, summary, through, before_pct)) = result else {
            return;
        };
        let _ = self.db.set_compaction(&id, &summary, through);
        if let Some(s) = self.session.as_mut().filter(|s| s.id == id) {
            s.compact_summary = Some(summary.clone());
            s.compact_through = through;
        }
        // Surface the digest in the transcript: update the existing row (a
        // re-compaction folds new messages into the same digest), or insert
        // one at the boundary — after the last message it covers, before
        // anything the user says next. The db row is anchored to that last
        // message's timestamp so reloads keep the same position.
        if self.session.as_ref().is_some_and(|s| s.id == id) {
            if let Some(row) = self.messages.iter_mut().find(|m| m.role == "compaction") {
                row.content.clone_from(&summary);
            } else {
                let through = (through as usize).min(self.messages.len());
                let anchor = self
                    .messages
                    .get(through.saturating_sub(1))
                    .and_then(|m| m.created_at.clone());
                self.messages.insert(
                    through,
                    crate::db::Message {
                        role: "compaction".to_string(),
                        content: summary.clone(),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: None,
                        persona: None,
                        created_at: anchor,
                    },
                );
                self.invalidate_history_cache();
            }
        }
        if self
            .db
            .update_compaction_message(&id, &summary)
            .is_ok_and(|n| n == 0)
        {
            // Anchor: the in-memory row we just placed (viewing the session),
            // else the boundary message's timestamp straight from the db
            // (job finished after the user switched away), else now.
            let anchor = self
                .messages
                .iter()
                .find(|m| m.role == "compaction")
                .and_then(|m| m.created_at.clone())
                .or_else(|| {
                    self.db
                        .message_created_at(&id, (through as usize).saturating_sub(1))
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            let _ = self.db.add_compaction_message(&id, &summary, &anchor);
        }
        self.context_total = None;
        let after_pct = self
            .context_limit()
            .filter(|&l| l > 0)
            .map(|l| self.context_used() * 100 / l);
        self.status = match after_pct {
            Some(after) => format!("compacted: {before_pct}% → {after}%"),
            None => "compacted".to_string(),
        };
    }

    /// Sessions compacted before compaction rows existed (or loaded from a
    /// db written by such a version) carry the digest only in
    /// `compact_summary`. Surface it as a transcript row at the boundary,
    /// exactly like a fresh compaction would, so the digest is never hidden
    /// behind the context popup. Idempotent: no-ops once a compaction row
    /// exists. Called after every session load.
    pub(crate) fn backfill_compaction_row(&mut self) {
        let Some(s) = self.session.as_ref() else {
            return;
        };
        let Some(summary) = s.compact_summary.clone() else {
            return;
        };
        if self.messages.iter().any(|m| m.role == "compaction") {
            return;
        }
        let through = (s.compact_through as usize).min(self.messages.len());
        let anchor = self
            .messages
            .get(through.saturating_sub(1))
            .and_then(|m| m.created_at.clone())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let id = s.id.clone();
        let _ = self.db.add_compaction_message(&id, &summary, &anchor);
        self.messages.insert(
            through,
            crate::db::Message {
                role: "compaction".to_string(),
                content: summary,
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: Some(anchor),
            },
        );
        self.invalidate_history_cache();
    }

    /// System/memory/conversation token estimate for the context breakdown
    /// popup (Ctrl+I). Each bucket is a ~4-chars/token estimate, same method
    /// `context_used` falls back to, so the parts add up to (roughly) the whole.
    pub fn context_breakdown(&self) -> ContextBreakdown {
        let mut instructions_chars = self.resolved_base_system_prompt().chars().count();
        instructions_chars +=
            std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
                .map_or(0, |s| s.trim().chars().count());
        let memory_chars = self.read_memory().chars().count();
        let mut skills_chars: usize = self
            .skills
            .iter()
            .map(|s| s.name.chars().count() + s.description.chars().count())
            .sum();
        if let Some(name) = &self.forced_skill
            && let Some(skill) = self.skills.iter().find(|s| &s.name == name)
        {
            skills_chars += std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map_or(0, |md| crate::skills::skill_body(&md).chars().count());
        }
        let mut conversation_chars: usize = self
            .effective_messages()
            .iter()
            // The digest transcript row is the same text as `compact_summary`
            // (counted below) — never double-count it.
            .filter(|m| m.role != "compaction")
            .map(|m| m.content.chars().count())
            .sum();
        if let Some(s) = self
            .session
            .as_ref()
            .and_then(|s| s.compact_summary.as_deref())
        {
            conversation_chars += s.chars().count();
        }
        if let Some(buf) = self.active_streaming_text() {
            conversation_chars += buf.chars().count();
        }
        ContextBreakdown {
            system_tokens: (instructions_chars / 4) as u64,
            memory_tokens: (memory_chars / 4) as u64,
            skills_tokens: (skills_chars / 4) as u64,
            conversation_tokens: (conversation_chars / 4) as u64,
            limit: self.context_limit(),
            compacted: self
                .session
                .as_ref()
                .is_some_and(|s| s.compact_summary.is_some()),
        }
    }

    /// Path to a temp file holding the active session's compaction digest, so
    /// it can be viewed/edited in `$EDITOR` from the context popup (Ctrl+G, `v`).
    /// `None` if the session hasn't been compacted yet.
    pub fn compact_summary_path(&self) -> Option<std::path::PathBuf> {
        let session = self.session.as_ref()?;
        let summary = session.compact_summary.as_ref()?;
        let path = std::env::temp_dir().join(format!("nexus-chat-compact-{}.md", session.id));
        std::fs::write(&path, summary).ok()?;
        Some(path)
    }

    /// Read `path` (from `compact_summary_path`) back after `$EDITOR` closes —
    /// hand-edits to the digest persist (db + in-memory), same as any other
    /// file-backed edit in the app.
    pub fn reload_compact_summary(&mut self, path: &std::path::Path) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(());
        };
        let text = text.trim().to_string();
        if text.is_empty() || Some(&text) == session.compact_summary.as_ref() {
            return Ok(());
        }
        let id = session.id.clone();
        let through = session.compact_through;
        self.db.set_compaction(&id, &text, through)?;
        if let Some(s) = self.session.as_mut() {
            s.compact_summary = Some(text);
        }
        self.status = "compaction digest updated".to_string();
        Ok(())
    }
}

/// The message tail handed to the compaction model: everything since the
/// last digest except rows that must never reach a model — tool-call blocks
/// and `App::excluded_from_model_history`. Without this, a digest could
/// carry contextless gate replies ("drop Q2") into later history even
/// though `build_history` skips them.
fn compaction_tail(messages: &[Message], through: usize) -> String {
    messages[through..]
        .iter()
        .filter(|m| m.role != "tool_call" && !App::excluded_from_model_history(m))
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        }
    }

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root =
            std::env::temp_dir().join(format!("nexus-compact-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        App::new(db, Some("k"), Space { root })
    }

    /// A fresh session with `n` user/assistant pairs loaded as the active one.
    fn app_with_session(n: usize) -> (App, String) {
        let mut a = test_app();
        let sid =
            a.db.create_session("t", "m", &a.active_space.id, "chat")
                .unwrap()
                .id;
        for i in 0..n {
            a.db.add_user_message(&sid, &format!("u{i}")).unwrap();
            a.db.add_assistant_message(&sid, &format!("a{i}"), None, None, None, None, None, None)
                .unwrap();
        }
        a.messages = a.db.load_messages(&sid).unwrap();
        a.session = a.db.get_session(&sid).unwrap();
        (a, sid)
    }

    #[test]
    fn on_compact_result_surfaces_the_digest_at_the_boundary() {
        let (mut a, sid) = app_with_session(2);

        a.on_compact_result(Some((sid.clone(), "digest text".to_string(), 3, 42)));

        // The digest row sits at the boundary: after the 3 compacted
        // messages, before anything the user says next.
        assert_eq!(a.messages.len(), 5);
        assert_eq!(a.messages[3].role, "compaction");
        assert_eq!(a.messages[3].content, "digest text");
        assert_eq!(a.messages[2].content, "u1"); // last compacted message
        // Session state applied.
        assert_eq!(a.session.as_ref().unwrap().compact_through, 3);
        assert_eq!(
            a.session.as_ref().unwrap().compact_summary.as_deref(),
            Some("digest text")
        );
        // Persisted, anchored to the last compacted message's timestamp so
        // reloads keep the same position.
        let stored = a.db.load_messages(&sid).unwrap();
        assert_eq!(stored.len(), 5);
        let digest = stored.iter().find(|m| m.role == "compaction").unwrap();
        assert_eq!(digest.content, "digest text");
        let last_compacted = stored.iter().find(|m| m.content == "u1").unwrap();
        assert_eq!(digest.created_at, last_compacted.created_at);
        assert!(a.status.contains("compacted"), "{}", a.status);
    }

    #[test]
    fn re_compaction_updates_the_digest_row_in_place() {
        let (mut a, sid) = app_with_session(5);

        a.on_compact_result(Some((sid.clone(), "digest one".to_string(), 4, 50)));
        assert_eq!(
            a.messages.iter().filter(|m| m.role == "compaction").count(),
            1
        );

        // Second compaction folds the rest in: same single row, new text.
        a.on_compact_result(Some((sid.clone(), "digest two".to_string(), 10, 60)));
        assert_eq!(
            a.messages.iter().filter(|m| m.role == "compaction").count(),
            1
        );
        let row = a.messages.iter().find(|m| m.role == "compaction").unwrap();
        assert_eq!(row.content, "digest two");
        let stored = a.db.load_messages(&sid).unwrap();
        assert_eq!(stored.iter().filter(|m| m.role == "compaction").count(), 1);
        assert_eq!(
            stored
                .iter()
                .find(|m| m.role == "compaction")
                .unwrap()
                .content,
            "digest two"
        );
    }

    #[test]
    fn backfill_surfaces_a_legacy_digest_at_the_boundary_and_is_idempotent() {
        let (mut a, sid) = app_with_session(1);
        // Legacy compaction: digest only in the session row, no transcript
        // message — the state of sessions compacted before digests rendered.
        a.db.set_compaction(&sid, "legacy digest", 2).unwrap();
        a.session = a.db.get_session(&sid).unwrap();

        a.backfill_compaction_row();
        assert_eq!(a.messages.len(), 3);
        assert_eq!(a.messages[2].role, "compaction");
        assert_eq!(a.messages[2].content, "legacy digest");
        assert_eq!(a.db.load_messages(&sid).unwrap().len(), 3);

        // A second backfill (another session load) adds nothing.
        a.backfill_compaction_row();
        assert_eq!(a.messages.len(), 3);
        assert_eq!(a.db.load_messages(&sid).unwrap().len(), 3);
    }

    #[test]
    fn compaction_tail_skips_rows_that_must_never_reach_the_model() {
        let mut msgs = vec![
            msg("user", "what should we research?"),
            msg("research_stage", "planner: working"),
            msg("survey", "For \"x\":\n 1. Depth?"),
            msg("gate_reply", "drop Q2"),
            msg("research_plan", "Research plan: …"),
            msg("error", "request failed"),
            msg("session_link", "sess-1\n↩ from: x"),
            msg("compaction", "folded-away digest"),
            msg("user", "the final question"),
            msg("tool_call", r#"{"name":"search"}"#),
        ];
        let mut persona = msg("assistant", "round reply");
        persona.persona = Some("Optimist".into());
        msgs.push(persona);

        let tail = compaction_tail(&msgs, 0);
        assert!(tail.contains("what should we research?"), "{tail}");
        assert!(tail.contains("the final question"), "{tail}");
        // Background rows, gate replies, errors, links, the digest row itself
        // (already fed via compact_summary), and persona round replies never
        // enter a digest — the compacted history would otherwise leak
        // contextless "drop Q2" to later models.
        for banned in [
            "planner: working",
            "Depth?",
            "drop Q2",
            "Research plan",
            "request failed",
            "sess-1",
            "folded-away digest",
            "round reply",
            "tool_call",
        ] {
            assert!(
                !tail.contains(banned),
                "digest must not contain {banned:?}: {tail}"
            );
        }
        // The compaction boundary still applies.
        let partial = compaction_tail(&msgs, 1);
        assert!(!partial.contains("what should we research?"), "{partial}");
        assert!(partial.contains("the final question"), "{partial}");
    }
}
