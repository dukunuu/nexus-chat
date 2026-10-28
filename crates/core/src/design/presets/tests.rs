use super::*;

fn luminance(hex: &str) -> f64 {
    let channel = |i: usize| {
        let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap()) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn every_preset_is_a_valid_design_with_its_own_voice() {
    let catalog = catalog();
    assert_eq!(catalog.len(), 8);
    let mut pairings = std::collections::BTreeSet::new();
    for (slug, character) in catalog {
        let design = preset(slug).unwrap();
        Design::check(&serde_json::to_string(&design).unwrap())
            .unwrap_or_else(|e| panic!("{slug}: {e:?}"));
        assert!(
            character.contains(';'),
            "{slug}: character names what it is for"
        );
        assert_eq!(
            design.slide.layouts.len(),
            Design::default().slide.layouts.len(),
            "{slug}"
        );
        assert!(design.slide.text_styles.contains_key("kicker"), "{slug}");
        // No two presets share a type pairing, and none is Inter on Inter.
        let pair = (
            design.fonts["heading"].clone(),
            design.fonts["body"].clone(),
        );
        assert_ne!(pair.0, pair.1, "{slug}: a display face over the body face");
        assert!(pairings.insert(pair), "{slug}: pairing repeated");
    }
    assert!(preset("nexus").is_none() && preset("nope").is_none());
}

#[test]
fn preset_palettes_keep_the_contrast_the_layouts_rely_on() {
    for p in PRESETS {
        let [bg, fg, accent, accent2, accent3, muted, surface, _] = p.colors;
        let slug = p.slug;
        assert!(contrast(fg, bg) >= 12.0, "{slug}: body text");
        assert!(contrast(fg, surface) >= 10.0, "{slug}: text on cards");
        assert!(contrast(muted, bg) >= 3.0, "{slug}: captions");
        assert!(contrast(accent, bg) >= 3.0, "{slug}: accent display text");
        // Text on the deep panel (accent2): bg on light designs, fg on dark ones.
        let on_panel = if dark(bg) { fg } else { bg };
        assert!(
            contrast(on_panel, accent2) >= 4.5,
            "{slug}: text on accent2 panels"
        );
        // Text on bright highlight cards stays dark.
        let dark_text = if dark(bg) { bg } else { fg };
        assert!(
            contrast(dark_text, accent3) >= 4.5,
            "{slug}: text on accent3 cards"
        );
    }
}

#[test]
fn dark_presets_keep_panels_dark_and_their_text_light() {
    let midnight = preset("midnight").unwrap();
    let closing = &midnight.slide.layouts["closing"];
    let Some(Fill::Linear { linear }) = &closing.fill else {
        panic!("closing is a gradient");
    };
    assert!(
        linear.stops.iter().all(|(_, c)| c.value() != "fg"),
        "no light end on a dark design"
    );
    let colors: Vec<_> = closing
        .nodes()
        .into_iter()
        .filter_map(|node| match &node.kind {
            Kind::Text { font, .. } => Some(font.color.clone()),
            _ => None,
        })
        .collect();
    assert!(
        colors.iter().all(|c| c.as_deref() != Some("bg")),
        "{colors:?}"
    );
    // A light preset keeps the layouts as drawn.
    assert_eq!(
        preset("editorial").unwrap().slide.layouts["closing"].fill,
        Design::default().slide.layouts["closing"].fill
    );
}

#[test]
fn presets_are_listed_loaded_and_resolved_like_stored_designs() {
    let root = std::env::temp_dir().join(format!("nexus-presets-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let names = crate::design::list(&root).unwrap();
    for (slug, _) in catalog() {
        assert!(names.contains(&slug.to_string()), "{slug}");
        assert_eq!(
            crate::design::load(&root, slug).unwrap().name,
            preset(slug).unwrap().name
        );
        assert!(crate::design::resolve(slug, None).is_ok());
    }
    std::fs::remove_dir_all(root).unwrap();
}
