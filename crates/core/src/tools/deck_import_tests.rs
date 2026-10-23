//! `deck create from`: importing a PowerPoint file from the space files.
//! The file is exported from a synthetic deck; no real presentation is used.

use super::tests::Fixture;
use super::*;
use crate::deck::export::pptx::export_pptx;

fn pptx() -> Vec<u8> {
    let deck = Deck::check(
        &json!({"version": 2, "title": "Synthetic", "slides": [
            {"id": "s1", "notes": "Say hello.", "layout": {"mode": "column", "padding": 100, "cross": "stretch"}, "children": [
                {"id": "a", "kind": "text", "text": "# Hello import"},
                {"id": "c", "kind": "chart", "chart": "bar", "labels": ["x"], "series": [{"name": "n", "values": [1]}], "sizing": {"h": 300}}
            ]},
            {"id": "s2", "children": [{"id": "b", "kind": "text", "text": "Second", "position": {"x": 100, "y": 100}, "sizing": {"w": 600, "h": 100}}]}
        ]})
        .to_string(),
    )
    .unwrap();
    export_pptx(&deck, None).unwrap()
}

#[tokio::test]
async fn create_from_a_space_file_imports_it_and_shows_the_slides() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.file("")).unwrap();
    std::fs::write(f.file("My Talk & more.pptx"), pptx()).unwrap();
    let created = f
        .run(
            "create",
            json!({"title": "Imported talk", "from": "My Talk & more.pptx"}),
        )
        .await;
    assert!(
        created.starts_with("created deck · decks/imported-talk.deck.json · 2 slides"),
        "{created}"
    );
    assert!(
        created.contains("imported from My Talk & more.pptx"),
        "{created}"
    );
    let images = f.images(&created);
    assert!(
        !images.is_empty() && image::open(&images[0]).is_ok(),
        "{created}"
    );
    let read = f
        .run("read", json!({"path": "decks/imported-talk.deck.json"}))
        .await;
    assert!(
        read.contains("Hello import") && read.contains("Second"),
        "{read}"
    );
    // Notes came across, and the deck can be edited like any other.
    let deck =
        Deck::check(&std::fs::read_to_string(f.file("decks/imported-talk.deck.json")).unwrap())
            .unwrap();
    assert_eq!(deck.title, "Imported talk");
    assert_eq!(deck.slides[0].notes, "Say hello.");
    let again = f
        .run(
            "create",
            json!({"title": "Imported talk", "from": "My Talk & more.pptx"}),
        )
        .await;
    assert!(again.contains("already exists"), "{again}");
}

#[tokio::test]
async fn create_from_reports_missing_and_unreadable_files_without_writing() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.file("")).unwrap();
    std::fs::write(f.file("notes.pptx"), b"not a presentation").unwrap();
    let missing = f
        .run("create", json!({"title": "A", "from": "nope.pptx"}))
        .await;
    assert!(
        missing.contains("no file") && missing.contains("nope.pptx"),
        "{missing}"
    );
    let bad = f
        .run("create", json!({"title": "B", "from": "notes.pptx"}))
        .await;
    assert!(bad.contains("not a .pptx file"), "{bad}");
    let outside = f
        .run("create", json!({"title": "C", "from": "../x.pptx"}))
        .await;
    assert!(outside.contains("no file"), "{outside}");
    assert!(!f.file("decks/a.deck.json").exists() && !f.file("decks/b.deck.json").exists());
}
