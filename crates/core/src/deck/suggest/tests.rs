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
    let edit = json!([{"op":"update_node","id":"b1","fields":{"text":"Better"}}]);
    let mut forged = edit.clone();
    forged[0]["base_hash"] = json!("untrusted");
    let (suggestions, errors) = prepare(&deck, vec![raw(edit), raw(forged)]);
    assert!(errors.is_empty());
    assert_eq!(suggestions.len(), 2);
    for suggestion in suggestions {
        let Op::UpdateNode { base_hash, .. } = &suggestion.ops[0] else {
            panic!()
        };
        assert_eq!(*base_hash, deck.node("b1").unwrap().hash());
        let next = apply(&deck, &suggestion.ops).unwrap();
        assert_eq!(next.node("b1").unwrap().text(), Some("Better"));
    }
    assert_eq!(deck, fixture());
}

#[test]
fn fills_insertion_ids_and_reports_located_independent_errors() {
    let deck = fixture();
    let (suggestions, errors) = prepare(
        &deck,
        vec![
            raw(json!([
                {"op":"add_slide","slide":{"children":[{"kind":"frame","children":[{"kind":"text","text":"Hi"}]}]}},
                {"op":"insert_node","parent":"s1","node":{"kind":"text","text":"Hi"}},
                {"op":"wrap","ids":["b2","b3"],"layout":{"mode":"row"}}
            ])),
            raw(json!([{"op":"update_node","id":"b9","fields":{}}])),
        ],
    );
    assert_eq!(suggestions.len(), 1);
    assert_eq!(errors[0].path, "suggestions[1].ops[0].id");
    assert_eq!(errors[0].message, "no node \"b9\" in the deck");
    let next = apply(&deck, &suggestions[0].ops).unwrap();
    let ids: Vec<String> = next
        .slides
        .iter()
        .flat_map(|s| {
            std::iter::once(s.id.clone()).chain(s.nodes().into_iter().map(|n| n.id.clone()))
        })
        .collect();
    assert_eq!(ids.len(), ids.iter().collect::<BTreeSet<_>>().len());
    // 2 original slides + 1 added, their 3 nodes, the 2-node subtree, 1 inserted, 1 wrap frame.
    assert_eq!(ids.len(), 3 + 3 + 2 + 1 + 1);
    assert!(!ids.contains(&String::new()));
    let (_, errors) = prepare(
        &deck,
        vec![raw(json!([{"op":"delete_slide","id":"missing"}]))],
    );
    assert_eq!(errors[0].path, "suggestions[0].ops[0]");
    let (_, errors) = prepare(
        &deck,
        vec![raw(
            json!([{"op":"insert_node","parent":"s1","node":{"id":"b1","kind":"text","text":"dup"}}]),
        )],
    );
    assert_eq!(errors[0].path, "suggestions[0].ops[0].node.id");
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
    assert!(!prepare(&deck, vec![raw(json!([{"op":"insert_node","parent":"s1","node":{"kind":"image","src":"assets/invented.png"}}]))]).1.is_empty());
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
