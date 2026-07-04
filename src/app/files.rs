//! Space filesets: importing files into `spaces/<name>/files/`, keeping the
//! db index in sync with the directory, and extracting searchable text.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::App;

/// One row of the file-picker browser.
pub struct PickerEntry {
    pub name: String,
    pub is_dir: bool,
}

impl App {
    /// Enter the picker at `picker_dir` (home on first open, remembered after).
    pub(crate) fn open_file_picker(&mut self) {
        self.picker_filter.clear();
        self.picker_selected = 0;
        self.reload_picker_entries();
        self.files_mode = super::FilesMode::Pick;
    }

    /// Re-read the current directory: dirs first, then files, both alphabetical.
    /// Unreadable dirs just yield an empty list (status explains).
    fn reload_picker_entries(&mut self) {
        let mut entries: Vec<PickerEntry> = match std::fs::read_dir(&self.picker_dir) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let is_dir = e.file_type().ok()?.is_dir();
                    Some(PickerEntry { name, is_dir })
                })
                .collect(),
            Err(e) => {
                self.status = format!("cannot read {}: {e}", self.picker_dir.display());
                Vec::new()
            }
        };
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        self.picker_entries = entries;
    }

    /// Entries matching the fuzzy filter (all of them, dirs first, when empty).
    pub fn filtered_picker_entries(&self) -> Vec<&PickerEntry> {
        let needle = self.picker_filter.trim();
        if needle.is_empty() {
            return self.picker_entries.iter().collect();
        }
        use crate::input::fuzzy_score;
        super::fuzzy_filter_sorted(&self.picker_entries, |e| fuzzy_score(&e.name, needle))
    }

    pub fn move_picker_selection(&mut self, delta: i32) {
        self.picker_selected =
            super::clamp_cursor(self.picker_selected, self.filtered_picker_entries().len(), delta);
    }

    pub fn picker_filter_push(&mut self, c: char) {
        self.picker_filter.push(c);
        self.picker_selected = 0;
    }

    /// Backspace erases the filter first; on an empty filter it goes up a level.
    pub fn picker_backspace(&mut self) {
        if !self.picker_filter.is_empty() {
            self.picker_filter.pop();
            self.picker_selected = 0;
            return;
        }
        if let Some(parent) = self.picker_dir.parent().map(|p| p.to_path_buf()) {
            self.picker_dir = parent;
            self.picker_selected = 0;
            self.reload_picker_entries();
        }
    }

    /// Enter descends into a directory, or imports the selected file.
    pub fn picker_enter(&mut self) -> Result<()> {
        let filtered = self.filtered_picker_entries();
        let Some(entry) = filtered.get(self.picker_selected) else {
            return Ok(());
        };
        let name = entry.name.clone();
        let is_dir = entry.is_dir;
        let path = self.picker_dir.join(&name);
        if is_dir {
            self.picker_dir = path;
            self.picker_filter.clear();
            self.picker_selected = 0;
            self.reload_picker_entries();
            return Ok(());
        }
        match self.import_file(&path) {
            Ok(n) => self.status = format!("imported {n}"),
            Err(e) => self.status = format!("import failed: {e}"),
        }
        self.files_mode = super::FilesMode::Browse;
        Ok(())
    }

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
        let mut ocr_jobs: Vec<(String, String, std::path::PathBuf)> = Vec::new();

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
            if let Some(f) = known.iter().find(|f| f.name == name && f.hash == hash) {
                // Stale "ocr…" (app quit mid-OCR) re-queues once no batch is in flight.
                if f.status == "ocr…" && self.ocr_rx.is_none() {
                    ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                }
                continue;
            }
            let size = bytes.len() as i64;
            let (status, chunks) = match crate::extract::extract_text(&path) {
                Ok(text) if text.trim().is_empty() => {
                    if name.to_lowercase().ends_with(".pdf") {
                        ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                        ("ocr…".to_string(), Vec::new())
                    } else {
                        ("no text (scanned?)".to_string(), Vec::new())
                    }
                }
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
        self.start_ocr(ocr_jobs);
        self.files_cache = self.db.list_files(&self.active_space.id).unwrap_or_default();
        self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
    }

    /// OCR queued scanned PDFs sequentially off the UI thread. One batch at a
    /// time: jobs arriving while a batch runs stay at "ocr…" and re-queue on a
    /// later rescan.
    pub(crate) fn start_ocr(&mut self, jobs: Vec<(String, String, std::path::PathBuf)>) {
        if jobs.is_empty() || self.ocr_rx.is_some() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.ocr_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            for (space_id, name, path) in jobs {
                let result = match crate::extract::ocr_pdf(&path) {
                    Ok(text) => Ok(text),
                    Err(crate::extract::OcrError::MissingTools) => {
                        Err("scanned pdf — install tesseract + poppler for ocr".to_string())
                    }
                    Err(crate::extract::OcrError::Failed(e)) => Err(format!("error: ocr: {e}")),
                };
                if tx.send((space_id, name, result)).is_err() {
                    return;
                }
            }
        });
    }

    /// A finished OCR job: persist chunks/status, refresh the cache only if the
    /// file's space is still active. `None` = batch done (channel closed).
    pub fn on_ocr_done(
        &mut self,
        r: Option<(String, String, std::result::Result<String, String>)>,
    ) {
        let Some((space_id, name, result)) = r else {
            self.ocr_rx = None;
            return;
        };
        let Ok(files) = self.db.list_files(&space_id) else { return };
        let Some(f) = files.iter().find(|f| f.name == name) else {
            return; // deleted mid-OCR
        };
        match result {
            Ok(text) if text.trim().is_empty() => {
                let _ = self.db.set_file_status(&f.id, "no text (ocr found nothing)");
            }
            Ok(text) => {
                let _ = self.db.set_file_chunks(&f.id, &crate::extract::chunk_lines(&text));
                let _ = self.db.set_file_status(&f.id, "ok");
            }
            Err(msg) => {
                let _ = self.db.set_file_status(&f.id, &msg);
            }
        }
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
            self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
        }
    }

    /// Copy `path` into the active space's files dir and index it. Returns
    /// the imported file's name. An existing file with the same name is
    /// overwritten (the rescan re-extracts it).
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

    pub(crate) fn open_files_popup(&mut self) {
        self.rescan_files();
        self.files_mode = super::FilesMode::Browse;
        self.popup = super::Popup::Files;
    }

    pub fn move_files_selection(&mut self, delta: i32) {
        self.files_selected = super::clamp_cursor(self.files_selected, self.files_cache.len(), delta);
    }

    pub fn start_files_add(&mut self) {
        self.files_edit.clear();
        self.files_mode = super::FilesMode::Add;
    }

    /// Import the path typed/pasted in Add mode. Bad paths report in the status
    /// line and return to Browse (nothing to roll back).
    pub fn confirm_files_add(&mut self) -> Result<()> {
        let raw = self.files_edit.trim().to_string();
        self.files_mode = super::FilesMode::Browse;
        if raw.is_empty() {
            return Ok(());
        }
        let path = std::path::PathBuf::from(&raw);
        if !path.is_file() {
            self.status = format!("not a file: {raw}");
            return Ok(());
        }
        match self.import_file(&path) {
            Ok(name) => self.status = format!("imported {name}"),
            Err(e) => self.status = format!("import failed: {e}"),
        }
        Ok(())
    }

    /// Open the highlighted file in the system viewer (Enter in Browse).
    pub fn open_selected_file(&mut self) {
        if let Some(f) = self.files_cache.get(self.files_selected) {
            let path = self.space.files_dir(&self.active_space.name).join(&f.name);
            let _ = open::that_detached(&path);
            self.status = format!("opened {}", f.name);
        }
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

    #[test]
    fn files_command_opens_popup_and_rescans() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seen.txt"), "content").unwrap();
        a.run_command("files").unwrap();
        assert!(a.popup == crate::app::Popup::Files);
        assert_eq!(a.files_cache.len(), 1);
        assert!(a.files_mode == crate::app::FilesMode::Browse);
    }

    #[test]
    fn confirm_files_add_imports_typed_path() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-add-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&src, "typed in").unwrap();
        a.start_files_add();
        assert!(a.files_mode == crate::app::FilesMode::Add);
        a.files_edit = src.to_string_lossy().to_string();
        a.confirm_files_add().unwrap();
        assert!(a.files_mode == crate::app::FilesMode::Browse);
        assert_eq!(a.files_cache.len(), 1);

        // A bad path reports in status and stays recoverable.
        a.start_files_add();
        a.files_edit = "/definitely/not/a/file".to_string();
        a.confirm_files_add().unwrap();
        assert!(a.status.contains("not a file"));
        assert_eq!(a.files_cache.len(), 1);
    }

    #[test]
    fn picker_lists_dirs_first_descends_and_imports() {
        let mut a = test_app();
        let root = std::env::temp_dir().join(format!("nexus-pick-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("subdir")).unwrap();
        std::fs::write(root.join("bbb.txt"), "file b").unwrap();
        std::fs::write(root.join("aaa.txt"), "file a").unwrap();

        a.picker_dir = root.clone();
        a.open_file_picker();
        assert!(a.files_mode == crate::app::FilesMode::Pick);
        let names: Vec<&str> = a.filtered_picker_entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["subdir", "aaa.txt", "bbb.txt"]); // dirs first, then alpha

        // Enter on a dir descends and reloads.
        a.picker_selected = 0;
        a.picker_enter().unwrap();
        assert_eq!(a.picker_dir, root.join("subdir"));
        assert!(a.filtered_picker_entries().is_empty());

        // Backspace with empty filter ascends.
        a.picker_backspace();
        assert_eq!(a.picker_dir, root);

        // Enter on a file imports it and returns to Browse.
        let idx = a.filtered_picker_entries().iter().position(|e| e.name == "aaa.txt").unwrap();
        a.picker_selected = idx;
        a.picker_enter().unwrap();
        assert!(a.files_mode == crate::app::FilesMode::Browse);
        assert!(a.files_cache.iter().any(|f| f.name == "aaa.txt"));
    }

    #[test]
    fn picker_filter_fuzzy_matches_and_backspace_edits_filter_first() {
        let mut a = test_app();
        let root = std::env::temp_dir().join(format!("nexus-pick-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("report-2026.pdf"), "x").unwrap();
        std::fs::write(root.join("notes.md"), "y").unwrap();
        a.picker_dir = root.clone();
        a.open_file_picker();

        a.picker_filter_push('r');
        a.picker_filter_push('p');
        a.picker_filter_push('t');
        let names: Vec<&str> = a.filtered_picker_entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["report-2026.pdf"]); // fuzzy subsequence "rpt"

        // Backspace edits the filter (does NOT ascend while filter non-empty).
        a.picker_backspace();
        assert_eq!(a.picker_filter, "rp");
        assert_eq!(a.picker_dir, root);
    }

    #[tokio::test]
    async fn rescan_marks_empty_pdf_ocr_and_spawns_batch() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scan.pdf"), crate::extract::minimal_pdf(None)).unwrap();

        a.rescan_files();
        assert_eq!(a.files_cache[0].status, "ocr…");
        assert!(a.ocr_rx.is_some(), "an ocr batch should be in flight");

        // A second rescan while the batch is in flight does not re-queue.
        a.rescan_files();
        assert_eq!(a.files_cache[0].status, "ocr…");
    }

    #[tokio::test]
    async fn rescan_requeues_stale_ocr_status_when_idle() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scan.pdf"), crate::extract::minimal_pdf(None)).unwrap();
        a.rescan_files();

        // Simulate an app restart mid-OCR: status stuck at "ocr…", no batch in flight.
        a.ocr_rx = None;
        a.rescan_files();
        assert!(a.ocr_rx.is_some(), "stale ocr… should re-queue");
    }

    #[test]
    fn on_ocr_done_ok_indexes_and_marks_ok() {
        let mut a = test_app();
        let id = a.db.upsert_file(&a.active_space.id, "scan.pdf", "h", 9, "ocr…").unwrap();
        let _ = id;
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "scan.pdf".to_string(),
            Ok("[page 1]\nquarterly revenue table".to_string()),
        )));
        assert_eq!(a.files_cache[0].status, "ok");
        let hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn on_ocr_done_empty_and_err_statuses() {
        let mut a = test_app();
        a.db.upsert_file(&a.active_space.id, "blank.pdf", "h1", 9, "ocr…").unwrap();
        a.db.upsert_file(&a.active_space.id, "bad.pdf", "h2", 9, "ocr…").unwrap();

        a.on_ocr_done(Some((a.active_space.id.clone(), "blank.pdf".to_string(), Ok(String::new()))));
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "bad.pdf".to_string(),
            Err("scanned pdf — install tesseract + poppler for ocr".to_string()),
        )));

        let by_name = |a: &App, n: &str| {
            a.files_cache.iter().find(|f| f.name == n).unwrap().status.clone()
        };
        assert_eq!(by_name(&a, "blank.pdf"), "no text (ocr found nothing)");
        assert_eq!(by_name(&a, "bad.pdf"), "scanned pdf — install tesseract + poppler for ocr");
    }

    #[test]
    fn on_ocr_done_for_inactive_space_writes_db_but_not_cache() {
        let mut a = test_app();
        let other = a.db.create_space("other").unwrap();
        a.db.upsert_file(&other.id, "scan.pdf", "h", 9, "ocr…").unwrap();

        a.on_ocr_done(Some((other.id.clone(), "scan.pdf".to_string(), Ok("found text".to_string()))));

        assert!(a.files_cache.is_empty(), "active-space cache must not show other space's file");
        let rows = a.db.list_files(&other.id).unwrap();
        assert_eq!(rows[0].status, "ok");

        // Deleted-mid-OCR: result for a row that no longer exists is a no-op.
        a.on_ocr_done(Some((other.id.clone(), "gone.pdf".to_string(), Ok("x".to_string()))));
    }

    #[test]
    fn on_ocr_done_none_clears_channel() {
        let mut a = test_app();
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        a.ocr_rx = Some(rx);
        a.on_ocr_done(None);
        assert!(a.ocr_rx.is_none());
    }
}
