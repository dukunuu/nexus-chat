use std::io::Cursor;
use std::path::PathBuf;

use serde_json::json;

use super::*;

struct Space(PathBuf);

impl Space {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexus-assets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("files/decks")).unwrap();
        Self(root)
    }
    fn files(&self) -> PathBuf {
        self.0.join("files")
    }
    fn assets(&self) -> PathBuf {
        self.files().join("decks/assets")
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(side: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(side, side))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

#[test]
fn a_space_file_is_copied_into_assets_under_a_content_name() {
    let space = Space::new();
    // Misnamed on purpose: the content decides, not the extension.
    std::fs::write(space.files().join("us_1.dat"), png(8)).unwrap();
    let copy = import_space_image("us_1.dat", &space.files(), &space.assets())
        .unwrap()
        .unwrap();
    assert!(
        copy.starts_with("assets/") && copy.ends_with("-us_1.png"),
        "{copy}"
    );
    let name = copy.strip_prefix("assets/").unwrap();
    assert_eq!(name.split('-').next().unwrap().len(), 8);
    assert!(space.assets().join(name).is_file());
    // The same file again lands on the same name, and `files/…` finds it too.
    let again = import_space_image("files/us_1.dat", &space.files(), &space.assets())
        .unwrap()
        .unwrap();
    assert_eq!(again, copy);
}

#[test]
fn only_images_by_content_are_taken() {
    let space = Space::new();
    std::fs::write(space.files().join("notes.png"), b"not really a picture").unwrap();
    std::fs::write(space.files().join("page.html"), b"<html></html>").unwrap();
    for name in ["notes.png", "page.html"] {
        let error = import_space_image(name, &space.files(), &space.assets()).unwrap_err();
        assert!(
            error.to_string().contains("not a PNG, JPEG or WebP image"),
            "{error}"
        );
    }
    assert!(
        !space.assets().exists() || std::fs::read_dir(space.assets()).unwrap().next().is_none()
    );
}

#[test]
fn large_files_and_outside_paths_are_refused_or_left_alone() {
    let space = Space::new();
    let big = std::fs::File::create(space.files().join("big.png")).unwrap();
    big.set_len(MAX_IMAGE_BYTES + 1).unwrap();
    let error = import_space_image("big.png", &space.files(), &space.assets()).unwrap_err();
    assert!(error.to_string().contains("at most 16 MB"), "{error}");

    std::fs::write(space.0.join("secret.png"), png(4)).unwrap();
    assert_eq!(
        import_space_image("../secret.png", &space.files(), &space.assets()).unwrap(),
        None
    );
    let outside = space.0.join("secret.png");
    assert_eq!(
        import_space_image(outside.to_str().unwrap(), &space.files(), &space.assets()).unwrap(),
        None
    );
}

#[test]
fn urls_existing_assets_and_unknown_names_stay_as_written() {
    let space = Space::new();
    std::fs::create_dir_all(space.assets()).unwrap();
    std::fs::write(space.assets().join("kept.png"), png(4)).unwrap();
    for src in [
        "https://example.com/a.png",
        "assets/kept.png",
        "assets/missing.png",
        "nothing.jpg",
    ] {
        assert_eq!(
            import_space_image(src, &space.files(), &space.assets()).unwrap(),
            None,
            "{src}"
        );
    }
    // A path through decks/assets resolves to the assets form without a copy.
    assert_eq!(
        import_space_image("decks/assets/kept.png", &space.files(), &space.assets()).unwrap(),
        Some("assets/kept.png".into())
    );
}

#[test]
fn srcs_are_found_in_slides_and_operations() {
    let mut value = json!([
        {"op": "insert_node", "parent": "s1", "node": {"kind": "frame", "children": [{"kind": "image", "src": "a.png"}]}},
        {"op": "update_node", "id": "x", "fields": {"src": "b.png"}},
        {"op": "add_slide", "slide": {"children": [{"kind": "image", "src": "c.png"}, {"kind": "text", "text": "src"}]}}
    ]);
    let mut seen = Vec::new();
    visit_srcs(&mut value, &mut |src| {
        seen.push(src.clone());
        src.insert_str(0, "assets/");
        Ok(())
    })
    .unwrap();
    seen.sort();
    assert_eq!(seen, ["a.png", "b.png", "c.png"]);
    assert_eq!(value[1]["fields"]["src"], "assets/b.png");
}
