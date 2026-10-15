use super::*;

const COUNTER: &str = include_str!("../../tests/fixtures/interactive/counter.json");
const RPS: &str = include_str!("../../tests/fixtures/interactive/rps.json");
fn session(source: &str) -> Session {
    Session::new(Document::parse(source).unwrap()).unwrap()
}

#[test]
fn authored_counter_forms_and_submissions_use_one_runtime() {
    let mut ui = session(COUNTER);
    assert_eq!(ui.value("count"), "0");
    assert_eq!(ui.activate("add").unwrap(), None);
    ui.activate("add").unwrap();
    assert_eq!(ui.text("Count: {{count}}").unwrap(), "Count: 2");
    ui.input("name", "Ada {{count}}".into()).unwrap();
    assert_eq!(
        ui.activate("submit").unwrap(),
        Some("UI response — Counter\nAda {{count}} chose 2".into())
    );
    assert!(ui.document().sends("submit"));
    assert!(!ui.document().sends("add"));
    ui.activate("reset").unwrap();
    assert_eq!(ui.value("count"), "0");
}

#[test]
fn rock_paper_scissors_rules_are_authored_data_not_a_builtin() {
    for player in 0..3 {
        for opponent in 0..3 {
            let mut data: Value = serde_json::from_str(RPS).unwrap();
            data["state"]["opponent"] = json!(opponent);
            let mut ui = session(&data.to_string());
            assert_eq!(ui.value("computer"), "?");
            assert_eq!(
                ui.activate(["rock", "paper", "scissors"][player]).unwrap(),
                None
            );
            let delta = (player + 3 - opponent) % 3;
            assert_eq!(
                ui.value("result"),
                ["Draw", "You win", "Computer wins"][delta]
            );
            assert_eq!(ui.value("wins"), if delta == 1 { "1" } else { "0" });
            assert_eq!(ui.value("losses"), if delta == 2 { "1" } else { "0" });
            assert_eq!(ui.value("draws"), if delta == 0 { "1" } else { "0" });
            ui.activate("reset").unwrap();
            assert_eq!(ui.value("wins"), "0");
        }
    }
}

#[test]
fn unsupported_code_unknown_bindings_cycles_and_controls_are_rejected() {
    for change in [
        json!({"op":"shell","command":"echo unsafe"}),
        json!({"op":"call","action":"add"}),
        json!({"op":"call","action":"missing"}),
        json!({"op":"set","key":"missing","value":1}),
        json!({"op":"set","key":"count","value":{"eval":"process.exit()"}}),
    ] {
        let mut data: Value = serde_json::from_str(COUNTER).unwrap();
        data["actions"]["add"] = json!([change]);
        assert!(Document::parse(&data.to_string()).is_err());
    }
    let mut data: Value = serde_json::from_str(COUNTER).unwrap();
    data["view"][0]["text"] = json!("\u{1b}[31munsafe");
    assert!(Document::parse(&data.to_string()).is_err());
    data["view"][0]["text"] = json!("{{missing}}");
    assert!(Document::parse(&data.to_string()).is_err());
    assert!(Document::parse(&"x".repeat(MAX_SOURCE + 1)).is_err());
}

#[test]
fn failed_actions_are_transactional_and_do_not_emit_partial_messages() {
    let mut data: Value = serde_json::from_str(COUNTER).unwrap();
    data["actions"]["bad"] = json!([
        {"op":"set","key":"count","value":123},
        {"op":"send","text":"first"}, {"op":"send","text":"second"}
    ]);
    let mut ui = session(&data.to_string());
    assert!(ui.activate("bad").is_err());
    assert_eq!(ui.value("count"), "0");
    data["actions"]["bad"] = json!([
        {"op":"set","key":"count","value":123},
        {"op":"set","key":"count","value":{"div":[1,0]}}
    ]);
    let mut ui = session(&data.to_string());
    assert!(ui.activate("bad").is_err());
    assert_eq!(ui.value("count"), "0");
    assert!(ui.input("count", "text".into()).is_err());
    assert!(ui.input("name", "x".repeat(1025)).is_err());
}

#[test]
fn prompt_example_is_valid_and_generic() {
    let prompt = include_str!("../../assets/prompts/interactive-ui.md");
    let source = prompt
        .split("```nexus-ui\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    let mut ui = session(source);
    ui.activate("plus").unwrap();
    assert_eq!(ui.value("count"), "1");
}

#[test]
fn expression_and_action_expansion_are_bounded() {
    let mut data: Value = serde_json::from_str(COUNTER).unwrap();
    let mut expr = json!(1);
    for _ in 0..20 {
        expr = json!({"not":expr});
    }
    data["actions"]["add"][0]["value"] = expr;
    assert!(Document::parse(&data.to_string()).is_err());
    let mut data: Value = serde_json::from_str(COUNTER).unwrap();
    for i in 0..7 {
        data["actions"][format!("fan{i}")] = json!(
            (0..10)
                .map(|_| json!({"op":"call","action":format!("fan{}",i+1)}))
                .collect::<Vec<_>>()
        );
    }
    data["actions"]["fan7"] = json!([]);
    assert!(Document::parse(&data.to_string()).is_err());
}

#[test]
fn numeric_and_boolean_expressions_have_safe_scalar_semantics() {
    for (expression, expected) in [
        (json!({"div":[1e-20,1e-20]}), "1"),
        (json!({"number":" 3.5 "}), "3.5"),
        (json!({"eq":[-0.0,0]}), "true"),
        (json!({"and":[false,{"gt":[{"div":[1,0]},0]}]}), "false"),
        (json!({"or":[true,{"gt":[{"div":[1,0]},0]}]}), "true"),
        (json!({"if":[true,7,{"div":[1,0]}]}), "7"),
    ] {
        let mut data: Value = serde_json::from_str(COUNTER).unwrap();
        data["actions"]["add"][0]["value"] = expression;
        let mut ui = session(&data.to_string());
        ui.activate("add").unwrap();
        assert_eq!(ui.value("count"), expected);
    }
}

#[test]
fn inputs_and_submission_badges_remain_valid_through_called_actions() {
    let mut data: Value = serde_json::from_str(COUNTER).unwrap();
    for value in [
        json!(9),
        json!("two\nlines"),
        json!({"random":["valid",false]}),
    ] {
        data["state"]["name"] = value;
        assert!(Document::parse(&data.to_string()).is_err());
    }
    data["state"]["name"] = json!("");
    data["actions"]["relay"] = json!([{"op":"call","action":"submit"}]);
    let mut ui = session(&data.to_string());
    assert!(ui.document().sends("relay"));
    assert!(
        ui.activate("relay")
            .unwrap()
            .unwrap()
            .starts_with("UI response — Counter\n")
    );
    data["actions"]["add"][0]["key"] = json!("name");
    let mut ui = session(&data.to_string());
    assert!(ui.activate("add").is_err());
    assert_eq!(ui.value("name"), "");
    assert!(ui.input("name", "two\nlines".into()).is_err());
    assert!(ui.document().rows() <= ROWS);
}

#[test]
fn authored_calculators_can_parse_inputs_but_not_code_or_nonfinite_numbers() {
    let mut data: Value = serde_json::from_str(COUNTER).unwrap();
    data["actions"]["add"][0]["value"] = json!({"mul":[{"number":{"get":"name"}},2]});
    let mut ui = session(&data.to_string());
    ui.input("name", "2.5".into()).unwrap();
    ui.activate("add").unwrap();
    assert_eq!(ui.value("count"), "5");
    for text in ["NaN", "inf", "1e100", "process.exit()", "1+2"] {
        ui.input("name", text.into()).unwrap();
        assert!(ui.activate("add").is_err());
        assert_eq!(ui.value("count"), "5");
    }
}
