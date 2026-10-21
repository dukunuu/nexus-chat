use super::*;

fn quiz() -> Value {
    json!({
        "version": 1, "title": "Capital?",
        "state": {"city": "Paris", "picked": false},
        "view": [
            {"type": "buttons", "items": [{"label": "B — {{city}}", "action": "pick"}]},
            {"type": "input", "key": "city", "label": "City"}
        ],
        "actions": {"pick": [
            {"op": "set", "key": "picked", "value": true},
            {"op": "answer"},
            {"op": "set", "key": "city", "value": "Rome"}
        ]}
    })
}

#[test]
fn answers_require_a_button_and_have_no_hidden_text_fields() {
    let data = quiz();
    let mut ui = Session::new(Document::parse(&data.to_string()).unwrap()).unwrap();
    assert!(ui.document().sends("pick"));
    assert!(ui.document().answers("pick"));
    assert!(ui.activate("pick").is_err());
    assert!(ui.activate_button(1, 0).is_err());
    assert_eq!(ui.value("picked"), "false");
    assert_eq!(ui.activate_button(0, 0).unwrap(), Some("B — Paris".into()));
    assert_eq!(ui.value("picked"), "true");
    assert_eq!(ui.value("city"), "Rome");

    for control in [
        json!({"type":"input","key":"city","label":"City","action":"pick"}),
        json!({"type":"toggle","key":"picked","label":"Pick","action":"pick"}),
        json!({"type":"text","text":"Pick","action":"pick"}),
    ] {
        let mut invalid = quiz();
        invalid["view"] = json!([control]);
        assert!(Document::parse(&invalid.to_string()).is_err());
    }
    let mut invalid = quiz();
    invalid["actions"]["pick"][1]["text"] = json!("hidden message");
    assert!(Document::parse(&invalid.to_string()).is_err());
}

#[test]
fn validation_counts_all_message_steps_including_called_actions() {
    for first in [json!({"op":"send","text":"first"}), json!({"op":"answer"})] {
        for second in [json!({"op":"send","text":"second"}), json!({"op":"answer"})] {
            for called in [false, true] {
                let mut data = quiz();
                data["actions"]["pick"] = if called {
                    data["actions"]["other"] = json!([second]);
                    json!([first, {"op":"call","action":"other"}])
                } else {
                    json!([first, second])
                };
                let answer_involved = first["op"] == "answer" || second["op"] == "answer";
                if answer_involved {
                    assert!(Document::parse(&data.to_string()).is_err());
                } else {
                    // Two sends predate answers: they still parse and fail
                    // transactionally when activated.
                    let mut ui = Session::new(Document::parse(&data.to_string()).unwrap()).unwrap();
                    assert!(ui.activate("pick").is_err());
                }
            }
        }
    }
    let mut data = quiz();
    data["actions"]["relay"] = json!([{"op":"call","action":"pick"}]);
    data["view"][0]["items"][0]["action"] = json!("relay");
    let mut ui = Session::new(Document::parse(&data.to_string()).unwrap()).unwrap();
    assert!(ui.document().answers("relay"));
    assert_eq!(ui.activate_button(0, 0).unwrap(), Some("B — Paris".into()));
    data["actions"]["relay"] = json!([
        {"op":"call","action":"pick"}, {"op":"call","action":"pick"}
    ]);
    assert!(Document::parse(&data.to_string()).is_err());
}

#[test]
fn failed_answer_actions_roll_back_without_returning_a_message() {
    let mut data = quiz();
    data["actions"]["pick"][2]["value"] = json!({"div":[1,0]});
    let mut ui = Session::new(Document::parse(&data.to_string()).unwrap()).unwrap();
    assert!(ui.activate_button(0, 0).is_err());
    assert_eq!(ui.value("picked"), "false");
    assert_eq!(ui.value("city"), "Paris");
}

#[test]
fn rendered_answers_are_single_pass_single_line_and_byte_bounded() {
    for label in [
        "  B — Paris  ".to_string(),
        "é".repeat(60),
        "{{picked}}".into(),
    ] {
        let mut data = quiz();
        data["view"][0]["items"][0]["label"] = json!("{{city}}");
        data["state"]["city"] = json!(label);
        let mut ui = Session::new(Document::parse(&data.to_string()).unwrap()).unwrap();
        assert_eq!(ui.activate_button(0, 0).unwrap(), Some(label));
    }
    for label in ["two\nlines".to_string(), "é".repeat(61), "x".repeat(121)] {
        let mut data = quiz();
        data["view"][0]["items"][0]["label"] = json!("{{city}}");
        data["state"]["city"] = json!(label);
        // Unbound scalar state permits newlines and strings over 120 bytes.
        data["view"].as_array_mut().unwrap().truncate(1);
        let mut ui = Session::new(Document::parse(&data.to_string()).unwrap()).unwrap();
        assert!(ui.activate_button(0, 0).is_err());
        assert_eq!(ui.value("picked"), "false");
    }
    for label in [
        "bad\tlabel",
        "bad\rlabel",
        "bad\u{1b}label",
        "bad\u{7f}label",
    ] {
        let mut data = quiz();
        data["state"]["city"] = json!(label);
        assert!(Document::parse(&data.to_string()).is_err());
        data["state"]["city"] = json!("Paris");
        data["view"][0]["items"][0]["label"] = json!(label);
        assert!(Document::parse(&data.to_string()).is_err());
    }
}
