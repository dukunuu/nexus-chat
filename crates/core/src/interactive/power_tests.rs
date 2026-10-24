use super::*;

fn session(definitions: &str, view: &str) -> Session {
    Session::new(
        Document::parse(&format!("root = Panel(\"Data\", [{view}])\n{definitions}")).unwrap(),
    )
    .unwrap()
}
fn rendered(session: &Session, index: usize) -> String {
    let Node::Text { text, .. } = &session.document().nodes()[index] else {
        panic!("text");
    };
    session.text(text).unwrap()
}

#[test]
fn objects_access_index_length_and_pluck_are_reactive_and_missing_is_null() {
    let mut s = session(
        "$rows = [{name: 'Ada', price: 3}, {name: 'Lin', price: 5}]\n$i = 0\nobj = {nested: {\"space key\": 7}}",
        "Text($rows[$i].name), Text($rows.price), Text($rows.length), Text(obj.nested['space key']), Text(obj.missing), Buttons([Button('Next', @Set($i, $i + 1))])",
    );
    assert_eq!(rendered(&s, 0), "Ada");
    assert_eq!(rendered(&s, 1), "[3,5]");
    assert_eq!(rendered(&s, 2), "2");
    assert_eq!(rendered(&s, 3), "7");
    assert_eq!(rendered(&s, 4), "null");
    s.activate_button(5, 0).unwrap();
    assert_eq!(rendered(&s, 0), "Lin");
    assert_eq!(
        eval(&json!({"index":[{"get":"rows"},999]}), &s.state, &mut 1024).unwrap(),
        Value::Null
    );
    assert_eq!(
        data::field(&json!([{}, {"name":"x"}]), "name"),
        json!([null, "x"])
    );
    assert_eq!(data::field(&Value::Null, "name"), Value::Null);
}

#[test]
fn every_builtin_has_spec_results_and_empty_nonarray_fallbacks() {
    let s = session(
        "a = [2, '4', -3]",
        "Text(@Count(a)), Text(@First(a)), Text(@Last(a)), Text(@Sum(a)), Text(@Avg(a)), Text(@Min(a)), Text(@Max(a)), Text(@Round(-1.5)), Text(@Round(1.235, 2)), Text(@Abs(-4)), Text(@Floor(1.9)), Text(@Ceil(1.1))",
    );
    let values: Vec<_> = (0..12).map(|i| rendered(&s, i)).collect();
    assert_eq!(
        values,
        [
            "3", "2", "-3", "3", "1", "-3", "4", "-1", "1.24", "4", "1", "2"
        ]
    );
    for op in ["count", "sum", "avg", "min", "max"] {
        assert_eq!(
            data::evaluate(op, &[json!(false)]).unwrap().as_f64(),
            Some(0.0)
        );
        assert_eq!(
            data::evaluate(op, &[json!([])]).unwrap().as_f64(),
            Some(0.0)
        );
    }
    for op in ["first", "last"] {
        assert_eq!(data::evaluate(op, &[json!([])]).unwrap(), Value::Null);
    }
    assert!(Document::parse("root = Panel('Bad', [Text(@Round(1, 100))])").is_err());
}

#[test]
fn filters_cover_all_operators_dot_paths_empty_field_and_sort_is_stable() {
    for (op, count) in [
        ("==", 1),
        ("!=", 2),
        (">", 1),
        ("<", 1),
        (">=", 2),
        ("<=", 2),
        ("contains", 1),
    ] {
        let target = if op == "contains" { "'2'" } else { "2" };
        let s = session(
            &format!("rows = [{{x: {{n: 1}}}}, {{x: {{n: 2}}}}, {{x: {{n: 3}}}}]"),
            &format!("Text(@Count(@Filter(rows, 'x.n', '{op}', {target})))"),
        );
        assert_eq!(rendered(&s, 0), count.to_string(), "{op}");
    }
    let s = session(
        "rows = [{n: 2, id: 'a'}, {n: '2', id: 'b'}, {n: 1, id: 'c'}]",
        "Text(@Sort(rows, 'n').id), Text(@Sort(rows, 'n', 'desc').id), Text(@Sort(['B','a'], '')), Text(@Filter([1,2,3], '', '>', 1))",
    );
    assert_eq!(rendered(&s, 0), "[\"c\",\"a\",\"b\"]");
    assert_eq!(rendered(&s, 1), "[\"a\",\"b\",\"c\"]");
    assert_eq!(rendered(&s, 2), "[\"a\",\"B\"]");
    assert_eq!(rendered(&s, 3), "[2,3]");
    assert_eq!(
        data::evaluate("sort", &[json!(1), json!("")]).unwrap(),
        json!(1)
    );
    assert_eq!(
        data::evaluate("filter", &[json!(null), json!(""), json!("?"), json!(1)]).unwrap(),
        json!([])
    );
}

#[test]
fn each_reexpands_on_state_change_and_actions_capture_their_own_row() {
    let mut s = session(
        "$rows = [{name: 'Ada'}, {name: 'Lin'}]\n$selected = ''\nrow = 'shadowed'\nlist = @Each($rows, 'row', Text(row.name))",
        "Text('People'), list, Buttons(@Each($rows, 'row', Button(row.name, @Set($selected, row.name)))), Buttons([Button('Replace', @Set($rows, [{name: 'Grace'}]))])",
    );
    assert_eq!(rendered(&s, 1), "Ada");
    assert_eq!(rendered(&s, 2), "Lin");
    s.activate_button(3, 1).unwrap();
    assert_eq!(s.value("selected"), "Lin");
    s.activate_button(4, 0).unwrap();
    assert_eq!(rendered(&s, 1), "Grace");
    assert_eq!(s.document().nodes().len(), 4);
    s.activate_button(2, 0).unwrap();
    assert_eq!(s.value("selected"), "Grace");
    let too_many = format!(
        "root = Panel('Cap', @Each([{}], 'x', Text(x)))",
        vec!["1"; 201].join(",")
    );
    assert!(
        Document::parse(&too_many)
            .unwrap_err()
            .to_string()
            .contains("200 items")
    );
    let too_many_nodes = format!(
        "root = Panel('Cap', @Each([{}], 'x', Text(x)))",
        vec!["1"; 17].join(",")
    );
    assert!(Document::parse(&too_many_nodes).is_err());
    let nested = session("", "Text(@Count(@Each([1,2], 'x', @Each([3], 'x', x))))");
    assert_eq!(rendered(&nested, 0), "2");
}

#[test]
fn operators_short_circuit_ternary_and_reset_restore_declared_defaults() {
    let mut s = session(
        "$n = 3\n$yes = true",
        "Progress('n', (!false && (3 != 4) && 3 >= 3 && 2 <= 3 || false) ? 9 % 4 : 1 / 0, 10), Buttons([Button('Change', Action([@Set($n, 7), @Set($yes, false)])), Button('Reset', @Reset($n, $yes))])",
    );
    let Node::Progress { value, .. } = &s.document().nodes()[0] else {
        panic!();
    };
    assert_eq!(s.number(value).unwrap(), 1.0);
    s.activate_button(1, 0).unwrap();
    assert_eq!(s.value("n"), "7");
    s.activate_button(1, 1).unwrap();
    assert_eq!(s.value("n"), "3");
    assert_eq!(s.value("yes"), "true");
}

#[test]
fn open_url_requires_activation_and_separate_confirmation_and_is_consumed() {
    let mut s = session(
        "",
        "Buttons([Button('Visit', @OpenUrl('https://example.com/path'))])",
    );
    assert!(s.pending_url().is_none());
    let mut opened = Vec::new();
    s.confirm_url(true, |url| {
        opened.push(url.to_string());
        Ok(())
    })
    .unwrap();
    assert!(opened.is_empty());
    s.activate_button(0, 0).unwrap();
    assert_eq!(s.pending_url(), Some("https://example.com/path"));
    s.confirm_url(false, |_| panic!("cancelled")).unwrap();
    s.activate_button(0, 0).unwrap();
    assert_eq!(
        s.confirm_url(true, |url| {
            opened.push(url.to_string());
            Ok(())
        })
        .unwrap(),
        Some("Opened https://example.com/path".into())
    );
    assert_eq!(opened, ["https://example.com/path"]);
    assert!(s.pending_url().is_none());
    for url in [
        "http://example.com",
        "javascript:alert(1)",
        "https://user:pass@example.com",
    ] {
        assert!(
            Document::parse(&format!(
                "root = Panel('Bad', [Buttons([Button('Go', @OpenUrl('{url}'))])])"
            ))
            .is_err()
        );
    }
}

#[test]
fn assistant_context_and_form_state_are_captured_at_the_send_step() {
    let mut s = session(
        "$name = 'Ada'\n$n = 1",
        "Input($name, 'Name'), Buttons([Button('Send', Action([@Set($n, 2), @ToAssistant('Hello ' + $name, {nested: {n: $n}}), @Set($n, 3)]))])",
    );
    s.input("name", "Lin".into()).unwrap();
    assert_eq!(
        s.activate_button(1, 0).unwrap(),
        Some("UI response — Data\nHello Lin".into())
    );
    assert_eq!(
        s.assistant_payload(),
        Some(&json!({"context":{"nested":{"n":2}}, "state":{"name":"Lin","n":2}}))
    );
    assert_eq!(s.value("n"), "3");
}

#[test]
fn every_input_rule_reports_inline_and_blocks_reads_but_not_reset() {
    for (rule, invalid, valid) in [
        ("{required: true}", "", "x"),
        ("{email: true}", "a@b", "a@b.co"),
        ("{url: true}", "relative", "https://example.com"),
        ("{numeric: true}", "word", "12abc"),
        ("{minLength: 2}", "a", "ab"),
        ("{maxLength: 2}", "abc", "ab"),
        ("{min: 2}", "1", "2"),
        ("{max: 2}", "3", "2"),
        (r"{pattern: '^[A-Z]+[0-9]?$'}", "bad", "AB2"),
    ] {
        let mut s = session(
            "$v = ''\n$copy = ''",
            &format!(
                "Input($v, 'Value', {rule}), Buttons([Button('Read', @Set($copy, $v)), Button('Send', @ToAssistant('send')), Button('Reset', @Reset($v))])"
            ),
        );
        s.input("v", invalid.into()).unwrap();
        assert!(s.input_error("v").is_some(), "{rule}");
        assert!(s.activate_button(1, 0).is_err(), "{rule}");
        assert!(s.activate_button(1, 1).is_err());
        s.input("v", valid.into()).unwrap();
        assert_eq!(s.input_error("v"), None, "{rule}");
        s.activate_button(1, 0).unwrap();
        assert_eq!(s.value("copy"), valid);
        s.activate_button(1, 2).unwrap();
        assert_eq!(s.value("v"), "");
    }
    assert!(
        validation::check(&serde_json::from_value(json!({"pattern":"^(a|b)+$"})).unwrap()).is_err()
    );
    assert_eq!(
        validation::error(&serde_json::from_value(json!({"email":true})).unwrap(), ""),
        None
    );
}

#[test]
fn patches_merge_delete_prune_and_retain_state_atomically() {
    let mut s = session(
        "$n = 1\nold = Text('old')\nunused = Text('orphan')\nbutton = Buttons([Button('Add', @Set($n, $n + 1))])",
        "old, button",
    );
    s.activate_button(1, 0).unwrap();
    s.patch("old = Text('new')\n$n = 9\nunused = null").unwrap();
    assert_eq!(rendered(&s, 0), "new");
    assert_eq!(s.value("n"), "2");
    let source = s.document.program.clone().unwrap();
    assert!(!source.contains("unused"));
    assert!(s.patch("old = Unknown('bad')").is_err());
    assert_eq!(s.document.program.as_ref(), Some(&source));
    assert_eq!(s.value("n"), "2");
    assert!(
        s.patch("old = null").is_err(),
        "deleting a still-referenced statement is invalid"
    );
    s.patch("root = Panel('Changed', [button])\nold = null")
        .unwrap();
    assert!(!s.document.program.as_ref().unwrap().contains("old ="));
    assert_eq!(s.value("n"), "2");
}

#[test]
fn streaming_objects_and_each_tails_are_safe_and_show_completed_siblings() {
    for tail in [
        "data = {name: 'Ada",
        "list = @Each([{name: 'Ada'}], 'x', Text(x.name)",
    ] {
        let source = format!("root = Panel('Live', [Text('Visible'), list])\n{tail}");
        let mut parser = StreamParser::default();
        for end in source.char_indices().map(|(i, _)| i).chain([source.len()]) {
            let _ = parser.set(&source[..end]);
        }
        let document = parser.set(&source).unwrap();
        assert!(matches!(&document.nodes()[0], Node::Text { text, .. } if text == "Visible"));
    }
    let deep = format!(
        "root = Panel('Deep', [Text({}1{})])",
        "{x:".repeat(40),
        "}".repeat(40)
    );
    assert!(Document::parse(&deep).is_err());
}
