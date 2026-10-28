//! Editorial, photo and repeated-content layouts for the built-in design.
use serde_json::{Value, json};

use crate::design::Spacing;

fn text(slot: &str, style: &str, color: &str) -> Value {
    let weight = if matches!(style, "title" | "heading" | "subheading" | "display") {
        700
    } else {
        400
    };
    json!({"kind":"text", "slot":slot, "text":slot, "style":style,
        "font":{"color":color, "weight":weight}, "sizing":{"w":"fill"}})
}

fn eyebrow(slot: &str, color: &str) -> Value {
    let mut node = text(slot, "caption", color);
    node["font"]["family"] = json!("mono");
    node["font"]["letter_spacing"] = json!(0.08);
    node
}

fn at(mut node: Value, rect: [f64; 4]) -> Value {
    let [x, y, w, h] = rect;
    node["position"] = json!({"x":x,"y":y});
    node["sizing"] = json!({"w":w,"h":h});
    node
}

fn stack(children: Vec<Value>, gap: f64) -> Value {
    let mut node = json!({"kind":"frame", "sizing":{"w":"fill","h":"fill"},
        "layout":{"mode":"column","gap":gap,"cross":"stretch"}});
    node["children"] = Value::Array(children);
    node
}

fn row(children: Vec<Value>, gap: f64) -> Value {
    let mut node = json!({"kind":"frame", "sizing":{"w":"fill","h":"fill"},
        "layout":{"mode":"row","gap":gap,"cross":"stretch"}});
    node["children"] = Value::Array(children);
    node
}

fn photo(slot: &str) -> Value {
    json!({"kind":"image", "slot":slot, "src":"assets/placeholder.png", "fit":"cover",
        "radius":32, "sizing":{"w":"fill","h":"fill"}})
}

fn portrait(slot: &str, side: f64) -> Value {
    json!({"kind":"image","slot":slot,"src":"assets/placeholder.png","fit":"cover","mask":"circle",
        "focus":{"x":0.5,"y":0.35},"shadow":"md","sizing":{"w":side,"h":side}})
}

fn panel(name: &str, children: Vec<Value>, fill: Value, gap: f64) -> Value {
    let mut node = stack(children, gap);
    node["name"] = json!(name);
    node["fill"] = fill;
    node["radius"] = json!(32);
    node["shadow"] = json!("md");
    node["layout"]["padding"] = json!(48);
    node
}

fn slide(fill: Value, children: Vec<Value>) -> Value {
    let mut node = json!({"layout":{"mode":"none"}});
    node["fill"] = fill;
    node["children"] = Value::Array(children);
    node
}

fn full_photo(angle: f64, stops: Value) -> Value {
    let mut node = at(photo("image"), [0.0, 0.0, 1920.0, 1080.0]);
    node["radius"] = json!(0);
    node["overlay"] = json!({"fill":{"linear":{"angle":angle}}});
    node["overlay"]["fill"]["linear"]["stops"] = stops;
    node
}

fn cover(spacing: Spacing) -> Value {
    let m = spacing.margin;
    slide(
        json!("fg"),
        vec![
            full_photo(
                90.0,
                json!([[0,{"color":"fg","opacity":0.94}],[0.55,{"color":"fg","opacity":0.65}],[1,{"color":"fg","opacity":0.08}]]),
            ),
            at(eyebrow("kicker", "bg"), [m, m, 1260.0, 48.0]),
            at(
                stack(
                    vec![text("title", "title", "bg"), text("subtitle", "body", "bg")],
                    spacing.gap,
                ),
                [m, 500.0, 1160.0, 440.0],
            ),
            at(
                text("caption", "caption", "bg"),
                [1400.0, 920.0, 400.0, 100.0],
            ),
        ],
    )
}

fn closing(spacing: Spacing) -> Value {
    let m = spacing.margin;
    slide(
        json!("fg"),
        vec![
            full_photo(
                180.0,
                json!([[0,{"color":"fg","opacity":0.10}],[0.45,{"color":"fg","opacity":0.28}],[1,{"color":"fg","opacity":0.96}]]),
            ),
            at(eyebrow("kicker", "bg"), [m, m, 1460.0, 48.0]),
            at(
                stack(
                    vec![text("title", "title", "bg"), text("subtitle", "body", "bg")],
                    spacing.gap,
                ),
                [m, 640.0, 1260.0, 360.0],
            ),
            at(
                text("caption", "caption", "bg"),
                [1440.0, 934.0, 360.0, 80.0],
            ),
        ],
    )
}

fn section(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let mut number = text("number", "display", "bg");
    number["font"]["valign"] = json!("middle");
    slide(
        json!({"linear":{"angle":135,"stops":[[0,"accent"],[1,"accent3"]]}}),
        vec![
            at(
                json!({"kind":"shape","shape":"ellipse","fill":"accent2"}),
                [1220.0, 140.0, 960.0, 960.0],
            ),
            at(eyebrow("kicker", "bg"), [m, 160.0, 1040.0, 48.0]),
            at(text("title", "title", "bg"), [m, 370.0, 1080.0, 330.0]),
            at(text("subtitle", "body", "bg"), [m, 800.0, 1080.0, 160.0]),
            at(number, [1360.0, 405.0, 500.0, 390.0]),
        ],
    )
}

fn stats(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let cards = (1..=3)
        .map(|i| {
            let mut label = text(&format!("stat{i}_label"), "body", "fg");
            label["font"]["weight"] = json!(700);
            panel(
                &format!("stat{i}"),
                vec![
                    text(
                        &format!("stat{i}_value"),
                        "title",
                        if i == 3 { "accent2" } else { "accent" },
                    ),
                    label,
                    text(
                        &format!("stat{i}_detail"),
                        "caption",
                        if i == 3 { "fg" } else { "muted" },
                    ),
                ],
                json!(if i == 3 {
                    "accent3"
                } else if i == 2 {
                    "surface"
                } else {
                    "bg"
                }),
                spacing.gap,
            )
        })
        .collect();
    slide(
        json!("accent2"),
        vec![
            at(eyebrow("kicker", "bg"), [m, 96.0, 1600.0, 48.0]),
            at(text("title", "heading", "bg"), [m, 178.0, 1680.0, 130.0]),
            at(row(cards, spacing.gap), [m, 370.0, 1680.0, 490.0]),
            at(text("takeaway", "body", "bg"), [m, 940.0, 1640.0, 100.0]),
        ],
    )
}

fn timeline(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let steps = (1..=5).map(|i| {
        let mut number = text("", "body", "bg");
        number["text"] = json!(format!("{i:02}"));
        number["font"]["align"] = json!("center");
        number["font"]["valign"] = json!("middle");
        number["sizing"]["h"] = json!("fill");
        let badge = json!({"kind":"frame","fill":"accent2","radius":60,"shadow":"sm",
            "sizing":{"w":120,"h":120}, "layout":{"mode":"column","cross":"stretch"}, "children":[number]});
        let mut title = text(&format!("step{i}_title"),"body","fg");
        title["font"]["weight"] = json!(700);
        let mut step = stack(vec![badge, eyebrow(&format!("step{i}_date"),"accent"), title,
            text(&format!("step{i}_body"),"caption","muted")], spacing.gap);
        step["name"] = json!(format!("step{i}"));
        step
    }).collect();
    slide(
        json!("bg"),
        vec![
            at(eyebrow("kicker", "accent2"), [m, 96.0, 1660.0, 48.0]),
            at(text("title", "heading", "fg"), [m, 176.0, 1680.0, 160.0]),
            at(
                json!({"kind":"shape","fill":"border"}),
                [m + 60.0, 450.0, 1560.0, 3.0],
            ),
            at(row(steps, spacing.gap), [m, 390.0, 1680.0, 550.0]),
            at(
                text("caption", "caption", "muted"),
                [m, 988.0, 1680.0, 48.0],
            ),
        ],
    )
}

fn image_grid(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let mut feature = stack(vec![photo("image1"), text("caption1", "body", "bg")], 24.0);
    feature["name"] = json!("image1");
    let tiles = (2..=4)
        .map(|i| {
            let mut image = photo(&format!("image{i}"));
            image["sizing"]["w"] = json!(240);
            image["radius"] = json!(20);
            let mut caption = text(&format!("caption{i}"), "body", "bg");
            caption["font"]["valign"] = json!("middle");
            caption["sizing"]["h"] = json!("fill");
            let mut tile = row(vec![image, caption], 24.0);
            tile["name"] = json!(format!("image{i}"));
            tile["fill"] = json!({"color":"bg","opacity":0.06});
            tile["radius"] = json!(24);
            tile["layout"]["padding"] = json!(24);
            tile
        })
        .collect();
    slide(
        json!("fg"),
        vec![
            at(eyebrow("kicker", "accent3"), [m, 96.0, 1660.0, 48.0]),
            at(text("title", "heading", "bg"), [m, 176.0, 1680.0, 140.0]),
            at(
                row(vec![feature, stack(tiles, spacing.gap)], spacing.gap),
                [m, 350.0, 1680.0, 640.0],
            ),
        ],
    )
}

fn quote_photo(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let portrait = at(portrait("image", 560.0), [1210.0, 290.0, 560.0, 560.0]);
    let mut quote_mark = text("", "title", "accent");
    quote_mark["text"] = json!("“");
    let mut attribution = text("attribution", "body", "fg");
    attribution["font"]["weight"] = json!(700);
    slide(
        json!({"linear":{"angle":90,"stops":[[0,"bg"],[1,"surface"]]}}),
        vec![
            at(eyebrow("kicker", "accent2"), [m, 100.0, 1660.0, 48.0]),
            at(quote_mark, [m, 230.0, 200.0, 140.0]),
            at(text("quote", "quote", "fg"), [m, 370.0, 1000.0, 330.0]),
            at(attribution, [m, 790.0, 1000.0, 70.0]),
            at(text("role", "caption", "muted"), [m, 880.0, 1000.0, 60.0]),
            at(
                json!({"kind":"shape","shape":"ellipse","fill":"accent3"}),
                [1170.0, 250.0, 640.0, 640.0],
            ),
            portrait,
        ],
    )
}

fn cards(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let cards = (1..=3).map(|i| {
        let mut icon = text(&format!("card{i}_icon"), "body", "bg");
        icon["font"]["weight"] = json!(700);
        icon["font"]["align"] = json!("center");
        icon["font"]["valign"] = json!("middle");
        icon["sizing"]["h"] = json!("fill");
        let badge = json!({"kind":"frame","fill":if i == 2 { "accent" } else { "accent2" }, "radius":48,
            "sizing":{"w":96,"h":96},"layout":{"mode":"column","cross":"stretch"},"children":[icon]});
        panel(&format!("card{i}"), vec![badge, text(&format!("card{i}_title"),"subheading","fg"),
            text(&format!("card{i}_body"),"body","muted")], json!({"linear":{"angle":145,"stops":[[0,"bg"],[1,"surface"]]}}), 24.0)
    }).collect();
    slide(
        json!("accent2"),
        vec![
            at(eyebrow("kicker", "accent3"), [m, 96.0, 1660.0, 48.0]),
            at(text("title", "heading", "bg"), [m, 176.0, 1680.0, 140.0]),
            at(text("subtitle", "body", "bg"), [m, 310.0, 1680.0, 110.0]),
            at(row(cards, spacing.gap), [m, 470.0, 1680.0, 490.0]),
        ],
    )
}

fn team(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let people = (1..=4)
        .map(|i| {
            let mut image = portrait(&format!("person{i}_image"), 300.0);
            image["shadow"] = json!("sm");
            let mut name = text(&format!("person{i}_name"), "body", "fg");
            name["font"]["weight"] = json!(700);
            name["font"]["align"] = json!("center");
            let mut role = text(&format!("person{i}_role"), "caption", "muted");
            role["font"]["align"] = json!("center");
            let mut person = stack(vec![image, name, role], 28.0);
            person["name"] = json!(format!("person{i}"));
            person["layout"]["cross"] = json!("center");
            person
        })
        .collect();
    slide(
        json!("bg"),
        vec![
            at(eyebrow("kicker", "accent2"), [m, 96.0, 1660.0, 48.0]),
            at(text("title", "heading", "fg"), [m, 176.0, 1680.0, 140.0]),
            at(text("subtitle", "body", "muted"), [m, 310.0, 1680.0, 100.0]),
            at(row(people, 64.0), [m, 470.0, 1680.0, 480.0]),
        ],
    )
}

fn chart_focus(spacing: Spacing) -> Value {
    let m = spacing.margin;
    let mut chart = photo("chart");
    chart["fit"] = json!("contain");
    chart["radius"] = json!(16);
    let mut chart_panel = panel("chart", vec![chart], json!("bg"), 0.0);
    chart_panel["layout"]["padding"] = json!(32);
    let mut takeaway = text("takeaway", "body", "bg");
    takeaway["font"]["weight"] = json!(700);
    let callout = panel(
        "takeaway",
        vec![
            eyebrow("label", "accent3"),
            text("metric", "title", "bg"),
            takeaway,
            text("body", "caption", "bg"),
        ],
        json!("accent2"),
        24.0,
    );
    slide(
        json!("surface"),
        vec![
            at(eyebrow("kicker", "accent2"), [m, 80.0, 1660.0, 48.0]),
            at(text("title", "heading", "fg"), [m, 150.0, 1680.0, 140.0]),
            at(chart_panel, [m, 320.0, 1080.0, 650.0]),
            at(callout, [1264.0, 320.0, 536.0, 650.0]),
            at(
                text("caption", "caption", "muted"),
                [m, 1000.0, 1680.0, 48.0],
            ),
        ],
    )
}

pub(super) fn builtin(spacing: Spacing) -> [(&'static str, Value); 10] {
    [
        ("cover_image", cover(spacing)),
        ("section_color", section(spacing)),
        ("stats", stats(spacing)),
        ("timeline", timeline(spacing)),
        ("image_grid", image_grid(spacing)),
        ("quote_photo", quote_photo(spacing)),
        ("cards", cards(spacing)),
        ("team", team(spacing)),
        ("chart_focus", chart_focus(spacing)),
        ("closing_image", closing(spacing)),
    ]
}
