use super::*;
use nexus_core::remote_image::Fetcher;
use serde_json::{Value, json};
use std::sync::Arc;

const PIC: &str = "https://img.example.invalid/pic.png";

fn fetcher() -> Arc<Fetcher> {
    let fetcher =
        Fetcher::new(std::env::temp_dir().join(format!("nexus-ui-img-{}", uuid::Uuid::new_v4())));
    crate::app_view::use_fetcher_for_tests(&fetcher);
    fetcher
}

fn png() -> Vec<u8> {
    let image = image::RgbImage::from_fn(60, 30, |x, y| image::Rgb([x as u8 * 4, y as u8 * 8, 99]));
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn text_of(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn canvas(source: &str) -> Vec<String> {
    let entry = Entry {
        session: Ok(Session::new(Document::parse(source).unwrap()).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    };
    text_of(&entry.canvas(80, &Theme::default(), false, true).0)
}

fn documents() -> Vec<(&'static str, String)> {
    let layout = |view: Value| json!({"version":1,"title":"Gallery","view":view}).to_string();
    let tiles = json!({
        "version":1,"title":"Pick","state":{"n":0},
        "view":[{"type":"buttons","items":[
            {"label":"Beach","action":"pick","body":["Quiet"],"image":PIC,"alt":"Beach photo"},
            {"label":"City","action":"pick","body":["Loud"],"image":PIC,"alt":"City photo"}
        ]}],
        "actions":{"pick":[{"op":"set","key":"n","value":1}]}
    })
    .to_string();
    vec![
        (
            "node",
            layout(
                json!([{"type":"image","src":PIC,"alt":"A cat","rows":5},{"type":"text","text":"after"}]),
            ),
        ),
        (
            "hero",
            layout(json!([{"type":"hero","title":"Trip","image":PIC,"alt":"Harbour"}])),
        ),
        (
            "cards",
            layout(json!([{"type":"cards","items":[
                {"title":"One","text":"First","image":PIC,"alt":"One photo"},
                {"title":"Two","text":"Second","image":PIC,"alt":"Two photo"},
                {"title":"Three","text":"Third"}
            ]}])),
        ),
        ("tiles", tiles),
    ]
}

#[test]
fn height_is_identical_while_loading_failed_disabled_and_loaded() {
    for (name, source) in documents() {
        let fetcher = fetcher();
        let loading_height = source_height(&source, 80);
        let loading = canvas(&source);
        assert!(
            loading
                .iter()
                .any(|row| row.contains("\u{25a3}") && row.contains("loading")),
            "{name}: {loading:#?}"
        );

        fetcher.seed_failure(PIC);
        let failed = canvas(&source);
        assert!(
            failed.iter().any(|row| row.contains("couldn't load")),
            "{name}: {failed:#?}"
        );

        fetcher.seed(PIC, &png()).unwrap();
        let ready = canvas(&source);
        assert!(
            ready.iter().any(|row| row.contains('\u{2580}')),
            "{name}: {ready:#?}"
        );
        assert!(!ready.iter().any(|row| row.contains("loading")), "{name}");

        fetcher.set_enabled(false);
        let off = canvas(&source);
        assert!(
            // Narrow tiles drop the host before they clip the description.
            off.iter().any(|row| row.contains("\u{25a3}")),
            "{name}: {off:#?}"
        );
        assert!(
            !off.iter()
                .any(|row| row.contains("loading") || row.contains("couldn't")),
            "{name}"
        );

        for (state, rows) in [("failed", &failed), ("ready", &ready), ("off", &off)] {
            assert_eq!(
                rows.len(),
                loading.len(),
                "{name} {state}: card height changed"
            );
        }
        assert_eq!(source_height(&source, 80), loading_height, "{name}");
    }
}

#[test]
fn image_node_reserves_exactly_its_declared_rows() {
    let _fetcher = fetcher();
    let make = |rows: u16| {
        json!({"version":1,"title":"G","view":[{"type":"image","src":PIC,"rows":rows},{"type":"text","text":"after"}]})
            .to_string()
    };
    let four = source_height(&make(4), 80);
    let nine = source_height(&make(9), 80);
    assert_eq!(nine - four, 5);
    let rows = canvas(&make(4));
    let after = rows.iter().position(|row| row.contains("after")).unwrap();
    let placeholder = rows.iter().position(|row| row.contains("loading")).unwrap();
    assert_eq!(
        after - placeholder,
        5,
        "four rows plus one blank line: {rows:#?}"
    );
}

#[test]
fn image_free_documents_are_unchanged() {
    let source = json!({"version":1,"title":"Plain","view":[
        {"type":"hero","title":"Trip"},
        {"type":"cards","items":[{"title":"One","text":"First"}]}
    ]})
    .to_string();
    let with_context = {
        let _fetcher = fetcher();
        canvas(&source)
    };
    assert!(!with_context.iter().any(|row| row.contains('\u{25a3}')));
}
