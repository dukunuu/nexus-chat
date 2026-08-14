use anyhow::Result;

use nexus_core::app::{Popup, SessionMode};

use crate::app_view::AppView;

impl AppView {
    pub fn open_session_picker(&mut self) -> Result<()> {
        self.core.sessions_cache = self.core.db.list_sessions(&self.core.active_space.id)?;
        if self.core.sessions_cache.is_empty() {
            self.push_status("no sessions yet — send a message to start one".to_string());
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
    pub fn filtered_sessions(&self) -> Vec<&nexus_core::db::Session> {
        let needle = self.session_filter.trim();
        if needle.is_empty() {
            return self.core.sessions_cache.iter().collect();
        }
        nexus_core::app::fuzzy_filter_sorted(&self.core.sessions_cache, |s| {
            nexus_core::app::session_score(s, needle)
        })
    }

    /// The session under the picker cursor (respecting the active filter).
    pub fn selected_session(&self) -> Option<nexus_core::db::Session> {
        self.filtered_sessions()
            .get(self.session_selected)
            .map(|s| (*s).clone())
    }

    pub fn move_session_selection(&mut self, delta: i32) {
        self.session_selected = nexus_core::app::clamp_cursor(
            self.session_selected,
            self.filtered_sessions().len(),
            delta,
        );
    }

    /// A filter keystroke re-runs the fuzzy match and resets the cursor to the top.
    pub fn session_filter_push(&mut self, c: char) {
        self.session_filter.insert_char(c);
        self.session_selected = 0;
    }

    pub fn session_filter_pop(&mut self) {
        self.session_filter.backspace();
        self.session_selected = 0;
    }

    /// Enter rename mode, seeding the edit buffer with the current title.
    pub fn start_rename(&mut self) {
        if let Some(s) = self.selected_session() {
            self.session_edit = s.title;
            self.session_mode = SessionMode::Rename;
        }
    }

    pub fn confirm_rename(&mut self) -> Result<()> {
        let title = self.session_edit.trim().to_string();
        if let (false, Some(s)) = (title.is_empty(), self.selected_session()) {
            self.core.db.set_session_title(&s.id, &title, None)?;
            if let Some(cached) = self.core.sessions_cache.iter_mut().find(|c| c.id == s.id) {
                cached.title.clone_from(&title);
            }
            if let Some(cur) = self.core.session.as_mut().filter(|c| c.id == s.id) {
                cur.title = title;
            }
        }
        self.session_mode = SessionMode::Browse;
        Ok(())
    }

    /// Delete the highlighted session; if it was the active one, reset to a blank
    /// state so the stale conversation doesn't linger.
    pub fn confirm_delete(&mut self) -> Result<()> {
        if let Some(s) = self.selected_session() {
            self.core.db.delete_session(&s.id)?;
            self.core.discard_chat_task(&s.id);
            self.core.unread.remove(&s.id);
            self.core.notifications.retain(|n| n.session_id != s.id);
            self.core.sessions_cache.retain(|c| c.id != s.id);
            if self.core.session.as_ref().is_some_and(|c| c.id == s.id) {
                self.core.session = None;
                self.core.messages.clear();
                self.core.context_total = None;
                self.push_viewport_reset();
                self.core.cleanup_incognito_images();
            }
            self.push_status(format!("deleted: {}", s.title));
        }
        self.session_mode = SessionMode::Browse;
        let len = self.filtered_sessions().len();
        self.session_selected = self.session_selected.min(len.saturating_sub(1));
        if self.core.sessions_cache.is_empty() {
            self.popup = Popup::None;
        }
        Ok(())
    }

    pub fn confirm_session(&mut self) -> Result<()> {
        if let Some(s) = self.selected_session() {
            self.core.switch_to_session_by_id(&s.id)?;
        }
        self.popup = Popup::None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::{Db, Message};
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-flow-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn test_app() -> AppView {
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("sk-or-test-key"),
            test_space(),
        ))
    }

    #[tokio::test]
    async fn deleting_the_streaming_session_discards_the_stream() {
        let mut a = test_app();
        let space = a.core.active_space.id.clone();
        let s = a
            .core
            .db
            .create_session("streaming", "a/one", &space, "chat")
            .unwrap();
        a.core.session = Some(s.clone());
        a.core.sessions_cache = a.core.db.list_sessions(&space).unwrap();
        a.session_selected = 0;
        // A live (streaming) chat task for that session, as if a response was
        // mid-flight when the user deleted it from the picker.
        let abort = tokio::task::spawn(async {}).abort_handle();
        a.core.chat_tasks.insert(
            1,
            nexus_core::app::ChatTask {
                id: 1,
                session_id: s.id.clone(),
                session_title: "streaming".into(),
                space_id: space.clone(),
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

        a.open_session_picker().unwrap();
        a.confirm_delete().unwrap(); // deletes the only (streaming) session
        assert!(!a.is_streaming());
        assert!(a.core.chat_tasks.is_empty());
        assert!(!a.core.unread.contains(&s.id));
    }

    #[test]
    fn delete_removes_session_and_clears_if_active() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        let space = a.active_space.id.clone();
        let s = a
            .core
            .db
            .create_session("doomed", "a/b", &space, "chat")
            .unwrap();
        a.core.sessions_cache = a.core.db.list_sessions(&space).unwrap();
        a.core.session = Some(s.clone());
        a.core.messages.push(Message {
            role: "user".into(),
            content: "hi".into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
        a.session_selected = 0;
        a.confirm_delete().unwrap();
        assert!(a.core.sessions_cache.is_empty());
        assert!(a.session.is_none());
        assert!(a.messages.is_empty());
        assert!(a.core.db.list_sessions(&space).unwrap().is_empty());
    }

    #[test]
    fn session_filter_matches_title_and_slug() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        let space = a.active_space.id.clone();
        let s1 = a
            .core
            .db
            .create_session("Rust async runtimes", "a/b", &space, "chat")
            .unwrap();
        let s2 = a
            .core
            .db
            .create_session("Cooking pasta", "a/b", &space, "chat")
            .unwrap();
        a.core
            .db
            .set_session_title(&s1.id, "Rust async runtimes", Some("rust-async"))
            .unwrap();
        a.core.sessions_cache = a.core.db.list_sessions(&space).unwrap();

        a.session_filter = "rust".into();
        let hits = a.filtered_sessions();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, s1.id);

        a.session_filter = "pasta".into();
        assert_eq!(a.filtered_sessions()[0].id, s2.id);
    }
}
