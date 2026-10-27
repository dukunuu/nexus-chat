//! The document's inline Markdown, shared by layout and file exporters.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::fmt::Write as _;

// Inline marks combine independently, including bold italic code links.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    pub footnote: Option<String>,
}

pub(crate) fn runs(source: &str) -> Vec<Run> {
    // Definitions live on Doc, outside the block. Register reference labels
    // with the parser, then omit those synthetic definitions from the output.
    let mut input = source.to_string();
    let mut labels = std::collections::BTreeSet::new();
    for tail in source.split("[^").skip(1) {
        if let Some((label, _)) = tail.split_once(']')
            && !label.is_empty()
            && !label.contains(char::is_whitespace)
        {
            labels.insert(label);
        }
    }
    for label in labels {
        let _ = write!(input, "\n\n[^{label}]: reference");
    }
    let mut out = Vec::new();
    let mut style = Run::default();
    let mut definition = false;
    for event in Parser::new_ext(
        &input,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES,
    ) {
        match event {
            Event::Start(Tag::FootnoteDefinition(_)) => {
                definition = true;
                continue;
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                definition = false;
                continue;
            }
            _ if definition => continue,
            _ => {}
        }
        match event {
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strikethrough) => style.strike = true,
            Event::End(TagEnd::Strikethrough) => style.strike = false,
            Event::Start(Tag::Link { dest_url, .. }) => style.link = Some(dest_url.into_string()),
            Event::End(TagEnd::Link) => style.link = None,
            Event::Text(text) => out.push(Run {
                text: text.into_string(),
                ..style.clone()
            }),
            Event::Code(text) => out.push(Run {
                text: text.into_string(),
                code: true,
                ..style.clone()
            }),
            Event::SoftBreak => out.push(Run {
                text: " ".into(),
                ..style.clone()
            }),
            Event::HardBreak => out.push(Run {
                text: "\n".into(),
                ..style.clone()
            }),
            Event::FootnoteReference(id) => out.push(Run {
                text: format!("[^{id}]"),
                footnote: Some(id.into_string()),
                ..style.clone()
            }),
            _ => {}
        }
    }
    out
}

pub(crate) fn escape(text: &str) -> String {
    crate::deck::text::escape(text)
}

/// Blend two `#rrggbb` colors; `amount` of `over` on top of `under`.
fn mix(under: &str, over: &str, amount: f64) -> Option<String> {
    let channels = |hex: &str| -> Option<[f64; 3]> {
        let hex = hex.strip_prefix('#')?;
        (hex.len() == 6).then_some(())?;
        let value = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f64::from);
        Some([value(0)?, value(2)?, value(4)?])
    };
    let (a, b) = (channels(under)?, channels(over)?);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let blend = |i: usize| (a[i] + (b[i] - a[i]) * amount).round().clamp(0.0, 255.0) as u8;
    Some(format!("#{:02x}{:02x}{:02x}", blend(0), blend(1), blend(2)))
}

/// The document's theme as a deck theme. The built-in design names only
/// bg/fg/accent/muted, so the `surface` and `border` roles that blocks use
/// are derived from them when a design does not define them.
pub(crate) fn theme(doc: &super::Doc) -> crate::deck::Theme {
    let mut colors = doc.theme.colors.clone();
    let (bg, fg) = (
        colors
            .get("bg")
            .cloned()
            .unwrap_or_else(|| "#ffffff".into()),
        colors
            .get("fg")
            .cloned()
            .unwrap_or_else(|| "#1d1d1f".into()),
    );
    for (role, amount) in [("surface", 0.06), ("border", 0.2)] {
        if let Some(derived) = mix(&bg, &fg, amount)
            && !colors.contains_key(role)
        {
            colors.insert(role.into(), derived);
        }
    }
    crate::deck::Theme {
        colors,
        fonts: doc.theme.fonts.clone(),
        font_files: doc.theme.font_files.clone(),
        text_styles: doc.theme.text_styles.clone(),
        ..Default::default()
    }
}

pub(crate) fn style(block: &super::Block) -> &str {
    block.style.as_deref().unwrap_or(match block.kind {
        super::Kind::Title { .. } => "title",
        super::Kind::Heading { level: 1, .. } => "h1",
        super::Kind::Heading { level: 2, .. } => "h2",
        super::Kind::Heading { .. } => "h3",
        super::Kind::Quote { .. } => "quote",
        super::Kind::Code { .. } => "code",
        _ => "body",
    })
}
