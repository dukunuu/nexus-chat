use super::*;
use serde_json::json;

#[test]
fn animation_resources_and_single_transaction_undo() {
    let root = std::env::temp_dir().join(format!("nexus-motion-{}", Uuid::new_v4()));
    let mut backend = Backend::start(None, &root.join("files"), &root.join("data")).unwrap();
    for name in ["animations", "playback", "animate-panel"] {
        let (mime, bytes) = backend
            .serve(&format!("nexus://localhost/{name}.js"))
            .unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!bytes.is_empty());
    }
    let before = backend.deck.clone();
    let slide = before.slides[0].id.clone();
    let node = before.slides[0].children[0].id.clone();
    let event = backend.dispatch(
        &json!({"command":"Apply","ops":[
            {"op":"set_transition","slide":slide,"transition":{"type":"push"}},
            {"op":"set_animation","id":node,"slot":"build_in","build":{"effect":"fade"}},
            {"op":"reorder_builds","slide":slide,"ids":[node]}
        ]})
        .to_string(),
    );
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    assert_eq!(backend.undo.len(), 1);
    let after = backend.deck.clone();
    backend.dispatch(r#"{"command":"Undo"}"#);
    assert_eq!(backend.deck, before);
    backend.dispatch(r#"{"command":"Redo"}"#);
    assert_eq!(backend.deck, after);
    let event = backend.dispatch(
        &json!({"command":"Apply","ops":[
            {"op":"set_transition","slide":slide,"transition":{"duration_ms":-1}}
        ]})
        .to_string(),
    );
    assert!(matches!(event, Event::Error { .. }));
    assert_eq!(backend.deck, after);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unparseable_animation_on_disk_is_dropped_with_a_one_time_notice() {
    let root = std::env::temp_dir().join(format!("nexus-motion-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("legacy.deck.json");
    fs::write(
        &path,
        json!({"version":2,"title":"Legacy","slides":[{"id":"s1","transition":{"kind":"push"},
            "children":[{"id":"a","kind":"text","text":"Hi","animation":{"kind":"fade"}}]}]})
        .to_string(),
    )
    .unwrap();
    let mut backend = Backend::start(Some(&path), &root.join("files"), &root.join("data")).unwrap();
    assert!(backend.deck.slides[0].transition.is_none());
    assert!(backend.deck.node("a").unwrap().animation.is_none());
    let Event::Deck { notice, .. } = backend.dispatch(r#"{"command":"Load"}"#) else {
        panic!("expected a deck event");
    };
    let notice = notice.expect("a notice about the dropped data");
    assert!(
        notice.contains("$.slides[0].transition") && notice.contains("animation"),
        "{notice}"
    );
    let slide = backend.deck.slides[0].id.clone();
    let Event::Deck { notice, .. } = backend.dispatch(
        &json!({"command":"Apply","ops":[{"op":"set_transition","slide":slide,"transition":{"type":"fade"}}]}).to_string(),
    ) else {
        panic!("expected a deck event");
    };
    assert!(notice.is_none(), "the notice is reported once");
    fs::remove_dir_all(root).unwrap();
}
