use anyhow::Result;

use super::{App, Popup, SpaceMode};
use crate::db::{DEFAULT_SPACE, Space as SpaceRow};

impl App {
    pub(super) fn open_space_picker(&mut self) -> Result<()> {
        self.spaces_cache = self.db.list_spaces()?;
        self.space_selected = self
            .spaces_cache
            .iter()
            .position(|s| s.id == self.active_space.id)
            .unwrap_or(0);
        self.space_filter.clear();
        self.space_mode = SpaceMode::Browse;
        self.popup = Popup::Space;
        Ok(())
    }

    /// Spaces matching the current fuzzy filter, best match first. Empty filter
    /// keeps db order (default first, then creation order).
    pub fn filtered_spaces(&self) -> Vec<&SpaceRow> {
        use crate::input::fuzzy_score;
        let needle = self.space_filter.trim();
        if needle.is_empty() {
            return self.spaces_cache.iter().collect();
        }
        super::fuzzy_filter_sorted(&self.spaces_cache, |s| fuzzy_score(&s.name, needle))
    }

    pub fn selected_space(&self) -> Option<SpaceRow> {
        self.filtered_spaces()
            .get(self.space_selected)
            .map(|s| (*s).clone())
    }

    pub fn move_space_selection(&mut self, delta: i32) {
        self.space_selected =
            super::clamp_cursor(self.space_selected, self.filtered_spaces().len(), delta);
    }

    pub fn space_filter_push(&mut self, c: char) {
        self.space_filter.insert_char(c);
        self.space_selected = 0;
    }

    pub fn space_filter_pop(&mut self) {
        self.space_filter.backspace();
        self.space_selected = 0;
    }

    pub fn start_space_create(&mut self) {
        self.space_edit.clear();
        self.space_mode = SpaceMode::Create;
    }

    /// A named space cannot be renamed/deleted if it's the default.
    pub fn start_space_rename(&mut self) {
        if let Some(s) = self.selected_space().filter(|s| s.name != DEFAULT_SPACE) {
            self.space_edit = s.name;
            self.space_mode = SpaceMode::Rename;
        }
    }

    pub fn confirm_space_create(&mut self) -> Result<()> {
        let name = self.space_edit.trim().to_string();
        if !name.is_empty() && !self.spaces_cache.iter().any(|s| s.name == name) {
            let s = self.db.create_space(&name)?;
            self.space.ensure_space_dir(&name)?;
            self.spaces_cache.push(s);
            self.space_selected = self.spaces_cache.len() - 1;
        }
        self.space_mode = SpaceMode::Browse;
        Ok(())
    }

    pub fn confirm_space_rename(&mut self) -> Result<()> {
        let name = self.space_edit.trim().to_string();
        if let (false, Some(s)) = (name.is_empty(), self.selected_space()) {
            self.db.rename_space(&s.id, &name)?;
            self.space.rename_space_dir(&s.name, &name)?;
            if let Some(reg) = self
                .app_server
                .as_ref()
                .map(super::super::appserver::AppServer::registry)
            {
                reg.rename_space(&s.name, &name);
            }
            if let Some(cached) = self.spaces_cache.iter_mut().find(|c| c.id == s.id) {
                cached.name.clone_from(&name);
            }
            if self.active_space.id == s.id {
                self.active_space.name = name;
                self.refresh_toolbox();
            }
        }
        self.space_mode = SpaceMode::Browse;
        Ok(())
    }

    /// Delete the highlighted space (default is never offered for delete —
    /// gated by `handle_space_popup`). Sessions move to default; if the active
    /// space was deleted, switch to default.
    pub fn confirm_space_delete(&mut self) -> Result<()> {
        if let Some(s) = self.selected_space().filter(|s| s.name != DEFAULT_SPACE) {
            self.db.delete_space(&s.id)?;
            self.space.remove_space_dir(&s.name)?;
            self.spaces_cache.retain(|c| c.id != s.id);
            if self.active_space.id == s.id {
                self.switch_to_default_space()?;
            }
            self.status = format!("deleted space: {}", s.name);
        }
        self.space_mode = SpaceMode::Browse;
        let len = self.filtered_spaces().len();
        self.space_selected = self.space_selected.min(len.saturating_sub(1));
        Ok(())
    }

    fn switch_to_default_space(&mut self) -> Result<()> {
        let default_id = self.db.default_space_id()?;
        let row = self
            .db
            .list_spaces()?
            .into_iter()
            .find(|s| s.id == default_id)
            .ok_or_else(|| anyhow::anyhow!("default space {default_id:?} no longer exists"))?;
        self.set_active_space(row);
        Ok(())
    }

    /// Switch the active space, clearing the open conversation (a session
    /// belongs to exactly one space).
    fn set_active_space(&mut self, row: SpaceRow) {
        self.active_space = row;
        self.session = None;
        self.messages.clear();
        self.context_total = None;
        self.scroll = 0;
        self.cleanup_incognito_images();
        self.rescan_files();
        self.refresh_toolbox();
        self.status = format!("space: {}", self.active_space.name);
    }

    /// Path to the highlighted space's instructions file, creating a stub with
    /// a short header comment if it doesn't exist yet (so $EDITOR has something
    /// to open).
    pub fn instructions_path_for_selected(&self) -> Option<std::path::PathBuf> {
        let s = self.selected_space()?;
        let path = self.space.instructions_path(&s.name);
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!("<!-- instructions for the \"{}\" space -->\n", s.name),
            );
        }
        Some(path)
    }

    /// Path to the highlighted space's memory file (the numbered facts a
    /// conversation in that space has accumulated), creating an empty stub
    /// with a header comment if nothing's been extracted yet.
    pub fn memory_path_for_selected(&self) -> Option<std::path::PathBuf> {
        let s = self.selected_space()?;
        let path = self.space.memory_path(&s.name);
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!(
                    "<!-- memory for the \"{}\" space — numbered facts, one per line -->\n",
                    s.name
                ),
            );
        }
        Some(path)
    }

    pub fn confirm_space(&mut self) {
        if let Some(s) = self.selected_space() {
            self.set_active_space(s);
        }
        self.popup = Popup::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn app() -> App {
        App::new(
            crate::db::Db::open_in_memory().unwrap(),
            Some("k"),
            test_space(),
        )
    }

    fn add_space(a: &mut App, name: &str) -> SpaceRow {
        let s = a.db.create_space(name).unwrap();
        a.spaces_cache.push(s.clone());
        s
    }

    #[test]
    fn open_space_picker_selects_the_active_space() {
        let mut a = app();
        let extra = add_space(&mut a, "other");
        let active = a.active_space.id.clone();
        a.open_space_picker().unwrap();
        assert_eq!(a.popup, Popup::Space);
        assert!(matches!(a.space_mode, SpaceMode::Browse));
        assert_eq!(a.spaces_cache.len(), 2);
        assert_eq!(a.selected_space().unwrap().id, active);
        assert_ne!(extra.id, active);
    }

    #[test]
    fn filtered_spaces_matches_by_fuzzy_name_and_keeps_db_order() {
        let mut a = app();
        let _docs = add_space(&mut a, "docs");
        let _notes = add_space(&mut a, "notes");
        a.open_space_picker().unwrap();

        // Empty filter: db order (default first, then creation order).
        let all = a.filtered_spaces();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name, DEFAULT_SPACE);

        a.space_filter_push('n');
        let hits = a.filtered_spaces();
        assert!(hits.iter().all(|s| s.name.contains('n')));
        assert_eq!(a.space_selected, 0);
    }

    #[test]
    fn move_space_selection_clamps_at_both_ends() {
        let mut a = app();
        let _docs = add_space(&mut a, "docs");
        a.open_space_picker().unwrap();
        a.move_space_selection(100);
        assert_eq!(a.space_selected, a.spaces_cache.len() - 1);
        a.move_space_selection(-100);
        assert_eq!(a.space_selected, 0);
    }

    #[test]
    fn rename_is_refused_for_the_default_space() {
        let mut a = app();
        a.open_space_picker().unwrap();
        // Selection starts on the active (default) space.
        a.start_space_rename();
        assert!(matches!(a.space_mode, SpaceMode::Browse)); // unchanged — refused
    }

    #[test]
    fn create_skips_empty_and_duplicate_names() {
        let mut a = app();
        add_space(&mut a, "docs");
        a.open_space_picker().unwrap();

        a.space_edit = "docs".into();
        a.confirm_space_create().unwrap();
        assert_eq!(a.spaces_cache.len(), 2); // duplicate refused

        a.space_edit = "  ".into();
        a.confirm_space_create().unwrap();
        assert_eq!(a.spaces_cache.len(), 2); // blank refused

        a.space_edit = "new-space".into();
        a.confirm_space_create().unwrap();
        assert_eq!(a.spaces_cache.len(), 3);
        assert!(matches!(a.space_mode, SpaceMode::Browse));
        assert!(
            a.spaces_cache
                .iter()
                .any(|s| s.name == "new-space" && s.id == a.selected_space().unwrap().id)
        );
    }

    #[test]
    fn rename_updates_row_and_active_space_in_place() {
        let mut a = app();
        let s = add_space(&mut a, "old-name");
        a.open_space_picker().unwrap();
        a.space_selected = a.spaces_cache.iter().position(|c| c.id == s.id).unwrap();
        a.start_space_rename();
        assert!(matches!(a.space_mode, SpaceMode::Rename));
        a.space_edit = "new-name".into();
        a.confirm_space_rename().unwrap();
        assert!(matches!(a.space_mode, SpaceMode::Browse));
        assert!(a.spaces_cache.iter().any(|c| c.name == "new-name"));
        assert!(!a.spaces_cache.iter().any(|c| c.name == "old-name"));
    }

    #[test]
    fn deleting_the_active_space_switches_to_default() {
        let mut a = app();
        let extra = add_space(&mut a, "doomed");
        // Make the extra space active.
        a.confirm_space(); // confirm selected... selection is default; pick doomed
        a.open_space_picker().unwrap();
        a.space_selected = a
            .spaces_cache
            .iter()
            .position(|c| c.id == extra.id)
            .unwrap();
        a.confirm_space();
        assert_eq!(a.active_space.id, extra.id);

        a.open_space_picker().unwrap();
        a.space_selected = a
            .spaces_cache
            .iter()
            .position(|c| c.id == extra.id)
            .unwrap();
        a.confirm_space_delete().unwrap();
        assert_ne!(a.active_space.id, extra.id);
        assert_eq!(a.active_space.name, DEFAULT_SPACE);
        assert!(!a.spaces_cache.iter().any(|c| c.id == extra.id));
    }

    #[test]
    fn delete_refuses_the_default_space() {
        let mut a = app();
        let extra = add_space(&mut a, "keep-me");
        a.open_space_picker().unwrap();
        a.space_selected = a
            .spaces_cache
            .iter()
            .position(|c| c.id == extra.id)
            .unwrap();
        a.confirm_space_delete().unwrap();
        assert_eq!(a.spaces_cache.len(), 1); // only the default remains

        // Selecting the default and deleting it is a no-op.
        a.space_selected = a
            .spaces_cache
            .iter()
            .position(|c| c.name == DEFAULT_SPACE)
            .unwrap();
        a.confirm_space_delete().unwrap();
        assert_eq!(a.spaces_cache.len(), 1);
        assert!(a.spaces_cache.iter().any(|c| c.name == DEFAULT_SPACE));
    }

    #[test]
    fn missing_default_space_is_an_error_not_a_panic() {
        let mut a = app();
        // Delete the default space row out from under the app (e.g. a stale
        // default id after manual db edits) — switching must fail visibly,
        // never panic.
        let default_id = a.db.default_space_id().unwrap();
        a.db.delete_space(&default_id).unwrap();
        assert!(a.switch_to_default_space().is_err());
    }
}
