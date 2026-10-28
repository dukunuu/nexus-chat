use super::*;
use serde_json::json;

#[test]
fn optional_effects_preserve_old_canonical_bytes_and_image_patterns() {
    let deck = Deck::classic_fixture("Compatibility");
    assert!(!deck.theme.colors.contains_key("surface"));
    assert!(Theme::default().colors.contains_key("surface"));
    let bytes = serde_json::to_vec(&deck).unwrap();
    let back = Deck::check(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(serde_json::to_vec(&back).unwrap(), bytes);
    assert!(back.theme.shadows.is_empty());
    for node in back.slides.iter().flat_map(Slide::nodes) {
        assert!(node.shadow.is_none());
        assert!(node.image_radius.is_none());
    }
    let node: Node = serde_json::from_value(json!({"kind":"image","src":"assets/photo.png","radius":24,"mask":"circle","focus":{"x":0.2,"y":0.8},"overlay":{"fill":{"color":"fg","opacity":0.4}}})).unwrap();
    let Kind::Image { src, fit } = &node.kind else {
        panic!("image");
    };
    assert_eq!(src, "assets/photo.png");
    assert_eq!(*fit, Fit::Contain);
    assert_eq!(node.image_radius, Some(24.0));
    assert_eq!(serde_json::to_value(&node).unwrap()["radius"], 24);
}

#[test]
fn effects_validate_with_paths_and_repair_two_color_gradients() {
    let source = json!({"version":2,"title":"Effects","slides":[{"gradient":["bg","accent"],"children":[{"kind":"shape","fill":{"linear":{"angle":90,"stops":[[0,"bg"],[0.5,{"color":"accent","opacity":0.5}],[1,"fg"]]}},"shadow":{"x":0,"y":8,"blur":201,"spread":0,"color":"fg"}}]}]});
    let errors = Deck::check(&source.to_string()).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.path == "$.slides[0].children[0].shadow.blur"),
        "{errors:?}"
    );
    let mut valid = source;
    valid["slides"][0]["children"][0]["shadow"]["blur"] = json!(32);
    let deck = Deck::check(&valid.to_string()).unwrap();
    assert!(matches!(deck.slides[0].fill, Some(Fill::Linear { .. })));
    valid["slides"][0]["children"][0]["fill"]["linear"]["stops"] =
        json!(vec![json!([0, "accent"]); 9]);
    let errors = Deck::check(&valid.to_string()).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.path.ends_with(".fill.linear.stops")),
        "{errors:?}"
    );
}

#[test]
fn svg_draws_gradient_alpha_shadow_and_image_clips() {
    let root = std::env::temp_dir().join(format!("nexus-effects-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    image::RgbImage::from_fn(80, 40, |x, _| {
        if x < 40 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    })
    .save(root.join("photo.png"))
    .unwrap();
    let deck = Deck::check(&json!({"version":2,"title":"Effects","theme":{"size":{"w":320,"h":180}},"slides":[{"fill":{"linear":{"angle":90,"stops":[[0,"#ff0000"],[1,"#0000ff"]]}},"children":[{"id":"image","kind":"image","src":"assets/photo.png","fit":"cover","radius":20,"focus":{"x":1,"y":0.5},"sizing":{"w":40,"h":40},"position":{"x":140,"y":70},"shadow":"sm"}]}]}).to_string()).unwrap();
    let png = render::render_slide_at(&deck, &deck.slides[0].id, 320, Some(&root)).unwrap();
    let pixels = image::load_from_memory(&png).unwrap().to_rgba8();
    assert!(pixels.get_pixel(0, 0)[0] > 250);
    assert!(pixels.get_pixel(319, 0)[2] > 250);
    assert!(
        pixels.get_pixel(160, 90)[2] > 250,
        "focus selects the blue half"
    );
    assert!(
        pixels.get_pixel(140, 70)[0] > 50,
        "rounded crop reveals the gradient"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn studio_effect_numbers_match_core() {
    let js = include_str!("../../../studio/ui/render.js");
    let css = include_str!("../../../studio/ui/studio.css");
    assert!(css.contains(&format!(
        "--shadow-blur-divisor:{}",
        effects::SHADOW_BLUR_DIVISOR
    )));
    assert!(css.contains("calc(var(--image-shadow-blur) / var(--shadow-blur-divisor))"));
    for (name, spec) in effects::presets() {
        let opacity = format!("{:.2}", spec.color.opacity());
        let opacity = opacity.strip_prefix('0').unwrap_or(&opacity);
        let expected = format!(
            "{name}:{{ x:{}, y:{}, blur:{}, spread:{}, color:{{ color:'{}', opacity:{opacity} }} }}",
            spec.x,
            spec.y,
            spec.blur,
            spec.spread,
            spec.color.value()
        );
        assert!(js.contains(&expected), "{expected}");
    }
    assert!(js.contains("(node.focus?.x ?? .5) * 100"));
    assert!(js.contains("circle(closest-side)"));
}
