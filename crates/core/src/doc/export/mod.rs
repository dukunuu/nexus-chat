//! Offline document exports. Names and exclusive writes match deck exports.
pub mod docx;
pub mod html;
pub mod pdf;
use super::{Doc, feedback::Thread};
use anyhow::{Result, bail};
use std::path::Path;

/// Document output formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Docx,
    Pdf,
    Md,
    Html,
}
impl std::str::FromStr for Format {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "docx" => Ok(Self::Docx),
            "pdf" => Ok(Self::Pdf),
            "md" => Ok(Self::Md),
            "html" => Ok(Self::Html),
            _ => bail!("format must be docx, pdf, md or html"),
        }
    }
}
impl Format {
    /// Filename suffix.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Pdf => "pdf",
            Self::Md => "md",
            Self::Html => "html",
        }
    }
}
/// Render a checked document; callers use `deck::export::write_export` to save it.
pub fn export(
    doc: &Doc,
    assets: Option<&Path>,
    threads: &[Thread],
    format: Format,
) -> Result<crate::deck::export::Export> {
    let mut warnings = Vec::new();
    let bytes = match format {
        Format::Docx => docx::export(doc, assets, threads, &mut warnings)?,
        Format::Html => html::export(doc, assets, &mut warnings)?.into_bytes(),
        Format::Md => super::markdown::export(doc).into_bytes(),
        Format::Pdf => pdf::export(doc, assets)?,
    };
    Ok(crate::deck::export::Export {
        bytes,
        file_name: format!("{}.{}", slug(&doc.title), format.extension()),
        warnings,
    })
}
pub(crate) fn slug(title: &str) -> String {
    let slug = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "document".into()
    } else {
        slug.chars().take(100).collect()
    }
}
