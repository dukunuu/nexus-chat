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
    invalid["version"] = json!(2);
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
fn save_is_atomic_and_reloads_exactly() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[{"op":"set_notes", "slide":"s1", "notes":"A speaker note"}]}),
    ));
    assert!(matches!(backend.event(), Event::Deck { dirty: true, .. }));
    let deck = backend.deck.clone();
    success(send(&mut backend, json!({"command":"Save", "deck":deck})));
    assert!(matches!(backend.event(), Event::Deck { dirty: false, .. }));
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
fn undo_redo_is_exact_across_mixed_transactions_and_save() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let original = backend.deck.clone();
    let hash = original.slides[0].blocks[0].hash();
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[
            {"op":"update_block", "slide":"s1", "id":"b1", "fields":{"text":"A new headline", "x":5}, "base_hash":hash},
            {"op":"set_notes", "slide":"s2", "notes":"Tell a story"},
            {"op":"move_slide", "id":"s2", "to":0}
        ]}),
    ));
    let middle = backend.deck.clone();
    success(send(
        &mut backend,
        json!({"command":"Apply", "ops":[{"op":"delete_slide", "id":"s1"}, {"op":"set_layout", "slide":"s2", "layout":"quote"}]}),
    ));
    let last = backend.deck.clone();
    success(send(&mut backend, json!({"command":"Save", "deck":last})));
    for expected in [&middle, &original] {
        success(send(&mut backend, json!({"command":"Undo"})));
        assert_eq!(&backend.deck, expected);
    }
    assert!(matches!(backend.event(), Event::Deck { dirty: true, .. }));
    for expected in [&middle, &last] {
        success(send(&mut backend, json!({"command":"Redo"})));
        assert_eq!(&backend.deck, expected);
    }
    assert!(matches!(backend.event(), Event::Deck { dirty: false, .. }));
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
