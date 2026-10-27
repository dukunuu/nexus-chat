use super::{tests::Fixture, *};

#[tokio::test]
async fn import_design_returns_brand_summary_layout_slots_and_contact_sheet() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.tools.space_files_dir).unwrap();
    std::fs::write(
        f.file("brand.potx"),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/templates/brand.potx"
        )),
    )
    .unwrap();
    let (result, _) = f
        .tools
        .run(
            "canvas",
            r#"{"action":"design","op":"import","from":"brand.potx"}"#,
        )
        .await;
    assert!(result.starts_with("imported design brand"), "{result}");
    for expected in [
        "#12789a",
        "Arial",
        "Calibri",
        "title-slide",
        "subtitle",
        "picture-with-caption",
        "image",
        "![template layouts](design/brand-layouts.png)",
    ] {
        assert!(result.contains(expected), "{expected}: {result}");
    }
    assert!(f.file("templates/brand.potx").is_file());
    assert!(f.file("design/brand-layouts.png").is_file());
    let (list, _) = f
        .tools
        .run("canvas", r#"{"action":"design","op":"list"}"#)
        .await;
    assert!(
        list.contains("brand · template templates/brand.potx"),
        "{list}"
    );
    let (refused, _) = f
        .tools
        .run(
            "canvas",
            r#"{"action":"design","op":"import","from":"../escape.potx"}"#,
        )
        .await;
    assert!(refused.contains("invalid path"), "{refused}");
}
