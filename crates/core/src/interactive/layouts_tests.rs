use super::*;

fn document(view: Value) -> Value {
    json!({"version":1,"title":"Overview","view":view})
}

fn nodes() -> Vec<Value> {
    vec![
        json!({"type":"hero","title":"Hello {{name}}","subtitle":"Short summary","badge":"NEW"}),
        json!({"type":"columns","items":[{"title":"A","items":[{"text":"Yes","mark":"ok"},{"text":"No","mark":"no"}]},{"title":"B","items":[{"text":"Point","mark":"dot"},{"text":"Text","mark":"none"}]}]}),
        json!({"type":"cards","items":[{"title":"Card","text":"Body","badge":"Ready","tone":"success"}]}),
        json!({"type":"stats","items":[{"value":"12","label":"commits","delta":"+3","tone":"warning"}]}),
        json!({"type":"steps","items":[{"title":"First","text":"Shipped","state":"done"},{"title":"Now","state":"current"},{"title":"Later","state":"todo"}]}),
        json!({"type":"callout","tone":"tip","title":"Tip","text":"Keep it short"}),
        json!({"type":"quote","text":"Less is more","by":"Ada"}),
        json!({"type":"divider","label":"Details"}),
    ]
}

#[test]
fn all_layout_nodes_plain_text_and_templates() {
    let mut data = document(json!(nodes()));
    data["state"] = json!({"name":"Ada"});
    let doc = Document::parse(&data.to_string()).unwrap();
    assert!(doc.is_static());
    assert!(matches!(doc.frame(), Frame::Rounded));
    assert!(matches!(doc.accent(), Accent::Accent2));
    assert_eq!(
        doc.plain_text(),
        "Overview\nHello Ada\nShort summary\nNEW\nA\n✓ Yes\n✗ No\nB\n● Point\nText\nCard\nReady\nBody\ncommits: 12 (+3)\n● First\nShipped\n◉ Now\n○ Later\nTip\nKeep it short\n\"Less is more\"\n— Ada\nDetails"
    );
    assert_eq!(doc.rows(), 30);
}

#[test]
fn optional_look_fields_accept_only_theme_roles_and_frame_names() {
    let mut data = document(json!([{"type":"text","text":"Body"}]));
    for frame in ["rounded", "double", "heavy", "dashed", "plain"] {
        for accent in ["accent", "accent2", "success", "warning", "error", "muted"] {
            data["frame"] = json!(frame);
            data["accent"] = json!(accent);
            assert!(Document::parse(&data.to_string()).is_ok());
        }
    }
    for (key, value) in [
        ("frame", json!("thin")),
        ("accent", json!("#123456")),
        ("frame", json!(1)),
        ("accent", json!(null)),
    ] {
        let mut invalid = data.clone();
        invalid[key] = value;
        assert!(Document::parse(&invalid.to_string()).is_err());
    }
}

#[test]
fn every_new_text_field_rejects_escapes_unknown_templates_and_oversize() {
    fn reject_text_fields(value: &Value, path: &mut Vec<String>, root: &Value) {
        if let Value::Object(fields) = value {
            for (key, value) in fields {
                path.push(key.clone());
                if value.is_string() && !["type", "tone", "mark", "state"].contains(&key.as_str()) {
                    for text in [
                        "{{missing}}".to_string(),
                        "\u{1b}[31m".into(),
                        "x".repeat(MAX_TEXT + 1),
                    ] {
                        let mut invalid = root.clone();
                        let mut target = &mut invalid;
                        for part in path.iter() {
                            target = if let Ok(index) = part.parse::<usize>() {
                                &mut target[index]
                            } else {
                                &mut target[part]
                            };
                        }
                        *target = json!(text);
                        let result = Document::parse(&invalid.to_string());
                        if text == "{{missing}}" {
                            let ui = Session::new(result.unwrap()).unwrap();
                            assert_eq!(ui.text(&text).unwrap(), "", "{path:?}");
                        } else {
                            assert!(result.is_err(), "{path:?}");
                        }
                    }
                } else {
                    reject_text_fields(value, path, root);
                }
                path.pop();
            }
        } else if let Value::Array(items) = value {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                reject_text_fields(item, path, root);
                path.pop();
            }
        }
    }
    let mut data = document(json!(nodes()));
    data["state"] = json!({"name":"Ada"});
    reject_text_fields(&data["view"], &mut vec!["view".into()], &data);
}

#[test]
fn layout_limits_unknown_enums_and_unknown_fields_are_rejected() {
    for mut node in nodes() {
        if node["type"] == "hero" {
            node["title"] = json!("Hello");
        }
        let base = document(json!([node.clone()]));
        assert!(Document::parse(&base.to_string()).is_ok());
        node["unexpected"] = json!(true);
        assert!(Document::parse(&document(json!([node])).to_string()).is_err());
    }
    for (kind, item, minimum, maximum) in [
        ("columns", json!({"title":"A","items":[]}), 2, 3),
        ("cards", json!({"title":"A","text":"Body"}), 1, 6),
        ("stats", json!({"value":"1","label":"Count"}), 1, 6),
        ("steps", json!({"title":"A"}), 1, 8),
    ] {
        for count in [0, minimum, maximum, maximum + 1] {
            let data = document(json!([{"type":kind,"items":vec![item.clone();count]}]));
            assert_eq!(
                Document::parse(&data.to_string()).is_ok(),
                (minimum..=maximum).contains(&count),
                "{data}"
            );
        }
    }
    for node in [
        json!({"type":"columns","items":[{"title":"A","items":vec![json!({"text":"A"});9]},{"title":"B","items":[]}]}),
        json!({"type":"columns","items":[{"title":"A","items":[{"text":"A","mark":"bad"}]},{"title":"B","items":[]}]}),
        json!({"type":"cards","items":[{"title":"A","text":"B","tone":"pink"}]}),
        json!({"type":"stats","items":[{"value":"1","label":"A","tone":"pink"}]}),
        json!({"type":"steps","items":[{"title":"A","state":"waiting"}]}),
        json!({"type":"callout","tone":"error","text":"A"}),
    ] {
        assert!(Document::parse(&document(json!([node])).to_string()).is_err());
    }
    let column = json!({"title":"A","items":vec![json!({"text":"A"});8]});
    let large = json!({"type":"columns","items":vec![column;3]});
    assert!(Document::parse(&document(json!([large])).to_string()).is_ok());
    let card = json!({"type":"cards","items":vec![json!({"title":"A","text":"B"});6]});
    assert!(Document::parse(&document(json!([card, card])).to_string()).is_err());
}

#[test]
fn controls_or_actions_are_not_static_and_legacy_plain_text_is_readable() {
    let source = include_str!("../../tests/fixtures/interactive/counter.json");
    let doc = Document::parse(source).unwrap();
    assert!(!doc.is_static());
    assert!(doc.plain_text().contains("Count: 0"));
    let mut data = document(json!([{"type":"text","text":"A"}]));
    data["actions"] = json!({"noop":[]});
    assert!(!Document::parse(&data.to_string()).unwrap().is_static());
    data["actions"] = json!({});
    data["state"] = json!({"ready":true});
    data["view"] = json!([{"type":"toggle","key":"ready","label":"Ready"}]);
    assert!(!Document::parse(&data.to_string()).unwrap().is_static());
    for tone in ["info", "tip", "warning", "danger"] {
        assert!(
            Document::parse(
                &document(json!([{"type":"callout","tone":tone,"text":"Note"}])).to_string()
            )
            .is_ok()
        );
    }
    let example = LAYOUTS
        .split("```nexus-ui\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    assert!(Document::parse(example).unwrap().is_static());
}
