use super::*;
use crate::backend::Backend;
use serde_json::json;
use std::{fs, path::PathBuf};

const DOTX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../core/tests/fixtures/templates/brand.dotx"
));
const POTX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../core/tests/fixtures/templates/brand.potx"
));
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn send(backend: &mut Backend, command: serde_json::Value) -> Event {
    backend.dispatch(&command.to_string())
}

#[test]
fn chooser_path_and_drop_bytes_import_offer_apply_and_export_both_kinds() {
    let temp =
        Temp(std::env::temp_dir().join(format!("nexus-studio-template-{}", uuid::Uuid::new_v4())));
    fs::create_dir_all(&temp.0).unwrap();
    let mut backend = Backend::start(None, &temp.0.join("files"), &temp.0.join("data")).unwrap();
    let source = temp.0.join("brand.potx");
    fs::write(&source, POTX).unwrap();
    let before = backend.deck.clone();
    assert!(
        matches!(send(&mut backend, json!({"command":"ImportTemplate","path":source})), Event::TemplateImported { slug, .. } if slug == "brand")
    );
    assert_eq!(backend.deck, before);
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ApplyDesign","design":"brand"})
        ),
        Event::Deck { template: true, .. }
    ));
    assert_eq!(backend.deck.design.as_deref(), Some("brand"));
    let mut exports = Vec::new();
    for selection in [None, Some("none")] {
        let event = send(
            &mut backend,
            json!({"command":"Export","format":"pptx","template":selection}),
        );
        let Event::Exported { path, .. } = event else {
            panic!("{event:?}");
        };
        let bytes = fs::read(path).unwrap();
        assert!(bytes.starts_with(b"PK"));
        exports.push(bytes);
    }
    assert_ne!(exports[0], exports[1]);
    assert!(matches!(
        send(&mut backend, json!({"command":"NewDoc","title":"Report"})),
        Event::Doc { .. }
    ));
    let data: String = DOTX.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        matches!(send(&mut backend, json!({"command":"ImportTemplateData","name":"word.dotx","data":data})), Event::TemplateImported { slug, .. } if slug == "word")
    );
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ApplyDesign","design":"word"})
        ),
        Event::Doc { .. }
    ));
    let saved = nexus_core::doc::Doc::check(&fs::read_to_string(&backend.path).unwrap()).unwrap();
    assert_eq!(saved.design.as_deref(), Some("word"));
    let Event::Designs { available } = send(&mut backend, json!({"command":"Designs"})) else {
        panic!("designs");
    };
    assert!(available.iter().any(|d| d.slug == "word" && d.current));
    let event = send(&mut backend, json!({"command":"DocExport","format":"docx"}));
    let Event::Exported { path, .. } = event else {
        panic!("{event:?}");
    };
    let templated = fs::read(path).unwrap();
    let event = send(
        &mut backend,
        json!({"command":"DocExport","format":"docx","template":"none"}),
    );
    let Event::Exported { path, .. } = event else {
        panic!("{event:?}");
    };
    assert_ne!(templated, fs::read(path).unwrap());
    assert!(matches!(
        send(
            &mut backend,
            json!({"command":"ImportTemplateData","name":"bad.dotm","data":"00"})
        ),
        Event::Error { .. }
    ));
}
