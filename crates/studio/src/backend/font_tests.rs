use super::*;
use serde_json::json;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexus-studio-font-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
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
    backend.dispatch(&value.to_string())
}
fn system_font() -> Vec<u8> {
    let db = deck::text::system_fonts();
    db.faces()
        .find_map(|face| {
            db.with_face_data(face.id, |bytes, _| {
                (bytes.len() <= deck::text::MAX_FONT_FILE
                    && (bytes.starts_with(&[0, 1, 0, 0])
                        || bytes.starts_with(b"OTTO")
                        || bytes.starts_with(b"true")))
                .then(|| bytes.to_vec())
            })
            .flatten()
        })
        .expect("test environment has a system font")
}

#[test]
fn listing_uses_the_renderers_cached_database_without_changing_the_deck() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let before = backend.deck.clone();
    let Event::Fonts { families } = send(&mut backend, json!({"command":"ListFonts"})) else {
        panic!("expected font metadata");
    };
    assert_eq!(
        families
            .iter()
            .map(|f| f.family.clone())
            .collect::<Vec<_>>(),
        deck::text::installed_families()
    );
    for family in families {
        assert!(!family.weights.is_empty());
        assert!(family.weights.windows(2).all(|pair| pair[0] < pair[1]));
    }
    assert!(std::ptr::eq(
        deck::text::installed_font_info(),
        deck::text::installed_font_info()
    ));
    assert_eq!(backend.deck, before);
    assert!(backend.undo.is_empty());
}

#[test]
fn import_copies_sanitized_font_metadata_and_is_one_undo_step() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let bytes = system_font();
    let db = deck::text::system_fonts();
    let metadata = db
        .faces()
        .find_map(|face| {
            db.with_face_data(face.id, |data, _| {
                (data == bytes).then(|| {
                    (
                        face.families[0].0.clone(),
                        face.weight.0,
                        face.style != Default::default(),
                    )
                })
            })
            .flatten()
        })
        .unwrap();
    let source = temp.0.join("Brand ! font.ttf");
    fs::write(&source, &bytes).unwrap();
    let before = backend.deck.clone();
    let event = send(&mut backend, json!({"command":"ImportFont", "path":source}));
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    let imported = backend.deck.theme.font_files.last().unwrap();
    assert_eq!(
        (&imported.family, imported.weight, imported.italic),
        (&metadata.0, metadata.1, metadata.2)
    );
    assert!(imported.file.starts_with("assets/fonts/Brand---font-"));
    assert!(deck::safe_asset_path(&imported.file));
    assert_eq!(
        fs::read(backend.path.parent().unwrap().join(&imported.file)).unwrap(),
        bytes
    );
    assert_eq!(
        backend
            .serve(&format!("nexus://localhost/{}", imported.file))
            .unwrap()
            .1,
        bytes
    );
    assert_eq!(read_deck(&backend.path).unwrap(), backend.deck);
    assert_eq!(backend.undo.len(), 1);
    let Event::Fonts { families } = send(&mut backend, json!({"command":"ListFonts"})) else {
        panic!("expected font metadata");
    };
    assert!(
        families
            .iter()
            .any(|f| f.family == metadata.0 && f.weights.contains(&metadata.1))
    );
    send(&mut backend, json!({"command":"Undo"}));
    assert_eq!(backend.deck, before);
    send(&mut backend, json!({"command":"Redo"}));
    assert_eq!(backend.deck.theme.font_files.len(), 1);
    send(&mut backend, json!({"command":"ImportFont", "path":source}));
    assert_eq!(
        backend.deck.theme.font_files.len(),
        1,
        "same file is deduplicated"
    );
    assert_eq!(backend.undo.len(), 1);
}

#[test]
fn import_rejects_other_types_invalid_data_oversize_and_parent_components() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let before = backend.deck.clone();
    for name in ["font.exe", "font.ttc", "broken.otf", "broken.woff2"] {
        let path = temp.0.join(name);
        fs::write(&path, b"not a font").unwrap();
        assert!(matches!(
            send(&mut backend, json!({"command":"ImportFont", "path":path})),
            Event::Error { .. }
        ));
    }
    let large = temp.0.join("large.ttf");
    fs::File::create(&large)
        .unwrap()
        .set_len(deck::text::MAX_FONT_FILE as u64 + 1)
        .unwrap();
    assert!(
        matches!(send(&mut backend, json!({"command":"ImportFont", "path":large})), Event::Error { message } if message.contains("size limit"))
    );
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ImportFont", "path":temp.0.join("a/../font.ttf")})
        ),
        Event::Error { .. }
    ));
    assert_eq!(backend.deck, before);
    assert!(backend.undo.is_empty());
    assert!(!asset_root(&backend.path).unwrap().join("fonts").exists());
}

#[cfg(unix)]
#[test]
fn import_refuses_font_directory_and_destination_symlinks() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let mut backend = temp.backend();
    let source = temp.0.join("font.ttf");
    fs::write(&source, system_font()).unwrap();
    let fonts = asset_root(&backend.path).unwrap().join("fonts");
    let outside = temp.0.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &fonts).unwrap();
    assert!(matches!(
        send(&mut backend, json!({"command":"ImportFont", "path":source})),
        Event::Error { message } if message.contains("symlink")
    ));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    fs::remove_file(&fonts).unwrap();
    fs::create_dir(&fonts).unwrap();
    let bytes = fs::read(&source).unwrap();
    let target = fonts.join(format!("font-{}.ttf", &hash_bytes(&bytes)[..16]));
    let private = outside.join("private.ttf");
    fs::write(&private, b"private").unwrap();
    symlink(&private, target).unwrap();
    assert!(matches!(
        send(&mut backend, json!({"command":"ImportFont", "path":source})),
        Event::Error { message } if message.contains("symlink")
    ));
    assert_eq!(fs::read(private).unwrap(), b"private");
    assert!(backend.undo.is_empty());
}

#[test]
fn file_chooser_bytes_import_without_exposing_or_accepting_a_source_path() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let bytes = system_font();
    const HEX: &[u8] = b"0123456789abcdef";
    let data: String = bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect();
    let event = send(
        &mut backend,
        json!({"command":"ImportFontData", "name":"Picked font.otf", "data":data}),
    );
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    assert_eq!(backend.deck.theme.font_files.len(), 1);
    assert_eq!(backend.undo.len(), 1);
    for (name, data) in [
        ("../escape.ttf", "00"),
        ("a\\b.ttf", "00"),
        ("font.exe", "00"),
        ("bad.ttf", "f"),
        ("bad.ttf", "zz"),
    ] {
        assert!(matches!(
            send(
                &mut backend,
                json!({"command":"ImportFontData", "name":name, "data":data})
            ),
            Event::Error { .. }
        ));
    }
    assert_eq!(backend.undo.len(), 1);
    assert!(
        backend
            .import_font_bytes("huge.ttf", &vec![0; deck::text::MAX_FONT_FILE + 1])
            .is_err()
    );
}

#[test]
fn font_assets_use_the_same_size_cap_as_core() {
    let temp = Temp::new();
    let backend = temp.backend();
    let fonts = asset_root(&backend.path).unwrap().join("fonts");
    fs::create_dir(&fonts).unwrap();
    let path = fonts.join("large.woff2");
    fs::File::create(&path)
        .unwrap()
        .set_len(deck::text::MAX_FONT_FILE as u64)
        .unwrap();
    assert_eq!(
        backend
            .serve("nexus://localhost/assets/fonts/large.woff2")
            .unwrap()
            .1
            .len(),
        deck::text::MAX_FONT_FILE
    );
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(deck::text::MAX_FONT_FILE as u64 + 1)
        .unwrap();
    assert!(
        backend
            .serve("nexus://localhost/assets/fonts/large.woff2")
            .is_err()
    );
}
