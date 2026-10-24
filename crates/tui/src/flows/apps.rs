//! The `/apps` popup's flow half: browse/edit/delete state. Domain helpers
//! (`app_url`, `app_file_count`, `list_apps`) stay in core.

use anyhow::{Context, Result};

use nexus_core::app::{AppsMode, PendingEditor, Popup};

use crate::app_view::AppView;

impl AppView {
    pub fn open_apps_popup(&mut self) {
        self.core.apps_cache = self.core.list_apps();
        self.pinned_apps = self.core.pinned_panels().unwrap_or_default();
        self.apps_selected = self.apps_selected.min(self.apps_total().saturating_sub(1));
        self.apps_mode = AppsMode::Browse;
        self.popup = Popup::Apps;
    }

    /// Web apps followed by pinned panels: the rows of the popup.
    pub fn apps_total(&self) -> usize {
        self.core.apps_cache.len() + self.pinned_apps.len()
    }

    /// The highlighted row, when it is a pinned panel rather than a web app.
    pub fn selected_pinned(&self) -> Option<&nexus_core::panels::Pinned> {
        self.apps_selected
            .checked_sub(self.core.apps_cache.len())
            .and_then(|index| self.pinned_apps.get(index))
    }

    pub fn move_apps_selection(&mut self, delta: i32) {
        self.apps_selected =
            nexus_core::app::clamp_cursor(self.apps_selected, self.apps_total(), delta);
    }

    /// Ctrl+R on a pinned panel: type its new name.
    pub fn start_panel_rename(&mut self) {
        let Some(name) = self.selected_pinned().map(|app| app.name.clone()) else {
            self.push_status("only pinned panels can be renamed here");
            return;
        };
        self.apps_edit = name;
        self.apps_mode = AppsMode::RenamePanel;
    }

    /// Enter in rename mode: apply the typed name.
    pub fn confirm_panel_rename(&mut self) {
        if let Some(id) = self.selected_pinned().map(|app| app.id.clone()) {
            let to = self.apps_edit.trim().to_string();
            match self.core.rename_panel(&id, &to) {
                Ok(()) => {
                    self.pinned_apps = self.core.pinned_panels().unwrap_or_default();
                    self.apps_mode = AppsMode::Browse;
                }
                Err(error) => self.push_status(format!("rename failed: {error}")),
            }
        } else {
            self.apps_mode = AppsMode::Browse;
        }
    }

    /// Enter edit-file mode for the selected app (Ctrl+E in Browse).
    pub fn start_app_edit(&mut self) {
        if self.selected_pinned().is_some() {
            self.push_status("pinned panels have no files · Ctrl+R renames");
            return;
        }
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
        if let Some(id) = self.selected_pinned().map(|app| app.id.clone()) {
            crate::interactive::open_pinned(self, &id);
            return;
        }
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
        if let Some(id) = self.selected_pinned().map(|app| app.id.clone()) {
            if let Err(error) = self.core.unpin_panel(&id) {
                self.push_status(format!("unpin failed: {error}"));
            }
            self.pinned_apps = self.core.pinned_panels().unwrap_or_default();
            self.apps_selected = self.apps_selected.min(self.apps_total().saturating_sub(1));
            self.apps_mode = AppsMode::Browse;
            return Ok(());
        }
        if let Some(name) = self.core.apps_cache.get(self.apps_selected).cloned() {
            let dir = self
                .core
                .space
                .apps_dir(&self.core.active_space.name)
                .join(&name);
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
            self.push_status(format!("removed app {name}"));
            self.core.apps_cache = self.core.list_apps();
            self.pinned_apps = self.core.pinned_panels().unwrap_or_default();
            self.apps_selected = self.apps_selected.min(self.apps_total().saturating_sub(1));
        }
        self.apps_mode = AppsMode::Browse;
        Ok(())
    }
}
