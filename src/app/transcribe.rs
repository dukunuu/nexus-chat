//! Clipboard-image transcription: encode the pasted image as a PNG data URL,
//! send it to the configured small vision model, and drop the transcript into
//! the composer — the main chat model never sees the image itself.

use anyhow::{Context, Result};
use base64::Engine;
use tokio::sync::mpsc;

use super::App;

/// Encode raw RGBA pixels as a `data:image/png;base64,…` URL.
pub(super) fn png_data_url(width: usize, height: usize, rgba: &[u8]) -> Result<String> {
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().context("png header")?;
        writer.write_image_data(rgba).context("png data")?;
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

impl App {
    /// Kick off transcription of a clipboard image (from Ctrl+V). Runs in the
    /// background; the transcript arrives as `AppEvent::Transcript`.
    pub fn transcribe_clipboard_image(&mut self, img: arboard::ImageData) {
        let Some(provider) = self.provider.clone() else {
            self.status = "set your API key first with /key".to_string();
            return;
        };
        let model = self.transcriber_model.trim().to_string();
        if model.is_empty() {
            self.status = "no transcriber model set — pick one in /config".to_string();
            return;
        }
        let url = match png_data_url(img.width, img.height, &img.bytes) {
            Ok(u) => u,
            Err(e) => {
                self.status = format!("could not encode image: {e}");
                return;
            }
        };
        self.status = format!("transcribing image ({}x{})…", img.width, img.height);
        let (tx, rx) = mpsc::unbounded_channel();
        self.transcript_rx = Some(rx);
        tokio::spawn(async move {
            let result = provider
                .transcribe_image(&model, &url)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// Insert a finished transcript at the composer cursor.
    pub fn on_transcript_result(&mut self, result: Option<std::result::Result<String, String>>) {
        self.transcript_rx = None;
        match result {
            Some(Ok(text)) if !text.trim().is_empty() => {
                self.input.insert_str(text.trim());
                self.status = "image transcribed".to_string();
            }
            Some(Ok(_)) => self.status = "transcriber returned nothing".to_string(),
            Some(Err(e)) => self.status = format!("transcription failed: {e}"),
            None => {}
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

    #[test]
    fn transcript_result_lands_in_composer() {
        let mut a = crate::app::tests::app_with_key();
        a.set_input("see: ");
        a.on_transcript_result(Some(Ok("hello from image".into())));
        assert_eq!(a.input_text(), "see: hello from image");
        a.on_transcript_result(Some(Err("model exploded".into())));
        assert!(a.status.contains("model exploded"));
    }
}
