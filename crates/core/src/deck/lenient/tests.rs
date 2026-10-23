use serde_json::{Value, json};

use super::*;

fn notes_of(context: &Context) -> String {
    context.notes().join(" | ")
}

#[test]
fn short_path_forms_become_deck_paths() {
    for (input, expected) in [
        ("talk", Some("decks/talk.deck.json")),
        ("talk.deck.json", Some("decks/talk.deck.json")),
        ("decks/talk", Some("decks/talk.deck.json")),
        ("decks/talk.json", Some("decks/talk.deck.json")),
        ("./talk.deck.json", Some("decks/talk.deck.json")),
        ("decks/sub/talk", Some("decks/sub/talk.deck.json")),
        ("decks/talk.deck.json", None),
        ("other/talk.deck.json", None),
        ("../talk", None),
        ("/tmp/talk", None),
        ("decks/.renders/x.deck.json", None),
        ("decks/a b.deck.json", None),
    ] {
        assert_eq!(normalize_path(input).as_deref(), expected, "{input}");
    }
}

#[test]
fn a_font_given_as_a_string_becomes_a_family_object() {
    let mut context = Context::for_new_deck();
    let mut slides = json!([{"children": [
        {"kind": "text", "text": "Hi", "font": "Avenir"},
        {"kind": "frame", "children": [{"kind": "text", "text": "In", "font": "Georgia"}]}
    ]}]);
    context.slides(&mut slides);
    assert_eq!(
        slides[0]["children"][0]["font"],
        json!({"family": "Avenir"})
    );
    assert_eq!(
        slides[0]["children"][1]["children"][0]["font"],
        json!({"family": "Georgia"})
    );
    assert!(
        notes_of(&context).contains("font \"Avenir\""),
        "{}",
        notes_of(&context)
    );
}

#[test]
fn an_unknown_text_style_falls_back_to_body_unless_the_call_defines_it() {
    let mut context = Context::for_new_deck();
    let mut slides = json!([{"children": [
        {"id": "k", "kind": "text", "text": "A", "style": "eyebrow"},
        {"id": "c", "kind": "code", "source": "x", "style": "mono-ish"},
        {"id": "t", "kind": "text", "text": "B", "style": "title"}
    ]}]);
    context.slides(&mut slides);
    assert_eq!(slides[0]["children"][0]["style"], "body");
    assert_eq!(slides[0]["children"][1]["style"], "code");
    assert_eq!(slides[0]["children"][2]["style"], "title");
    assert!(
        notes_of(&context).contains("\"eyebrow\" on k"),
        "{}",
        notes_of(&context)
    );

    // Defined by set_text_style in the same call: kept, in any operation order.
    let mut context = Context::for_new_deck();
    let mut ops = vec![
        json!({"op": "insert_node", "parent": "s1", "node": {"kind": "text", "text": "A", "style": "eyebrow"}}),
        json!({"op": "set_text_style", "name": "eyebrow", "font": {"size": 24}}),
    ];
    context.ops(&mut ops);
    assert_eq!(ops[0]["node"]["style"], "eyebrow");

    // Defined by a text_styles entry supplied with the slides.
    let mut context = Context::for_new_deck();
    let mut styles = json!({"eyebrow": {"size": 24, "letter_spacing": 0.1}});
    context.text_styles(&mut styles);
    let mut slides = json!([{"children": [{"kind": "text", "text": "A", "style": "eyebrow"}]}]);
    context.slides(&mut slides);
    assert_eq!(slides[0]["children"][0]["style"], "eyebrow");
}

#[test]
fn shape_look_alike_kinds_become_shapes() {
    let mut context = Context::for_new_deck();
    let mut slides = json!([{"children": [
        {"kind": "ellipse", "fill": "accent"},
        {"kind": "rect"}, {"kind": "circle"}, {"kind": "line"}, {"kind": "arrow"}
    ]}]);
    context.slides(&mut slides);
    let kinds: Vec<&Value> = slides[0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| &n["kind"])
        .collect();
    assert!(kinds.iter().all(|k| **k == "shape"));
    let shapes: Vec<&str> = slides[0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["shape"].as_str().unwrap())
        .collect();
    assert_eq!(shapes, ["ellipse", "rect", "ellipse", "line", "arrow"]);
}

#[test]
fn numeric_strings_and_integral_floats_are_coerced_when_lossless() {
    let mut context = Context::for_new_deck();
    let mut slides = json!([{"layout": {"mode": "column", "gap": "24", "padding": ["80", "80px", 80, 80]}, "children": [
        {"kind": "text", "text": "58", "sizing": {"w": "640", "h": "hug"},
         "font": {"size": "58", "weight": 700.0, "line_height": "1.2"}, "opacity": "0.5",
         "position": {"x": "10", "y": 20.0}}
    ]}]);
    context.slides(&mut slides);
    let node = &slides[0]["children"][0];
    assert_eq!(node["sizing"], json!({"w": 640, "h": "hug"}));
    assert_eq!(node["font"]["size"], json!(58));
    assert_eq!(node["font"]["weight"], json!(700));
    assert_eq!(node["font"]["line_height"], json!(1.2));
    assert_eq!(node["opacity"], json!(0.5));
    assert_eq!(node["position"]["x"], json!(10));
    assert_eq!(node["text"], "58", "text content is never touched");
    assert_eq!(slides[0]["layout"]["gap"], json!(24));
    assert_eq!(slides[0]["layout"]["padding"], json!([80, 80, 80, 80]));

    // A fractional weight cannot be coerced losslessly: left for validation.
    let mut fractional =
        json!({"op": "update_node", "id": "a", "fields": {"font": {"weight": 650.5}}});
    Context::for_new_deck().ops(std::slice::from_mut(&mut fractional));
    assert_eq!(fractional["fields"]["font"]["weight"], json!(650.5));
}

#[test]
fn letter_spacing_beyond_two_is_taken_as_px_and_converted_to_em() {
    let mut context = Context::for_new_deck();
    let mut slides = json!([{"children": [
        {"kind": "text", "text": "A", "font": {"size": 40, "letter_spacing": 4}},
        {"kind": "text", "text": "B", "style": "caption", "font": {"letter_spacing": 2.6}},
        {"kind": "text", "text": "C", "font": {"letter_spacing": 0.05}}
    ]}]);
    context.slides(&mut slides);
    assert_eq!(
        slides[0]["children"][0]["font"]["letter_spacing"],
        json!(0.1)
    );
    // caption is 26 px: 2.6 px is 0.1 em.
    assert_eq!(
        slides[0]["children"][1]["font"]["letter_spacing"],
        json!(0.1)
    );
    assert_eq!(
        slides[0]["children"][2]["font"]["letter_spacing"],
        json!(0.05)
    );
    assert!(
        notes_of(&context).contains("letter_spacing"),
        "{}",
        notes_of(&context)
    );
}

#[test]
fn letter_spacing_in_an_update_uses_the_nodes_current_size() {
    let deck = Deck::check(
        &json!({"version": 2, "title": "t", "slides": [{"id": "s1", "children": [
            {"id": "a", "kind": "text", "text": "A", "font": {"size": 50}}
        ]}]})
        .to_string(),
    )
    .unwrap();
    let mut context = Context::for_deck(&deck);
    let mut ops =
        vec![json!({"op": "update_node", "id": "a", "fields": {"font": {"letter_spacing": 5}}})];
    context.ops(&mut ops);
    assert_eq!(ops[0]["fields"]["font"]["letter_spacing"], json!(0.1));
}

#[test]
fn repairs_are_reported_once_and_bounded() {
    let mut context = Context::for_new_deck();
    let mut slides = json!([{"children": (0..30).map(|i| json!({"kind": "text", "text": "x", "style": format!("s{i}")})).collect::<Vec<_>>()}]);
    context.slides(&mut slides);
    let notes = context.notes();
    assert!(notes.len() <= 13, "{notes:?}");
    assert!(notes.last().unwrap().contains("more repairs"), "{notes:?}");
}

#[test]
fn known_mistakes_get_a_one_line_fix() {
    assert!(
        hint(
            "$.x.font",
            "invalid type: string \"Avenir\", expected struct Font"
        )
        .unwrap()
        .contains("{\"family\":\"Avenir\"}")
    );
    assert!(
        hint("$.n.font.letter_spacing", "expected -0.5 to 2 em")
            .unwrap()
            .contains("em")
    );
    assert!(
        hint("$.n.style", "unknown text style \"x\"")
            .unwrap()
            .contains("set_text_style")
    );
    assert!(hint("$.n", "unknown field `colour`").is_some());
    assert!(hint("$.n", "something nobody has seen").is_none());
}
