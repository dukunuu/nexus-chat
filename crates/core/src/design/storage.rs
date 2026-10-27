//! Confined design storage under a space's files directory.
use super::Design;
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

/// A non-hidden slug with no traversal (including embedded `..`).
#[must_use]
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('.')
        && !slug.contains("..")
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Confine a relative path; check every component for symlinks, even dangling
/// ones. Callers supply only validated non-hidden components.
pub(crate) fn confined(files: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(!files.is_symlink(), "space files must not be a symlink");
    std::fs::create_dir_all(files)?;
    let mut path = files.canonicalize()?;
    for part in relative.split('/') {
        ensure!(
            valid_slug(part),
            "invalid path component; fix: use letters, digits, - _ . without .. or hidden names"
        );
        path.push(part);
        ensure!(!path.is_symlink(), "path must not contain symlinks");
    }
    Ok(path)
}

fn path(files: &Path, slug: &str) -> Result<PathBuf> {
    ensure!(
        valid_slug(slug),
        "invalid design slug; fix: use letters, digits, - _ . without .. or hidden names"
    );
    confined(files, &format!("design/{slug}.design.json"))
}

/// Sorted stored designs, plus the built-in fallback.
pub fn list(files: &Path) -> Result<Vec<String>> {
    let dir = confined(files, "design")?;
    let mut names = vec!["nexus".into()];
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            ensure!(
                !entry.path().is_symlink(),
                "design folder contains a symlink"
            );
            if let Some(name) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.strip_suffix(".design.json"))
                && valid_slug(name)
            {
                names.push(name.into());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Load a stored design, falling back to the built-in for `nexus` only.
pub fn load(files: &Path, slug: &str) -> Result<Design> {
    let path = path(files, slug)?;
    if slug == "nexus" && !path.exists() {
        return Ok(Design::default());
    }
    Design::check(&std::fs::read_to_string(path)?).map_err(|errors| {
        anyhow::anyhow!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })
}

/// Atomically persist a checked design under the supplied slug.
pub fn save(files: &Path, slug: &str, design: &Design) -> Result<()> {
    let path = path(files, slug)?;
    let checked = Design::check(&serde_json::to_string(design)?)
        .map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    crate::deck::write_atomic(&path, &serde_json::to_vec_pretty(&checked)?)
}

/// Current default, or `nexus` when the pointer does not exist.
pub fn default_slug(files: &Path) -> Result<String> {
    let path = confined(files, "design/default")?;
    if !path.exists() {
        return Ok("nexus".into());
    }
    let slug = std::fs::read_to_string(path)?.trim().to_string();
    ensure!(valid_slug(&slug), "invalid default design slug");
    Ok(slug)
}

/// Set the default only to a loadable design.
pub fn set_default(files: &Path, slug: &str) -> Result<()> {
    load(files, slug)?;
    crate::deck::write_atomic(
        &confined(files, "design/default")?,
        format!("{slug}\n").as_bytes(),
    )
}
