use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;
use crate::db::DEFAULT_SPACE;

/// Resolves the shared data directory (holding `nexus.db`) and per-space
/// directories (`spaces/<name>/`, each with `memory.md` + `instructions.md`).
pub struct Space {
    pub root: PathBuf,
}

impl Space {
    /// Resolve the data dir, running the one-time layout migration from the old
    /// "single global space" layout (`spaces/global/nexus.db`) if present.
    pub fn open() -> Result<Self> {
        let root = config::project_dirs()?.data_dir().to_path_buf();
        let space = Space { root };
        space.migrate_legacy_layout()?;
        std::fs::create_dir_all(space.root.join("spaces"))
            .with_context(|| format!("creating {}", space.root.join("spaces").display()))?;
        Ok(space)
    }

    /// Old layout: db and the sole space both lived under `spaces/global/`.
    /// Move the db up to the shared root and rename the space dir to `default`.
    fn migrate_legacy_layout(&self) -> Result<()> {
        let legacy_dir = self.root.join("spaces").join("global");
        let legacy_db = legacy_dir.join("nexus.db");
        let new_db = self.root.join("nexus.db");
        if legacy_db.exists() && !new_db.exists() {
            std::fs::create_dir_all(&self.root)
                .with_context(|| format!("creating {}", self.root.display()))?;
            std::fs::rename(&legacy_db, &new_db)
                .with_context(|| format!("moving {} to {}", legacy_db.display(), new_db.display()))?;
        }
        let default_dir = self.root.join("spaces").join(DEFAULT_SPACE);
        if legacy_dir.exists() && !default_dir.exists() {
            std::fs::rename(&legacy_dir, &default_dir).with_context(|| {
                format!("moving {} to {}", legacy_dir.display(), default_dir.display())
            })?;
        }
        Ok(())
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("nexus.db")
    }

    fn space_dir(&self, name: &str) -> PathBuf {
        self.root.join("spaces").join(name)
    }

    /// Ensure `spaces/<name>/` exists, for a space just created in the db.
    pub fn ensure_space_dir(&self, name: &str) -> Result<()> {
        let dir = self.space_dir(name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))
    }

    pub fn memory_path(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("memory.md")
    }

    pub fn instructions_path(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("instructions.md")
    }

    /// Comma-separated domains `web_search` always excludes in this space.
    pub fn blocked_domains_path(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("blocked_domains.txt")
    }

    /// Directory holding a space's imported fileset (created on demand).
    pub fn files_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("files")
    }

    /// Directory holding a space's pasted conversation images.
    pub fn images_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("images")
    }

    /// Directory holding a space's model-created apps (created on demand).
    pub fn apps_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("apps")
    }

    /// Root of all spaces — what the app server serves from.
    pub fn spaces_root(&self) -> PathBuf {
        self.root.join("spaces")
    }

    /// Rename a space's directory (its db row is renamed separately).
    pub fn rename_space_dir(&self, old: &str, new: &str) -> Result<()> {
        let from = self.space_dir(old);
        let to = self.space_dir(new);
        if from.exists() {
            std::fs::rename(&from, &to)
                .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        }
        Ok(())
    }

    /// Remove a space's directory (memory + instructions gone with it).
    pub fn remove_space_dir(&self, name: &str) -> Result<()> {
        let dir = self.space_dir(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }
}
