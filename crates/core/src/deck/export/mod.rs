//! Offline, deterministic deck exports. Assets are read only from the deck's
//! assets directory or its existing remote-image cache; nothing is fetched.

pub mod html;
pub mod images;
pub mod pdf;
pub mod pptx;

use std::io::{Cursor, Write as _};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;

use super::text::{Row, classify, inline};
use super::{Deck, Kind, Transform};

/// Available file formats (`png` is a zip of slide images).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pptx,
    Pdf,
    Png,
    Html,
    Json,
}

impl std::str::FromStr for Format {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pptx" => Ok(Self::Pptx),
            "pdf" => Ok(Self::Pdf),
            "png" => Ok(Self::Png),
            "html" => Ok(Self::Html),
            "json" => Ok(Self::Json),
            _ => bail!("format must be pptx, pdf, png, html or json"),
        }
    }
}

impl Format {
    /// File suffix, including the compound deck JSON suffix.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Pptx => "pptx",
            Self::Pdf => "pdf",
            Self::Png => "zip",
            Self::Html => "html",
            Self::Json => "deck.json",
        }
    }
}

/// Raster export controls. Contact sheets are single PNGs rather than zips.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub width: u32,
    pub contact_sheet: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 1920,
            contact_sheet: false,
        }
    }
}

/// A complete file and any user-visible approximations made while creating it.
#[derive(Debug)]
pub struct Export {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub warnings: Vec<String>,
}

/// Convert a checked deck to one self-contained file, without filesystem writes.
pub fn export(
    deck: &Deck,
    assets: Option<&Path>,
    format: Format,
    options: Options,
) -> Result<Export> {
    let mut warnings = Vec::new();
    let bytes = match format {
        Format::Pptx => pptx::with_warnings(deck, assets, &mut warnings)?,
        Format::Pdf => {
            warnings.push("PDF pages are raster images with an invisible Helvetica text layer, not vector artwork.".into());
            pdf::export_pdf(deck, assets)?
        }
        Format::Png if options.contact_sheet => images::contact_sheet(deck, assets)?,
        Format::Png => images::export_png(deck, assets, options.width)?,
        Format::Html => html::with_warnings(deck, assets, &mut warnings)?.into_bytes(),
        Format::Json => {
            warnings.push(
                "Deck JSON references assets; keep the assets folder beside the file.".into(),
            );
            serde_json::to_vec_pretty(deck)?
        }
    };
    if matches!(format, Format::Pdf | Format::Png) {
        for node in deck.slides.iter().flat_map(super::Slide::nodes) {
            if let Kind::Image { src, .. } = &node.kind
                && super::layout::image_path(src, assets).is_none()
            {
                warnings.push(format!(
                    "{}: image unavailable; exported a placeholder ({src}).",
                    node.id
                ));
            }
        }
        for font in &deck.theme.font_files {
            if !super::text::loadable_font(&font.file) {
                warnings.push(format!(
                    "{}: core raster rendering uses fallback fonts for {}.",
                    font.family, font.file
                ));
            }
        }
    }
    let ext = if format == Format::Png && options.contact_sheet {
        "png"
    } else {
        format.extension()
    };
    Ok(Export {
        bytes,
        file_name: format!("{}.{ext}", slug(&deck.title)),
        warnings,
    })
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "deck".into()
    } else {
        out.chars().take(100).collect()
    }
}

/// Write an export under `<files>/exports`, or to an explicitly supplied absolute
/// path. Existing names get ` (2)`, ` (3)`, …; the default exports directory
/// cannot be a symlink. Explicit user-selected parent directories are trusted.
pub fn write_export(output: &Export, files: &Path, path: Option<&Path>) -> Result<PathBuf> {
    let requested = if let Some(path) = path {
        ensure!(
            path.is_absolute(),
            "export destination must be an absolute path you entered"
        );
        path.to_path_buf()
    } else {
        std::fs::create_dir_all(files)?;
        let directory = files.canonicalize()?.join("exports");
        ensure!(
            !directory.is_symlink(),
            "exports directory must not be a symlink"
        );
        directory.join(&output.file_name)
    };
    ensure!(
        !requested
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "parent path components are not allowed"
    );
    ensure!(!requested.is_symlink(), "export file must not be a symlink");
    let parent = requested.parent().context("missing export folder")?;
    std::fs::create_dir_all(parent)?;
    let requested = parent.canonicalize()?.join(
        requested
            .file_name()
            .context("expected an export filename")?,
    );
    let parent = requested.parent().expect("canonical export parent");
    let name = requested
        .file_name()
        .context("expected an export filename")?
        .to_string_lossy();
    let (stem, ext) = name.strip_suffix(".deck.json").map_or_else(
        || name.rsplit_once('.').unwrap_or((&name, "")),
        |stem| (stem, "deck.json"),
    );
    for index in 1.. {
        let candidate = if index == 1 {
            requested.clone()
        } else {
            parent.join(format!(
                "{stem} ({index}){}",
                if ext.is_empty() {
                    String::new()
                } else {
                    format!(".{ext}")
                }
            ))
        };
        // Reserve the name exclusively before the existing atomic-write helper
        // replaces our empty reservation. Never overwrite another user's file.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                if let Err(error) = super::write_atomic(&candidate, &output.bytes) {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("unbounded filename sequence")
}

pub(super) fn zip(parts: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    for (name, bytes) in parts {
        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

pub(super) fn transform(text: &str, case: Transform) -> String {
    match case {
        Transform::None => text.into(),
        Transform::Uppercase => text.to_uppercase(),
        Transform::Lowercase => text.to_lowercase(),
        Transform::Capitalize => text
            .split_inclusive(char::is_whitespace)
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |c| {
                    c.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect(),
    }
}

pub(super) fn visible_nodes(slide: &super::Slide) -> Vec<&super::Node> {
    fn walk<'a>(nodes: &'a [super::Node], out: &mut Vec<&'a super::Node>) {
        for node in nodes.iter().filter(|n| !n.hidden) {
            out.push(node);
            walk(node.children(), out);
        }
    }
    let mut out = Vec::new();
    walk(&slide.children, &mut out);
    out
}

pub(super) fn plain(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let text = match classify(line) {
                Row::Heading(_, s) | Row::Bullet(s) | Row::Numbered(s) | Row::Paragraph(s) => s,
            };
            inline(text).into_iter().map(|r| r.text).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
