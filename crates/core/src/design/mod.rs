//! Shared, self-contained design tokens and medium-specific typography.
pub mod layouts;
pub mod lint;
pub mod storage;
mod validate;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::deck::{Font, FontFile, Slide, SlideSize, Theme};
use crate::doc::model::Page;

pub use lint::{Drift, lint_deck, summary};
pub use storage::{default_slug, list, load, save, set_default};

/// A modular type scale in document points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scale {
    pub base_pt: f64,
    pub ratio: f64,
}
impl Default for Scale {
    fn default() -> Self {
        Self {
            base_pt: 11.0,
            ratio: 1.25,
        }
    }
}

/// Origin of an imported design.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub template: String,
    pub imported_at: String,
}

/// Slide-space rhythm, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Spacing {
    pub margin: f64,
    pub gap: f64,
}
impl Default for Spacing {
    fn default() -> Self {
        Self {
            margin: 120.0,
            gap: 32.0,
        }
    }
}

/// Pixel typography and named frame trees.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlideDesign {
    pub size: SlideSize,
    pub text_styles: BTreeMap<String, Font>,
    pub spacing: Spacing,
    pub layouts: BTreeMap<String, Slide>,
}

/// Point typography and export page setup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocDesign {
    pub text_styles: BTreeMap<String, Font>,
    pub page: Page,
    pub measure_ch: u16,
}
impl Default for DocDesign {
    fn default() -> Self {
        Self {
            text_styles: BTreeMap::new(),
            page: Page::default(),
            measure_ch: 72,
        }
    }
}

/// Version-one design. Layouts reuse the deck's slide and node types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Design {
    pub version: u8,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub fonts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_files: Vec<FontFile>,
    #[serde(default)]
    pub scale: Scale,
    #[serde(default)]
    pub slide: SlideDesign,
    #[serde(default)]
    pub doc: DocDesign,
}

impl Default for Design {
    fn default() -> Self {
        let theme = Theme::default();
        let (_, doc) = derive_styles(Scale::default(), &theme.fonts);
        Self {
            version: 1,
            name: "nexus".into(),
            source: None,
            colors: theme.colors,
            fonts: theme.fonts,
            font_files: theme.font_files,
            scale: Scale::default(),
            slide: SlideDesign {
                size: theme.size,
                text_styles: theme.text_styles,
                spacing: Spacing::default(),
                layouts: layouts::builtin(Spacing::default()),
            },
            doc: DocDesign {
                text_styles: doc,
                ..DocDesign::default()
            },
        }
    }
}

impl Design {
    /// Canonical JSON SHA-256, including the layouts.
    #[must_use]
    pub fn revision(&self) -> String {
        crate::deck::hash(self)
    }

    /// A filename-safe default slug derived from the display name.
    #[must_use]
    pub fn slug(&self) -> String {
        let name: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let name = name.trim_matches('-');
        if name.is_empty() {
            "design".into()
        } else {
            name.into()
        }
    }

    /// Full slide theme; decks never need the design to render.
    #[must_use]
    pub fn slide_theme(&self) -> Theme {
        Theme {
            colors: self.colors.clone(),
            fonts: self.fonts.clone(),
            font_files: self.font_files.clone(),
            size: self.slide.size,
            text_styles: self.slide.text_styles.clone(),
        }
    }
}

/// Derive both media from one modular scale. Document roles use exponents
/// display=9, title=4, h1/quote=3, h2/lead=2, h3=1, body=0, caption/code=-1. Slide sizes are
/// those point sizes times 42/11 (at 1920 px wide), so an 11 pt body is 42 px.
/// Families stay role references, so changing a role remaps both media. `fonts`
/// supplies the available roles; absent roles fall back to body.
#[must_use]
pub fn derive_styles(
    scale: Scale,
    fonts: &BTreeMap<String, String>,
) -> (BTreeMap<String, Font>, BTreeMap<String, Font>) {
    let mut doc: BTreeMap<String, Font> = BTreeMap::new();
    for (name, exponent, role, weight) in [
        ("display", 9, "heading", 800),
        ("title", 4, "heading", 700),
        ("h1", 3, "heading", 700),
        ("h2", 2, "heading", 600),
        ("h3", 1, "heading", 600),
        ("body", 0, "body", 400),
        ("quote", 3, "heading", 500),
        ("lead", 2, "body", 400),
        ("caption", -1, "body", 400),
        ("code", -1, "mono", 400),
    ] {
        doc.insert(
            name.into(),
            Font {
                family: Some(
                    if fonts.contains_key(role) {
                        role
                    } else {
                        "body"
                    }
                    .into(),
                ),
                size: Some(crate::deck::model::round2(
                    scale.base_pt * scale.ratio.powi(exponent),
                )),
                weight: Some(weight),
                line_height: Some(if name == "display" {
                    1.0
                } else if exponent > 0 {
                    1.15
                } else {
                    1.4
                }),
                color: Some(
                    match name {
                        "caption" => "muted",
                        "display" => "accent",
                        _ => "fg",
                    }
                    .into(),
                ),
                italic: Some(false),
                ..Font::default()
            },
        );
    }
    let mut slide = BTreeMap::new();
    for (name, font) in &doc {
        let mut font = font.clone();
        font.size = font
            .size
            .map(|size| crate::deck::model::round2(size * 42.0 / 11.0));
        let name = match name.as_str() {
            "h1" => "heading",
            "h2" => "subheading",
            other => other,
        };
        slide.insert(name.into(), font);
    }
    (slide, doc)
}

/// Caller-owned lookup keeps operations independent of the filesystem.
pub type Resolver<'a> = dyn Fn(&str) -> anyhow::Result<Design> + 'a;

/// Resolve a design, using the built-in only for the explicit fallback name.
pub(crate) fn resolve(slug: &str, resolver: Option<&Resolver<'_>>) -> anyhow::Result<Design> {
    if let Some(resolver) = resolver {
        resolver(slug)
    } else if slug == "nexus" {
        Ok(Design::default())
    } else {
        anyhow::bail!("unknown design {slug:?}; fix: supply a design resolver or use nexus")
    }
}

#[cfg(test)]
mod tests;
