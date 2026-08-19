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
use crate::provider::{ChatMessage, ChatParams};

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
    pub fn excluded_from_model_history(m: &Message) -> bool {
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
    pub fn effective_messages(&self) -> &[Message] {
        let through = self
            .session
            .as_ref()
            .and_then(|s| usize::try_from(s.compact_through).ok())
            .unwrap_or(0)
            .min(self.messages.len());
        &self.messages[through..]
    }

    /// Whether `id` is the session whose background compaction is still running.
    #[must_use]
    pub fn is_compacting_session(&self, id: &str) -> bool {
        self.compact_rx.is_some() && self.compacting_session_id.as_deref() == Some(id)
    }

    /// Whether the session currently on screen is being compacted.
    #[must_use]
    pub fn is_compacting_current_session(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| self.is_compacting_session(&s.id))
    }

    /// After a reply, auto-compact once context usage crosses the configured
    /// threshold (0 disables it).
    pub fn maybe_compact(&mut self) {
        if self.settings.compact_threshold == 0 || self.compact_rx.is_some() || self.is_streaming()
        {
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
            self.push_status("already compacting…".to_string());
            return;
        }
        if self.is_streaming() {
            self.push_status("wait for the current response to finish".to_string());
            return;
        }
        let Some(session) = self.session.as_ref() else {
            self.push_status("no active session to compact".to_string());
            return;
        };
        let through = usize::try_from(session.compact_through)
            .unwrap_or(0)
            .min(self.messages.len());
        if compaction_tail(&self.messages, through).trim().is_empty() {
            self.push_status("nothing new to compact".to_string());
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
        let (session_id, through, prior_summary) = {
            let Some(session) = self.session.as_ref() else {
                return;
            };
            let through = usize::try_from(session.compact_through)
                .unwrap_or(0)
                .min(self.messages.len());
            (session.id.clone(), through, session.compact_summary.clone())
        };
        let tail = compaction_tail(&self.messages, through);
        if tail.trim().is_empty() {
            return; // only the existing digest or excluded UI rows remain
        }

        // A saved utility-model id may belong to a backend that is no longer
        // configured (especially for sessions created before the backend
        // prefixes were introduced). Resolve it like every other background
        // utility job, falling back to the active session backend/model.
        let requested_model = if self.memory_model.trim().is_empty() {
            let Some(model) = self.current_model.clone() else {
                self.push_status("pick a model first with /model".to_string());
                return;
            };
            model
        } else {
            self.memory_model.trim().to_string()
        };
        let Some((provider, raw_model)) = self.resolve_utility_model_backend(&requested_model)
        else {
            self.push_status(format!(
                "model backend unavailable: {requested_model} — pick another with /model"
            ));
            return;
        };
        let new_through = self.messages.len() as i64;
        let prompt_cache_key = format!("compaction:{}", self.prompt_cache_key_for(&session_id));
        let (tx, rx) = mpsc::unbounded_channel();
        self.compact_rx = Some(rx);
        self.compacting_session_id = Some(session_id.clone());
        // The history pane gets a transient "compacting" block immediately;
        // the input bar also keeps its compact status while the request runs.
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
            let params = ChatParams {
                prompt_cache_key: Some(prompt_cache_key),
                ..ChatParams::default()
            };
            if let Ok(completion) = provider
                .complete_with_params(&raw_model, msgs, &params)
                .await
            {
                let summary = completion.text.trim().to_string();
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
        self.compacting_session_id = None;
        let Some((id, summary, through, before_pct)) = result else {
            self.push_status("compaction failed — no digest returned".to_string());
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
        let in_view = self.session.as_ref().is_some_and(|s| s.id == id);
        if in_view {
            self.bump_cache_epoch();
        }
        let mut history_invalidated = false;
        if in_view {
            if let Some(row) = self.messages.iter_mut().find(|m| m.role == "compaction") {
                row.content.clone_from(&summary);
                history_invalidated = true;
            } else {
                let through = usize::try_from(through)
                    .unwrap_or(0)
                    .min(self.messages.len());
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
                history_invalidated = true;
            }
        }
        if history_invalidated {
            self.push_history_invalidated();
        }
        if self
            .db
            .update_compaction_message(&id, &summary)
            .is_ok_and(|n| n == 0)
        {
            // Anchor: the in-memory row we just placed (viewing the session),
            // else the boundary message's timestamp straight from the db
            // (job finished after the user switched away), else now.
            let anchor = in_view
                .then(|| {
                    self.messages
                        .iter()
                        .find(|m| m.role == "compaction")
                        .and_then(|m| m.created_at.clone())
                })
                .flatten()
                .or_else(|| {
                    self.db
                        .message_created_at(
                            &id,
                            usize::try_from(through).unwrap_or(0).saturating_sub(1),
                        )
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
        self.push_status(match after_pct {
            Some(after) => format!("compacted: {before_pct}% → {after}%"),
            None => "compacted".to_string(),
        });
    }

    /// Sessions compacted before compaction rows existed (or loaded from a
    /// db written by such a version) carry the digest only in
    /// `compact_summary`. Surface it as a transcript row at the boundary,
    /// exactly like a fresh compaction would, so the digest is never hidden
    /// behind the context popup. Idempotent: no-ops once a compaction row
    /// exists. Called after every session load.
    pub fn backfill_compaction_row(&mut self) {
        let Some(s) = self.session.as_ref() else {
            return;
        };
        let Some(summary) = s.compact_summary.clone() else {
            return;
        };
        if self.messages.iter().any(|m| m.role == "compaction") {
            return;
        }
        let through = usize::try_from(s.compact_through)
            .unwrap_or(0)
            .min(self.messages.len());
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
        self.push_history_invalidated();
    }

    /// System/memory/conversation token estimate for the context breakdown
    /// popup (Ctrl+I). Each bucket is a ~4-chars/token estimate, same method
    /// `context_used` falls back to, so the parts add up to (roughly) the whole.
    pub fn context_breakdown(&self) -> ContextBreakdown {
        let mut instructions_chars = self.resolved_base_system_prompt().chars().count();
        instructions_chars +=
            std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
                .map_or(0, |s| s.trim().chars().count());
        let memory_chars = self.memory_snapshot().chars().count();
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
        self.bump_cache_epoch();
        if let Some(row) = self.messages.iter_mut().find(|m| m.role == "compaction") {
            row.content.clone_from(&text);
            self.push_history_invalidated();
        }
        if let Some(s) = self.session.as_mut() {
            s.compact_summary = Some(text);
        }
        self.push_status("compaction digest updated".to_string());
        Ok(())
    }
}

/// The message tail handed to the compaction model: everything since the
/// last digest except rows that must never reach a model via
/// `App::excluded_from_model_history`. Tool-call rows are retained so a
/// compaction cannot silently erase the model's tool findings.
fn compaction_tail(messages: &[Message], through: usize) -> String {
    messages[through.min(messages.len())..]
        .iter()
        .filter(|m| !App::excluded_from_model_history(m))
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
        assert!(a.last_status().contains("compacted"), "{}", a.last_status());
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
    fn force_compact_ignores_a_backfilled_digest_row() {
        let (mut a, sid) = app_with_session(1);
        a.db.set_compaction(&sid, "legacy digest", 2).unwrap();
        a.session = a.db.get_session(&sid).unwrap();
        a.backfill_compaction_row();

        // The visible legacy digest is not new conversation content. It must
        // not cause a second compaction request every time the session opens.
        a.force_compact();
        assert!(a.compact_rx.is_none());
        assert!(a.last_status().contains("nothing new"));
    }

    #[test]
    fn compaction_failure_clears_the_running_marker() {
        let mut a = test_app();
        let session =
            a.db.create_session("t", "m", &a.active_space.id, "chat")
                .unwrap();
        a.session = Some(session.clone());
        a.compacting_session_id = Some(session.id.clone());
        let (_tx, rx) = mpsc::unbounded_channel();
        a.compact_rx = Some(rx);

        a.on_compact_result(None);

        assert!(a.compact_rx.is_none());
        assert!(a.compacting_session_id.is_none());
        assert!(a.last_status().contains("compaction failed"));
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
            msg(
                "tool_call",
                r#"{"name":"search","result":"important finding"}"#,
            ),
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
        ] {
            assert!(
                !tail.contains(banned),
                "digest must not contain {banned:?}: {tail}"
            );
        }
        assert!(
            tail.contains("important finding"),
            "tool findings must survive: {tail}"
        );
        // The compaction boundary still applies.
        let partial = compaction_tail(&msgs, 1);
        assert!(!partial.contains("what should we research?"), "{partial}");
        assert!(partial.contains("the final question"), "{partial}");
    }
}
