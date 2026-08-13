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

use nexus_core::app::{FilesMode, FilesTab, Popup};

use crate::app_view::AppView;

/// One row of the file-picker browser.
pub struct PickerEntry {
    pub name: String,
    pub is_dir: bool,
}

impl AppView {
    /// Enter the picker at `picker_dir` (home on first open, remembered after).
    pub fn open_file_picker(&mut self) {
        self.picker_filter.clear();
        self.picker_selected = 0;
        self.reload_picker_entries();
        self.files_mode = FilesMode::Pick;
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
                let dir = self.picker_dir.display().to_string();
                self.push_status(format!("cannot read {dir}: {e}"));
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
        nexus_core::app::fuzzy_filter_sorted(&self.picker_entries, |e| {
            nexus_core::app::fuzzy_score(&e.name, needle)
        })
    }

    pub fn move_picker_selection(&mut self, delta: i32) {
        self.picker_selected = nexus_core::app::clamp_cursor(
            self.picker_selected,
            self.filtered_picker_entries().len(),
            delta,
        );
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
        if let Some(parent) = self.picker_dir.parent().map(std::path::Path::to_path_buf) {
            self.picker_dir = parent;
            self.picker_selected = 0;
            self.reload_picker_entries();
        }
    }

    /// Enter descends into a directory, or imports the selected file.
    pub fn picker_enter(&mut self) {
        let filtered = self.filtered_picker_entries();
        let Some(entry) = filtered.get(self.picker_selected) else {
            return;
        };
        let name = entry.name.clone();
        let is_dir = entry.is_dir;
        let path = self.picker_dir.join(&name);
        if is_dir {
            self.picker_dir = path;
            self.picker_filter.clear();
            self.picker_selected = 0;
            self.reload_picker_entries();
            return;
        }
        match self.core.import_file(&path) {
            Ok(n) => self.push_status(format!("imported {n}")),
            Err(e) => self.push_status(format!("import failed: {e}")),
        }
        self.files_mode = FilesMode::Browse;
    }

    /// Ctrl+O in /files: throw away the selected file's extracted text (and
    /// vectors) and re-index it from disk with the current OCR engine — how a
    /// tesseract-mangled book gets redone after configuring a VLM, without
    /// re-importing.
    pub fn reextract_selected_file(&mut self) {
        let Some(f) = self.core.files_cache.get(self.files_selected).cloned() else {
            return;
        };
        self.core.reextract_file(&f.name);
    }

    /// Force OCR on the selected file, bypassing text extraction entirely.
    /// Useful when `pdf_extract` gives unreliable text and you want VLM OCR
    /// output instead.
    pub fn reocr_selected_file(&mut self) {
        let Some(f) = self.core.files_cache.get(self.files_selected).cloned() else {
            return;
        };
        self.core.reocr_file(&f.name);
    }

    /// Delete the highlighted file: disk copy and index rows both go.
    pub fn confirm_files_delete(&mut self) -> Result<()> {
        if let Some(f) = self.core.files_cache.get(self.files_selected).cloned() {
            self.core.delete_file(&f.name)?;
        }
        self.files_mode = FilesMode::Browse;
        Ok(())
    }

    pub fn open_files_popup(&mut self, tab: FilesTab) {
        self.files_tab = tab;
        match tab {
            FilesTab::Images => {
                self.core.refresh_images();
            }
            FilesTab::Scripts => {
                self.core.refresh_scripts();
            }
            FilesTab::Files => {
                self.core.rescan_files();
            }
        }
        self.files_mode = FilesMode::Browse;
        self.popup = Popup::Files;
    }

    pub fn move_files_selection(&mut self, delta: i32) {
        self.files_selected =
            nexus_core::app::clamp_cursor(self.files_selected, self.core.files_cache.len(), delta);
    }

    pub fn start_files_add(&mut self) {
        self.files_edit.clear();
        self.files_mode = FilesMode::Add;
    }

    /// Import the path typed/pasted in Add mode. Bad paths report in the status
    /// line and return to Browse (nothing to roll back).
    pub fn confirm_files_add(&mut self) {
        let raw = self.files_edit.trim().to_string();
        self.files_mode = FilesMode::Browse;
        if raw.is_empty() {
            return;
        }
        let path = std::path::PathBuf::from(&raw);
        if !path.is_file() {
            self.push_status(format!("not a file: {raw}"));
            return;
        }
        match self.core.import_file(&path) {
            Ok(name) => self.push_status(format!("imported {name}")),
            Err(e) => self.push_status(format!("import failed: {e}")),
        }
    }

    /// Ctrl+R in Browse: pre-fill the edit line with the current name.
    pub fn start_files_rename(&mut self) {
        if let Some(f) = self.core.files_cache.get(self.files_selected) {
            self.files_edit = f.name.clone();
            self.files_mode = FilesMode::Rename;
        }
    }

    /// Rename the highlighted file on disk; the rescan swaps the index rows
    /// (old name dropped, new name re-extracted).
    pub fn confirm_files_rename(&mut self) {
        let new = self.files_edit.trim().to_string();
        self.files_mode = FilesMode::Browse;
        let Some(f) = self.core.files_cache.get(self.files_selected).cloned() else {
            return;
        };
        if new.is_empty() || new == f.name {
            return;
        }
        if let Err(e) = self.core.rename_file(&f.name, &new) {
            self.push_status(e.to_string());
            return;
        }
        // Re-point the cursor at the renamed row (the rescan reordered).
        self.files_selected = self
            .core
            .files_cache
            .iter()
            .position(|f| f.name == new)
            .unwrap_or(self.files_selected);
    }

    /// Open the highlighted file in the system viewer (Enter in Browse).
    pub fn open_selected_file(&mut self) {
        let Some(f) = self.core.files_cache.get(self.files_selected).cloned() else {
            return;
        };
        let path = self
            .core
            .space
            .files_dir(&self.core.active_space.name)
            .join(&f.name);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "md" {
            self.pending_editor = Some(nexus_core::app::PendingEditor::ScriptFile(path));
        } else {
            let _ = open::that_detached(&path);
        }
        self.push_status(format!("opened {}", f.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::{App, AppCommand};
    use nexus_core::db::Db;
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-files-flow-{}", uuid::Uuid::new_v4())),
        }
    }

    fn test_app() -> AppView {
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("sk-or-test-key"),
            test_space(),
        ))
    }

    #[test]
    fn files_command_opens_popup_and_rescans() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seen.txt"), "content").unwrap();
        a.run_command("files").unwrap();
        assert_eq!(a.popup, nexus_core::app::Popup::Files);
        assert_eq!(a.core.files_cache.len(), 1);
        assert!(a.files_mode == nexus_core::app::FilesMode::Browse);
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
        assert!(a.files_mode == nexus_core::app::FilesMode::Pick);
        let names: Vec<&str> = a
            .filtered_picker_entries()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["subdir", "aaa.txt", "bbb.txt"]); // dirs first, then alpha

        // Enter on a dir descends and reloads.
        a.picker_selected = 0;
        a.picker_enter();
        assert_eq!(a.picker_dir, root.join("subdir"));
        assert!(a.filtered_picker_entries().is_empty());

        // Backspace with empty filter ascends.
        a.picker_backspace();
        assert_eq!(a.picker_dir, root);

        // Enter on a file imports it and returns to Browse.
        let idx = a
            .filtered_picker_entries()
            .iter()
            .position(|e| e.name == "aaa.txt")
            .unwrap();
        a.picker_selected = idx;
        a.picker_enter();
        assert!(a.files_mode == nexus_core::app::FilesMode::Browse);
        assert!(a.core.files_cache.iter().any(|f| f.name == "aaa.txt"));
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
        let names: Vec<&str> = a
            .filtered_picker_entries()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["report-2026.pdf"]); // fuzzy subsequence "rpt"

        // Backspace edits the filter (does NOT ascend while filter non-empty).
        a.picker_backspace();
        assert_eq!(a.picker_filter, "rp");
        assert_eq!(a.picker_dir, root);
    }
}
