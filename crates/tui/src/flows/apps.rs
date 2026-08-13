//! The `/apps` popup's flow half: browse/edit/delete state. Domain helpers
//! (`app_url`, `app_file_count`, `list_apps`) stay in core.

use anyhow::{Context, Result};

use nexus_core::app::{AppsMode, PendingEditor, Popup};

use crate::app_view::AppView;

impl AppView {
    pub fn open_apps_popup(&mut self) {
        self.core.apps_cache = self.core.list_apps();
        self.apps_selected = self
            .apps_selected
            .min(self.core.apps_cache.len().saturating_sub(1));
        self.apps_mode = AppsMode::Browse;
        self.popup = Popup::Apps;
    }

    pub fn move_apps_selection(&mut self, delta: i32) {
        self.apps_selected =
            nexus_core::app::clamp_cursor(self.apps_selected, self.core.apps_cache.len(), delta);
    }

    /// Enter edit-file mode for the selected app (Ctrl+E in Browse).
    pub fn start_app_edit(&mut self) {
        let Some(name) = self.core.apps_cache.get(self.apps_selected).cloned() else {
            return;
        };
        self.apps_edit = "index.html".to_string();
        self.apps_mode = AppsMode::EditFile;
        self.push_status(format!(
            "edit {name}/ (type filename, Enter to open in $EDITOR)"
        ));
    }

    /// Confirm in `EditFile`: open the typed path in $EDITOR.
    pub fn confirm_app_edit(&mut self) {
        let Some(name) = self.core.apps_cache.get(self.apps_selected).cloned() else {
            return;
        };
        let file = self.apps_edit.trim().to_string();
        if file.is_empty() || file.starts_with('/') || file.contains("..") {
            self.push_status(format!("invalid path: {file}"));
            return;
        }
        let path = self
            .core
            .space
            .apps_dir(&self.core.active_space.name)
            .join(&name)
            .join(&file);
        if !path.is_file() {
            self.push_status(format!("no such file: {name}/{file}"));
            return;
        }
        self.pending_editor = Some(PendingEditor::AppFile(path));
        self.apps_mode = AppsMode::Browse;
    }

    /// Enter in Browse: open the highlighted app in the system browser.
    pub fn open_selected_app(&mut self) {
        let Some(name) = self.core.apps_cache.get(self.apps_selected) else {
            return;
        };
        match self.core.app_url(name) {
            Some(url) => {
                let _ = open::that_detached(&url);
                self.push_status(format!("opened {url}"));
            }
            None => self.push_status("app server not running".to_string()),
        }
    }

    /// Ctrl+D confirm: the app's directory (and everything in it) goes.
    pub fn confirm_app_delete(&mut self) -> Result<()> {
        if let Some(name) = self.core.apps_cache.get(self.apps_selected).cloned() {
            let dir = self
                .core
                .space
                .apps_dir(&self.core.active_space.name)
                .join(&name);
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
            self.push_status(format!("removed app {name}"));
            self.core.apps_cache = self.core.list_apps();
            self.apps_selected = self
                .apps_selected
                .min(self.core.apps_cache.len().saturating_sub(1));
        }
        self.apps_mode = AppsMode::Browse;
        Ok(())
    }
}
