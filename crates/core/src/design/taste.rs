//! Taste hints: the structural habits that make a deck look generic, found
//! without rendering. They go with every deck write as one `taste:` line, so
//! the model fixes them before the user sees them. Hints, never errors.

use std::collections::BTreeSet;

use crate::deck::{Deck, Dim, Fill, Kind, Node, Slide, Theme};

/// Distinct text sizes beyond which the hierarchy blurs.
const MAX_SIZES: usize = 6;
/// One-off hex colors beyond which the palette is gone.
const MAX_HEX: usize = 4;
/// The largest text on a slide below this reads as a document, not a slide.
const SMALL_TITLE: f64 = 64.0;
/// A visual anchor's minimum share of the slide area.
const ANCHOR_AREA: f64 = 0.01;
const MAX_BULLETS: usize = 5;
const MAX_WORDS: usize = 70;

/// What one pass over the deck found.
#[derive(Default)]
struct Scan {
    sizes: BTreeSet<String>,
    families: BTreeSet<String>,
    hex: BTreeSet<String>,
    texts: usize,
    plain: Vec<usize>,
    small: Vec<usize>,
    walls: Vec<usize>,
    faint: Vec<usize>,
}

fn scan(deck: &Deck) -> Scan {
    let theme = &deck.theme;
    let mut found = Scan::default();
    for (index, slide) in deck.slides.iter().enumerate() {
        let number = index + 1;
        let ground = ground(slide, theme);
        let mut largest: f64 = 0.0;
        let mut anchored = slide.background.is_some()
            || slide
                .fill
                .as_deref()
                .is_some_and(|fill| !matches!(fill, "bg" | "surface"));
        let mut wall = false;
        let mut low = false;
        visit(&slide.children, true, &mut |node, on_slide| {
            if let Some(fill) = node_fill(node)
                && let Some(value) = hex_of(fill)
            {
                found.hex.insert(value);
            }
            match &node.kind {
                Kind::Text { style, font, .. }
                | Kind::Table { style, font, .. }
                | Kind::Code { style, font, .. } => {
                    found.texts += 1;
                    let resolved = theme.resolve_font(style, font);
                    found.sizes.insert(format!("{:.0}", resolved.size));
                    found
                        .families
                        .insert(theme.family(&resolved.family).to_ascii_lowercase());
                    if let Some(color) = font.color.as_deref().filter(|c| c.starts_with('#')) {
                        found.hex.insert(color.to_ascii_lowercase());
                    }
                    largest = largest.max(resolved.size);
                    if resolved.size >= 200.0 {
                        anchored = true;
                    }
                    if let Kind::Text { text, .. } = &node.kind {
                        let bullets = text.lines().filter(|l| is_bullet(l)).count();
                        wall |=
                            bullets > MAX_BULLETS || text.split_whitespace().count() > MAX_WORDS;
                    }
                    if on_slide
                        && contrast(&crate::deck::text::color(theme, &resolved.color), &ground)
                            < 3.0
                    {
                        low = true;
                    }
                }
                Kind::Image { .. } | Kind::Chart { .. } => anchored = true,
                Kind::Shape { .. } | Kind::Frame { .. } => {
                    if node_fill(node).is_some() && area(node, deck) >= ANCHOR_AREA {
                        anchored = true;
                    }
                }
            }
        });
        if !anchored && !slide.children.is_empty() {
            found.plain.push(number);
        }
        if largest > 0.0 && largest < SMALL_TITLE {
            found.small.push(number);
        }
        if wall {
            found.walls.push(number);
        }
        if low {
            found.faint.push(number);
        }
    }
    found
}

/// Every hint for `deck`, most important first.
#[must_use]
pub fn hints(deck: &Deck) -> Vec<String> {
    let Scan {
        sizes,
        families,
        hex,
        texts,
        plain,
        small,
        walls,
        faint,
    } = scan(deck);
    let mut out = Vec::new();
    if !faint.is_empty() {
        out.push(format!(
            "low-contrast text on {}; use fg on light grounds and bg on dark ones",
            slides(&faint)
        ));
    }
    if !plain.is_empty() && deck.slides.len() > 1 {
        out.push(format!(
            "{} only text; give each a visual anchor (photo, big number, chart, color field or shapes)",
            slides(&plain)
        ));
    }
    if !walls.is_empty() {
        out.push(format!("{} read as walls of text; cut to 3 points or split into cards, a stat row or a timeline", slides(&walls)));
    }
    if !small.is_empty() {
        out.push(format!("{}: the largest text is under {SMALL_TITLE} px; titles want the title style (120 px or more)", slides(&small)));
    }
    if let Some(run) = repeated(deck) {
        out.push(format!(
            "{run} repeat one layout; vary the structure from slide to slide"
        ));
    }
    if texts >= 3 && families.len() == 1 {
        out.push(
            "one typeface throughout; pair a display face (heading) with the body face".into(),
        );
    }
    if sizes.len() > MAX_SIZES {
        out.push(format!(
            "{} text sizes; keep to the named styles",
            sizes.len()
        ));
    }
    if hex.len() > MAX_HEX {
        out.push(format!("{} one-off hex colors; use the theme roles (bg fg accent accent2 accent3 muted surface)", hex.len()));
    }
    if deck.slides.len() >= 5 && same_ground(deck) {
        out.push("every slide has the same background; set the cover, sections and closing on a dark or color field".into());
    }
    out
}

/// The hints as one line for a tool result, or nothing for a clean deck.
#[must_use]
pub fn summary(hints: &[String]) -> Option<String> {
    (!hints.is_empty()).then(|| {
        format!(
            "taste: {}",
            hints.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
        )
    })
}

fn visit(nodes: &[Node], on_slide: bool, f: &mut impl FnMut(&Node, bool)) {
    for node in nodes.iter().filter(|n| !n.hidden) {
        f(node, on_slide);
        // Text inside a filled frame sits on that frame, not the slide.
        let inner = on_slide && node_fill(node).is_none();
        visit(node.children(), inner, f);
    }
}

fn node_fill(node: &Node) -> Option<&Fill> {
    match &node.kind {
        Kind::Frame { fill, .. } | Kind::Shape { fill, .. } => fill.as_ref(),
        _ => None,
    }
}

fn hex_of(fill: &Fill) -> Option<String> {
    let value: &str = fill;
    value.starts_with('#').then(|| value.to_ascii_lowercase())
}

fn is_bullet(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("- ")
        || line.starts_with("* ")
        || line
            .split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The share of the slide a node's fixed box covers; flexible boxes count.
fn area(node: &Node, deck: &Deck) -> f64 {
    let size = deck.theme.size;
    match (node.sizing.w, node.sizing.h) {
        (Dim::Fixed(w), Dim::Fixed(h)) => w * h / (size.w * size.h),
        _ => 1.0,
    }
}

/// The slide's own background color, as hex.
fn ground(slide: &Slide, theme: &Theme) -> String {
    let role = slide.fill.as_deref().unwrap_or("bg");
    crate::deck::text::color(theme, role)
}

fn same_ground(deck: &Deck) -> bool {
    let first = deck.slides.first().map(|s| {
        (
            s.fill.as_deref().map(str::to_string),
            s.background.is_some(),
        )
    });
    deck.slides.iter().all(|s| {
        Some((
            s.fill.as_deref().map(str::to_string),
            s.background.is_some(),
        )) == first
    })
}

/// The first run of three or more slides with the same structure.
fn repeated(deck: &Deck) -> Option<String> {
    let shapes: Vec<String> = deck.slides.iter().map(structure).collect();
    let mut start = 0;
    for i in 1..=shapes.len() {
        if i == shapes.len() || shapes[i] != shapes[start] {
            if i - start >= 3 {
                return Some(format!("slides {}-{}", start + 1, i));
            }
            start = i;
        }
    }
    None
}

fn structure(slide: &Slide) -> String {
    fn walk(nodes: &[Node], out: &mut String) {
        for node in nodes {
            out.push_str(node.kind.name());
            if !node.children().is_empty() {
                out.push('(');
                walk(node.children(), out);
                out.push(')');
            }
        }
    }
    let mut out = String::new();
    walk(&slide.children, &mut out);
    out
}

fn slides(numbers: &[usize]) -> String {
    let list = numbers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if numbers.len() == 1 {
        format!("slide {list}")
    } else {
        format!("slides {list}")
    }
}

fn luminance(hex: &str) -> f64 {
    let digits = hex.trim_start_matches('#');
    let digits = if digits.len() == 3 {
        digits.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        digits.chars().take(6).collect()
    };
    let channel = |i: usize| {
        let c = u8::from_str_radix(digits.get(i..i + 2).unwrap_or("00"), 16).map_or(0.0, f64::from)
            / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests;
