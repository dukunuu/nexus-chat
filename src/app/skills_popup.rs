use super::App;
use super::{Popup, SkillsMode};
use tokio::sync::mpsc;

impl App {
    /// List installed skills in the status line. Replaced by a real popup in
    /// a later phase; `/skills` needs *something* to do meanwhile.
    pub(super) fn open_skills_popup(&mut self) {
        self.skills_mode = SkillsMode::Browse;
        self.skills_selected = self.skills_selected.min(self.skills.len().saturating_sub(1));
        self.popup = Popup::Skills;
    }

    pub(crate) fn reload_skills(&mut self) {
        self.skills = crate::skills::load_skills(&self.toolbox.skills_dir);
        let len = self.skills.len();
        self.skills_selected = self.skills_selected.min(len.saturating_sub(1));
    }

    pub(crate) fn move_skills_selection(&mut self, delta: i32) {
        self.skills_selected = super::clamp_cursor(self.skills_selected, self.skills.len(), delta);
    }

    pub(crate) fn start_skill_install(&mut self) {
        self.skills_edit.clear();
        self.skills_mode = SkillsMode::Install;
    }

    pub(crate) fn start_skill_remove(&mut self) {
        if self.skills.get(self.skills_selected).is_some() {
            self.skills_mode = SkillsMode::ConfirmRemove;
        }
    }

    /// Parse the typed `owner/repo/path` (or `owner/repo`) and kick off the
    /// background GitHub fetch. Same bg-task shape as memory extraction.
    pub(crate) fn confirm_skill_install(&mut self) {
        let spec = self.skills_edit.trim().to_string();
        self.skills_mode = SkillsMode::Browse;
        let Some((owner, repo, path)) = crate::skills::parse_gh_shorthand(&spec) else {
            self.status = format!("expected owner/repo/path, got: {spec}");
            return;
        };
        let dest = self.toolbox.skills_dir.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.skills_rx = Some(rx);
        self.status = format!("installing {spec}…");
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = crate::skills::install_from_github(&client, &owner, &repo, &path, &dest)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    pub(crate) fn on_skill_install_result(&mut self, result: Option<Result<String, String>>) {
        self.skills_rx = None;
        match result {
            Some(Ok(name)) => {
                self.reload_skills();
                self.status = format!("installed skill: {name}");
            }
            Some(Err(e)) => self.status = format!("skill install failed: {e}"),
            None => {}
        }
    }

    pub(crate) fn confirm_skill_remove(&mut self) {
        if let Some(skill) = self.skills.get(self.skills_selected) {
            let name = skill.name.clone();
            let _ = std::fs::remove_dir_all(&skill.dir);
            self.reload_skills();
            self.status = format!("removed skill: {name}");
        }
        self.skills_mode = SkillsMode::Browse;
    }

    /// Path to the highlighted skill's SKILL.md, for Ctrl+E in the skills popup.
    pub(crate) fn skill_edit_path_for_selected(&self) -> Option<std::path::PathBuf> {
        self.skills.get(self.skills_selected).map(|s| s.dir.join("SKILL.md"))
    }
}
