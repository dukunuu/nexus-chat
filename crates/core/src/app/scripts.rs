// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use super::App;

impl App {
    /// Read the space's scripts dir and populate `scripts_cache`. A missing or
    /// empty dir produces an empty cache, never an error. The scripts popup's
    /// flow (selection/edit state, $EDITOR handoff) lives in the view layer.
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
                    Some(super::ScriptMeta {
                        name: e.file_name().to_string_lossy().to_string(),
                        size: meta.len(),
                        modified,
                    })
                })
                .collect(),
        };
        self.scripts_cache.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Domain half of script create: touch the file (if absent) and refresh
    /// the cache. Returns the created path. The view owns the edit buffer and
    /// the $EDITOR handoff.
    pub fn ensure_script_file(&mut self, name: &str) -> anyhow::Result<std::path::PathBuf> {
        use anyhow::Context as _;
        let dir = self.space.scripts_dir(&self.active_space.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(name);
        if !path.exists() {
            std::fs::write(&path, "").with_context(|| format!("creating {}", path.display()))?;
        }
        self.refresh_scripts();
        Ok(path)
    }

    /// Domain half of script rename: move the file on disk. Returns an error
    /// message string when the target already exists (the view turns it into
    /// a status line); Ok otherwise.
    pub fn rename_script_file(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let dir = self.space.scripts_dir(&self.active_space.name);
        let from_path = dir.join(from);
        let to_path = dir.join(to);
        if to_path.exists() {
            anyhow::bail!("{to} already exists");
        }
        std::fs::rename(&from_path, &to_path).with_context(|| {
            format!("renaming {} to {}", from_path.display(), to_path.display())
        })?;
        self.refresh_scripts();
        Ok(())
    }

    /// Domain half of script delete: remove the file from disk and refresh.
    /// Returns whether a row existed.
    pub fn delete_script_file(&mut self, name: &str) -> anyhow::Result<bool> {
        use anyhow::Context as _;
        let dir = self.space.scripts_dir(&self.active_space.name);
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            self.refresh_scripts();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
