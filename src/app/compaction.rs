use anyhow::Result;
use tokio::sync::mpsc;

use super::{App, ContextBreakdown};
use crate::db::Message;
use crate::provider::ChatMessage;

impl App {
    // --- auto-compaction ---

    /// The messages actually sent on the next turn: everything after the
    /// session's compaction boundary, or all of them if it hasn't compacted
    /// (yet). The full, uncompacted history stays in `self.messages`/the db
    /// for scrollback — only what's sent shrinks.
    pub(super) fn effective_messages(&self) -> &[Message] {
        let through = self
            .session
            .as_ref()
            .map(|s| s.compact_through as usize)
            .unwrap_or(0)
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
        if pct < self.settings.compact_threshold as u64 {
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
        let pct = self
            .context_limit()
            .filter(|&l| l > 0)
            .map(|l| {
                self.context_used()
                    .checked_mul(100)
                    .and_then(|v| v.checked_div(l))
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        self.start_compaction(pct);
    }

    /// Kick off the background compaction job on the memory model (falling
    /// back to the session model), same pattern as memory extraction.
    /// `before_pct` is only used to report the before/after status on completion.
    fn start_compaction(&mut self, before_pct: u64) {
        let Some(provider) = self.provider.clone() else {
            self.status = "set your API key first with /key".to_string();
            return;
        };
        let model = if !self.memory_model.trim().is_empty() {
            self.memory_model.clone()
        } else {
            match self.current_model.clone() {
                Some(m) => m,
                None => {
                    self.status = "pick a model first with /model".to_string();
                    return;
                }
            }
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let through = session.compact_through as usize;
        if through >= self.messages.len() {
            return; // nothing new since the last compaction to fold in
        }
        let prior_summary = session.compact_summary.clone();
        let tail: String = self.messages[through..]
            .iter()
            .filter(|m| m.role != "tool_call")
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        let session_id = session.id.clone();
        let new_through = self.messages.len() as i64;
        let (tx, rx) = mpsc::unbounded_channel();
        self.compact_rx = Some(rx);
        self.status = "compacting…".to_string();
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
            if let Ok(summary) = provider.complete(&model, msgs).await {
                let summary = summary.trim().to_string();
                if !summary.is_empty() {
                    let _ = tx.send((session_id, summary, new_through, before_pct));
                }
            }
        });
    }

    /// Apply a compaction digest to the matching session (in memory + db).
    /// Clears the exact usage total — it reflects the pre-compaction request,
    /// so `context_used` should fall back to the (now accurate) estimate
    /// until the next real response reports fresh usage.
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

    /// System/memory/conversation token estimate for the context breakdown
    /// popup (Ctrl+I). Each bucket is a ~4-chars/token estimate, same method
    /// `context_used` falls back to, so the parts add up to (roughly) the whole.
    pub fn context_breakdown(&self) -> ContextBreakdown {
        let mut instructions_chars = self.resolved_base_system_prompt().chars().count();
        instructions_chars +=
            std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
                .map(|s| s.trim().chars().count())
                .unwrap_or(0);
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
                .map(|md| crate::skills::skill_body(&md).chars().count())
                .unwrap_or(0);
        }
        let mut conversation_chars: usize = self
            .effective_messages()
            .iter()
            .map(|m| m.content.chars().count())
            .sum();
        if let Some(s) = self
            .session
            .as_ref()
            .and_then(|s| s.compact_summary.as_deref())
        {
            conversation_chars += s.chars().count();
        }
        if let Some(buf) = &self.streaming {
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
