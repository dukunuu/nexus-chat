use super::*;
use serde_json::json;

#[test]
fn background_images_have_frontend_routes_cache_hashes_and_source_checks() {
    let root = std::env::temp_dir().join(format!("nexus-background-{}", Uuid::new_v4()));
    let mut backend = Backend::start(None, &root.join("files"), &root.join("data")).unwrap();
    let remote = "https://example.com/background.png";
    backend.deck = Deck::check(
        &json!({"version":2,"title":"Backgrounds","slides":[
            {"background":{"image":remote,"fit":"cover"}},
            {"background":{"image":"assets/local.png","fit":"contain"}}
        ]})
        .to_string(),
    )
    .unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(4, 3, image::Rgba([40, 80, 120, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    let assets = asset_root(&backend.path).unwrap();
    fs::write(assets.join("local.png"), &png).unwrap();
    let route = image_hash(remote);
    let Event::Deck { images, .. } = backend.event() else {
        panic!("deck event");
    };
    assert_eq!(images[remote], route);
    assert!(!images.contains_key(&format!("/asset/{route}")));
    assert_eq!(images["assets/local.png"], hash_bytes(&png));
    assert_eq!(
        backend
            .serve("nexus://localhost/assets/local.png")
            .unwrap()
            .1,
        png
    );
    // Seeding the vetted cache exercises the real protocol path without networking.
    backend.fetcher.seed(remote, &png).unwrap();
    let Event::Deck { images, .. } = backend.event() else {
        panic!("deck event");
    };
    assert_eq!(images[&format!("/asset/{route}")], hash_bytes(&png));
    let (fetcher, source) = backend
        .remote_image(&format!("nexus://localhost/asset/{route}"))
        .unwrap();
    assert_eq!(source, remote);
    assert_eq!(fs::read(fetcher.cached(&source).unwrap()).unwrap(), png);
    assert!(
        backend
            .remote_image("nexus://localhost/asset/not-in-this-deck")
            .is_err()
    );
    backend.deck.slides[0].background = None;
    assert!(
        backend
            .remote_image(&format!("nexus://localhost/asset/{route}"))
            .is_err()
    );
    let Event::Deck { images, .. } = backend.event() else {
        panic!("deck event");
    };
    assert!(!images.contains_key(remote));
    fs::remove_dir_all(root).unwrap();
}
