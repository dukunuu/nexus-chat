//! Confined `docs/…/*.doc.json` files and content-addressed assets.
use super::Doc;
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

/// Resolve a non-hidden doc path without traversing symlinks.
pub fn confined(files: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(
        relative.starts_with("docs/") && relative.ends_with(".doc.json"),
        "path must look like docs/name.doc.json"
    );
    crate::design::storage::confined(files, relative)
}

/// Load and validate a document inside this space.
pub fn load(files: &Path, relative: &str) -> Result<Doc> {
    let path = confined(files, relative)?;
    Doc::check(&std::fs::read_to_string(path)?).map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// Validate before any write, preserve history, and re-anchor the existing
/// sidecar after saving. Failed operations never call this function.
pub fn save(files: &Path, relative: &str, doc: &Doc, who: crate::deck::history::Who) -> Result<()> {
    let path = confined(files, relative)?;
    let doc = Doc::check(&serde_json::to_string(doc)?).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut threads = super::feedback::load(&path)?;
    let resolver = |slug: &str| crate::design::load(files, slug);
    super::feedback::reanchor_with_resolver(&mut threads, &doc, Some(&resolver));
    crate::deck::history::record_before_write_for::<Doc>(&path, &doc.revision(), who)?;
    crate::deck::write_atomic(&path, &serde_json::to_vec_pretty(&doc)?)?;
    if !threads.is_empty() {
        super::feedback::save(&path, &threads)?;
    }
    Ok(())
}

/// Copy an image into `docs/assets/` using the deck's content-addressed store.
pub fn store_image(files: &Path, bytes: &[u8], label: &str, stem: &str) -> Result<String> {
    let assets = crate::design::storage::confined(files, "docs/assets")?;
    crate::deck::assets::store_image_bytes(bytes, label, stem, &assets)
}
