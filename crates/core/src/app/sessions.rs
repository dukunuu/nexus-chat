use anyhow::Result;

use super::App;
use crate::db::Session;

impl App {
    pub fn activate_notification(&mut self, index: usize) -> Result<()> {
        let Some(notification) = self.notifications.remove(index) else {
            return Ok(());
        };
        self.switch_to_session_by_id(&notification.session_id)
    }

    // --- commands ---

    /// Clear back to a blank conversation. Doesn't touch the db — a session
    /// row is only created lazily on the first message actually sent (same as
    /// the very first message of the app), so `/new` without typing anything
    /// doesn't leave an empty "new chat" behind in the session list.
    pub fn new_session(&mut self) {
        self.session = None;
        self.messages.clear();
        self.refresh_memory_snapshot();
        // A selection points into the old session's wrapped lines — a stale
        // one would mis-highlight the new chat and resolve links against the
        // wrong messages; the view clears it on `ViewportReset`.
        self.context_total = None;
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        self.push_status("new chat — send a message to start it".to_string());
    }

    /// Switch to a session by its id. Used by session-link navigation (Ctrl+O),
    /// notification clicks, the `ResolveSession` command, and the session
    /// picker's confirm (via the view layer).
    pub fn switch_to_session_by_id(&mut self, id: &str) -> Result<()> {
        let Some(s) = self
            .db
            .get_session(id)?
            .or_else(|| self.sessions_cache.iter().find(|s| s.id == id).cloned())
        else {
            self.push_status(format!("session not found: {id}"));
            return Ok(());
        };
        self.messages = self.db.load_messages(&s.id)?;
        self.unread.remove(&s.id);
        self.notifications.retain(|n| n.session_id != s.id);
        self.push_status(format!("switched to: {}", s.title));
        self.current_model = Some(s.model.clone());
        self.web_mode = s.web_mode;
        self.session = Some(s);
        self.refresh_memory_snapshot();
        self.backfill_compaction_row();
        self.restore_survey_gate_prompt();
        self.refresh_toolbox();
        self.context_total = None;
        // Selection + scroll point into the previous session's lines; the
        // view resets them on `ViewportReset`.
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        // Opening an old session should be enough to trigger auto-compaction
        // once its catalog/context window is available. `on_models_result`
        // repeats this check when the model fetch races the session switch.
        self.maybe_compact();
        Ok(())
    }
}

/// Best fuzzy score of `needle` against a session's title, slug, and uuid.
pub fn session_score(s: &Session, needle: &str) -> Option<i32> {
    use crate::app::fuzzy_score;
    let mut best = fuzzy_score(&s.title, needle);
    let upd = |best: &mut Option<i32>, cand: Option<i32>| {
        if let Some(c) = cand {
            *best = Some(best.map_or(c, |b| b.max(c)));
        }
    };
    if let Some(slug) = &s.slug {
        upd(&mut best, fuzzy_score(slug, needle).map(|v| v + 2));
    }
    upd(&mut best, fuzzy_score(&s.id, needle));
    best
}

/// Parse the model's topic reply into `(topic, slug)`. Tolerates surrounding prose
/// or code fences by extracting the first `{...}` and reading the two fields.
pub fn parse_topic(text: &str) -> Option<(String, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let json = text.get(start..=end)?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let topic = v.get("topic").and_then(|t| t.as_str())?.trim();
    if topic.is_empty() {
        return None;
    }
    let raw_slug = v.get("id").and_then(|s| s.as_str()).unwrap_or(topic);
    Some((topic.to_string(), slugify(raw_slug)))
}

/// Normalise to a short kebab-case slug: lowercase, `[a-z0-9-]`, max 5 words.
pub fn slugify(s: &str) -> String {
    let slug = s
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "chat".to_string()
    } else {
        slug
    }
}
