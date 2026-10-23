use serde_json::{Value, json};

use super::*;

fn deck() -> Deck {
    Deck::check(
        &json!({"version": 2, "title": "Synthetic", "slides": [
            {"id": "s1", "layout": {"mode": "column", "gap": 10}, "children": [
                {"id": "a", "kind": "text", "text": "A"},
                {"id": "row", "kind": "frame", "layout": {"mode": "row"}, "children": [
                    {"id": "r1", "kind": "text", "text": "one"},
                    {"id": "r2", "kind": "text", "text": "two"}
                ]}
            ]}
        ]})
        .to_string(),
    )
    .unwrap()
}

fn insert(i: usize, node: Value) -> Value {
    json!({"op": "insert_node", "parent": "s1", "index": i, "node": node})
}

#[test]
fn a_hundred_operations_with_a_new_image_apply_directly() {
    let mut ops: Vec<Value> = (0..99)
        .map(|i| insert(i, json!({"kind": "text", "text": format!("line {i}")})))
        .collect();
    ops.push(insert(
        0,
        json!({"kind": "image", "src": "assets/never-seen-before.png"}),
    ));
    let prepared = prepare(&deck(), ops).unwrap();
    assert_eq!(prepared.ops.len(), 100);
    assert!(prepared.next.slides[0].nodes().len() >= 100);
    // Generated ids are unique and the deck passed the whole-deck check.
    let ids: std::collections::BTreeSet<_> = prepared
        .next
        .node_ids()
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert_eq!(ids.len(), prepared.next.node_ids().len());
}

#[test]
fn the_direct_limit_is_five_hundred_operations() {
    let ops = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|_| json!({"op": "set_notes", "slide": "s1", "notes": "x"}))
            .collect()
    };
    assert!(prepare(&deck(), ops(500)).is_ok());
    let errors = prepare(&deck(), ops(501)).err().unwrap();
    assert!(errors[0].message.contains("1 to 500"), "{errors:?}");
    assert!(prepare(&deck(), vec![]).is_err());
}

#[test]
fn suggestions_keep_their_stricter_rules() {
    let many: Vec<Value> = (0..41)
        .map(|_| json!({"op": "set_notes", "slide": "s1", "notes": "x"}))
        .collect();
    let (valid, errors) = crate::deck::suggest::prepare(
        &deck(),
        vec![crate::deck::suggest::RawSuggestion {
            why: "w".into(),
            ops: many,
        }],
    );
    assert!(
        valid.is_empty() && errors[0].message.contains("at most 40"),
        "{errors:?}"
    );
    let image = vec![insert(0, json!({"kind": "image", "src": "assets/new.png"}))];
    let (valid, errors) = crate::deck::suggest::prepare(
        &deck(),
        vec![crate::deck::suggest::RawSuggestion {
            why: "w".into(),
            ops: image,
        }],
    );
    assert!(
        valid.is_empty() && errors[0].message.contains("image src must already occur"),
        "{errors:?}"
    );
}

#[test]
fn every_located_error_is_reported() {
    let errors = prepare(
        &deck(),
        vec![
            json!({"op": "update_node", "id": "ghost", "fields": {"text": "x"}}),
            json!({"op": "launch_missiles"}),
            json!({"op": "set_notes", "slide": "s1", "notes": "fine"}),
            json!({"op": "delete_node", "id": "also-missing"}),
        ],
    )
    .err()
    .unwrap();
    let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["ops[0].id", "ops[1]", "ops[3]"], "{errors:?}");
}

#[test]
fn a_node_can_be_updated_after_it_is_inserted_in_the_same_call() {
    let prepared = prepare(
        &deck(),
        vec![
            insert(0, json!({"id": "fresh", "kind": "text", "text": "A"})),
            json!({"op": "update_node", "id": "fresh", "fields": {"text": "B"}}),
            json!({"op": "update_node", "id": "fresh", "fields": {"font": {"size": 50}}}),
        ],
    )
    .unwrap();
    assert_eq!(prepared.next.node("fresh").unwrap().text(), Some("B"));
}

#[test]
fn a_deleted_id_can_be_reused_by_a_later_insert() {
    let prepared = prepare(
        &deck(),
        vec![
            json!({"op": "delete_node", "id": "a"}),
            insert(0, json!({"id": "a", "kind": "text", "text": "again"})),
        ],
    )
    .unwrap();
    assert_eq!(prepared.next.node("a").unwrap().text(), Some("again"));
}

#[test]
fn children_in_a_patch_replace_the_children() {
    let prepared = prepare(
        &deck(),
        vec![json!({"op": "update_node", "id": "row", "fields": {
            "layout": {"gap": 40},
            "children": [{"id": "r1", "kind": "text", "text": "kept id"}, {"kind": "text", "text": "new"}]
        }})],
    )
    .unwrap();
    let row = prepared.next.node("row").unwrap();
    assert_eq!(row.children().len(), 2);
    assert_eq!(row.children()[0].text(), Some("kept id"));
    assert!(prepared.next.node("r2").is_none());
    assert!(
        prepared.notes[0].contains("children of row replaced"),
        "{:?}",
        prepared.notes
    );

    let slide = prepare(
        &deck(),
        vec![json!({"op": "update_slide", "id": "s1", "fields": {"children": [{"kind": "text", "text": "only"}]}})],
    )
    .unwrap();
    assert_eq!(slide.next.slides[0].children.len(), 1);

    let error = prepare(
        &deck(),
        vec![json!({"op": "update_node", "id": "a", "fields": {"children": []}})],
    )
    .err()
    .unwrap();
    assert!(
        error[0].message.contains("only a frame has children"),
        "{error:?}"
    );
}
