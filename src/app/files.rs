//! Space filesets: importing files into `spaces/<name>/files/`, keeping the
//! db index in sync with the directory, and extracting searchable text.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::App;

impl App {
    /// Sync the active space's files directory with the db: new or changed
    /// files (by sha256) are re-extracted and re-indexed, rows for deleted
    /// files are dropped, and `files_cache` is refreshed. Best-effort: a
    /// single bad file gets an "error: …" status instead of failing the scan.
    /// ponytail: runs synchronously on the UI task — extraction of a huge PDF
    /// blocks a beat; move to a blocking task if that ever hurts.
    pub fn rescan_files(&mut self) {
        let dir = self.space.files_dir(&self.active_space.name);
        let known = self.db.list_files(&self.active_space.id).unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();

        let entries = std::fs::read_dir(&dir).map(|rd| rd.flatten().collect::<Vec<_>>()).unwrap_or_default();
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            seen.push(name.clone());
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let hash = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
            if known.iter().any(|f| f.name == name && f.hash == hash) {
                continue; // unchanged
            }
            let size = bytes.len() as i64;
            let (status, chunks) = match crate::extract::extract_text(&path) {
                Ok(text) if text.trim().is_empty() => ("no text (scanned?)".to_string(), Vec::new()),
                Ok(text) => ("ok".to_string(), crate::extract::chunk_lines(&text)),
                Err(e) => (format!("error: {e}"), Vec::new()),
            };
            if let Ok(id) = self.db.upsert_file(&self.active_space.id, &name, &hash, size, &status) {
                let _ = self.db.set_file_chunks(&id, &chunks);
            }
        }
        for gone in known.iter().filter(|f| !seen.contains(&f.name)) {
            let _ = self.db.delete_file(&gone.id);
        }
        self.files_cache = self.db.list_files(&self.active_space.id).unwrap_or_default();
        self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
    }

    /// Copy `path` into the active space's files dir and index it. Returns
    /// the imported file's name. An existing file with the same name is
    /// overwritten (the rescan re-extracts it).
    #[allow(dead_code)] // called from the files popup (Task 6+); remove with first caller
    pub fn import_file(&mut self, path: &Path) -> Result<String> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .context("path has no file name")?;
        let dir = self.space.files_dir(&self.active_space.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::copy(path, dir.join(&name))
            .with_context(|| format!("copying {} into the space", path.display()))?;
        self.rescan_files();
        Ok(name)
    }

    /// Delete the highlighted file: disk copy and index rows both go.
    #[allow(dead_code)] // called from the files popup (Task 6+); remove with first caller
    pub fn confirm_files_delete(&mut self) -> Result<()> {
        if let Some(f) = self.files_cache.get(self.files_selected).cloned() {
            let disk = self.space.files_dir(&self.active_space.name).join(&f.name);
            if disk.exists() {
                std::fs::remove_file(&disk).with_context(|| format!("removing {}", disk.display()))?;
            }
            self.db.delete_file(&f.id)?;
            self.status = format!("removed {}", f.name);
            self.rescan_files();
        }
        self.files_mode = super::FilesMode::Browse;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root = std::env::temp_dir().join(format!("nexus-files-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k".into()), space)
    }

    #[test]
    fn import_copies_extracts_and_indexes() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-src-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&src, "# quarterly report\nrevenue up").unwrap();

        let name = a.import_file(&src).unwrap();
        assert_eq!(name, src.file_name().unwrap().to_string_lossy());
        assert_eq!(a.files_cache.len(), 1);
        assert_eq!(a.files_cache[0].status, "ok");
        // Copied into the space's files dir.
        assert!(a.space.files_dir(&a.active_space.name).join(&name).exists());
        // Indexed: searchable.
        let hits = crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn rescan_picks_up_dropped_and_deleted_files() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dropped.txt"), "hello dropped").unwrap();

        a.rescan_files();
        assert_eq!(a.files_cache.len(), 1);
        assert_eq!(a.files_cache[0].name, "dropped.txt");

        // Changing content re-extracts (hash change), deleting drops the row.
        std::fs::write(dir.join("dropped.txt"), "hello again").unwrap();
        a.rescan_files();
        assert_eq!(a.files_cache.len(), 1);
        std::fs::remove_file(dir.join("dropped.txt")).unwrap();
        a.rescan_files();
        assert!(a.files_cache.is_empty());
    }

    #[test]
    fn empty_extraction_gets_no_text_status() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.txt"), "   ").unwrap();
        a.rescan_files();
        assert_eq!(a.files_cache[0].status, "no text (scanned?)");
    }

    #[test]
    fn delete_removes_disk_file_and_row() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gone.txt"), "bye").unwrap();
        a.rescan_files();
        a.files_selected = 0;
        a.confirm_files_delete().unwrap();
        assert!(a.files_cache.is_empty());
        assert!(!dir.join("gone.txt").exists());
    }
}
