//! Hermetic editorial gallery and native Office fidelity fixtures.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};
use std::path::Path;

use serde_json::json;

use super::*;
use crate::deck::{self, Fill, Rect, text::Typesetter};

const NEW_LAYOUTS: [&str; 10] = [
    "cover_image",
    "section_color",
    "stats",
    "timeline",
    "image_grid",
    "quote_photo",
    "cards",
    "team",
    "chart_focus",
    "closing_image",
];

fn mix(pixel: &mut [f64; 3], color: [f64; 3], alpha: f64) {
    for (channel, color) in pixel.iter_mut().zip(color) {
        *channel = *channel * (1.0 - alpha) + color * alpha;
    }
}
fn oval(x: f64, y: f64, center: [f64; 2], radius: [f64; 2]) -> f64 {
    let distance = ((x - center[0]) / radius[0]).powi(2) + ((y - center[1]) / radius[1]).powi(2);
    (1.0 / (1.0 + ((distance - 1.0) * 36.0).exp())).clamp(0.0, 1.0)
}
fn photo(assets: &Path, variant: u32, portrait: bool) {
    let (width, height) = if portrait { (480, 640) } else { (960, 640) };
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let (x, y) = (
            f64::from(x) / f64::from(width),
            f64::from(y) / f64::from(height),
        );
        let n = f64::from(variant);
        let mut pixel = [85.0 + 70.0 * y, 117.0 + 52.0 * y, 116.0 + 30.0 * y];
        let light = (-((x - 0.74).powi(2) + (y - 0.18).powi(2)) / 0.09).exp();
        mix(&mut pixel, [246.0, 211.0, 158.0], light * 0.8);
        if portrait {
            let skin = [
                [204.0, 145.0, 113.0],
                [157.0, 104.0, 78.0],
                [221.0, 172.0, 137.0],
                [181.0, 125.0, 94.0],
            ][variant as usize - 1];
            let shirt = [
                [31.0, 70.0, 66.0],
                [151.0, 72.0, 51.0],
                [51.0, 60.0, 64.0],
                [126.0, 103.0, 66.0],
            ][variant as usize - 1];
            let cx = 0.5 + (n * 2.0).sin() * 0.025;
            let shirt_light = 0.8 + 0.25 * (-(x - cx + 0.1).powi(2) / 0.08).exp();
            mix(
                &mut pixel,
                shirt.map(|c| c * shirt_light),
                oval(x, y, [cx, 0.94], [0.43, 0.36]),
            );
            mix(
                &mut pixel,
                skin.map(|c| c * 0.87),
                oval(x, y, [cx, 0.575], [0.078, 0.08]),
            );
            mix(
                &mut pixel,
                [43.0, 34.0, 31.0],
                oval(x, y, [cx, 0.33], [0.195, 0.23]),
            );
            let face = oval(x, y, [cx, 0.38], [0.165, 0.185]);
            let nx = (x - cx) / 0.165;
            let shade = 0.67 + 0.34 * (1.0 - nx.powi(2)).max(0.0).sqrt() - nx * 0.12;
            mix(&mut pixel, skin.map(|c| c * shade), face);
            let cheek_light =
                (-((x - cx + 0.055).powi(2) / 0.004 + (y - 0.38).powi(2) / 0.016)).exp();
            mix(&mut pixel, skin.map(|c| c * 1.13), cheek_light * face * 0.3);
            for side in [-1.0, 1.0] {
                let eye = cx + side * 0.055;
                mix(
                    &mut pixel,
                    [62.0, 44.0, 37.0],
                    oval(x, y, [eye, 0.364], [0.030, 0.004]) * 0.35,
                );
                mix(
                    &mut pixel,
                    [224.0, 216.0, 202.0],
                    oval(x, y, [eye, 0.389], [0.022, 0.006]) * 0.5,
                );
                mix(
                    &mut pixel,
                    [43.0, 34.0, 31.0],
                    oval(x, y, [eye, 0.389], [0.007, 0.006]) * 0.8,
                );
            }
            mix(
                &mut pixel,
                skin.map(|c| c * 1.18),
                oval(x, y, [cx - 0.008, 0.423], [0.017, 0.040]) * 0.25,
            );
            mix(
                &mut pixel,
                [126.0, 76.0, 58.0],
                oval(x, y, [cx + 0.018, 0.437], [0.012, 0.026]) * 0.16,
            );
            mix(
                &mut pixel,
                [133.0, 72.0, 59.0],
                oval(x, y, [cx, 0.478], [0.048, 0.005]) * 0.6,
            );
            mix(
                &mut pixel,
                [49.0, 36.0, 31.0],
                oval(x, y, [cx - 0.024, 0.208], [0.175, 0.07]),
            );
        } else {
            for layer in 0..4 {
                let depth = f64::from(layer);
                let ridge = 0.43
                    + depth * 0.12
                    + 0.08 * (x * 5.0 + n + depth).sin()
                    + 0.025 * (x * 17.0 + n).cos();
                let alpha = 1.0 / (1.0 + ((ridge - y) * 110.0).exp());
                mix(
                    &mut pixel,
                    [
                        111.0 - depth * 22.0 + n * 3.0,
                        139.0 - depth * 26.0,
                        131.0 - depth * 25.0,
                    ],
                    alpha,
                );
            }
            let mist = (-((y - 0.61) / 0.045).powi(2)).exp() * 0.23;
            mix(&mut pixel, [221.0, 216.0, 190.0], mist);
        }
        let grain = ((x * 993.0 + y * 211.0).sin() * (x * 347.0 - y * 727.0).sin()) * 1.8;
        image::Rgb(pixel.map(|c| (c + grain).clamp(0.0, 255.0) as u8))
    });
    image
        .save(assets.join(format!(
            "{}{variant}.png",
            if portrait { "portrait" } else { "landscape" }
        )))
        .unwrap();
}

fn assets(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    for variant in 1..=4 {
        photo(root, variant, false);
        photo(root, variant, true);
    }
    let chart = Deck::check(&json!({"version":2,"title":"Illustrative pilot data","theme":{"size":{"w":1200,"h":700},"colors":{"bg":"#faf9f6"}},"slides":[{"id":"chart","children":[{"kind":"chart","chart":"bar","labels":["2024","2025","2026","2027"],"series":[{"name":"Decision cycle / days","values":[42,38,32,26]}],"position":{"x":40,"y":20},"sizing":{"w":1120,"h":660}}]}]}).to_string()).unwrap();
    let png = deck::render::render_slide_at(&chart, "chart", 1200, Some(root)).unwrap();
    std::fs::write(root.join("chart.png"), png).unwrap();
}

fn values(name: &str) -> BTreeMap<String, String> {
    let common = match name {
        "cover_image" => vec![
            ("image", "assets/landscape1.png"),
            ("kicker", "NEXUS / FIELD NOTES 2027"),
            ("title", "Make the future\nfeel closer."),
            ("subtitle", "A clearer perspective on what comes next."),
            ("caption", "A new perspective.\nA shared direction."),
        ],
        "closing_image" => vec![
            ("image", "assets/landscape3.png"),
            ("kicker", "THE NEXT CHAPTER / TOGETHER"),
            ("title", "Better, together."),
            ("subtitle", "Let's make room for what comes next."),
            ("caption", "hello@example.com\nnexus / studio"),
        ],
        "section_color" => vec![
            ("kicker", "CHAPTER TWO / A SHARED DIRECTION"),
            ("title", "A better way\nforward."),
            (
                "subtitle",
                "From scattered information to clear, collective action.",
            ),
            ("number", "02"),
        ],
        "stats" => vec![
            ("kicker", "THE PILOT / IN NUMBERS"),
            ("title", "The impact, in perspective."),
            ("takeaway", "Less friction. More room for meaningful work."),
        ],
        "timeline" => vec![
            ("kicker", "THE JOURNEY / ONE DELIBERATE STEP AT A TIME"),
            ("title", "From first question to lasting change."),
            (
                "caption",
                "An illustrative five-stage roadmap. Built to learn, not just to launch.",
            ),
        ],
        "image_grid" => vec![
            ("kicker", "A SENSE OF PLACE / 01—04"),
            ("title", "Room for a different perspective."),
            ("image1", "assets/landscape1.png"),
            ("caption1", "Places that open up possibility."),
            ("image2", "assets/landscape2.png"),
            ("caption2", "See the bigger picture."),
            ("image3", "assets/landscape3.png"),
            ("caption3", "Find a new point of view."),
            ("image4", "assets/landscape4.png"),
            ("caption4", "Leave space to explore."),
        ],
        "quote_photo" => vec![
            ("kicker", "PEOPLE / PERSPECTIVE"),
            ("image", "assets/portrait1.png"),
            ("quote", "Great tools make\ncomplex things\nfeel simple."),
            ("attribution", "Lena Ortiz"),
            ("role", "Founder / Fieldwork Studio"),
        ],
        "cards" => vec![
            ("kicker", "THE APPROACH / THREE CONNECTED IDEAS"),
            ("title", "Three ways to move forward."),
            (
                "subtitle",
                "A shared foundation for thoughtful, confident work.",
            ),
        ],
        "team" => vec![
            ("kicker", "THE PEOPLE / BEHIND THE WORK"),
            ("title", "Good people. Great possibilities."),
            ("subtitle", "Different perspectives. One shared direction."),
        ],
        "chart_focus" => vec![
            ("kicker", "THE EVIDENCE / DECISION CYCLE"),
            ("title", "Clarity creates momentum."),
            ("chart", "assets/chart.png"),
            ("label", "LESS TIME TO DECIDE"),
            ("metric", "38%"),
            ("takeaway", "Clarity compounds\nwith every cycle."),
            ("body", "42 days to 26.\nIllustrative pilot data."),
            (
                "caption",
                "Decision cycle in days / illustrative cohort, 2024–2027",
            ),
        ],
        _ => panic!("unknown gallery layout {name}"),
    };
    let mut values: BTreeMap<_, _> = common
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    if name == "stats" {
        for (i, (value, label, detail)) in [
            ("38%", "Faster decisions", "Less time finding the signal."),
            ("2.4x", "More connection", "Ideas move across the team."),
            (
                "12",
                "Days to a first pilot",
                "Small steps. Visible progress.",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            for (suffix, text) in [("value", value), ("label", label), ("detail", detail)] {
                values.insert(format!("stat{}_{suffix}", i + 1), text.into());
            }
        }
    }
    if name == "timeline" {
        for (i, (date, title, body)) in [
            (
                "JAN / 01",
                "Discover",
                "Listen closely. Ask a better question.",
            ),
            ("FEB / 02", "Focus", "Choose the challenge that matters."),
            ("MAR / 03", "Prototype", "Make the idea tangible, together."),
            ("APR / 04", "Pilot", "Learn from the work, in the work."),
            ("MAY / 05", "Scale", "Keep what works. Build what is next."),
        ]
        .into_iter()
        .enumerate()
        {
            for (suffix, text) in [("date", date), ("title", title), ("body", body)] {
                values.insert(format!("step{}_{suffix}", i + 1), text.into());
            }
        }
    }
    if name == "cards" {
        for (i, (icon, title, body)) in [
            (
                "AI",
                "Find clarity",
                "Bring the useful ideas into one clear view.",
            ),
            (
                "UX",
                "Build focus",
                "Give every conversation a shared direction.",
            ),
            (
                "GO",
                "Move faster",
                "Turn a good decision into a confident next step.",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            for (suffix, text) in [("icon", icon), ("title", title), ("body", body)] {
                values.insert(format!("card{}_{suffix}", i + 1), text.into());
            }
        }
    }
    if name == "team" {
        for (i, (name, role)) in [
            ("Maya Chen", "Creative Director"),
            ("Alex Rivera", "Product Strategy"),
            ("Sam Patel", "Design & Research"),
            ("Jordan Lee", "Engineering"),
        ]
        .into_iter()
        .enumerate()
        {
            values.insert(
                format!("person{}_image", i + 1),
                format!("assets/portrait{}.png", i + 1),
            );
            values.insert(format!("person{}_name", i + 1), name.into());
            values.insert(format!("person{}_role", i + 1), role.into());
        }
    }
    values
}

fn checked(name: &str, design: &Design, values: &BTreeMap<String, String>) -> Deck {
    let mut deck = Deck::from_design(name, design);
    deck.slides.push(instantiate(design, name, values).unwrap());
    Deck::check(&serde_json::to_string(&deck).unwrap()).unwrap()
}

fn text_fits(deck: &Deck, assets: &Path, rects: &BTreeMap<String, Rect>) {
    let mut typesetter = Typesetter::new(&deck.theme, Some(assets));
    let families = crate::deck::text::deck_font_info(&deck.theme, Some(assets));
    for node in deck.slides[0].nodes() {
        if let Kind::Text { text, style, font } = &node.kind {
            let rect = rects[&node.id];
            let resolved = deck.theme.resolve_font(style, font);
            // Layouts are sized for their font stack; when no named family
            // in it is installed (Linux CI: no Inter, Helvetica or Segoe) a
            // generic fallback face runs wider.
            let stack = crate::deck::text::font_stack(&deck.theme, &resolved.family);
            if !stack
                .split(',')
                .map(|name| name.trim().trim_matches('\''))
                .any(|name| families.iter().any(|f| f.family == name))
            {
                eprintln!(
                    "skipped {} {}: no font of {stack} is installed",
                    deck.title, node.id
                );
                continue;
            }
            let style = typesetter.text_style(resolved, false);
            let (height, width) = typesetter.markdown(
                &mut String::new(),
                &style,
                text,
                (0.0, 0.0, rect.w),
                "#000000",
            );
            assert!(
                height <= rect.h + 1.0 && width <= rect.w + 1.0,
                "{} {} {text:?}: content {width}x{height}, box {}x{}",
                deck.title,
                node.id,
                rect.w,
                rect.h
            );
        }
    }
}

fn fill_of(node: &Node) -> Option<&Fill> {
    match &node.kind {
        Kind::Frame { fill, .. } | Kind::Shape { fill, .. } => fill.as_ref(),
        _ => None,
    }
}
fn same_paint(before: Option<&Fill>, after: Option<&Fill>) {
    assert_eq!(before.is_some(), after.is_some());
    if let (Some(before), Some(after)) = (before, after) {
        assert_eq!(
            std::mem::discriminant(before),
            std::mem::discriminant(after)
        );
    }
}

fn round_trip(deck: &Deck, bytes: &[u8], assets: &Path, imported: &Path) -> (usize, usize) {
    let back = deck::import::import_pptx(bytes, imported).unwrap().deck;
    assert_eq!(back.slides.len(), 1);
    same_paint(
        deck.slides[0]
            .fill
            .as_ref()
            .filter(|fill| matches!(fill, Fill::Linear { .. })),
        back.slides[0]
            .fill
            .as_ref()
            .filter(|fill| matches!(fill, Fill::Linear { .. })),
    );
    let before_rects: BTreeMap<_, _> = deck::layout_at(deck, &deck.slides[0], Some(assets))
        .into_iter()
        .collect();
    let after_rects: BTreeMap<_, _> = deck::layout_at(&back, &back.slides[0], Some(imported))
        .into_iter()
        .collect();
    let mut counts = (0, 0);
    for original in deck.slides[0].nodes() {
        // Flattened, unpainted flex containers have no visible Office primitive.
        // Every painted background and every descendant is compared separately.
        if matches!(
            original.kind,
            Kind::Frame {
                fill: None,
                stroke: None,
                ..
            }
        ) {
            continue;
        }
        let name = if matches!(original.kind, Kind::Image { .. } | Kind::Shape { .. }) {
            &original.id
        } else {
            original.name.as_ref().unwrap_or(&original.id)
        };
        let found = back.slides[0]
            .nodes()
            .into_iter()
            .find(|node| node.name.as_ref() == Some(name))
            .unwrap_or_else(|| panic!("{}: missing {name}", deck.title));
        let (a, b) = (before_rects[&original.id], after_rects[&found.id]);
        for (a, b) in [a.x, a.y, a.w, a.h].into_iter().zip([b.x, b.y, b.w, b.h]) {
            assert!((a - b).abs() <= 1.0, "{} {name}: {a} != {b}", deck.title);
        }
        same_paint(fill_of(original), fill_of(found));
        assert_eq!(
            original.shadow.is_some(),
            found.shadow.is_some(),
            "{name}: shadow"
        );
        assert_eq!(original.mask, found.mask, "{name}: mask");
        assert!(
            (original.image_radius.unwrap_or(0.0) - found.image_radius.unwrap_or(0.0)).abs() <= 0.1,
            "{name}: radius"
        );
        same_paint(
            original.overlay.as_ref().map(|overlay| &overlay.fill),
            found.overlay.as_ref().map(|overlay| &overlay.fill),
        );
        if let Kind::Frame { radius, .. } | Kind::Shape { radius, .. } = &original.kind {
            let Kind::Shape { radius: actual, .. } = &found.kind else {
                panic!("{name}: shape");
            };
            assert!((radius - actual).abs() <= 0.1, "{name}: rounded geometry");
        }
        if let Kind::Text { text, style, font } = &original.kind {
            let Kind::Text {
                text: actual,
                style: actual_style,
                font: actual_font,
            } = &found.kind
            else {
                panic!("{name}: text");
            };
            assert_eq!(text, actual, "{} {name}", deck.title);
            let expected = deck.theme.resolve_font(style, font);
            let actual = back.theme.resolve_font(actual_style, actual_font);
            assert_eq!(
                deck.theme.family(&expected.family),
                back.theme.family(&actual.family),
                "{name}: family"
            );
            assert!(
                (expected.size - actual.size).abs() <= 0.02,
                "{name}: font size"
            );
            // PPTX carries bold or regular, so a 600 subheading comes back as 700.
            assert_eq!(
                expected.weight >= 600,
                actual.weight >= 600,
                "{name}: weight"
            );
            assert_eq!(expected.italic, actual.italic, "{name}: italic");
            counts.1 += 1;
        }
        counts.0 += 1;
    }
    counts
}

fn package_is_complete(bytes: &[u8]) {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut parts = BTreeMap::new();
    for i in 0..archive.len() {
        let mut part = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        part.read_to_end(&mut bytes).unwrap();
        parts.insert(part.name().to_string(), bytes);
    }
    let types =
        deck::import::xml::parse(std::str::from_utf8(&parts["[Content_Types].xml"]).unwrap())
            .unwrap();
    for (name, bytes) in &parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let root = deck::import::xml::parse(std::str::from_utf8(bytes).unwrap()).unwrap();
            for rel in root.kids_named("Relationship") {
                assert_ne!(rel.attr("TargetMode"), Some("External"));
                let base = Path::new(name).parent().unwrap().parent().unwrap();
                let target = rel.attr("Target").unwrap();
                let path = if target.starts_with('/') {
                    target.into()
                } else {
                    format!("{}/{target}", base.display())
                };
                let mut components = Vec::new();
                for part in path.split('/') {
                    match part {
                        "" | "." => {}
                        ".." => {
                            components.pop().unwrap();
                        }
                        value => components.push(value),
                    }
                }
                assert!(
                    parts.contains_key(&components.join("/")),
                    "{name}: missing {target}"
                );
            }
        }
        if name != "[Content_Types].xml" {
            assert!(
                types
                    .kids_named("Override")
                    .any(|part| part.attr("PartName") == Some(format!("/{name}").as_str()))
                    || types
                        .kids_named("Default")
                        .any(|part| part.attr("Extension") == name.rsplit('.').next()),
                "{name}: missing content type"
            );
        }
    }
}

#[test]
fn editorial_layout_gallery_checks_renders_and_round_trips_every_painted_node() {
    let root =
        std::env::temp_dir().join(format!("nexus-editorial-layouts-{}", uuid::Uuid::new_v4()));
    let assets_dir = root.join("assets");
    assets(&assets_dir);
    let design = Design::default();
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let mut fixtures = Vec::new();
    for name in NEW_LAYOUTS {
        let deck = checked(name, &design, &values(name));
        assert!(
            crate::design::lint_deck(&deck, &design).is_empty(),
            "{name}: design drift"
        );
        let png = deck::render::render_slide_at(&deck, &deck.slides[0].id, 1200, Some(&assets_dir))
            .unwrap();
        let bytes = deck::export::pptx::export_pptx(&deck, Some(&assets_dir)).unwrap();
        package_is_complete(&bytes);
        // Separate from the older count-test's generic placeholder fixtures.
        if std::env::var_os("NEXUS_LAYOUT_PREVIEWS").is_some() {
            let output = target.join("layout-previews/rich");
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(output.join(format!("{name}.png")), png).unwrap();
        }
        if std::env::var_os("NEXUS_PPTX_SAMPLES").is_some() {
            let output = target.join("pptx-samples/rich");
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(output.join(format!("{name}.pptx")), &bytes).unwrap();
        }
        fixtures.push((name, deck, bytes));
    }
    let mut report = String::new();
    for (name, deck, bytes) in fixtures {
        let rects = deck::layout_at(&deck, &deck.slides[0], Some(&assets_dir))
            .into_iter()
            .collect();
        text_fits(&deck, &assets_dir, &rects);
        let counts = round_trip(
            &deck,
            &bytes,
            &assets_dir,
            &root.join("imported").join(name),
        );
        use std::fmt::Write as _;
        writeln!(report,"{name}: {} painted rects within 1 px; {} text/font comparisons; fills, shadows, masks, radii and overlays retained",counts.0,counts.1).unwrap();
    }
    if std::env::var_os("NEXUS_PPTX_SAMPLES").is_some() {
        std::fs::write(target.join("pptx-samples/rich/round-trip.txt"), report).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unfilled_repeats_remove_their_photos_badges_and_card_backgrounds() {
    let design = Design::default();
    for (layout, slot, keep, remove) in [
        ("stats", "stat1_value", "stat1", "stat2"),
        ("timeline", "step1_title", "step1", "step2"),
        ("cards", "card1_title", "card1", "card2"),
        ("team", "person1_name", "person1", "person2"),
        ("image_grid", "caption1", "image1", "image2"),
    ] {
        let values = BTreeMap::from([(slot.to_string(), "Present".to_string())]);
        let deck = checked(layout, &design, &values);
        let nodes = deck.slides[0].nodes();
        assert!(
            nodes.iter().any(|node| node.name.as_deref() == Some(keep)),
            "{layout}: kept repeat"
        );
        assert!(
            !nodes
                .iter()
                .any(|node| node.name.as_deref() == Some(remove)),
            "{layout}: orphaned repeat decoration"
        );
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.text() == Some("Present"))
                .count(),
            1
        );
    }
}
