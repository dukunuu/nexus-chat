use super::*;

#[test]
fn attached_theme_updates_only_chrome_and_is_in_the_next_load() {
    let root = std::env::temp_dir().join(format!("nexus-studio-theme-{}", Uuid::new_v4()));
    let mut backend = Backend::start(None, &root.join("files"), &root.join("data")).unwrap();
    let before = backend.deck.clone();
    let saved = fs::read(&backend.path).unwrap();
    let colors: ThemeColors =
        serde_json::from_str(include_str!("../../ui-dev/fixtures/theme-light.json")).unwrap();
    let event = backend
        .receive_link(LinkMessage::Theme {
            colors: colors.clone(),
        })
        .unwrap();
    assert!(matches!(event, Event::Theme { colors: actual } if actual == colors));
    assert!(matches!(backend.event(), Event::Deck { colors: actual, .. } if actual == colors));
    assert_eq!(backend.deck, before);
    assert_eq!(fs::read(&backend.path).unwrap(), saved);
    assert!(backend.undo.is_empty());
    fs::remove_dir_all(root).unwrap();
}
