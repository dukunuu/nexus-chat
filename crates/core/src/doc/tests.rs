use super::*;
use crate::design::Design;
use serde_json::{Value, json};

fn fixture() -> Doc {
    Doc::check(&json!({"version":1,"title":"Plan","blocks":[
        {"id":"b1","kind":"title","text":"Plan"},
        {"id":"b2","kind":"heading","level":1,"text":"Goals"},
        {"id":"b3","kind":"paragraph","text":"Ship **the docs**. Q4 Q4."},
        {"id":"b4","kind":"list","ordered":false,"items":[{"text":"One"},{"text":"Two","children":[{"text":"Child"}]}]}
    ]}).to_string()).unwrap()
}
fn edit(doc: &Doc, raw: Value) -> Doc {
    direct::prepare(doc, raw.as_array().unwrap().clone())
        .unwrap()
        .next
}
fn error(doc: &Doc, raw: Value, path: &str) -> String {
    let errors = direct::prepare(doc, vec![raw]).err().unwrap();
    assert!(errors[0].path.contains(path), "{errors:?}");
    assert!(errors[0].message.contains("fix:"), "{errors:?}");
    errors[0].message.clone()
}
fn temp() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("nexus-doc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn schema_round_trip_includes_every_block_kind_and_revisions_are_canonical() {
    let source = json!({"version":1,"title":"All","blocks":[
        {"kind":"title","text":"Title"}, {"kind":"heading","level":2,"text":"Heading"},
        {"kind":"paragraph","text":"**bold**, *italic*, `code`, [link](https://example.com), ~~strike~~ [^1]"},
        {"kind":"quote","text":"Quote"}, {"kind":"callout","tone":"info","text":"Info"},
        {"kind":"code","lang":"sql","text":"select 1"},
        {"kind":"list","ordered":true,"items":[{"text":"Item","checked":true,"children":[{"text":"Nested"}]}]},
        {"kind":"table","header":true,"align":["left","right"],"rows":[["A","B"],["1","2"]]},
        {"kind":"image","src":"assets/a.png","alt":"A","caption":"Figure 1","width":"wide"},
        {"kind":"chart","chart":{"chart":"bar","labels":["A"],"series":[{"name":"Sales","values":[1]}]}},
        {"kind":"divider"}, {"kind":"page_break"}, {"kind":"toc"}
    ],"footnotes":{"1":"A source"}});
    let doc = Doc::check(&source.to_string()).unwrap();
    assert_eq!(
        Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap(),
        doc
    );
    assert_eq!(doc.blocks.len(), 13);
    let mut reordered = source
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<serde_json::Map<_, _>>();
    assert_eq!(
        Doc::check(&Value::Object(reordered.clone()).to_string())
            .unwrap()
            .revision(),
        doc.revision()
    );
    reordered.insert("title".into(), json!("Changed"));
    assert_ne!(
        Doc::check(&Value::Object(reordered).to_string())
            .unwrap()
            .revision(),
        doc.revision()
    );
    assert!(Doc::check(r#"{"version":1,"title":"Bad","blocks":[],"extra":1}"#).is_err());
}

#[test]
fn ids_are_stable_and_limits_are_located() {
    let doc = Doc::check(r#"{"version":1,"title":"Ids","blocks":[{"kind":"paragraph","text":"a"},{"id":"b1","kind":"paragraph","text":"b"},{"id":"b1","kind":"paragraph","text":"c"}]}"#).unwrap();
    assert_eq!(doc.blocks[1].id, "b1");
    assert_ne!(doc.blocks[0].id, doc.blocks[1].id);
    assert_eq!(
        Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap(),
        doc
    );
    let invalid = [
        (json!({"kind":"heading","level":4,"text":"Bad"}), ".level"),
        (
            json!({"kind":"paragraph","text":"x".repeat(32*1024)}),
            "blocks[0]",
        ),
        (json!({"kind":"table","rows":vec![vec!["x"];201]}), ".rows"),
        (json!({"kind":"table","rows":[vec!["x";21]]}), ".rows"),
        (json!({"kind":"table","rows":[["x"],["y","z"]]}), ".rows"),
        (json!({"kind":"image","src":"../bad.png"}), ".src"),
        (
            json!({"kind":"paragraph","text":"x","style":"missing"}),
            ".style",
        ),
    ];
    for (block, path) in invalid {
        let errors = Doc::check(&json!({"version":1,"title":"Bad","blocks":[block]}).to_string())
            .unwrap_err();
        assert!(errors.iter().any(|e| e.path.contains(path)), "{errors:?}");
    }
    let mut item = json!({"text":"deep"});
    for _ in 0..4 {
        item = json!({"text":"parent","children":[item]});
    }
    assert!(
        Doc::check(
            &json!({"version":1,"title":"Bad","blocks":[{"kind":"list","items":[item]}]})
                .to_string()
        )
        .unwrap_err()
        .iter()
        .any(|e| e.message.contains("depth"))
    );
    assert!(
        Doc::check(
            &json!({"version":1,"title":"Bad","blocks":vec![json!({"kind":"divider"});2001]})
                .to_string()
        )
        .unwrap_err()
        .iter()
        .any(|e| e.path == "$.blocks")
    );
    assert!(
        Doc::check(
            &json!({"version":1,"title":"Bad","page":{"margins_pt":[500,500,500,500]},"blocks":[]})
                .to_string()
        )
        .is_err()
    );
}

#[test]
fn every_operation_applies_in_order_and_keeps_the_original() {
    let base = fixture();
    let revision = base.revision();
    let doc = edit(
        &base,
        json!([
            {"op":"insert_blocks","after":"b3","blocks":[{"id":"b5","kind":"paragraph","text":"Added"}]},
            {"op":"update_block","id":"b5","patch":{"text":"Updated","style":"caption"}},
            {"op":"replace_text","id":"b3","find":"Q4","replace":"Q1","occurrence":2},
            {"op":"replace_text","id":"b4","item":"2.1","find":"Child","replace":"Nested"},
            {"op":"move_blocks","ids":["b5"],"after":"b1"},
            {"op":"delete_blocks","ids":["b2"]},
            {"op":"insert_markdown","after":"b4","markdown":"## Next\n\nA paragraph [^n].\n\n[^n]: Note"},
            {"op":"set_page","patch":{"footer":"{page} / {pages}"}},
            {"op":"set_title","title":"New plan"}
        ]),
    );
    assert_eq!(base.revision(), revision);
    assert_eq!(doc.title, "New plan");
    assert_eq!(doc.blocks[1].id, "b5");
    assert!(doc.block("b2").is_none());
    assert_eq!(
        doc.block("b3").unwrap().text(),
        Some("Ship **the docs**. Q4 Q1.")
    );
    assert_eq!(doc.footnotes["n"], "Note");
    assert!(
        matches!(&doc.block("b4").unwrap().kind, Kind::List { items, .. } if items[1].children[0].text == "Nested")
    );
    assert_eq!(doc.page.footer, "{page} / {pages}");
    let changed = edit(
        &doc,
        json!([{"op":"replace_text","id":"b3","find":"Q","replace":"Quarter "}]),
    );
    assert_eq!(
        changed.block("b3").unwrap().text(),
        Some("Ship **the docs**. Quarter 4 Quarter 1.")
    );
    let design = Design::default();
    let applied = edit(&doc, json!([{"op":"apply_design","design":"nexus"}]));
    assert_eq!(applied.design_rev, Some(design.revision()));
    assert_eq!(
        applied.block("b5").unwrap().style.as_deref(),
        Some("caption")
    );
}

#[test]
fn bad_operations_have_located_fix_hints_and_are_atomic() {
    let doc = fixture();
    for (op, path) in [
        (
            json!({"op":"insert_blocks","after":"missing","blocks":[]}),
            "id",
        ),
        (
            json!({"op":"insert_markdown","after":"missing","markdown":"text"}),
            "id",
        ),
        (
            json!({"op":"update_block","id":"b3","patch":{"id":"new"}}),
            "patch",
        ),
        (json!({"op":"delete_blocks","ids":["missing"]}), "id"),
        (
            json!({"op":"move_blocks","ids":["b1"],"after":"b1"}),
            "after",
        ),
        (
            json!({"op":"replace_text","id":"b3","find":"absent","replace":"x"}),
            "find",
        ),
        (
            json!({"op":"replace_text","id":"b3","find":"Q4","replace":"x","occurrence":0}),
            "occurrence",
        ),
        (
            json!({"op":"replace_text","id":"b4","item":"2.9","find":"x","replace":"y"}),
            "item",
        ),
        (
            json!({"op":"set_page","patch":{"margins_pt":[-1,1,1,1]}}),
            "margins_pt",
        ),
        (json!({"op":"apply_design","design":"missing"}), "design"),
        (json!({"op":"set_title","title":2}), "ops[0]"),
    ] {
        error(&doc, op, path);
    }
    let before = doc.revision();
    let errors = direct::prepare(
        &doc,
        vec![
            json!({"op":"set_title","title":"Never lands"}),
            json!({"op":"replace_text","id":"b3","find":"Absent","replace":"No"}),
        ],
    )
    .err()
    .unwrap();
    assert_eq!(errors[0].path, "ops[1].find");
    assert_eq!(doc.revision(), before);
    let files = temp();
    storage::save(&files, "docs/a.doc.json", &doc, history::Who::Model).unwrap();
    let bytes = std::fs::read(files.join("docs/a.doc.json")).unwrap();
    assert!(direct::prepare(&doc, vec![json!({"op":"delete_blocks","ids":["missing"]})]).is_err());
    assert_eq!(std::fs::read(files.join("docs/a.doc.json")).unwrap(), bytes);
    std::fs::remove_dir_all(files).unwrap();
}

#[test]
fn suggestions_are_bounded_and_cannot_introduce_image_sources() {
    let base = fixture();
    let raw = |ops| suggest::RawSuggestion {
        why: "Tighter wording".into(),
        ops,
    };
    let (valid, errors) = suggest::prepare(
        &base,
        vec![raw(vec![
            json!({"op":"replace_text","id":"b3","find":"Ship","replace":"Deliver"}),
        ])],
    );
    assert!(errors.is_empty());
    assert_eq!(valid.len(), 1);
    assert!(
        apply(&base, &valid[0].ops)
            .unwrap()
            .block("b3")
            .unwrap()
            .text()
            .unwrap()
            .starts_with("Deliver")
    );
    let op = json!({"op":"set_title","title":"Plan"});
    assert!(
        suggest::prepare(&base, vec![raw(vec![op.clone(); 40])])
            .1
            .is_empty()
    );
    assert!(
        !suggest::prepare(&base, vec![raw(vec![op; 41])])
            .1
            .is_empty()
    );
    assert!(
        !suggest::prepare(
            &base,
            vec![raw(vec![
                json!({"op":"insert_markdown","markdown":"![A](assets/new.png)"})
            ])]
        )
        .1
        .is_empty()
    );
    assert!(
        !suggest::prepare(&base, (0..6).map(|_| raw(vec![])).collect())
            .1
            .is_empty()
    );
    let base = edit(
        &base,
        json!([{"op":"insert_blocks","blocks":[{"kind":"image","src":"assets/existing.png"}]}]),
    );
    assert!(
        suggest::prepare(
            &base,
            vec![raw(vec![
                json!({"op":"insert_markdown","markdown":"![A](assets/existing.png)"})
            ])]
        )
        .1
        .is_empty()
    );
}

#[test]
fn markdown_covers_gfm_and_round_trips_source_and_footnotes() {
    let source = "# A title\n\n## Goals\n\n### Details\n\n#### Deep\n\nA **bold** and *italic* [link](https://example.com) with ~~strike~~ and `code` [^n].\n\n- [x] Done\n    - Nested\n- [ ] Next\n\n1. First\n2. Second\n\n> A quote\n\n> [!NOTE]\n> A note\n\n```sql\nselect 1;\n```\n\n| Region | Revenue |\n| :--- | ---: |\n| EU | 4.1 |\n\n![Chart](assets/chart.png \"Figure 1\")\n\n---\n\n[^n]: A footnote";
    let parsed = markdown::parse_document(source, true);
    assert!(matches!(parsed.blocks[0].kind, Kind::Title { .. }));
    assert!(matches!(
        markdown::parse("# Not a title")[0].kind,
        Kind::Heading { level: 1, .. }
    ));
    assert!(matches!(
        parsed.blocks[3].kind,
        Kind::Heading { level: 3, .. }
    ));
    assert!(parsed.blocks.iter().any(|b| matches!(&b.kind, Kind::List { items, .. } if items[0].checked == Some(true) && items[0].children.len() == 1)));
    assert!(
        parsed
            .blocks
            .iter()
            .any(|b| matches!(b.kind, Kind::Callout { .. }))
    );
    assert!(parsed.blocks.iter().any(|b| matches!(&b.kind, Kind::Table { align, .. } if *align == vec![crate::deck::Align::Left,crate::deck::Align::Right])));
    assert_eq!(parsed.footnotes["n"], "A footnote");
    let mut doc = Doc::from_design("Markdown", &Design::default());
    doc.blocks = parsed.blocks;
    doc.footnotes = parsed.footnotes;
    let again = markdown::parse_document(&markdown::render(&doc), true);
    assert_eq!(again.blocks, doc.blocks);
    assert_eq!(again.footnotes, doc.footnotes);
    let mut extended = doc.clone();
    extended.blocks = vec![Block::new(Kind::PageBreak), Block::new(Kind::Toc)];
    assert_eq!(
        markdown::parse(&markdown::render(&extended)),
        extended.blocks
    );
}

#[test]
fn outline_is_bounded_and_selection_returns_full_sections() {
    let doc = fixture();
    let text = outline(&doc, 1000);
    assert!(text.contains("[b2] ## Goals"));
    assert!(text.contains("[b4] - One"));
    let small = outline(&doc, 45);
    assert!(small.contains("more:"));
    assert!(small.chars().count() <= 45);
    for n in 0..20 {
        assert!(outline(&doc, n).chars().count() <= n);
    }
    assert!(read_blocks(&doc, &["b2".into()]).contains("Q4 Q4"));
    assert!(!read_blocks(&doc, &["b3".into()]).contains("Goals"));
    let doc = edit(
        &doc,
        json!([{"op":"insert_blocks","blocks":[{"kind":"table","header":true,"rows":[["Region","Revenue"],["EU","4.1"]]}]}]),
    );
    assert!(outline(&doc, 1000).contains("table 2x2: Region | Revenue"));
}

#[test]
fn docs_share_history_comments_reanchor_unicode_and_confine_files() {
    use feedback::{Anchor, Range};
    let files = temp();
    let relative = "docs/nested/plan.doc.json";
    let path = files.join(relative);
    let doc = edit(
        &fixture(),
        json!([{"op":"update_block","id":"b3","patch":{"text":"é Ship the docs"}}]),
    );
    storage::save(&files, relative, &doc, history::Who::User).unwrap();
    let anchor = Anchor {
        block: Some("b3".into()),
        range: Some(Range { start: 2, end: 6 }),
        quote: "Ship".into(),
    };
    assert!(anchor.check(&doc).is_ok());
    let raw = suggest::RawSuggestion {
        why: "Direct".into(),
        ops: vec![json!({"op":"replace_text","id":"b3","find":"Ship","replace":"Deliver"})],
    };
    let proposal = suggest::prepare(&doc, vec![raw]).0.pop().unwrap();
    let edit_guard = feedback::Edit::new(&doc, proposal, &anchor);
    let thread = feedback::Thread {
        id: "t1".into(),
        anchor,
        severity: feedback::Severity::Should,
        created: 1,
        resolved: None,
        messages: vec![feedback::Message::new_for(
            feedback::Role::Model,
            "A comment".into(),
            vec![edit_guard],
        )],
    };
    feedback::save(&path, &[thread]).unwrap();
    let moved = edit(
        &doc,
        json!([{"op":"replace_text","id":"b3","find":"é ","replace":"é New prefix "}]),
    );
    storage::save(&files, relative, &moved, history::Who::Model).unwrap();
    let threads = feedback::load(&path).unwrap();
    assert_eq!(threads[0].anchor.range, Some(Range { start: 13, end: 17 }));
    assert_eq!(
        threads[0].messages[0].edits[0].status,
        feedback::EditStatus::Outdated
    );
    let entries = history::list(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].load_for::<Doc>().unwrap(), doc);
    assert!(
        entries[0]
            .path
            .starts_with(files.join("docs/.history/plan"))
    );
    let removed = edit(
        &moved,
        json!([{"op":"replace_text","id":"b3","find":"Ship","replace":"Deliver"}]),
    );
    storage::save(&files, relative, &removed, history::Who::Model).unwrap();
    let threads = feedback::load(&path).unwrap();
    assert_eq!(threads[0].anchor.block.as_deref(), Some("b3"));
    assert_eq!(threads[0].anchor.range, None);
    let deleted = edit(&removed, json!([{"op":"delete_blocks","ids":["b3"]}]));
    storage::save(&files, relative, &deleted, history::Who::Model).unwrap();
    assert_eq!(feedback::load(&path).unwrap()[0].anchor.block, None);
    assert_eq!(storage::load(&files, relative).unwrap(), deleted);
    for bad in [
        "a.doc.json",
        "decks/a.doc.json",
        "docs/../a.doc.json",
        "docs/.hidden/a.doc.json",
        "docs/a..b.doc.json",
        "docs/a b.doc.json",
    ] {
        assert!(storage::confined(&files, bad).is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(files.join("outside"), files.join("docs/link")).unwrap();
        assert!(storage::confined(&files, "docs/link/a.doc.json").is_err());
    }
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let asset = storage::store_image(&files, png.get_ref(), "test", "photo").unwrap();
    assert!(files.join("docs").join(asset).is_file());
    std::fs::remove_dir_all(files).unwrap();
}
