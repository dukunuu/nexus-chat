use super::*;

fn data(node: Value, state: Value) -> Value {
    json!({"version":1,"title":"Panel","state":state,"view":[node],"actions":{}})
}
fn parse(node: Value, state: Value) -> Result<Document> {
    Document::parse(&data(node, state).to_string())
}
fn session(node: Value, state: Value) -> Session {
    Session::new(parse(node, state).unwrap()).unwrap()
}

#[test]
fn every_component_parses_with_fixed_height_and_defaults() {
    for (node, state, rows) in [
        (
            json!({"type":"kv","items":[["Name","{{name}}"],["Status","Ready"]]}),
            json!({"name":"Ada"}),
            2,
        ),
        (
            json!({"type":"table","columns":["Name","Status"],"rows_data":[["Ada","Ready"]],"select":"row"}),
            json!({"row":0}),
            2,
        ),
        (
            json!({"type":"table","columns":["Name"],"rows_data":[]}),
            json!({}),
            1,
        ),
        (
            json!({"type":"progress","label":"Done","value":{"get":"n"}}),
            json!({"n":0.5}),
            1,
        ),
        (
            json!({"type":"bars","items":[{"label":"A","value":{"add":[1,2]}}]}),
            json!({}),
            1,
        ),
        (
            json!({"type":"sparkline","values":[0,1,{"number":"2.5"}]}),
            json!({}),
            1,
        ),
        (
            json!({"type":"checklist","items":[{"label":"Ready","key":"ready"}]}),
            json!({"ready":true}),
            1,
        ),
        (
            json!({"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]}),
            json!({"mode":"Fast"}),
            1,
        ),
        (
            json!({"type":"toggle","key":"ready","label":"Ready"}),
            json!({"ready":false}),
            1,
        ),
    ] {
        let doc = parse(node.clone(), state).unwrap();
        assert_eq!(doc.nodes()[0].rows(), rows, "{node}");
        assert_eq!(doc.rows(), rows + 3);
        Session::new(doc).unwrap();
        let mut unknown = node;
        unknown["handler"] = json!("code");
        assert!(
            parse(
                unknown,
                json!({"name":"Ada","row":0,"n":1,"ready":true,"mode":"Fast"})
            )
            .is_err()
        );
    }
}

#[test]
fn component_collection_limits_are_inclusive() {
    for (kind, field, item, min, max) in [
        ("kv", "items", json!(["Label", "Value"]), 1, 8),
        ("table", "columns", json!("Header"), 1, 6),
        ("bars", "items", json!({"label":"A","value":1}), 1, 8),
        ("sparkline", "values", json!(1), 1, 64),
        (
            "checklist",
            "items",
            json!({"label":"A","key":"ready"}),
            1,
            8,
        ),
        ("select", "options", json!("A"), 2, 8),
    ] {
        for count in [0, min - 1, min, max, max + 1] {
            let mut node = json!({"type":kind});
            node[field] = json!(vec![item.clone(); count]);
            if kind == "table" {
                node["rows_data"] = json!([]);
            }
            if kind == "select" {
                node["key"] = json!("mode");
                node["label"] = json!("Mode");
            }
            assert_eq!(
                parse(node, json!({"ready":true,"mode":"A"})).is_ok(),
                (min..=max).contains(&count),
                "{kind}: {count}"
            );
        }
    }
    for count in [0, 8, 9] {
        let result = parse(
            json!({"type":"table","columns":["A"],"rows_data":vec![vec!["cell"];count]}),
            json!({}),
        );
        assert_eq!(result.is_ok(), count <= 8);
        if let Ok(doc) = result {
            assert_eq!(usize::from(doc.rows()), count + 4);
        }
    }
    for rows in [0, 1, 3, 4] {
        assert_eq!(
            parse(
                json!({"type":"sparkline","values":[1],"rows":rows}),
                json!({})
            )
            .is_ok(),
            (1..=3).contains(&rows)
        );
    }
}

#[test]
fn component_bindings_require_existing_fields_of_the_right_type() {
    for node in [
        json!({"type":"toggle","key":"bound","label":"A"}),
        json!({"type":"checklist","items":[{"key":"bound","label":"A"}]}),
        json!({"type":"select","key":"bound","label":"A","options":["A","B"]}),
        json!({"type":"table","columns":["A"],"rows_data":[],"select":"bound"}),
    ] {
        assert!(parse(node.clone(), json!({})).is_err());
        for wrong in [json!(null), json!([]), json!({"random":[null]})] {
            assert!(parse(node.clone(), json!({"bound":wrong})).is_err());
        }
        let valid = match node["type"].as_str().unwrap() {
            "select" => json!("A"),
            "table" => json!(0),
            _ => json!(true),
        };
        assert!(parse(node.clone(), json!({"bound":valid})).is_ok());
        assert!(
            parse(
                node.clone(),
                json!({"bound":{"random":[valid,false,0,"bad"]}})
            )
            .is_err()
        );
        for wrong in [json!(true), json!(0), json!("A")] {
            let expected = wrong == valid || (valid.is_boolean() && wrong.is_boolean());
            assert_eq!(
                parse(node.clone(), json!({"bound":wrong})).is_ok(),
                expected
            );
        }
    }
}

#[test]
fn malformed_cells_options_expressions_and_templates_are_rejected() {
    for node in [
        json!({"type":"kv","items":[["A"]]}),
        json!({"type":"kv","items":[["A","B","C"]]}),
        json!({"type":"table","columns":["A","B"],"rows_data":[["one"]]}),
        json!({"type":"table","columns":["A"],"rows_data":[["one","two"]]}),
        json!({"type":"select","key":"mode","label":"A","options":["A","x".repeat(65)]}),
        json!({"type":"select","key":"mode","label":"A","options":["A","two\nlines"]}),
        json!({"type":"select","key":"mode","label":"A","options":["B","C"]}),
        json!({"type":"bars","items":[{"label":"A","value":1,"action":"bad"}]}),
        json!({"type":"checklist","items":[{"label":"A","key":"ready","action":"bad"}]}),
        json!({"type":"toggle","key":"ready","label":"{{missing}}"}),
        json!({"type":"table","columns":["{{missing}}"],"rows_data":[]}),
        json!({"type":"table","columns":["A"],"rows_data":[["{{missing}}"]]}),
        json!({"type":"kv","items":[["{{missing}}","A"]]}),
        json!({"type":"kv","items":[["A","{{missing}}"]]}),
        json!({"type":"bars","items":[{"label":"{{missing}}","value":1}]}),
        json!({"type":"progress","label":"{{missing}}","value":1}),
        json!({"type":"select","key":"mode","label":"{{missing}}","options":["A","B"]}),
        json!({"type":"checklist","items":[{"label":"{{missing}}","key":"ready"}]}),
    ] {
        assert!(
            parse(node.clone(), json!({"mode":"A","ready":true})).is_err(),
            "{node}"
        );
    }
    assert!(
        parse(
            json!({"type":"select","key":"mode","label":"A","options":["A","x".repeat(64)]}),
            json!({"mode":"A"})
        )
        .is_ok()
    );
    for value in [
        json!(true),
        json!("1"),
        json!({"get":"missing"}),
        json!({"get":"ready"}),
        json!({"random":[1,false]}),
        json!({"eq":[1,1]}),
        json!({"if":[true,1,false]}),
        json!({"eval":"code"}),
    ] {
        for node in [
            json!({"type":"progress","label":"A","value":value}),
            json!({"type":"progress","label":"A","value":1,"max":value}),
            json!({"type":"bars","items":[{"label":"A","value":value}]}),
            json!({"type":"sparkline","values":[value]}),
        ] {
            assert!(parse(node, json!({"ready":true})).is_err());
        }
    }
}

#[test]
fn new_nodes_share_the_existing_node_and_row_budgets() {
    let node = json!({"type":"toggle","key":"ready","label":"Ready"});
    let mut source = data(node.clone(), json!({"ready":true}));
    source["view"] = json!(vec![node.clone(); 16]);
    assert!(Document::parse(&source.to_string()).is_ok());
    source["view"] = json!(vec![node; 17]);
    assert!(Document::parse(&source.to_string()).is_err());
    let tall = json!({"type":"kv","items":vec![["A","B"];7]});
    source["view"] = json!([tall, tall, tall]);
    assert_eq!(Document::parse(&source.to_string()).unwrap().rows(), ROWS);
    source["view"][0]["items"] = json!(vec![["A", "B"]; 8]);
    assert!(Document::parse(&source.to_string()).is_err());
}

#[test]
fn local_boolean_and_choice_controls_do_not_run_actions() {
    let mut source = data(
        json!({"type":"toggle","key":"ready","label":"Ready"}),
        json!({"ready":false,"done":true,"mode":"Fast","untouched":false}),
    );
    source["view"] = json!([
        {"type":"toggle","key":"ready","label":"Ready"},
        {"type":"checklist","items":[{"label":"Done","key":"done"}]},
        {"type":"select","key":"mode","label":"Mode","options":["Fast","Full","Safe"]}
    ]);
    source["actions"]["send"] = json!([{"op":"send","text":"Should not run"}]);
    let mut ui = Session::new(Document::parse(&source.to_string()).unwrap()).unwrap();
    ui.toggle("ready").unwrap();
    ui.toggle("done").unwrap();
    assert_eq!(ui.value("ready"), "true");
    assert_eq!(ui.value("done"), "false");
    assert!(ui.toggle("untouched").is_err());
    assert!(ui.cycle(0, true).is_err());
    for expected in ["Full", "Safe", "Fast"] {
        ui.cycle(2, true).unwrap();
        assert_eq!(ui.value("mode"), expected);
    }
    ui.cycle(2, false).unwrap();
    assert_eq!(ui.value("mode"), "Safe");
    let mut ui = session(
        json!({"type":"select","key":"mode","label":"Mode","options":["Fast","Full","Full","Safe"]}),
        json!({"mode":"Full"}),
    );
    ui.cycle(0, true).unwrap();
    assert_eq!(ui.value("mode"), "Safe");
    ui.cycle(0, false).unwrap();
    assert_eq!(ui.value("mode"), "Full");
    ui.cycle(0, false).unwrap();
    assert_eq!(ui.value("mode"), "Fast");
}

#[test]
fn table_indices_and_progress_values_clamp_without_growing() {
    for (initial, expected) in [(json!(-9), 0), (json!(1.9), 1), (json!(99), 2)] {
        let mut ui = session(
            json!({"type":"table","columns":["A"],"rows_data":[["a"],["b"],["c"]],"select":"row"}),
            json!({"row":initial}),
        );
        assert_eq!(ui.selected_row("row"), expected);
        ui.select_row(0, usize::MAX).unwrap();
        assert_eq!(ui.selected_row("row"), 2);
        ui.select_row(0, 0).unwrap();
        assert_eq!(ui.selected_row("row"), 0);
        assert_eq!(ui.document().rows(), 7);
    }
    let ui = session(
        json!({"type":"table","columns":["A"],"rows_data":[],"select":"row"}),
        json!({"row":99}),
    );
    assert_eq!(ui.selected_row("row"), 0);
    let ui = session(
        json!({"type":"progress","label":"Done","value":0}),
        json!({}),
    );
    for (value, max, expected) in [
        (-1.0, 10.0_f64, 0.0),
        (11.0, 10.0, 10.0),
        (2.5, 10.0, 2.5),
        (1.0, 0.0, 0.0),
        (1.0, -1.0, 0.0),
    ] {
        assert_eq!(
            ui.progress(&json!(value), &json!(max)).unwrap(),
            (expected, max.max(0.0))
        );
    }
}

#[test]
fn actions_preserve_bindings_transactionally_and_clamp_table_selection() {
    let mut source = data(
        json!({"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]}),
        json!({"mode":"Fast","ready":false,"row":0}),
    );
    source["view"] = json!([
        source["view"][0],
        {"type":"toggle","key":"ready","label":"Ready"},
        {"type":"table","columns":["A"],"rows_data":[["a"],["b"]],"select":"row"}
    ]);
    for (key, value) in [
        ("mode", json!("Other")),
        ("mode", json!(false)),
        ("ready", json!(1)),
        ("row", json!("1")),
    ] {
        source["actions"]["bad"] = json!([
            {"op":"send","text":"Must not escape"},
            {"op":"set","key":key,"value":value}
        ]);
        let mut ui = Session::new(Document::parse(&source.to_string()).unwrap()).unwrap();
        assert!(ui.activate("bad").is_err());
        assert_eq!(ui.value("mode"), "Fast");
        assert_eq!(ui.value("ready"), "false");
        assert_eq!(ui.value("row"), "0");
    }
    source["actions"]["good"] = json!([
        {"op":"set","key":"mode","value":"Full"},
        {"op":"set","key":"ready","value":true},
        {"op":"set","key":"row","value":99}
    ]);
    let mut ui = Session::new(Document::parse(&source.to_string()).unwrap()).unwrap();
    ui.activate("good").unwrap();
    assert_eq!(ui.value("mode"), "Full");
    assert_eq!(ui.value("ready"), "true");
    assert_eq!(ui.value("row"), "1");
}
