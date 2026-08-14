use super::App;
use tokio::sync::mpsc;

impl App {
    /// Re-read installed skills from disk (after an install/remove, or a
    /// Ctrl+E hand-edit of a SKILL.md). The view re-clamps its cursor after
    /// calling this.
    pub fn reload_skills(&mut self) {
        self.skills = crate::skills::load_skills(&crate::skills::skills_dir(&self.space.root));
    }

    /// Domain half of `/skills` install: parse the typed `owner/repo/path`
    /// (or `owner/repo`) and kick off the background GitHub fetch. Same
    /// bg-task shape as memory extraction. The view owns the edit buffer and
    /// mode; `Ok(())` means the task started (or the spec was invalid — the
    /// message is pushed as a status line).
    pub fn start_skill_install(&mut self, spec: &str) {
        let spec = spec.trim().to_string();
        let Some((owner, repo, path)) = crate::skills::parse_gh_shorthand(&spec) else {
            self.push_status(format!("expected owner/repo/path, got: {spec}"));
            return;
        };
        let dest = crate::skills::skills_dir(&self.space.root);
        let (tx, rx) = mpsc::unbounded_channel();
        self.skills_rx = Some(rx);
        self.push_status(format!("installing {spec}…"));
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = crate::skills::install_from_github(&client, &owner, &repo, &path, &dest)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    pub fn on_skill_install_result(&mut self, result: Option<Result<String, String>>) {
        self.skills_rx = None;
        match result {
            Some(Ok(name)) => {
                self.reload_skills();
                self.push_status(format!("installed skill: {name}"));
            }
            Some(Err(e)) => self.push_status(format!("skill install failed: {e}")),
            None => {}
        }
    }
}
