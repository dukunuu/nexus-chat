use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;
use crate::deck::Deck;

fn deck(layout: Value, children: Value) -> Deck {
    Deck::check(
        &json!({"version": 2, "title": "t", "slides": [{"id": "s", "layout": layout, "children": children}]})
            .to_string(),
    )
    .unwrap_or_else(|e| panic!("{e:?}"))
}

fn rects(deck: &Deck) -> HashMap<String, Rect> {
    layout_map(deck, &deck.slides[0], None)
}

fn boxed(id: &str, w: f64, h: f64) -> Value {
    json!({"id": id, "kind": "frame", "sizing": {"w": w, "h": h}})
}

fn at(r: &HashMap<String, Rect>, id: &str) -> (f64, f64, f64, f64) {
    let r = r[id];
    (r.x, r.y, r.w, r.h)
}

#[test]
fn a_row_shares_leftover_space_with_fill_children() {
    let d = deck(
        json!({"mode": "row", "gap": 20, "padding": 40}),
        json!([
            boxed("a", 200.0, 100.0),
            {"id": "b", "kind": "frame", "sizing": {"w": "fill", "h": 100}},
            boxed("c", 100.0, 100.0)
        ]),
    );
    let r = rects(&d);
    assert_eq!(at(&r, "a"), (40.0, 40.0, 200.0, 100.0));
    assert_eq!(at(&r, "b"), (260.0, 40.0, 1500.0, 100.0));
    assert_eq!(at(&r, "c"), (1780.0, 40.0, 100.0, 100.0));
}

#[test]
fn two_fill_children_split_evenly_and_respect_limits() {
    let d = deck(
        json!({"mode": "row", "gap": 10}),
        json!([
            {"id": "a", "kind": "frame", "sizing": {"w": "fill", "h": 50}},
            {"id": "b", "kind": "frame", "sizing": {"w": "fill", "h": 50, "max_w": 300}}
        ]),
    );
    let r = rects(&d);
    assert_eq!(r["b"].w, 300.0, "max_w stops the growth");
    assert_eq!(r["a"].w, 1920.0 - 300.0 - 10.0);
}

#[test]
fn cross_axis_alignment() {
    let children = || json!([boxed("a", 100.0, 100.0)]);
    for (cross, y) in [("start", 40.0), ("center", 490.0), ("end", 940.0)] {
        let d = deck(
            json!({"mode": "row", "padding": 40, "cross": cross}),
            children(),
        );
        assert_eq!(rects(&d)["a"].y, y, "{cross}");
    }
    let d = deck(
        json!({"mode": "row", "padding": 40, "cross": "stretch"}),
        json!([{"id": "a", "kind": "frame", "sizing": {"w": 100}}]),
    );
    assert_eq!(
        at(&rects(&d), "a"),
        (40.0, 40.0, 100.0, 1000.0),
        "hug stretches"
    );
    let d = deck(
        json!({"mode": "row", "padding": 40, "cross": "stretch"}),
        json!([boxed("a", 100.0, 100.0)]),
    );
    assert_eq!(rects(&d)["a"].h, 100.0, "fixed does not");
    let d = deck(
        json!({"mode": "row", "padding": 40, "cross": "start"}),
        json!([{"id": "a", "kind": "frame", "sizing": {"w": 100, "h": "fill"}}]),
    );
    assert_eq!(
        rects(&d)["a"].h,
        1000.0,
        "fill stretches whatever the parent says"
    );
}

#[test]
fn main_axis_distribution() {
    let three = || {
        json!([
            boxed("a", 100.0, 50.0),
            boxed("b", 100.0, 50.0),
            boxed("c", 100.0, 50.0)
        ])
    };
    let xs = |main: &str| {
        let d = deck(json!({"mode": "row", "main": main}), three());
        let r = rects(&d);
        ["a", "b", "c"].map(|id| r[id].x)
    };
    assert_eq!(xs("start"), [0.0, 100.0, 200.0]);
    assert_eq!(xs("center"), [810.0, 910.0, 1010.0]);
    assert_eq!(xs("end"), [1620.0, 1720.0, 1820.0]);
    assert_eq!(xs("space_between"), [0.0, 910.0, 1820.0]);
}

#[test]
fn columns_stack_with_gap_and_padding() {
    let d = deck(
        json!({"mode": "column", "gap": 12, "padding": [10, 20, 30, 40]}),
        json!([boxed("a", 100.0, 50.0), boxed("b", 100.0, 70.0)]),
    );
    let r = rects(&d);
    assert_eq!(at(&r, "a"), (40.0, 10.0, 100.0, 50.0));
    assert_eq!(at(&r, "b"), (40.0, 72.0, 100.0, 70.0));
}

#[test]
fn wrapping_starts_a_new_line_with_the_cross_gap() {
    let items: Vec<Value> = (0..4)
        .map(|i| boxed(&format!("i{i}"), 100.0, 50.0))
        .collect();
    let d = deck(
        json!({"mode": "none"}),
        json!([{"id": "f", "kind": "frame", "position": {"x": 0, "y": 0}, "sizing": {"w": 350, "h": 300},
                "layout": {"mode": "row", "wrap": true, "gap": 10, "gap_cross": 30}, "children": items}]),
    );
    let r = rects(&d);
    assert_eq!(at(&r, "i2"), (220.0, 0.0, 100.0, 50.0));
    assert_eq!(at(&r, "i3"), (0.0, 80.0, 100.0, 50.0));
}

#[test]
fn hug_frames_wrap_their_children() {
    let d = deck(
        json!({"mode": "column", "padding": 100}),
        json!([{"id": "f", "kind": "frame", "layout": {"mode": "column", "gap": 5, "padding": 10},
                "children": [boxed("a", 100.0, 40.0), boxed("b", 80.0, 20.0)]}]),
    );
    let r = rects(&d);
    assert_eq!(at(&r, "f"), (100.0, 100.0, 120.0, 85.0));
    assert_eq!(at(&r, "b"), (110.0, 155.0, 80.0, 20.0));
}

#[test]
fn nested_frames_report_absolute_positions() {
    let d = deck(
        json!({"mode": "column", "padding": 50}),
        json!([{"id": "outer", "kind": "frame", "layout": {"mode": "row", "padding": 20, "gap": 10},
                "sizing": {"w": 600, "h": 200},
                "children": [boxed("x", 100.0, 100.0),
                   {"id": "inner", "kind": "frame", "sizing": {"w": "fill", "h": "fill"},
                    "layout": {"mode": "none"},
                    "children": [{"id": "pin", "kind": "frame", "sizing": {"w": 10, "h": 10}, "position": {"x": 7, "y": 9}}]}]}]),
    );
    let r = rects(&d);
    assert_eq!(at(&r, "outer"), (50.0, 50.0, 600.0, 200.0));
    assert_eq!(at(&r, "x"), (70.0, 70.0, 100.0, 100.0));
    assert_eq!(at(&r, "inner"), (180.0, 70.0, 450.0, 160.0));
    assert_eq!(at(&r, "pin"), (187.0, 79.0, 10.0, 10.0));
}

#[test]
fn absolute_children_ignore_the_flow_and_none_parents_place_everything() {
    let d = deck(
        json!({"mode": "column", "gap": 10}),
        json!([boxed("a", 100.0, 50.0),
               {"id": "abs", "kind": "frame", "sizing": {"w": 30, "h": 30}, "position": {"x": 500, "y": 300}},
               {"id": "wide", "kind": "frame", "sizing": {"w": "fill", "h": 20}, "position": {"x": 100, "y": 0}},
               boxed("b", 100.0, 50.0)]),
    );
    let r = rects(&d);
    assert_eq!(at(&r, "abs"), (500.0, 300.0, 30.0, 30.0));
    assert_eq!(r["b"].y, 60.0, "the flow skips absolute children");
    assert_eq!(
        (r["wide"].x, r["wide"].w),
        (100.0, 1820.0),
        "fill pins the far edge"
    );
    let d = deck(
        json!({"mode": "none"}),
        json!([boxed("a", 100.0, 50.0), {"id": "m", "kind": "frame", "sizing": {"w": 5, "h": 5}, "position": {"x": 40, "y": 60}}]),
    );
    let r = rects(&d);
    assert_eq!(
        at(&r, "a"),
        (0.0, 0.0, 100.0, 50.0),
        "auto in a none parent sits at the origin"
    );
    assert_eq!(at(&r, "m"), (40.0, 60.0, 5.0, 5.0));
}

#[test]
fn hidden_nodes_take_no_space() {
    let d = deck(
        json!({"mode": "row", "gap": 20}),
        json!([boxed("a", 100.0, 50.0),
               {"id": "ghost", "kind": "frame", "hidden": true, "sizing": {"w": 500, "h": 50}},
               boxed("b", 100.0, 50.0)]),
    );
    let r = rects(&d);
    assert_eq!(r["b"].x, 120.0);
    assert_eq!(r["ghost"].w, 0.0);
}

#[test]
fn text_measures_with_real_glyphs_and_wraps_to_its_box() {
    let long = "A sentence that keeps going and going and going until it has to wrap somewhere.";
    let d = deck(
        json!({"mode": "column", "padding": 60, "cross": "start"}),
        json!([
            {"id": "hug", "kind": "text", "text": long},
            {"id": "boxed", "kind": "text", "text": long, "sizing": {"w": 600}},
            {"id": "narrow", "kind": "text", "text": long, "sizing": {"w": 300}},
            {"id": "big", "kind": "text", "text": long, "sizing": {"w": 600}, "font": {"size": 84}}
        ]),
    );
    let r = rects(&d);
    // 42 px at 1.35 is one 56.7 px line.
    assert!((r["hug"].h - 56.7).abs() < 0.1, "{:?}", r["hug"]);
    assert!(r["hug"].w > 600.0 && r["hug"].w <= 1800.0, "{:?}", r["hug"]);
    assert!(r["boxed"].h > r["hug"].h);
    assert!(r["narrow"].h > r["boxed"].h);
    assert!(r["big"].h > r["boxed"].h * 1.8);
    assert_eq!(r["boxed"].w, 600.0);
    // A hug text in a narrow row shrinks and wraps instead of overflowing.
    let d = deck(
        json!({"mode": "row", "padding": 60}),
        json!([{"id": "t", "kind": "text", "text": long}, boxed("side", 1200.0, 40.0)]),
    );
    let r = rects(&d);
    assert!(r["t"].w <= 1920.0 - 120.0 - 1200.0 + 0.5, "{:?}", r["t"]);
    assert!(r["t"].h > 56.7 * 2.0, "{:?}", r["t"]);
}

#[test]
fn typography_changes_the_box() {
    let node = |font: Value| {
        let d = deck(
            json!({"mode": "column", "cross": "start"}),
            json!([{"id": "t", "kind": "text", "text": "Typography", "font": font}]),
        );
        rects(&d)["t"]
    };
    let plain = node(json!({}));
    let spaced = node(json!({"letter_spacing": 0.2}));
    let upper = node(json!({"transform": "uppercase"}));
    let heavy = node(json!({"weight": 900}));
    let tall = node(json!({"line_height": 2.0}));
    assert!(
        spaced.w > plain.w + 10.0 * 42.0 * 0.15,
        "{spaced:?} {plain:?}"
    );
    assert!(upper.w > plain.w, "{upper:?} {plain:?}");
    assert!(heavy.w >= plain.w);
    assert!((tall.h - 84.0).abs() < 0.1, "{tall:?}");
}

#[test]
fn lists_headings_and_paragraphs_add_up() {
    let d = deck(
        json!({"mode": "column", "cross": "stretch"}),
        json!([
            {"id": "p", "kind": "text", "text": "one\ntwo"},
            {"id": "h", "kind": "text", "text": "# Head"},
            {"id": "l", "kind": "text", "text": "- one\n- two"},
            {"id": "tight", "kind": "text", "text": "one\ntwo", "font": {"line_height": 1.0}}
        ]),
    );
    let r = rects(&d);
    // two paragraphs: 2 lines of 56.7 and a 0.3 em gap between them, none after.
    assert!((r["p"].h - (2.0 * 56.7 + 12.6)).abs() < 0.2, "{:?}", r["p"]);
    assert!((r["h"].h - 42.0 * 1.4 * 1.1).abs() < 0.2, "{:?}", r["h"]);
    assert!((r["l"].h - 2.0 * (56.7 + 42.0)).abs() < 0.2, "{:?}", r["l"]);
    assert!(
        (r["tight"].h - 84.0).abs() < 0.2,
        "tight text has no paragraph gap: {:?}",
        r["tight"]
    );
}

#[test]
fn tables_code_and_charts_have_natural_sizes() {
    let d = deck(
        json!({"mode": "column", "cross": "start"}),
        json!([
            {"id": "tb", "kind": "table", "rows": [["Name", "Value"], ["alpha", "1"]]},
            {"id": "cd", "kind": "code", "source": "let x = 1;\nlet y = 2;"},
            {"id": "ch", "kind": "chart", "chart": "bar", "labels": ["a"], "series": [{"name": "s", "values": [1]}], "sizing": {"w": 800}},
            {"id": "im", "kind": "image", "src": "assets/none.png", "sizing": {"h": 180}}
        ]),
    );
    let r = rects(&d);
    assert!(r["tb"].w > 100.0 && r["tb"].h > 2.0 * 42.0, "{:?}", r["tb"]);
    assert!(r["cd"].h > 2.0 * 36.0 * 1.35, "{:?}", r["cd"]);
    assert_eq!(
        (r["ch"].w, r["ch"].h),
        (800.0, 450.0),
        "16:9 follows the width"
    );
    assert_eq!(
        (r["im"].w, r["im"].h),
        (320.0, 180.0),
        "a missing image is 16:9"
    );
}

#[test]
fn layout_lists_every_node_in_tree_order() {
    let d = Deck::new_from_layouts("Order");
    for slide in &d.slides {
        let listed: Vec<String> = layout(&d, slide).into_iter().map(|(id, _)| id).collect();
        let tree: Vec<String> = slide.nodes().into_iter().map(|n| n.id.clone()).collect();
        assert_eq!(listed, tree);
    }
}

// -- parity with the browser -----------------------------------------------------

/// `crates/studio/ui-dev/fixtures`: `parity-<name>.deck.json` laid out by taffy
/// against `rects-<name>.json`, the same decks laid out by Chromium with the
/// CSS mapping in this module's header (`node crates/studio/ui-dev/rects.mjs`).
fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../studio/ui-dev/fixtures")
}

/// Tolerance in px: frames and fixed boxes, then boxes sized by their text.
const FRAME_TOLERANCE: f64 = 2.0;
const TEXT_TOLERANCE: f64 = 6.0;

fn depends_on_text(node: &crate::deck::Node) -> bool {
    use crate::deck::{Dim, Kind};
    matches!(
        node.kind,
        Kind::Text { .. } | Kind::Table { .. } | Kind::Code { .. }
    ) || node.sizing.w == Dim::Hug
        || node.sizing.h == Dim::Hug
        || node.children().iter().any(depends_on_text)
}

/// Compare every `parity-<name>.deck.json` with `rects-<name>.json`. Returns
/// the largest delta seen for boxes that do not depend on text and for those
/// that do. A deck whose rects are absent is only validated; text decks are
/// skipped when the machine lacks the font the rects were made with.
fn compare_parity(dir: &std::path::Path) -> Vec<(String, f64, f64)> {
    let mut report = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("skipped: {} is not present", dir.display());
        return report;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_prefix("parity-")?
                .strip_suffix(".deck.json")
                .map(str::to_string)
        })
        .collect();
    names.sort();
    for name in names {
        let source = std::fs::read_to_string(dir.join(format!("parity-{name}.deck.json"))).unwrap();
        let deck = Deck::check(&source).unwrap_or_else(|e| panic!("parity-{name}: {e:?}"));
        let rects_file = dir.join(format!("rects-{name}.json"));
        let Ok(expected) = std::fs::read_to_string(&rects_file) else {
            eprintln!(
                "skipped parity-{name}: {} is absent. TODO(editor half): generate it with \
                 `node crates/studio/ui-dev/rects.mjs`",
                rects_file.display()
            );
            continue;
        };
        let expected: Value = serde_json::from_str(&expected).unwrap();
        let font = expected["font"].as_str().unwrap_or_default();
        let has_font = font.is_empty()
            || crate::deck::text::installed_families()
                .iter()
                .any(|f| f == font);
        let (mut frames, mut texts) = (0.0_f64, 0.0_f64);
        for slide in &deck.slides {
            let ours = layout_map(&deck, slide, None);
            // Without the font, text boxes take other sizes and move the
            // boxes laid out after them: only fixed sizes still compare.
            let shifted = !has_font && slide.nodes().into_iter().any(depends_on_text);
            let theirs = expected["slides"][&slide.id]
                .as_object()
                .unwrap_or_else(|| panic!("rects-{name}.json has no slide {}", slide.id));
            for node in slide.nodes() {
                if node.hidden {
                    continue;
                }
                let text_sized = depends_on_text(node);
                if text_sized && !has_font {
                    eprintln!("skipped {name}/{}: {font} is not installed", node.id);
                    continue;
                }
                let want: Vec<f64> = theirs
                    .get(&node.id)
                    .and_then(Value::as_array)
                    .unwrap_or_else(|| panic!("rects-{name}.json lacks {}", node.id))
                    .iter()
                    .map(|v| v.as_f64().unwrap())
                    .collect();
                let got = ours[&node.id];
                let position = if shifted {
                    [0.0, 0.0]
                } else {
                    [got.x - want[0], got.y - want[1]]
                };
                let delta = [position[0], position[1], got.w - want[2], got.h - want[3]]
                    .iter()
                    .fold(0.0_f64, |m, d| m.max(d.abs()));
                let (limit, seen) = if text_sized {
                    (TEXT_TOLERANCE, &mut texts)
                } else {
                    (FRAME_TOLERANCE, &mut frames)
                };
                *seen = seen.max(delta);
                assert!(
                    delta <= limit,
                    "{name}/{}/{}: taffy ({}, {}, {}, {}) vs browser {want:?}, off by {delta} (limit {limit})",
                    slide.id,
                    node.id,
                    got.x,
                    got.y,
                    got.w,
                    got.h
                );
            }
        }
        report.push((name, frames, texts));
    }
    report
}

#[test]
fn taffy_matches_the_browser_rects_for_the_parity_decks() {
    for (name, frames, texts) in compare_parity(&fixtures()) {
        eprintln!(
            "parity {name}: max delta {frames:.2} px (frames), {texts:.2} px (text-sized boxes)"
        );
    }
}
