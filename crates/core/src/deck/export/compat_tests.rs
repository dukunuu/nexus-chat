use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};

use serde_json::json;

use crate::deck::import::xml;
use crate::deck::{self, Deck, Kind};

fn parts(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..archive.len())
        .map(|index| {
            let mut file = archive.by_index(index).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            (file.name().to_string(), bytes)
        })
        .collect()
}

fn validate_package(bytes: &[u8]) {
    let parts = parts(bytes);
    let types = xml::parse(std::str::from_utf8(&parts["[Content_Types].xml"]).unwrap()).unwrap();
    for (name, bytes) in &parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let root = xml::parse(std::str::from_utf8(bytes).unwrap()).unwrap();
            if name.ends_with(".rels") {
                for rel in root.kids_named("Relationship") {
                    assert_ne!(rel.attr("TargetMode"), Some("External"));
                    let base = std::path::Path::new(name)
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap();
                    let target = rel.attr("Target").unwrap();
                    let joined = if target.starts_with('/') {
                        target.trim_start_matches('/').to_string()
                    } else {
                        format!("{}/{target}", base.display())
                    };
                    let mut normalized = Vec::new();
                    for component in joined.split('/') {
                        match component {
                            "" | "." => {}
                            ".." => {
                                normalized.pop().unwrap();
                            }
                            value => normalized.push(value),
                        }
                    }
                    let target = normalized.join("/");
                    assert!(parts.contains_key(&target), "{name}: missing {target}");
                }
            }
        }
        if name == "[Content_Types].xml" {
            continue;
        }
        let extension = name.rsplit('.').next().unwrap();
        assert!(
            types
                .kids_named("Override")
                .any(|part| part.attr("PartName") == Some(format!("/{name}").as_str()))
                || types
                    .kids_named("Default")
                    .any(|part| part.attr("Extension") == Some(extension)),
            "untyped part {name}"
        );
    }
    for name in [
        "ppt/presProps.xml",
        "ppt/viewProps.xml",
        "ppt/tableStyles.xml",
    ] {
        assert!(parts.contains_key(name), "{name}");
    }
    assert_eq!(
        parts
            .keys()
            .filter(
                |name| name.starts_with("ppt/slideLayouts/slideLayout") && name.ends_with(".xml")
            )
            .count(),
        5
    );
}

#[test]
fn portable_pptx_round_trips_fixed_boxes_fonts_and_flattened_rotation() {
    let deck = Deck::check(&json!({"version":2,"title":"Portable","slides":[{"children":[{"id":"frame","kind":"frame","fill":"accent","position":{"x":100,"y":100},"sizing":{"w":400,"h":200},"rotation":90,"children":[{"id":"copy","kind":"text","text":"Clear decisions","font":{"family":"Inter","size":40,"weight":700,"italic":true,"line_height":1.25,"letter_spacing":0.05},"position":{"x":20,"y":20},"sizing":{"w":100,"h":40}}]},{"id":"mono","kind":"text","text":"42 / 100","font":{"family":"JetBrains Mono","size":32,"weight":400},"position":{"x":700,"y":200},"sizing":{"w":300,"h":80}}]}]}).to_string()).unwrap();
    let bytes = super::pptx::export_pptx(&deck, None).unwrap();
    validate_package(&bytes);
    let root = std::env::temp_dir().join(format!("nexus-pptx-{}", uuid::Uuid::new_v4()));
    let imported = deck::import::import_pptx(&bytes, &root).unwrap();
    let back = &imported.deck;
    let rects: BTreeMap<_, _> = deck::layout(back, &back.slides[0]).into_iter().collect();
    for (name, expected) in [
        ("frame", [100.0, 100.0, 400.0, 200.0]),
        ("copy", [310.0, 50.0, 100.0, 40.0]),
        ("mono", [700.0, 200.0, 300.0, 80.0]),
    ] {
        let node = back.slides[0]
            .nodes()
            .into_iter()
            .find(|node| node.name.as_deref() == Some(name))
            .unwrap();
        let actual = rects[&node.id];
        for (actual, expected) in [actual.x, actual.y, actual.w, actual.h]
            .into_iter()
            .zip(expected)
        {
            assert!(
                (actual - expected).abs() <= 1.0,
                "{name}: {actual} != {expected}"
            );
        }
        if let Kind::Text { text, style, font } = &node.kind {
            let Kind::Text {
                text: original,
                style: original_style,
                font: original_font,
            } = &deck.node(name).unwrap().kind
            else {
                panic!("text");
            };
            assert_eq!(text, original);
            let expected = deck.theme.resolve_font(original_style, original_font);
            let actual = back.theme.resolve_font(style, font);
            assert_eq!(
                back.theme.family(&actual.family),
                deck.theme.family(&expected.family)
            );
            assert!((actual.size - expected.size).abs() <= 0.1);
            assert_eq!(actual.weight, expected.weight);
            assert_eq!(actual.italic, expected.italic);
        }
    }
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn effects_write_native_drawingml_and_portable_text_properties() {
    let deck = Deck::check(&json!({"version":2,"title":"Effects","slides":[{"fill":{"linear":{"angle":90,"stops":[[0,"bg"],[1,"accent"]]}},"children":[{"kind":"shape","fill":{"linear":{"angle":135,"stops":[[0,"accent"],[1,{"color":"fg","opacity":0.3}]]}},"shadow":"md","stroke":{"color":"fg","width":2,"dash":"dash"},"sizing":{"w":400,"h":240}},{"kind":"text","text":"A portable story","font":{"family":"SF Pro","size":42,"line_height":1.4},"position":{"x":500,"y":100},"sizing":{"w":800,"h":120}}]}]}).to_string()).unwrap();
    let bytes = super::pptx::export_pptx(&deck, None).unwrap();
    validate_package(&bytes);
    let parts = parts(&bytes);
    let slide = std::str::from_utf8(&parts["ppt/slides/slide1.xml"]).unwrap();
    for expected in [
        "<a:gradFill",
        "<a:outerShdw",
        "<a:alpha val=\"30000\"",
        "<a:prstDash val=\"dash\"",
        "<a:lnSpc><a:spcPts val=\"2940\"",
        "<a:latin typeface=\"Inter\"",
        "<a:ea typeface=\"Inter\"",
        "<a:cs typeface=\"Inter\"",
        "lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\"",
    ] {
        assert!(slide.contains(expected), "{expected}");
    }
    assert!(!slide.contains("spAutoFit"));
    assert!(!slide.contains("spcPct"));
    assert!(!slide.contains("<p:grpSp>"));
    let root = std::env::temp_dir().join(format!("nexus-effect-import-{}", uuid::Uuid::new_v4()));
    let back = deck::import::import_pptx(&bytes, &root).unwrap().deck;
    assert!(matches!(
        back.slides[0].fill,
        Some(deck::Fill::Linear { .. })
    ));
    let shape = back.slides[0]
        .nodes()
        .into_iter()
        .find(|node| matches!(node.kind, Kind::Shape { .. }))
        .unwrap();
    let Kind::Shape { fill, stroke, .. } = &shape.kind else {
        unreachable!()
    };
    assert!(matches!(fill, Some(deck::Fill::Linear { .. })));
    assert!(shape.shadow.is_some());
    assert_eq!(stroke.as_ref().unwrap().dash, deck::Dash::Dash);
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn every_layout_exports_a_complete_package_and_optional_samples() {
    let design = crate::design::Design::default();
    let root = std::env::temp_dir().join(format!("nexus-layout-export-{}", uuid::Uuid::new_v4()));
    let assets = root.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    image::RgbImage::from_fn(640, 640, |x, y| {
        image::Rgb([
            u8::try_from(x * 200 / 640).unwrap(),
            u8::try_from(y * 160 / 640 + 40).unwrap(),
            120,
        ])
    })
    .save(assets.join("sample.png"))
    .unwrap();
    for (name, layout) in &design.slide.layouts {
        let values = crate::design::layouts::slots(layout)
            .into_iter()
            .map(|slot| {
                let value = match slot.as_str() {
                    "image" => "assets/sample.png",
                    "title" => "Make room for what matters.",
                    "subtitle" => "One source. A clearer story.",
                    "kicker" => "NEXUS / A NEW PERSPECTIVE",
                    "number" => "42%",
                    "quote" => "Clarity is the space between the things we choose to say.",
                    "attribution" => "The design team",
                    "left_title" => "Today",
                    "right_title" => "Tomorrow",
                    "items" => "1. Ask the question\n2. Follow the evidence\n3. Make the decision",
                    _ => "A shared foundation. Deliberate choices. Room to grow.",
                };
                (slot, value.to_string())
            })
            .collect();
        let mut deck = Deck::from_design(name, &design);
        deck.slides = vec![crate::design::layouts::instantiate(&design, name, &values).unwrap()];
        deck = Deck::check(&serde_json::to_string(&deck).unwrap()).unwrap();
        assert!(crate::design::lint_deck(&deck, &design).is_empty());
        let bytes = super::pptx::export_pptx(&deck, Some(&assets)).unwrap();
        validate_package(&bytes);
        let imported = deck::import::import_pptx(&bytes, &root.join("imported")).unwrap();
        assert_eq!(imported.deck.slides.len(), 1);
        if std::env::var_os("NEXUS_PPTX_SAMPLES").is_some() {
            let output =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pptx-samples");
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(output.join(format!("{name}.pptx")), bytes).unwrap();
        }
    }
    if std::env::var_os("NEXUS_PPTX_SAMPLES").is_some() {
        let prompt = include_str!("../../../tests/fixtures/prompt-snapshots/chat.txt").replace(
            "- OS: <os> (<arch>)",
            &format!(
                "- OS: {} ({})",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        );
        let definitions: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/prompt-snapshots/tool-defs.json"
        ))
        .unwrap();
        let compact = serde_json::to_string(&definitions).unwrap();
        let canvas = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "canvas")
            .unwrap();
        let report = format!(
            "Estimated tokens (same chars/4 metric as the snapshot gate):\nSystem + tools: {} / 3960\nCanvas definition: {}\nDefinition and goldens unchanged.\n",
            (prompt.chars().count() + compact.chars().count()) / 4,
            serde_json::to_string(canvas).unwrap().chars().count() / 4
        );
        std::fs::write(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/pptx-samples/tokens.txt"),
            report,
        )
        .unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pictures_write_native_masks_crops_scrims_and_backgrounds() {
    let root = std::env::temp_dir().join(format!("nexus-picture-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    image::RgbImage::from_pixel(400, 200, image::Rgb([70, 110, 150]))
        .save(root.join("photo.png"))
        .unwrap();
    let deck = Deck::check(&json!({"version":2,"title":"Photos","slides":[{"background":{"image":"assets/photo.png","fit":"cover","overlay":{"fill":{"color":"fg","opacity":0.4}}},"children":[{"kind":"image","src":"assets/photo.png","fit":"cover","focus":{"x":0.25,"y":0.5},"radius":24,"shadow":"md","overlay":{"fill":{"color":"fg","opacity":0.3}},"position":{"x":100,"y":100},"sizing":{"w":200,"h":200}},{"kind":"image","src":"assets/photo.png","mask":"circle","fit":"contain","focus":{"x":0.5,"y":0.9},"position":{"x":400,"y":100},"sizing":{"w":200,"h":200}}]}]}).to_string()).unwrap();
    let bytes = super::pptx::export_pptx(&deck, Some(&root)).unwrap();
    validate_package(&bytes);
    let parts = parts(&bytes);
    let slide = std::str::from_utf8(&parts["ppt/slides/slide1.xml"]).unwrap();
    for expected in [
        "<p:bg><p:bgPr><a:blipFill>",
        "prst=\"roundRect\"",
        "prst=\"ellipse\"",
        "<a:outerShdw",
        "<a:srcRect l=\"12500\" r=\"37500\"",
        "<a:alpha val=\"30000\"",
        "<a:alpha val=\"40000\"",
    ] {
        assert!(slide.contains(expected), "{expected}");
    }
    let imported_assets = root.join("imported");
    let back = deck::import::import_pptx(&bytes, &imported_assets)
        .unwrap()
        .deck;
    let before_rects: BTreeMap<_, _> = deck::layout_at(&deck, &deck.slides[0], Some(&root))
        .into_iter()
        .collect();
    let after_rects: BTreeMap<_, _> =
        deck::layout_at(&back, &back.slides[0], Some(&imported_assets))
            .into_iter()
            .collect();
    for original in deck.slides[0].nodes() {
        let found = back.slides[0]
            .nodes()
            .into_iter()
            .find(|node| node.name.as_deref() == Some(&original.id))
            .unwrap();
        let (a, b) = (before_rects[&original.id], after_rects[&found.id]);
        for (a, b) in [a.x, a.y, a.w, a.h].into_iter().zip([b.x, b.y, b.w, b.h]) {
            assert!((a - b).abs() <= 1.0);
        }
        assert_eq!(found.mask, original.mask);
        assert_eq!(found.image_radius, original.image_radius);
        assert_eq!(found.shadow.is_some(), original.shadow.is_some());
        assert_eq!(found.overlay.is_some(), original.overlay.is_some());
        if let Kind::Image {
            fit: deck::Fit::Contain,
            ..
        } = original.kind
        {
            assert!(matches!(
                found.kind,
                Kind::Image {
                    fit: deck::Fit::Contain,
                    ..
                }
            ));
            assert!((found.focus.unwrap().y - 0.9).abs() < 0.01);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn placeholder_boxes_scale_to_small_slides() {
    let mut deck = Deck::titled("Small slide");
    deck.theme.size = deck::SlideSize { w: 640.0, h: 360.0 };
    let bytes = super::pptx::export_pptx(&deck, None).unwrap();
    validate_package(&bytes);
    for (name, bytes) in parts(&bytes) {
        if name.starts_with("ppt/slideLayouts/slideLayout") && name.ends_with(".xml") {
            let layout = xml::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
            for shape in layout.find("spTree").unwrap().kids_named("sp") {
                let transform = shape.path(&["spPr", "xfrm"]).unwrap();
                let offset = transform.kid("off").unwrap();
                let extent = transform.kid("ext").unwrap();
                let (x, y, w, h) = (
                    offset.num("x").unwrap(),
                    offset.num("y").unwrap(),
                    extent.num("cx").unwrap(),
                    extent.num("cy").unwrap(),
                );
                assert!(
                    w > 0.0
                        && h > 0.0
                        && x >= 0.0
                        && y >= 0.0
                        && x + w <= 12_192_000.0
                        && y + h <= 6_858_000.0,
                    "{name}: {x},{y} {w}x{h}"
                );
            }
        }
    }
}

#[test]
fn font_map_preserves_google_families_and_known_slides_fonts() {
    for (source, expected) in [
        ("system-ui", "Inter"),
        ("Helvetica Neue, Arial", "Inter"),
        ("SF Mono", "JetBrains Mono"),
        ("Georgia", "Georgia"),
        ("Inter", "Inter"),
        ("JetBrains Mono", "JetBrains Mono"),
        ("DM Sans", "DM Sans"),
    ] {
        assert_eq!(super::pptx::google_font(source), expected);
    }
}
