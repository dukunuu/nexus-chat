//! The `/apps` popup: viewing (and pruning) the space's model-built apps.

use anyhow::{Context, Result};

use super::{App, AppsMode};

impl App {
    pub fn open_apps_popup(&mut self) {
        self.apps_cache = self.list_apps();
        self.apps_selected = self
            .apps_selected
            .min(self.apps_cache.len().saturating_sub(1));
        self.apps_mode = AppsMode::Browse;
        self.popup = super::Popup::Apps;
    }

    pub fn move_apps_selection(&mut self, delta: i32) {
        self.apps_selected = super::clamp_cursor(self.apps_selected, self.apps_cache.len(), delta);
    }

    /// An app's live URL, when the server is running.
    pub fn app_url(&self, name: &str) -> Option<String> {
        let s = self.app_server.as_ref()?;
        let uuid = s.registry().resolve(&self.active_space.name, name)?;
        Some(s.app_url(&uuid))
    }

    /// Enter edit-file mode for the selected app (Ctrl+E in Browse).
    pub fn start_app_edit(&mut self) {
        let Some(name) = self.apps_cache.get(self.apps_selected).cloned() else {
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
        let Some(name) = self.apps_cache.get(self.apps_selected).cloned() else {
            return;
        };
        let file = self.apps_edit.trim().to_string();
        if file.is_empty() || file.starts_with('/') || file.contains("..") {
            self.push_status(format!("invalid path: {file}"));
            return;
        }
        let path = self
            .space
            .apps_dir(&self.active_space.name)
            .join(&name)
            .join(&file);
        if !path.is_file() {
            self.push_status(format!("no such file: {name}/{file}"));
            return;
        }
        self.pending_editor = Some(crate::app::PendingEditor::AppFile(path));
        self.apps_mode = AppsMode::Browse;
    }

    /// Enter in Browse: open the highlighted app in the system browser.
    pub fn open_selected_app(&mut self) {
        let Some(name) = self.apps_cache.get(self.apps_selected) else {
            return;
        };
        match self.app_url(name) {
            Some(url) => {
                let _ = open::that_detached(&url);
                self.push_status(format!("opened {url}"));
            }
            None => self.push_status("app server not running".to_string()),
        }
    }

    /// Ctrl+D confirm: the app's directory (and everything in it) goes.
    pub fn confirm_app_delete(&mut self) -> Result<()> {
        if let Some(name) = self.apps_cache.get(self.apps_selected).cloned() {
            let dir = self.space.apps_dir(&self.active_space.name).join(&name);
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
            self.push_status(format!("removed app {name}"));
            self.apps_cache = self.list_apps();
            self.apps_selected = self
                .apps_selected
                .min(self.apps_cache.len().saturating_sub(1));
        }
        self.apps_mode = AppsMode::Browse;
        Ok(())
    }

    /// How many files an app holds (recursive; `node_modules` counted as one
    /// "deps" marker would be noise, so it's skipped entirely).
    pub fn app_file_count(&self, name: &str) -> usize {
        fn count(dir: &std::path::Path) -> usize {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return 0;
            };
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        if e.file_name() == "node_modules" {
                            0
                        } else {
                            count(&p)
                        }
                    } else {
                        1
                    }
                })
                .sum()
        }
        count(&self.space.apps_dir(&self.active_space.name).join(name))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, AppsMode, Popup};
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root = std::env::temp_dir().join(format!("nexus-apps-popup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k"), space)
    }

    #[test]
    fn apps_command_lists_apps_and_delete_removes_dir() {
        let mut a = test_app();
        let dir = a.space.apps_dir(&a.active_space.name);
        std::fs::create_dir_all(dir.join("deck/js")).unwrap();
        std::fs::write(dir.join("deck/index.html"), "x").unwrap();
        std::fs::write(dir.join("deck/js/a.js"), "y").unwrap();
        std::fs::create_dir_all(dir.join("deck/node_modules/p")).unwrap();
        std::fs::write(dir.join("deck/node_modules/p/i.js"), "z").unwrap();

        a.run_command("apps").unwrap();
        assert!(a.popup == Popup::Apps);
        assert_eq!(a.apps_cache, vec!["deck"]);
        assert_eq!(a.app_file_count("deck"), 2); // node_modules skipped

        a.apps_mode = AppsMode::ConfirmDelete;
        a.confirm_app_delete().unwrap();
        assert!(a.apps_cache.is_empty());
        assert!(!dir.join("deck").exists());
        assert!(a.apps_mode == AppsMode::Browse);
    }

    #[test]
    fn app_url_requires_running_server() {
        let a = test_app();
        assert!(a.app_url("deck").is_none()); // no server in tests
    }
}
