use super::*;
use serde_json::json;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-studio-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn backend(&self) -> Backend {
        Backend::start(None, &self.0.join("files"), &self.0.join("data")).unwrap()
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
fn success(event: Event) {
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
}

#[test]
fn command_parser_rejects_unknown_and_malformed_messages() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    for source in [
        "null",
        "{}",
        r#"{"command":"DeleteEverything"}"#,
        r#"{"command":"Load","path":"/etc/passwd"}"#,
        r#"{"command":"Open"}"#,
        r#"{"command":"Apply","ops":[{"op":"launch_shell"}]}"#,
    ] {
        assert!(
            matches!(backend.dispatch(source), Event::Error { .. }),
            "{source}"
        );
    }
    for value in [
        json!({"command":"Load"}),
        json!({"command":"Apply", "ops":[]}),
        json!({"command":"Undo"}),
        json!({"command":"Redo"}),
    ] {
        success(send(&mut backend, value));
    }
    let before = backend.deck.clone();
    let mut invalid = serde_json::to_value(&before).unwrap();
    invalid["version"] = json!(3);
    assert!(matches!(
        send(&mut backend, json!({"command":"Save","deck":invalid})),
        Event::Error { .. }
    ));
    assert_eq!(backend.deck, before);
}

#[test]
fn protocol_only_serves_embedded_ui_and_safe_images() {
    let temp = Temp::new();
    let backend = temp.backend();
    let assets = asset_root(&backend.path).unwrap();
    fs::write(assets.join("test.png"), b"image bytes").unwrap();
    fs::write(assets.join("a chart.png"), b"chart bytes").unwrap();
    assert_eq!(
        backend
            .serve("nexus://localhost/assets/a%20chart.png")
            .unwrap()
            .1,
        b"chart bytes"
    );
    assert_eq!(
        backend
            .serve("nexus://localhost/assets/test.png")
            .unwrap()
            .1,
        b"image bytes"
    );
    assert_eq!(
        backend.serve("nexus://localhost/index.html").unwrap().0,
        "text/html; charset=utf-8"
    );
    for module in ["app", "canvas", "strip", "panel", "render", "helpers"] {
        assert_eq!(
            backend
                .serve(&format!("nexus://localhost/{module}.js"))
                .unwrap()
                .0,
            "text/javascript"
        );
    }
    for uri in [
        "nexus://localhost/assets/../test.png",
        "nexus://localhost/assets//tmp/x.png",
        "nexus://localhost/assets/%2e%2e/x.png",
        "nexus://localhost/assets/%252e%252e/x.png",
        "nexus://localhost/assets/%2fetc/passwd",
        "nexus://localhost/assets/%ff.png",
        "nexus://localhost/assets/%2.png",
        "nexus://localhost/assets/C:\\x.png",
        "nexus://localhost//etc/passwd",
        "nexus://localhost/etc/passwd",
        "file:///etc/passwd",
        "nexus://other/index.html",
        "nexus://localhost.evil/index.html",
        "https://example.com/x.png",
        "nexus://localhost/assets/test.png?x=1",
        "nexus://localhost/assets/test.svg",
    ] {
        assert!(backend.serve(uri).is_err(), "{uri}");
    }
    assert!(backend.serve("http://nexus.localhost/index.html").is_ok());
}

#[cfg(unix)]
#[test]
fn assets_reject_escaping_symlinks() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let backend = temp.backend();
    let outside = temp.0.join("outside.png");
    fs::write(&outside, b"private").unwrap();
    let assets = asset_root(&backend.path).unwrap();
    symlink(&outside, assets.join("escape.png")).unwrap();
    symlink(&temp.0, assets.join("nested")).unwrap();
    assert!(
        backend
            .serve("nexus://localhost/assets/escape.png")
            .is_err()
    );
    assert!(
        backend
            .serve("nexus://localhost/assets/nested/outside.png")
            .is_err()
    );
    symlink(&temp.0, assets.join("images")).unwrap();
    assert!(backend.serve("nexus://localhost/assets/test.png").is_err());
}

#[test]
fn autosave_and_save_are_atomic_and_reload_exactly() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[{"op":"set_notes", "slide":"s1", "notes":"A speaker note"}]}),
    ));
    // Autosave: the edit is in the file before anyone asks to save.
    assert_eq!(read_deck(&backend.path).unwrap(), backend.deck);
    let deck = backend.deck.clone();
    success(send(&mut backend, json!({"command":"Save", "deck":deck})));
    assert_eq!(read_deck(&backend.path).unwrap(), deck);
    assert!(
        fs::read_dir(backend.path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
    );
    let old = backend.deck.clone();
    let failure_path = temp.0.join("a-directory");
    fs::create_dir(&failure_path).unwrap();
    backend.path = failure_path;
    let mut changed = old.clone();
    changed.title = "Not saved".into();
    assert!(matches!(
        send(&mut backend, json!({"command":"Save", "deck":changed})),
        Event::Error { .. }
    ));
    assert_eq!(backend.deck, old);
    assert!(fs::read_dir(&temp.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn undo_redo_is_exact_across_mixed_transactions_and_autosaved() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let original = backend.deck.clone();
    let hash = original.node("b1").unwrap().hash();
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[
            {"op":"update_node", "id":"b1", "fields":{"text":"A new headline", "position":{"x":300, "y":200}}, "base_hash":hash},
            {"op":"set_notes", "slide":"s2", "notes":"Tell a story"},
            {"op":"move_slide", "id":"s2", "to":0}
        ]}),
    ));
    let middle = backend.deck.clone();
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[{"op":"delete_slide", "id":"s1"}, {"op":"set_layout", "target":"s2", "layout":{"mode":"column", "gap":24}}]}),
    ));
    let last = backend.deck.clone();
    success(send(&mut backend, json!({"command":"Save", "deck":last})));
    for expected in [&middle, &original] {
        success(send(&mut backend, json!({"command":"Undo"})));
        assert_eq!(&backend.deck, expected);
        assert_eq!(
            &read_deck(&backend.path).unwrap(),
            expected,
            "undo is autosaved too"
        );
    }
    for expected in [&middle, &last] {
        success(send(&mut backend, json!({"command":"Redo"})));
        assert_eq!(&backend.deck, expected);
        assert_eq!(&read_deck(&backend.path).unwrap(), expected);
    }
    success(send(&mut backend, json!({"command":"Undo"})));
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[{"op":"set_notes","slide":"s1","notes":"New branch"}]}),
    ));
    let branched = backend.deck.clone();
    success(send(&mut backend, json!({"command":"Redo"})));
    assert_eq!(backend.deck, branched);
}

#[test]
fn invalid_apply_does_not_change_deck_or_history() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let original = backend.deck.clone();
    let event = send(
        &mut backend,
        json!({"command":"Apply", "ops":[{"op":"set_notes","slide":"s1","notes":"Not committed"},{"op":"delete_slide","id":"missing"}]}),
    );
    assert!(matches!(event, Event::Error { .. }));
    assert_eq!(backend.deck, original);
    assert!(backend.undo.is_empty());
    assert!(backend.redo.is_empty());
}

#[test]
fn remembers_last_user_path_and_new_decks_stay_in_space() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert_eq!(backend.path, temp.0.join("files/decks/untitled.deck.json"));
    let outside = temp.0.join("explicit.deck.json");
    let doc = Deck::new_from_layouts("An explicit file");
    atomic_write(&outside, &serde_json::to_vec(&doc).unwrap()).unwrap();
    success(send(
        &mut backend,
        json!({"command":"Open", "path":outside}),
    ));
    let mut reopened = temp.backend();
    assert_eq!(reopened.deck, doc);
    assert_eq!(reopened.path, outside);
    success(send(
        &mut reopened,
        json!({"command":"NewDeck", "title":"../../not-a-path"}),
    ));
    assert!(reopened.path.starts_with(temp.0.join("files/decks")));
    assert_ne!(reopened.path, outside);
    assert_eq!(reopened.deck.title, "../../not-a-path");
    assert_eq!(read_deck(&outside).unwrap(), doc);
    assert_eq!(temp.backend().path, reopened.path);
    assert!(matches!(
        send(
            &mut reopened,
            json!({"command":"Open", "path":"../outside.deck.json"})
        ),
        Event::Error { .. }
    ));
}

#[cfg(unix)]
#[test]
fn new_deck_rejects_a_space_directory_symlink_escape() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let files = temp.0.join("files");
    let outside = temp.0.join("outside");
    fs::create_dir(&files).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, files.join("decks")).unwrap();
    assert!(Backend::start(None, &files, &temp.0.join("data")).is_err());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn importing_image_returns_only_a_safe_asset_reference() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let source = temp.0.join("source.png");
    image::RgbaImage::new(2, 2).save(&source).unwrap();
    let event = send(
        &mut backend,
        json!({"command":"ImportAsset", "path":source}),
    );
    let Event::Asset { src } = event else {
        panic!("{event:?}")
    };
    assert!(src.starts_with("assets/"));
    assert!(deck::safe_asset_path(&src));
    let bytes = backend
        .serve(&format!("nexus://localhost/{src}"))
        .unwrap()
        .1;
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 2);
    let invalid = temp.0.join("invalid.png");
    fs::write(&invalid, b"not an image").unwrap();
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ImportAsset", "path":invalid})
        ),
        Event::Error { .. }
    ));
}

#[test]
fn event_scripts_escape_untrusted_strings() {
    let event = Event::Error {
        message: "</script>\"\\\n\u{2028}alert(1)".into(),
    };
    let script = event.script();
    assert!(!script.contains("</script>"));
    assert!(!script.contains('\u{2028}'));
    let payload = script
        .strip_prefix("window.studioEvent(")
        .unwrap()
        .strip_suffix(");")
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(value["message"], "</script>\"\\\n\u{2028}alert(1)");
}

// -- the file is the single source of truth, shared with the model's `deck` tool --

use nexus_core::deck::feedback::{
    self, Anchor, Edit, EditStatus, Message as Note, Role, Severity, Thread,
};
use nexus_core::deck::link::Message as Link;
use nexus_core::deck::suggest::{self, RawSuggestion};

/// Another writer (the model's `deck` tool) changes the deck file.
fn external_edit(backend: &Backend, ops: serde_json::Value) -> Deck {
    let ops: Vec<Op> = serde_json::from_value(ops).unwrap();
    let next = deck::apply(&read_deck(&backend.path).unwrap(), &ops).unwrap();
    atomic_write(&backend.path, &serde_json::to_vec_pretty(&next).unwrap()).unwrap();
    next
}

/// A model comment pinned on `b1` of `s1`, carrying one suggested edit.
fn model_thread(backend: &Backend, text: &str) -> Thread {
    let deck = backend.deck.clone();
    let hash = deck.node("b1").unwrap().hash();
    let raw = RawSuggestion {
        why: "Shorter headline".into(),
        ops: vec![
            json!({"op":"update_node","id":"b1","fields":{"text":"A sharper headline"},"base_hash":hash}),
        ],
    };
    let (mut valid, errors) = suggest::prepare(&deck, vec![raw]);
    assert!(errors.is_empty(), "{errors:?}");
    let anchor = Anchor {
        slide: "s1".into(),
        block: Some("b1".into()),
        x: 210,
        y: 390,
        px: true,
    };
    let edit = Edit::new(&deck, valid.pop().unwrap(), &anchor);
    Thread {
        id: "model-thread".into(),
        anchor,
        severity: Severity::Should,
        created: feedback::now(),
        resolved: None,
        messages: vec![Note::new(Role::Model, text.into(), vec![edit])],
    }
}

#[test]
fn every_applied_edit_reaches_the_file_at_once() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let start = backend.deck.clone();
    success(send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"set_notes","slide":"s1","notes":"One"}]}),
    ));
    assert_eq!(read_deck(&backend.path).unwrap(), backend.deck);
    assert_ne!(backend.deck, start);
    assert!(matches!(
        backend.dispatch(r#"{"command":"Apply","ops":[{"op":"delete_slide","id":"nope"}]}"#),
        Event::Error { .. }
    ));
    assert_eq!(
        read_deck(&backend.path).unwrap(),
        backend.deck,
        "a failed edit leaves the file alone"
    );
}

#[test]
fn an_external_deck_edit_reloads_as_one_undo_step_and_flashes_the_changed_nodes() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let original = backend.deck.clone();
    assert!(backend.poll().unwrap().is_none(), "nothing changed yet");
    // One model edit touching two blocks and a note, written in one rename.
    let external = external_edit(
        &backend,
        json!([
            {"op":"update_node","id":"b1","fields":{"text":"Written by the model"},"base_hash":original.node("b1").unwrap().hash()},
            {"op":"update_node","id":"b_subtitle","fields":{"text":"Also changed"},"base_hash":original.node("b_subtitle").unwrap().hash()},
            {"op":"set_notes","slide":"s1","notes":"Model notes"}
        ]),
    );
    let Some(Event::Deck {
        deck,
        changed,
        notice,
        ..
    }) = backend.poll().unwrap()
    else {
        panic!("the external edit was not noticed");
    };
    assert_eq!(deck, external);
    assert_eq!(
        changed,
        ["b1", "b_subtitle"],
        "only the changed nodes flash"
    );
    assert!(notice.is_some_and(|n| n.contains("reloaded")));
    assert_eq!(backend.undo.len(), 1, "the whole edit is one undo step");
    assert!(
        backend.poll().unwrap().is_none(),
        "and is not reloaded twice"
    );

    // Cmd+Z takes the model's edit back, in the file as well; redo restores it.
    success(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(backend.deck, original);
    assert_eq!(read_deck(&backend.path).unwrap(), original);
    assert!(
        backend.poll().unwrap().is_none(),
        "our own undo is not mistaken for an external edit"
    );
    success(send(&mut backend, json!({"command":"Redo"})));
    assert_eq!(read_deck(&backend.path).unwrap(), external);
}

#[test]
fn a_local_action_against_a_deck_that_changed_on_disk_loses_to_the_file() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let external = external_edit(
        &backend,
        json!([{"op":"set_notes","slide":"s1","notes":"From the model"}]),
    );
    let event = send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"set_notes","slide":"s1","notes":"From me"}]}),
    );
    let Event::Deck { notice, deck, .. } = event else {
        panic!("{event:?}")
    };
    assert!(
        notice.is_some_and(|n| n.contains("not applied")),
        "the toast says the file won"
    );
    assert_eq!(deck, external);
    assert_eq!(read_deck(&backend.path).unwrap(), external);
}

#[test]
fn an_unreadable_external_file_is_reported_once() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    fs::write(&backend.path, "{ not a deck").unwrap();
    assert!(backend.poll().is_err());
    assert!(
        backend.poll().unwrap().is_none(),
        "no error storm every 500 ms"
    );
    assert!(
        matches!(
            backend.dispatch(r#"{"command":"Load"}"#),
            Event::Deck { .. }
        ),
        "the page keeps the last good deck"
    );
}

#[test]
fn model_comments_arrive_by_file_and_do_not_drop_local_commands() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    feedback::save(
        &backend.path,
        &[model_thread(&backend, "Make the headline sharper.")],
    )
    .unwrap();
    let Some(Event::Deck {
        threads, changed, ..
    }) = backend.poll().unwrap()
    else {
        panic!("the sidecar change was not noticed");
    };
    assert_eq!(threads.len(), 1);
    assert!(changed.is_empty());
    assert_eq!(backend.undo.len(), 0, "comments never touch the undo stack");

    // A local note while the model's comment exists keeps both, in the shared file.
    let anchor = json!({"slide":"s1","block":null,"x":2,"y":2});
    success_or_feedback(send(
        &mut backend,
        json!({"command":"CommentCreate","anchor":anchor,"text":"My own note","send":false}),
    ));
    let saved = feedback::load(&backend.path).unwrap();
    assert_eq!(saved.len(), 2);
    assert!(
        saved.iter().any(|t| t.id == "model-thread")
            && saved
                .iter()
                .any(|t| matches!(t.messages[0].role, Role::You))
    );
    // A model comment added meanwhile is picked up before the next local write.
    let mut more = saved;
    more.push(Thread {
        id: "second".into(),
        ..model_thread(&backend, "Another point.")
    });
    feedback::save(&backend.path, &more).unwrap();
    success_or_feedback(send(
        &mut backend,
        json!({"command":"CommentResolve","thread":"model-thread"}),
    ));
    let saved = feedback::load(&backend.path).unwrap();
    assert_eq!(saved.len(), 3);
    assert!(
        saved
            .iter()
            .find(|t| t.id == "model-thread")
            .unwrap()
            .resolved
            .is_some()
    );
}

fn success_or_feedback(event: Event) {
    assert!(
        matches!(event, Event::Deck { .. } | Event::Feedback { .. }),
        "{event:?}"
    );
}

#[test]
fn a_suggested_edit_previews_applies_as_one_undo_step_and_persists_its_status() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let original = backend.deck.clone();
    let thread = model_thread(&backend, "Make the headline sharper.");
    let edit_id = thread.messages[0].edits[0].proposal.id.clone();
    feedback::save(&backend.path, &[thread]).unwrap();
    backend.poll().unwrap();

    let Event::Preview { deck } = send(
        &mut backend,
        json!({"command":"PreviewEdit","owner":"model-thread","edit":edit_id}),
    ) else {
        panic!("expected a preview");
    };
    assert_ne!(deck, original);
    assert_eq!(backend.deck, original, "previewing changes nothing");
    assert_eq!(read_deck(&backend.path).unwrap(), original);

    success(send(
        &mut backend,
        json!({"command":"ApplyEdit","owner":"model-thread","edit":edit_id}),
    ));
    assert_eq!(
        read_deck(&backend.path).unwrap(),
        backend.deck,
        "applying is autosaved"
    );
    assert_eq!(backend.undo.len(), 1);
    assert_eq!(
        feedback::load(&backend.path).unwrap()[0].messages[0].edits[0].status,
        EditStatus::Applied
    );
    // An applied edit cannot be applied twice.
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ApplyEdit","owner":"model-thread","edit":edit_id})
        ),
        Event::Error { .. }
    ));
    success(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(backend.deck, original);
    assert_eq!(read_deck(&backend.path).unwrap(), original);
}

#[test]
fn dismissed_and_outdated_edits_are_not_applicable() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let thread = model_thread(&backend, "Make the headline sharper.");
    let edit_id = thread.messages[0].edits[0].proposal.id.clone();
    feedback::save(&backend.path, &[thread]).unwrap();
    backend.poll().unwrap();
    // The block the edit guards changes in the file: the edit goes stale.
    external_edit(
        &backend,
        json!([{"op":"update_node","id":"b1","fields":{"text":"Changed meanwhile"},"base_hash":backend.deck.node("b1").unwrap().hash()}]),
    );
    backend.poll().unwrap();
    assert_eq!(
        feedback::load(&backend.path).unwrap()[0].messages[0].edits[0].status,
        EditStatus::Pending,
        "status is only persisted with the next write"
    );
    assert!(
        matches!(send(&mut backend, json!({"command":"PreviewEdit","owner":"model-thread","edit":edit_id})), Event::Error { message } if message.contains("Outdated"))
    );
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ApplyEdit","owner":"model-thread","edit":edit_id})
        ),
        Event::Error { .. }
    ));
    success_or_feedback(send(
        &mut backend,
        json!({"command":"DismissEdit","owner":"model-thread","edit":edit_id}),
    ));
    assert_eq!(
        feedback::load(&backend.path).unwrap()[0].messages[0].edits[0].status,
        EditStatus::Dismissed
    );
}

#[test]
fn comments_go_to_nexus_only_over_the_link_and_only_when_linked() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let anchor = json!({"slide":"s1","block":"b1","x":210,"y":390});
    // Standalone: a note is kept; sending is refused with the way to link.
    success_or_feedback(send(
        &mut backend,
        json!({"command":"CommentCreate","anchor":anchor,"text":"Local note","send":false}),
    ));
    let Event::Error { message } = send(
        &mut backend,
        json!({"command":"CommentCreate","anchor":anchor,"text":"Send me","send":true}),
    ) else {
        panic!("sending while unlinked must fail");
    };
    assert!(message.contains("/studio"), "{message}");
    assert!(backend.outgoing.is_empty());
    assert_eq!(
        feedback::load(&backend.path).unwrap().len(),
        1,
        "the refused comment was not saved"
    );

    backend.linked = true;
    backend.hello();
    success_or_feedback(send(
        &mut backend,
        json!({"command":"CommentCreate","anchor":anchor,"text":"Send me","send":true}),
    ));
    let thread = feedback::load(&backend.path).unwrap().pop().unwrap();
    let thread_id = thread.id.clone();
    success_or_feedback(send(
        &mut backend,
        json!({"command":"CommentReply","thread":thread_id,"text":"And this","send":true}),
    ));
    success_or_feedback(send(
        &mut backend,
        json!({"command":"CommentReply","thread":thread_id,"text":"Not this","send":false}),
    ));
    success_or_feedback(send(
        &mut backend,
        json!({"command":"Focus","slide":"s2","blocks":["b3"]}),
    ));
    let sent = std::mem::take(&mut backend.outgoing);
    assert!(matches!(&sent[0], Link::Hello { .. }));
    assert!(
        matches!(&sent[1], Link::Comment { thread_id: id, slide, block, point: (210, 390), text, .. } if *id == thread_id && slide == "s1" && block.as_deref() == Some("b1") && text == "Send me")
    );
    assert!(matches!(&sent[2], Link::Reply { text, .. } if text == "And this"));
    assert!(
        matches!(&sent[3], Link::Focus { slide, blocks, .. } if slide == "s2" && blocks == &["b3".to_string()])
    );
    assert_eq!(sent.len(), 4, "notes stay local");
}

#[test]
fn link_messages_open_focus_and_close_without_losing_history() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    backend.linked = true;
    success(send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"set_notes","slide":"s1","notes":"Edit"}]}),
    ));
    let here = backend.path.clone();
    // Opening the deck that is already open (even by another spelling) keeps undo.
    let spelled = here
        .parent()
        .unwrap()
        .join("../decks")
        .join(here.file_name().unwrap());
    backend
        .receive_link(Link::Open { deck_path: spelled })
        .unwrap();
    assert_eq!(backend.undo.len(), 1);
    assert!(
        matches!(backend.receive_link(Link::Focus { deck_path: here.clone(), slide: "s2".into(), blocks: vec![] }).unwrap(), Event::Focus { slide } if slide == "s2")
    );
    // A focus for another deck is ignored.
    assert!(matches!(
        backend
            .receive_link(Link::Focus {
                deck_path: temp.0.join("other.deck.json"),
                slide: "s2".into(),
                blocks: vec![]
            })
            .unwrap(),
        Event::Deck { .. }
    ));
    // Opening another deck switches to it, with a fresh history.
    let other = temp.0.join("files/decks/other.deck.json");
    atomic_write(
        &other,
        &serde_json::to_vec(&Deck::new_from_layouts("Other")).unwrap(),
    )
    .unwrap();
    backend.outgoing.clear();
    backend
        .receive_link(Link::Open {
            deck_path: other.clone(),
        })
        .unwrap();
    assert_eq!(backend.deck.title, "Other");
    assert!(backend.undo.is_empty());
    assert!(matches!(&backend.outgoing[..], [Link::Hello { deck_path }] if *deck_path == other));
    backend.receive_link(Link::Bye).unwrap();
    assert!(!backend.linked);
}

#[test]
fn the_html_renderer_uses_the_shared_layout_numbers() {
    use nexus_core::deck::render::LAYOUT;
    let css = include_str!("../../ui/studio.css");
    let js = include_str!("../../ui/render.js");
    // CSS writes lengths without a leading zero: `.3em`.
    let n = |value: f64| format!("{value}").trim_start_matches('0').to_string();
    for needle in [
        format!(".block p {{ margin:0 0 {}em", n(LAYOUT.paragraph_gap)),
        format!(
            "line-height:{}; margin:0 0 {}em; font-weight:{}",
            LAYOUT.heading_line_height,
            n(LAYOUT.heading_gap),
            LAYOUT.heading_weight
        ),
        format!(".block h1 {{ font-size:{}em", LAYOUT.heading_scale[0]),
        format!(".block h2 {{ font-size:{}em", LAYOUT.heading_scale[1]),
        format!(".block h3 {{ font-size:{}em", LAYOUT.heading_scale[2]),
        format!("padding-left:{}em", LAYOUT.list_indent),
        format!("padding:{}em 0", n(LAYOUT.list_item_padding)),
        format!(
            "padding:{}em {}em",
            n(LAYOUT.cell_padding_y),
            n(LAYOUT.cell_padding_x)
        ),
        format!(".block-code {{ padding:{}em", n(LAYOUT.code_padding)),
        format!("padding:0 {}em", n(LAYOUT.inline_code_padding)),
        // The last paragraph or heading has no trailing margin, so a hugging
        // text box is as tall as the core renderer measures it.
        ".block-text > :is(p,h1,h2,h3):last-child { margin-bottom:0; }".to_string(),
        ".block-text.tight p { margin:0; }".to_string(),
    ] {
        assert!(css.contains(&needle), "studio.css is missing `{needle}`");
    }
    assert!(js.contains(&format!(
        "viewBox: '0 0 {} {}'",
        LAYOUT.chart_width, LAYOUT.chart_height
    )));
    assert!(
        js.contains(&format!("font.line_height < {}", LAYOUT.tight_line_height)),
        "tight text (titles) has no paragraph gap in both renderers"
    );
    // The flexbox mapping documented in `deck/layout.rs`.
    for needle in [
        "'0 0 auto'",
        "'1 1 0px'",
        "'0 1 auto'",
        "alignSelf = 'stretch'",
        "alignContent = 'flex-start'",
        "space_between:'space-between'",
        "inset 0 0 0",
    ] {
        assert!(js.contains(needle), "render.js is missing `{needle}`");
    }
    assert!(
        css.contains("overflow:visible"),
        "overflowing text stays visible past its box"
    );
}

#[test]
fn a_version_one_deck_opens_as_version_two_and_is_saved_that_way() {
    let temp = Temp::new();
    let path = temp.0.join("old.deck.json");
    fs::write(
        &path,
        include_str!("../../../core/tests/fixtures/deck/classic.deck.json"),
    )
    .unwrap();
    let mut backend =
        Backend::start(Some(&path), &temp.0.join("files"), &temp.0.join("data")).unwrap();
    assert_eq!(backend.deck.version, 2);
    let Event::Deck { deck, hashes, .. } = backend.event() else {
        panic!()
    };
    assert_eq!(deck.slides[0].children[0].id, "b1");
    assert_eq!(hashes["b1"], deck.node("b1").unwrap().hash());
    // The file is migrated by the first write, not by opening it.
    let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 1);
    success(send(
        &mut backend,
        json!({"command":"Apply","ops":[{"op":"set_notes","slide":"s1","notes":"Now v2"}]}),
    ));
    let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 2);
    assert!(on_disk["slides"][0].get("blocks").is_none());
    assert_eq!(read_deck(&path).unwrap(), backend.deck);
}

#[test]
fn events_carry_a_hash_for_every_nested_node_and_flash_only_what_changed() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let Event::Deck { hashes, .. } = backend.event() else {
        panic!()
    };
    // `b3` sits inside the `b_row` frame on slide 2.
    for id in ["b1", "b_row", "b3", "b_chart"] {
        assert!(hashes.contains_key(id), "{id}");
    }
    let original = backend.deck.clone();
    external_edit(
        &backend,
        json!([{"op":"update_node","id":"b3","fields":{"text":"- Inside a frame"},"base_hash":original.node("b3").unwrap().hash()}]),
    );
    let Some(Event::Deck { changed, .. }) = backend.poll().unwrap() else {
        panic!("not noticed");
    };
    assert_eq!(changed, ["b3"], "the frame around the node does not flash");
}

#[test]
fn deck_fonts_are_served_from_the_assets_folder() {
    let temp = Temp::new();
    let backend = temp.backend();
    let fonts = asset_root(&backend.path).unwrap().join("fonts");
    fs::create_dir_all(&fonts).unwrap();
    fs::write(fonts.join("brand.woff2"), b"wOF2").unwrap();
    let (mime, bytes) = backend
        .serve("nexus://localhost/assets/fonts/brand.woff2")
        .unwrap();
    assert_eq!((mime, bytes.as_slice()), ("font/woff2", &b"wOF2"[..]));
    assert!(
        backend
            .serve("nexus://localhost/assets/fonts/../../x.woff2")
            .is_err()
    );
    assert!(
        backend
            .serve("nexus://localhost/assets/fonts/script.js")
            .is_err()
    );
}

#[test]
fn comments_are_pinned_in_slide_pixels_and_old_grid_sidecars_are_converted() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    // An anchor outside the 1920 x 1080 slide is refused.
    let out = json!({"slide":"s1","block":null,"x":1920,"y":10});
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"CommentCreate","anchor":out,"text":"x","send":false})
        ),
        Event::Error { .. }
    ));
    // A version-1 sidecar (grid cells) is read as pixels.
    fs::write(
        feedback::sidecar(&backend.path),
        r#"[{"id":"old","anchor":{"slide":"s1","block":"b1","x":3,"y":6},"severity":"should","created":1,"resolved":null,"messages":[]}]"#,
    )
    .unwrap();
    let Some(Event::Deck { threads, .. }) = backend.poll().unwrap() else {
        panic!("sidecar change not noticed");
    };
    assert_eq!((threads[0].anchor.x, threads[0].anchor.y), (210, 390));
}

/// `NEXUS_RENDER_OUT=<dir> cargo test -p nexus-studio write_core_renders_of_the_dev_deck`
/// draws the dev-harness deck with the core renderer, to compare with the HTML
/// screenshots from `ui-dev/shot.mjs`. Does nothing unless the variable is set.
#[test]
fn write_core_renders_of_the_dev_deck() {
    let Some(out) = std::env::var_os("NEXUS_RENDER_OUT") else {
        return;
    };
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../ui-dev/fixtures/workspace.json")).unwrap();
    let deck = Deck::check(&fixture["deck"].to_string()).unwrap();
    fs::create_dir_all(&out).unwrap();
    for (id, name) in [("s1", "title"), ("s2", "content-chart"), ("s3", "quote")] {
        let png = deck::render::render_slide(&deck, id, 1440).unwrap();
        fs::write(
            Path::new(&out).join(format!("core-workspace-{name}.png")),
            png,
        )
        .unwrap();
    }
}
