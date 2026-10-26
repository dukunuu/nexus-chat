use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;

fn starter() -> Deck {
    Deck::classic_fixture("A better story")
}
fn check(value: &Value) -> std::result::Result<Deck, Vec<CheckError>> {
    Deck::check(&serde_json::to_string(value).unwrap())
}
fn value() -> Value {
    serde_json::to_value(starter()).unwrap()
}
fn op(value: Value) -> Op {
    serde_json::from_value(value).expect("op")
}
fn update(deck: &Deck, id: &str, fields: Value) -> Op {
    Op::UpdateNode {
        id: id.into(),
        fields: fields.as_object().unwrap().clone(),
        base_hash: deck.node(id).unwrap().hash(),
    }
}
fn text(id: &str, body: &str) -> Value {
    json!({"id": id, "kind": "text", "text": body})
}
fn ids(deck: &Deck) -> Vec<String> {
    deck.slides
        .iter()
        .flat_map(|s| {
            std::iter::once(s.id.clone()).chain(s.nodes().into_iter().map(|n| n.id.clone()))
        })
        .collect()
}

// -- version 1 migration -------------------------------------------------------

#[test]
fn the_classic_v1_deck_migrates_to_absolute_px_nodes() {
    let deck = starter();
    assert_eq!(deck.version, 2);
    assert_eq!(
        deck.theme.size,
        SlideSize {
            w: 1920.0,
            h: 1080.0
        }
    );
    let s1 = &deck.slides[0];
    assert_eq!(s1.layout.mode, Mode::None);
    assert!(s1.name.is_none() && s1.transition.is_none());
    let title = &s1.children[0];
    assert_eq!(title.id, "b1");
    assert_eq!(title.sizing, Sizing::fixed(1680.0, 300.0));
    assert_eq!(title.position, Position::Absolute { x: 120.0, y: 360.0 });
    let Kind::Text { style, font, .. } = &title.kind else {
        panic!("text")
    };
    assert_eq!(style, "body");
    // xxl: 119 px with the tight title line height; left alignment is the default.
    assert_eq!(font.size, Some(119.0));
    assert_eq!(font.line_height, Some(1.08));
    assert_eq!(font.align, None);
    let sizes: Vec<_> = deck.slides[1]
        .children
        .iter()
        .map(|n| match &n.kind {
            Kind::Text { font, .. } => font.size,
            _ => None,
        })
        .collect();
    // xl is 81 px; l is 58 px.
    assert_eq!(sizes, [Some(81.0), Some(58.0)]);
    assert_eq!(ids(&deck), ["s1", "b1", "s2", "b2", "b3"]);
}

#[test]
fn migration_is_idempotent_and_leaves_v2_alone() {
    let v1 = include_str!("../../tests/fixtures/deck/classic.deck.json");
    let once = Deck::check(v1).unwrap();
    let again = Deck::check(&serde_json::to_string_pretty(&once).unwrap()).unwrap();
    assert_eq!(again, once);
    assert_eq!(again.revision(), once.revision());
    let mut value: Value = serde_json::from_str(v1).unwrap();
    migrate::v1_to_v2(&mut value);
    let snapshot = value.clone();
    migrate::v1_to_v2(&mut value);
    assert_eq!(value, snapshot);
}

#[test]
fn migration_maps_every_block_kind_and_style() {
    let v1 = json!({"version": 1, "title": "Kinds", "slides": [{
        "id": "s", "layout": "quote", "notes": "n", "blocks": [
        {"id": "t", "kind": "text", "x": 1, "y": 2, "w": 10, "h": 3, "text": "Hi",
         "style": {"align": "center", "size": "s", "color": "accent"}},
        {"id": "c", "kind": "code", "x": 0, "y": 0, "w": 4, "h": 2, "language": "rs", "source": "x", "style": {"size": "m"}},
        {"id": "tb", "kind": "table", "x": 0, "y": 0, "w": 4, "h": 2, "rows": [["a"]], "style": {}},
        {"id": "sh", "kind": "shape", "x": 0, "y": 0, "w": 2, "h": 2, "shape": "ellipse", "fill": "accent", "stroke": "fg", "style": {}},
        {"id": "ch", "kind": "chart", "x": 0, "y": 0, "w": 2, "h": 2, "chart": "pie", "labels": ["a"], "series": [{"name": "s", "values": [1]}], "style": {}},
        {"id": "im", "kind": "image", "x": 0, "y": 0, "w": 2, "h": 2, "src": "assets/a.png", "fit": "cover", "style": {}}
    ]}]});
    let deck = check(&v1).unwrap();
    let node = |id: &str| deck.node(id).unwrap();
    let Kind::Text { font, .. } = &node("t").kind else {
        panic!()
    };
    assert_eq!(font.size, Some(26.0));
    assert_eq!(font.align, Some(Align::Center));
    assert_eq!(font.color.as_deref(), Some("accent"));
    // Code at the old `m` size is 42 px; the code style is 36.
    let Kind::Code { style, font, .. } = &node("c").kind else {
        panic!()
    };
    assert_eq!((style.as_str(), font.size), ("code", Some(42.0)));
    let Kind::Shape { stroke, fill, .. } = &node("sh").kind else {
        panic!()
    };
    assert_eq!(fill.as_deref(), Some("accent"));
    assert_eq!(stroke.as_ref().unwrap().color, "fg");
    assert!((stroke.as_ref().unwrap().width - 2.4).abs() < 1e-9);
    assert!(matches!(node("ch").kind, Kind::Chart { .. }));
    assert!(matches!(
        node("im").kind,
        Kind::Image {
            fit: Fit::Cover,
            ..
        }
    ));
    assert_eq!(deck.slides[0].notes, "n");
    assert_eq!(check(&serde_json::to_value(&deck).unwrap()).unwrap(), deck);
}

#[test]
fn migration_keeps_the_v1_grid_repair_and_reports_bad_blocks_where_they_are() {
    let mut doc = json!({"version": 1, "title": "x", "slides": [{"blocks": [
        {"kind": "text", "x": -50, "y": 99, "w": 100, "h": -2, "text": "a", "style": {}}
    ]}]});
    let deck = check(&doc).unwrap();
    let node = &deck.slides[0].children[0];
    // x 0, y 17, w 32, h 1 grid units, times 60.
    assert_eq!(node.position, Position::Absolute { x: 0.0, y: 1020.0 });
    assert_eq!(node.sizing, Sizing::fixed(1920.0, 60.0));
    doc["slides"][0]["blocks"][0]["kind"] = json!("script");
    let errors = check(&doc).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.path == "$.slides[0].children[0].kind")
    );
}

// -- validation -----------------------------------------------------------------

#[test]
fn round_trip_all_node_kinds() {
    let mut doc = value();
    let payloads = [
        json!({"kind": "image", "src": "assets/chart.png", "fit": "cover"}),
        json!({"kind": "shape", "shape": "ellipse", "fill": "accent", "stroke": {"color": "fg", "width": 3}, "radius": 4}),
        json!({"kind": "table", "rows": [["Name", "Value"], ["Revenue", "18%"]]}),
        json!({"kind": "code", "language": "rust", "source": "fn main() {}", "font": {"size": 30}}),
        json!({"kind": "chart", "chart": "line", "labels": ["Q1", "Q2"], "series": [{"name": "Growth", "values": [2.5, 4.0]}]}),
        json!({"kind": "frame", "name": "Row", "fill": "muted", "stroke": {"color": "fg", "width": 2}, "radius": 12, "clip": true,
               "layout": {"mode": "row", "wrap": true, "gap": 16, "gap_cross": 8, "padding": [8, 16], "main": "space_between", "cross": "center"},
               "sizing": {"w": "fill", "h": "hug", "min_w": 100, "max_h": 400}, "opacity": 0.5, "rotation": 3.5,
               "animation": {"build_in": {"effect": "fade"}},
               "children": [text("inner", "Hello")]}),
    ];
    for mut payload in payloads {
        payload["position"] = json!({"x": 20, "y": 30});
        doc["slides"][0]["children"]
            .as_array_mut()
            .unwrap()
            .push(payload);
    }
    doc["slides"][0]["transition"] = json!({"type": "push"});
    let deck = check(&doc).unwrap();
    assert_eq!(
        Deck::check(&serde_json::to_string_pretty(&deck).unwrap()).unwrap(),
        deck
    );
    assert_eq!(deck.slides[0].children.len(), 7);
    assert_eq!(
        deck.slides[0].transition.as_ref().map(|t| t.kind),
        Some(TransitionType::Push)
    );
    let frame = deck.slides[0].children.last().unwrap();
    assert_eq!(
        frame
            .animation
            .as_ref()
            .and_then(|a| a.build_in.as_ref())
            .map(|b| b.effect),
        Some(Effect::Fade)
    );
    let Kind::Frame { layout, .. } = &frame.kind else {
        panic!()
    };
    assert_eq!(layout.padding, [8.0, 16.0, 8.0, 16.0]);
    assert_eq!(layout.main, Main::SpaceBetween);
    assert_eq!(frame.sizing.w, Dim::Fill);
    assert_eq!(frame.sizing.min_w, Some(100.0));
    assert_eq!(deck.node("inner").unwrap().id, "inner");
}

#[test]
fn fills_ids_preserving_existing_ids_and_theme_defaults() {
    let mut doc = value();
    doc.as_object_mut().unwrap().remove("theme");
    doc["slides"][0]["id"] = json!("");
    doc["slides"][1]["id"] = json!("b1");
    doc["slides"][1]["children"][0]
        .as_object_mut()
        .unwrap()
        .remove("id");
    let deck = check(&doc).unwrap();
    let all = ids(&deck);
    assert_eq!(all.len(), all.iter().collect::<BTreeSet<_>>().len());
    assert!(all.iter().all(|id| !id.is_empty()));
    assert_eq!(check(&doc).unwrap(), deck);
    assert_eq!(deck.theme, Theme::default());
    assert!(deck.theme.text_styles.contains_key("title"));
}

#[test]
fn partial_theme_fills_base_tokens_and_styles() {
    let mut doc = value();
    doc["theme"] = json!({"colors": {"accent": "#ffcc00", "custom": "#123"},
        "text_styles": {"quote": {"family": "heading", "size": 64, "italic": true}}});
    doc["slides"][0]["children"][0]["font"] = json!({"color": "custom"});
    doc["slides"][0]["children"][0]["style"] = json!("quote");
    let deck = check(&doc).unwrap();
    assert_eq!(deck.theme.colors["accent"], "#ffcc00");
    assert!(deck.theme.colors.contains_key("bg"));
    assert_eq!(deck.theme.fonts["body"], "Inter");
    assert!(deck.theme.text_styles.contains_key("body"));
    let font = deck.theme.resolve_font("quote", &Font::default());
    assert_eq!((font.size, font.italic, font.weight), (64.0, true, 400));
}

#[test]
fn errors_are_located_and_bad_structures_do_not_panic() {
    for source in [
        "null",
        "[]",
        "1",
        "false",
        "{}",
        r#"{"version":2,"title":"x","slides":[null,1,[]]}"#,
        r#"{"version":3,"title":"x","slides":[]}"#,
    ] {
        assert!(Deck::check(source).is_err(), "{source}");
    }
    let mut doc = value();
    doc["slides"][0]["layout"] = json!("poster");
    doc["slides"][1]["children"][0]["kind"] = json!("script");
    let errors = check(&doc).unwrap_err();
    let has = |path: &str| errors.iter().any(|e| e.path == path);
    assert!(has("$.slides[0]"), "{errors:?}");
    assert!(has("$.slides[1].children[0].kind"), "{errors:?}");
    let mut doc = value();
    doc["slides"][1]["children"][1]["font"] = json!({"size": 0, "weight": 1200, "color": "nope"});
    doc["slides"][1]["children"][1]["style"] = json!("missing");
    let errors = check(&doc).unwrap_err();
    for path in [
        "$.slides[1].children[1].font.size",
        "$.slides[1].children[1].font.weight",
        "$.slides[1].children[1].font.color",
        "$.slides[1].children[1].style",
    ] {
        assert!(errors.iter().any(|e| e.path == path), "{path}: {errors:?}");
    }
    let mut doc = value();
    doc["slides"][0]["children"][0]["opacity"] = json!(2);
    doc["slides"][0]["children"][0]["sizing"] = json!({"w": 10, "h": 10, "min_w": 20, "max_w": 5});
    let errors = check(&doc).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.path == "$.slides[0].children[0].opacity")
    );
    assert!(
        errors
            .iter()
            .any(|e| e.path == "$.slides[0].children[0].sizing")
    );
    let mut doc = value();
    doc["slides"][0]["children"][0]["sizing"] = json!({"w": "wide"});
    let errors = check(&doc).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.path == "$.slides[0].children[0]" && e.message.contains("hug")),
        "{errors:?}"
    );
}

#[test]
fn image_sources_are_confined_or_https() {
    for src in [
        "http://example.com/x.png",
        "../secret.png",
        "/tmp/x.png",
        "assets/../x.png",
        "C:\\secret.png",
        "//host/x.png",
        "assets/%2e%2e/x.png",
        "https://user:pass@example.com/a.png",
    ] {
        let mut doc = value();
        doc["slides"][0]["children"][0] = json!({"kind": "image", "id": "b1", "src": src});
        assert!(
            check(&doc)
                .unwrap_err()
                .iter()
                .any(|e| e.path == "$.slides[0].children[0].src"),
            "{src}"
        );
    }
    for src in [
        "chart.png",
        "assets/chart.png",
        "https://example.com/chart.png",
    ] {
        let mut doc = value();
        doc["slides"][0]["children"][0] = json!({"kind": "image", "src": src});
        assert!(check(&doc).is_ok(), "{src}");
    }
}

#[test]
fn deck_font_files_are_confined_to_assets_fonts() {
    let mut doc = value();
    doc["theme"]["font_files"] = json!([
        {"family": "Brand", "file": "assets/fonts/brand.ttf", "weight": 700, "fallback": ["Inter"]},
        {"family": "Web", "file": "assets/fonts/web.woff2"}
    ]);
    let deck = check(&doc).unwrap();
    assert_eq!(deck.theme.font_files[0].weight, 700);
    assert_eq!(deck.theme.font_files[1].weight, 400);
    for file in [
        "assets/img.png",
        "assets/fonts/../x.ttf",
        "/etc/fonts/a.ttf",
        "assets/fonts/a.exe",
    ] {
        doc["theme"]["font_files"] = json!([{"family": "Brand", "file": file}]);
        assert!(
            check(&doc)
                .unwrap_err()
                .iter()
                .any(|e| e.path == "$.theme.font_files[0].file"),
            "{file}"
        );
    }
    doc["theme"]["font_files"] = json!([{"family": "Bad\"Name", "file": "assets/fonts/a.ttf"}]);
    assert!(check(&doc).is_err());
}

#[test]
fn revision_is_canonical_and_sensitive_to_edits() {
    let a = Deck::check(
        r##"{"title":"x","version":2,"slides":[],"theme":{"colors":{"fg":"#fff","bg":"#000"}}}"##,
    )
    .unwrap();
    let b = Deck::check(
        r##"{"theme":{"colors":{"bg":"#000","fg":"#fff"}},"slides":[],"version":2,"title":"x"}"##,
    )
    .unwrap();
    assert_eq!(a.revision(), b.revision());
    assert_eq!(a.revision().len(), 64);
    let next = apply(
        &starter(),
        &[update(&starter(), "b1", json!({"text": "Changed"}))],
    )
    .unwrap();
    assert_ne!(next.revision(), starter().revision());
}

#[test]
fn limits_are_inclusive() {
    let mut doc = value();
    doc["slides"][0]["children"][0]["text"] = json!("x".repeat(8192));
    assert!(check(&doc).is_ok());
    doc["slides"][0]["children"][0]["text"] = json!("x".repeat(8193));
    assert!(check(&doc).is_err());
    let mut doc = value();
    doc["slides"] = json!(vec![doc["slides"][0].clone(); 200]);
    assert!(check(&doc).is_ok());
    doc["slides"]
        .as_array_mut()
        .unwrap()
        .push(json!({"children": []}));
    assert!(
        check(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.path == "$.slides")
    );
    let mut doc = value();
    doc["slides"][0]["children"] = json!(vec![text("", "x"); 200]);
    assert!(check(&doc).is_ok());
    doc["slides"][0]["children"]
        .as_array_mut()
        .unwrap()
        .push(text("", "x"));
    assert!(
        check(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.path == "$.slides[0].children[200]")
    );
    // Frames nest eight deep and no further.
    let nest = |levels: usize| {
        let mut node = text("leaf", "x");
        for depth in 0..levels {
            node = json!({"id": format!("f{depth}"), "kind": "frame", "children": [node]});
        }
        node
    };
    let mut doc = value();
    doc["slides"][0]["children"] = json!([nest(validate::MAX_DEPTH)]);
    assert!(check(&doc).is_err());
    doc["slides"][0]["children"] = json!([nest(validate::MAX_DEPTH - 1)]);
    assert!(check(&doc).is_ok());
    let base = serde_json::to_string(&starter()).unwrap();
    let at_limit = format!("{base}{}", " ".repeat(MAX_SOURCE - base.len()));
    assert!(Deck::check(&at_limit).is_ok());
    assert_eq!(
        Deck::check(&format!("{at_limit} ")).unwrap_err()[0].path,
        "$"
    );
}

// -- operations -----------------------------------------------------------------

#[test]
fn slide_operations() {
    let deck = starter();
    let slide = Slide::new("s3");
    let added = apply(
        &deck,
        &[Op::AddSlide {
            after: Some("s1".into()),
            slide: slide.clone(),
        }],
    )
    .unwrap();
    assert_eq!(
        added
            .slides
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["s1", "s3", "s2"]
    );
    let appended = apply(&deck, &[Op::AddSlide { after: None, slide }]).unwrap();
    assert_eq!(appended.slides[2].id, "s3");
    let moved = apply(
        &added,
        &[Op::MoveSlide {
            id: "s3".into(),
            to: 0,
        }],
    )
    .unwrap();
    assert_eq!(moved.slides[0].id, "s3");
    let next = apply(
        &moved,
        &[
            op(
                json!({"op": "update_slide", "id": "s3", "fields": {"name": "Intro", "fill": "accent", "transition": {"type": "fade"}}}),
            ),
            Op::SetNotes {
                slide: "s3".into(),
                notes: "Remember the story".into(),
            },
            Op::DeleteSlide { id: "s2".into() },
        ],
    )
    .unwrap();
    assert_eq!(next.slides[0].name.as_deref(), Some("Intro"));
    assert_eq!(next.slides[0].fill.as_deref(), Some("accent"));
    assert_eq!(
        next.slides[0].transition.as_ref().map(|t| t.kind),
        Some(TransitionType::Fade)
    );
    assert_eq!(next.slides[0].notes, "Remember the story");
    assert_eq!(next.slides.len(), 2);
    let cleared = apply(
        &next,
        &[op(
            json!({"op": "update_slide", "id": "s3", "fields": {"fill": null}}),
        )],
    )
    .unwrap();
    assert_eq!(cleared.slides[0].fill, None);
    assert!(
        apply(
            &next,
            &[op(
                json!({"op": "update_slide", "id": "s3", "fields": {"children": []}})
            )]
        )
        .is_err()
    );
}

#[test]
fn node_operations_insert_update_delete_move() {
    let deck = starter();
    let next = apply(
        &deck,
        &[
            op(
                json!({"op": "insert_node", "parent": "s1", "node": {"id": "extra", "kind": "text", "text": "A footnote", "position": {"x": 10, "y": 900}}}),
            ),
            op(
                json!({"op": "insert_node", "parent": "s1", "index": 0, "node": {"id": "wrapper", "kind": "frame", "layout": {"mode": "column", "gap": 8}}}),
            ),
            update(
                &deck,
                "b1",
                json!({"text": "A new title", "position": {"x": 90}}),
            ),
            op(json!({"op": "move_node", "id": "extra", "parent": "wrapper"})),
            op(json!({"op": "delete_node", "id": "b3"})),
        ],
    )
    .unwrap();
    let s1 = &next.slides[0];
    assert_eq!(
        s1.children
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        ["wrapper", "b1"]
    );
    assert_eq!(s1.children[0].children()[0].id, "extra");
    assert_eq!(next.node("b1").unwrap().text(), Some("A new title"));
    assert_eq!(
        next.node("b1").unwrap().position,
        Position::Absolute { x: 90.0, y: 360.0 }
    );
    assert!(next.node("b3").is_none());
    for bad in [
        json!({"op": "move_node", "id": "wrapper", "parent": "extra"}),
        json!({"op": "move_node", "id": "wrapper", "parent": "wrapper"}),
        json!({"op": "insert_node", "parent": "s1", "node": text("b1", "dup")}),
        json!({"op": "insert_node", "parent": "b1", "node": text("z", "in text")}),
        json!({"op": "insert_node", "parent": "s1", "index": 9, "node": text("z", "far")}),
    ] {
        assert!(apply(&next, &[op(bad.clone())]).is_err(), "{bad}");
    }
}

#[test]
fn update_node_is_a_merge_patch() {
    let deck = starter();
    let styled = apply(
        &deck,
        &[update(
            &deck,
            "b2",
            json!({"font": {"color": "accent", "weight": 800}}),
        )],
    )
    .unwrap();
    let again = apply(
        &styled,
        &[update(
            &styled,
            "b2",
            json!({"font": {"weight": 500, "color": null}}),
        )],
    )
    .unwrap();
    let Kind::Text { font, .. } = &again.node("b2").unwrap().kind else {
        panic!()
    };
    assert_eq!(font.size, Some(81.0), "other overrides survive the patch");
    assert_eq!(font.weight, Some(500));
    assert_eq!(font.color, None);
    let sized = apply(
        &again,
        &[update(
            &again,
            "b2",
            json!({"sizing": {"w": "fill"}, "position": "auto"}),
        )],
    )
    .unwrap();
    let node = sized.node("b2").unwrap();
    assert_eq!(
        (node.sizing.w, node.sizing.h),
        (Dim::Fill, Dim::Fixed(180.0))
    );
    assert_eq!(node.position, Position::Auto);
    for fields in [
        json!({"id": "renamed"}),
        json!({"kind": "shape"}),
        json!({"children": []}),
        json!({"arbitrary": 1}),
    ] {
        assert!(apply(&deck, &[update(&deck, "b2", fields)]).is_err());
    }
}

#[test]
fn stale_hashes_fail_but_one_transaction_may_update_a_node_twice() {
    let deck = starter();
    let revision = deck.revision();
    let two = [
        update(&deck, "b1", json!({"text": "new"})),
        update(&deck, "b1", json!({"opacity": 0.5})),
    ];
    let next = apply(&deck, &two).unwrap();
    let node = next.node("b1").unwrap();
    assert_eq!((node.text(), node.opacity), (Some("new"), 0.5));
    let forged = Op::UpdateNode {
        id: "b1".into(),
        fields: json!({"text": "x"}).as_object().unwrap().clone(),
        base_hash: "0".repeat(64),
    };
    assert!(
        apply(&deck, &[forged])
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    // A node changed since the caller read it is refused.
    let changed = apply(&deck, &[update(&deck, "b1", json!({"text": "elsewhere"}))]).unwrap();
    let late = update(&deck, "b1", json!({"text": "late"}));
    assert!(
        apply(&changed, &[late])
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    for bad in [
        Op::DeleteSlide {
            id: "missing".into(),
        },
        Op::MoveSlide {
            id: "s1".into(),
            to: 99,
        },
        op(json!({"op": "delete_node", "id": "missing"})),
        op(json!({"op": "set_layout", "target": "missing", "layout": {"mode": "row"}})),
        op(json!({"op": "wrap", "ids": ["b1", "b2"], "layout": {"mode": "row"}})),
        op(json!({"op": "unwrap", "id": "b1"})),
    ] {
        assert!(
            apply(
                &deck,
                &[update(&deck, "b1", json!({"text": "not committed"})), bad]
            )
            .is_err()
        );
        assert_eq!(deck.revision(), revision);
    }
}

#[test]
fn set_layout_converts_children_like_figma() {
    let deck = starter();
    let column = apply(
        &deck,
        &[op(
            json!({"op": "set_layout", "target": "s2", "layout": {"mode": "column", "gap": 20, "padding": 40}}),
        )],
    )
    .unwrap();
    let s2 = &column.slides[1];
    assert_eq!(s2.layout.mode, Mode::Column);
    assert!(s2.children.iter().all(|n| n.position == Position::Auto));
    let rects = layout_map(&column, s2, None);
    assert_eq!(rects["b2"].x, 40.0);
    assert_eq!(rects["b2"].y, 40.0);
    assert_eq!(
        rects["b3"].y,
        40.0 + 180.0 + 20.0,
        "gap between column children"
    );
    // Back to none: children keep the place they had.
    let back = apply(
        &column,
        &[op(
            json!({"op": "set_layout", "target": "s2", "layout": {"mode": "none"}}),
        )],
    )
    .unwrap();
    let again = layout_map(&back, &back.slides[1], None);
    assert_eq!(again["b2"], rects["b2"]);
    assert_eq!(again["b3"], rects["b3"]);
    assert!(matches!(
        back.node("b3").unwrap().position,
        Position::Absolute { .. }
    ));
}

#[test]
fn wrap_and_unwrap_keep_the_picture() {
    let deck = starter();
    let before = layout_map(&deck, &deck.slides[1], None);
    let wrapped = apply(
        &deck,
        &[op(
            json!({"op": "wrap", "ids": ["b2", "b3"], "layout": {"mode": "column", "gap": 10}, "id": "stack"}),
        )],
    )
    .unwrap();
    let stack = wrapped.node("stack").unwrap();
    assert_eq!(stack.children().len(), 2);
    assert!(
        stack
            .children()
            .iter()
            .all(|n| n.position == Position::Auto)
    );
    assert_eq!(stack.position, Position::Absolute { x: 120.0, y: 120.0 });
    let rects = layout_map(&wrapped, &wrapped.slides[1], None);
    assert_eq!(rects["stack"].x, 120.0);
    assert_eq!(
        rects["stack"].w, 1680.0,
        "hug width follows the widest child"
    );
    assert_eq!(rects["b3"].y, rects["b2"].y + rects["b2"].h + 10.0);
    // A free frame keeps the exact positions.
    let free = apply(
        &deck,
        &[op(
            json!({"op": "wrap", "ids": ["b3", "b2"], "layout": {"mode": "none"}}),
        )],
    )
    .unwrap();
    let rects = layout_map(&free, &free.slides[1], None);
    assert_eq!(rects["b2"], before["b2"]);
    assert_eq!(rects["b3"], before["b3"]);
    let unwrapped = apply(&free, &[op(json!({"op": "unwrap", "id": "frame_1"}))]).unwrap();
    assert!(unwrapped.node("frame_1").is_none());
    let rects = layout_map(&unwrapped, &unwrapped.slides[1], None);
    assert_eq!(rects["b2"], before["b2"]);
    assert_eq!(rects["b3"], before["b3"]);
    assert!(
        apply(
            &deck,
            &[op(
                json!({"op": "wrap", "ids": [], "layout": {"mode": "row"}})
            )]
        )
        .is_err()
    );
}

#[test]
fn text_styles_and_theme_operations() {
    let deck = starter();
    let next = apply(
        &deck,
        &[
            op(json!({"op": "set_text_style", "name": "body", "font": {"size": 50, "weight": 500}})),
            op(json!({"op": "set_text_style", "name": "quote", "font": {"italic": true}})),
        ],
    )
    .unwrap();
    let body = next.theme.resolve_font("body", &Font::default());
    assert_eq!(
        (body.size, body.weight, body.line_height),
        (50.0, 500, 1.35)
    );
    assert!(next.theme.resolve_font("quote", &Font::default()).italic);
    assert!(
        apply(
            &deck,
            &[op(
                json!({"op": "set_text_style", "name": "Bad Name", "font": {}})
            )]
        )
        .is_err()
    );
    let mut theme = Theme::default();
    theme.colors.insert("accent".into(), "#ff0000".into());
    assert_eq!(
        apply(
            &deck,
            &[Op::SetTheme {
                theme: theme.clone()
            }]
        )
        .unwrap()
        .theme,
        theme
    );
}

#[test]
fn deck_constructors_are_valid_and_laid_out() {
    for deck in [Deck::titled("Hello"), Deck::new_from_layouts("Hello")] {
        assert_eq!(deck.version, 2);
        for slide in &deck.slides {
            let rects = layout(&deck, slide);
            assert_eq!(rects.len(), slide.nodes().len());
            assert!(
                rects.iter().all(|(_, r)| r.w > 0.0 && r.h > 0.0),
                "{rects:?}"
            );
        }
    }
    assert_eq!(Deck::titled("Hello").slides[0].children.len(), 1);
}

// -- raster images ---------------------------------------------------------------

#[test]
fn renders_draw_photos_not_an_empty_spot() {
    // A photo the model placed must show in the render it sees; without
    // resvg's raster decoding the spot came back as plain background, and
    // models rebuilt the picture out of shapes.
    let dir = std::env::temp_dir().join(format!("nexus-raster-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let photo = image::RgbImage::from_pixel(8, 8, image::Rgb([220, 20, 30]));
    photo.save(dir.join("photo.jpg")).unwrap();
    let mut doc = value();
    doc["slides"][0]["fill"] = json!("#ffffff");
    doc["slides"][0]["layout"] = json!({"mode": "none"});
    doc["slides"][0]["children"] = json!([{
        "id": "im", "kind": "image", "src": "photo.jpg", "fit": "cover",
        "position": {"x": 0, "y": 0}, "sizing": {"w": 1920, "h": 1080}
    }]);
    let deck = check(&doc).unwrap();
    let id = deck.slides[0].id.clone();
    let png = render::render_slide_at(&deck, &id, 320, Some(&dir)).unwrap();
    let pixel = *image::load_from_memory(&png)
        .unwrap()
        .to_rgba8()
        .get_pixel(160, 90);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80,
        "the photo is drawn: {pixel:?}"
    );
}
