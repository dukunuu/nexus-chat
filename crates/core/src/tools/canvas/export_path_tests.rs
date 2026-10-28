//! `export` writes outside the space only to a path the user wrote.
use super::tests::Fixture;
use super::*;

async fn call(f: &Fixture, action: &str, mut v: Value) -> String {
    v["action"] = json!(action);
    f.tools.run("canvas", &v.to_string()).await.0
}

fn user_said(f: &Fixture, text: &str) {
    *f.tools.decision_context.lock().unwrap() = ("session".into(), text.into());
}

#[tokio::test]
async fn a_deck_exports_to_a_path_the_user_gave() {
    let f = Fixture::new();
    call(&f, "create", json!({"title": "Talk"})).await;
    let target = f.root.join("Desktop/talk.pptx");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let path = target.to_string_lossy().into_owned();

    // Nothing in the conversation names it: refused, nothing written.
    user_said(&f, "export the deck");
    let refused = call(
        &f,
        "export",
        json!({"path": "decks/talk.deck.json", "format": path}),
    )
    .await;
    assert!(
        refused.contains("where the user wrote the path"),
        "{refused}"
    );
    assert!(!target.exists());

    user_said(&f, &format!("please export it to {path}"));
    let done = call(
        &f,
        "export",
        json!({"path": "decks/talk.deck.json", "format": path}),
    )
    .await;
    assert!(done.starts_with("exported decks/talk.deck.json"), "{done}");
    assert!(done.contains("talk.pptx"), "{done}");
    assert_eq!(&std::fs::read(&target).unwrap()[..2], b"PK");
    assert!(
        !f.file("exports").exists(),
        "the default folder was not used"
    );

    // The CLI's rule: an existing file is never overwritten.
    let again = call(
        &f,
        "export",
        json!({"path": "decks/talk.deck.json", "text": path}),
    )
    .await;
    assert!(again.contains("talk (2).pptx"), "{again}");
    assert!(target.with_file_name("talk (2).pptx").is_file());

    // The extension picks the format, including a deck's own JSON.
    let json_path = f.root.join("Desktop/talk.deck.json");
    user_said(&f, &format!("save a copy as {}", json_path.display()));
    let saved = call(
        &f,
        "export",
        json!({"path": "decks/talk.deck.json", "format": json_path.to_string_lossy()}),
    )
    .await;
    assert!(saved.starts_with("exported"), "{saved}");
    assert!(json_path.is_file());

    // Relative words still mean exports/.
    let plain = call(
        &f,
        "export",
        json!({"path": "decks/talk.deck.json", "format": "pdf"}),
    )
    .await;
    assert!(plain.contains("exports/talk.pdf"), "{plain}");
}

#[tokio::test]
async fn a_path_needs_an_extension_and_must_be_absolute() {
    let f = Fixture::new();
    call(&f, "create", json!({"title": "Talk"})).await;
    let bare = f.root.join("Desktop/talk");
    let said = bare.to_string_lossy().into_owned();
    user_said(&f, &said);
    let result = call(
        &f,
        "export",
        json!({"path": "decks/talk.deck.json", "format": said}),
    )
    .await;
    assert!(result.contains("needs an extension"), "{result}");
    assert!(!bare.exists());
}

#[tokio::test]
async fn a_doc_exports_to_a_path_the_user_gave_with_markdown_images_beside_it() {
    let f = Fixture::new();
    call(
        &f,
        "create",
        json!({"kind": "doc", "title": "Memo", "markdown": "Hello."}),
    )
    .await;
    let folder = f.root.join("Documents");
    std::fs::create_dir_all(&folder).unwrap();
    let docx = folder.join("memo.docx");
    user_said(&f, &format!("export to {}", docx.display()));
    let done = call(
        &f,
        "export",
        json!({"path": "docs/memo.doc.json", "format": docx.to_string_lossy()}),
    )
    .await;
    assert!(done.starts_with("exported docs/memo.doc.json"), "{done}");
    assert_eq!(&std::fs::read(&docx).unwrap()[..2], b"PK");

    let md = folder.join("memo.md");
    user_said(&f, &format!("export to {}", md.display()));
    let done = call(
        &f,
        "export",
        json!({"path": "docs/memo.doc.json", "format": md.to_string_lossy()}),
    )
    .await;
    assert!(done.starts_with("exported"), "{done}");
    assert!(std::fs::read_to_string(&md).unwrap().contains("Hello."));
}
