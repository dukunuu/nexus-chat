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

use std::path::PathBuf;

use nexus_core::app::{FilesMode, FilesTab, Popup};
use nexus_core::filesearch;

use crate::app_view::AppView;

/// Rows the find picker shows at most.
const PICKER_MAX: usize = 200;

impl AppView {
    /// Ctrl+N in /files: the system Open panel (several files at once) when
    /// a desktop session can show one, otherwise the find picker.
    pub fn import_files(&mut self) {
        if !self.dialogs.available() {
            self.open_file_picker();
            return;
        }
        let start = self.picker_home.join("Downloads");
        let title = format!("Import into {}", self.active_space.name);
        let picked = self.dialogs.open_files(&title, Some(&start), &[]);
        self.import_paths(&picked);
    }

    /// Copy each path into the space, with one status line for the batch.
    pub fn import_paths(&mut self, paths: &[PathBuf]) {
        let mut imported = Vec::new();
        for path in paths {
            match self.core.import_file(path) {
                Ok(name) => imported.push(name),
                Err(e) => {
                    self.push_status(format!("import failed: {e}"));
                    return;
                }
            }
        }
        match imported.as_slice() {
            [] => {}
            [name] => self.push_status(format!("imported {name}")),
            names => self.push_status(format!("imported {} files", names.len())),
        }
    }

    /// Open the find picker on the newest files in the usual drop folders.
    pub fn open_file_picker(&mut self) {
        self.picker_query.clear();
        self.files_mode = FilesMode::Pick;
        self.refresh_picker();
    }

    /// Re-run the picker for its query: recent files when empty, path
    /// completion for `/…` and `~…`, otherwise a background search whose
    /// results replace the current ones when they land.
    fn refresh_picker(&mut self) {
        self.picker_selected = 0;
        self.picker_rx = None;
        let home = self.picker_home.clone();
        let query = self.picker_query.clone();
        if query.trim().is_empty() {
            self.picker_results = filesearch::recent(&home, PICKER_MAX);
        } else if query.starts_with('/') || query.starts_with('~') {
            self.picker_results = filesearch::complete(&query, &home);
        } else {
            let spotlight = self.picker_spotlight;
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(filesearch::search(&home, &query, PICKER_MAX, spotlight));
            });
            self.picker_rx = Some(rx);
        }
    }

    /// A background search finished for the current query.
    pub fn finish_picker_search(&mut self, results: Vec<filesearch::Found>) {
        self.picker_rx = None;
        self.picker_results = results;
        self.picker_selected = 0;
    }

    pub fn move_picker_selection(&mut self, delta: i32) {
        self.picker_selected =
            nexus_core::app::clamp_cursor(self.picker_selected, self.picker_results.len(), delta);
    }

    pub fn picker_push(&mut self, c: char) {
        self.picker_type(c.encode_utf8(&mut [0; 4]));
    }

    /// Append typed or pasted text to the query and search once for it.
    pub fn picker_type(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.picker_query.push_str(text);
        self.refresh_picker();
    }

    pub fn picker_backspace(&mut self) {
        if self.picker_query.pop().is_some() {
            self.refresh_picker();
        }
    }

    /// Enter imports the highlighted file, or completes into a directory.
    pub fn picker_enter(&mut self) {
        let Some(found) = self.picker_results.get(self.picker_selected).cloned() else {
            return;
        };
        if found.is_dir {
            self.picker_complete();
            return;
        }
        self.import_paths(&[found.path]);
        self.files_mode = FilesMode::Browse;
    }

    /// Tab puts the highlighted entry's path in the query (a directory gets
    /// its trailing slash), switching to path completion.
    pub fn picker_complete(&mut self) {
        let Some(found) = self.picker_results.get(self.picker_selected) else {
            return;
        };
        let mut query = filesearch::tilde(&found.path, &self.picker_home);
        if found.is_dir {
            query.push('/');
        }
        self.picker_query = query;
        self.refresh_picker();
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
        } else if crate::image_viewer::is_image_extension(ext) {
            // Images get the viewer (terminal first); it says so itself when
            // it falls back to the system viewer.
            self.open_image_target(
                &path.to_string_lossy(),
                &crate::image_viewer::Anchor::ByTarget,
                crate::image_viewer::Return::Files,
            );
            if self.popup == nexus_core::app::Popup::ImageViewer {
                return;
            }
        } else {
            let _ = open::that_detached(&path);
        }
        self.push_status(format!("opened {}", f.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
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
    /// A picker whose home is a temp folder holding `files`.
    fn picker_app(files: &[&str]) -> (AppView, PathBuf) {
        let mut a = test_app();
        let home = std::env::temp_dir().join(format!("nexus-pick-{}", uuid::Uuid::new_v4()));
        for file in files {
            let path = home.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "content").unwrap();
        }
        a.picker_home = home.canonicalize().unwrap();
        (a, home)
    }

    fn shown(a: &AppView) -> Vec<String> {
        a.picker_results
            .iter()
            .map(|f| filesearch::tilde(&f.path, &a.picker_home))
            .collect()
    }

    #[test]
    fn picker_opens_on_recent_files_and_searches_in_the_background() {
        let (mut a, home) = picker_app(&["Downloads/report.pdf", "work/deep/q3 report.txt"]);
        a.open_file_picker();
        assert!(a.files_mode == FilesMode::Pick);
        assert_eq!(shown(&a), ["~/Downloads/report.pdf"]);

        for c in "q3 rep".chars() {
            a.picker_push(c);
        }
        let results = a.picker_rx.take().unwrap().blocking_recv().unwrap();
        a.finish_picker_search(results);
        assert_eq!(shown(&a), ["~/work/deep/q3 report.txt"]);

        a.picker_enter();
        assert!(a.files_mode == FilesMode::Browse);
        assert!(a.core.files_cache.iter().any(|f| f.name == "q3 report.txt"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn picker_completes_typed_paths_and_enter_descends_into_directories() {
        let (mut a, home) = picker_app(&["Documents/notes.md", "docs.txt"]);
        a.open_file_picker();
        for c in "~/do".chars() {
            a.picker_push(c);
        }
        assert!(a.picker_rx.is_none(), "path completion answers at once");
        assert_eq!(shown(&a), ["~/Documents", "~/docs.txt"]);

        a.picker_enter();
        assert_eq!(a.picker_query, "~/Documents/");
        assert_eq!(shown(&a), ["~/Documents/notes.md"]);
        a.picker_backspace();
        assert_eq!(shown(&a), ["~/Documents"]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn ctrl_n_imports_what_the_open_panel_picks_or_falls_back_to_the_picker() {
        use nexus_core::canvas::dialogs::Scripted;
        let (mut a, home) = picker_app(&["a.txt", "b.txt"]);
        let picked = a.picker_home.join("a.txt");
        a.dialogs = Box::new(Scripted::new([Some(picked)]));
        a.import_files();
        assert!(a.core.files_cache.iter().any(|f| f.name == "a.txt"));
        assert!(a.files_mode == FilesMode::Browse);

        a.dialogs = Box::new(Scripted::unavailable());
        a.import_files();
        assert!(a.files_mode == FilesMode::Pick);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn files_dropped_on_the_popup_import_at_once_with_escaped_names() {
        let (mut a, home) = picker_app(&["My File.pdf", "notes.md"]);
        a.open_files_popup(FilesTab::Files);
        let root = a.picker_home.display().to_string();
        a.paste(&format!("{root}/My\\ File.pdf '{root}/notes.md'"));
        let names: Vec<&str> = a.core.files_cache.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"My File.pdf") && names.contains(&"notes.md"),
            "{names:?}"
        );
        assert!(a.files_mode == FilesMode::Browse);

        a.open_file_picker();
        a.paste("not a path");
        assert_eq!(a.picker_query, "not a path");
        std::fs::remove_dir_all(home).unwrap();
    }
}
