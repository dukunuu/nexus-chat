//! Studio's small, per-device settings: the folder each space last exported to.
//! They live in `studio-settings.json` at the data root (beside
//! `studio-recent.json`), are never synced and are safe to delete.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize)]
struct Settings {
    /// Space files folder (as text) to the folder its last export went to.
    #[serde(default)]
    export_dirs: BTreeMap<String, PathBuf>,
}

fn file(data_root: &Path) -> PathBuf {
    data_root.join("studio-settings.json")
}

fn load(data_root: &Path) -> Settings {
    std::fs::read(file(data_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The folder this space last exported to, if it still exists.
#[must_use]
pub fn export_dir(data_root: &Path, files: &Path) -> Option<PathBuf> {
    load(data_root)
        .export_dirs
        .remove(&files.to_string_lossy().into_owned())
        .filter(|dir| dir.is_dir())
}

/// Remember the folder of a file just exported for `files`.
pub fn remember_export(data_root: &Path, files: &Path, exported: &Path) -> Result<()> {
    let Some(dir) = exported.parent().filter(|d| d.is_absolute()) else {
        return Ok(());
    };
    let mut settings = load(data_root);
    settings
        .export_dirs
        .insert(files.to_string_lossy().into_owned(), dir.to_path_buf());
    crate::deck::write_atomic(&file(data_root), &serde_json::to_vec_pretty(&settings)?)
}
