use anyhow::Result;

use super::{App, SessionMode, Popup};
use crate::db::Session;

impl App {
    // --- commands ---

    /// Clear back to a blank conversation. Doesn't touch the db — a session
    /// row is only created lazily on the first message actually sent (same as
    /// the very first message of the app), so `/new` without typing anything
    /// doesn't leave an empty "new chat" behind in the session list.
    pub(super) fn new_session(&mut self) -> Result<()> {
        self.session = None;
        self.messages.clear();
        self.context_total = None;
        self.scroll = 0;
        self.clear_image_state();
        self.status = "new chat — send a message to start it".to_string();
        Ok(())
    }

    pub(super) fn open_session_picker(&mut self) -> Result<()> {
        self.sessions_cache = self.db.list_sessions(&self.active_space.id)?;
        if self.sessions_cache.is_empty() {
            self.status = "no sessions yet — send a message to start one".to_string();
            return Ok(());
        }
        self.session_selected = 0;
        self.session_filter.clear();
        self.session_mode = SessionMode::Browse;
        self.popup = Popup::Session;
        Ok(())
    }

    /// Sessions matching the current fuzzy filter (title, slug, and id), best
    /// match first. Empty filter keeps the recency order from the db.
    pub(crate) fn filtered_sessions(&self) -> Vec<&Session> {
        let needle = self.session_filter.trim();
        if needle.is_empty() {
            return self.sessions_cache.iter().collect();
        }
        super::fuzzy_filter_sorted(&self.sessions_cache, |s| session_score(s, needle))
    }

    /// The session under the picker cursor (respecting the active filter).
    pub(crate) fn selected_session(&self) -> Option<Session> {
        self.filtered_sessions().get(self.session_selected).map(|s| (*s).clone())
    }

    pub(crate) fn move_session_selection(&mut self, delta: i32) {
        self.session_selected =
            super::clamp_cursor(self.session_selected, self.filtered_sessions().len(), delta);
    }

    /// A filter keystroke re-runs the fuzzy match and resets the cursor to the top.
    pub(crate) fn session_filter_push(&mut self, c: char) {
        self.session_filter.push(c);
        self.session_selected = 0;
    }

    pub(crate) fn session_filter_pop(&mut self) {
        self.session_filter.pop();
        self.session_selected = 0;
    }

    /// Enter rename mode, seeding the edit buffer with the current title.
    pub(crate) fn start_rename(&mut self) {
        if let Some(s) = self.selected_session() {
            self.session_edit = s.title;
            self.session_mode = SessionMode::Rename;
        }
    }

    pub(crate) fn confirm_rename(&mut self) -> Result<()> {
        let title = self.session_edit.trim().to_string();
        if let (false, Some(s)) = (title.is_empty(), self.selected_session()) {
            self.db.set_session_title(&s.id, &title, None)?;
            if let Some(cached) = self.sessions_cache.iter_mut().find(|c| c.id == s.id) {
                cached.title = title.clone();
            }
            if let Some(cur) = self.session.as_mut().filter(|c| c.id == s.id) {
                cur.title = title;
            }
        }
        self.session_mode = SessionMode::Browse;
        Ok(())
    }

    /// Delete the highlighted session; if it was the active one, reset to a blank
    /// state so the stale conversation doesn't linger.
    pub(crate) fn confirm_delete(&mut self) -> Result<()> {
        if let Some(s) = self.selected_session() {
            self.db.delete_session(&s.id)?;
            self.sessions_cache.retain(|c| c.id != s.id);
            if self.session.as_ref().is_some_and(|c| c.id == s.id) {
                self.session = None;
                self.messages.clear();
                self.context_total = None;
                self.scroll = 0;
                self.clear_image_state();
            }
            self.status = format!("deleted: {}", s.title);
        }
        self.session_mode = SessionMode::Browse;
        let len = self.filtered_sessions().len();
        self.session_selected = self.session_selected.min(len.saturating_sub(1));
        if self.sessions_cache.is_empty() {
            self.popup = Popup::None;
        }
        Ok(())
    }

    pub(crate) fn confirm_session(&mut self) -> Result<()> {
        if let Some(s) = self.selected_session() {
            self.messages = self.db.load_messages(&s.id)?;
            self.current_model = Some(s.model.clone());
            self.status = format!("switched to: {}", s.title);
            self.session = Some(s);
            // Estimate from history until the next response reports exact usage.
            self.context_total = None;
            self.scroll = 0;
            self.clear_image_state();
        }
        self.popup = Popup::None;
        Ok(())
    }
}

/// Best fuzzy score of `needle` against a session's title, slug, and uuid.
fn session_score(s: &Session, needle: &str) -> Option<i32> {
    use crate::input::fuzzy_score;
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
pub(super) fn parse_topic(text: &str) -> Option<(String, String)> {
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
fn slugify(s: &str) -> String {
    let slug = s
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { "chat".to_string() } else { slug }
}
