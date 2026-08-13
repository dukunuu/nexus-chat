// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::{Context, Result};

use super::{App, ScriptMeta, ScriptsMode};

impl App {
    /// Read the space's scripts dir and populate `scripts_cache`. A missing or
    /// empty dir produces an empty cache, never an error.
    pub fn refresh_scripts(&mut self) {
        let dir = self.space.scripts_dir(&self.active_space.name);
        let _ = std::fs::create_dir_all(&dir);
        self.scripts_cache = match std::fs::read_dir(&dir) {
            Err(_) => Vec::new(),
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().is_file())
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| {
                            chrono::DateTime::from_timestamp(
                                t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
                                0,
                            )
                        })
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default();
                    Some(ScriptMeta {
                        name: e.file_name().to_string_lossy().to_string(),
                        size: meta.len(),
                        modified,
                    })
                })
                .collect(),
        };
        self.scripts_cache.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn move_scripts_selection(&mut self, delta: i32) {
        self.scripts_selected =
            super::clamp_cursor(self.scripts_selected, self.scripts_cache.len(), delta);
    }

    /// Enter in Browse: open the script in $EDITOR.
    pub fn open_selected_script(&mut self) {
        let Some(s) = self.scripts_cache.get(self.scripts_selected) else {
            return;
        };
        let path = self
            .space
            .scripts_dir(&self.active_space.name)
            .join(&s.name);
        self.pending_editor = Some(super::PendingEditor::ScriptFile(path));
    }

    /// Ctrl+N: switch to Create mode.
    pub fn start_script_create(&mut self) {
        self.scripts_edit.clear();
        self.scripts_mode = ScriptsMode::Create;
    }

    /// Enter in Create mode: create the file and open in $EDITOR.
    pub fn confirm_script_create(&mut self) -> Result<()> {
        let name = self.scripts_edit.trim().to_string();
        self.scripts_mode = ScriptsMode::Browse;
        if name.is_empty() {
            return Ok(());
        }
        let dir = self.space.scripts_dir(&self.active_space.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(&name);
        if !path.exists() {
            std::fs::write(&path, "").with_context(|| format!("creating {}", path.display()))?;
        }
        self.refresh_scripts();
        self.pending_editor = Some(super::PendingEditor::ScriptFile(path));
        self.status = format!("created {name}");
        Ok(())
    }

    /// Ctrl+R in Browse: pre-fill the edit line with the current name.
    pub fn start_script_rename(&mut self) {
        if let Some(s) = self.scripts_cache.get(self.scripts_selected) {
            self.scripts_edit = s.name.clone();
            self.scripts_mode = ScriptsMode::Rename;
        }
    }

    /// Enter in Rename mode: rename the file on disk.
    pub fn confirm_script_rename(&mut self) -> Result<()> {
        let new = self.scripts_edit.trim().to_string();
        self.scripts_mode = ScriptsMode::Browse;
        let Some(s) = self.scripts_cache.get(self.scripts_selected).cloned() else {
            return Ok(());
        };
        if new.is_empty() || new == s.name {
            return Ok(());
        }
        let dir = self.space.scripts_dir(&self.active_space.name);
        let from = dir.join(&s.name);
        let to = dir.join(&new);
        if to.exists() {
            self.status = format!("{new} already exists");
            return Ok(());
        }
        std::fs::rename(&from, &to)
            .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        self.refresh_scripts();
        self.status = format!("renamed {} → {new}", s.name);
        Ok(())
    }

    /// Ctrl+D confirm: delete the script file from disk.
    pub fn confirm_script_delete(&mut self) -> Result<()> {
        let dir = self.space.scripts_dir(&self.active_space.name);
        if let Some(s) = self.scripts_cache.get(self.scripts_selected).cloned() {
            let path = dir.join(&s.name);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
            self.status = format!("removed {}", s.name);
            self.refresh_scripts();
        }
        self.scripts_mode = ScriptsMode::Browse;
        Ok(())
    }
}
