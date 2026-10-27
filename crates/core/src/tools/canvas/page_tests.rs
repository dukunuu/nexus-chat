use super::tests::Fixture;
use super::*;
async fn call(f: &Fixture, action: &str, mut v: Value) -> String {
    v["action"] = json!(action);
    f.tools.run("canvas", &v.to_string()).await.0
}
#[tokio::test]
async fn doc_view_returns_revision_keyed_page_images_and_filters_by_blocks() {
    let f = Fixture::new();
    let created = call(
        &f,
        "create",
        json!({"kind":"doc","title":"Pages","markdown":"Opening."}),
    )
    .await;
    assert!(created.starts_with("created doc"), "{created}");
    let edit=call(&f,"edit",json!({"path":"docs/pages.doc.json","ops":[{"op":"insert_blocks","blocks":[{"id":"break","kind":"page_break"},{"id":"second","kind":"paragraph","text":"Second page."}]}]})).await;
    assert!(edit.starts_with("edited doc"), "{edit}");
    let result = call(
        &f,
        "view",
        json!({"path":"docs/pages.doc.json","blocks":["second"]}),
    )
    .await;
    assert!(
        result.starts_with("![page 2](docs/.renders/pages/"),
        "{result}"
    );
    let image = result
        .trim()
        .split_once("](")
        .unwrap()
        .1
        .trim_end_matches(')');
    assert!(
        std::fs::read(f.file(image))
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    let sheet = call(&f, "view", json!({"path":"docs/pages.doc.json"})).await;
    assert!(
        sheet.starts_with("![pages](docs/.renders/pages/sheet-"),
        "{sheet}"
    );
    let export = call(
        &f,
        "export",
        json!({"path":"docs/pages.doc.json","format":"docx"}),
    )
    .await;
    assert!(export.contains("exports/pages.docx"), "{export}");
    let imported = call(
        &f,
        "create",
        json!({"kind":"doc","from":"exports/pages.docx","path":"docs/imported.doc.json"}),
    )
    .await;
    assert!(imported.starts_with("created doc"), "{imported}");
    assert!(
        crate::doc::storage::load(&f.tools.space_files_dir, "docs/imported.doc.json")
            .unwrap()
            .blocks
            .iter()
            .any(|b| b.plain_text().contains("Second page"))
    );
}
#[tokio::test]
async fn markdown_images_resolve_against_source_not_space_root() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.file("notes/pictures")).unwrap();
    image::RgbImage::new(2, 2)
        .save(f.file("notes/pictures/source.png"))
        .unwrap();
    std::fs::write(
        f.file("notes/report.md"),
        "# Report\n\n![A study](pictures/source.png)",
    )
    .unwrap();
    let result = call(&f, "create", json!({"kind":"doc","from":"notes/report.md"})).await;
    assert!(result.starts_with("created doc"), "{result}");
    let doc = crate::doc::storage::load(&f.tools.space_files_dir, "docs/report.doc.json").unwrap();
    let src = doc
        .blocks
        .iter()
        .find_map(|b| {
            if let crate::doc::Kind::Image { src, .. } = &b.kind {
                Some(src)
            } else {
                None
            }
        })
        .unwrap();
    assert!(f.file(&format!("docs/{src}")).is_file());
}
