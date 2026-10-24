use super::*;

const PROGRAM: &str = r#"root = Panel("Budget", [Input($name, "Name"), Toggle($ready, "Ready"), Tabs([Tab("A", [Text("a")]), Tab("B", [Text("b")])]), Accordion([Section("S", [Text("s")]), Section("T", [Text("t")], true)]), Table(["Item", "Cost"], [["x", "3"], ["y", "1"], ["z", "2"]], $row, true, 2, true)])
$name = "Ada"
$ready = false
$row = 0"#;

fn session(source: &str) -> Session {
    Session::new(Document::parse(source).unwrap()).unwrap()
}

/// Node indices are preorder, so find components by kind rather than position.
fn index(session: &Session, wanted: fn(&Node) -> bool) -> usize {
    session
        .visible_nodes()
        .into_iter()
        .find_map(|(index, node)| wanted(node).then_some(index))
        .expect("component present")
}
fn tabs(s: &Session) -> usize {
    index(s, |node| matches!(node, Node::Tabs { .. }))
}
fn accordion(s: &Session) -> usize {
    index(s, |node| matches!(node, Node::Accordion { .. }))
}
fn table(s: &Session) -> usize {
    index(s, |node| matches!(node, Node::Table { .. }))
}

#[test]
fn state_and_presentation_survive_encode_decode_and_restore() {
    let mut first = session(PROGRAM);
    first.input("name", "Grace".into()).unwrap();
    first.toggle("ready").unwrap();
    let (tabs, accordion, table) = (tabs(&first), accordion(&first), table(&first));
    first.select_tab(tabs, 1).unwrap();
    first.expand_section(accordion, 0).unwrap();
    first.sort_table(table, 1).unwrap();
    first.page_table(table, true).unwrap();
    let record = first.snapshot().encode().unwrap();

    let mut second = session(PROGRAM);
    assert_eq!(second.value("name"), "Ada");
    second
        .restore(&PanelSnapshot::decode(&record).unwrap())
        .unwrap();
    assert_eq!(second.value("name"), "Grace");
    assert_eq!(second.value("ready"), "true");
    assert_eq!(second.tab(tabs), 1);
    assert!(second.section_open(accordion, 0));
    assert!(second.section_open(accordion, 1));
    assert_eq!(second.table_state(table).sort, Some((1, false)));
    assert_eq!(second.table_state(table).page, 1);
    assert_eq!(second.snapshot(), first.snapshot());
    assert!(!second.table_state(table).searching);
}

#[test]
fn every_state_change_moves_the_revision() {
    let mut s = session(PROGRAM);
    let (tabs, accordion, table) = (tabs(&s), accordion(&s), table(&s));
    let mut seen = vec![s.revision()];
    s.input("name", "x".into()).unwrap();
    seen.push(s.revision());
    s.select_tab(tabs, 1).unwrap();
    seen.push(s.revision());
    s.expand_section(accordion, 0).unwrap();
    seen.push(s.revision());
    s.sort_table(table, 0).unwrap();
    seen.push(s.revision());
    assert!(seen.windows(2).all(|pair| pair[0] < pair[1]), "{seen:?}");
}

#[test]
fn restore_drops_presentation_the_program_no_longer_has() {
    let mut first = session(PROGRAM);
    let (tabs, table) = (tabs(&first), table(&first));
    first.select_tab(tabs, 1).unwrap();
    first.sort_table(table, 1).unwrap();
    let mut snapshot = first.snapshot();
    // The same names, but a program without tabs or a sortable table.
    let mut simpler = session(
        "root = Panel(\"Budget\", [Input($name, \"Name\"), Table([\"Item\"], [[\"x\"]])])\n$name = \"Ada\"\n$ready = false\n$row = 0",
    );
    snapshot.state.insert("name".into(), json!("Grace"));
    simpler.restore(&snapshot).unwrap();
    assert_eq!(simpler.value("name"), "Grace");
    let simple_table = index(&simpler, |node| matches!(node, Node::Table { .. }));
    assert_eq!(simpler.table_state(simple_table).sort, None);
    assert_eq!(simpler.table_rows(simple_table), vec![0]);
}

#[test]
fn invalid_saved_values_leave_the_panel_untouched() {
    let mut s = session(PROGRAM);
    let mut snapshot = s.snapshot();
    snapshot.state.insert("ready".into(), json!("not a bool"));
    snapshot.state.insert("name".into(), json!("Changed"));
    let before = s.snapshot();
    assert!(s.restore(&snapshot).is_err());
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.value("name"), "Ada");
}

#[test]
fn records_are_versioned_and_bounded() {
    let s = session(PROGRAM);
    let mut snapshot = s.snapshot();
    assert_eq!(snapshot.v, 1);
    let text = snapshot.encode().unwrap();
    assert!(PanelSnapshot::decode(&text).is_ok());
    snapshot.v = 2;
    let newer = serde_json::to_string(&snapshot).unwrap();
    let error = PanelSnapshot::decode(&newer).unwrap_err().to_string();
    assert!(error.contains("newer"), "{error}");
    assert!(PanelSnapshot::decode("not json").is_err());
    assert!(PanelSnapshot::decode(&" ".repeat(MAX_RECORD + 1)).is_err());

    // An oversized query cache is dropped before the state is.
    let mut big = s.snapshot();
    big.queries
        .insert("q".into(), json!("x".repeat(MAX_RECORD + 10)));
    let lean = big.encode().unwrap();
    assert!(lean.len() <= MAX_RECORD);
    assert!(PanelSnapshot::decode(&lean).unwrap().queries.is_empty());
    // And a state too large to keep is refused rather than truncated.
    big.state
        .insert("name".into(), json!("y".repeat(MAX_RECORD)));
    assert!(big.encode().is_none());
}

#[test]
fn query_results_are_cached_and_restored_without_scheduling_a_call() {
    let source = r#"root = Panel("Files", [Text(q.text), Buttons([Button("Refresh", @Run(q))])])
q = Query("files", {action:"read",name:"a"}, {text:"Defaults"}, 1)"#;
    let mut fresh = session(source);
    let mut snapshot = fresh.snapshot();
    assert!(snapshot.queries.is_empty());
    snapshot
        .queries
        .insert("q".into(), json!({"text": "Cached text"}));
    fresh.restore(&snapshot).unwrap();
    let Node::Text { text, .. } = &fresh.document().nodes()[0] else {
        panic!("text")
    };
    assert_eq!(fresh.text(text).unwrap(), "Cached text");
    assert_eq!(
        fresh.snapshot().queries["q"],
        json!({"text": "Cached text"})
    );
    // Restored data is inert: nothing runs until the person activates the panel.
    let mut calls = 0;
    assert!(
        fresh
            .next_data_call(std::time::Instant::now(), &mut calls)
            .unwrap()
            .is_none()
    );
    assert!(fresh.needs_data_load());
    // Unknown statement ids are ignored.
    snapshot.queries.insert("gone".into(), json!(1));
    fresh.restore(&snapshot).unwrap();
    assert!(!fresh.snapshot().queries.contains_key("gone"));
}

#[test]
fn saved_names_wait_for_a_replayed_patch_that_declares_them() {
    let base = "root = Panel(\"Todo\", [Text(\"Items\")])\n$count = 1";
    let patch = "root = Panel(\"Todo\", [Text(\"Items\"), Text($note)])\n$note = \"fresh\"";
    // The patched panel was edited, saved, and then reloaded from its original source.
    let mut edited = session(base);
    edited.patch(patch).unwrap();
    edited.input("note", "kept".into()).unwrap_err();
    let mut snapshot = edited.snapshot();
    snapshot.state.insert("note".into(), json!("kept"));

    let mut reloaded = session(base);
    reloaded.restore(&snapshot).unwrap();
    assert_eq!(reloaded.value("note"), "");
    assert_eq!(reloaded.snapshot().state["note"], "kept");
    reloaded.patch(patch).unwrap();
    assert_eq!(reloaded.value("note"), "kept");
}

#[test]
fn panel_keys_separate_sources_and_repeats() {
    let a = panel_key("root = Panel(\"A\", [Text(\"x\")])", 0);
    assert_eq!(a, panel_key("root = Panel(\"A\", [Text(\"x\")])", 0));
    assert_ne!(a, panel_key("root = Panel(\"A\", [Text(\"x\")])", 1));
    assert_ne!(a, panel_key("root = Panel(\"B\", [Text(\"x\")])", 0));
}

#[test]
fn url_hosts_are_normalized_and_https_only() {
    assert_eq!(
        url_host("https://Example.COM/a?b=1").unwrap(),
        "example.com"
    );
    assert_eq!(
        url_host("https://example.com:8443/x").unwrap(),
        "example.com:8443"
    );
    assert_eq!(
        url_host("https://example.com:443/x").unwrap(),
        "example.com"
    );
    assert_eq!(
        url_host("https://bücher.example/").unwrap(),
        "xn--bcher-kva.example"
    );
    for url in [
        "http://example.com",
        "ftp://example.com",
        "https://user@example.com",
        "javascript:alert(1)",
        "example.com",
    ] {
        assert!(url_host(url).is_err(), "{url}");
    }
}

#[test]
fn pending_host_follows_the_confirmation() {
    let mut s = session(
        "root = Panel(\"Go\", [Buttons([Button(\"Visit\", @OpenUrl(\"https://Docs.Example.com/x\"))])])",
    );
    assert_eq!(s.pending_host(), None);
    s.activate_button(0, 0).unwrap();
    assert_eq!(s.pending_host().as_deref(), Some("docs.example.com"));
    s.confirm_url(false, |_| panic!("cancelled")).unwrap();
    assert_eq!(s.pending_host(), None);
}

#[test]
fn a_saved_value_of_the_wrong_kind_is_refused() {
    let mut s = session(
        "root = Panel(\"N\", [Text($n), Text($flag), Text($free)])\n$n = 1\n$flag = true\n$free = null",
    );
    let mut snapshot = s.snapshot();
    snapshot.state.insert("free".into(), json!("anything"));
    s.restore(&snapshot).unwrap();
    assert_eq!(s.value("free"), "anything");
    for (key, bad) in [("n", json!("x")), ("flag", json!(1)), ("n", json!(null))] {
        let mut wrong = s.snapshot();
        wrong.state.insert(key.into(), bad);
        assert!(s.restore(&wrong).is_err(), "{key}");
    }
}
