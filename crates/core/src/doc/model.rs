//! Version-one flowing documents. Typography uses the deck's `Font` data,
//! interpreted as points here. A separate theme avoids irrelevant slide size.
use crate::deck::{Align, Chart, Font, FontFile, Series};
use crate::design::Design;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Shared tokens, document styles and reading measure, without a slide size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub colors: BTreeMap<String, String>,
    pub fonts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_files: Vec<FontFile>,
    pub text_styles: BTreeMap<String, Font>,
    pub measure_ch: u16,
}
impl Theme {
    /// Copy every token so this document renders without the source design.
    #[must_use]
    pub fn from_design(design: &Design) -> Self {
        Self {
            colors: design.colors.clone(),
            fonts: design.fonts.clone(),
            font_files: design.font_files.clone(),
            text_styles: design.doc.text_styles.clone(),
            measure_ch: design.doc.measure_ch,
        }
    }
}
impl Default for Theme {
    fn default() -> Self {
        Self::from_design(&Design::default())
    }
}

/// Supported export paper sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Paper {
    #[default]
    A4,
    Letter,
}

/// Page geometry in points; margins are top, right, bottom, left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Page {
    pub size: Paper,
    pub margins_pt: [f64; 4],
    pub header: String,
    pub footer: String,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            size: Paper::A4,
            margins_pt: [72.0; 4],
            header: String::new(),
            footer: "{page}".into(),
        }
    }
}

/// Callout semantics, independent of color values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Info,
    Warn,
    Success,
    Note,
}

/// Image width relative to the reading column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageWidth {
    Inline,
    Wide,
    #[default]
    Full,
}

/// List items are addressed by one-based dotted paths (`2.1` is the first
/// child of the second item). Paths are local to their stable block id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListItem {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ListItem>,
}

/// Deck chart payload, grouped to avoid conflating it with block metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChartData {
    pub chart: Chart,
    pub labels: Vec<String>,
    pub series: Vec<Series>,
}

/// The document block language. Text is source Markdown, not HTML.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Title {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    Quote {
        text: String,
    },
    Callout {
        tone: Tone,
        text: String,
    },
    Code {
        #[serde(default)]
        lang: String,
        text: String,
    },
    List {
        #[serde(default)]
        ordered: bool,
        items: Vec<ListItem>,
    },
    Table {
        #[serde(default)]
        header: bool,
        #[serde(default)]
        align: Vec<Align>,
        rows: Vec<Vec<String>>,
    },
    Image {
        src: String,
        #[serde(default)]
        alt: String,
        #[serde(default)]
        caption: String,
        #[serde(default)]
        width: ImageWidth,
    },
    Chart {
        chart: ChartData,
    },
    Divider,
    PageBreak,
    Toc,
}

/// A stable, optionally styled block. No inline font or color overrides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(flatten)]
    pub kind: Kind,
}
impl Block {
    /// An unassigned block; validation assigns a stable id.
    #[must_use]
    pub fn new(kind: Kind) -> Self {
        Self {
            id: String::new(),
            style: None,
            kind,
        }
    }

    /// Exact editable source for text blocks (not a rendered/plain string).
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            Kind::Title { text }
            | Kind::Heading { text, .. }
            | Kind::Paragraph { text }
            | Kind::Quote { text }
            | Kind::Callout { text, .. }
            | Kind::Code { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Exact editable source, for the replace operation.
    pub(crate) fn text_mut(&mut self) -> Option<&mut String> {
        match &mut self.kind {
            Kind::Title { text }
            | Kind::Heading { text, .. }
            | Kind::Paragraph { text }
            | Kind::Quote { text }
            | Kind::Callout { text, .. }
            | Kind::Code { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Plain text for character-offset comment anchors.
    #[must_use]
    pub fn plain_text(&self) -> String {
        fn items(out: &mut Vec<String>, list: &[ListItem]) {
            for item in list {
                out.push(super::markdown::plain(&item.text));
                items(out, &item.children);
            }
        }
        if let Some(text) = self.text() {
            return if matches!(self.kind, Kind::Code { .. }) {
                text.into()
            } else {
                super::markdown::plain(text)
            };
        }
        match &self.kind {
            Kind::List { items: list, .. } => {
                let mut out = Vec::new();
                items(&mut out, list);
                out.join("\n")
            }
            Kind::Table { rows, .. } => rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|s| super::markdown::plain(s))
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Kind::Image { alt, caption, .. } => format!("{alt}\n{caption}"),
            _ => String::new(),
        }
    }
}

/// A complete, self-contained flowing document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Doc {
    pub version: u8,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_rev: Option<String>,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub page: Page,
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub footnotes: BTreeMap<String, String>,
}
