use super::*;

fn fixture() -> Deck {
    Deck::classic_fixture("Story")
}
fn raw(ops: Value) -> RawSuggestion {
    RawSuggestion {
        why: "Tighter story".into(),
        ops: ops.as_array().unwrap().clone(),
    }
}

#[test]
fn supplies_and_overwrites_hashes_and_applies_independently() {
    let deck = fixture();
    let edit = json!([{"op":"update_block","slide":"s1","id":"b1","fields":{"text":"Better"}}]);
    let mut forged = edit.clone();
    forged[0]["base_hash"] = json!("untrusted");
    let (suggestions, errors) = prepare(&deck, vec![raw(edit), raw(forged)]);
    assert!(errors.is_empty());
    assert_eq!(suggestions.len(), 2);
    for suggestion in suggestions {
        let Op::UpdateBlock { base_hash, .. } = &suggestion.ops[0] else {
            panic!()
        };
        assert_eq!(*base_hash, deck.slides[0].blocks[0].hash());
        let next = apply(&deck, &suggestion.ops).unwrap();
        assert_eq!(
            next.slides[0].blocks[0].content,
            Content::Text {
                text: "Better".into()
            }
        );
    }
    assert_eq!(deck, fixture());
}

#[test]
fn fills_insertion_ids_and_reports_located_independent_errors() {
    let deck = fixture();
    let (suggestions, errors) = prepare(
        &deck,
        vec![
            raw(
                json!([{"op":"add_slide","slide":{"blocks":[{"kind":"text","x":0,"y":0,"w":2,"h":2,"text":"Hi"}]}},{"op":"insert_block","slide":"s1","block":{"kind":"text","x":0,"y":0,"w":2,"h":2,"text":"Hi"}}]),
            ),
            raw(json!([{"op":"update_block","slide":"s3","id":"b9","fields":{}}])),
        ],
    );
    assert_eq!(suggestions.len(), 1);
    assert_eq!(errors[0].path, "suggestions[1].ops[0].id");
    assert_eq!(errors[0].message, "no block \"b9\" on slide \"s3\"");
    let next = apply(&deck, &suggestions[0].ops).unwrap();
    let ids: BTreeSet<_> = next
        .slides
        .iter()
        .flat_map(|s| std::iter::once(&s.id).chain(s.blocks.iter().map(|b| &b.id)))
        .collect();
    assert_eq!(ids.len(), 8);
    assert!(!ids.contains(&String::new()));
    let (_, errors) = prepare(
        &deck,
        vec![raw(json!([{"op":"delete_slide","id":"missing"}]))],
    );
    assert_eq!(errors[0].path, "suggestions[0].ops[0]");
}

#[test]
fn proposal_limits_and_asset_provenance() {
    let deck = fixture();
    for items in [vec![], (0..6).map(|_| raw(json!([]))).collect()] {
        assert!(!prepare(&deck, items).1.is_empty());
    }
    for why in ["a".repeat(201), "two\nlines".into()] {
        assert_eq!(
            prepare(&deck, vec![RawSuggestion { why, ops: vec![] }]).1[0].path,
            "suggestions[0].why"
        );
    }
    let op = json!({"op":"set_notes","slide":"s1","notes":"note"});
    assert!(
        prepare(&deck, vec![raw(json!(vec![op.clone(); 40]))])
            .1
            .is_empty()
    );
    assert!(!prepare(&deck, vec![raw(json!(vec![op; 41]))]).1.is_empty());
    assert!(!prepare(&deck, vec![raw(json!([{"op":"insert_block","slide":"s1","block":{"kind":"image","x":0,"y":0,"w":2,"h":2,"src":"assets/invented.png"}}]))]).1.is_empty());
}

#[test]
fn selection_reports_unknown_slides_and_blocks() {
    let deck = fixture();
    let mut errors = Vec::new();
    Selection {
        slides: vec!["s1".into(), "missing".into()],
        blocks: vec![("s1".into(), "b1".into()), ("s1".into(), "nope".into())],
    }
    .check(&deck, &mut errors);
    let paths: Vec<_> = errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["selection.slides[1]", "selection.blocks[1]"]);
    let mut errors = Vec::new();
    Selection::default().check(&deck, &mut errors);
    assert!(errors.is_empty());
}
