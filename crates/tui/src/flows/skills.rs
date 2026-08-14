//! The `/skills` popup's flow half: cursor/mode state and the $EDITOR
//! handoff. `reload_skills`, the install task, and result handling stay in
//! core.

use nexus_core::app::{Popup, SkillsMode};

use crate::app_view::AppView;

impl AppView {
    pub fn open_skills_popup(&mut self) {
        self.skills_mode = SkillsMode::Browse;
        self.skills_selected = self
            .skills_selected
            .min(self.core.skills.len().saturating_sub(1));
        self.popup = Popup::Skills;
    }

    /// Re-read installed skills from disk, then re-clamp the cursor.
    pub fn reload_skills(&mut self) {
        self.core.reload_skills();
        let len = self.core.skills.len();
        self.skills_selected = self.skills_selected.min(len.saturating_sub(1));
    }

    pub fn move_skills_selection(&mut self, delta: i32) {
        self.skills_selected =
            nexus_core::app::clamp_cursor(self.skills_selected, self.core.skills.len(), delta);
    }

    pub fn start_skill_install(&mut self) {
        self.skills_edit.clear();
        self.skills_mode = SkillsMode::Install;
    }

    pub fn start_skill_remove(&mut self) {
        if self.core.skills.get(self.skills_selected).is_some() {
            self.skills_mode = SkillsMode::ConfirmRemove;
        }
    }

    /// Parse the typed `owner/repo/path` (or `owner/repo`) and kick off the
    /// background GitHub fetch (the domain half owns the task).
    pub fn confirm_skill_install(&mut self) {
        let spec = self.skills_edit.clone();
        self.skills_mode = SkillsMode::Browse;
        self.core.start_skill_install(&spec);
    }

    pub fn confirm_skill_remove(&mut self) {
        if let Some(skill) = self.core.skills.get(self.skills_selected) {
            let name = skill.name.clone();
            let _ = std::fs::remove_dir_all(&skill.dir);
            self.reload_skills();
            self.push_status(format!("removed skill: {name}"));
        }
        self.skills_mode = SkillsMode::Browse;
    }

    /// Path to the highlighted skill's SKILL.md, for Ctrl+E in the skills popup.
    pub fn skill_edit_path_for_selected(&self) -> Option<std::path::PathBuf> {
        self.core
            .skills
            .get(self.skills_selected)
            .map(|s| s.dir.join("SKILL.md"))
    }
}
