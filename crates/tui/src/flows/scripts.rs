// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;

use nexus_core::app::{PendingEditor, ScriptsMode};

use crate::app_view::AppView;

impl AppView {
    pub fn move_scripts_selection(&mut self, delta: i32) {
        self.scripts_selected = nexus_core::app::clamp_cursor(
            self.scripts_selected,
            self.core.scripts_cache.len(),
            delta,
        );
    }

    /// Enter in Browse: open the script in $EDITOR.
    pub fn open_selected_script(&mut self) {
        let Some(s) = self.core.scripts_cache.get(self.scripts_selected) else {
            return;
        };
        let path = self
            .core
            .space
            .scripts_dir(&self.core.active_space.name)
            .join(&s.name);
        self.pending_editor = Some(PendingEditor::ScriptFile(path));
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
        let path = self.core.ensure_script_file(&name)?;
        self.pending_editor = Some(PendingEditor::ScriptFile(path));
        self.push_status(format!("created {name}"));
        Ok(())
    }

    /// Ctrl+R in Browse: pre-fill the edit line with the current name.
    pub fn start_script_rename(&mut self) {
        if let Some(s) = self.core.scripts_cache.get(self.scripts_selected) {
            self.scripts_edit = s.name.clone();
            self.scripts_mode = ScriptsMode::Rename;
        }
    }

    /// Enter in Rename mode: rename the file on disk.
    pub fn confirm_script_rename(&mut self) {
        let new = self.scripts_edit.trim().to_string();
        self.scripts_mode = ScriptsMode::Browse;
        let Some(s) = self.core.scripts_cache.get(self.scripts_selected).cloned() else {
            return;
        };
        if new.is_empty() || new == s.name {
            return;
        }
        if let Err(e) = self.core.rename_script_file(&s.name, &new) {
            self.push_status(e.to_string());
            return;
        }
        self.push_status(format!("renamed {} → {new}", s.name));
    }

    /// Ctrl+D confirm: delete the script file from disk.
    pub fn confirm_script_delete(&mut self) -> Result<()> {
        if let Some(s) = self.core.scripts_cache.get(self.scripts_selected).cloned() {
            self.core.delete_script_file(&s.name)?;
            self.push_status(format!("removed {}", s.name));
        }
        self.scripts_mode = ScriptsMode::Browse;
        Ok(())
    }
}
