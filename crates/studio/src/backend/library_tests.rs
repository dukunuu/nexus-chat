//! The library commands and every native dialog, with the dialogs scripted.

use nexus_core::canvas::dialogs::Scripted;
use nexus_core::deck::history::Who;
use nexus_core::doc::{self, Doc};
use serde_json::json;

use super::*;

const POTX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../core/tests/fixtures/templates/brand.potx"
));

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-studio-library-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn files(&self) -> PathBuf {
        self.0.join("files")
    }
    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
    fn deck(&self, relative: &str, title: &str) -> PathBuf {
        let path = self.files().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec_pretty(&Deck::titled(title)).unwrap(),
        )
        .unwrap();
        path
    }
    fn doc(&self, relative: &str, title: &str, markdown: &str) {
        let mut doc = Doc::from_design(title, &nexus_core::design::Design::default());
        doc.blocks = doc::markdown::parse_document(markdown, true).blocks;
        let doc = Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap();
        doc::storage::save(&self.files(), relative, &doc, Who::Model).unwrap();
    }
    /// A backend (no file given: the library shows) with scripted dialogs.
    fn backend(&self, answers: impl IntoIterator<Item = Option<PathBuf>>) -> (Backend, Scripted) {
        let dialogs = Scripted::new(answers);
        let mut backend = Backend::launch(None, &self.files(), &self.data()).unwrap();
        backend.dialogs = Arc::new(dialogs.clone());
        (backend, dialogs)
    }
    fn opened(
        &self,
        path: &Path,
        answers: impl IntoIterator<Item = Option<PathBuf>>,
    ) -> (Backend, Scripted) {
        let dialogs = Scripted::new(answers);
        let mut backend = Backend::launch(Some(path), &self.files(), &self.data()).unwrap();
        backend.dialogs = Arc::new(dialogs.clone());
        (backend, dialogs)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn send(backend: &mut Backend, value: serde_json::Value) -> Event {
    backend.dispatch(&serde_json::to_string(&value).unwrap())
}

fn library(event: Event) -> (Vec<library::Item>, Option<String>, bool, Option<String>) {
    let Event::Library {
        items,
        current,
        home,
        selected,
        ..
    } = event
    else {
        panic!("{event:?}");
    };
    (items, current, home, selected)
}

#[test]
fn studio_started_with_no_file_shows_the_library_and_writes_nothing() {
    let temp = Temp::new();
    temp.deck("decks/talk.deck.json", "Talk");
    let (mut backend, _) = temp.backend([]);
    assert_eq!(
        fs::read_dir(temp.files().join("decks")).unwrap().count(),
        2,
        "the deck and assets/"
    );
    assert!(
        !backend.path.exists(),
        "a blank deck is not written until it is edited"
    );
    let Event::Deck { home, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("a deck event");
    };
    assert!(home);
    let (items, current, home, _) = library(send(&mut backend, json!({"command":"Library"})));
    assert!(home && current.is_none());
    assert_eq!(items.len(), 1);

    // The first real change writes the file and ends "home".
    let event = send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"add_slide","slide":{"id":"s1","children":[]}}]}),
    );
    let Event::Deck { home, .. } = event else {
        panic!("{event:?}");
    };
    assert!(!home);
    assert!(backend.path.exists());
}

#[test]
fn a_file_given_on_the_command_line_skips_the_library() {
    let temp = Temp::new();
    let path = temp.deck("decks/talk.deck.json", "Talk");
    let (mut backend, _) = temp.opened(&path, []);
    let Event::Deck { home, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("a deck event");
    };
    assert!(!home);
}

#[test]
fn the_library_lists_everything_without_a_single_absolute_path() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    temp.doc("docs/memo.doc.json", "Memo", "# Memo\n\nHello.\n");
    let (mut backend, _) = temp.opened(&talk, []);
    let event = send(&mut backend, json!({"command":"Library"}));
    let wire = event.script();
    assert!(
        !wire.contains(temp.0.to_str().unwrap()),
        "the page only ever sees paths inside the space: {wire}"
    );
    let (items, current, home, _) = library(event);
    assert!(!home);
    assert_eq!(current.as_deref(), Some("decks/talk.deck.json"));
    let mut relative: Vec<_> = items.iter().map(|i| i.relative.as_str()).collect();
    relative.sort_unstable();
    assert_eq!(relative, ["decks/talk.deck.json", "docs/memo.doc.json"]);
    let talk = items
        .iter()
        .find(|i| i.relative.starts_with("decks"))
        .unwrap();
    assert!(
        talk.current
            && talk
                .thumb
                .as_deref()
                .is_some_and(|t| t.starts_with("/library/"))
    );
    assert_eq!((talk.title.as_str(), talk.count), ("Talk", 1));
    assert!(
        !items
            .iter()
            .find(|i| i.relative.starts_with("docs"))
            .unwrap()
            .current
    );
}

#[test]
fn thumbnails_are_served_from_the_last_listing_and_nothing_else() {
    let temp = Temp::new();
    temp.deck("decks/talk.deck.json", "Talk");
    temp.doc("docs/memo.doc.json", "Memo", "# Memo\n\nHello.\n");
    let (mut backend, _) = temp.backend([]);
    let (items, ..) = library(send(&mut backend, json!({"command":"Library"})));
    for item in &items {
        let thumb = item.thumb.clone().unwrap();
        let (mime, bytes) = backend.serve(&format!("nexus://localhost{thumb}")).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(&bytes[..4], b"\x89PNG", "{thumb}");
        let (files, entry) = backend
            .thumbnail_job(&format!("nexus://localhost{thumb}"))
            .unwrap();
        assert_eq!(files, backend.files_dir);
        assert_eq!(entry.relative, item.relative);
    }
    assert!(
        backend
            .serve("nexus://localhost/library/000000000000-00000000.png")
            .is_err()
    );
    assert!(
        backend
            .serve("nexus://localhost/library/..%2Fx.png")
            .is_err()
    );
    assert!(
        backend
            .thumbnail_job("nexus://localhost/assets/x.png")
            .is_err()
    );
    // After the file changes, the old key is no longer served.
    let stale = items[0].thumb.clone().unwrap();
    let relative = items[0].relative.clone();
    send(
        &mut backend,
        json!({"command":"LibraryRename","path":relative,"title":"Changed"}),
    );
    let (fresh, ..) = library(send(&mut backend, json!({"command":"Library"})));
    let moved = fresh.iter().find(|i| i.relative == relative).unwrap();
    assert_ne!(moved.thumb.as_ref(), Some(&stale));
    assert!(backend.serve(&format!("nexus://localhost{stale}")).is_err());
}

#[test]
fn library_open_switches_between_decks_and_docs_and_keeps_undo_for_the_open_file() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    temp.doc("docs/memo.doc.json", "Memo", "# Memo\n\nHello.\n");
    let (mut backend, _) = temp.opened(&talk, []);
    let event = send(
        &mut backend,
        json!({"command":"LibraryOpen","path":"docs/memo.doc.json"}),
    );
    assert!(matches!(event, Event::Doc { home: false, .. }), "{event:?}");
    assert!(backend.is_doc());
    let event = send(
        &mut backend,
        json!({"command":"LibraryOpen","path":"decks/talk.deck.json"}),
    );
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    assert!(!backend.is_doc());
    // Edit, then "open" the same file: the undo history survives.
    send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"set_notes","slide":"s1","notes":"n"}]}),
    );
    assert_eq!(backend.undo.len(), 1);
    send(
        &mut backend,
        json!({"command":"LibraryOpen","path":"decks/talk.deck.json"}),
    );
    assert_eq!(backend.undo.len(), 1);
}

#[test]
fn library_paths_are_confined_to_the_space() {
    let temp = Temp::new();
    temp.deck("decks/talk.deck.json", "Talk");
    let outside = temp.0.join("outside.deck.json");
    fs::write(
        &outside,
        serde_json::to_vec(&Deck::titled("Outside")).unwrap(),
    )
    .unwrap();
    let hidden = temp.deck("decks/.history/old/x.deck.json", "Old");
    let (mut backend, _) = temp.backend([]);
    for bad in [
        "../outside.deck.json".to_string(),
        outside.to_string_lossy().into_owned(),
        hidden.to_string_lossy().into_owned(),
        "decks/.history/old/x.deck.json".into(),
        "decks/missing.deck.json".into(),
        "notes.txt".into(),
    ] {
        for command in [
            "LibraryOpen",
            "LibraryDelete",
            "LibraryDuplicate",
            "LibraryReveal",
        ] {
            assert!(
                matches!(
                    send(&mut backend, json!({"command": command, "path": bad})),
                    Event::Error { .. }
                ),
                "{command} {bad}"
            );
        }
        assert!(matches!(
            send(
                &mut backend,
                json!({"command":"LibraryRename","path":bad,"title":"x"})
            ),
            Event::Error { .. }
        ));
        assert!(matches!(
            send(
                &mut backend,
                json!({"command":"LibraryExport","path":bad,"format":"pptx"})
            ),
            Event::Error { .. }
        ));
    }
    assert!(outside.is_file() && hidden.is_file());
    // Commands outside the protocol are refused like any other.
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"LibraryOpen","path":"decks/talk.deck.json","extra":1})
        ),
        Event::Error { .. }
    ));
}

#[test]
fn rename_changes_the_title_of_any_file_and_keeps_the_open_one_undoable() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    temp.doc("docs/memo.doc.json", "Memo", "# Memo\n\nHello.\n");
    let (mut backend, _) = temp.opened(&talk, []);
    // Another file: the file changes, the window does not.
    let (items, ..) = library(send(
        &mut backend,
        json!({"command":"LibraryRename","path":"docs/memo.doc.json","title":"Q3 memo"}),
    ));
    assert_eq!(
        items
            .iter()
            .find(|i| i.relative == "docs/memo.doc.json")
            .unwrap()
            .title,
        "Q3 memo"
    );
    assert_eq!(backend.deck.title, "Talk");
    // The open deck: the window updates and Cmd+Z takes it back.
    let event = send(
        &mut backend,
        json!({"command":"LibraryRename","path":"decks/talk.deck.json","title":"Q3 review"}),
    );
    assert!(
        matches!(&event, Event::Deck { deck, .. } if deck.title == "Q3 review"),
        "{event:?}"
    );
    assert_eq!(read_deck(&talk).unwrap().title, "Q3 review");
    send(&mut backend, json!({"command":"Undo"}));
    assert_eq!(read_deck(&talk).unwrap().title, "Talk");
    // The open doc.
    send(
        &mut backend,
        json!({"command":"LibraryOpen","path":"docs/memo.doc.json"}),
    );
    let event = send(
        &mut backend,
        json!({"command":"LibraryRename","path":"docs/memo.doc.json","title":"Final memo"}),
    );
    assert!(
        matches!(&event, Event::Doc { doc, .. } if doc.title == "Final memo"),
        "{event:?}"
    );
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"LibraryRename","path":"docs/memo.doc.json","title":"  "})
        ),
        Event::Error { .. }
    ));
}

#[test]
fn duplicate_selects_the_new_copy() {
    let temp = Temp::new();
    temp.deck("decks/talk.deck.json", "Talk");
    let (mut backend, _) = temp.backend([]);
    let (items, _, _, selected) = library(send(
        &mut backend,
        json!({"command":"LibraryDuplicate","path":"decks/talk.deck.json"}),
    ));
    assert_eq!(selected.as_deref(), Some("decks/talk-copy.deck.json"));
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i.title == "Talk copy"));
}

#[test]
fn delete_moves_to_the_trash_and_the_open_file_hands_the_window_back_to_the_library() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    temp.deck("decks/other.deck.json", "Other");
    let (mut backend, _) = temp.opened(&talk, []);
    let (items, current, home, _) = library(send(
        &mut backend,
        json!({"command":"LibraryDelete","path":"decks/other.deck.json"}),
    ));
    assert_eq!(items.len(), 1);
    assert_eq!(current.as_deref(), Some("decks/talk.deck.json"));
    assert!(!home);
    assert!(fs::read_dir(temp.files().join(".trash")).unwrap().count() == 1);
    // Deleting the file that is open.
    let (items, current, home, _) = library(send(
        &mut backend,
        json!({"command":"LibraryDelete","path":"decks/talk.deck.json"}),
    ));
    assert!(items.is_empty() && current.is_none() && home);
    assert!(!talk.exists());
    let Event::Deck { home, .. } = send(&mut backend, json!({"command":"Load"})) else {
        panic!("a deck event");
    };
    assert!(home);
    assert!(!backend.path.exists(), "the placeholder is not a file");
    assert_eq!(
        fs::read_dir(temp.files().join(".trash")).unwrap().count(),
        2
    );
    // The window keeps working: a new deck is a real file again.
    let event = send(&mut backend, json!({"command":"NewDeck","title":"Fresh"}));
    assert!(
        matches!(event, Event::Deck { home: false, .. }),
        "{event:?}"
    );
}

#[test]
fn export_asks_where_pre_filled_with_the_title_and_writes_there() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Q3 review");
    let target = temp.0.join("Desktop/review.pptx");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let second = temp.0.join("Desktop/plain");
    let (mut backend, dialogs) = temp.opened(&talk, [Some(target.clone()), Some(second.clone())]);
    let event = send(&mut backend, json!({"command":"ExportAs","format":"pptx"}));
    let Event::Exported { path, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(path, target);
    assert_eq!(&fs::read(&target).unwrap()[..2], b"PK");
    let asked = dialogs.requests();
    assert!(asked[0].save);
    assert_eq!(asked[0].name, "Q3 review.pptx");
    assert_eq!(asked[0].filters, ["PowerPoint"]);
    assert_eq!(asked[0].start, None, "no export folder is known yet");
    assert!(
        !temp.files().join("exports").exists(),
        "nothing went to exports/"
    );
    // Reveal works for what the dialog wrote (and only for such files).
    assert!(backend.exports.contains(&path));

    // The folder is remembered, per space, for the next dialog; a missing
    // extension is added; an existing file is replaced.
    fs::write(&second.with_extension("pdf"), b"old").unwrap();
    let event = send(&mut backend, json!({"command":"ExportAs","format":"pdf"}));
    let Event::Exported { path, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(path, temp.0.join("Desktop/plain.pdf"));
    assert_eq!(&fs::read(&path).unwrap()[..4], b"%PDF");
    assert_eq!(
        dialogs.requests()[1].start.as_deref(),
        Some(temp.0.join("Desktop").as_path())
    );
    assert_eq!(
        nexus_core::canvas::settings::export_dir(&temp.data(), &backend.files_dir),
        Some(temp.0.join("Desktop"))
    );
    assert!(backend.undo.is_empty(), "exporting never edits");
}

#[test]
fn cancelling_a_dialog_does_nothing() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    let (mut backend, dialogs) = temp.opened(&talk, [None, None, None, None]);
    let before = backend.deck.clone();
    for command in [
        json!({"command":"ExportAs","format":"pptx"}),
        json!({"command":"SaveCopy"}),
        json!({"command":"OpenDialog"}),
        json!({"command":"PickTemplate"}),
    ] {
        let event = send(&mut backend, command.clone());
        assert!(matches!(event, Event::Idle {}), "{command}: {event:?}");
    }
    assert_eq!(dialogs.requests().len(), 4);
    assert_eq!(backend.deck, before);
    assert!(backend.exports.is_empty());
    assert!(!temp.files().join("exports").exists());
    assert!(nexus_core::canvas::settings::export_dir(&temp.data(), &backend.files_dir).is_none());
    assert_eq!(backend.path, talk);
}

#[test]
fn a_doc_exports_through_the_dialog_and_markdown_images_land_beside_it() {
    let temp = Temp::new();
    temp.doc(
        "docs/memo.doc.json",
        "Memo",
        "# Memo\n\nHello.\n\n![chart](assets/c.png)\n",
    );
    fs::create_dir_all(temp.files().join("docs/assets")).unwrap();
    image::RgbaImage::new(4, 4)
        .save(temp.files().join("docs/assets/c.png"))
        .unwrap();
    let out = temp.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let (mut backend, dialogs) = temp.opened(
        &temp.files().join("docs/memo.doc.json"),
        [Some(out.join("memo.md")), Some(out.join("memo"))],
    );
    let Event::Exported { path, .. } =
        send(&mut backend, json!({"command":"ExportAs","format":"md"}))
    else {
        panic!("exported");
    };
    assert_eq!(path, out.join("memo.md"));
    let text = fs::read_to_string(&path).unwrap();
    let image = text.split("](").nth(1).unwrap().split(')').next().unwrap();
    assert!(image.starts_with("assets/"), "{text}");
    assert!(out.join(image).is_file(), "the image sits beside the file");
    assert!(!temp.files().join("exports").exists());
    assert_eq!(dialogs.requests()[0].name, "Memo.md");

    let Event::Exported { path, .. } =
        send(&mut backend, json!({"command":"ExportAs","format":"docx"}))
    else {
        panic!("exported");
    };
    assert_eq!(path, out.join("memo.docx"));
    assert_eq!(&fs::read(path).unwrap()[..2], b"PK");
    assert!(
        matches!(
            send(&mut backend, json!({"command":"ExportAs","format":"pptx"})),
            Event::Error { .. }
        ),
        "a doc has no pptx"
    );
}

#[test]
fn a_library_file_exports_without_being_opened() {
    let temp = Temp::new();
    temp.deck("decks/talk.deck.json", "Talk");
    temp.doc("docs/memo.doc.json", "Memo", "# Memo\n\nHello.\n");
    let out = temp.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let (mut backend, dialogs) =
        temp.backend([Some(out.join("t.pdf")), Some(out.join("m.html")), None]);
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"LibraryExport","path":"decks/talk.deck.json","format":"pdf"})
        ),
        Event::Exported { .. }
    ));
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"LibraryExport","path":"docs/memo.doc.json","format":"html"})
        ),
        Event::Exported { .. }
    ));
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"LibraryExport","path":"docs/memo.doc.json","format":"pdf"})
        ),
        Event::Idle {}
    ));
    assert_eq!(&fs::read(out.join("t.pdf")).unwrap()[..4], b"%PDF");
    assert!(
        fs::read_to_string(out.join("m.html"))
            .unwrap()
            .contains("Memo")
    );
    let names: Vec<_> = dialogs.requests().into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["Talk.pdf", "Memo.html", "Memo.pdf"]);
    assert!(
        !backend.path.exists(),
        "the library window stayed on its placeholder"
    );
}

#[test]
fn save_a_copy_writes_the_file_with_the_assets_it_uses() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    fs::create_dir_all(temp.files().join("decks/assets")).unwrap();
    image::RgbaImage::new(4, 4)
        .save(temp.files().join("decks/assets/pic.png"))
        .unwrap();
    let (mut backend, _) = temp.opened(&talk, []);
    send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"insert_node","parent":"s1","node":
            {"id":"pic","kind":"image","src":"assets/pic.png","sizing":{"w":100,"h":100},"position":{"x":10,"y":10}}}]}),
    );
    let out = temp.0.join("Backups");
    fs::create_dir_all(&out).unwrap();
    let dialogs = Scripted::new([Some(out.join("talk-v2"))]);
    backend.dialogs = Arc::new(dialogs.clone());
    let event = send(&mut backend, json!({"command":"SaveCopy"}));
    let Event::Saved { path, assets } = event else {
        panic!("{event:?}");
    };
    assert_eq!(path, out.join("talk-v2.deck.json"));
    assert_eq!(assets, 1);
    assert!(out.join("assets/pic.png").is_file());
    assert_eq!(read_deck(&path).unwrap(), backend.deck);
    assert_eq!(backend.path, talk, "the window stays on the original");
    assert_eq!(dialogs.requests()[0].name, "Talk copy.deck.json");
}

#[test]
fn save_a_copy_is_pre_filled_and_never_the_open_file() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    let (mut backend, dialogs) = temp.opened(&talk, [Some(talk.clone())]);
    let event = send(&mut backend, json!({"command":"SaveCopy"}));
    assert!(matches!(event, Event::Error { .. }), "{event:?}");
    assert_eq!(dialogs.requests()[0].name, "Talk copy.deck.json");
    assert_eq!(dialogs.requests()[0].filters, ["JSON"]);
    assert_eq!(
        read_deck(&talk).unwrap().title,
        "Talk",
        "the open file is untouched"
    );
}

#[test]
fn open_dialog_opens_nexus_files_and_imports_office_and_markdown_files() {
    let temp = Temp::new();
    let outside = temp.0.join("Downloads");
    fs::create_dir_all(&outside).unwrap();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    temp.doc("docs/memo.doc.json", "Memo", "# Memo\n\nHello.\n");
    // Real packages come from the exporters, so the import paths see real files.
    let (mut backend, _) = temp.opened(&talk, []);
    let Event::Exported { path: pptx, .. } = send(
        &mut backend,
        json!({"command":"Export","format":"pptx","path":outside.join("deck.pptx")}),
    ) else {
        panic!("pptx");
    };
    send(
        &mut backend,
        json!({"command":"LibraryOpen","path":"docs/memo.doc.json"}),
    );
    let Event::Exported { path: docx, .. } = send(
        &mut backend,
        json!({"command":"DocExport","format":"docx","path":outside.join("memo.docx")}),
    ) else {
        panic!("docx");
    };
    let markdown = outside.join("notes.md");
    fs::write(&markdown, "# Field notes\n\nSome *notes*.\n").unwrap();
    let text = outside.join("readme.txt");
    fs::write(&text, "no").unwrap();

    let answers = [
        Some(temp.files().join("decks/talk.deck.json")),
        Some(temp.files().join("docs/memo.doc.json")),
        Some(pptx),
        Some(docx),
        Some(markdown),
        Some(text),
        None,
    ];
    let dialogs = Scripted::new(answers);
    backend.dialogs = Arc::new(dialogs.clone());

    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    assert!(
        matches!(event, Event::Deck { .. }) && backend.path == talk,
        "{event:?}"
    );
    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    assert!(
        matches!(event, Event::Doc { .. }) && backend.is_doc(),
        "{event:?}"
    );
    // A PowerPoint file becomes a new deck in the space, then opens.
    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    assert!(backend.path.starts_with(temp.files().join("decks")));
    assert_ne!(backend.path, talk);
    assert!(!backend.is_doc());
    // The approximations of the import are reported once, with that event.
    let Event::Deck {
        import_warnings, ..
    } = send(&mut backend, json!({"command":"Load"}))
    else {
        panic!("a deck event");
    };
    assert!(import_warnings.is_empty(), "{import_warnings:?}");
    // A Word file and a Markdown file become new docs, with a notice.
    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    let Event::Doc {
        doc,
        notice,
        relative,
        ..
    } = event
    else {
        panic!("a doc");
    };
    assert_eq!(doc.title, "Memo");
    assert_eq!(
        relative, "docs/memo-2.doc.json",
        "the name taken by the first memo is not reused"
    );
    assert!(notice.unwrap().starts_with("Imported memo.docx"));
    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    let Event::Doc { doc, relative, .. } = event else {
        panic!("a doc");
    };
    assert_eq!(
        (doc.title.as_str(), relative.as_str()),
        ("Field notes", "docs/field-notes.doc.json")
    );
    // Anything else is refused, and a cancel changes nothing.
    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    assert!(matches!(event, Event::Error { .. }), "{event:?}");
    let event = send(&mut backend, json!({"command":"OpenDialog"}));
    assert!(matches!(event, Event::Idle {}), "{event:?}");
    assert!(dialogs.requests().iter().all(|r| !r.save));
    assert_eq!(
        dialogs.requests()[0].start.as_deref(),
        Some(backend.files_dir.canonicalize().unwrap().as_path())
    );
    assert!(
        dialogs.requests()[0]
            .filters
            .contains(&"Decks, docs and imports")
    );
}

#[test]
fn add_template_uses_the_open_dialog_and_imports_the_chosen_file() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    let source = temp.0.join("brand.potx");
    fs::write(&source, POTX).unwrap();
    let (mut backend, dialogs) = temp.opened(&talk, [Some(source)]);
    let before = backend.deck.clone();
    let event = send(&mut backend, json!({"command":"PickTemplate"}));
    assert!(
        matches!(&event, Event::TemplateImported { slug, .. } if slug == "brand"),
        "{event:?}"
    );
    assert_eq!(backend.deck, before, "importing never applies a design");
    let asked = dialogs.requests();
    assert_eq!(asked.len(), 1);
    assert!(!asked[0].save);
    assert_eq!(asked[0].filters, ["Office templates"]);
}

#[test]
fn dialog_paths_never_reach_the_page_except_as_the_result_of_an_export() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    let out = temp.0.join("Secret folder");
    fs::create_dir_all(&out).unwrap();
    let (mut backend, _) = temp.opened(&talk, [Some(out.join("a.pptx"))]);
    // The listing, the deck event and cancelled dialogs carry no outside path.
    for command in [json!({"command":"Library"}), json!({"command":"Load"})] {
        let wire = send(&mut backend, command).script();
        assert!(!wire.contains("Secret folder"), "{wire}");
    }
    // The Exported event names the file the user just chose, for Reveal.
    let event = send(&mut backend, json!({"command":"ExportAs","format":"pptx"}));
    let Event::Exported { path, .. } = event else {
        panic!("exported");
    };
    assert!(path.starts_with(&out));
    // And Reveal refuses a file that was not exported by this window.
    assert!(matches!(
        send(&mut backend, json!({"command":"Reveal","path":talk})),
        Event::Error { .. }
    ));
}

#[test]
fn the_library_page_is_embedded_and_builds_only_text_nodes() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    for (uri, file, mime) in [
        ("/library.js", "library.js", "text/javascript"),
        ("/library.css", "library.css", "text/css; charset=utf-8"),
    ] {
        let (served_mime, served) =
            embedded(uri).unwrap_or_else(|| panic!("{uri} is not embedded"));
        assert_eq!(served_mime, mime);
        assert_eq!(served, fs::read_to_string(ui.join(file)).unwrap());
    }
    // Titles come from files the model may have written: never markup.
    let source = embedded("/library.js").unwrap().1;
    for forbidden in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "fetch(",
        "XMLHttpRequest",
        "WebSocket",
    ] {
        assert!(!source.contains(forbidden), "library.js uses {forbidden}");
    }
    // Every import resolves to an embedded module.
    for line in source
        .lines()
        .filter(|l| l.trim_start().starts_with("import "))
    {
        let target = line.split(['\'', '"']).nth(1).unwrap();
        let resolved = format!("/{}", target.trim_start_matches("./"));
        assert!(embedded(&resolved).is_some(), "library.js imports {target}");
    }
    // The page links the stylesheet and holds the mount point before the
    // presentation markup that the HTML export embeds.
    let (_, index) = embedded("/index.html").unwrap();
    assert!(index.contains("href=\"/library.css\""));
    assert!(
        index.find("id=\"library\"").unwrap() < index.find("id=\"presentation\"").unwrap(),
        "the library mount stays out of the exported presentation"
    );
    // The window can only reach the library through the embedded page: no new
    // remote origin was added to its policy (the policy test pins the rest).
    assert!(!source.contains("http://") && !source.contains("https://"));
}

#[test]
fn a_dialog_is_shown_off_the_backend_and_finished_with_the_answer() {
    let temp = Temp::new();
    let talk = temp.deck("decks/talk.deck.json", "Talk");
    let out = temp.0.join("Desktop");
    fs::create_dir_all(&out).unwrap();
    let (mut backend, dialogs) = temp.opened(&talk, [Some(out.join("t.pptx")), None]);
    // Only the dialog commands become jobs; anything else (or nonsense) is for `dispatch`.
    for source in [
        r#"{"command":"Library"}"#,
        r#"{"command":"Undo"}"#,
        "not json",
        r#"{"command":"ExportAs","format":"pptx","extra":1}"#,
    ] {
        assert!(backend.prepare_dialog(source).is_none(), "{source}");
    }
    // A job that cannot start answers with its error.
    assert!(
        backend
            .prepare_dialog(
                r#"{"command":"LibraryExport","path":"../x.deck.json","format":"pptx"}"#
            )
            .unwrap()
            .is_err()
    );
    assert!(
        backend
            .prepare_dialog(r#"{"command":"ExportAs","format":"mp3"}"#)
            .unwrap()
            .is_err()
    );
    let job = backend
        .prepare_dialog(r#"{"command":"ExportAs","format":"pptx"}"#)
        .unwrap()
        .unwrap();
    // The dialog runs on another thread, as the window does it; meanwhile the
    // backend answers anything else.
    let shown = std::thread::spawn(move || job.ask());
    let during = send(&mut backend, json!({"command":"Library"}));
    assert!(matches!(during, Event::Library { .. }));
    let answer = shown.join().unwrap();
    let Event::Exported { path, .. } = backend.finish_dialog(answer) else {
        panic!("exported");
    };
    assert_eq!(path, out.join("t.pptx"));
    // A cancel finishes as Idle.
    let job = backend
        .prepare_dialog(r#"{"command":"SaveCopy"}"#)
        .unwrap()
        .unwrap();
    let event = backend.finish_dialog(job.ask());
    assert!(matches!(event, Event::Idle {}), "{event:?}");
    assert_eq!(dialogs.requests().len(), 2);
    // And a failure after the dialog is an Error event, not a panic.
    let bad = Scripted::new([Some(out.join("missing-folder/x.pptx"))]);
    backend.dialogs = Arc::new(bad);
    let job = backend
        .prepare_dialog(r#"{"command":"ExportAs","format":"pdf"}"#)
        .unwrap()
        .unwrap();
    assert!(matches!(
        backend.finish_dialog(job.ask()),
        Event::Error { .. }
    ));
}
