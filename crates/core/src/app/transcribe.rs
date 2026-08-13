//! Image handling: encode clipboard images to PNG, attach them as markdown
//! `![alt](filename.ext)` in message content, and describe images for
//! non-vision models.

// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::{Context, Result};
use base64::Engine;

/// Encode raw RGBA pixels as PNG bytes.
pub fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().context("png header")?;
        writer.write_image_data(rgba).context("png data")?;
    }
    Ok(bytes)
}

/// `data:image/png;base64,…` URL for PNG bytes.
pub fn png_bytes_data_url(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Encode raw RGBA pixels as a `data:image/png;base64,…` URL. Only exercised
/// directly by tests now — production code goes through `encode_png` +
/// `png_bytes_data_url` separately to avoid re-decoding the PNG it just wrote.
#[cfg(test)]
pub fn png_data_url(width: usize, height: usize, rgba: &[u8]) -> Result<String> {
    let bytes = encode_png(width, height, rgba)?;
    Ok(png_bytes_data_url(&bytes))
}

impl super::App {
    /// Save a clipboard image to the space's images dir and return a markdown
    /// snippet `![pasted image](filename.ext)` that can be inserted into the
    /// composer text.
    pub fn save_clipboard_image(
        &mut self,
        width: usize,
        height: usize,
        bytes: &[u8],
    ) -> Option<String> {
        let bytes = match encode_png(width, height, bytes) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("could not encode image: {e}");
                return None;
            }
        };
        let (dir, filename) = if self.incognito {
            let d = self.incognito_img_dir.get_or_insert_with(|| {
                let p =
                    std::env::temp_dir().join(format!("nexus-incognito-{}", uuid::Uuid::new_v4()));
                let _ = std::fs::create_dir_all(&p);
                p
            });
            (d.clone(), format!("{}.png", uuid::Uuid::new_v4()))
        } else {
            let dir = self.space.files_dir(&self.active_space.name);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.status = format!("could not create {}: {e}", dir.display());
                return None;
            }
            (dir, format!("{}.png", uuid::Uuid::new_v4()))
        };
        let path = dir.join(&filename);
        if let Err(e) = std::fs::write(&path, &bytes) {
            self.status = format!("could not write {}: {e}", path.display());
            return None;
        }
        if !self.incognito {
            // Also save as a space file so the model can see/OCR/search it.
            let files_dir = self.space.files_dir(&self.active_space.name);
            if std::fs::create_dir_all(&files_dir).is_ok() {
                let _ = std::fs::write(files_dir.join(&filename), &bytes);
            }
            self.rescan_files();
        }
        Some(format!("![pasted image]({filename})"))
    }

    /// Remove the incognito temp image directory if it exists.
    pub fn cleanup_incognito_images(&mut self) {
        if let Some(d) = self.incognito_img_dir.take() {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_rgba_as_png_data_url() {
        // 2x1 image: red pixel, transparent pixel.
        let rgba = [255, 0, 0, 255, 0, 0, 0, 0];
        let url = png_data_url(2, 1, &rgba).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        // Round-trip: the payload decodes as a real 2x1 PNG.
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, 2);
        assert_eq!(reader.info().height, 1);
    }

    #[tokio::test]
    async fn save_clipboard_returns_markdown_snippet() {
        let mut a = crate::app::tests::app_with_key();
        a.incognito = true; // skip rescan_files -> tokio::spawn
        let rgba = vec![255, 0, 0, 255, 0, 0, 0, 0];
        let result = a.save_clipboard_image(2, 1, &rgba);
        assert!(result.is_some());
        let md = result.unwrap();
        assert!(md.starts_with("![pasted image]("));
        assert!(md.ends_with(".png)"));
    }
}
