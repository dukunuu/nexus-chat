use super::*;

fn request(kind: Kind) -> Request {
    let deck = Deck::new_from_layouts("A clear story");
    Request {
        kind,
        revision: deck.revision(),
        deck,
        selection: Selection::default(),
        anchor: None,
        messages: vec![],
        instruction: String::new(),
        model: None,
    }
}
fn anchor() -> Value {
    json!({"slide":"s1","block":"b1","x":3,"y":6})
}
fn edit() -> Value {
    json!({"why":"Clarify the headline","ops":[{"op":"update_block","slide":"s1","id":"b1","fields":{"text":"A clearer story"}}]})
}
fn comment() -> Value {
    json!({"anchor":anchor(),"text":"Shorten the headline so the hierarchy is easier to scan.","severity":"should","edit":edit()})
}

#[test]
fn anchors_require_a_real_slide_block_and_grid_point() {
    let mut req = request(Kind::Thread);
    assert!(req.check().is_err());
    req.anchor = Some(serde_json::from_value(anchor()).unwrap());
    assert!(req.check().is_ok());
    req.anchor.as_mut().unwrap().block = Some("missing".into());
    assert!(req.check().unwrap_err().iter().any(|e| e.path == "anchor"));
    req.anchor.as_mut().unwrap().block = None;
    req.anchor.as_mut().unwrap().x = 32;
    assert!(req.check().is_err());
    req.anchor.as_mut().unwrap().x = 0;
    req.anchor.as_mut().unwrap().slide = "missing".into();
    assert!(req.check().is_err());
}

#[test]
fn large_context_has_at_most_twelve_full_slides_with_current_first() {
    let mut req = request(Kind::Chat);
    for i in 3..=25 {
        let mut slide = req.deck.slides[0].clone();
        slide.id = format!("s{i}");
        for block in &mut slide.blocks {
            block.id = format!("{i}_{}", block.id);
        }
        req.deck.slides.push(slide);
    }
    req.selection.slides = vec!["s25".into()];
    let context = prompt_context(&req);
    assert_eq!(context, prompt_context(&req));
    let value: Value = serde_json::from_str(&context).unwrap();
    let slides = value["slides"].as_array().unwrap();
    assert_eq!(
        slides.iter().filter(|s| s.get("blocks").is_some()).count(),
        12
    );
    assert!(slides[24].get("blocks").is_some());
    assert!(slides[12].get("summary").is_some());
    req.kind = Kind::Thread;
    req.anchor = Some(Anchor {
        slide: "s25".into(),
        block: None,
        x: 0,
        y: 0,
    });
    let value: Value = serde_json::from_str(&prompt_context(&req)).unwrap();
    assert_eq!(
        value["slides"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s.get("blocks").is_some())
            .count(),
        1
    );
}

#[test]
fn thread_replies_are_short_but_decimal_values_are_not_sentences() {
    let mut req = request(Kind::Thread);
    req.anchor = Some(serde_json::from_value(anchor()).unwrap());
    req.messages = vec![Message {
        role: Role::You,
        text: "Keep the values, improve legibility.".into(),
    }];
    let context: Value = serde_json::from_str(&prompt_context(&req)).unwrap();
    assert_eq!(
        context["messages"][0]["text"],
        "Keep the values, improve legibility."
    );
    let (_, errors) = prepare(
        &req,
        &json!({"text":"Keep 2.5 as written. Increase the contrast so it is easier to read."})
            .to_string(),
    );
    assert!(errors.is_empty());
    let (response, errors) = prepare(&req, &json!({"text":"One. Two. Three."}).to_string());
    assert!(response.text.is_empty());
    assert!(errors.iter().any(|e| e.path == "text"));
}

#[test]
fn oversized_small_deck_still_uses_compact_context() {
    let mut req = request(Kind::Review);
    req.deck.slides[0].notes = "context ".repeat(4000);
    let context: Value = serde_json::from_str(&prompt_context(&req)).unwrap();
    assert_eq!(context["slides"].as_array().unwrap().len(), 2);
}

#[test]
fn fenced_comments_validate_severity_anchors_and_ops_independently() {
    let req = request(Kind::Review);
    let mut bad_anchor = comment();
    bad_anchor["anchor"]["block"] = json!("missing");
    let mut bad_severity = comment();
    bad_severity["severity"] = json!("critical");
    let mut bad_ops = comment();
    bad_ops["edit"]["ops"][0]["id"] = json!("missing");
    let reply = json!({"text":"A strong direction.","comments":[comment(),bad_anchor,bad_severity,bad_ops]});
    let (response, errors) = prepare(&req, &format!("```json\n{reply}\n```\nDone."));
    assert_eq!(response.comments.len(), 1);
    assert_eq!(errors.len(), 3);
    assert!(errors.iter().all(|e| e.path.starts_with("comments[")));
    let proposal = response.comments[0].edit.as_ref().unwrap();
    assert!(
        matches!(&proposal.ops[0], super::super::Op::UpdateBlock { base_hash, .. } if base_hash.len() == 64)
    );
    assert!(super::super::apply(&req.deck, &proposal.ops).is_ok());
}

#[test]
fn review_is_few_and_plain_and_chat_options_are_independent() {
    let req = request(Kind::Review);
    let (response, errors) = prepare(
        &req,
        &json!({"text":"Review","comments":vec![comment();9]}).to_string(),
    );
    assert_eq!(response.comments.len(), 8);
    assert_eq!(errors[0].path, "comments");
    let mut wordy = comment();
    wordy["text"] = json!("First. Second. Third.");
    assert!(
        !prepare(
            &req,
            &json!({"text":"Review","comments":[wordy]}).to_string()
        )
        .1
        .is_empty()
    );
    let chat = request(Kind::Chat);
    let (response, errors) = prepare(
        &chat,
        &json!({"text":"Two directions.","options":[edit(),edit()]}).to_string(),
    );
    assert!(errors.is_empty());
    assert_eq!(response.options.len(), 2);
    assert_ne!(response.options[0].id, response.options[1].id);
    assert!(
        !prepare(
            &chat,
            &json!({"text":"One direction.","options":[edit()]}).to_string()
        )
        .1
        .is_empty()
    );
    assert!(!prepare(&chat, "not json").1.is_empty());
}

#[test]
fn request_limits_cover_instruction_revision_and_deck_size() {
    let mut req = request(Kind::Chat);
    req.instruction = "é".repeat(2000);
    assert!(req.check().is_ok());
    req.instruction.push('x');
    assert!(req.check().is_err());
    req.instruction.clear();
    req.revision = "stale".into();
    assert!(req.check().is_err());
    let mut req = request(Kind::Chat);
    req.deck.slides[0].blocks = vec![req.deck.slides[0].blocks[0].clone(); 41];
    req.revision = req.deck.revision();
    assert!(req.check().is_err());
    let mut req = request(Kind::Chat);
    req.selection.slides.push("missing".into());
    assert!(req.check().is_err());
}
