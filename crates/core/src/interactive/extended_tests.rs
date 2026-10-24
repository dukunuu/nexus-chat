use super::*;

fn session(node: Value, state: Value) -> Session {
    Session::new(Document::parse(&json!({"version":1,"title":"Demo","view":[node],"state":state,"actions":{"save":[{"op":"set","key":"saved","value":true}]}}).to_string()).unwrap()).unwrap()
}

#[test]
fn every_new_signature_lowers_and_every_prefix_is_safe() {
    for component in [
        "Tabs([Tab(\"A\", [Text(\"One\")]), Tab(\"B\", [Text(\"Two\")])], $n)",
        "Accordion([Section(\"More\", [Text(\"Detail\")], true)])",
        "Chart(\"line\", [Series(\"A\", [1, $n, 3], \"success\")], [\"Jan\", \"Feb\", \"Mar\"], 6)",
        "Table([\"Name\"], [[\"Ada\"], [\"Bob\"]], $n, true, 1, true)",
        "Form(\"Contact\", [Input($name, \"Name\"), Slider($n, \"N\", 0, 5, 1)], \"Save\", @ToAssistant($name))",
        "Timeline([Event(\"Now\", \"Ready\", \"Details\", \"success\")])",
        "Rating($n, \"Score\", 5)",
        "Code(\"let x = 1;\", \"rust\")",
        "Badge(\"Ready\", \"success\")",
        "Meter(\"Done\", $n, 5, \"warning\")",
        "Slider($n, \"Level\", 0, 10, 1)",
    ] {
        let source = format!(
            "root = Panel(\"Demo\", [Text(\"Arriving\"), {component}])\n$n = 0\n$name = \"Ada\"\n"
        );
        let document =
            Document::check(&source).unwrap_or_else(|errors| panic!("{component}: {errors:?}"));
        assert_eq!(document.nodes().len(), 2, "{component}");
        assert!(!document.plain_text().is_empty());
        let mut parser = StreamParser::default();
        for (end, _) in source
            .char_indices()
            .chain(std::iter::once((source.len(), '\0')))
        {
            if let Some(document) = parser.set(&source[..end]) {
                assert!(Session::new(document).is_ok(), "{component} at {end}");
            }
        }
        assert_eq!(parser.set(&source).unwrap().rows(), document.rows());
    }
    for kind in ["line", "area", "scatter", "bar", "pie"] {
        assert!(
            Document::check(&format!(
                "root = Panel(\"C\", [Chart(\"{kind}\", [Series(\"A\", [1, 2])])])"
            ))
            .is_ok()
        );
    }
}

#[test]
fn new_components_report_located_schema_and_semantic_errors() {
    for node in [
        json!({"type":"tabs","items":[{"label":"A","view":[]}],"key":"missing"}),
        json!({"type":"accordion","items":[]}),
        json!({"type":"chart","kind":"radar","series":[]}),
        json!({"type":"chart","kind":"line","series":[{"name":"A","values":["bad"]}]}),
        json!({"type":"form","title":"F","fields":[],"submit_label":"Go","action":"absent"}),
        json!({"type":"timeline","items":[{"time":"Now"}]}),
        json!({"type":"rating","key":"n","label":"R","max":0}),
        json!({"type":"code","source":"x","lang":"rust\n```"}),
        json!({"type":"badge","text":"x","tone":"purple"}),
        json!({"type":"meter","label":"M","value":"bad"}),
        json!({"type":"slider","key":"n","label":"S","min":5,"max":1,"step":0}),
        json!({"type":"table","columns":["A"],"rows_data":[],"page_size":0}),
    ] {
        let source = json!({"version":1,"title":"Bad","state":{"n":0},"view":[node]});
        let errors = Document::check_value(source).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.path.starts_with("view[0]") && !error.message.is_empty()),
            "{errors:?}"
        );
    }
    let errors = Document::check_value(json!({"version":1,"title":"Bad","view":[{"type":"tabs","items":[{"label":"A","view":[{"type":"meter","label":"M","value":1,"tone":"blue"}]}]}]})).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.path == "view[0].items[0].view[0].tone")
    );
}

#[test]
fn tabs_bind_state_and_hidden_controls_keep_stable_node_addresses() {
    let mut session = session(
        json!({"type":"tabs","key":"tab","items":[{"label":"A","view":[{"type":"input","key":"name","label":"Name"}]},{"label":"B","view":[{"type":"slider","key":"n","label":"N","min":0,"max":5,"step":1}]}]}),
        json!({"tab":0,"name":"Ada","n":1,"saved":false}),
    );
    assert_eq!(
        session
            .visible_nodes()
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    session.select_tab(0, 1).unwrap();
    assert_eq!(session.value("tab"), "1");
    assert_eq!(
        session
            .visible_nodes()
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    session.step(2, true).unwrap();
    assert_eq!(session.value("n"), "2");
    session.select_tab(0, 99).unwrap();
    assert_eq!(session.tab(0), 1);
}

#[test]
fn accordion_expansion_is_independent_and_defaults_are_respected() {
    let mut session = session(
        json!({"type":"accordion","items":[{"title":"A","view":[{"type":"text","text":"One"}],"open":true},{"title":"B","view":[{"type":"text","text":"Two"}]}]}),
        json!({"saved":false}),
    );
    assert!(session.section_open(0, 0));
    assert!(!session.section_open(0, 1));
    session.expand_section(0, 1).unwrap();
    session.expand_section(0, 0).unwrap();
    assert_eq!(
        session
            .visible_nodes()
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert!(session.expand_section(0, 2).is_err());
}

#[test]
fn form_blocks_empty_or_invalid_fields_and_submits_current_values_transactionally() {
    let node = json!({"type":"form","title":"Profile","fields":[{"type":"input","key":"name","label":"Name","rules":{"required":true}}],"submit_label":"Save","action":"save"});
    let mut session = session(node.clone(), json!({"name":"","saved":false}));
    assert!(!session.form_valid(0));
    assert!(session.submit_form(0).is_err());
    assert_eq!(session.value("saved"), "false");
    session.input("name", "  ".into()).unwrap();
    assert!(!session.form_valid(0));
    session.input("name", "Ada".into()).unwrap();
    assert!(session.form_valid(0));
    session.submit_form(0).unwrap();
    assert_eq!(session.value("saved"), "true");
    let mut invalid = node;
    invalid["fields"][0]["rules"] = json!({"required": true, "minLength": 5});
    let session = self::session(invalid, json!({"name":"Ada","saved":false}));
    assert!(!session.form_valid(0));
}

#[test]
fn table_sort_filter_page_and_selection_preserve_original_row_indices() {
    let mut session = session(
        json!({"type":"table","columns":["Name","Count"],"rows_data":[["Zoe","10"],["Ada","2"],["Bob","1"]],"select":"row","sortable":true,"page_size":1,"search":true}),
        json!({"row":0,"saved":false}),
    );
    session.sort_table(0, 1).unwrap();
    assert_eq!(session.table_rows(0), [2, 1, 0]);
    session.sort_table(0, 1).unwrap();
    assert_eq!(session.table_rows(0), [0, 1, 2]);
    session.page_table(0, true).unwrap();
    assert_eq!(session.table_state(0).page, 1);
    session.search_table(0, "AD".into(), true).unwrap();
    assert_eq!(session.table_rows(0), [1]);
    assert_eq!(session.table_state(0).page, 0);
    session.select_row(0, session.table_rows(0)[0]).unwrap();
    assert_eq!(session.value("row"), "1");
    session.search_table(0, "nothing".into(), false).unwrap();
    session.page_table(0, true).unwrap();
    assert_eq!(session.table_state(0).page, 0);
    assert!(session.table_rows(0).is_empty());
}

#[test]
fn rating_slider_and_copy_are_local_and_bounded() {
    let mut rating = session(
        json!({"type":"rating","key":"n","label":"Score"}),
        json!({"n":99,"saved":false}),
    );
    assert_eq!(rating.value("n"), "5");
    rating.rate(0, 2).unwrap();
    assert_eq!(rating.value("n"), "2");
    let mut slider = session(
        json!({"type":"slider","key":"n","label":"N","min":1,"max":5,"step":0.5}),
        json!({"n":0,"saved":false}),
    );
    slider.slide(0, 0.3).unwrap();
    assert_eq!(slider.value("n"), "2");
    slider.step(0, false).unwrap();
    assert_eq!(slider.value("n"), "1.5");
    let mut code = session(
        json!({"type":"code","source":"let name = \"{{literal}}\";","lang":"rust"}),
        json!({"saved":false}),
    );
    code.copy_code(0);
    assert_eq!(
        code.take_copy().as_deref(),
        Some("let name = \"{{literal}}\";")
    );
    assert_eq!(code.take_copy(), None);
}
