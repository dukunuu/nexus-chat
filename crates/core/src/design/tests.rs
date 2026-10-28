use super::*;
use crate::deck::{self, Deck, Op};
use serde_json::json;

fn temp() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("nexus-design-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn defaults_and_derived_roles_share_a_scale() {
    let design = Design::default();
    assert_eq!(design.slide_theme(), deck::Theme::default());
    assert_eq!(design.doc.page.margins_pt, [72.0; 4]);
    assert_eq!(design.doc.page.footer, "{page}");
    assert_eq!(design.doc.measure_ch, 72);
    assert_eq!(design.doc.text_styles["body"].size, Some(11.0));
    assert_eq!(
        Design::check(&serde_json::to_string(&design).unwrap()).unwrap(),
        design
    );
    let (slide, doc) = derive_styles(Scale::default(), &design.fonts);
    assert_eq!(slide["body"].size, Some(42.0));
    assert_eq!(doc["h1"].size, Some(21.48));
    assert_eq!(slide["heading"].family, doc["h1"].family);
    let sparse = Design::check(r#"{"version":1,"name":"Minimal"}"#).unwrap();
    assert_eq!(sparse.colors, design.colors);
    assert_eq!(sparse.slide.text_styles["body"].size, Some(42.0));
}

#[test]
fn validation_repairs_layout_ids_and_locates_limits() {
    let mut source = serde_json::to_value(Design::default()).unwrap();
    source["slide"]["layouts"]["title"]["children"][0]["id"] = json!("");
    source["slide"]["layouts"]["title"]["children"][1]["id"] = json!("");
    let repaired = Design::check(&source.to_string()).unwrap();
    let ids = repaired.slide.layouts["title"]
        .nodes()
        .iter()
        .map(|n| &n.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), repaired.slide.layouts["title"].nodes().len());
    let text = source["slide"]["layouts"]["title"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .position(|n| n["kind"] == "text")
        .unwrap();
    source["slide"]["layouts"]["title"]["children"][text]["font"] = json!({"size": -2});
    let errors = Design::check(&source.to_string()).unwrap_err();
    let path = format!("$.slide.layouts.title.children[{text}].font.size");
    assert!(errors.iter().any(|e| e.path == path), "{errors:?}");
    source["slide"]["layouts"] = json!(
        (0..41)
            .map(|i| (format!("l{i}"), deck::Slide::new("s")))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(
        Design::check(&source.to_string())
            .unwrap_err()
            .iter()
            .any(|e| e.path == "$.slide.layouts")
    );
    source["slide"]["layouts"] =
        json!({"too_many": {"children": vec![json!({"kind":"text","text":"x"}); 201]}});
    assert!(
        Design::check(&source.to_string())
            .unwrap_err()
            .iter()
            .any(|e| e.message.contains("200 nodes"))
    );
    for patch in [
        json!({"scale":{"ratio":0}}),
        json!({"slide":{"spacing":{"gap":-1}}}),
        json!({"colors":{"bg":"red"}}),
        json!({"extra":true}),
    ] {
        let mut value = json!({"version":1,"name":"Bad"});
        value
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(Design::check(&value.to_string()).is_err());
    }
}

#[test]
fn storage_revisions_defaults_and_confinement() {
    let files = temp();
    assert_eq!(default_slug(&files).unwrap(), "nexus");
    let mut design = Design::default();
    design.name = "Brand".into();
    save(&files, "brand", &design).unwrap();
    assert_eq!(load(&files, "brand").unwrap(), design);
    assert_eq!(list(&files).unwrap(), vec!["brand", "nexus"]);
    set_default(&files, "brand").unwrap();
    assert_eq!(default_slug(&files).unwrap(), "brand");
    assert!(set_default(&files, "missing").is_err());
    let rev = design.revision();
    design.name.push('!');
    assert_ne!(design.revision(), rev);
    let a = Design::check(r#"{"name":"X","version":1}"#).unwrap();
    let b = Design::check(r#"{"version":1,"name":"X"}"#).unwrap();
    assert_eq!(a.revision(), b.revision());
    for slug in ["", ".hidden", "../x", "a..b", "a/b", "a b", "/tmp/x"] {
        assert!(load(&files, slug).is_err());
        assert!(save(&files, slug, &design).is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(files.join("absent"), files.join("design/link.design.json"))
            .unwrap();
        assert!(load(&files, "link").is_err());
        assert!(save(&files, "link", &design).is_err());
    }
    std::fs::remove_dir_all(files).unwrap();
}

#[test]
fn lint_covers_overrides_and_has_a_bounded_one_line_summary() {
    let design = Design::default();
    let mut deck = Deck::from_design("Clean", &design);
    let prepared = deck::direct::prepare(&deck, vec![json!({"op":"add_slide","layout":"content","slots":{"title":"Clean","body":"No drift"}})]).unwrap();
    deck = prepared.next;
    assert!(lint_deck(&deck, &design).is_empty());
    assert!(summary(&[]).is_none());
    let mut value = serde_json::to_value(&deck).unwrap();
    value["slides"][0]["children"][0]["font"] =
        json!({"color":"#ff00ff","family":"Other Sans","size":53});
    deck = Deck::check(&value.to_string()).unwrap();
    let drift = lint_deck(&deck, &design);
    assert_eq!(drift.len(), 3);
    let summary = summary(&drift).unwrap();
    assert!(summary.starts_with("design: 1 nodes use hex colors outside the design"));
    assert!(summary.contains("1 font sizes are not text styles"));
    assert!(!summary.contains('\n'));
    assert!(summary.chars().count() <= 200);
    let drift = (0..10)
        .map(|n| Drift::Color {
            id: format!("b{n}"),
            value: "#fff".into(),
        })
        .collect::<Vec<_>>();
    assert!(super::summary(&drift).unwrap().contains("+4"));
    let long = "long_node_id".repeat(10);
    let drift = vec![
        Drift::Color {
            id: long.clone(),
            value: "#fff".into(),
        },
        Drift::Size {
            id: long.clone(),
            px: 53.0,
        },
        Drift::Family {
            id: long,
            value: "Other".into(),
        },
    ];
    let line = super::summary(&drift).unwrap();
    assert!(line.len() <= 200);
    assert!(line.ends_with(')'));
    assert!(line.contains("+1"));
}

#[test]
fn deck_design_ops_preserve_overrides_and_old_serialization() {
    let design = Design::default();
    let base = Deck::from_design("Design", &design);
    let prepared = deck::direct::prepare(
        &base,
        vec![json!({"op":"add_slide","layout":"title","slots":{"title":"Hello"},"id":"intro"})],
    )
    .unwrap();
    assert_eq!(prepared.next.slides[0].id, "intro");
    // Decoration (the accent rule) stays; of the text slots only the filled one remains.
    let texts = prepared.next.slides[0]
        .nodes()
        .iter()
        .filter(|n| n.text().is_some())
        .count();
    assert_eq!(texts, 1, "unfilled slots removed");
    for raw in [
        json!({"op":"add_slide","layout":"missing","slots":{}}),
        json!({"op":"add_slide","layout":"title","slots":{"wrong":"x"}}),
    ] {
        let errors = deck::direct::prepare(&base, vec![raw]).err().unwrap();
        assert!(errors[0].message.contains("fix:"));
        assert!(errors[0].message.contains("title"));
    }
    let (proposals, errors) = deck::suggest::prepare(
        &base,
        vec![deck::suggest::RawSuggestion {
            why: "Start the story".into(),
            ops: vec![json!({"op":"add_slide","layout":"title","slots":{"title":"Hello"}})],
        }],
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        deck::apply(&base, &proposals[0].ops).unwrap().slides.len(),
        1
    );
    let mut next = prepared.next;
    let id = next.slides[0]
        .nodes()
        .iter()
        .find(|n| n.text().is_some())
        .unwrap()
        .id
        .clone();
    let mut custom = design.clone();
    custom.fonts.insert("heading".into(), "Custom Sans".into());
    let resolver = |_: &str| Ok(custom.clone());
    next = deck::ops::apply_with_resolver(
        &next,
        &[
            Op::UpdateNode {
                id: id.clone(),
                fields: json!({"font":{"size":57}}).as_object().unwrap().clone(),
                base_hash: next.node(&id).unwrap().hash(),
            },
            Op::ApplyDesign {
                design: Some("brand".into()),
            },
        ],
        Some(&resolver),
    )
    .unwrap();
    assert_eq!(next.design.as_deref(), Some("brand"));
    assert_eq!(next.design_rev, Some(custom.revision()));
    assert_eq!(next.theme.family("heading"), "Custom Sans");
    assert!(
        matches!(&next.node(&id).unwrap().kind, deck::Kind::Text { font, .. } if font.size == Some(57.0))
    );
    let old = Deck::titled("Old");
    let source = serde_json::to_string_pretty(&old).unwrap();
    assert!(!source.contains("design"));
    assert_eq!(
        source,
        serde_json::to_string_pretty(&Deck::check(&source).unwrap()).unwrap()
    );
}

#[test]
fn design_updates_precede_layout_resolution_in_direct_edits_and_suggestions() {
    let base = Deck::from_design("Layouts", &Design::default());
    let mut brand = Design::default();
    let layout = brand.slide.layouts["title"].clone();
    brand.slide.layouts.clear();
    brand.slide.layouts.insert("brand_title".into(), layout);
    let resolver = |slug: &str| {
        assert_eq!(slug, "brand");
        Ok(brand.clone())
    };
    let ops = vec![
        json!({"op":"apply_design","design":"brand"}),
        json!({"op":"add_slide","layout":"brand_title","slots":{"title":"Brand story"}}),
    ];
    let direct = deck::direct::prepare_with_resolver(&base, ops.clone(), Some(&resolver)).unwrap();
    assert_eq!(direct.next.design.as_deref(), Some("brand"));
    let (valid, errors) = deck::suggest::prepare_with_resolver(
        &base,
        vec![deck::suggest::RawSuggestion {
            why: "Follow the brand".into(),
            ops,
        }],
        Some(&resolver),
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        deck::ops::apply_with_resolver(&base, &valid[0].ops, Some(&resolver))
            .unwrap()
            .slides
            .len(),
        1
    );
}

#[test]
fn every_builtin_layout_checks_and_renders_to_png() {
    let design = Design::default();
    let files = temp();
    let assets = files.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    let picture = image::RgbImage::from_fn(640, 640, |x, y| {
        if x.abs_diff(320).pow(2) + y.abs_diff(320).pow(2) < 180_u32.pow(2) {
            image::Rgb([174, 89, 62])
        } else {
            image::Rgb([40, 41, 35])
        }
    });
    picture.save(assets.join("sample.png")).unwrap();
    assert_eq!(design.slide.layouts.len(), 21);
    for (name, layout) in &design.slide.layouts {
        let values = layouts::slots(layout).into_iter().map(|slot| {
            let value = match slot.as_str() {
                "image" => "assets/sample.png", "title" => "Make room for what matters.",
                "kicker" => "NEXUS / A NEW PERSPECTIVE", "subtitle" => "One source. A clearer story.",
                "number" => "42%", "quote" => "Clarity is the space between the things we choose to say.",
                "attribution" => "The design team", "left_title" => "Today", "right_title" => "Tomorrow",
                "items" => "1. Start with the question\n2. Follow the evidence\n3. Make the decision",
                "left" => "- A shared foundation\n- Deliberate choices\n- Room to grow",
                "right" => "- A consistent voice\n- Fewer distractions\n- More possibility",
                "caption" => "A shared foundation, built to last.",
                _ => "Start with the story. Keep what is essential, and leave room for the next idea.",
            }; (slot, value.to_string())
        }).collect::<BTreeMap<_,_>>();
        let base = Deck::from_design(name, &design);
        let deck = deck::direct::prepare(
            &base,
            vec![json!({"op":"add_slide","layout":name,"slots":values})],
        )
        .unwrap()
        .next;
        assert_eq!(
            Deck::check(&serde_json::to_string(&deck).unwrap()).unwrap(),
            deck
        );
        assert!(lint_deck(&deck, &design).is_empty());
        let png =
            deck::render::render_slide_at(&deck, &deck.slides[0].id, 960, Some(&assets)).unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (960, 540));
        let background = image.get_pixel(0, 0);
        assert!(
            image.pixels().filter(|p| *p != background).count() > 1000,
            "empty render: {name}"
        );
        if std::env::var_os("NEXUS_LAYOUT_PREVIEWS").is_some() {
            let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/layout-previews");
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join(format!("{name}.png")), png).unwrap();
        }
    }
    std::fs::remove_dir_all(files).unwrap();
}
