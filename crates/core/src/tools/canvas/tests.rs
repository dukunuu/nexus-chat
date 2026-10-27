//! Hermetic public canvas workflows, independent of the legacy deck callers.
use super::tests::Fixture;
use super::*;
use crate::doc::{self, Doc, Kind as BlockKind};

async fn call(f: &Fixture, action: &str, mut v: Value) -> String {
    v["action"] = json!(action);
    f.tools.run("canvas", &v.to_string()).await.0
}
fn doc(f: &Fixture) -> Doc {
    doc::storage::load(&f.tools.space_files_dir, "docs/report.doc.json").unwrap()
}
async fn create(f: &Fixture) -> String {
    call(f, "create", json!({"kind":"doc","markdown":"# Report\n\nOpening.\n\n## Goals\n\nShip **carefully**.\n\n## Next\n\nLater."})).await
}

#[tokio::test]
async fn docs_create_read_edit_rebase_comments_history_and_export() {
    let f = Fixture::new();
    let created = create(&f).await;
    assert!(
        created.starts_with("created doc · docs/report.doc.json · 6 blocks"),
        "{created}"
    );
    assert!(created.contains("doc schema (sent once per session)"));
    let base = doc(&f);
    assert_eq!(base.title, "Report");
    let heading = &base.blocks[2].id;
    let paragraph = &base.blocks[3].id;
    let read = call(
        &f,
        "read",
        json!({"path":"docs/report.doc.json","blocks":[heading]}),
    )
    .await;
    assert!(read.contains(&format!("revision {}", short(&base.revision()))));
    assert!(read.contains("Ship **carefully**."), "{read}");
    let full = doc::read_blocks(&base, std::slice::from_ref(heading));
    assert!(!full.contains("Later."));
    assert!(!read.contains("doc schema (sent once per session)"));
    let edited = call(&f, "edit", json!({"path":"docs/report.doc.json","revision":base.revision(),"ops":[{"op":"replace_text","id":paragraph,"find":"carefully","replace":"confidently"}]})).await;
    assert!(
        edited.contains("- replace_text") && edited.contains("Ship **confidently**."),
        "{edited}"
    );
    let rebased = call(&f, "edit", json!({"path":"docs/report.doc.json","revision":base.revision(),"ops":[{"op":"replace_text","id":base.blocks[1].id,"find":"Opening","replace":"Introduction"}]})).await;
    assert!(rebased.contains("rebased:"), "{rebased}");
    let current = doc(&f).revision();
    let refused = call(&f, "edit", json!({"path":"docs/report.doc.json","revision":base.revision(),"ops":[{"op":"replace_text","id":paragraph,"find":"confidently","replace":"quickly"}]})).await;
    assert!(
        refused.contains("stale revision")
            && refused.contains("Nothing was written")
            && refused.contains(paragraph),
        "{refused}"
    );
    assert_eq!(doc(&f).revision(), current);
    let commented = call(&f, "comment", json!({"path":"docs/report.doc.json","block":paragraph,"quote":"confidently","text":"Keep the original caution; rollout is still gated.","severity":"should","ops":[{"op":"replace_text","id":paragraph,"find":"confidently","replace":"carefully"}]})).await;
    assert!(commented.starts_with("commented on"), "{commented}");
    let path = f.file("docs/report.doc.json");
    let threads = doc::feedback::load(&path).unwrap();
    let thread = &threads[0];
    assert_eq!(
        thread.anchor.range,
        Some(doc::feedback::Range { start: 5, end: 16 })
    );
    assert_eq!(thread.messages[0].edits.len(), 1);
    assert_eq!(
        doc(&f).revision(),
        current,
        "suggestions must not apply directly"
    );
    let read = call(&f, "read", json!({"path":"docs/report.doc.json"})).await;
    assert!(
        read.contains(&thread.id) && read.contains("confidently") && read.contains("suggestion")
    );
    let reply = call(&f, "comment", json!({"path":"docs/report.doc.json","thread":thread.id,"text":"Agreed; this is a suggestion, not an applied edit."})).await;
    assert!(reply.starts_with("replied in thread"));
    let resolved = call(
        &f,
        "resolve",
        json!({"path":"docs/report.doc.json","thread":thread.id}),
    )
    .await;
    assert!(resolved.starts_with("resolved thread"));
    assert!(doc::feedback::load(&path).unwrap()[0].resolved.is_some());
    let history = call(&f, "history", json!({"path":"docs/report.doc.json"})).await;
    assert!(history.contains(&base.revision()[..8]), "{history}");
    let restored = call(
        &f,
        "history",
        json!({"path":"docs/report.doc.json","revision":base.revision()}),
    )
    .await;
    assert!(restored.starts_with("restored"), "{restored}");
    assert_eq!(doc(&f), base);
    let exported = call(
        &f,
        "export",
        json!({"path":"docs/report.doc.json","format":"md"}),
    )
    .await;
    assert!(exported.contains("exports/report.md"), "{exported}");
    let markdown = std::fs::read_to_string(f.file("exports/report.md")).unwrap();
    assert!(markdown.contains("Ship **carefully**.") && !markdown.contains("nexus-block"));
    let again = call(
        &f,
        "export",
        json!({"path":"docs/report.doc.json","format":"md"}),
    )
    .await;
    assert!(again.contains("report (2).md"), "{again}");
    for (action, fields) in [("view", json!({})), ("export", json!({"format":"pdf"}))] {
        let mut v = fields;
        v["path"] = json!("docs/report.doc.json");
        let result = call(&f, action, v).await;
        assert_eq!(result.lines().count(), 1, "{result}");
    }
}

#[tokio::test]
async fn markdown_file_import_and_independent_deferred_references() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.file("")).unwrap();
    std::fs::write(f.file("notes.md"), "Intro.\n\n# Report\n\nText.").unwrap();
    let created = call(&f, "create", json!({"kind":"doc","from":"notes.md"})).await;
    assert!(
        created.contains("created doc · docs/report.doc.json"),
        "{created}"
    );
    assert_eq!(doc(&f).title, "Report");
    assert!(matches!(doc(&f).blocks[1].kind, BlockKind::Title { .. }));
    assert!(created.contains("doc schema"));
    let deck = f.run("create", json!({"title":"Slides"})).await;
    assert!(deck.contains("schema (sent once per session)"), "{deck}");
    let fresh = Fixture::new();
    fresh.tools.deck_link.lock().unwrap().doc_schema_shown = false;
    assert!(fresh.tools.doc_schema_if_fits(7900).is_empty());
    assert!(!fresh.tools.deck_link.lock().unwrap().doc_schema_shown);
    assert!(!fresh.tools.doc_schema_if_fits(0).is_empty());
    assert!(fresh.tools.doc_schema_if_fits(0).is_empty());
}

#[tokio::test]
async fn canvas_alias_read_only_batch_and_research_exclusion() {
    let mut f = Fixture::new();
    let names = f
        .tools
        .defs()
        .into_iter()
        .map(|d| d.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"canvas".into()) && !names.contains(&"deck".into()));
    create(&f).await;
    let args = json!({"action":"read","path":"docs/report.doc.json"}).to_string();
    assert_eq!(
        f.tools.run("deck", &args).await,
        f.tools.run("canvas", &args).await
    );
    for action in ["read", "view", "fonts"] {
        assert!(super::super::is_read_only_tool(
            "canvas",
            &json!({"action":action}).to_string()
        ));
    }
    for op in ["list", "read", "edit", "set_default"] {
        assert_eq!(
            super::super::is_read_only_tool(
                "canvas",
                &json!({"action":"design","op":op}).to_string()
            ),
            matches!(op, "list" | "read")
        );
    }
    let batch = f.tools.run("batch", &json!({"calls":[{"tool":"canvas","arguments":{"action":"read","path":"docs/report.doc.json"}}]}).to_string()).await.0;
    assert!(
        batch.contains("doc · docs/report.doc.json") && batch.contains("[1/1]"),
        "{batch}"
    );
    f.tools.research_only = true;
    assert!(
        !f.tools
            .defs()
            .iter()
            .any(|d| d.name == "canvas" || d.name == "deck")
    );
}

#[tokio::test]
async fn designs_layouts_and_drift_are_visible_through_canvas() {
    let f = Fixture::new();
    let listed = call(&f, "design", json!({"op":"list"})).await;
    assert!(
        listed.contains("nexus · built-in · default") && listed.contains("templates: none yet"),
        "{listed}"
    );
    let read = call(&f, "design", json!({"op":"read"})).await;
    let value: Value = serde_json::from_str(read.split_once('\n').unwrap().1).unwrap();
    assert_eq!(
        value["slide"]["layouts"]["big_number"],
        json!(["body", "number", "title"])
    );
    assert!(value["doc"]["text_styles"]["body"].is_object());
    assert!(
        read.chars().count() < 8000,
        "design reference must fit the tool-result cap"
    );
    let mut base = crate::design::Design {
        name: "Brand display name".into(),
        ..crate::design::Design::default()
    };
    base.slide
        .text_styles
        .insert("brand_copy".into(), base.slide.text_styles["body"].clone());
    crate::design::save(&f.tools.space_files_dir, "brand", &base).unwrap();
    let edited = call(&f, "design", json!({"op":"edit","design":"brand","patch":{"colors":{"accent":"#124578"},"doc":{"text_styles":{"body":{"size":12}}}}})).await;
    assert!(
        edited.starts_with("edited design brand · revision"),
        "{edited}"
    );
    let brand = crate::design::load(&f.tools.space_files_dir, "brand").unwrap();
    assert_eq!(brand.colors["accent"], "#124578");
    assert_eq!(brand.doc.text_styles["body"].size, Some(12.0));
    assert_eq!(
        call(&f, "design", json!({"op":"set_default","design":"brand"})).await,
        "default design: brand\n"
    );
    let created = call(&f, "create", json!({"title":"Branded", "slides":[{"children":[{"id":"custom","kind":"text","style":"brand_copy","text":"Custom design style"}]}]})).await;
    assert!(created.starts_with("created deck"), "{created}");
    assert!(
        !created.lines().any(|l| l.starts_with("design:")),
        "{created}"
    );
    let path = f.file("decks/branded.deck.json");
    let deck = load(&path).unwrap();
    assert_eq!(deck.design.as_deref(), Some("brand"));
    assert_eq!(deck.design_rev.as_deref(), Some(brand.revision().as_str()));
    assert_eq!(deck.theme, brand.slide_theme());
    assert!(
        matches!(&deck.node("custom").unwrap().kind, Kind::Text { style, .. } if style == "brand_copy")
    );
    assert!(!f.tools.deck_link.lock().unwrap().changed.is_empty());
    let added = call(&f,"edit",json!({"path":"decks/branded.deck.json","ops":[{"op":"add_slide","layout":"big_number","slots":{"number":"42%","title":"Growth","body":"Measured, not invented."}}]})).await;
    assert!(
        added.contains("add_slide") && !added.lines().any(|l| l.starts_with("design:")),
        "{added}"
    );
    let deck = load(&path).unwrap();
    let number = deck
        .slides
        .last()
        .unwrap()
        .nodes()
        .into_iter()
        .find(|n| matches!(&n.kind, Kind::Text { text, .. } if text == "42%"))
        .unwrap()
        .id
        .clone();
    let drift = call(&f,"edit",json!({"path":"decks/branded.deck.json","ops":[{"op":"update_node","id":number,"fields":{"font":{"color":"#123456"}}}]})).await;
    assert!(
        drift.lines().last().unwrap().starts_with("design:"),
        "{drift}"
    );
    let fixed = call(&f,"edit",json!({"path":"decks/branded.deck.json","ops":[{"op":"update_node","id":number,"fields":{"font":{"color":"accent"}}}]})).await;
    assert!(!fixed.lines().any(|l| l.starts_with("design:")), "{fixed}");
    let added_drift = call(&f, "edit", json!({"path":"decks/branded.deck.json","ops":[{"op":"add_slide","slide":{"children":[{"kind":"text","text":"Off-system","font":{"size":57,"color":"#123456"}}]}}]})).await;
    assert!(
        added_drift.contains("add_slide")
            && added_drift.lines().last().unwrap().starts_with("design:"),
        "{added_drift}"
    );
    let doc_created = create(&f).await;
    assert!(doc_created.starts_with("created doc"));
    assert_eq!(doc(&f).design.as_deref(), Some("brand"));
}

#[tokio::test]
async fn markdown_export_flattens_all_internal_extensions_and_copies_images() {
    let f = Fixture::new();
    create(&f).await;
    std::fs::create_dir_all(f.file("docs/assets")).unwrap();
    image::RgbImage::new(2, 2)
        .save(f.file("docs/assets/picture.png"))
        .unwrap();
    let edited = call(&f,"edit",json!({"path":"docs/report.doc.json","ops":[{"op":"insert_blocks","blocks":[
        {"kind":"toc"},{"kind":"page_break"},
        {"kind":"paragraph","text":"Styled prose","style":"caption"},
        {"kind":"image","src":"assets/picture.png","width":"inline","alt":"Picture","caption":"Caption"},
        {"kind":"table","header":false,"rows":[["A","B"]]},
        {"kind":"chart","chart":{"chart":"bar","labels":["Q1"],"series":[{"name":"Revenue","values":[42]}]}}
    ]}]})).await;
    assert!(edited.starts_with("edited doc"), "{edited}");
    let result = call(
        &f,
        "export",
        json!({"path":"docs/report.doc.json","format":"md"}),
    )
    .await;
    assert!(result.starts_with("exported"), "{result}");
    let output = std::fs::read_to_string(f.file("exports/report.md")).unwrap();
    assert!(
        !output.contains("nexus-block") && !output.contains("page_break"),
        "{output}"
    );
    for expected in [
        "- [Goals](#goals)",
        "Styled prose",
        "![Picture](assets/",
        "\"Caption\")",
        "|  |  |",
        "| Q1 | 42 |",
        "| Label | Revenue |",
    ] {
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
    assert_eq!(
        std::fs::read_dir(f.file("exports/assets")).unwrap().count(),
        1
    );
}

#[test]
fn revised_layout_previews() {
    let design = crate::design::Design::default();
    assert!((280.0..=320.0).contains(&design.slide.text_styles["display"].size.unwrap()));
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/layout-previews/p1b");
    let assets =
        std::env::temp_dir().join(format!("nexus-canvas-preview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&assets).unwrap();
    image::RgbImage::from_fn(640, 640, |x, y| {
        if x.abs_diff(320).pow(2) + y.abs_diff(320).pow(2) < 180_u32.pow(2) {
            image::Rgb([174, 89, 62])
        } else {
            image::Rgb([40, 41, 35])
        }
    })
    .save(assets.join("sample.png"))
    .unwrap();
    for (name, layout) in &design.slide.layouts {
        let slots = crate::design::layouts::slots(layout).into_iter().map(|slot| {
            let value = match slot.as_str() {
                "number" if name == "section" => "02", "number" => "42%",
                "image" => "assets/sample.png", "title" => "Make room for what matters.",
                "kicker" => "NEXUS / A NEW PERSPECTIVE", "subtitle" => "One source. A clearer story.",
                "quote" => "Clarity is the space between the things we choose to say.", "attribution" => "The design team",
                "left_title"=>"Today", "right_title"=>"Tomorrow",
                "items"=>"1. Start with the question\n2. Follow the evidence\n3. Make the decision",
                "left"=>"- A shared foundation\n- Deliberate choices\n- Room to grow",
                "right"=>"- A consistent voice\n- Fewer distractions\n- More possibility",
                "caption"=>"A shared foundation, built to last.",
                _=>"Start with the story. Keep what is essential, and leave room for the next idea.",
            }; (slot,value.to_string())
        }).collect::<BTreeMap<_,_>>();
        let base = Deck::from_design(name, &design);
        let next = direct::prepare(
            &base,
            vec![json!({"op":"add_slide","layout":name,"slots":slots})],
        )
        .unwrap()
        .next;
        assert!(
            crate::design::lint_deck(&next, &design).is_empty(),
            "{name}"
        );
        let png = render::render_slide_at(&next, &next.slides[0].id, 640, Some(&assets)).unwrap();
        if std::env::var_os("NEXUS_LAYOUT_PREVIEWS").is_some() {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(format!("{name}.png")), png).unwrap();
        }
    }
    std::fs::remove_dir_all(assets).unwrap();
}

#[tokio::test]
async fn doc_creates_and_edits_tell_the_terminal_which_block_to_show() {
    let f = Fixture::new();
    create(&f).await;
    let path = f.file("docs/report.doc.json");
    let changed = std::mem::take(&mut f.tools.deck_link.lock().unwrap().changed);
    assert_eq!(changed.len(), 1);
    assert!(crate::deck::same_file(&changed[0].0, &path));
    let base = doc(&f);
    assert_eq!(
        changed[0].1, base.blocks[0].id,
        "a new doc opens on its first block"
    );
    let paragraph = base.blocks[3].id.clone();
    call(&f, "edit", json!({"path":"docs/report.doc.json","ops":[{"op":"replace_text","id":paragraph,"find":"carefully","replace":"calmly"}]})).await;
    let changed = std::mem::take(&mut f.tools.deck_link.lock().unwrap().changed);
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed[0].1, paragraph,
        "an edit opens on the first changed block"
    );
    // A change that touches no block (the title) still opens the doc, on its first block.
    call(
        &f,
        "edit",
        json!({"path":"docs/report.doc.json","ops":[{"op":"set_title","title":"Renamed"}]}),
    )
    .await;
    let changed = std::mem::take(&mut f.tools.deck_link.lock().unwrap().changed);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].1, base.blocks[0].id);
    // A refused edit tells nobody.
    call(
        &f,
        "edit",
        json!({"path":"docs/report.doc.json","ops":[{"op":"delete_blocks","ids":["nope"]}]}),
    )
    .await;
    assert!(f.tools.deck_link.lock().unwrap().changed.is_empty());
}
