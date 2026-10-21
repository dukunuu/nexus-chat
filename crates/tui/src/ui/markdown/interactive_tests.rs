use super::*;

const SOURCE: &str = include_str!("../../../../core/tests/fixtures/interactive/counter.json");
#[test]
fn fallback_shows_only_the_first_three_located_errors() {
    let source = r#"{"version":1,"title":"Broken","view":[{"type":"radio"},{"type":"slider"},{"type":"canvas"},{"type":"tabs"}]}"#;
    let rendered = render(&format!("```nexus-ui\n{source}\n```"), 240, colors());
    assert!(rendered.widgets.is_empty());
    let text = plain(&rendered);
    for index in 0..3 {
        assert!(
            text.contains(&format!("view[{index}]: unknown node type")),
            "{text}"
        );
    }
    assert!(!text.contains("view[3]:"), "{text}");
    assert!(text.contains("valid: hero, columns"), "{text}");
    assert_eq!(rendered.blocks, vec![source]);
}

fn colors() -> MdColors {
    MdColors::default()
}
fn plain(r: &Rendered) -> String {
    r.lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ui_fences_keep_order_source_and_copy_metadata_without_a_json_dump() {
    for fence in ["```nexus-ui", "~~~NeXuS-Ui"] {
        let end: String = fence.chars().take(3).collect();
        let r = render(
            &format!("Before\n\n{fence}\n{}\n{end}\n\nAfter", SOURCE.trim()),
            72,
            colors(),
        );
        assert_eq!(r.widgets.len(), 1);
        let p = &r.widgets[0];
        assert!(p.complete);
        assert_eq!(p.height, 11);
        assert_eq!(p.source, SOURCE.trim());
        assert_eq!(r.lines.len(), r.code.len());
        let block = r.code[p.row].unwrap();
        assert_eq!(r.blocks[block], SOURCE.trim());
        assert!(
            r.code[p.row..p.row + usize::from(p.height)]
                .iter()
                .all(|id| *id == Some(block))
        );
        let text = plain(&r);
        assert!(text.find("Before").unwrap() < text.find("interactive output").unwrap());
        assert!(text.find("interactive output").unwrap() < text.find("After").unwrap());
        assert!(!text.contains("\"version\""));
    }
}
#[test]
fn closing_a_complete_document_does_not_change_transcript_geometry() {
    let open = format!("```nexus-ui\n{}\n", SOURCE.trim());
    let before = render(&open, 72, colors());
    let after = render(&format!("{open}```"), 72, colors());
    assert!(!before.widgets[0].complete);
    assert!(after.widgets[0].complete);
    assert_eq!(before.widgets[0].height, after.widgets[0].height);
    assert_eq!(before.lines.len(), after.lines.len());
    assert_eq!(before.code, after.code);
}
#[test]
fn literal_fences_do_not_activate_and_bad_definitions_keep_safe_source() {
    let literal = format!("````markdown\n```nexus-ui\n{SOURCE}```\n````");
    assert!(render(&literal, 72, colors()).widgets.is_empty());
    let source = "{\"version\":99,\"bad\":\"\u{1b}[31m\"}";
    let r = render(&format!("```nexus-ui\n{source}\n```"), 72, colors());
    assert_eq!(r.blocks, vec![source]);
    assert!(plain(&r).contains("version"));
    assert!(!plain(&r).contains('\u{1b}'));
    let narrow = render(&format!("```nexus-ui\n{}\n```", SOURCE.trim()), 6, colors());
    assert!(narrow.widgets.is_empty());
    assert_eq!(narrow.blocks, vec![SOURCE.trim()]);
}

#[test]
fn a_broken_block_shows_its_reason_and_readable_source_without_placeholder_rows() {
    // Mismatched brackets, as a model wrote it.
    let broken = r#"{"version":1,"title":"Quiz","view":[{"type":"buttons","items":[{"label":"A","action":"a"}]],"actions":{"a":[{"op":"answer"}]}}"#;
    let r = render(&format!("```nexus-ui\n{broken}\n```"), 72, colors());
    let text = plain(&r);
    assert!(
        r.widgets.is_empty(),
        "no card is reserved for a broken block"
    );
    assert!(
        text.contains("interactive UI · couldn't be shown"),
        "{text}"
    );
    assert!(!text.contains("interactive output"), "{text}");
    // Quotes are shown as written, not as \" escapes.
    assert!(text.contains(r#"{"version":1"#), "{text}");
    assert!(!text.contains(r#"\""#), "{text}");
    // The reason line comes first inside the box.
    let reason = r.lines[1]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!reason.contains("version"), "{reason}");
}
