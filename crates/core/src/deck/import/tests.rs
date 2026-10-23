use std::path::PathBuf;

use super::fixtures::{Package, slide};
use super::*;
use crate::deck::export::{self, plain};
use crate::deck::{Kind, Node, Slide, layout_at};

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn assets(&self) -> PathBuf {
        self.0.join("assets")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn import(package: &Package, temp: &Temp) -> Imported {
    import_pptx(&package.bytes(), &temp.assets()).unwrap()
}

fn only(deck: &Deck) -> &Slide {
    assert_eq!(deck.slides.len(), 1);
    &deck.slides[0]
}

/// Every node under `nodes`, depth first.
fn walk<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        walk(node.children(), out);
    }
}

fn all(slide: &Slide) -> Vec<&Node> {
    let mut out = Vec::new();
    walk(&slide.children, &mut out);
    out
}

fn texts(node: &Node) -> Vec<String> {
    let mut nodes = vec![node];
    walk(node.children(), &mut nodes);
    nodes
        .into_iter()
        .filter_map(|n| match &n.kind {
            Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// -- round trips through the exporter ------------------------------------------

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 2.0
}

fn assert_round_trip(original: &Deck) {
    let temp = Temp::new();
    let bytes = export::pptx::export_pptx(original, None).unwrap();
    let imported = import_pptx(&bytes, &temp.assets()).unwrap();
    let back = &imported.deck;
    assert_eq!(back.slides.len(), original.slides.len());
    assert_eq!(back.theme.size, original.theme.size);
    for (before, after) in original.slides.iter().zip(&back.slides) {
        assert_eq!(after.notes, before.notes, "notes of {}", before.id);
        match (&before.transition, &after.transition) {
            (Some(b), Some(a)) if b.kind != crate::deck::TransitionType::None => {
                assert_eq!(
                    (a.kind, a.direction, a.duration_ms),
                    (b.kind, b.direction, b.duration_ms)
                );
            }
            (Some(b), None) => assert_eq!(b.kind, crate::deck::TransitionType::None),
            _ => {}
        }
        let rects_before = layout_at(original, before, None);
        let rects_after = layout_at(back, after, Some(&temp.assets()));
        let imported_nodes = all(after);
        for node in export::visible_nodes(before) {
            let found = imported_nodes
                .iter()
                .find(|n| n.name.as_deref() == Some(node.name.as_deref().unwrap_or(&node.id)))
                .unwrap_or_else(|| panic!("no node for {} on {}", node.id, before.id));
            let expect = match &node.kind {
                Kind::Frame { .. } | Kind::Code { .. } => "frame",
                Kind::Text { .. } => "text",
                Kind::Image { .. } | Kind::Chart { .. } => "image",
                Kind::Shape { .. } => "shape",
                Kind::Table { .. } => "table",
            };
            assert_eq!(found.kind.name(), expect, "kind of {}", node.id);
            let a = rects_before
                .iter()
                .find(|(id, _)| *id == node.id)
                .unwrap()
                .1;
            let b = rects_after
                .iter()
                .find(|(id, _)| *id == found.id)
                .unwrap()
                .1;
            if matches!(&node.kind, Kind::Shape { shape, .. } if matches!(shape, crate::deck::Shape::Line | crate::deck::Shape::Arrow))
            {
                // A line is its length and its stroke's thickness, centred.
                assert!(
                    near(a.x, b.x) && near(a.w, b.w) && near(a.y + a.h / 2.0, b.y + b.h / 2.0),
                    "{a:?} {b:?}"
                );
            } else {
                assert!(
                    near(a.x, b.x) && near(a.y, b.y) && near(a.w, b.w) && near(a.h, b.h),
                    "{}: {a:?} vs {b:?}",
                    node.id
                );
            }
            match &node.kind {
                Kind::Text { text, .. } | Kind::Code { source: text, .. } => {
                    let got = texts(found).join("\n");
                    assert_eq!(plain(&got), plain(text), "text of {}", node.id);
                }
                Kind::Table { rows, .. } => {
                    let Kind::Table { rows: got, .. } = &found.kind else {
                        unreachable!()
                    };
                    assert_eq!(got, rows);
                }
                _ => {}
            }
        }
    }
}

#[test]
fn the_classic_fixture_round_trips_through_the_exporter() {
    assert_round_trip(&Deck::classic_fixture("A better story"));
}

#[test]
fn the_motion_fixture_round_trips_through_the_exporter() {
    let motion = Deck::check(include_str!(
        "../../../tests/fixtures/deck/export-motion.deck.json"
    ))
    .unwrap();
    assert_round_trip(&motion);
}

// -- hand-built packages ---------------------------------------------------------

/// A text shape: `props` go in `p:spPr` after the transform, `body` is the
/// content of `p:txBody` after `a:lstStyle`.
fn sp(id: u32, name: &str, ph: &str, spr: &str, body: &str) -> String {
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{name}\"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr>{spr}</p:spPr><p:txBody><a:bodyPr/><a:lstStyle/>{body}</p:txBody></p:sp>"
    )
}

fn para(text: &str) -> String {
    format!("<a:p><a:r><a:rPr lang=\"en-US\"/><a:t>{text}</a:t></a:r></a:p>")
}

fn rect_spr(x: i64, y: i64, w: i64, h: i64, extra: &str) -> String {
    format!(
        "{}<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>{extra}",
        fixtures::xfrm(x, y, w, h)
    )
}

fn text_of(node: &Node) -> &str {
    match &node.kind {
        Kind::Text { text, .. } => text,
        other => panic!("not text: {other:?}"),
    }
}

fn abs(node: &Node) -> (f64, f64) {
    match node.position {
        crate::deck::Position::Absolute { x, y } => (x, y),
        crate::deck::Position::Auto => panic!("not absolute"),
    }
}

fn size(node: &Node) -> (f64, f64) {
    match (node.sizing.w, node.sizing.h) {
        (crate::deck::Dim::Fixed(w), crate::deck::Dim::Fixed(h)) => (w, h),
        other => panic!("not fixed: {other:?}"),
    }
}

fn assert_close(a: f64, b: f64) {
    assert!((a - b).abs() < 0.05, "{a} != {b}");
}

#[test]
fn placeholders_inherit_position_size_and_text_style_from_layout_and_master() {
    let layout = format!(
        "{}{}",
        sp(
            2,
            "T",
            "<p:ph type=\"title\"/>",
            "",
            "<a:p><a:endParaRPr/></a:p>"
        ),
        sp(
            3,
            "B",
            "<p:ph idx=\"1\"/>",
            &fixtures::xfrm(1_270_000, 2_540_000, 6_350_000, 3_175_000),
            "<a:p><a:endParaRPr/></a:p>"
        )
    );
    let shapes = format!(
        "{}{}{}",
        sp(2, "Title 1", "<p:ph type=\"title\"/>", "", &para("Hello")),
        sp(
            3,
            "Content 2",
            "<p:ph idx=\"1\"/>",
            "",
            &format!(
                "{}<a:p><a:pPr lvl=\"1\"/><a:r><a:t>Beta</a:t></a:r></a:p>",
                para("Alpha item")
            )
        ),
        // Slide numbers come from the master and are not imported.
        sp(
            4,
            "Number",
            "<p:ph type=\"sldNum\" idx=\"4\"/>",
            "",
            &para("9")
        )
    );
    let mut package = Package::new(vec![slide(&shapes)]);
    package.layout_tree = layout;
    let temp = Temp::new();
    let imported = import(&package, &temp);
    let slide = only(&imported.deck);
    assert_eq!(slide.children.len(), 2, "{slide:#?}");
    let title = &slide.children[0];
    assert_eq!(text_of(title), "Hello");
    // The master's box, less the default 0.1 in side and 0.05 in top insets.
    assert_close(abs(title).0, 609_600.0 / 6350.0 + 14.4);
    assert_close(abs(title).1, 304_800.0 / 6350.0 + 7.2);
    assert_close(size(title).0, 10_972_800.0 / 6350.0 - 28.8);
    let Kind::Text { style, font, .. } = &title.kind else {
        unreachable!()
    };
    assert_eq!(style, "title");
    // 44 pt bold in the master's title style is the deck's title style.
    assert!(font.is_empty(), "{font:?}");
    assert_eq!(imported.deck.theme.text_styles["title"].size, Some(88.0));
    assert_eq!(imported.deck.theme.text_styles["title"].weight, Some(700));
    let body = &slide.children[1];
    assert_eq!(text_of(body), "- Alpha item\n  - Beta");
    // The layout's box wins over the master's.
    assert_close(abs(body).0, 200.0 + 14.4);
    assert_close(abs(body).1, 400.0 + 7.2);
    assert_close(size(body).0, 1000.0 - 28.8);
    let Kind::Text { font, .. } = &body.kind else {
        unreachable!()
    };
    assert!(font.is_empty(), "{font:?}");
}

#[test]
fn numbered_and_nested_bullets_become_markdown_lists() {
    let numbered = "<a:p><a:pPr><a:buAutoNum type=\"arabicPeriod\"/></a:pPr><a:r><a:t>First</a:t></a:r></a:p><a:p><a:pPr><a:buAutoNum type=\"arabicPeriod\"/></a:pPr><a:r><a:t>Second</a:t></a:r></a:p><a:p><a:pPr lvl=\"2\"><a:buChar char=\"x\"/></a:pPr><a:r><a:rPr b=\"1\"/><a:t>Deep</a:t></a:r><a:r><a:rPr i=\"1\"/><a:t> and slanted</a:t></a:r></a:p><a:p><a:pPr><a:buNone/></a:pPr><a:r><a:t>- not a list</a:t></a:r></a:p>";
    let shapes = sp(
        2,
        "Box",
        "",
        &rect_spr(0, 0, 6_350_000, 3_175_000, "<a:noFill/>"),
        numbered,
    );
    let temp = Temp::new();
    let imported = import(&Package::new(vec![slide(&shapes)]), &temp);
    let node = &only(&imported.deck).children[0];
    let text = text_of(node);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "1. First");
    assert_eq!(lines[1], "1. Second");
    assert_eq!(lines[2], "    - **Deep** *and slanted*");
    // Text that merely looks like a list item stays text.
    assert!(!lines[3].starts_with("- "), "{:?}", lines[3]);
    assert!(lines[3].ends_with("- not a list"));
}

#[test]
fn theme_colors_and_fonts_become_the_deck_theme() {
    let fill = "<a:solidFill><a:schemeClr val=\"accent1\"/></a:solidFill>";
    let custom = "<a:solidFill><a:srgbClr val=\"123456\"/></a:solidFill>";
    let shapes = format!(
        "{}{}{}",
        sp(2, "Accent", "", &rect_spr(0, 0, 635_000, 635_000, fill), ""),
        sp(
            3,
            "Custom",
            "",
            &rect_spr(0, 0, 635_000, 635_000, custom),
            ""
        ),
        sp(
            4,
            "Words",
            "",
            &rect_spr(0, 1_000_000, 3_000_000, 635_000, "<a:noFill/>"),
            "<a:p><a:r><a:rPr sz=\"2000\"><a:latin typeface=\"Arial\"/><a:solidFill><a:schemeClr val=\"accent2\"/></a:solidFill></a:rPr><a:t>Arial words</a:t></a:r></a:p>"
        )
    );
    let temp = Temp::new();
    let imported = import(&Package::new(vec![slide(&shapes)]), &temp);
    let deck = &imported.deck;
    assert_eq!(deck.theme.colors["fg"], "#111111");
    assert_eq!(deck.theme.colors["bg"], "#ffffff");
    assert_eq!(deck.theme.colors["accent"], "#4472c4");
    assert_eq!(deck.theme.fonts["heading"], "Georgia");
    assert_eq!(deck.theme.fonts["body"], "Verdana");
    let children = &only(deck).children;
    let Kind::Shape { fill, .. } = &children[0].kind else {
        panic!()
    };
    assert_eq!(fill.as_deref(), Some("accent"));
    let Kind::Shape { fill, .. } = &children[1].kind else {
        panic!()
    };
    assert_eq!(fill.as_deref(), Some("#123456"));
    let Kind::Text { font, .. } = &children[2].kind else {
        panic!()
    };
    assert_eq!(font.family.as_deref(), Some("Arial"));
    assert_eq!(font.size, Some(40.0));
    assert_eq!(font.color.as_deref(), Some("#ed7d31"));
}

#[test]
fn groups_apply_their_child_offsets_and_scale() {
    let group = format!(
        "<p:grpSp><p:nvGrpSpPr><p:cNvPr id=\"10\" name=\"Group\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"1000000\" y=\"1000000\"/><a:ext cx=\"2000000\" cy=\"1000000\"/><a:chOff x=\"500000\" y=\"500000\"/><a:chExt cx=\"4000000\" cy=\"2000000\"/></a:xfrm></p:grpSpPr>{}</p:grpSp>",
        sp(
            11,
            "Inner",
            "",
            &rect_spr(
                1_500_000,
                1_000_000,
                2_000_000,
                1_000_000,
                "<a:solidFill><a:srgbClr val=\"FF0000\"/></a:solidFill>"
            ),
            ""
        )
    );
    let temp = Temp::new();
    let imported = import(&Package::new(vec![slide(&group)]), &temp);
    let frame = &only(&imported.deck).children[0];
    assert_eq!(frame.kind.name(), "frame");
    assert_close(abs(frame).0, 1_000_000.0 / 6350.0);
    assert_close(size(frame).0, 2_000_000.0 / 6350.0);
    let inner = &frame.children()[0];
    // (1.5M - 0.5M) * (2M / 4M) EMU from the group's corner.
    assert_close(abs(inner).0, 500_000.0 / 6350.0);
    assert_close(abs(inner).1, 250_000.0 / 6350.0);
    assert_close(size(inner).0, 1_000_000.0 / 6350.0);
    assert_close(size(inner).1, 500_000.0 / 6350.0);
}

fn pic(id: u32, rid: &str, crop: &str, x: i64, y: i64, w: i64, h: i64) -> String {
    format!(
        "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"Picture {id}\"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"{rid}\"/>{crop}<a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}</p:spPr></p:pic>",
        fixtures::xfrm(x, y, w, h)
    )
}

#[test]
fn pictures_are_copied_with_crops_baked_in_and_unreadable_ones_become_placeholders() {
    let shapes = format!(
        "{}{}{}",
        pic(
            2,
            "rId5",
            "<a:srcRect l=\"25000\" r=\"25000\"/>",
            0,
            0,
            1_270_000,
            1_270_000
        ),
        pic(3, "rId5", "", 2_000_000, 0, 1_270_000, 635_000),
        pic(4, "rId6", "", 4_000_000, 0, 1_270_000, 635_000)
    );
    let mut package = Package::new(vec![
        slide(&shapes)
            .rel("rId5", "image", "../media/image1.png")
            .rel("rId6", "image", "../media/image2.emf"),
    ]);
    package
        .extra
        .push(("ppt/media/image1.png".into(), fixtures::png(100, 50)));
    package.extra.push((
        "ppt/media/image2.emf".into(),
        b"\x01\x00\x00\x00 not an image".to_vec(),
    ));
    let temp = Temp::new();
    let imported = import(&package, &temp);
    let children = &only(&imported.deck).children;
    let (Kind::Image { src: cropped, fit }, Kind::Image { src: whole, .. }) =
        (&children[0].kind, &children[1].kind)
    else {
        panic!("{children:#?}")
    };
    assert_eq!(*fit, crate::deck::Fit::Cover);
    assert_ne!(cropped, whole);
    let path = |src: &str| temp.0.join(src);
    assert_eq!(image::image_dimensions(path(cropped)).unwrap(), (50, 50));
    assert_eq!(image::image_dimensions(path(whole)).unwrap(), (100, 50));
    // The cropped copy kept the left half's color only: red.
    let pixel = image::open(path(cropped))
        .unwrap()
        .to_rgb8()
        .get_pixel(10, 10)
        .0;
    assert!(pixel[0] > 150 && pixel[2] < 100, "{pixel:?}");
    // The format the deck cannot hold stands in as a placeholder.
    assert_eq!(children[2].name.as_deref(), Some("Picture (not imported)"));
    assert!(
        imported.warnings.iter().any(|w| w.contains("emf")),
        "{:?}",
        imported.warnings
    );
    assert!(imported.warnings.iter().any(|w| w.contains("cropped")));
    imported
        .deck
        .slides
        .iter()
        .flat_map(Slide::nodes)
        .for_each(|n| {
            if let Kind::Image { src, .. } = &n.kind {
                assert!(
                    crate::deck::image_resolves(src, Some(&temp.assets())),
                    "{src}"
                );
            }
        });
}

#[test]
fn a_background_image_is_a_full_bleed_node_at_the_back_and_a_color_fills_the_slide() {
    let slide_xml = slide(&sp(2, "Box", "", &rect_spr(0, 0, 635_000, 635_000, "<a:noFill/>"), &para("Front")))
        .rel("rId5", "image", "../media/image1.png")
        .background("<p:bgPr><a:blipFill><a:blip r:embed=\"rId5\"/><a:stretch><a:fillRect/></a:stretch></a:blipFill><a:effectLst/></p:bgPr>");
    let colored = slide("").background(
        "<p:bgPr><a:solidFill><a:srgbClr val=\"102030\"/></a:solidFill><a:effectLst/></p:bgPr>",
    );
    let mut package = Package::new(vec![slide_xml, colored]);
    package
        .extra
        .push(("ppt/media/image1.png".into(), fixtures::png(64, 36)));
    let temp = Temp::new();
    let imported = import(&package, &temp);
    let first = &imported.deck.slides[0];
    assert_eq!(first.children.len(), 2);
    assert_eq!(first.children[0].name.as_deref(), Some("Background"));
    assert_eq!(size(&first.children[0]), (1920.0, 1080.0));
    assert_eq!(first.children[1].kind.name(), "text");
    assert_eq!(imported.deck.slides[1].fill.as_deref(), Some("#102030"));
}

#[test]
fn tables_become_table_nodes_and_charts_become_placeholders_with_a_warning() {
    let table = "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"5\" name=\"Table 4\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"635000\" y=\"635000\"/><a:ext cx=\"6350000\" cy=\"1905000\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/table\"><a:tbl><a:tblPr/><a:tblGrid><a:gridCol w=\"3175000\"/><a:gridCol w=\"3175000\"/></a:tblGrid><a:tr h=\"635000\"><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:rPr sz=\"1600\" b=\"1\"/><a:t>Head</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>Two</a:t></a:r></a:p></a:txBody></a:tc></a:tr><a:tr h=\"635000\"><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>a &amp; b</a:t></a:r></a:p><a:p><a:r><a:t>more</a:t></a:r></a:p></a:txBody></a:tc><a:tc hMerge=\"1\"><a:txBody><a:bodyPr/><a:p><a:endParaRPr/></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>";
    let chart = "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"6\" name=\"Chart 5\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"635000\" y=\"3175000\"/><a:ext cx=\"3175000\" cy=\"1905000\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" r:id=\"rId9\"/></a:graphicData></a:graphic></p:graphicFrame>";
    let temp = Temp::new();
    let imported = import(
        &Package::new(vec![slide(&format!("{table}{chart}"))]),
        &temp,
    );
    let children = &only(&imported.deck).children;
    let Kind::Table { rows, font, .. } = &children[0].kind else {
        panic!("{children:#?}")
    };
    assert_eq!(
        rows,
        &vec![
            vec!["Head".to_string(), "Two".into()],
            vec!["a & b more".into(), String::new()]
        ]
    );
    assert_eq!(font.size, Some(32.0));
    assert_eq!(font.weight, Some(700));
    assert_eq!(size(&children[0]), (1000.0, 300.0));
    let Kind::Image { .. } = &children[1].kind else {
        panic!()
    };
    assert_eq!(children[1].name.as_deref(), Some("Chart (not imported)"));
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("chart") && w.contains("slide 1"))
    );
}

#[test]
fn lines_arrows_and_rotation_keep_their_geometry() {
    let connector = "<p:cxnSp><p:nvCxnSpPr><p:cNvPr id=\"3\" name=\"Arrow\"/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr><p:spPr><a:xfrm flipV=\"1\"><a:off x=\"635000\" y=\"635000\"/><a:ext cx=\"3810000\" cy=\"2540000\"/></a:xfrm><a:prstGeom prst=\"straightConnector1\"><a:avLst/></a:prstGeom><a:ln w=\"25400\"><a:solidFill><a:srgbClr val=\"00AA00\"/></a:solidFill><a:tailEnd type=\"triangle\"/></a:ln></p:spPr></p:cxnSp>";
    let temp = Temp::new();
    let imported = import(&Package::new(vec![slide(connector)]), &temp);
    let node = &only(&imported.deck).children[0];
    let Kind::Shape { shape, stroke, .. } = &node.kind else {
        panic!()
    };
    assert_eq!(*shape, crate::deck::Shape::Arrow);
    assert_close(stroke.as_ref().unwrap().width, 4.0);
    // Up and to the right: the corner-to-corner length, turned 33.7 degrees
    // counter-clockwise (326.3 clockwise).
    assert_close(size(node).0, (3_810_000.0_f64).hypot(2_540_000.0) / 6350.0);
    assert_close(node.rotation.unwrap(), 360.0 - 33.69);
}

#[test]
fn shapes_with_text_keep_both_as_one_frame_and_unknown_parts_only_warn() {
    let boxed = sp(
        2,
        "Callout",
        "",
        &rect_spr(
            635_000,
            635_000,
            3_175_000,
            1_270_000,
            "<a:solidFill><a:srgbClr val=\"FFF5D6\"/></a:solidFill><a:ln w=\"12700\"><a:solidFill><a:srgbClr val=\"F89018\"/></a:solidFill></a:ln>",
        ),
        "<a:p><a:pPr algn=\"ctr\"/><a:r><a:rPr sz=\"2000\"/><a:t>Centered</a:t></a:r></a:p>",
    );
    let odd = "<p:somethingNew><p:inner/></p:somethingNew><p:sp><p:nvSpPr><p:cNvPr id=\"9\" name=\"Triangle\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"635000\" cy=\"635000\"/></a:xfrm><a:prstGeom prst=\"triangle\"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val=\"00FF00\"/></a:solidFill></p:spPr></p:sp>";
    let mut package = Package::new(vec![slide(&format!("{boxed}{odd}"))]);
    package
        .extra
        .push(("ppt/embeddings/oleObject1.bin".into(), vec![1, 2, 3]));
    package
        .extra
        .push(("customXml/item1.xml".into(), b"<x/>".to_vec()));
    let temp = Temp::new();
    let imported = import(&package, &temp);
    let children = &only(&imported.deck).children;
    assert_eq!(children.len(), 2);
    let frame = &children[0];
    assert_eq!(frame.kind.name(), "frame");
    assert_eq!(frame.children().len(), 2);
    assert_eq!(frame.children()[0].kind.name(), "shape");
    let Kind::Text { font, .. } = &frame.children()[1].kind else {
        panic!()
    };
    assert_eq!(font.align, Some(crate::deck::Align::Center));
    assert!(
        imported.warnings.iter().any(|w| w.contains("triangle")),
        "{:?}",
        imported.warnings
    );
}

#[test]
fn radial_gradients_become_stacked_translucent_ellipses_and_custom_paths_keep_their_bounds() {
    let glow = sp(
        2,
        "Glow",
        "",
        &rect_spr(
            0,
            0,
            6_350_000,
            6_350_000,
            "<a:gradFill><a:gsLst><a:gs pos=\"0\"><a:srgbClr val=\"F0C000\"><a:alpha val=\"40000\"/></a:srgbClr></a:gs><a:gs pos=\"60000\"><a:srgbClr val=\"F0C000\"><a:alpha val=\"0\"/></a:srgbClr></a:gs></a:gsLst><a:path path=\"circle\"/></a:gradFill>",
        ),
        "",
    );
    let stripe = "<p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Stripe\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"6350000\" cy=\"635000\"/></a:xfrm><a:custGeom><a:pathLst><a:path w=\"6350000\" h=\"635000\"><a:moveTo><a:pt x=\"0\" y=\"0\"/></a:moveTo><a:lnTo><a:pt x=\"63500\" y=\"0\"/></a:lnTo><a:lnTo><a:pt x=\"63500\" y=\"635000\"/></a:lnTo><a:lnTo><a:pt x=\"0\" y=\"635000\"/></a:lnTo><a:close/></a:path></a:pathLst></a:custGeom><a:solidFill><a:srgbClr val=\"F89018\"/></a:solidFill></p:spPr></p:sp>";
    let temp = Temp::new();
    let imported = import(
        &Package::new(vec![slide(&format!("{glow}{stripe}"))]),
        &temp,
    );
    let children = &only(&imported.deck).children;
    let rings = children[0].children();
    assert!(rings.len() >= 5, "{}", rings.len());
    assert!(rings.iter().all(|r| matches!(
        &r.kind,
        Kind::Shape {
            shape: crate::deck::Shape::Ellipse,
            ..
        }
    )));
    // The widest ring comes first and stays inside the box.
    assert!(size(&rings[0]).0 > size(&rings[rings.len() - 1]).0);
    assert!(size(&rings[0]).0 <= 1000.0);
    // A path that fills a 1 % strip is a 1 % strip, not the whole box.
    assert_close(size(&children[1]).0, 10.0);
    assert_close(size(&children[1]).1, 100.0);
}

#[test]
fn transitions_notes_and_entrance_animations_are_imported() {
    let shapes = sp(
        5,
        "Animated",
        "",
        &rect_spr(0, 0, 1_270_000, 635_000, "<a:noFill/>"),
        &para("Fade me"),
    );
    let transition = "<p:transition spd=\"med\"><p:push dir=\"u\"/></p:transition>";
    let timing = "<p:timing><p:tnLst><p:par><p:cTn id=\"1\" dur=\"indefinite\" restart=\"never\" nodeType=\"tmRoot\"><p:childTnLst><p:seq concurrent=\"1\" nextAc=\"seek\"><p:cTn id=\"2\" dur=\"indefinite\" nodeType=\"mainSeq\"><p:childTnLst><p:par><p:cTn id=\"3\" fill=\"hold\"><p:stCondLst><p:cond delay=\"indefinite\"/></p:stCondLst><p:childTnLst><p:par><p:cTn id=\"4\" fill=\"hold\"><p:stCondLst><p:cond delay=\"0\"/></p:stCondLst><p:childTnLst><p:par><p:cTn id=\"5\" presetID=\"10\" presetClass=\"entr\" presetSubtype=\"0\" fill=\"hold\" nodeType=\"clickEffect\"><p:stCondLst><p:cond delay=\"0\"/></p:stCondLst><p:childTnLst><p:animEffect transition=\"in\" filter=\"fade\"><p:cBhvr><p:cTn id=\"6\" dur=\"500\"/><p:tgtEl><p:spTgt spid=\"5\"/></p:tgtEl></p:cBhvr></p:animEffect></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst></p:timing>";
    let spec = slide(&shapes)
        .after_tree(&format!("{transition}{timing}"))
        .rel("rId7", "notesSlide", "../notesSlides/notesSlide1.xml");
    let mut package = Package::new(vec![spec]);
    let notes = format!(
        "<p:notes {}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Image\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"sldImg\"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Notes\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>first</a:t></a:r></a:p><a:p><a:r><a:t>second</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>",
        fixtures::NS
    );
    package
        .extra
        .push(("ppt/notesSlides/notesSlide1.xml".into(), notes.into_bytes()));
    let temp = Temp::new();
    let imported = import(&package, &temp);
    let slide = only(&imported.deck);
    assert_eq!(slide.notes, "first\nsecond");
    let transition = slide.transition.as_ref().unwrap();
    assert_eq!(transition.kind, crate::deck::TransitionType::Push);
    assert_eq!(transition.direction, Some(crate::deck::Direction::Up));
    assert_eq!(transition.duration_ms, 500.0);
    let node = &slide.children[0];
    let build = node.animation.as_ref().unwrap().build_in.as_ref().unwrap();
    assert_eq!(build.effect, crate::deck::Effect::Fade);
    assert_eq!(build.trigger, crate::deck::Trigger::Click);
    assert_eq!(build.duration_ms, 500.0);
    assert_eq!(slide.builds, vec![node.id.clone()]);
}

#[test]
fn smartart_without_a_saved_drawing_keeps_its_text_with_a_warning() {
    let data = format!(
        "<dgm:dataModel xmlns:dgm=\"http://schemas.openxmlformats.org/drawingml/2006/diagram\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><dgm:ptLst><dgm:pt modelId=\"1\" type=\"doc\"><dgm:t><a:p><a:r><a:t>ignore</a:t></a:r></a:p></dgm:t></dgm:pt><dgm:pt modelId=\"2\"><dgm:t><a:p><a:r><a:t>Step one</a:t></a:r></a:p></dgm:t></dgm:pt><dgm:pt modelId=\"3\"><dgm:t><a:p><a:r><a:t>Step two</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst></dgm:dataModel>"
    );
    let frame = "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"4\" name=\"Diagram\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"635000\" y=\"635000\"/><a:ext cx=\"6350000\" cy=\"3175000\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/diagram\"><dgm:relIds xmlns:dgm=\"http://schemas.openxmlformats.org/drawingml/2006/diagram\" r:dm=\"rId8\"/></a:graphicData></a:graphic></p:graphicFrame>";
    let mut package = Package::new(vec![slide(frame).rel(
        "rId8",
        "diagramData",
        "../diagrams/data1.xml",
    )]);
    package
        .extra
        .push(("ppt/diagrams/data1.xml".into(), data.into_bytes()));
    let temp = Temp::new();
    let imported = import(&package, &temp);
    let node = &only(&imported.deck).children[0];
    assert_eq!(text_of(node), "- Step one\n- Step two");
    assert!(imported.warnings.iter().any(|w| w.contains("SmartArt")));
}

#[test]
fn a_slide_with_too_many_shapes_keeps_the_first_ones_and_says_so() {
    let many: String = (0..230)
        .map(|i| {
            sp(
                10 + i,
                "Dot",
                "",
                &rect_spr(
                    i64::from(i) * 10_000,
                    0,
                    5_000,
                    5_000,
                    "<a:solidFill><a:srgbClr val=\"000000\"/></a:solidFill>",
                ),
                "",
            )
        })
        .collect();
    let temp = Temp::new();
    let imported = import(&Package::new(vec![slide(&many)]), &temp);
    assert_eq!(only(&imported.deck).nodes().len(), 200);
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("more than 200"))
    );
}

#[test]
fn other_slide_sizes_are_normalized_to_1920_wide() {
    let mut package = Package::new(vec![slide(&sp(
        2,
        "Box",
        "",
        &rect_spr(
            0,
            0,
            4_572_000,
            3_429_000,
            "<a:solidFill><a:srgbClr val=\"000000\"/></a:solidFill>",
        ),
        "",
    ))]);
    package.size = (9_144_000, 6_858_000);
    let temp = Temp::new();
    let imported = import(&package, &temp);
    assert_eq!(
        (imported.deck.theme.size.w, imported.deck.theme.size.h),
        (1920.0, 1440.0)
    );
    assert_eq!(size(&only(&imported.deck).children[0]), (960.0, 720.0));
}

#[test]
fn files_that_are_not_presentations_fail_with_a_clear_error() {
    let temp = Temp::new();
    let error = |bytes: &[u8]| import_pptx(bytes, &temp.assets()).unwrap_err().to_string();
    assert!(error(b"just some text").contains("not a .pptx"));
    assert!(error(&[]).contains("not a .pptx"));
    let zip_without_parts = fixtures::zip(&[("hello.txt".into(), b"hi".to_vec())]);
    assert!(error(&zip_without_parts).contains("not a PowerPoint presentation"));
    let no_slides = Package::new(vec![]).bytes();
    assert!(error(&no_slides).contains("no slides"));
}

#[test]
fn importing_a_file_never_overwrites_an_existing_deck() {
    let temp = Temp::new();
    let source = temp.0.join("My Talk.pptx");
    std::fs::write(
        &source,
        Package::new(vec![slide(&sp(
            2,
            "Box",
            "",
            &rect_spr(0, 0, 635_000, 635_000, "<a:noFill/>"),
            &para("Hi"),
        ))])
        .bytes(),
    )
    .unwrap();
    let decks = temp.0.join("decks");
    let first = import_pptx_file(&source, &decks, None).unwrap();
    let second = import_pptx_file(&source, &decks, None).unwrap();
    assert_eq!(first.path.file_name().unwrap(), "my-talk.deck.json");
    assert_eq!(second.path.file_name().unwrap(), "my-talk-2.deck.json");
    let saved = Deck::check(&std::fs::read_to_string(&first.path).unwrap()).unwrap();
    assert_eq!(saved, first.deck);
    assert!(import_pptx_file(&temp.0.join("nope.txt"), &decks, None).is_err());
}
