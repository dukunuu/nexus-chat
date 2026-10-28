//! Curated built-in designs. Each is a complete point of view: a palette with
//! one dominant color and one sharp accent, a display face paired with a body
//! face (Google Fonts, fetched into the deck when the device lacks them), a
//! type scale with real contrast, corners and shadows. The `character` line is
//! what the model reads when it picks one for a topic.

use std::collections::BTreeMap;

use super::{Design, DocDesign, Scale, SlideDesign, Spacing, derive_styles, layouts};
use crate::deck::{Color, Fill, Kind, Node, ShadowSpec};

/// How a preset's shadows look: soft, none, or hard offset blocks.
#[derive(Clone, Copy)]
enum Shadows {
    Soft,
    Flat,
    Block,
}

struct Preset {
    slug: &'static str,
    name: &'static str,
    character: &'static str,
    /// bg, fg, accent, accent2, accent3, muted, surface, border.
    colors: [&'static str; 8],
    heading: &'static str,
    body: &'static str,
    mono: &'static str,
    heading_weight: u16,
    /// Tracking for the title, heading and display styles, in em.
    tracking: f64,
    /// Title and display sizes relative to the derived scale.
    drama: f64,
    /// The kicker's family role; `mono` reads technical.
    kicker_family: &'static str,
    radius: f64,
    shadows: Shadows,
}

const PRESETS: &[Preset] = &[
    Preset {
        slug: "editorial",
        name: "Editorial",
        character: "magazine serif headlines on warm paper, ink text, one vermilion accent; essays, strategy, culture, research narratives",
        colors: [
            "#f4efe6", "#1b1a17", "#d9472b", "#1f3a5f", "#c9a227", "#6f6a60", "#eae3d6", "#d6cdbd",
        ],
        heading: "Fraunces",
        body: "Inter",
        mono: "JetBrains Mono",
        heading_weight: 600,
        tracking: -0.02,
        drama: 1.3,
        kicker_family: "body",
        radius: 4.0,
        shadows: Shadows::Flat,
    },
    Preset {
        slug: "midnight",
        name: "Midnight",
        character: "deep navy dark mode, electric periwinkle with cyan highlights, geometric grotesk, mono labels; AI, engineering, product launches, technical talks",
        colors: [
            "#0b1020", "#eef1f8", "#6e8bff", "#1d2b6b", "#22d3ee", "#8a93ab", "#151b2e", "#262e47",
        ],
        heading: "Space Grotesk",
        body: "Inter",
        mono: "JetBrains Mono",
        heading_weight: 700,
        tracking: -0.03,
        drama: 1.25,
        kicker_family: "mono",
        radius: 20.0,
        shadows: Shadows::Soft,
    },
    Preset {
        slug: "swiss",
        name: "Swiss",
        character: "International style: white, black, one signal red, tight heavy grotesk, square corners, no shadows; data, research, finance, architecture",
        colors: [
            "#ffffff", "#111111", "#e30613", "#111111", "#ffcc00", "#6b6b6b", "#f2f2f2", "#dadada",
        ],
        heading: "Inter Tight",
        body: "Inter",
        mono: "JetBrains Mono",
        heading_weight: 800,
        tracking: -0.04,
        drama: 1.4,
        kicker_family: "body",
        radius: 0.0,
        shadows: Shadows::Flat,
    },
    Preset {
        slug: "terracotta",
        name: "Terracotta",
        character: "sun-baked clay, sage and saffron, soft display serif with a friendly sans, round corners; food, travel, community, personal stories",
        colors: [
            "#fbf4ec", "#3a2418", "#c8553d", "#2f5d50", "#f2a541", "#8c6f5e", "#f1e2d3", "#e4d2bf",
        ],
        heading: "DM Serif Display",
        body: "DM Sans",
        mono: "DM Mono",
        heading_weight: 400,
        tracking: -0.01,
        drama: 1.3,
        kicker_family: "body",
        radius: 28.0,
        shadows: Shadows::Soft,
    },
    Preset {
        slug: "forest",
        name: "Forest",
        character: "pale moss, deep green and brass, high-contrast serif with a humanist sans; nature, sustainability, wellness, education, nonprofits",
        colors: [
            "#eef2ec", "#14261c", "#2e7d4f", "#14261c", "#c9a227", "#5e6e63", "#dfe7dc", "#cbd6c7",
        ],
        heading: "Playfair Display",
        body: "Source Sans 3",
        mono: "JetBrains Mono",
        heading_weight: 700,
        tracking: -0.01,
        drama: 1.25,
        kicker_family: "body",
        radius: 16.0,
        shadows: Shadows::Soft,
    },
    Preset {
        slug: "pop",
        name: "Pop",
        character: "cream, tomato, cobalt and sunflower, chunky expressive grotesk, black outlines and hard offset shadows; startups, events, youth, playful pitches",
        colors: [
            "#fff8e7", "#1a1a1a", "#f04a28", "#3b5bff", "#ffc736", "#5a5a5a", "#ffffff", "#1a1a1a",
        ],
        heading: "Bricolage Grotesque",
        body: "DM Sans",
        mono: "Space Mono",
        heading_weight: 800,
        tracking: -0.03,
        drama: 1.35,
        kicker_family: "mono",
        radius: 24.0,
        shadows: Shadows::Block,
    },
    Preset {
        slug: "noir",
        name: "Noir",
        character: "near-black, warm ivory and antique gold, refined light serif with generous space, no shadows; luxury, fashion, film, premium brands",
        colors: [
            "#111111", "#f5f1ea", "#d4af37", "#8c1c13", "#a8a29e", "#9a948a", "#1c1c1c", "#2e2e2e",
        ],
        heading: "Cormorant Garamond",
        body: "Inter",
        mono: "JetBrains Mono",
        heading_weight: 500,
        tracking: 0.0,
        drama: 1.45,
        kicker_family: "body",
        radius: 0.0,
        shadows: Shadows::Flat,
    },
    Preset {
        slug: "clarity",
        name: "Clarity",
        character: "crisp white and slate, confident blue on navy panels with a green highlight, modern geometric sans; SaaS, consulting, business reviews, dashboards",
        colors: [
            "#f7f9fc", "#0f172a", "#2563eb", "#1e3a8a", "#10b981", "#64748b", "#ffffff", "#e2e8f0",
        ],
        heading: "Manrope",
        body: "Inter",
        mono: "JetBrains Mono",
        heading_weight: 800,
        tracking: -0.02,
        drama: 1.2,
        kicker_family: "body",
        radius: 16.0,
        shadows: Shadows::Soft,
    },
];

const ROLES: [&str; 8] = [
    "bg", "fg", "accent", "accent2", "accent3", "muted", "surface", "border",
];

/// The curated slugs with their one-line character, in a stable order.
#[must_use]
pub fn catalog() -> Vec<(&'static str, &'static str)> {
    PRESETS.iter().map(|p| (p.slug, p.character)).collect()
}

/// The curated design `slug`, if it is one.
#[must_use]
pub fn preset(slug: &str) -> Option<Design> {
    PRESETS.iter().find(|p| p.slug == slug).map(build)
}

fn shadow(x: f64, y: f64, blur: f64, color: &str, opacity: f64) -> ShadowSpec {
    ShadowSpec {
        x,
        y,
        blur,
        spread: 0.0,
        color: Color::Alpha {
            color: color.into(),
            opacity,
        },
    }
}

fn build(p: &Preset) -> Design {
    let fonts: BTreeMap<String, String> =
        [("heading", p.heading), ("body", p.body), ("mono", p.mono)]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
    let scale = Scale::default();
    let (mut slide, doc) = derive_styles(scale, &fonts);
    for (name, font) in &mut slide {
        if matches!(
            name.as_str(),
            "title" | "heading" | "subheading" | "display" | "quote"
        ) {
            font.weight = Some(if name == "subheading" {
                p.heading_weight.clamp(400, 600)
            } else {
                p.heading_weight
            });
            font.letter_spacing = Some(p.tracking);
        }
        if name == "title" {
            font.size = font.size.map(|s| (s * p.drama).round());
        }
        if name == "kicker" {
            font.family = Some(p.kicker_family.into());
            if p.kicker_family == "mono" {
                font.letter_spacing = Some(0.06);
            }
        }
    }
    let spacing = Spacing {
        radius: p.radius,
        ..Spacing::default()
    };
    let shadows = match p.shadows {
        Shadows::Soft => BTreeMap::new(),
        Shadows::Flat => ["sm", "md", "lg"]
            .map(|name| (name.to_string(), shadow(0.0, 0.0, 0.0, "fg", 0.0)))
            .into(),
        Shadows::Block => [("sm", 6.0), ("md", 10.0), ("lg", 16.0)]
            .map(|(name, offset)| (name.to_string(), shadow(offset, offset, 0.0, "fg", 1.0)))
            .into(),
    };
    let mut layouts = layouts::builtin(spacing);
    if dark(p.colors[0]) {
        for layout in layouts.values_mut() {
            let bright = layout.fill.as_ref().is_some_and(is_bright);
            for node in &mut layout.children {
                for_dark(node, bright);
            }
            if let Some(fill) = &mut layout.fill {
                fill_on_dark(fill);
            }
        }
    }
    let base = Design::default();
    let mut all_shadows = base.shadows.clone();
    all_shadows.extend(shadows);
    Design {
        name: p.name.into(),
        colors: ROLES
            .iter()
            .zip(p.colors)
            .map(|(role, value)| ((*role).to_string(), value.to_string()))
            .collect(),
        fonts,
        scale,
        shadows: all_shadows,
        slide: SlideDesign {
            text_styles: slide,
            spacing,
            layouts,
            ..base.slide.clone()
        },
        doc: DocDesign {
            text_styles: doc,
            ..base.doc.clone()
        },
        ..base
    }
}

/// Whether a background is dark enough that `fg` is the light color.
fn dark(hex: &str) -> bool {
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_or(0.0, f64::from);
    channel(1) * 0.2126 + channel(3) * 0.7152 + channel(5) * 0.0722 < 100.0
}

/// The layouts paint dark panels with `fg` and set text on them in `bg`,
/// which assumes a light design. On a dark one those trade places, so the
/// panels stay dark and their text stays light; text on a bright accent panel
/// is dark (`bg`), and elsewhere `accent2` text (a deep panel color here)
/// takes the bright `accent3`. Cards on `bg` or `surface` with `fg` text are already right.
fn for_dark(node: &mut Node, on_bright: bool) {
    if let Some(overlay) = &mut node.overlay {
        fill_on_dark(&mut overlay.fill);
    }
    let mut bright = on_bright;
    if let Kind::Frame {
        fill: Some(fill), ..
    }
    | Kind::Shape {
        fill: Some(fill), ..
    } = &mut node.kind
    {
        bright = is_bright(fill);
        fill_on_dark(fill);
    }
    if let Kind::Text { font, .. } = &mut node.kind {
        let swapped = match (font.color.as_deref(), bright) {
            (Some("bg"), false) => Some("fg"),
            (Some("fg"), true) => Some("bg"),
            (Some("accent2"), false) => Some("accent3"),
            _ => None,
        };
        if let Some(color) = swapped {
            font.color = Some(color.into());
        }
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            for_dark(child, bright);
        }
    }
}

/// A fill that starts in a bright accent; text on it stays dark.
fn is_bright(fill: &Fill) -> bool {
    matches!(&**fill, "accent" | "accent3")
}

fn fill_on_dark(fill: &mut Fill) {
    let swap = |color: &mut Color| match color {
        Color::Solid(value) | Color::Alpha { color: value, .. } if value == "fg" => {
            *value = "bg".into();
        }
        _ => {}
    };
    match fill {
        Fill::Solid(color) => swap(color),
        Fill::Linear { linear } => linear.stops.iter_mut().for_each(|(_, color)| swap(color)),
        Fill::Radial { radial } => radial.stops.iter_mut().for_each(|(_, color)| swap(color)),
    }
}

#[cfg(test)]
mod tests;
