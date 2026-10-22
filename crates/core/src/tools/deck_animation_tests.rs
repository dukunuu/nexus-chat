use super::*;

#[test]
fn read_summarizes_transition_and_all_build_slots_in_order() {
    let deck = Deck::check(
        &json!({"version":2,"title":"Motion","slides":[{
            "id":"s1","transition":{"type":"push","direction":"left"},"builds":["b","a"],"children":[
                {"id":"a","kind":"text","text":"Two","animation":{
                    "build_in":{"effect":"rise","trigger":"with_previous"},
                    "build_out":{"effect":"fade","trigger":"after_previous"}}},
                {"id":"b","kind":"text","text":"One","animation":{
                    "build_in":{"effect":"fade","by":"word"},"emphasis":{"effect":"pulse"}}}
            ]
        }]})
        .to_string(),
    )
    .unwrap();
    let path =
        std::env::temp_dir().join(format!("nexus-motion-{}.deck.json", uuid::Uuid::new_v4()));
    let read = read_text("decks/motion.deck.json", &path, &deck).unwrap();
    assert!(
        read.contains("transition: push left 400ms ease_out"),
        "{read}"
    );
    assert!(
        read.contains(
            "builds: 1 click fade by word b · 2 click emphasis pulse b · 3 with-prev rise a · 4 after-prev out fade a"
        ),
        "{read}"
    );
}

#[test]
fn animation_reference_is_only_in_the_once_per_session_cheat_sheet() {
    let mut shown = false;
    let schema = schema_once(&mut shown);
    for field in [
        "transition{",
        "animation{",
        "stagger_ms",
        "with_previous",
        "set_transition{",
        "set_animation{",
        "reorder_builds{",
        "same id or name",
    ] {
        assert!(schema.contains(field), "missing {field}");
    }
    assert_eq!(schema_once(&mut shown), "");
}
