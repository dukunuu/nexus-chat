use super::*;
use serde_json::json;

fn deck(slides: serde_json::Value) -> Deck {
    Deck::check(&json!({"version":2,"title":"Taste","slides":slides}).to_string()).unwrap()
}

fn text(text: &str, style: &str) -> serde_json::Value {
    json!({"kind":"text","text":text,"style":style})
}

#[test]
fn a_generic_deck_gets_every_hint_it_earns() {
    let bullets = "- one\n- two\n- three\n- four\n- five\n- six\n- seven";
    let slide = |n: usize| {
        json!({"children":[
            {"kind":"text","text":format!("Topic {n}"),"font":{"size":40,"color":"#333333"}},
            {"kind":"text","text":bullets,"font":{"size":22 + n,"color":format!("#00000{n}")}}
        ]})
    };
    let deck = deck(json!((1..=6).map(slide).collect::<Vec<_>>()));
    let hints = hints(&deck);
    let all = hints.join("\n");
    for expected in [
        "only text",
        "walls of text",
        "largest text is under 64 px",
        "slides 1-6 repeat one layout",
        "one typeface",
        "text sizes",
        "one-off hex colors",
        "same background",
    ] {
        assert!(all.contains(expected), "{expected}\n{all}");
    }
    assert!(summary(&hints).unwrap().starts_with("taste: "));
}

#[test]
fn a_designed_deck_is_left_alone() {
    let deck = deck(json!([
        {"fill":"accent2","children":[{"kind":"text","text":"Strategy review","style":"kicker","font":{"color":"accent3"}}, {"kind":"text","text":"A quieter kind of momentum","style":"title","font":{"color":"bg"}}]},
        {"children":[text("Churn fell 18% after onboarding changed","title"), {"kind":"image","src":"assets/a.png","sizing":{"w":800,"h":600}}]},
        {"children":[text("38%","display"), text("Faster onboarding","heading")]},
        {"children":[text("What we keep","title"), {"kind":"frame","fill":"surface","sizing":{"w":600,"h":400},"layout":{"mode":"column"},"children":[text("- fewer meetings\n- one owner","body")]}]},
        {"fill":"accent2","children":[{"kind":"text","text":"Thank you","style":"title","font":{"color":"bg"}}]}
    ]));
    let mut fonts = deck.clone();
    fonts
        .theme
        .fonts
        .insert("heading".into(), "Fraunces".into());
    assert_eq!(hints(&fonts), Vec::<String>::new());
    assert!(summary(&[]).is_none());
}

#[test]
fn low_contrast_is_measured_against_the_slide_not_a_card() {
    let deck = deck(json!([
        {"fill":"#ffffff","children":[{"kind":"text","text":"Pale","style":"title","font":{"color":"#eeeeee"}}]},
        {"fill":"#ffffff","children":[{"kind":"frame","fill":"#111111","sizing":{"w":900,"h":500},"layout":{"mode":"column"},"children":[{"kind":"text","text":"On a dark card","style":"title","font":{"color":"#ffffff"}}]}]}
    ]));
    let all = hints(&deck).join("\n");
    assert!(all.contains("low-contrast text on slide 1;"), "{all}");
}
