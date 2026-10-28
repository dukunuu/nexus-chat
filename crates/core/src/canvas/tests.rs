use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{TimeZone as _, Utc};

use super::dialogs::{Dialogs as _, Filter, Scripted};
use super::*;
use crate::deck::link::Kind;

struct Space(PathBuf);

impl Space {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-library-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn put(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn deck(&self, relative: &str, title: &str) -> PathBuf {
        self.put(
            relative,
            &serde_json::to_vec_pretty(&Deck::titled(title)).unwrap(),
        )
    }

    fn doc(&self, relative: &str, title: &str) -> PathBuf {
        let mut doc = Doc::from_design(title, &crate::design::Design::default());
        doc.blocks = crate::doc::markdown::parse_document(
            &format!("# {title}\n\nA paragraph of text.\n"),
            true,
        )
        .blocks;
        let doc = Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap();
        self.put(relative, &serde_json::to_vec_pretty(&doc).unwrap())
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn age(path: &Path, seconds_ago: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(seconds_ago))
        .unwrap();
}

#[test]
fn the_library_lists_decks_and_docs_newest_first() {
    let space = Space::new();
    let old = space.deck("decks/old.deck.json", "Old deck");
    let newest = space.doc("docs/newest.doc.json", "Newest doc");
    let middle = space.deck("decks/talks/middle.deck.json", "Middle deck");
    age(&old, 3000);
    age(&middle, 200);
    age(&newest, 10);

    let entries = library(&space.0);
    let order: Vec<_> = entries.iter().map(|e| e.relative.as_str()).collect();
    assert_eq!(
        order,
        [
            "docs/newest.doc.json",
            "decks/talks/middle.deck.json",
            "decks/old.deck.json"
        ]
    );
    let doc = &entries[0];
    assert_eq!(doc.kind, Kind::Doc);
    assert_eq!(doc.title, "Newest doc");
    assert_eq!(doc.count, 2, "a heading and a paragraph");
    assert_eq!(doc.path, newest);
    assert_eq!(doc.count_label(), "2 blocks");
    assert_eq!(entries[1].kind, Kind::Deck);
    assert_eq!(entries[1].count_label(), "1 slide");
    assert!(entries[1].design.is_none() || entries[1].design.as_deref() == Some("nexus"));
    assert!(entries.iter().all(|e| e.revision.len() == 64));
}

#[test]
fn history_renders_assets_trash_hidden_links_and_broken_files_are_not_listed() {
    let space = Space::new();
    space.deck("decks/real.deck.json", "Real");
    space.deck("decks/.history/real/20260101-abc-user.deck.json", "Kept");
    space.deck("decks/.renders/real/x.deck.json", "Cached");
    space.deck("decks/assets/stray.deck.json", "Stray");
    space.deck("decks/.hidden.deck.json", "Hidden");
    space.deck(".trash/20260101/gone.deck.json", "Trashed");
    space.deck("elsewhere/outside.deck.json", "Outside the folders");
    space.doc("docs/.history/r/20260101-abc-user.doc.json", "Kept doc");
    space.put("decks/broken.deck.json", b"{ not json");
    space.put("decks/notes.txt", b"hello");
    #[cfg(unix)]
    {
        let target = space.deck("outside-the-library.deck.json", "Linked");
        std::os::unix::fs::symlink(&target, space.0.join("decks/link.deck.json")).unwrap();
        std::os::unix::fs::symlink(space.0.join("elsewhere"), space.0.join("decks/dir-link"))
            .unwrap();
    }
    let entries = library(&space.0);
    assert_eq!(
        entries
            .iter()
            .map(|e| e.relative.as_str())
            .collect::<Vec<_>>(),
        ["decks/real.deck.json"]
    );
    assert!(library(&space.0.join("missing")).is_empty());
}

#[test]
fn thumbnails_are_cached_by_revision_and_rendered_lazily() {
    let space = Space::new();
    space.deck("decks/talk.deck.json", "Talk");
    space.doc("docs/memo.doc.json", "Memo");
    let entries = library(&space.0);
    let deck = entries.iter().find(|e| e.kind == Kind::Deck).unwrap();
    let doc = entries.iter().find(|e| e.kind == Kind::Doc).unwrap();

    let deck_thumb = deck.thumbnail.clone().unwrap();
    let rev8 = &deck.revision[..8];
    assert_eq!(
        deck_thumb,
        space.0.join(format!("decks/.renders/talk/7331-{rev8}.png")),
        "the first slide's render, named as the canvas tool names it (hex of the slide id)"
    );
    assert_eq!(
        doc.thumbnail.clone().unwrap(),
        space.0.join(format!(
            "docs/.renders/memo/thumb-{}.png",
            &doc.revision[..8]
        ))
    );
    assert!(!deck_thumb.exists(), "listing never renders");

    assert_eq!(ensure_thumbnail(&space.0, deck).unwrap(), deck_thumb);
    let bytes = std::fs::read(&deck_thumb).unwrap();
    assert_eq!(&bytes[..4], b"\x89PNG");
    let modified = std::fs::metadata(&deck_thumb).unwrap().modified().unwrap();
    assert_eq!(ensure_thumbnail(&space.0, deck).unwrap(), deck_thumb);
    assert_eq!(
        std::fs::metadata(&deck_thumb).unwrap().modified().unwrap(),
        modified,
        "a cached thumbnail is not drawn again"
    );

    let doc_thumb = ensure_thumbnail(&space.0, doc).unwrap();
    assert_eq!(&std::fs::read(&doc_thumb).unwrap()[..4], b"\x89PNG");

    // Editing changes the revision, so the key moves and the old doc image goes.
    files::rename(&space.0, "decks/talk.deck.json", "Talk, revised").unwrap();
    files::rename(&space.0, "docs/memo.doc.json", "Memo, revised").unwrap();
    let after = library(&space.0);
    let deck2 = after.iter().find(|e| e.kind == Kind::Deck).unwrap();
    let doc2 = after.iter().find(|e| e.kind == Kind::Doc).unwrap();
    assert_ne!(deck2.thumbnail, deck.thumbnail);
    assert_ne!(doc2.thumbnail, doc.thumbnail);
    let doc_thumb2 = ensure_thumbnail(&space.0, doc2).unwrap();
    assert!(doc_thumb2.is_file() && !doc_thumb.exists());

    // Drawing the new slide retires the old one, and a stale entry cannot draw
    // the newer file under its old key.
    let deck_thumb2 = ensure_thumbnail(&space.0, deck2).unwrap();
    assert_eq!(Some(deck_thumb2), deck2.thumbnail.clone());
    assert!(!deck_thumb.exists());
    assert!(ensure_thumbnail(&space.0, deck).is_err());
}

#[test]
fn an_empty_deck_has_no_thumbnail() {
    let space = Space::new();
    let mut deck = Deck::titled("Empty");
    deck.slides.clear();
    space.put("decks/empty.deck.json", &serde_json::to_vec(&deck).unwrap());
    let entries = library(&space.0);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].count, 0);
    assert!(entries[0].thumbnail.is_none());
    assert!(ensure_thumbnail(&space.0, &entries[0]).is_err());
}

#[test]
fn relative_times_read_naturally() {
    let now = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
    let ago = |seconds| relative_time(now - chrono::Duration::seconds(seconds), now);
    assert_eq!(ago(5), "just now");
    assert_eq!(ago(-30), "just now");
    assert_eq!(ago(60), "1 minute ago");
    assert_eq!(ago(59 * 60), "59 minutes ago");
    assert_eq!(ago(3600), "1 hour ago");
    assert_eq!(ago(5 * 3600), "5 hours ago");
    assert_eq!(ago(30 * 3600), "yesterday");
    assert_eq!(ago(3 * 86_400), "3 days ago");
    assert_eq!(ago(40 * 86_400), "18 Sep 2026");
}

#[test]
fn resolve_accepts_only_visible_library_files() {
    let space = Space::new();
    let deck = space.deck("decks/talk.deck.json", "Talk");
    space.deck("decks/.history/x/old.deck.json", "Old");
    let doc = space.doc("docs/sub/memo.doc.json", "Memo");
    assert_eq!(
        files::resolve(&space.0, "decks/talk.deck.json").unwrap(),
        (Kind::Deck, deck)
    );
    assert_eq!(
        files::resolve(&space.0, "docs/sub/memo.doc.json").unwrap(),
        (Kind::Doc, doc)
    );
    for bad in [
        "",
        "talk.deck.json",
        "decks/../decks/talk.deck.json",
        "decks/.history/x/old.deck.json",
        "decks/assets/x.deck.json",
        "decks/missing.deck.json",
        "decks/talk.doc.json",
        "docs/talk.deck.json",
        "/etc/passwd",
        "decks\\talk.deck.json",
        "decks//talk.deck.json",
    ] {
        assert!(files::resolve(&space.0, bad).is_err(), "{bad:?}");
    }
}

#[test]
fn rename_changes_the_title_and_keeps_the_file_and_its_history() {
    let space = Space::new();
    space.deck("decks/talk.deck.json", "Talk");
    space.doc("docs/memo.doc.json", "Memo");
    files::rename(&space.0, "decks/talk.deck.json", "  Q3 review  ").unwrap();
    files::rename(&space.0, "docs/memo.doc.json", "Q3 *memo*").unwrap();
    let entries = library(&space.0);
    let titles: Vec<_> = entries
        .iter()
        .map(|e| (e.relative.as_str(), e.title.as_str()))
        .collect();
    assert!(
        titles.contains(&("decks/talk.deck.json", "Q3 review")),
        "{titles:?}"
    );
    assert!(
        titles.contains(&("docs/memo.doc.json", "Q3 *memo*")),
        "{titles:?}"
    );
    let doc =
        Doc::check(&std::fs::read_to_string(space.0.join("docs/memo.doc.json")).unwrap()).unwrap();
    let crate::doc::Kind::Title { text } = &doc.blocks[0].kind else {
        panic!("{:?}", doc.blocks[0]);
    };
    assert_eq!(
        crate::doc::markdown::plain(text),
        "Q3 *memo*",
        "the title block follows"
    );
    let kept = crate::deck::history::list(&space.0.join("decks/talk.deck.json")).unwrap();
    assert_eq!(kept.len(), 1, "the replaced version is kept");
    for bad in ["", "   ", "tab\there"] {
        assert!(files::rename(&space.0, "decks/talk.deck.json", bad).is_err());
    }
    assert!(files::rename(&space.0, "decks/talk.deck.json", &"x".repeat(201)).is_err());
}

#[test]
fn duplicate_makes_a_titled_copy_beside_the_original() {
    let space = Space::new();
    space.deck("decks/talk.deck.json", "Talk");
    space.doc("docs/memo.doc.json", "Memo");
    space.put("decks/talk.deck.json.comments.json", b"[]");
    assert_eq!(
        files::duplicate(&space.0, "decks/talk.deck.json").unwrap(),
        "decks/talk-copy.deck.json"
    );
    assert_eq!(
        files::duplicate(&space.0, "decks/talk.deck.json").unwrap(),
        "decks/talk-copy-2.deck.json"
    );
    assert_eq!(
        files::duplicate(&space.0, "docs/memo.doc.json").unwrap(),
        "docs/memo-copy.doc.json"
    );
    let entries = library(&space.0);
    let title = |relative: &str| {
        entries
            .iter()
            .find(|e| e.relative == relative)
            .unwrap()
            .title
            .clone()
    };
    assert_eq!(title("decks/talk-copy.deck.json"), "Talk copy");
    assert_eq!(title("docs/memo-copy.doc.json"), "Memo copy");
    assert_eq!(title("decks/talk.deck.json"), "Talk");
    assert!(
        !space
            .0
            .join("decks/talk-copy.deck.json.comments.json")
            .exists()
    );
    let copy =
        Doc::check(&std::fs::read_to_string(space.0.join("docs/memo-copy.doc.json")).unwrap())
            .unwrap();
    let crate::doc::Kind::Title { text } = &copy.blocks[0].kind else {
        panic!("{:?}", copy.blocks[0]);
    };
    assert_eq!(crate::doc::markdown::plain(text), "Memo copy");
}

#[test]
fn delete_moves_the_file_with_its_comments_and_history_to_the_trash() {
    let space = Space::new();
    space.deck("decks/talk.deck.json", "Talk");
    space.deck("decks/keep.deck.json", "Keep");
    space.put("decks/talk.deck.json.comments.json", b"[]");
    space.put("decks/.history/talk/20260101-aaaaaaaa-user.json", b"{}");
    space.put("decks/.renders/talk/x.png", b"png");
    let folder = files::delete(&space.0, "decks/talk.deck.json").unwrap();
    assert!(folder.starts_with(space.0.join(".trash")));
    assert!(folder.join("talk.deck.json").is_file());
    assert!(folder.join("talk.deck.json.comments.json").is_file());
    assert!(folder.join("history/20260101-aaaaaaaa-user.json").is_file());
    assert!(!space.0.join("decks/talk.deck.json").exists());
    assert!(!space.0.join("decks/.renders/talk").exists());
    let left: Vec<_> = library(&space.0).into_iter().map(|e| e.relative).collect();
    assert_eq!(left, ["decks/keep.deck.json"]);
    assert!(
        files::delete(&space.0, "decks/talk.deck.json").is_err(),
        "already gone"
    );
    assert!(files::delete(&space.0, "../x.deck.json").is_err());
    // A second delete of a same-named deck does not collide in the trash.
    space.deck("decks/talk.deck.json", "Talk again");
    let second = files::delete(&space.0, "decks/talk.deck.json").unwrap();
    assert_ne!(second, folder);
}

#[test]
fn save_copy_writes_the_file_with_the_assets_it_uses() {
    let space = Space::new();
    let png = {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(4, 4)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    };
    space.put("decks/assets/used.png", &png);
    space.put("decks/assets/unused.png", &png);
    let mut deck = Deck::titled("Talk");
    deck.slides[0].children.push(
        serde_json::from_value(serde_json::json!({
            "id": "pic", "kind": "image", "src": "assets/used.png"
        }))
        .unwrap(),
    );
    let source = space.put(
        "decks/talk.deck.json",
        &serde_json::to_vec_pretty(&deck).unwrap(),
    );
    let target = space.0.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();

    // The extension is made right whatever the dialog returned.
    let saved = files::save_copy(&source, &target.join("backup.json")).unwrap();
    assert_eq!(saved.path, target.join("backup.deck.json"));
    assert_eq!(saved.assets, 1);
    assert!(target.join("assets/used.png").is_file());
    assert!(!target.join("assets/unused.png").exists());
    assert_eq!(
        std::fs::read(&saved.path).unwrap(),
        std::fs::read(&source).unwrap()
    );
    // Saving again replaces the copy and never touches an asset already there.
    let again = files::save_copy(&source, &target.join("backup.deck.json")).unwrap();
    assert_eq!((again.path, again.assets), (saved.path, 0));
    // Not onto the file being edited, and the folder must exist.
    assert!(files::save_copy(&source, &source).is_err());
    assert!(files::save_copy(&source, &space.0.join("nope/x.deck.json")).is_err());
}

#[test]
fn save_copy_of_a_doc_uses_the_doc_suffix() {
    let space = Space::new();
    let source = space.doc("docs/memo.doc.json", "Memo");
    let saved = files::save_copy(&source, &space.0.join("memo2")).unwrap();
    assert_eq!(saved.path, space.0.join("memo2.doc.json"));
    assert!(Doc::check(&std::fs::read_to_string(saved.path).unwrap()).is_ok());
}

#[test]
fn write_chosen_replaces_what_the_dialog_confirmed() {
    let space = Space::new();
    let target = space.0.join("out.pptx");
    std::fs::write(&target, b"old").unwrap();
    assert_eq!(files::write_chosen(b"new", &target).unwrap(), target);
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(files::write_chosen(b"x", Path::new("relative.pptx")).is_err());
    assert!(files::write_chosen(b"x", &space.0.join("a/../b.pptx")).is_err());
    assert!(files::write_chosen(b"x", &space.0.join("missing/b.pptx")).is_err());
    assert!(files::write_chosen(b"x", &space.0).is_err());
}

#[test]
fn export_names_come_from_the_title() {
    assert_eq!(
        files::export_file_name("Q3 review", "pptx"),
        "Q3 review.pptx"
    );
    assert_eq!(files::export_file_name("a/b: c?*", "pdf"), "a-b- c--.pdf");
    assert_eq!(files::export_file_name("  ..  ", "docx"), "Untitled.docx");
    assert_eq!(
        files::with_extension(PathBuf::from("/x/report"), "pdf"),
        PathBuf::from("/x/report.pdf")
    );
    assert_eq!(
        files::with_extension(PathBuf::from("/x/report.final"), "pdf"),
        PathBuf::from("/x/report.final")
    );
}

#[test]
fn the_last_export_folder_is_remembered_per_space() {
    let data = Space::new();
    let one = Space::new();
    let two = Space::new();
    let folder = data.0.join("exports-here");
    std::fs::create_dir_all(&folder).unwrap();
    assert_eq!(settings::export_dir(&data.0, &one.0), None);
    settings::remember_export(&data.0, &one.0, &folder.join("a.pptx")).unwrap();
    assert_eq!(settings::export_dir(&data.0, &one.0), Some(folder.clone()));
    assert_eq!(settings::export_dir(&data.0, &two.0), None);
    std::fs::remove_dir(&folder).unwrap();
    assert_eq!(
        settings::export_dir(&data.0, &one.0),
        None,
        "a folder that is gone is forgotten"
    );
}

#[test]
fn scripted_dialogs_answer_in_order_and_remember_what_they_were_asked() {
    let dialogs = Scripted::new([Some(PathBuf::from("/a.pptx")), None]);
    let filters = [Filter {
        name: "PowerPoint",
        extensions: &["pptx"],
    }];
    assert_eq!(
        dialogs.save_file("Export", Some(Path::new("/start")), "Q3.pptx", &filters),
        Some(PathBuf::from("/a.pptx"))
    );
    assert_eq!(dialogs.open_file("Open", None, &filters), None);
    assert_eq!(
        dialogs.open_file("Open", None, &filters),
        None,
        "an empty queue cancels"
    );
    let asked = dialogs.requests();
    assert_eq!(asked.len(), 3);
    assert!(asked[0].save);
    assert_eq!(asked[0].name, "Q3.pptx");
    assert_eq!(asked[0].start.as_deref(), Some(Path::new("/start")));
    assert_eq!(asked[0].filters, ["PowerPoint"]);
    assert!(!asked[1].save);
    assert!(dialogs.available());
    assert!(!Scripted::unavailable().available());
}
