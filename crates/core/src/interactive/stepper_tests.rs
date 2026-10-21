use super::*;

fn source() -> Value {
    json!({
        "version":1,"title":"Planner","state":{"days":5},
        "view":[{"type":"stepper","key":"days","label":"Days","min":1,"max":7,"step":2}],
        "actions":{}
    })
}

fn session(value: &Value) -> Session {
    Session::new(Document::parse(&value.to_string()).unwrap()).unwrap()
}

#[test]
fn stepper_is_a_one_row_focusable_control_with_readable_plain_text() {
    let doc = Document::check(&source().to_string()).unwrap();
    assert_eq!(doc.nodes()[0].rows(), 1);
    assert_eq!(doc.rows(), 4);
    assert_eq!(doc.control_count(), 1);
    assert!(!doc.is_static());
    assert_eq!(doc.plain_text(), "Planner\nDays: 5");
}

#[test]
fn stepper_rejects_invalid_ranges_steps_and_bindings() {
    for (field, value) in [
        ("min", json!(7)),
        ("min", json!(8)),
        ("max", json!(1)),
        ("max", json!(-1)),
        ("step", json!(0)),
        ("step", json!(-0.5)),
        ("min", json!(-1e13)),
        ("max", json!(1e13)),
        ("step", json!(1e13)),
        ("min", json!("1")),
        ("max", json!(null)),
        ("step", json!(true)),
        ("label", json!("{{unclosed")),
        ("key", json!("missing")),
        ("action", json!("send")),
    ] {
        let mut value_source = source();
        value_source["view"][0][field] = value;
        assert!(Document::parse(&value_source.to_string()).is_err());
    }
    for value in [
        json!(null),
        json!(true),
        json!("5"),
        json!({"random":[1,"2"]}),
    ] {
        let mut source = source();
        source["state"]["days"] = value;
        assert!(Document::check(&source.to_string()).is_err());
    }
    for field in ["min", "max", "step"] {
        let source = source().to_string();
        let original = match field {
            "min" => "\"min\":1",
            "max" => "\"max\":7",
            _ => "\"step\":2",
        };
        for invalid in ["1e400", "NaN", "Infinity", "-Infinity"] {
            let invalid_source = source.replace(original, &format!("\"{field}\":{invalid}"));
            assert!(Document::check(&invalid_source).is_err());
        }
    }
}

#[test]
fn stepper_clamps_initial_state_including_random_values() {
    for (initial, expected) in [(json!(-9), "1"), (json!(99), "7"), (json!(2.5), "2.5")] {
        let mut source = source();
        source["state"]["days"] = initial;
        let doc = Document::parse(&source.to_string()).unwrap();
        assert_eq!(doc.plain_text(), format!("Planner\nDays: {expected}"));
        assert_eq!(Session::new(doc).unwrap().value("days"), expected);
    }
    let mut source = source();
    source["state"]["days"] = json!({"random":[99]});
    assert_eq!(session(&source).value("days"), "7");
}

#[test]
fn local_steps_respect_both_bounds_and_never_run_actions() {
    let mut source = source();
    source["state"]["untouched"] = json!(false);
    source["actions"]["send"] = json!([
        {"op":"set","key":"untouched","value":true},
        {"op":"send","text":"Must not run"}
    ]);
    let mut ui = session(&source);
    for (forward, expected) in [
        (true, "7"),
        (true, "7"),
        (false, "5"),
        (false, "3"),
        (false, "1"),
        (false, "1"),
    ] {
        ui.step(0, forward).unwrap();
        assert_eq!(ui.value("days"), expected);
        assert_eq!(ui.value("untouched"), "false");
    }
    assert!(ui.step(1, true).is_err());
    source["view"][0] = json!({"type":"text","text":"Not a stepper"});
    assert!(session(&source).step(0, true).is_err());
}

#[test]
fn fractional_steps_clamp_without_snapping_initial_values() {
    let mut source = source();
    source["state"]["days"] = json!(0.25);
    source["view"][0]["min"] = json!(-0.5);
    source["view"][0]["max"] = json!(0.5);
    source["view"][0]["step"] = json!(0.5);
    let mut ui = session(&source);
    for (forward, expected) in [
        (true, "0.5"),
        (false, "0"),
        (false, "-0.5"),
        (false, "-0.5"),
    ] {
        ui.step(0, forward).unwrap();
        assert_eq!(ui.value("days"), expected);
    }
}

#[test]
fn actions_clamp_numeric_stepper_values_and_roll_back_wrong_types() {
    let mut source = source();
    for (value, expected) in [
        (json!(99), Some("7")),
        (json!(-1), Some("1")),
        (json!("3"), None),
    ] {
        source["actions"]["set"] = json!([{"op":"set","key":"days","value":value}]);
        let mut ui = session(&source);
        if let Some(expected) = expected {
            assert_eq!(ui.activate("set").unwrap(), None);
            assert_eq!(ui.value("days"), expected);
        } else {
            assert!(ui.activate("set").is_err());
            assert_eq!(ui.value("days"), "5");
        }
    }
}

#[test]
fn check_diagnostics_include_stepper_and_its_fields() {
    let mut source = source();
    source["view"][0]["type"] = json!("slider");
    let errors = Document::check(&source.to_string()).unwrap_err();
    assert_eq!(errors[0].path, "view[0]");
    assert!(errors[0].message.contains("stepper"));
    source["view"][0]["type"] = json!("stepper");
    source["view"][0]["handler"] = json!("code");
    let errors = Document::check(&source.to_string()).unwrap_err();
    for field in ["key", "label", "min", "max", "step"] {
        assert!(errors[0].message.contains(field));
    }
    source["view"][0].as_object_mut().unwrap().remove("handler");
    for field in ["min", "max", "step"] {
        let mut missing = source.clone();
        missing["view"][0].as_object_mut().unwrap().remove(field);
        let errors = Document::check(&missing.to_string()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.path == format!("view[0].{field}"))
        );
    }
}

#[test]
fn prompt_app_example_checks_and_updates_derived_progress_before_submission() {
    let prompt = include_str!("../../assets/prompts/interactive-ui.md");
    let source = prompt
        .split("```nexus-ui\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    assert!(source.len() <= 700);
    let doc = Document::check(source).unwrap();
    Document::parse(source).unwrap();
    assert_eq!(doc.control_count(), 4);
    let mut ui = Session::new(doc).unwrap();
    let Node::Progress { value, max, .. } = ui.document().nodes()[2].clone() else {
        panic!("app needs state-derived progress");
    };
    assert_eq!(ui.progress(&value, &max).unwrap(), (2.0, 5.0));
    ui.step(0, true).unwrap();
    ui.toggle("hiking").unwrap();
    assert_eq!(ui.progress(&value, &max).unwrap(), (5.0, 6.0));
    assert_eq!(
        ui.activate_button(3, 0).unwrap(),
        Some("UI response — Trip planner\nPlan 6 days: beach=true, hiking=true".into())
    );
}
