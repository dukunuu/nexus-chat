//! The `/watch` picker's flow half: browse/confirm/delete state. `create_watch`
//! and the due-run logic stay in core.

use anyhow::Result;

use nexus_core::app::{Popup, WatchMode};

use crate::app_view::AppView;

impl AppView {
    pub fn open_watch_picker(&mut self) -> Result<()> {
        self.core.watches_cache = self.core.db.list_watches(&self.core.active_space.id)?;
        self.watch_selected = 0;
        self.watch_mode = WatchMode::Browse;
        self.popup = Popup::Watch;
        Ok(())
    }

    pub fn move_watch_selection(&mut self, delta: i32) {
        self.watch_selected = nexus_core::app::clamp_cursor(
            self.watch_selected,
            self.core.watches_cache.len(),
            delta,
        );
    }

    /// Enter on the watch picker: jump to the watch's own research session
    /// (the domain switch handles messages/toolbox/viewport reset).
    pub fn confirm_watch_session(&mut self) -> Result<()> {
        if let Some(w) = self.core.watches_cache.get(self.watch_selected).cloned() {
            self.core.switch_to_session_by_id(&w.session_id)?;
        }
        self.popup = Popup::None;
        Ok(())
    }

    pub fn delete_selected_watch(&mut self) {
        if let Some(w) = self.core.watches_cache.get(self.watch_selected).cloned() {
            self.core.delete_watch(&w.id).ok();
            self.watch_selected = self
                .watch_selected
                .min(self.core.watches_cache.len().saturating_sub(1));
            self.push_status(format!("deleted watch: {}", w.topic));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::Db;
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-watch-flow-{}", uuid::Uuid::new_v4())),
        }
    }

    #[test]
    fn watch_picker_resets_confirm_mode_on_open() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        let space = a.active_space.id.clone();
        let session = a
            .core
            .db
            .create_session("watch", "a/b", &space, "chat")
            .unwrap();
        let _ = a
            .core
            .db
            .create_watch(&space, "rust async", 24, &session.id)
            .unwrap();

        a.watch_mode = WatchMode::ConfirmDelete;
        a.open_watch_picker().unwrap();

        assert_eq!(a.popup, Popup::Watch);
        assert_eq!(a.watch_mode, WatchMode::Browse);
    }
}
