//! Uploaded Office templates: bounded extraction and export into their own OPC packages.
// OPC part names are case sensitive, unlike user-selected file extensions.
#![allow(clippy::case_sensitive_file_extension_comparisons)]
mod package;
mod powerpoint;
pub(crate) use powerpoint::placeholder_xml;
mod word;
mod word_export;

use super::{Design, Source, storage::confined};
use anyhow::{Context as _, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// A stored template and its extracted design, with every approximation.
#[derive(Debug)]
pub struct Imported {
    pub slug: String,
    pub design: Design,
    pub warnings: Vec<String>,
}

/// Recognized safe template suffix, refusing macro-enabled Office files.
pub fn extension(name: &Path) -> Result<String> {
    let ext = name
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    ensure!(
        !matches!(ext.as_str(), "potm" | "pptm" | "dotm" | "docm"),
        "macro templates are not supported"
    );
    ensure!(
        matches!(ext.as_str(), "potx" | "pptx" | "dotx" | "docx"),
        "template must be .pptx, .potx, .docx or .dotx"
    );
    Ok(ext)
}

/// Import a user-selected local file; callers handling model paths confine those first.
pub fn import_file(files: &Path, source: &Path, name: Option<&str>) -> Result<Imported> {
    extension(source)?;
    ensure!(
        std::fs::metadata(source)?.len() <= crate::deck::import::MAX_PPTX_BYTES,
        "template exceeds 250 MB"
    );
    import_bytes(files, source, &std::fs::read(source)?, name)
}

/// Import chooser bytes, storing the original safe package and refreshing its design.
pub fn import_bytes(
    files: &Path,
    source: &Path,
    bytes: &[u8],
    name: Option<&str>,
) -> Result<Imported> {
    let ext = extension(source)?;
    package::open(bytes)?;
    let name = name.unwrap_or_else(|| {
        source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Template")
    });
    let slug = Design {
        name: name.into(),
        ..Design::default()
    }
    .slug();
    let assets = confined(files, &format!("design/assets/{slug}"))?;
    // Existing child symlinks are refused by the image store too.
    let (mut design, mut warnings) = if matches!(ext.as_str(), "pptx" | "potx") {
        crate::deck::import::pptx::extract_design(bytes, &assets)?
    } else {
        word::extract(bytes)?
    };
    design.name = name.chars().take(120).collect();
    let relative = format!("templates/{slug}.{ext}");
    design.source = Some(Source {
        template: relative.clone(),
        imported_at: chrono::Utc::now().to_rfc3339(),
        sha256: Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            }),
    });
    // Asset paths are relative to design/assets, and can be materialized beside
    // any deck without losing the original content-addressed design artwork.
    for layout in design.slide.layouts.values_mut() {
        fn paths(nodes: &mut [crate::deck::Node], slug: &str) {
            for node in nodes {
                if let crate::deck::Kind::Image { src, .. } = &mut node.kind {
                    *src = format!(
                        "assets/{slug}/{}",
                        src.strip_prefix("assets/").unwrap_or(src)
                    );
                }
                if let Some(children) = node.children_mut() {
                    paths(children, slug);
                }
            }
        }
        paths(&mut layout.children, &slug);
    }
    derive_surfaces(&mut design);
    let design = Design::check(&serde_json::to_string(&design)?)
        .map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    crate::deck::write_atomic(&confined(files, &relative)?, bytes)?;
    super::save(files, &slug, &design)?;
    warnings.sort();
    warnings.dedup();
    Ok(Imported {
        slug,
        design,
        warnings,
    })
}

fn derive_surfaces(design: &mut Design) {
    for (role, foreground) in [("surface", 0.04), ("border", 0.2)] {
        let channel = |key: &str, offset| {
            u8::from_str_radix(&design.colors[key][offset..offset + 2], 16).unwrap_or(0)
        };
        let mut color = String::from("#");
        for offset in [1, 3, 5] {
            let bg = f64::from(channel("bg", offset));
            let fg = f64::from(channel("fg", offset));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let value = (bg * (1.0 - foreground) + fg * foreground).round() as u8;
            let _ = write!(color, "{value:02x}");
        }
        design.colors.insert(role.into(), color);
    }
}

/// Copy imported artwork to a document's assets folder, keeping safe relative paths.
pub fn materialize(files: &Path, design: &Design, assets: &Path) -> Result<()> {
    let Some(source) = &design.source else {
        return Ok(());
    };
    if !source.template.ends_with("pptx") && !source.template.ends_with("potx") {
        return Ok(());
    }
    let slug = Path::new(&source.template)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("template slug")?;
    let source = confined(files, &format!("design/assets/{slug}"))?;
    if !source.exists() {
        return Ok(());
    }
    ensure!(!assets.is_symlink(), "assets must not be a symlink");
    let target = assets.join(slug);
    ensure!(
        !target.is_symlink(),
        "template assets must not be a symlink"
    );
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        ensure!(
            !entry.path().is_symlink(),
            "template image must not be a symlink"
        );
        let destination = target.join(entry.file_name());
        ensure!(
            !destination.is_symlink(),
            "template image must not be a symlink"
        );
        std::fs::copy(entry.path(), destination)?;
    }
    Ok(())
}

/// Render the layout gallery through the ordinary deck renderer (960 px per slide).
pub fn contact_sheet(files: &Path, imported: &Imported) -> Result<Option<PathBuf>> {
    if imported
        .design
        .slide
        .layouts
        .values()
        .all(|l| l.source_layout.is_none())
    {
        return Ok(None);
    }
    let deck = crate::deck::Deck::check(&serde_json::json!({"version":2,"title":imported.design.name,"theme":imported.design.slide_theme(),"slides":imported.design.slide.layouts.values().collect::<Vec<_>>()}).to_string()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let bytes =
        crate::deck::export::images::contact_sheet(&deck, Some(&files.join("design/assets")))?;
    let path = confined(files, &format!("design/{}-layouts.png", imported.slug))?;
    crate::deck::write_atomic(&path, &bytes)?;
    Ok(Some(path))
}

fn source_bytes(
    files: &Path,
    slug: Option<&str>,
    selection: Option<&str>,
    kind: &str,
) -> Result<Option<Vec<u8>>> {
    ensure!(
        selection.is_none_or(|s| s == "none"),
        "template must be none, or omitted to use the design's template"
    );
    if selection == Some("none") {
        return Ok(None);
    }
    let Some(slug) = slug else {
        return Ok(None);
    };
    let design = super::load(files, slug)?;
    let Some(source) = design.source else {
        return Ok(None);
    };
    let path = confined(files, &source.template)?;
    let ext = extension(&path)?;
    if (kind == "pptx" && !matches!(ext.as_str(), "pptx" | "potx"))
        || (kind == "docx" && !matches!(ext.as_str(), "docx" | "dotx"))
    {
        return Ok(None);
    }
    ensure!(
        std::fs::metadata(&path)?.len() <= crate::deck::import::MAX_PPTX_BYTES,
        "template exceeds 250 MB"
    );
    Ok(Some(std::fs::read(path)?))
}

/// Export a deck, reusing its source package unless `selection` is `none`.
pub fn export_deck(
    deck: &crate::deck::Deck,
    assets: Option<&Path>,
    files: &Path,
    format: crate::deck::export::Format,
    selection: Option<&str>,
) -> Result<crate::deck::export::Export> {
    let template = if format == crate::deck::export::Format::Pptx {
        source_bytes(files, deck.design.as_deref(), selection, "pptx")?
    } else {
        None
    };
    let mut plain = deck.clone();
    if template.is_some() {
        fn prune(nodes: &mut Vec<crate::deck::Node>) {
            nodes.retain(|n| !n.template_background);
            for node in nodes {
                if let Some(children) = node.children_mut() {
                    prune(children);
                }
            }
        }
        fn identify(nodes: &mut [crate::deck::Node]) {
            for node in nodes {
                if node.source_placeholder.is_some() {
                    node.name = Some(node.id.clone());
                }
                if let Some(children) = node.children_mut() {
                    identify(children);
                }
            }
        }
        for slide in &mut plain.slides {
            if slide.source_layout.is_some() {
                prune(&mut slide.children);
                identify(&mut slide.children);
            }
        }
    }
    let mut output = crate::deck::export::export(
        &plain,
        assets,
        format,
        crate::deck::export::Options::default(),
    )?;
    if let Some(template) = template {
        output.bytes = powerpoint::merge(&template, &output.bytes, &plain, &mut output.warnings)?;
    }
    Ok(output)
}

/// Export a doc, resolving Word styles by built-in name in its source package.
pub fn export_doc(
    doc: &crate::doc::Doc,
    assets: Option<&Path>,
    threads: &[crate::doc::feedback::Thread],
    files: &Path,
    format: crate::doc::export::Format,
    selection: Option<&str>,
) -> Result<crate::deck::export::Export> {
    let mut output = crate::doc::export::export(doc, assets, threads, format)?;
    if format == crate::doc::export::Format::Docx
        && let Some(template) = source_bytes(files, doc.design.as_deref(), selection, "docx")?
    {
        output.bytes = word_export::merge(&template, &output.bytes)?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests;
