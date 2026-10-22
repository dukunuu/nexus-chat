//! User-requested font imports, confined to the opened deck's assets/fonts.

use super::{Backend, absolute, asset_root, atomic_write, checked, hash_bytes, read_bounded};
use anyhow::{Context as _, Result, ensure};
use nexus_core::deck::{FontFile, text};
use std::{fs, path::Path};

impl Backend {
    pub(super) fn import_font(&mut self, source: &Path) -> Result<()> {
        let source = absolute(source)?;
        let bytes = read_bounded(&source, text::MAX_FONT_FILE)?;
        self.import_font_bytes(
            source
                .file_name()
                .and_then(|s| s.to_str())
                .context("invalid font filename")?,
            &bytes,
        )
    }

    pub(super) fn import_font_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\']) && !matches!(name, "." | ".."),
            "expected a font filename, not a path"
        );
        ensure!(
            bytes.len() <= text::MAX_FONT_FILE,
            "font exceeds size limit"
        );
        let source = Path::new(name);
        let extension = source
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        ensure!(
            matches!(extension.as_str(), "ttf" | "otf" | "woff2"),
            "expected a ttf, otf or woff2 font"
        );
        let stem: String = source
            .file_stem()
            .context("missing font filename")?
            .to_string_lossy()
            .chars()
            .take(64)
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let metadata = text::imported_font_info(bytes);
        // WOFF2 is intentionally webview-only; fontdb cannot decompress it.
        ensure!(
            if extension == "woff2" {
                valid_woff2(bytes)
            } else {
                metadata.is_some()
                    && (bytes.starts_with(&[0, 1, 0, 0])
                        || bytes.starts_with(b"OTTO")
                        || bytes.starts_with(b"true"))
            },
            "invalid font file"
        );
        let (family, weight, italic) = metadata.unwrap_or_else(|| (stem.clone(), 400, false));
        let filename = format!("{stem}-{}.{}", &hash_bytes(bytes)[..16], extension);
        let relative = format!("assets/fonts/{filename}");
        let mut next = self.deck.clone();
        next.theme.font_files.retain(|f| f.file != relative);
        next.theme.font_files.push(FontFile {
            family,
            file: relative,
            weight,
            italic,
            fallback: vec![next.theme.family("body").into()],
        });
        let next = checked(&next)?;
        let root = asset_root(&self.path)?;
        let fonts = root.join("fonts");
        ensure!(!fonts.is_symlink(), "fonts directory must not be a symlink");
        fs::create_dir_all(&fonts)?;
        let fonts = fonts.canonicalize()?;
        ensure!(fonts.starts_with(&root), "fonts folder escapes assets");
        let destination = fonts.join(filename);
        ensure!(!destination.is_symlink(), "font must not be a symlink");
        atomic_write(&destination, bytes)?;
        self.replace(next)
    }
}

fn valid_woff2(bytes: &[u8]) -> bool {
    bytes.len() >= 48
        && bytes.starts_with(b"wOF2")
        && bytes.get(8..12).is_some_and(|length| {
            u32::from_be_bytes(length.try_into().expect("four bytes")) as usize == bytes.len()
        })
        && bytes[12..14] != [0, 0]
        && bytes[14..16] == [0, 0]
        && bytes.get(20..24).is_some_and(|size| {
            let compressed = u32::from_be_bytes(size.try_into().expect("four bytes")) as usize;
            compressed > 0 && compressed <= bytes.len() - 48
        })
}
