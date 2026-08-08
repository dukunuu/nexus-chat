use anyhow::{Context, Result};

use super::{App, ImageMeta, ImagesMode};

impl App {
    /// Read the space's images dir and populate `images_cache` (name, size,
    /// modified). A missing or empty dir produces an empty cache, never an error.
    pub(crate) fn refresh_images(&mut self) {
        let dir = self.space.files_dir(&self.active_space.name);
        let _ = std::fs::create_dir_all(&dir);
        self.images_cache = match std::fs::read_dir(&dir) {
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
                    Some(ImageMeta {
                        name: e.file_name().to_string_lossy().to_string(),
                        size: meta.len(),
                        modified,
                    })
                })
                .collect(),
        };
        self.images_cache.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn move_images_selection(&mut self, delta: i32) {
        self.images_selected =
            super::clamp_cursor(self.images_selected, self.images_cache.len(), delta);
    }

    /// Enter in Browse: open the image in the system viewer.
    pub fn open_selected_image(&mut self) {
        let Some(img) = self.images_cache.get(self.images_selected) else {
            return;
        };
        let path = self
            .space
            .files_dir(&self.active_space.name)
            .join(&img.name);
        let _ = open::that_detached(&path);
        self.status = format!("opened {}", img.name);
    }

    /// Ctrl+D confirm: delete the image file from disk and refresh.
    pub fn confirm_images_delete(&mut self) -> Result<()> {
        let dir = self.space.files_dir(&self.active_space.name);
        if let Some(img) = self.images_cache.get(self.images_selected).cloned() {
            let path = dir.join(&img.name);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
            self.status = format!("removed {}", img.name);
            self.refresh_images();
        }
        self.images_mode = ImagesMode::Browse;
        Ok(())
    }
}
