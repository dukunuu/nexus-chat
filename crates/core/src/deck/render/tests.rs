use serde_json::{Value, json};

use super::*;
use crate::deck::text::{Mark, Row, Setting, classify, inline, system_fonts};
use crate::deck::{Align, Op, Transform, apply, layout_map};

fn rgba(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory(png).expect("png").to_rgba8()
}

fn has_fonts() -> bool {
    !system_fonts().is_empty()
}

fn op(value: Value) -> Op {
    serde_json::from_value(value).expect("op")
}

fn deck_of(layout: Value, children: Value) -> Deck {
    Deck::check(
        &json!({"version": 2, "title": "t", "slides": [{"id": "s", "layout": layout, "children": children}]})
            .to_string(),
    )
    .unwrap_or_else(|e| panic!("{e:?}"))
}

/// Whether anything but the background is drawn inside `rect`, scaled from
/// the 1920 px slide to an image `width` wide.
fn inked(image: &image::RgbaImage, rect: Rect, width: u32) -> bool {
    let scale = f64::from(width) / 1920.0;
    let background = *image.get_pixel(width - 1, image.height() - 1);
    let (x0, y0) = ((rect.x * scale) as u32, (rect.y * scale) as u32);
    let (x1, y1) = (
        (((rect.x + rect.w) * scale) as u32).min(image.width()),
        (((rect.y + rect.h) * scale) as u32).min(image.height()),
    );
    (y0..y1).any(|y| (x0..x1).any(|x| *image.get_pixel(x, y) != background))
}

#[test]
fn fixture_renders_at_size_with_ink_in_every_node() {
    let deck = Deck::classic_fixture("Renderer fixture");
    for slide in &deck.slides {
        let image = rgba(&render_slide(&deck, &slide.id, 1600).unwrap());
        assert_eq!(image.dimensions(), (1600, 900));
        let rects = layout_map(&deck, slide, None);
        for node in slide.nodes() {
            assert!(
                inked(&image, rects[&node.id], 1600),
                "node {} is blank",
                node.id
            );
        }
    }
    let half = rgba(&render_slide(&deck, "s1", 800).unwrap());
    assert_eq!(half.dimensions(), (800, 450));
    assert!(render_slide(&deck, "nope", 800).is_err());
    assert!(render_slide(&deck, "s1", 10).is_err());
}

#[test]
fn every_node_kind_draws() {
    let deck = Deck::classic_fixture("Kinds");
    let nodes = json!([
        {"id": "t", "kind": "table", "position": {"x": 120, "y": 60}, "sizing": {"w": 720, "h": 240}, "rows": [["Name", "Value"], ["alpha", "1"]], "font": {"size": 26}},
        {"id": "c", "kind": "code", "position": {"x": 960, "y": 60}, "sizing": {"w": 720, "h": 240}, "language": "rs", "source": "fn main() {}\nlet x = 1;", "font": {"size": 26}},
        {"id": "p", "kind": "chart", "position": {"x": 120, "y": 480}, "sizing": {"w": 480, "h": 360}, "chart": "pie", "labels": ["a", "b"], "series": [{"name": "s", "values": [1, 3]}]},
        {"id": "l", "kind": "chart", "position": {"x": 720, "y": 480}, "sizing": {"w": 480, "h": 360}, "chart": "line", "labels": ["a", "b"], "series": [{"name": "s", "values": [1, 3]}]},
        {"id": "e", "kind": "shape", "position": {"x": 1320, "y": 480}, "sizing": {"w": 240, "h": 240}, "shape": "ellipse", "fill": "accent", "stroke": {"color": "muted", "width": 6}},
        {"id": "a", "kind": "shape", "position": {"x": 1320, "y": 840}, "sizing": {"w": 360, "h": 120}, "shape": "arrow", "stroke": {"color": "accent", "width": 8}},
        {"id": "i", "kind": "image", "position": {"x": 1680, "y": 480}, "sizing": {"w": 180, "h": 180}, "src": "assets/missing.png", "fit": "cover"}
    ]);
    let ops: Vec<Op> = nodes
        .as_array()
        .unwrap()
        .iter()
        .map(|node| op(json!({"op": "insert_node", "parent": "s2", "node": node})))
        .collect();
    let deck = apply(&deck, &ops).unwrap();
    let image = rgba(&render_slide(&deck, "s2", 1920).unwrap());
    let rects = layout_map(&deck, &deck.slides[1], None);
    for node in deck.slides[1].nodes() {
        assert!(
            inked(&image, rects[&node.id], 1920),
            "node {} is blank",
            node.id
        );
    }
}

#[test]
fn frames_draw_fill_stroke_radius_and_clip() {
    let deck = deck_of(
        json!({"mode": "none"}),
        json!([
            {"id": "f", "kind": "frame", "position": {"x": 100, "y": 100}, "sizing": {"w": 400, "h": 300},
             "fill": "#ff0000", "stroke": {"color": "#0000ff", "width": 20}, "radius": 60, "clip": true,
             "layout": {"mode": "none"},
             "children": [{"id": "big", "kind": "frame", "position": {"x": 300, "y": 200}, "sizing": {"w": 600, "h": 600}, "fill": "#00ff00"}]}
        ]),
    );
    let image = rgba(&render_slide(&deck, "s", 1920).unwrap());
    let px = |x: u32, y: u32| image.get_pixel(x, y).0;
    assert_eq!(px(300, 250), [255, 0, 0, 255], "fill");
    assert_eq!(px(110, 250), [0, 0, 255, 255], "stroke is inside the box");
    assert_eq!(
        px(103, 103),
        [250, 249, 246, 255],
        "rounded corner shows the slide"
    );
    assert_eq!(px(450, 350), [0, 255, 0, 255], "child inside the frame");
    assert_eq!(
        px(600, 350),
        [250, 249, 246, 255],
        "clip cuts the child at the frame edge"
    );
}

#[test]
fn slide_fill_hidden_nodes_opacity_and_auto_layout_position() {
    let deck = deck_of(
        json!({"mode": "row", "gap": 100, "padding": 100}),
        json!([
            {"id": "a", "kind": "frame", "sizing": {"w": 200, "h": 200}, "fill": "#ff0000"},
            {"id": "gone", "kind": "frame", "hidden": true, "sizing": {"w": 200, "h": 200}, "fill": "#00ff00"},
            {"id": "b", "kind": "frame", "sizing": {"w": 200, "h": 200}, "fill": "#0000ff", "opacity": 0.5}
        ]),
    );
    let mut deck = deck;
    deck.slides[0].fill = Some("#202020".into());
    let image = rgba(&render_slide(&deck, "s", 1920).unwrap());
    let px = |x: u32, y: u32| image.get_pixel(x, y).0;
    assert_eq!(px(5, 5), [32, 32, 32, 255]);
    assert_eq!(px(150, 150), [255, 0, 0, 255]);
    // `b` follows `a` after one gap, the hidden node takes no room.
    assert!(
        px(450, 150)[2].abs_diff(143) <= 2,
        "50% blue over #202020: {:?}",
        px(450, 150)
    );
    assert_eq!(px(750, 150), [32, 32, 32, 255]);
}

#[test]
fn typography_is_numeric_and_changes_what_is_drawn() {
    if !has_fonts() {
        return;
    }
    let draw = |font: Value| {
        let deck = deck_of(
            json!({"mode": "none"}),
            json!([{"id": "t", "kind": "text", "text": "Hamburgefons", "position": {"x": 100, "y": 100},
                    "sizing": {"w": 1200, "h": 400}, "font": font}]),
        );
        let image = rgba(&render_slide(&deck, "s", 1920).unwrap());
        // Bounding box of ink.
        let background = *image.get_pixel(1919, 1079);
        let mut bounds = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, pixel) in image.enumerate_pixels() {
            if *pixel != background {
                bounds = (
                    bounds.0.min(x),
                    bounds.1.min(y),
                    bounds.2.max(x),
                    bounds.3.max(y),
                );
            }
        }
        bounds
    };
    let base = draw(json!({"size": 60}));
    let big = draw(json!({"size": 120}));
    assert!(
        big.2 - big.0 > (base.2 - base.0) * 3 / 2,
        "{base:?} {big:?}"
    );
    let spaced = draw(json!({"size": 60, "letter_spacing": 0.3}));
    assert!(spaced.2 > base.2 + 20, "{base:?} {spaced:?}");
    let bottom = draw(json!({"size": 60, "valign": "bottom"}));
    assert!(bottom.1 > base.1 + 200, "{base:?} {bottom:?}");
    let right = draw(json!({"size": 60, "align": "right"}));
    assert!(right.0 > base.0 + 600, "{base:?} {right:?}");
    let upper = draw(json!({"size": 60, "transform": "uppercase"}));
    assert!(upper.2 > base.2, "{base:?} {upper:?}");
    let italic = draw(json!({"size": 60, "italic": true, "weight": 800}));
    assert_ne!(italic, base);
}

#[test]
fn narrower_boxes_wrap_into_more_lines_using_real_glyph_widths() {
    if !has_fonts() {
        return;
    }
    let theme = Theme::default();
    let mut text = Typesetter::new(&theme, None);
    let family = font_stack(&theme, "body");
    let setting = Setting {
        family: &family,
        weight: 400,
        italic: false,
        em: 35.2,
        line_height: 1.35,
        align: Align::Left,
        spacing: 0.0,
        transform: Transform::None,
    };
    let runs = inline("A sentence with **bold words** and a longer ending that has to wrap.");
    let wide = text.wrap(&runs, &setting, 1400.0).len();
    let narrow = text.wrap(&runs, &setting, 400.0).len();
    let narrower = text.wrap(&runs, &setting, 200.0).len();
    assert_eq!(wide, 1);
    assert!(
        narrow > wide && narrower > narrow,
        "{wide} {narrow} {narrower}"
    );
    // A single unbreakable word is split between characters, not lost.
    let long = [crate::deck::text::Run {
        text: "w".repeat(200),
        ..Default::default()
    }];
    assert!(text.wrap(&long, &setting, 300.0).len() > 2);
}

#[test]
fn overflow_stays_visible_below_a_fixed_box() {
    if !has_fonts() {
        return;
    }
    let long = "overflowing words ".repeat(60);
    let deck = deck_of(
        json!({"mode": "none"}),
        json!([{"id": "t", "kind": "text", "text": long, "position": {"x": 100, "y": 100}, "sizing": {"w": 800, "h": 100}}]),
    );
    let image = rgba(&render_slide(&deck, "s", 1920).unwrap());
    let rect = Rect {
        x: 0.0,
        y: 220.0,
        w: 1920.0,
        h: 800.0,
    };
    assert!(
        inked(&image, rect, 1920),
        "text beyond the box must still be drawn"
    );
}

#[test]
fn markdown_runs_and_lines_match_the_html_subset() {
    let runs = inline("a **b** *c* `d` [e](https://x.y) *f");
    let kinds: Vec<_> = runs.iter().map(|r| (r.text.as_str(), r.mark)).collect();
    assert_eq!(
        kinds,
        [
            ("a ", Mark::Plain),
            ("b", Mark::Bold),
            (" ", Mark::Plain),
            ("c", Mark::Italic),
            (" ", Mark::Plain),
            ("d", Mark::Code),
            (" ", Mark::Plain),
            ("e", Mark::Link),
            (" *f", Mark::Plain),
        ]
    );
    assert_eq!(classify("## Hi"), Row::Heading(2, "Hi"));
    assert_eq!(classify("####   x"), Row::Paragraph("####   x"));
    assert_eq!(classify("  - item"), Row::Bullet("item"));
    assert_eq!(classify("* item"), Row::Bullet("item"));
    assert_eq!(classify("**bold** x"), Row::Paragraph("**bold** x"));
    assert_eq!(classify("12. item"), Row::Numbered("item"));
    assert_eq!(classify("12.item"), Row::Paragraph("12.item"));
}

#[test]
fn sheets_show_every_requested_slide_and_validate_input() {
    let deck = Deck::classic_fixture("Sheet");
    let ids: Vec<String> = deck.slides.iter().map(|s| s.id.clone()).collect();
    let one = rgba(&render_deck_sheet(&deck, &ids, 1).unwrap());
    let two = rgba(&render_deck_sheet(&deck, &ids, 2).unwrap());
    assert!(two.width() > one.width() && two.height() < one.height());
    assert!(render_deck_sheet(&deck, &[], 2).is_err());
    assert!(render_deck_sheet(&deck, &ids, 0).is_err());
    assert!(render_deck_sheet(&deck, &["missing".into()], 2).is_err());
}

#[test]
fn imported_deck_fonts_are_registered_under_their_family() {
    // Any installed font file stands in for an imported one.
    let db = system_fonts();
    let Some(face) = db.faces().find(|f| {
        matches!(&f.source, usvg::fontdb::Source::File(p)
            if p.extension().is_some_and(|e| e == "ttf" || e == "otf")
                && std::fs::metadata(p).is_ok_and(|m| m.len() < 4_000_000))
    }) else {
        return;
    };
    let usvg::fontdb::Source::File(source) = &face.source else {
        return;
    };
    let root = std::env::temp_dir().join(format!("nexus-fonts-{}", uuid::Uuid::new_v4()));
    let fonts = root.join("assets/fonts");
    std::fs::create_dir_all(&fonts).unwrap();
    let name = format!("brand.{}", source.extension().unwrap().to_string_lossy());
    std::fs::copy(source, fonts.join(&name)).unwrap();
    let mut deck = Deck::titled("Fonts");
    deck.theme.font_files = vec![crate::deck::FontFile {
        family: "Nexus Test Brand".into(),
        file: format!("assets/fonts/{name}"),
        weight: 400,
        italic: false,
        fallback: vec![],
    }];
    let imported = crate::deck::text::deck_fonts(&deck.theme, Some(&root.join("assets")));
    assert!(
        imported
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| n == "Nexus Test Brand"))
    );
    assert!(
        !system_fonts()
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| n == "Nexus Test Brand")),
        "the shared system database is left alone"
    );
    // And a file outside assets, or an unreadable one, is skipped.
    deck.theme.font_files[0].file = "assets/fonts/none.ttf".into();
    let skipped = crate::deck::text::deck_fonts(&deck.theme, Some(&root.join("assets")));
    assert!(
        !skipped
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| n == "Nexus Test Brand"))
    );
    // Rendering with the imported family draws something.
    deck.theme.font_files[0].file = format!("assets/fonts/{name}");
    deck.theme
        .fonts
        .insert("body".into(), "Nexus Test Brand".into());
    let png = render_slide_at(&deck, "s1", 800, Some(&root.join("assets"))).unwrap();
    assert!(inked(
        &rgba(&png),
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0
        },
        800
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cache_names_by_revision_prunes_old_ones_and_keeps_snapshots() {
    let root = std::env::temp_dir().join(format!("nexus-render-{}", uuid::Uuid::new_v4()));
    let files = root.join("files");
    let path = files.join("decks/a.deck.json");
    let mut deck = Deck::classic_fixture("Cache");
    let first = render_cached(&deck, &path, &files, &["s1".into()], false).unwrap();
    assert!(
        first.starts_with("decks/.renders/a/") && first.ends_with(".png"),
        "{first}"
    );
    assert!(files.join(&first).is_file());
    let snapshot = render_snapshot(&deck, &path, &files, "s1").unwrap();
    assert!(snapshot.contains("snap-"));
    deck.title = "Changed".into();
    let second = render_cached(&deck, &path, &files, &["s1".into()], false).unwrap();
    assert_ne!(first, second);
    assert!(!files.join(&first).exists(), "old revision is pruned");
    assert!(files.join(&second).is_file());
    assert!(files.join(&snapshot).is_file(), "snapshots are durable");
    let sheet = render_cached(&deck, &path, &files, &["s1".into(), "s2".into()], true).unwrap();
    assert!(files.join(&sheet).is_file() && files.join(&second).is_file());
    std::fs::remove_dir_all(root).unwrap();
}

/// `NEXUS_RENDER_OUT=<dir> cargo test -p nexus-chat-core write_fixture_renders`
/// writes the migrated classic deck and an auto-layout deck, to compare with
/// the HTML renderer. Does nothing unless the variable is set.
#[test]
fn write_fixture_renders() {
    let Some(out) = std::env::var_os("NEXUS_RENDER_OUT") else {
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let classic = Deck::classic_fixture("A better story");
    for (id, name) in [("s1", "title"), ("s2", "content")] {
        std::fs::write(
            out.join(format!("core-classic-{name}.png")),
            render_slide(&classic, id, 1600).unwrap(),
        )
        .unwrap();
    }
    let auto = Deck::new_from_layouts("An auto-layout story");
    for (id, name) in [("s1", "title"), ("s2", "content-chart")] {
        std::fs::write(
            out.join(format!("core-autolayout-{name}.png")),
            render_slide(&auto, id, 1600).unwrap(),
        )
        .unwrap();
    }
}
