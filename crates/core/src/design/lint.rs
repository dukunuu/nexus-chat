//! Non-blocking consistency hints, independent of rendering.
use super::Design;
use crate::deck::{Deck, Font, Kind};
use std::collections::BTreeSet;

/// A specific off-system override, addressed by slide/node id.
#[derive(Clone, Debug, PartialEq)]
pub enum Drift {
    Color { id: String, value: String },
    Size { id: String, px: f64 },
    Family { id: String, value: String },
}

/// Find explicit hex colors, sizes and families outside the design. Color
/// spellings normalize short hex, case and opaque alpha before comparison.
#[must_use]
pub fn lint_deck(deck: &Deck, design: &Design) -> Vec<Drift> {
    let mut out = Vec::new();
    for slide in &deck.slides {
        if let Some(fill) = &slide.fill {
            color(&mut out, &slide.id, fill, design);
        }
        for node in slide.nodes() {
            match &node.kind {
                Kind::Text { font, style, .. }
                | Kind::Table { font, style, .. }
                | Kind::Code { font, style, .. } => {
                    let inherited = deck
                        .theme
                        .text_styles
                        .get(style)
                        .cloned()
                        .unwrap_or_default()
                        .merged(font);
                    font_drift(&mut out, &node.id, &inherited, design);
                    if let Some(size) = inherited.size
                        && !design
                            .slide
                            .text_styles
                            .values()
                            .any(|f| f.size.is_some_and(|n| (n - size).abs() < 0.01))
                    {
                        out.push(Drift::Size {
                            id: node.id.clone(),
                            px: size,
                        });
                    }
                }
                Kind::Shape { fill, stroke, .. } | Kind::Frame { fill, stroke, .. } => {
                    if let Some(fill) = fill {
                        color(&mut out, &node.id, fill, design);
                    }
                    if let Some(stroke) = stroke {
                        color(&mut out, &node.id, &stroke.color, design);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn normalized(value: &str) -> String {
    let raw = value.trim_start_matches('#').to_lowercase();
    let raw = if matches!(raw.len(), 3 | 4) {
        raw.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        raw
    };
    raw.strip_suffix("ff")
        .filter(|_| raw.len() == 8)
        .unwrap_or(&raw)
        .into()
}
fn color(out: &mut Vec<Drift>, id: &str, value: &str, design: &Design) {
    if value.starts_with('#')
        && !design
            .colors
            .values()
            .any(|c| normalized(c) == normalized(value))
    {
        out.push(Drift::Color {
            id: id.into(),
            value: value.into(),
        });
    }
}
fn font_drift(out: &mut Vec<Drift>, id: &str, font: &Font, design: &Design) {
    if let Some(value) = &font.color {
        color(out, id, value, design);
    }
    if let Some(value) = &font.family
        && !design.fonts.contains_key(value)
        && !design.fonts.values().any(|family| family == value)
        && !design.font_files.iter().any(|file| file.family == *value)
    {
        out.push(Drift::Family {
            id: id.into(),
            value: value.clone(),
        });
    }
}

/// Doc blocks have no inline font/color overrides yet, only named styles.
#[must_use]
pub fn lint_doc(_doc: &crate::doc::Doc, _design: &Design) -> Vec<Drift> {
    Vec::new()
}

/// One compact result line. Count unique nodes, list up to six ids per kind,
/// then `+N`; the complete line never exceeds 200 characters.
#[must_use]
pub fn summary(drift: &[Drift]) -> Option<String> {
    let mut groups: [BTreeSet<String>; 3] = std::array::from_fn(|_| BTreeSet::new());
    for item in drift {
        let (kind, label) = match item {
            Drift::Color { id, .. } => (0, id.clone()),
            Drift::Size { id, px } => (1, format!("{id} {px}px")),
            Drift::Family { id, .. } => (2, id.clone()),
        };
        groups[kind].insert(label);
    }
    let mut limits = [6; 3];
    loop {
        let mut parts = Vec::new();
        for (index, (ids, label)) in groups
            .iter()
            .zip([
                "nodes use hex colors outside the design",
                "font sizes are not text styles",
                "nodes use families outside the design",
            ])
            .enumerate()
        {
            if ids.is_empty() {
                continue;
            }
            let mut names = ids.iter().take(limits[index]).cloned().collect::<Vec<_>>();
            if ids.len() > names.len() {
                names.push(format!("+{}", ids.len() - names.len()));
            }
            parts.push(format!("{} {label} ({})", ids.len(), names.join(", ")));
        }
        if parts.is_empty() {
            return None;
        }
        let line = format!("design: {}", parts.join("; "));
        if line.chars().count() <= 200 {
            return Some(line);
        }
        let index = groups
            .iter()
            .enumerate()
            .filter(|(i, ids)| limits[*i] > 0 && !ids.is_empty())
            .max_by_key(|(i, ids)| ids.iter().take(limits[*i]).map(String::len).sum::<usize>())
            .map(|(i, _)| i)
            .expect("a non-empty id list can be shortened");
        limits[index] -= 1;
    }
}
