use super::*;

const SOURCE: &str = include_str!("../../../../core/tests/fixtures/interactive/counter.json");
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
        assert_eq!(p.height, 13);
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
