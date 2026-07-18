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
        let needle = self.space_filter.trim();
        if needle.is_empty() {
            return self.spaces_cache.iter().collect();
        }
        use crate::input::fuzzy_score;
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
            if let Some(reg) = self.app_server.as_ref().map(|s| s.registry()) {
                reg.rename_space(&s.name, &name);
            }
            if let Some(cached) = self.spaces_cache.iter_mut().find(|c| c.id == s.id) {
                cached.name = name.clone();
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
            .unwrap();
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
        self.clear_image_state();
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

    pub fn confirm_space(&mut self) -> Result<()> {
        if let Some(s) = self.selected_space() {
            self.set_active_space(s);
        }
        self.popup = Popup::None;
        Ok(())
    }
}
