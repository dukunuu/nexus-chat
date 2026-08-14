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

use nexus_core::app::ImagesMode;

use crate::app_view::AppView;

impl AppView {
    pub fn move_images_selection(&mut self, delta: i32) {
        self.images_selected = nexus_core::app::clamp_cursor(
            self.images_selected,
            self.core.images_cache.len(),
            delta,
        );
    }

    /// Enter in Browse: open the image in the system viewer.
    pub fn open_selected_image(&mut self) {
        let Some(img) = self.core.images_cache.get(self.images_selected).cloned() else {
            return;
        };
        let path = self
            .core
            .space
            .files_dir(&self.core.active_space.name)
            .join(&img.name);
        let _ = open::that_detached(&path);
        self.push_status(format!("opened {}", img.name));
    }

    /// Ctrl+D confirm: delete the image file from disk and refresh.
    pub fn confirm_images_delete(&mut self) -> Result<()> {
        if let Some(img) = self.core.images_cache.get(self.images_selected).cloned() {
            self.core.delete_image_file(&img.name)?;
        }
        self.images_mode = ImagesMode::Browse;
        Ok(())
    }
}
