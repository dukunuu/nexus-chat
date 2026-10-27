//! One-line descriptions of fonts and nodes, shared by the `canvas` tool and the
//! terminal's comment messages, so the model sees the same compact shapes
//! everywhere.

use std::fmt::Write as _;

use super::{Deck, Font, Kind, Rect};

/// A px length without a pointless `.0`.
#[must_use]
pub fn px(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// Only the fields a font overrides: `size=58 color=accent`.
#[must_use]
pub fn font_line(font: &Font) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &font.family {
        parts.push(format!("family={v}"));
    }
    if let Some(v) = font.size {
        parts.push(format!("size={}", px(v)));
    }
    if let Some(v) = font.weight {
        parts.push(format!("weight={v}"));
    }
    if font.italic == Some(true) {
        parts.push("italic".into());
    }
    if let Some(v) = font.line_height {
        parts.push(format!("line_height={v}"));
    }
    if let Some(v) = font.letter_spacing {
        parts.push(format!("letter_spacing={v}"));
    }
    if let Some(v) = &font.color {
        parts.push(format!("color={v}"));
    }
    if let Some(v) = font.align {
        parts.push(format!("align={}", format!("{v:?}").to_lowercase()));
    }
    if let Some(v) = font.valign {
        parts.push(format!("valign={}", format!("{v:?}").to_lowercase()));
    }
    if let Some(v) = font.transform {
        parts.push(format!("transform={}", format!("{v:?}").to_lowercase()));
    }
    parts.join(" ")
}

/// The node a comment is anchored to, on one line: id, kind, rectangle, text
/// style and font overrides, and its text (at most `text_chars` characters).
#[must_use]
pub fn node_brief(
    deck: &Deck,
    slide_id: &str,
    node_id: &str,
    assets: Option<&std::path::Path>,
    text_chars: usize,
) -> Option<String> {
    let slide = deck.slide(slide_id)?;
    let node = slide.node(node_id)?;
    let rect: Option<Rect> = super::layout_map(deck, slide, assets).remove(node_id);
    let mut out = format!("{} {}", node.id, node.kind.name());
    if let Some(r) = rect {
        let _ = write!(out, " {},{} {}x{}", px(r.x), px(r.y), px(r.w), px(r.h));
    }
    let clipped = |text: &str| -> String {
        let flat = text.replace('\n', " / ");
        if flat.chars().count() > text_chars {
            format!("{}…", flat.chars().take(text_chars).collect::<String>())
        } else {
            flat
        }
    };
    match &node.kind {
        Kind::Text { text, style, font } => {
            let _ = write!(out, " style {style}");
            let overrides = font_line(font);
            if !overrides.is_empty() {
                let _ = write!(out, " {overrides}");
            }
            let _ = write!(out, " : {:?}", clipped(text));
        }
        Kind::Code {
            source,
            style,
            font,
            ..
        } => {
            let _ = write!(
                out,
                " style {style} {} : {:?}",
                font_line(font),
                clipped(source)
            );
        }
        Kind::Image { src, .. } => {
            let _ = write!(out, " {src}");
        }
        _ => {}
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_node_is_one_line_with_rect_style_overrides_and_clipped_text() {
        let long = "word ".repeat(100);
        let deck = Deck::check(
            &json!({"version": 2, "title": "t", "slides": [{"id": "s1", "layout": {"mode": "column", "padding": 100}, "children": [
                {"id": "a", "kind": "text", "text": long, "style": "heading", "font": {"size": 64, "color": "accent"}}
            ]}]})
            .to_string(),
        )
        .unwrap();
        let line = node_brief(&deck, "s1", "a", None, 300).unwrap();
        assert!(line.starts_with("a text 100,100 "), "{line}");
        assert!(
            line.contains(" style heading size=64 color=accent : \""),
            "{line}"
        );
        assert!(line.ends_with("…\""), "{line}");
        assert!(line.chars().count() < 400, "{}", line.chars().count());
        assert!(!line.contains('\n'));
        assert!(node_brief(&deck, "s1", "nope", None, 300).is_none());
    }
}
