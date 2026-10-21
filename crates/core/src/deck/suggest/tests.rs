use super::*;

fn request() -> SuggestRequest {
    let deck = Deck::new_from_layouts("Story");
    SuggestRequest {
        revision: deck.revision(),
        deck,
        selection: Selection::default(),
        instruction: String::new(),
        model: None,
    }
}
fn raw(ops: Value) -> RawSuggestion {
    RawSuggestion {
        why: "Tighter story".into(),
        ops: ops.as_array().unwrap().clone(),
    }
}

#[test]
fn supplies_and_overwrites_hashes_and_applies_independently() {
    let deck = request().deck;
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
    assert_eq!(deck, request().deck);
}

#[test]
fn fills_insertion_ids_and_reports_located_independent_errors() {
    let deck = request().deck;
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
fn limits_and_asset_provenance() {
    let mut req = request();
    req.instruction = "é".repeat(2000);
    assert!(req.check().is_ok());
    req.instruction.push('x');
    assert!(req.check().is_err());
    req.instruction.clear();
    req.deck.slides[0].blocks = vec![req.deck.slides[0].blocks[0].clone(); 41];
    req.revision = req.deck.revision();
    assert!(req.check().is_err());
    let deck = request().deck;
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
fn context_selection_small_whole_and_stable_order() {
    let mut req = request();
    req.selection.slides.push("s1".into());
    let small: Value = serde_json::from_str(&prompt_context(&req)).unwrap();
    assert!(small["slides"][1].get("blocks").is_some());
    for i in 2..13 {
        let mut slide = req.deck.slides[1].clone();
        slide.id = format!("s{i}extra");
        req.deck.slides.push(slide);
    }
    let context = prompt_context(&req);
    assert_eq!(context, prompt_context(&req));
    let selected: Value = serde_json::from_str(&context).unwrap();
    assert!(selected["slides"][0].get("blocks").is_some());
    assert!(selected["slides"][1].get("blocks").is_none());
    assert_eq!(selected["slides"][1]["title"], "Your next big idea");
    req.selection = Selection {
        slides: vec![],
        blocks: vec![("s2".into(), "b2".into())],
    };
    let blocks: Value = serde_json::from_str(&prompt_context(&req)).unwrap();
    assert!(blocks["slides"][1].get("blocks").is_some());
    req.selection = Selection::default();
    let whole: Value = serde_json::from_str(&prompt_context(&req)).unwrap();
    assert!(
        whole["slides"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("blocks").is_some())
    );
}

#[test]
fn forgiving_json_is_still_structured() {
    assert!(parse_reply("```json\n{\"suggestions\":[]}\n```\nDone.").is_ok());
    assert!(parse_reply("{\"suggestions\":[]} trailing prose").is_ok());
    assert!(parse_reply("not JSON").is_err());
}
