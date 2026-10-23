//! Space files as deck images. A model that has a photo or a generated image
//! in the space files can name it as an image `src` (`us_1.jpg`,
//! `files/us_1.jpg`, the relative path a tool printed); this module checks it
//! really is an image, copies it into the deck's `assets/` folder under a
//! content-addressed name and gives back the `assets/…` path to store.

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use image::ImageFormat;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::write_atomic;

/// Largest image file taken from the space files.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Longest side accepted, in pixels.
const MAX_SIDE: u32 = 16_384;

/// Call `f` on every image `src` an edit or a list of slides carries: image
/// nodes anywhere in the value, and the `src` of an `update_node` patch.
pub fn visit_srcs(value: &mut Value, f: &mut dyn FnMut(&mut String) -> Result<()>) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                visit_srcs(item, f)?;
            }
        }
        Value::Object(map) => {
            let is_image = map.get("kind").and_then(Value::as_str) == Some("image");
            let is_patch = map.get("op").and_then(Value::as_str) == Some("update_node");
            if is_image && let Some(Value::String(src)) = map.get_mut("src") {
                f(src)?;
            }
            if is_patch
                && let Some(Value::String(src)) =
                    map.get_mut("fields").and_then(|f| f.get_mut("src"))
            {
                f(src)?;
            }
            for inner in map.values_mut() {
                if inner.is_array() || inner.is_object() {
                    visit_srcs(inner, f)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn sanitized_stem(stem: &str) -> String {
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        "image".into()
    } else {
        clean
    }
}

fn extension(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("png"),
        ImageFormat::Jpeg => Some("jpg"),
        ImageFormat::WebP => Some("webp"),
        _ => None,
    }
}

/// A file in the space files named by `src`: relative to the space files, with
/// or without a leading `files/`, or an absolute path inside them. Symlinks
/// and `..` must not leave the space files.
#[must_use]
pub fn find_space_file(src: &str, space_files: &Path) -> Option<std::path::PathBuf> {
    let cleaned = src.trim().trim_start_matches("./");
    let root = space_files.canonicalize().ok()?;
    let candidates = [
        Some(cleaned),
        cleaned.strip_prefix("files/"),
        cleaned.strip_prefix(&format!("{}/", root.display())),
    ];
    for candidate in candidates.into_iter().flatten() {
        let joined = if Path::new(candidate).is_absolute() {
            Path::new(candidate).to_path_buf()
        } else {
            root.join(candidate)
        };
        if let Ok(real) = joined.canonicalize()
            && real.starts_with(&root)
            && real.is_file()
        {
            return Some(real);
        }
    }
    None
}

/// Resolve an image `src` to something a deck can store.
///
/// Returns `Ok(None)` when `src` should stay as written: an https URL, a path
/// already inside the deck's `assets/`, or a name that matches no file (the
/// deck keeps it and reports a placeholder). Returns `Ok(Some(path))` with the
/// `assets/…` path of the copy when a space file was imported.
///
/// # Errors
/// When the file exists but is not an acceptable image: not an image by
/// content, too large, or outside the space files.
pub fn import_space_image(src: &str, space_files: &Path, assets: &Path) -> Result<Option<String>> {
    if src.starts_with("https://") || src.starts_with("data:") || src.contains('\0') {
        return Ok(None);
    }
    let cleaned = src.trim().trim_start_matches("./");
    if let Some(inside) = cleaned.strip_prefix("assets/")
        && assets.join(inside).is_file()
    {
        return Ok(None);
    }
    let Some(file) = find_space_file(cleaned, space_files) else {
        return Ok(None);
    };
    if let Ok(assets_real) = assets.canonicalize()
        && let Ok(inside) = file.strip_prefix(&assets_real)
    {
        return Ok(Some(format!(
            "assets/{}",
            inside.to_string_lossy().replace('\\', "/")
        )));
    }
    let length = std::fs::metadata(&file)?.len();
    ensure!(
        length <= MAX_IMAGE_BYTES,
        "{src} is {} MB; images from the space files can be at most {} MB",
        length.div_ceil(1024 * 1024),
        MAX_IMAGE_BYTES / (1024 * 1024)
    );
    let bytes = std::fs::read(&file).with_context(|| format!("could not read {src}"))?;
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    store_image_bytes(&bytes, src, &stem, assets).map(Some)
}

/// Check that `bytes` are an acceptable image (PNG, JPEG or WebP by content,
/// at most [`MAX_IMAGE_BYTES`] and 16384 px on a side) and store them in the
/// deck's `assets/` folder under a content-addressed name made from `stem`.
/// Returns the `assets/…` path to store; the same bytes always give the same
/// path. `label` names the source in error messages.
///
/// # Errors
/// When the bytes are not an acceptable image, or cannot be written.
pub fn store_image_bytes(bytes: &[u8], label: &str, stem: &str, assets: &Path) -> Result<String> {
    ensure!(
        bytes.len() as u64 <= MAX_IMAGE_BYTES,
        "{label} is {} MB; images can be at most {} MB",
        (bytes.len() as u64).div_ceil(1024 * 1024),
        MAX_IMAGE_BYTES / (1024 * 1024)
    );
    let format = image::guess_format(bytes).ok();
    let Some(extension) = format.and_then(extension) else {
        bail!("{label} is not a PNG, JPEG or WebP image (checked by content, not by name)");
    };
    let (w, h) = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()
        .with_context(|| format!("{label} is not a readable image"))?;
    ensure!(
        w.max(h) <= MAX_SIDE && w > 0 && h > 0,
        "{label} is {w}x{h} px; at most {MAX_SIDE} px on a side"
    );
    let digest = Sha256::digest(bytes);
    let sha8 = digest.iter().take(4).fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    let name = format!("{sha8}-{}.{extension}", sanitized_stem(stem));
    ensure!(!assets.is_symlink(), "assets folder must not be a symlink");
    std::fs::create_dir_all(assets)?;
    let target = assets.join(&name);
    if !target.is_file() {
        write_atomic(&target, bytes)?;
    }
    Ok(format!("assets/{name}"))
}

#[cfg(test)]
mod tests;
