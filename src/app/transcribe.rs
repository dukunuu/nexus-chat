//! Conversation image attachments: stage a pasted clipboard image as a saved
//! PNG under the active space's `images/` dir, and describe it in the
//! background (for models that can't see images) via the small vision model.

use anyhow::{Context, Result};
use base64::Engine;
use tokio::sync::mpsc;

use super::App;

/// Encode raw RGBA pixels as PNG bytes.
pub(super) fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
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
pub(super) fn png_bytes_data_url(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Encode raw RGBA pixels as a `data:image/png;base64,…` URL. Only exercised
/// directly by tests now — production code goes through `encode_png` +
/// `png_bytes_data_url` separately to avoid re-decoding the PNG it just wrote.
#[cfg(test)]
pub(super) fn png_data_url(width: usize, height: usize, rgba: &[u8]) -> Result<String> {
    let bytes = encode_png(width, height, rgba)?;
    Ok(png_bytes_data_url(&bytes))
}

/// A pasted image waiting to be sent with the next message.
pub struct PendingImage {
    pub path: std::path::PathBuf,
}

impl App {
    /// Save a clipboard image to the space's images dir and stage it for the
    /// next message. No model call happens here — vision models get the raw
    /// image at send; non-vision models trigger description on demand.
    pub fn attach_clipboard_image(&mut self, img: arboard::ImageData) {
        let bytes = match encode_png(img.width, img.height, &img.bytes) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("could not encode image: {e}");
                return;
            }
        };
        let dir = self.space.images_dir(&self.active_space.name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.status = format!("could not create {}: {e}", dir.display());
            return;
        }
        let path = dir.join(format!("{}.png", uuid::Uuid::new_v4()));
        if let Err(e) = std::fs::write(&path, &bytes) {
            self.status = format!("could not write {}: {e}", path.display());
            return;
        }
        // Also save as a space file so the model can see/OCR/search it.
        {
            let files_dir = self.space.files_dir(&self.active_space.name);
            if std::fs::create_dir_all(&files_dir).is_ok() {
                let _ = std::fs::write(files_dir.join(path.file_name().unwrap()), &bytes);
            }
        }
        self.rescan_files();
        self.pending_images.push(PendingImage { path });
        let n = self.pending_images.len();
        self.status = format!(
            "{n} image{} attached (Esc clears)",
            if n == 1 { "" } else { "s" }
        );
    }

    /// Describe `todo` images ((message_images row id, png path)) with the image
    /// model, one at a time; results arrive as AppEvent::Described.
    pub(crate) fn start_describing(&mut self, todo: Vec<(String, String)>) {
        let model = self.transcriber_model.trim().to_string();
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            return;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        self.describe_rx = Some(rx);
        self.status = "understanding image…".to_string();
        tokio::spawn(async move {
            for (image_id, path) in todo {
                let result = match std::fs::read(&path) {
                    Ok(bytes) => {
                        let url = png_bytes_data_url(&bytes);
                        provider
                            .describe_image(&raw_model, &url)
                            .await
                            .map_err(|e| e.to_string())
                    }
                    Err(e) => Err(format!("could not read {path}: {e}")),
                };
                let _ = tx.send((image_id, result));
            }
        });
    }

    /// One description finished (or the channel closed → all done).
    pub fn on_described(&mut self, r: Option<(String, std::result::Result<String, String>)>) {
        match r {
            Some((image_id, Ok(desc))) => {
                let _ = self.db.set_image_description(&image_id, &desc);
                for m in &mut self.messages {
                    for img in &mut m.images {
                        if img.id == image_id {
                            img.description = Some(desc.clone());
                        }
                    }
                }
            }
            Some((_, Err(e))) => {
                self.status = format!("image understanding failed: {e}");
                self.deferred_send = None; // abort the pending send; user retries
            }
            None => {
                self.describe_rx = None;
                self.resume_deferred_send();
            }
        }
    }

    /// All descriptions arrived: fire the request that was waiting on them.
    pub(crate) fn resume_deferred_send(&mut self) {
        if self.deferred_send.take().is_some() {
            let _ = self.start_stream();
        }
    }

    /// Drop all in-flight image work: staged attachments, a deferred send, and
    /// the describe channel. Called when the conversation context changes
    /// (new/switched session or space) so a resume can't fire into it.
    pub(crate) fn clear_image_state(&mut self) {
        self.pending_images.clear();
        self.deferred_send = None;
        self.describe_rx = None;
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
    async fn attach_saves_png_and_pushes_pending() {
        let mut a = crate::app::tests::app_with_key();
        let img = arboard::ImageData {
            width: 2,
            height: 1,
            bytes: std::borrow::Cow::Owned(vec![255, 0, 0, 255, 0, 0, 0, 0]),
        };
        a.attach_clipboard_image(img);
        assert_eq!(a.pending_images.len(), 1);
        assert!(a.pending_images[0].path.exists());
        assert!(a.status.contains("image attached"));
    }

    #[test]
    fn described_result_persists_description() {
        let mut a = crate::app::tests::app_with_key();
        // Seed one message with an image via the db layer.
        let s = a.db.create_session("t", "a/b", &a.active_space.id).unwrap();
        let mid = a.db.add_user_message(&s.id, "see").unwrap();
        let imgs =
            a.db.add_message_images(&mid, &["/tmp/x.png".into()])
                .unwrap();
        a.session = Some(s.clone());
        a.messages = a.db.load_messages(&s.id).unwrap();

        a.on_described(Some((
            imgs[0].id.clone(),
            Ok("a diagram of the login flow".into()),
        )));
        assert_eq!(
            a.messages[0].images[0].description.as_deref(),
            Some("a diagram of the login flow")
        );
        let reloaded = a.db.load_messages(&s.id).unwrap();
        assert_eq!(
            reloaded[0].images[0].description.as_deref(),
            Some("a diagram of the login flow")
        );
    }
}
