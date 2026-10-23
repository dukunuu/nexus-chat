//! The deck tool's first-time-success behaviors: direct edits, reads that fit,
//! stale revisions, lenient input, history and images. Synthetic decks only.

use std::io::Cursor;

use super::tests::Fixture;
use super::*;

const PATH: &str = "decks/synthetic.deck.json";

fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(16, 9))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

async fn two_nodes(f: &Fixture) {
    let created = f
        .run(
            "create",
            json!({"title": "Synthetic", "slides": [{"id": "s1", "layout": {"mode": "column", "gap": 20, "padding": 100, "cross": "stretch"}, "children": [
                {"id": "a", "kind": "text", "text": "Alpha"},
                {"id": "b", "kind": "text", "text": "Beta"}
            ]}]}),
        )
        .await;
    assert!(created.starts_with("created deck"), "{created}");
}

fn revision_of(result: &str) -> String {
    result
        .split("revision ")
        .nth(1)
        .and_then(|rest| rest.split([' ', '\n']).next())
        .unwrap()
        .to_string()
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

// -- direct edits are not suggestions -------------------------------------------

#[tokio::test]
async fn a_direct_edit_takes_a_hundred_operations_and_a_new_image() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let mut ops: Vec<Value> = (0..99)
        .map(|i| json!({"op": "insert_node", "parent": "s1", "node": {"kind": "text", "text": format!("line {i}")}}))
        .collect();
    ops.push(json!({"op": "insert_node", "parent": "s1", "node": {"kind": "image", "src": "assets/not-in-the-deck-yet.png"}}));
    let edited = f.run("edit", json!({"path": PATH, "ops": ops})).await;
    assert!(
        edited.starts_with("edited slide 1"),
        "{}",
        &edited[..edited.len().min(600)]
    );
    assert!(
        edited.contains("100 operations: 100 insert_node"),
        "{edited}"
    );
    // Many operations still make a result that fits the tool-result cap.
    assert!(chars(&edited) < 8000, "{}", chars(&edited));
    assert!(edited.contains("more changed nodes"), "{edited}");
}

#[tokio::test]
async fn a_comment_suggestion_keeps_the_forty_operation_and_new_image_rules() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let many: Vec<Value> = (0..41)
        .map(|_| json!({"op": "set_notes", "slide": "s1", "notes": "x"}))
        .collect();
    let refused = f
        .run(
            "comment",
            json!({"path": PATH, "slide": "s1", "severity": "should", "text": "Too many.", "ops": many}),
        )
        .await;
    assert!(
        refused.starts_with("deck failed")
            && refused.contains("at most 40")
            && refused.contains("use edit"),
        "{refused}"
    );
    let image = f
        .run(
            "comment",
            json!({"path": PATH, "slide": "s1", "severity": "should", "text": "Add art.",
                "ops": [{"op": "insert_node", "parent": "s1", "node": {"kind": "image", "src": "assets/new.png"}}]}),
        )
        .await;
    assert!(image.contains("image src must already occur"), "{image}");
    assert!(feedback::load(&f.file(PATH)).unwrap().is_empty());
}

#[tokio::test]
async fn edit_results_carry_the_new_revision_and_the_changed_nodes_with_rects() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let before = std::fs::read_to_string(f.file(PATH)).unwrap();
    let old = Deck::check(&before).unwrap().revision();
    let edited = f
        .run(
            "edit",
            json!({"path": PATH, "ops": [{"op": "update_node", "id": "b", "fields": {"text": "Beta, longer"}}]}),
        )
        .await;
    let new = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap())
        .unwrap()
        .revision();
    assert!(
        edited.contains(&format!("revision {} (was {})", &new[..12], &old[..12])),
        "{edited}"
    );
    assert!(
        edited.contains("slide 1 (id s1)\n  b text 100,"),
        "{edited}"
    );
    assert!(edited.contains("\"Beta, longer\""), "{edited}");
    assert!(
        !edited.contains("\n  a text"),
        "unchanged nodes are not listed: {edited}"
    );
}

// -- read that fits ---------------------------------------------------------------

fn big_deck(slides: usize, nodes: usize) -> Value {
    json!((0..slides)
        .map(|s| json!({"id": format!("slide_{s}"), "layout": {"mode": "column", "gap": 8, "padding": 40}, "children":
            (0..nodes).map(|n| json!({"id": format!("node_{s}_{n}"), "kind": "text",
                "text": format!("Synthetic line {n} of slide {s} with some filler words to take space"), "font": {"size": 24}})).collect::<Vec<_>>()}))
        .collect::<Vec<_>>())
}

#[tokio::test]
async fn read_of_a_big_deck_is_an_outline_with_complete_ids_and_a_more_slides_line() {
    let f = Fixture::new();
    f.run(
        "create",
        json!({"title": "Big", "slides": big_deck(12, 14)}),
    )
    .await;
    let read = f.run("read", json!({"path": "decks/big.deck.json"})).await;
    assert!(chars(&read) <= 8000, "{}", chars(&read));
    assert!(
        read.contains("outline (the full tree is over 7000"),
        "{read}"
    );
    assert!(!read.contains("tool result truncated"), "{read}");
    // Node lines are `id kind x,y wxh "text"` and ids are never cut.
    assert!(read.contains("  node_0_0 text 40,40 "), "{read}");
    for line in read.lines().filter(|l| l.trim_start().starts_with("node_")) {
        let id = line.split_whitespace().next().unwrap();
        let (_, rest) = id.split_once('_').unwrap();
        let (s, n) = rest.split_once('_').unwrap();
        assert!(
            s.parse::<usize>().unwrap() < 12 && n.parse::<usize>().unwrap() < 14,
            "{line}"
        );
    }
    let more = read
        .lines()
        .find(|l| l.starts_with("… "))
        .expect("a more-slides line");
    assert!(
        more.contains("more slides") && more.contains("read with slides:["),
        "{more}"
    );
    // The slides it lists are really missing from the outline, the rest are there.
    let listed = more.split("slides:[").nth(1).unwrap();
    let first_missing = listed.trim_start_matches('"').split('"').next().unwrap();
    assert!(!read.contains(&format!("(id {first_missing})")), "{read}");
}

#[tokio::test]
async fn read_with_slides_returns_just_those_in_full_by_id_or_number() {
    let f = Fixture::new();
    f.run(
        "create",
        json!({"title": "Big", "slides": big_deck(12, 14)}),
    )
    .await;
    let read = f
        .run(
            "read",
            json!({"path": "decks/big.deck.json", "slides": ["slide_2", 5, "7"]}),
        )
        .await;
    assert!(
        !read.contains("outline (") && chars(&read) <= 8000,
        "{}",
        chars(&read)
    );
    assert!(read.contains("showing slides 3, 5, 7 of 12"), "{read}");
    for id in ["slide_2", "slide_4", "slide_6"] {
        assert!(read.contains(&format!("(id {id})")), "{id}: {read}");
    }
    assert!(
        !read.contains("(id slide_3)") && !read.contains("(id slide_0)"),
        "{read}"
    );
    // Full detail: font overrides and sizing, not just the outline.
    assert!(read.contains("size=24"), "{read}");
    let bad = f
        .run(
            "read",
            json!({"path": "decks/big.deck.json", "slides": ["nope"]}),
        )
        .await;
    assert!(
        bad.contains("unknown slide") && bad.contains("slide_0"),
        "{bad}"
    );
    let range = f
        .run(
            "read",
            json!({"path": "decks/big.deck.json", "slides": [99]}),
        )
        .await;
    assert!(range.contains("unknown slide"), "{range}");
}

#[tokio::test]
async fn a_small_deck_still_reads_in_full_and_the_field_reference_waits_for_room() {
    let f = Fixture::new();
    f.run(
        "create",
        json!({"title": "Big", "slides": big_deck(12, 14)}),
    )
    .await;
    // The create result had room for the reference; take it back to test the read path.
    f.tools.deck_link.lock().unwrap().schema_shown = false;
    let big = f.run("read", json!({"path": "decks/big.deck.json"})).await;
    assert!(
        !big.contains("schema (sent once"),
        "no room beside a big read: {}",
        chars(&big)
    );
    assert!(!f.tools.deck_link.lock().unwrap().schema_shown);
    two_nodes(&f).await;
    f.tools.deck_link.lock().unwrap().schema_shown = false;
    let small = f.run("read", json!({"path": PATH})).await;
    assert!(
        !small.contains("outline (the full tree") && small.contains("a text [100,"),
        "{small}"
    );
    assert!(small.contains("schema (sent once"), "{small}");
    assert!(chars(&small) <= 8000, "{}", chars(&small));
}

// -- stale revisions ----------------------------------------------------------------

#[tokio::test]
async fn a_stale_revision_that_touches_nothing_changed_is_rebased() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let read = f.run("read", json!({"path": PATH})).await;
    let seen = revision_of(&read);
    // Someone else (studio, or an earlier call) changes node a.
    f.run("edit", json!({"path": PATH, "ops": [{"op": "update_node", "id": "a", "fields": {"text": "Alpha 2"}}]})).await;
    let rebased = f
        .run(
            "edit",
            json!({"path": PATH, "revision": seen, "ops": [{"op": "update_node", "id": "b", "fields": {"text": "Beta 2"}}]}),
        )
        .await;
    assert!(rebased.starts_with("edited slide 1"), "{rebased}");
    assert!(
        rebased.contains("rebased: the deck changed since revision") && rebased.contains("(a)"),
        "{rebased}"
    );
    let deck = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
    assert_eq!(deck.node("a").unwrap().text(), Some("Alpha 2"));
    assert_eq!(deck.node("b").unwrap().text(), Some("Beta 2"));
}

#[tokio::test]
async fn a_stale_revision_that_overlaps_is_refused_with_the_changed_nodes() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let seen = revision_of(&f.run("read", json!({"path": PATH})).await);
    let changed = f
        .run("edit", json!({"path": PATH, "ops": [{"op": "update_node", "id": "a", "fields": {"text": "Alpha 2"}}]}))
        .await;
    let current = revision_of(&changed);
    let before = std::fs::read(f.file(PATH)).unwrap();
    let refused = f
        .run(
            "edit",
            json!({"path": PATH, "revision": seen, "ops": [{"op": "update_node", "id": "a", "fields": {"text": "Mine"}}]}),
        )
        .await;
    assert!(
        refused.starts_with("deck failed: stale revision"),
        "{refused}"
    );
    assert!(
        refused.contains("- a: now: a text") && refused.contains("\"Alpha 2\""),
        "{refused}"
    );
    assert!(
        refused.contains(&format!("revision {current}")) && refused.contains("Nothing was written"),
        "{refused}"
    );
    assert_eq!(std::fs::read(f.file(PATH)).unwrap(), before);
    // Deleting what changed is an overlap too, and the unchanged revision still passes.
    let current_full = Deck::check(&String::from_utf8(before).unwrap())
        .unwrap()
        .revision();
    let ok = f
        .run("edit", json!({"path": PATH, "revision": current_full, "ops": [{"op": "set_notes", "slide": "s1", "notes": "n"}]}))
        .await;
    assert!(
        ok.starts_with("edited slide 1") && !ok.contains("rebased"),
        "{ok}"
    );
}

// -- lenient input -------------------------------------------------------------------

#[tokio::test]
async fn create_repairs_the_usual_mistakes_and_says_so() {
    let f = Fixture::new();
    let created = f
        .run(
            "create",
            json!({"title": "Lenient", "path": "talk", "slides": [{"id": "s1", "children": [
                {"id": "t", "kind": "text", "text": "Hello", "font": "Avenir", "style": "eyebrow"},
                {"id": "dot", "kind": "ellipse", "sizing": {"w": "120", "h": 120.0}},
                {"id": "sp", "kind": "text", "text": "Wide", "font": {"size": 40, "letter_spacing": 4, "weight": "700"}}
            ]}]}),
        )
        .await;
    assert!(
        created.starts_with("created deck · decks/talk.deck.json"),
        "{created}"
    );
    for note in [
        "normalized: path \"talk\" → decks/talk.deck.json",
        "normalized: font \"Avenir\"",
        "text style \"eyebrow\" on t is not defined → body",
        "normalized: kind \"ellipse\" → shape",
        "letter_spacing 4 is em, not px → 0.1",
    ] {
        assert!(created.contains(note), "{note}\n{created}");
    }
    let deck =
        Deck::check(&std::fs::read_to_string(f.file("decks/talk.deck.json")).unwrap()).unwrap();
    assert!(matches!(
        &deck.node("dot").unwrap().kind,
        Kind::Shape { .. }
    ));
    let Kind::Text { font, style, .. } = &deck.node("t").unwrap().kind else {
        panic!("text")
    };
    assert_eq!(font.family.as_deref(), Some("Avenir"));
    assert_eq!(style, "body");
    // Every short path form lands on the same file.
    for form in ["talk.deck.json", "decks/talk"] {
        let read = f.run("read", json!({"path": form})).await;
        assert!(read.contains("decks/talk.deck.json"), "{form}: {read}");
    }
}

#[tokio::test]
async fn create_defines_a_style_the_call_supplies() {
    let f = Fixture::new();
    let created = f
        .run(
            "create",
            json!({"title": "Styled", "text_styles": {"eyebrow": {"size": 24, "weight": 600, "letter_spacing": 0.12}},
                "slides": [{"id": "s1", "children": [{"id": "k", "kind": "text", "text": "KICKER", "style": "eyebrow"}]}]}),
        )
        .await;
    assert!(created.starts_with("created deck"), "{created}");
    let deck =
        Deck::check(&std::fs::read_to_string(f.file("decks/styled.deck.json")).unwrap()).unwrap();
    let Kind::Text { style, .. } = &deck.node("k").unwrap().kind else {
        panic!("text")
    };
    assert_eq!(style, "eyebrow");
    assert_eq!(deck.theme.text_styles["eyebrow"].size, Some(24.0));
}

#[tokio::test]
async fn edit_repairs_ops_and_children_patches_replace_children() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let edited = f
        .run(
            "edit",
            json!({"path": "synthetic", "ops": [
                {"op": "update_node", "id": "a", "fields": {"font": {"size": "64", "weight": 700.0}, "style": "nope"}},
                {"op": "insert_node", "parent": "s1", "node": {"id": "fr", "kind": "frame", "layout": {"mode": "row", "gap": "16"}, "children": [
                    {"id": "c1", "kind": "text", "text": "one"}]}},
                {"op": "update_node", "id": "fr", "fields": {"children": [{"kind": "text", "text": "two"}, {"kind": "circle"}]}}
            ]}),
        )
        .await;
    assert!(edited.starts_with("edited slide 1"), "{edited}");
    assert!(
        edited.contains("normalized: path \"synthetic\""),
        "{edited}"
    );
    assert!(
        edited.contains("children of fr replaced: 1 removed, 2 inserted"),
        "{edited}"
    );
    let deck = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
    assert_eq!(deck.node("fr").unwrap().children().len(), 2);
    assert!(deck.node("c1").is_none());
    let Kind::Text { font, style, .. } = &deck.node("a").unwrap().kind else {
        panic!("text")
    };
    assert_eq!(
        (font.size, font.weight, style.as_str()),
        (Some(64.0), Some(700), "body")
    );
}

#[tokio::test]
async fn invalid_input_reports_every_located_error_with_a_fix_hint() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let before = std::fs::read(f.file(PATH)).unwrap();
    let rejected = f
        .run(
            "edit",
            json!({"path": PATH, "ops": [
                {"op": "update_node", "id": "ghost", "fields": {"text": "x"}},
                {"op": "update_node", "id": "a", "fields": {"font": {"letter_spacing": 1.9, "color": "not-a-color"}}},
                {"op": "insert_node", "parent": "s1", "node": {"kind": "text", "text": "x", "colour": "red"}},
                {"op": "teleport"}
            ]}),
        )
        .await;
    assert!(
        rejected.starts_with("deck failed: Nothing was written. "),
        "{rejected}"
    );
    assert!(
        rejected.contains("- ops[0].id: no node \"ghost\""),
        "{rejected}"
    );
    assert!(
        rejected.contains("- ops[2]:") && rejected.contains("fix: remove that field"),
        "{rejected}"
    );
    assert!(rejected.contains("- ops[3]:"), "{rejected}");
    assert!(
        rejected.starts_with("deck failed: Nothing was written. 3 problems to fix:"),
        "{rejected}"
    );
    assert_eq!(std::fs::read(f.file(PATH)).unwrap(), before);

    // Whole-deck problems are located by deck path and carry hints too.
    let located = f
        .run(
            "edit",
            json!({"path": PATH, "ops": [{"op": "update_node", "id": "a", "fields": {"font": {"color": "not-a-color", "size": 5000}}}]}),
        )
        .await;
    assert!(
        located.contains("$.slides[0].children[0].font.color")
            && located.contains("fix: use a theme role"),
        "{located}"
    );
    assert!(located.contains("font.size"), "{located}");
}

// -- history ---------------------------------------------------------------------------

#[tokio::test]
async fn history_lists_versions_and_restoring_is_a_new_write() {
    let f = Fixture::new();
    two_nodes(&f).await;
    let original = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
    let empty = f.run("history", json!({"path": PATH})).await;
    assert!(
        empty.contains("no earlier versions are kept yet"),
        "{empty}"
    );
    f.run("edit", json!({"path": PATH, "ops": [{"op": "update_node", "id": "a", "fields": {"text": "Changed"}}]})).await;
    f.run("edit", json!({"path": PATH, "ops": [{"op": "add_slide", "slide": {"children": [{"kind": "text", "text": "More"}]}}]})).await;
    let listed = f.run("history", json!({"path": PATH})).await;
    assert!(listed.contains("2 earlier version(s)"), "{listed}");
    assert!(
        listed.contains("replaced by model") && listed.contains("then: slide 1 changed"),
        "{listed}"
    );
    assert!(listed.contains("then: 1 slide added"), "{listed}");
    let rev8 = &original.revision()[..8];
    assert!(listed.contains(rev8), "{listed}");

    let restored = f
        .run("history", json!({"path": PATH, "revision": rev8}))
        .await;
    assert!(
        restored.starts_with(&format!("restored {PATH} to version {rev8}")),
        "{restored}"
    );
    assert!(restored.contains("new write"), "{restored}");
    let now = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
    assert_eq!(now, original);
    // The version the restore replaced is kept, so the restore can be undone.
    let after = f.run("history", json!({"path": PATH})).await;
    assert!(after.contains("3 earlier version(s)"), "{after}");
    let again = f
        .run("history", json!({"path": PATH, "revision": rev8}))
        .await;
    assert!(again.contains("already at that version"), "{again}");
    let missing = f
        .run("history", json!({"path": PATH, "revision": "ffffffff"}))
        .await;
    assert!(
        missing.starts_with("deck failed") && missing.contains("no kept version"),
        "{missing}"
    );
}

// -- images ----------------------------------------------------------------------------

#[tokio::test]
async fn a_space_file_becomes_a_deck_asset_and_the_src_is_rewritten() {
    let f = Fixture::new();
    two_nodes(&f).await;
    std::fs::write(f.file("us_1.jpg"), png()).unwrap();
    for src in ["us_1.jpg", "files/us_1.jpg"] {
        let edited = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "insert_node", "parent": "s1", "node": {"kind": "image", "src": src, "sizing": {"w": 320, "h": 180}}}]}),
            )
            .await;
        assert!(edited.starts_with("edited slide 1"), "{edited}");
        assert!(
            edited.contains("normalized: image \"")
                && edited.contains("(copied from the space files)"),
            "{edited}"
        );
        assert!(!edited.contains("placeholder:"), "{edited}");
    }
    let deck = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
    let srcs: Vec<String> = deck
        .slides
        .iter()
        .flat_map(Slide::nodes)
        .filter_map(|n| match &n.kind {
            Kind::Image { src, .. } => Some(src.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(srcs.len(), 2);
    assert_eq!(srcs[0], srcs[1], "same content, same asset");
    assert!(
        srcs[0].starts_with("assets/") && srcs[0].ends_with("-us_1.png"),
        "{srcs:?}"
    );
    assert!(f.file("decks").join(&srcs[0]).is_file());
}

#[tokio::test]
async fn a_space_file_that_is_not_an_image_is_refused() {
    let f = Fixture::new();
    two_nodes(&f).await;
    std::fs::write(f.file("table.csv"), "a,b\n1,2\n").unwrap();
    let before = std::fs::read(f.file(PATH)).unwrap();
    let refused = f
        .run(
            "edit",
            json!({"path": PATH, "ops": [{"op": "insert_node", "parent": "s1", "node": {"kind": "image", "src": "table.csv"}}]}),
        )
        .await;
    assert!(
        refused.starts_with("deck failed") && refused.contains("is not a PNG, JPEG or WebP image"),
        "{refused}"
    );
    assert_eq!(std::fs::read(f.file(PATH)).unwrap(), before);
}

#[tokio::test]
async fn https_images_are_fetched_through_the_injected_fetcher_and_failures_are_per_node() {
    let mut f = Fixture::new();
    let assets = f.file("decks/assets");
    let fetcher = Fetcher::for_test(&assets);
    // Seeded: pretends the download finished, nothing touches the network.
    fetcher
        .seed("https://images.example/ok.png", &png())
        .unwrap();
    fetcher.seed_failure("https://images.example/missing.png");
    f.tools.deck_images = Some(fetcher);
    let created = f
        .run(
            "create",
            json!({"title": "Remote", "slides": [{"id": "s1", "children": [
                {"id": "good", "kind": "image", "src": "https://images.example/ok.png", "sizing": {"w": 320, "h": 180}},
                {"id": "b3", "kind": "image", "src": "https://images.example/missing.png", "sizing": {"w": 320, "h": 180}},
                {"id": "gone", "kind": "image", "src": "assets/never-added.png", "sizing": {"w": 320, "h": 180}}
            ]}]}),
        )
        .await;
    assert!(created.starts_with("created deck"), "{created}");
    assert!(created.contains("image b3: image unavailable"), "{created}");
    assert!(!created.contains("image good:"), "{created}");
    // Placeholders are listed with the reason, whether the fetch failed or the file is missing.
    assert!(
        created.contains("placeholder: image b3 on slide 1"),
        "{created}"
    );
    assert!(
        created.contains("placeholder: image gone on slide 1") && created.contains("no such file"),
        "{created}"
    );
    assert!(!created.contains("placeholder: image good"), "{created}");
}

#[tokio::test]
async fn remote_images_that_are_switched_off_are_reported_not_fetched() {
    let mut f = Fixture::new();
    let off = Fetcher::new(f.root.join("device-cache"));
    off.set_enabled(false);
    f.tools.remote_images = Some(off);
    let created = f
        .run(
            "create",
            json!({"title": "Off", "slides": [{"id": "s1", "children": [
                {"id": "pic", "kind": "image", "src": "https://images.example/x.png", "sizing": {"w": 100, "h": 100}}]}]}),
        )
        .await;
    assert!(created.contains("remote images are off"), "{created}");
}

// -- confinement of the new write paths --------------------------------------------------

#[tokio::test]
async fn history_lives_beside_the_deck_and_is_not_a_deck_path() {
    let f = Fixture::new();
    two_nodes(&f).await;
    f.run(
        "edit",
        json!({"path": PATH, "ops": [{"op": "set_notes", "slide": "s1", "notes": "n"}]}),
    )
    .await;
    assert!(f.file("decks/.history/synthetic").is_dir());
    let hidden = f
        .run(
            "read",
            json!({"path": "decks/.history/synthetic/x.deck.json"}),
        )
        .await;
    assert!(hidden.contains("hidden folders"), "{hidden}");
}
