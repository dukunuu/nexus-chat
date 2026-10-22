use serde_json::{Value, json};

use super::model::{Animation, Build, Easing, Effect, Transition, TransitionType, Trigger};
use super::{Deck, Op, apply};

fn op(value: Value) -> Op {
    serde_json::from_value(value).unwrap()
}

fn fixture() -> Deck {
    Deck::check(
        &json!({"version":2,"title":"Motion","slides":[{
            "id":"s1","children":[
                {"id":"a","kind":"text","text":"One two"},
                {"id":"f","kind":"frame","children":[{"id":"b","kind":"shape"}]}
            ]
        }]})
        .to_string(),
    )
    .unwrap()
}

fn errors(value: &Value) -> Vec<(String, String)> {
    Deck::check(&value.to_string())
        .unwrap_err()
        .into_iter()
        .map(|e| (e.path, e.message))
        .collect()
}

fn with(slide: &Value) -> Value {
    json!({"version":2,"title":"Motion","slides":[slide]})
}

#[test]
fn typed_payloads_default_and_round_trip() {
    let transition: Transition = serde_json::from_value(json!({"type":"smart"})).unwrap();
    assert_eq!(transition.kind, TransitionType::Smart);
    assert_eq!(transition.duration_ms, 400.0);
    assert_eq!(transition.easing, Easing::EaseOut);
    assert_eq!(transition.direction, None);
    let animation: Animation = serde_json::from_value(json!({"build_in":{}})).unwrap();
    let build = animation.build_in.as_ref().unwrap();
    assert_eq!(build.effect, Effect::Fade);
    assert_eq!(build.trigger, Trigger::Click);
    assert_eq!(build.delay_ms, 0.0);
    assert_eq!(build.duration_ms, 400.0);
    assert_eq!(
        serde_json::from_value::<Animation>(serde_json::to_value(&animation).unwrap()).unwrap(),
        animation
    );
    assert!(serde_json::from_value::<Build>(json!({"trigger":"unknown"})).is_err());
    assert!(serde_json::from_value::<Build>(json!({"speed":1})).is_err());
    assert!(serde_json::from_value::<Transition>(json!({"type":"unknown"})).is_err());
}

#[test]
fn loaded_decks_round_trip_and_serialize_whole_numbers() {
    let source = with(
        &json!({"id":"s1","transition":{"type":"push","direction":"left","duration_ms":600,"easing":"spring"},
        "builds":["a"],"children":[
        {"id":"a","kind":"text","text":"Hi","animation":{
            "build_in":{"effect":"rise","direction":"up","delay_ms":150,"trigger":"after_previous","by":"word","stagger_ms":40},
            "emphasis":{"effect":"pulse"},"build_out":{"effect":"blur"}}}]}),
    );
    let deck = Deck::check(&source.to_string()).unwrap();
    let json = serde_json::to_value(&deck).unwrap();
    assert_eq!(json["slides"][0]["transition"]["duration_ms"], 600);
    assert_eq!(
        json["slides"][0]["children"][0]["animation"]["build_in"]["stagger_ms"],
        40
    );
    assert_eq!(Deck::check(&json.to_string()).unwrap(), deck);
    assert_eq!(deck.slides[0].build_sequence().len(), 3);
}

#[test]
fn unparseable_placeholders_are_dropped_with_a_load_note() {
    let source = with(
        &json!({"id":"s1","transition":{"kind":"push"},"builds":"nope","children":[
        {"id":"a","kind":"text","text":"Hi","animation":{"kind":"fade"}},
        {"id":"b","kind":"text","text":"Ok","animation":{"build_in":{"duration_ms":"fast"}}},
        {"id":"c","kind":"text","text":"Fine","animation":{"build_in":{"effect":"rise"}}}]}),
    );
    let (deck, notes) = Deck::check_noted(&source.to_string()).unwrap();
    assert!(deck.slides[0].transition.is_none());
    assert!(deck.node("a").unwrap().animation.is_none());
    assert!(deck.node("b").unwrap().animation.is_none());
    assert!(deck.node("c").unwrap().animation.is_some());
    assert_eq!(deck.slides[0].builds, ["c"]);
    assert_eq!(notes.len(), 4, "{notes:?}");
    assert!(notes[0].starts_with("$.slides[0].transition"), "{notes:?}");
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("$.slides[0].children[0].animation"))
    );
    assert!(notes.iter().any(|n| n.starts_with("$.slides[0].builds")));
    assert!(
        Deck::check(&source.to_string()).is_ok(),
        "check forgives the same data"
    );
    // The repaired deck is clean on the next load.
    let (_, again) = Deck::check_noted(&serde_json::to_string(&deck).unwrap()).unwrap();
    assert!(again.is_empty());
}

#[test]
fn loaded_values_are_range_and_slot_checked_with_located_errors() {
    let found = errors(&with(
        &json!({"id":"s1","transition":{"duration_ms":6000},"children":[
        {"id":"a","kind":"text","text":"x","animation":{"build_in":{"effect":"pulse","delay_ms":10001}}},
        {"id":"b","kind":"shape","animation":{"emphasis":{"effect":"fade"},"build_out":{"effect":"typewriter","by":"line"}}}]}),
    ));
    let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
    for expected in [
        "$.slides[0].transition.duration_ms",
        "$.slides[0].children[0].animation.build_in.effect",
        "$.slides[0].children[0].animation.build_in.delay_ms",
        "$.slides[0].children[1].animation.emphasis.effect",
        "$.slides[0].children[1].animation.build_out.effect",
        "$.slides[0].children[1].animation.build_out.by",
    ] {
        assert!(
            paths.contains(&expected),
            "{expected} missing from {paths:?}"
        );
    }
    let negative = errors(&with(&json!({"id":"s1","children":[
        {"id":"a","kind":"text","text":"x","animation":{"build_in":{"duration_ms":-1,"stagger_ms":20000}}}]})));
    assert_eq!(negative.len(), 2, "{negative:?}");
}

#[test]
fn empty_animation_objects_are_removed() {
    let deck = Deck::check(
        &with(&json!({"id":"s1","children":[{"id":"a","kind":"text","text":"x","animation":{}}]}))
            .to_string(),
    )
    .unwrap();
    assert!(deck.node("a").unwrap().animation.is_none());
    assert!(deck.slides[0].builds.is_empty());
}

#[test]
fn animation_ops_are_atomic_and_clearable() {
    let deck = fixture();
    let revision = deck.revision();
    let ops = vec![
        op(json!({"op":"set_transition","slide":"s1","transition":{"type":"push"}})),
        op(
            json!({"op":"set_animation","id":"a","slot":"build_in","build":{"effect":"typewriter","by":"word"}}),
        ),
        op(json!({"op":"set_animation","id":"b","slot":"emphasis","build":{"effect":"pulse"}})),
        op(json!({"op":"reorder_builds","slide":"s1","ids":["b","a"]})),
    ];
    let changed = apply(&deck, &ops).unwrap();
    assert_eq!(changed.slides[0].builds, ["b", "a"]);
    assert_eq!(
        changed.slides[0].transition.as_ref().unwrap().kind,
        TransitionType::Push
    );
    assert_eq!(deck.revision(), revision, "the undo snapshot is untouched");
    let mut invalid = ops;
    invalid.push(op(
        json!({"op":"reorder_builds","slide":"s1","ids":["missing"]}),
    ));
    assert!(apply(&deck, &invalid).is_err());
    assert_eq!(deck.revision(), revision);
    let cleared = apply(
        &changed,
        &[
            op(json!({"op":"set_animation","id":"a","slot":"build_in","build":null})),
            op(json!({"op":"set_transition","slide":"s1","transition":null})),
        ],
    )
    .unwrap();
    assert!(cleared.node("a").unwrap().animation.is_none());
    assert!(cleared.slides[0].transition.is_none());
    assert_eq!(cleared.slides[0].builds, ["b"]);
}

#[test]
fn set_animation_keeps_other_slots() {
    let deck = apply(
        &fixture(),
        &[
            op(json!({"op":"set_animation","id":"a","slot":"build_in","build":{"effect":"rise"}})),
            op(json!({"op":"set_animation","id":"a","slot":"build_out","build":{"effect":"fade"}})),
        ],
    )
    .unwrap();
    let animation = deck.node("a").unwrap().animation.as_ref().unwrap();
    assert_eq!(animation.build_in.as_ref().unwrap().effect, Effect::Rise);
    assert_eq!(animation.build_out.as_ref().unwrap().effect, Effect::Fade);
    let one = apply(
        &deck,
        &[op(
            json!({"op":"set_animation","id":"a","slot":"build_in","build":null}),
        )],
    )
    .unwrap();
    assert!(
        one.node("a")
            .unwrap()
            .animation
            .as_ref()
            .unwrap()
            .build_in
            .is_none()
    );
}

#[test]
fn ops_reject_ranges_and_incompatible_slots_without_changes() {
    let deck = fixture();
    for (id, slot, build, path) in [
        (
            "a",
            "build_in",
            json!({"duration_ms":5001}),
            "build_in.duration_ms",
        ),
        ("a", "build_in", json!({"delay_ms":-1}), "build_in.delay_ms"),
        (
            "a",
            "build_in",
            json!({"stagger_ms":10001}),
            "build_in.stagger_ms",
        ),
        (
            "a",
            "build_in",
            json!({"effect":"pulse"}),
            "build_in.effect",
        ),
        (
            "a",
            "build_out",
            json!({"effect":"shake"}),
            "build_out.effect",
        ),
        ("a", "emphasis", json!({"effect":"rise"}), "emphasis.effect"),
        (
            "b",
            "build_in",
            json!({"effect":"typewriter"}),
            "build_in.effect",
        ),
        ("b", "build_in", json!({"by":"word"}), "build_in.by"),
    ] {
        let error = apply(
            &deck,
            &[op(
                json!({"op":"set_animation","id":id,"slot":slot,"build":build}),
            )],
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(path) && error.contains("animation"),
            "{error}"
        );
    }
    let error = apply(
        &deck,
        &[op(
            json!({"op":"set_transition","slide":"s1","transition":{"duration_ms":-1}}),
        )],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("$.slides[0].transition.duration_ms"),
        "{error}"
    );
    assert!(
        apply(
            &deck,
            &[op(json!({"op":"set_animation","id":"a","slot":"build_in","build":{"duration_ms":0,"delay_ms":10000,"stagger_ms":10000}}))]
        )
        .is_ok()
    );
    assert!(
        apply(
            &deck,
            &[op(
                json!({"op":"reorder_builds","slide":"s1","ids":["a","a"]})
            )]
        )
        .is_err()
    );
    let plain = apply(
        &deck,
        &[op(json!({"op":"reorder_builds","slide":"s1","ids":["a"]}))],
    );
    assert!(plain.unwrap_err().to_string().contains("no animation"));
    // The op payloads themselves are typed.
    assert!(
        serde_json::from_value::<Op>(
            json!({"op":"set_transition","slide":"s1","transition":{"type":"wobble"}})
        )
        .is_err()
    );
}

#[test]
fn build_order_repairs_unknown_deleted_and_missing_nodes() {
    let deck = fixture();
    let mut value = serde_json::to_value(&deck).unwrap();
    value["slides"][0]["builds"] = json!(["gone", "b", "b"]);
    value["slides"][0]["children"][0]["animation"] = json!({"build_in":{"effect":"fade"}});
    value["slides"][0]["children"][1]["children"][0]["animation"] =
        json!({"build_in":{"effect":"rise"}});
    let normalized = Deck::check(&value.to_string()).unwrap();
    assert_eq!(normalized.slides[0].builds, ["b", "a"]);
    let deleted = apply(&normalized, &[op(json!({"op":"delete_node","id":"f"}))]).unwrap();
    assert_eq!(deleted.slides[0].builds, ["a"]);
    assert_eq!(
        Deck::check(&serde_json::to_string(&normalized).unwrap()).unwrap(),
        normalized
    );
}

#[test]
fn merge_patches_keep_other_animation_fields() {
    let deck = fixture();
    let animated = apply(
        &deck,
        &[
            op(json!({"op":"set_transition","slide":"s1","transition":{"type":"fade","duration_ms":800}})),
            op(json!({"op":"set_animation","id":"a","slot":"build_in","build":{"effect":"rise","delay_ms":200}})),
        ],
    )
    .unwrap();
    let changed = apply(
        &animated,
        &[
            op(json!({"op":"update_slide","id":"s1","fields":{"transition":{"type":"push"}}})),
            op(json!({"op":"update_node","id":"a","base_hash":animated.node("a").unwrap().hash(),"fields":{"animation":{"build_in":{"duration_ms":600}}}})),
        ],
    )
    .unwrap();
    let transition = changed.slides[0].transition.as_ref().unwrap();
    assert_eq!(transition.kind, TransitionType::Push);
    assert_eq!(transition.duration_ms, 800.0);
    let build = changed
        .node("a")
        .unwrap()
        .animation
        .as_ref()
        .unwrap()
        .build_in
        .as_ref()
        .unwrap();
    assert_eq!(
        (build.delay_ms, build.duration_ms, build.effect),
        (200.0, 600.0, Effect::Rise)
    );
    // Null removes a transition or an animation.
    let cleared = apply(
        &changed,
        &[
            op(json!({"op":"update_slide","id":"s1","fields":{"transition":null}})),
            op(json!({"op":"update_node","id":"a","base_hash":changed.node("a").unwrap().hash(),"fields":{"animation":null}})),
        ],
    )
    .unwrap();
    assert!(cleared.slides[0].transition.is_none());
    assert!(cleared.node("a").unwrap().animation.is_none());
    assert!(cleared.slides[0].builds.is_empty());
}

#[test]
fn merge_patches_cannot_smuggle_unparseable_animation() {
    let deck = fixture();
    let bad = apply(
        &deck,
        &[op(
            json!({"op":"update_slide","id":"s1","fields":{"transition":{"kind":"fade"}}}),
        )],
    );
    assert!(bad.is_err());
    let hash = deck.node("a").unwrap().hash();
    let bad = apply(
        &deck,
        &[op(
            json!({"op":"update_node","id":"a","base_hash":hash,"fields":{"animation":{"build_in":{"effect":"zoom"}}}}),
        )],
    );
    assert!(bad.is_err());
}
