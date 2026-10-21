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
fn update(deck: &Deck, fields: Value) -> Op {
    Op::UpdateBlock {
        slide: "s1".into(),
        id: "b1".into(),
        fields: fields.as_object().unwrap().clone(),
        base_hash: deck.slides[0].blocks[0].hash(),
    }
}

#[test]
fn round_trip_all_block_kinds() {
    let mut doc = value();
    let payloads = [
        json!({"kind":"image", "src":"assets/chart.png", "fit":"cover"}),
        json!({"kind":"shape", "shape":"ellipse", "fill":"accent", "stroke":"fg"}),
        json!({"kind":"table", "rows":[["Name", "Value"], ["Revenue", "18%"]]}),
        json!({"kind":"code", "language":"rust", "source":"fn main() {}"}),
        json!({"kind":"chart", "chart":"line", "labels":["Q1", "Q2"], "series":[{"name":"Growth", "values":[2.5, 4.0]}]}),
    ];
    for mut payload in payloads {
        for (k, v) in json!({"x":2,"y":3,"w":12,"h":8}).as_object().unwrap() {
            payload[k] = v.clone();
        }
        doc["slides"][0]["blocks"]
            .as_array_mut()
            .unwrap()
            .push(payload);
    }
    let deck = check(&doc).unwrap();
    assert_eq!(
        Deck::check(&serde_json::to_string_pretty(&deck).unwrap()).unwrap(),
        deck
    );
    assert_eq!(deck.slides[0].blocks.len(), 6);
}

#[test]
fn fills_ids_preserving_existing_ids_and_clamps() {
    let mut doc = value();
    doc.as_object_mut().unwrap().remove("theme");
    doc["slides"][0]["id"] = json!("");
    doc["slides"][1]["id"] = json!("b1");
    doc["slides"][0]["blocks"][0]["x"] = json!(-50);
    doc["slides"][0]["blocks"][0]["y"] = json!(99);
    doc["slides"][0]["blocks"][0]["w"] = json!(100);
    doc["slides"][0]["blocks"][0]["h"] = json!(-2);
    doc["slides"][1]["blocks"][0]
        .as_object_mut()
        .unwrap()
        .remove("id");
    let deck = check(&doc).unwrap();
    let b = &deck.slides[0].blocks[0];
    assert_eq!((b.x, b.y, b.w, b.h), (0, 17, 32, 1));
    let ids: Vec<_> = deck
        .slides
        .iter()
        .flat_map(|s| std::iter::once(&s.id).chain(s.blocks.iter().map(|b| &b.id)))
        .collect();
    assert_eq!(ids.len(), ids.iter().collect::<BTreeSet<_>>().len());
    assert!(ids.iter().all(|id| !id.is_empty()));
    assert_eq!(check(&doc).unwrap(), deck);
    assert_eq!(deck.theme, Theme::default());
}

#[test]
fn partial_theme_fills_base_tokens() {
    let mut doc = value();
    doc["theme"] = json!({"colors":{"accent":"#ffcc00", "custom":"#123"}});
    doc["slides"][0]["blocks"][0]["style"]["color"] = json!("custom");
    let deck = check(&doc).unwrap();
    assert_eq!(deck.theme.colors["accent"], "#ffcc00");
    assert!(deck.theme.colors.contains_key("bg"));
    assert_eq!(deck.theme.fonts["body"], "Inter");
}

#[test]
fn errors_are_located_and_bad_structures_do_not_panic() {
    for source in [
        "null",
        "[]",
        "1",
        "false",
        "{}",
        r#"{"version":1,"title":"x","slides":[null,1,[]]}"#,
    ] {
        assert!(Deck::check(source).is_err(), "{source}");
    }
    let mut doc = value();
    doc["slides"][0]["layout"] = json!("poster");
    doc["slides"][1]["blocks"][0]["kind"] = json!("script");
    let errors = check(&doc).unwrap_err();
    assert!(errors.iter().any(|e| e.path == "$.slides[0].layout"));
    assert!(
        errors
            .iter()
            .any(|e| e.path == "$.slides[1].blocks[0].kind")
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
        doc["slides"][0]["blocks"][0] =
            json!({"kind":"image", "id":"b1", "x":0,"y":0,"w":2,"h":2,"src":src});
        assert!(
            check(&doc)
                .unwrap_err()
                .iter()
                .any(|e| e.path == "$.slides[0].blocks[0].src"),
            "{src}"
        );
    }
    for src in [
        "chart.png",
        "assets/chart.png",
        "https://example.com/chart.png",
    ] {
        let mut doc = value();
        doc["slides"][0]["blocks"][0] = json!({"kind":"image", "x":0,"y":0,"w":2,"h":2,"src":src});
        assert!(check(&doc).is_ok(), "{src}");
    }
}

#[test]
fn revision_is_canonical_and_sensitive_to_edits() {
    let a = Deck::check(
        r##"{"title":"x","version":1,"slides":[],"theme":{"colors":{"fg":"#fff","bg":"#000"}}}"##,
    )
    .unwrap();
    let b = Deck::check(
        r##"{"theme":{"colors":{"bg":"#000","fg":"#fff"}},"slides":[],"version":1,"title":"x"}"##,
    )
    .unwrap();
    assert_eq!(a.revision(), b.revision());
    assert_eq!(a.revision().len(), 64);
    let next = apply(&starter(), &[update(&starter(), json!({"text":"Changed"}))]).unwrap();
    assert_ne!(next.revision(), starter().revision());
}

#[test]
fn slide_operations() {
    let deck = starter();
    let slide = Slide {
        id: "s3".into(),
        layout: Layout::Blank,
        notes: String::new(),
        blocks: vec![],
    };
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
            Op::SetLayout {
                slide: "s3".into(),
                layout: Layout::Quote,
            },
            Op::SetNotes {
                slide: "s3".into(),
                notes: "Remember the story".into(),
            },
            Op::DeleteSlide { id: "s2".into() },
        ],
    )
    .unwrap();
    assert_eq!(next.slides[0].layout, Layout::Quote);
    assert_eq!(next.slides[0].notes, "Remember the story");
    assert_eq!(next.slides.len(), 2);
}

#[test]
fn block_operations_and_style_targets() {
    let deck = starter();
    let block = text_block("extra", "A footnote", (2, 16, 28, 2), TextSize::S);
    let next = apply(
        &deck,
        &[
            Op::InsertBlock {
                slide: "s1".into(),
                block,
            },
            update(&deck, json!({"x":-2,"text":"A new title"})),
            Op::Restyle {
                target: "extra".into(),
                style: Style {
                    color: Some("accent".into()),
                    ..Style::default()
                },
            },
            Op::DeleteBlock {
                slide: "s2".into(),
                id: "b3".into(),
            },
        ],
    )
    .unwrap();
    assert_eq!(next.slides[0].blocks.len(), 2);
    assert_eq!(next.slides[0].blocks[0].x, 0);
    assert_eq!(
        next.slides[0].blocks[0].content,
        Content::Text {
            text: "A new title".into()
        }
    );
    assert_eq!(
        next.slides[0].blocks[1].style.color.as_deref(),
        Some("accent")
    );
    assert_eq!(next.slides[1].blocks.len(), 1);
    let style = Style {
        size: TextSize::Xl,
        ..Style::default()
    };
    for target in ["s1", "deck"] {
        let styled = apply(
            &next,
            &[Op::Restyle {
                target: target.into(),
                style: style.clone(),
            }],
        )
        .unwrap();
        assert!(styled.slides[0].blocks.iter().all(|b| b.style == style));
    }
    let mut theme = Theme::default();
    theme.colors.insert("accent".into(), "#ff0000".into());
    assert_eq!(
        apply(
            &next,
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
fn failed_transactions_are_atomic_and_stale_updates_fail() {
    let deck = starter();
    let revision = deck.revision();
    let ops = [
        update(&deck, json!({"text":"new"})),
        update(&deck, json!({"x":3})),
    ];
    assert!(
        apply(&deck, &ops)
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    assert_eq!(deck.revision(), revision);
    assert!(apply(&deck, &[update(&deck, json!({"id":"renamed"}))]).is_err());
    assert!(apply(&deck, &[update(&deck, json!({"arbitrary":"bad"}))]).is_err());
    for op in [
        Op::DeleteSlide {
            id: "missing".into(),
        },
        Op::MoveSlide {
            id: "s1".into(),
            to: 99,
        },
        Op::DeleteBlock {
            slide: "s1".into(),
            id: "missing".into(),
        },
        Op::Restyle {
            target: "missing".into(),
            style: Style::default(),
        },
    ] {
        assert!(apply(&deck, &[update(&deck, json!({"text":"not committed"})), op]).is_err());
        assert_eq!(deck.revision(), revision);
    }
}

#[test]
fn limits_are_inclusive() {
    let mut doc = value();
    doc["slides"][0]["blocks"][0]["text"] = json!("x".repeat(8192));
    assert!(check(&doc).is_ok());
    doc["slides"][0]["blocks"][0]["text"] = json!("x".repeat(8193));
    assert!(check(&doc).is_err());
    let mut doc = value();
    doc["slides"] = json!(vec![doc["slides"][0].clone(); 200]);
    assert!(check(&doc).is_ok());
    doc["slides"]
        .as_array_mut()
        .unwrap()
        .push(json!({"blocks":[]}));
    assert!(
        check(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.path == "$.slides")
    );
    let mut doc = value();
    doc["slides"][0]["blocks"] = json!(vec![doc["slides"][0]["blocks"][0].clone(); 40]);
    assert!(check(&doc).is_ok());
    doc["slides"][0]["blocks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kind":"text","x":0,"y":0,"w":1,"h":1,"text":"x"}));
    assert!(
        check(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.path == "$.slides[0].blocks")
    );
    let base = serde_json::to_string(&starter()).unwrap();
    let at_limit = format!("{base}{}", " ".repeat(MAX_SOURCE - base.len()));
    assert!(Deck::check(&at_limit).is_ok());
    assert_eq!(
        Deck::check(&format!("{at_limit} ")).unwrap_err()[0].path,
        "$"
    );
}
