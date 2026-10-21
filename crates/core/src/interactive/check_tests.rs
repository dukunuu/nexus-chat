use super::*;

fn document(view: Value) -> Value {
    json!({"version":1,"title":"Quiz","state":{"count":0},"view":view,"actions":{"pick":[{"op":"answer"}]}})
}

#[test]
fn syntax_errors_keep_serde_line_and_column() {
    let error = &Document::check("{\n  \"view\": [}\n}").unwrap_err()[0];
    assert_eq!(error.path, "$");
    assert!(error.message.contains("line 2 column"), "{error:?}");
}

#[test]
fn hallucinations_report_each_location_and_valid_alternatives() {
    let mut value = document(json!([
        {"type":"radio"},
        {"type":"text","text":"Hi","bogus":true},
        {"type":"text","text":"Hi","tone":"purple"},
        {"type":"text","text":42},
        {"type":"buttons","items":[{"label":"A","action":"pick","primary":"yes"}]},
        {"type":"columns","items":[{"title":"A","items":[{"text":"B","mark":"maybe"}]}]}
    ]));
    value["actions"]["pick"] = json!([{"op":"answer","label":"x"},{"op":"invented"}]);
    let errors = Document::check(&value.to_string()).unwrap_err();
    for (path, words) in [
        (
            "view[0]",
            &["unknown node type \"radio\"", "valid:", "select"][..],
        ),
        (
            "view[1]",
            &["unknown field \"bogus\"", "text", "tone", "rows"],
        ),
        ("view[2].tone", &["purple", "normal", "muted", "error"]),
        ("view[3].text", &["expected a string"]),
        ("view[4].items[0].primary", &["expected a boolean"]),
        (
            "view[5].items[0].items[0].mark",
            &["maybe", "ok", "no", "dot", "none"],
        ),
        (
            "actions.pick[1]",
            &[
                "unknown action op \"invented\"",
                "set",
                "call",
                "send",
                "answer",
            ],
        ),
    ] {
        let error = errors
            .iter()
            .find(|error| error.path == path)
            .unwrap_or_else(|| panic!("missing {path}: {errors:?}"));
        for word in words {
            assert!(error.message.contains(word), "{error:?} lacks {word}");
        }
    }
    assert_eq!(errors.len(), 7);
}

#[test]
fn top_level_parts_and_required_nested_fields_are_located() {
    let value = json!({"version":"one","title":false,"frame":"red","accent":"blue","state":[],"view":[{"type":"buttons","items":[{"action":"x"}]}],"actions":{"pick":"answer"}});
    let errors = Document::check(&value.to_string()).unwrap_err();
    for path in [
        "version",
        "title",
        "frame",
        "accent",
        "state",
        "view[0].items[0].label",
        "actions.pick",
    ] {
        assert!(errors.iter().any(|error| error.path == path), "{errors:?}");
    }
    let errors = Document::check(r#"{"view":[]}"#).unwrap_err();
    assert!(errors.iter().any(|error| error.path == "version"));
    assert!(errors.iter().any(|error| error.path == "title"));
}

#[test]
fn errors_are_bounded_and_semantic_failures_are_located() {
    let value = document(json!(
        (0..16).map(|_| json!({"type":"radio"})).collect::<Vec<_>>()
    ));
    let errors = Document::check(&value.to_string()).unwrap_err();
    assert_eq!(errors.len(), 10);
    assert_eq!(errors[9].path, "view[9]");
    let mut value = document(json!([{"type":"text","text":"{{missing}}"}]));
    let ui = Session::new(Document::check(&value.to_string()).unwrap()).unwrap();
    assert_eq!(ui.text("{{missing}}").unwrap(), "");
    value["state"]["count"] = json!([]);
    value["actions"]["pick"] = json!([{"op":"call","action":"missing"}]);
    let errors = Document::check(&value.to_string()).unwrap_err();
    for path in ["state.count", "actions.pick[0]"] {
        assert!(errors.iter().any(|error| error.path == path), "{errors:?}");
    }
    assert!(
        Document::check(&"x".repeat(MAX_SOURCE + 1)).unwrap_err()[0]
            .message
            .contains("16 KiB")
    );
}

#[test]
fn schema_errors_do_not_hide_independent_semantic_errors_or_shift_step_indices() {
    let mut value =
        document(json!([{"type":"radio"},{"type":"input","key":"missing","label":"Name"}]));
    value["state"]["count"] = json!([]);
    value["actions"]["pick"] =
        json!([{"op":"answer"},{"op":"answer"},{"op":"call","action":"missing"}]);
    let errors = Document::check(&value.to_string()).unwrap_err();
    for path in ["view[0]", "view[1]", "state.count", "actions.pick[2]"] {
        assert!(errors.iter().any(|error| error.path == path), "{errors:?}");
    }
}

#[test]
fn fixtures_and_all_prompt_examples_check_ok() {
    for source in [
        include_str!("../../tests/fixtures/interactive/counter.json"),
        include_str!("../../tests/fixtures/interactive/rps.json"),
    ] {
        let checked = Document::check(source).unwrap();
        let parsed = Document::parse(source).unwrap();
        assert_eq!(checked.title(), parsed.title());
        assert_eq!(checked.rows(), parsed.rows());
    }
    for prompt in [INSTRUCTIONS, LAYOUTS] {
        for example in prompt.split("```nexus-ui\n").skip(1) {
            Document::check(example.split("\n```").next().unwrap()).unwrap();
        }
    }
}

#[test]
fn parse_still_ignores_answer_labels_and_collapses_repeated_answers() {
    let mut value = document(json!([{"type":"buttons","items":[{"label":"A","action":"pick"}]}]));
    value["actions"]["pick"] = json!([{"op":"answer","label":{"ignored":true}},{"op":"answer"}]);
    let mut checked = Session::new(Document::check(&value.to_string()).unwrap()).unwrap();
    assert_eq!(checked.activate_button(0, 0).unwrap(), Some("A".into()));
    let mut session = Session::new(Document::parse(&value.to_string()).unwrap()).unwrap();
    assert_eq!(session.activate_button(0, 0).unwrap(), Some("A".into()));
    value["actions"]["pick"][0] = json!({"op":"answer"});
    let mut session = Session::new(Document::check(&value.to_string()).unwrap()).unwrap();
    assert_eq!(session.activate_button(0, 0).unwrap(), Some("A".into()));
}

#[test]
fn trailing_closers_are_forgiven_but_other_trailing_content_is_not() {
    let source = r#"{"version":1,"title":"Fine","view":[{"type":"text","text":"OK"}]}"#;
    for suffix in ["}", "]]", " \n } ]\t}"] {
        assert!(Document::check(&format!("{source}{suffix}")).is_ok());
        assert!(Document::parse(&format!("{source}{suffix}")).is_ok());
    }
    for suffix in [" extra", "{}", "} true", ",\"actions\":{}", "] garbage"] {
        assert!(Document::check(&format!("{source}{suffix}")).is_err());
        assert!(Document::parse(&format!("{source}{suffix}")).is_err());
    }
}

#[test]
fn table_rows_alias_renders_and_duplicate_spellings_are_rejected() {
    let mut value = document(json!([{"type":"table","columns":["Name"],"rows":[["Ada"]]}]));
    let checked = Document::check(&value.to_string()).unwrap();
    assert!(checked.plain_text().contains("Ada"));
    assert!(
        Document::parse(&value.to_string())
            .unwrap()
            .plain_text()
            .contains("Ada")
    );
    value["view"][0]["rows_data"] = json!([["Grace"]]);
    assert!(Document::check(&value.to_string()).is_err());
    assert!(Document::parse(&value.to_string()).is_err());
}

#[test]
fn undefined_templates_render_empty_and_existing_keys_still_expand() {
    let value = document(json!([
        {"type":"text","text":"at {{datetime}} count {{ count }}!"},
        {"type":"table","columns":["{{missing}}"],"rows":[["{{count}}"]]}
    ]));
    for doc in [
        Document::check(&value.to_string()).unwrap(),
        Document::parse(&value.to_string()).unwrap(),
    ] {
        let session = Session::new(doc).unwrap();
        assert_eq!(
            session.text("at {{datetime}} count {{ count }}!").unwrap(),
            "at  count 0!"
        );
        assert_eq!(session.text("{{missing}}").unwrap(), "");
    }
    let value = document(json!([{"type":"text","text":"{{unclosed"}]));
    assert!(Document::check(&value.to_string()).is_err());
}

#[test]
fn checking_preserves_answer_label_and_repeated_answer_leniency() {
    let mut value =
        document(json!([{"type":"buttons","items":[{"label":"Pick","action":"pick"}]}]));
    value["actions"]["pick"] = json!([{"op":"answer","label":"ignored"},{"op":"answer"}]);
    let mut session = Session::new(Document::check(&value.to_string()).unwrap()).unwrap();
    assert_eq!(session.activate_button(0, 0).unwrap(), Some("Pick".into()));
}
