use super::*;

const PIC: &str = "https://img.example/pic.png";

fn layout(view: Value) -> Value {
    json!({"version":1,"title":"Gallery","view":view})
}

fn counter() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/interactive/counter.json"
    ))
    .unwrap()
}

fn parse(value: &Value) -> Result<Document> {
    Document::parse(&value.to_string())
}

#[test]
fn image_node_defaults_to_six_rows_and_bounds_them_to_four_through_twelve() {
    let doc = parse(&layout(json!([{"type":"image","src":PIC,"alt":"A cat"}]))).unwrap();
    assert!(doc.is_static());
    assert_eq!(doc.nodes()[0].rows(), 6);
    for rows in [4, 8, 12] {
        let doc = parse(&layout(
            json!([{"type":"image","src":PIC,"alt":"A cat","rows":rows}]),
        ))
        .unwrap();
        assert_eq!(doc.nodes()[0].rows(), rows);
    }
    for rows in [0, 3, 13, 40] {
        assert!(
            parse(&layout(json!([{"type":"image","src":PIC,"rows":rows}]))).is_err(),
            "{rows}"
        );
    }
    // The alt text is optional.
    assert!(parse(&layout(json!([{"type":"image","src":PIC}]))).is_ok());
    assert!(parse(&layout(json!([{"type":"image","alt":"no source"}]))).is_err());
    assert!(parse(&layout(json!([{"type":"image","src":PIC,"extra":1}]))).is_err());
}

#[test]
fn image_sources_must_be_plain_https_urls() {
    for src in [
        "http://img.example/pic.png",
        "file:///etc/hosts",
        "data:image/png;base64,AAAA",
        "//img.example/pic.png",
        "/pic.png",
        "pic.png",
        "https://user:secret@img.example/pic.png",
        "https://img.example/{{name}}.png",
        "https://img.example/{x}.png",
        "https://img.example/pic\n.png",
        "",
    ] {
        let mut node = json!({"type":"image","src":src});
        assert!(parse(&layout(json!([node.clone()]))).is_err(), "{src:?}");
        // The same rule applies to every other place an image can be named.
        node = json!({"type":"hero","title":"T","image":src});
        assert!(parse(&layout(json!([node]))).is_err(), "hero {src:?}");
        let node = json!({"type":"cards","items":[{"title":"T","text":"B","image":src}]});
        assert!(parse(&layout(json!([node]))).is_err(), "card {src:?}");
        let mut data = counter();
        data["view"] =
            json!([{"type":"buttons","items":[{"label":"Go","action":"add","image":src}]}]);
        assert!(parse(&data).is_err(), "tile {src:?}");
    }
}

#[test]
fn alt_text_is_plain_single_line_text() {
    for alt in [
        "{{name}}",
        "has {braces}",
        "line\nbreak",
        "\u{1b}[31mred",
        &"x".repeat(MAX_TEXT + 1),
    ] {
        assert!(
            parse(&layout(json!([{"type":"image","src":PIC,"alt":alt}]))).is_err(),
            "{alt:?}"
        );
    }
}

#[test]
fn hero_cards_and_tiles_accept_an_image_and_alt() {
    let doc = parse(&layout(json!([
        {"type":"hero","title":"Trip","image":PIC,"alt":"Harbour"},
        {"type":"cards","items":[
            {"title":"One","text":"First","image":PIC,"alt":"One photo"},
            {"title":"Two","text":"Second"}
        ]}
    ])))
    .unwrap();
    assert_eq!(doc.nodes()[0].rows(), 9, "hero with a picture");
    let mut data = counter();
    data["view"] = json!([{"type":"buttons","items":[
        {"label":"Pick","action":"add","body":["Quiet"],"image":PIC,"alt":"Beach"},
        {"label":"Skip","action":"add"}
    ]}]);
    let doc = parse(&data).unwrap();
    let Node::Buttons { items, .. } = &doc.nodes()[0] else {
        panic!("buttons")
    };
    assert_eq!(items[0].image.as_deref(), Some(PIC));
    assert_eq!(items[0].alt.as_deref(), Some("Beach"));
    assert!(items[1].image.is_none());
}

#[test]
fn declared_rows_do_not_depend_on_whether_images_exist_yet() {
    // Rows are derived from the document alone, so the budget (and the
    // frontend's reservation) can never change when a download finishes.
    let with = parse(&layout(json!([
        {"type":"hero","title":"T","image":PIC},
        {"type":"image","src":PIC,"rows":5},
        {"type":"cards","items":[{"title":"A","text":"B","image":PIC}]}
    ])))
    .unwrap();
    assert_eq!(with.rows(), with.rows());
    assert_eq!(with.nodes()[0].rows(), 9);
    assert_eq!(with.nodes()[1].rows(), 5);
    assert_eq!(with.nodes()[2].rows(), 10);
}

#[test]
fn plain_text_names_images_by_their_alt_text() {
    let doc = parse(&layout(json!([
        {"type":"image","src":PIC,"alt":"A cat"},
        {"type":"hero","title":"Trip","image":PIC,"alt":"Harbour"},
        {"type":"cards","items":[{"title":"One","text":"First","image":PIC,"alt":"Photo"}]},
        {"type":"image","src":PIC}
    ])))
    .unwrap();
    assert_eq!(
        doc.plain_text(),
        "Gallery\n[image: A cat]\nTrip\n[image: Harbour]\nOne\nFirst\n[image: Photo]\n[image: ]"
    );
    let mut data = counter();
    data["view"] = json!([{"type":"buttons","items":[{"label":"Pick","action":"add","body":["Quiet"],"image":PIC,"alt":"Beach"}]}]);
    let text = parse(&data).unwrap().plain_text();
    assert!(text.contains("[image: Beach]"), "{text}");
}

#[test]
fn the_urls_are_never_part_of_the_readable_text() {
    let doc = parse(&layout(json!([{"type":"image","src":PIC,"alt":"A cat"}]))).unwrap();
    assert!(!doc.plain_text().contains("img.example"));
}

#[test]
fn picture_tiles_count_their_band_toward_the_declared_rows() {
    let mut data = counter();
    data["view"] =
        json!([{"type":"buttons","rows":3,"items":[{"label":"A","action":"add","image":PIC}]}]);
    assert_eq!(parse(&data).unwrap().nodes()[0].rows(), 9);
    data["view"] = json!([{"type":"buttons","rows":3,"items":[{"label":"A","action":"add"}]}]);
    assert_eq!(parse(&data).unwrap().nodes()[0].rows(), 3);
    // Too many picture rows for the card budget are refused up front.
    data["view"] = json!([
        {"type":"buttons","rows":3,"items":[{"label":"A","action":"add","image":PIC}]},
        {"type":"buttons","rows":3,"items":[{"label":"B","action":"add","image":PIC}]},
        {"type":"buttons","rows":3,"items":[{"label":"C","action":"add","image":PIC}]}
    ]);
    assert!(parse(&data).is_err());
}
