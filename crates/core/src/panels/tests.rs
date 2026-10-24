use super::*;
use crate::db::Db;
use serde_json::json;

const BUDGET: &str = "root = Panel(\"Budget\", [Text(\"Spent: \" + $spent), Input($note, \"Note\"), Buttons([Button(\"Spend\", @Set($spent, $spent + 5))])])\n$spent = 10\n$note = \"\"";

fn temp_db_path() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nexus-panels-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("nexus.db")
}

fn edited() -> Session {
    let mut session = Session::new(Document::parse(BUDGET).unwrap()).unwrap();
    session.activate_button(2, 0).unwrap();
    assert_eq!(session.value("spent"), "15");
    session.input("note", "groceries".into()).unwrap();
    session
}

#[test]
fn panel_state_round_trips_across_a_db_reopen() {
    let path = temp_db_path();
    let record = edited().snapshot().encode().unwrap();
    {
        let db = Db::open(&path).unwrap();
        save_state(db.conn(), "s1", "key-0", &record).unwrap();
        save_state(db.conn(), "s2", "key-0", "{\"v\":1}").unwrap();
    }
    let db = Db::open(&path).unwrap();
    let states = load_states(db.conn(), "s1").unwrap();
    assert_eq!(states.len(), 1);
    let mut restored = Session::new(Document::parse(BUDGET).unwrap()).unwrap();
    restored
        .restore(&PanelSnapshot::decode(&states["key-0"]).unwrap())
        .unwrap();
    assert_eq!(restored.value("note"), "groceries");
    // Replacing a record keeps one row per panel.
    save_state(db.conn(), "s1", "key-0", &record).unwrap();
    assert_eq!(load_states(db.conn(), "s1").unwrap().len(), 1);
    assert!(
        save_state(
            db.conn(),
            "s1",
            "big",
            &"x".repeat(crate::interactive::MAX_RECORD + 1)
        )
        .is_err()
    );
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn migration_adds_the_tables_to_an_older_db_and_deleting_a_session_drops_its_states() {
    let path = temp_db_path();
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute_batch("DROP TABLE panel_states; DROP TABLE pinned_panels;")
            .unwrap();
    }
    let db = Db::open(&path).unwrap();
    let space = db.default_space_id().unwrap();
    let session = db
        .create_session("chat", "model", &space, "chat")
        .unwrap()
        .id;
    save_state(db.conn(), &session, "k", "{\"v\":1}").unwrap();
    db.delete_session(&session).unwrap();
    assert!(load_states(db.conn(), &session).unwrap().is_empty());
    assert!(list(db.conn(), &space).unwrap().is_empty());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn pin_list_open_keeps_the_program_and_state() {
    let db = Db::open_in_memory().unwrap();
    let conn = db.conn();
    let session = edited();
    let name = suggest_name(conn, "space", session.document().title()).unwrap();
    assert_eq!(name, "budget");
    let app = pin(
        conn,
        "space",
        &name,
        session.program().unwrap(),
        &session.snapshot(),
    )
    .unwrap();
    assert_eq!(
        (app.name.as_str(), app.title.as_str()),
        ("budget", "Budget")
    );

    let listed = list(conn, "space").unwrap();
    assert_eq!(listed, vec![app.clone()]);
    assert!(list(conn, "other").unwrap().is_empty());
    for reference in [app.id.as_str(), "budget"] {
        let found = find(conn, "space", reference).unwrap().unwrap();
        let mut opened = Session::new(Document::parse(&found.program).unwrap()).unwrap();
        opened
            .restore(&PanelSnapshot::decode(&found.record).unwrap())
            .unwrap();
        assert_eq!(opened.value("note"), "groceries");
        assert_eq!(opened.value("spent"), "15");
    }
    assert_eq!(suggest_name(conn, "space", "Budget").unwrap(), "budget-2");

    // State saved from the opened view lands back on the same app.
    let mut opened = Session::new(Document::parse(&app.program).unwrap()).unwrap();
    opened.input("note", "rent".into()).unwrap();
    save_pinned_state(conn, &app.id, &opened.snapshot().encode().unwrap()).unwrap();
    let again = find(conn, "space", "budget").unwrap().unwrap();
    assert_eq!(
        PanelSnapshot::decode(&again.record).unwrap().state["note"],
        "rent"
    );
    assert!(again.updated_at >= app.updated_at);
}

#[test]
fn names_are_validated_unique_and_bounded() {
    let db = Db::open_in_memory().unwrap();
    let conn = db.conn();
    let session = edited();
    let program = session.program().unwrap();
    for bad in ["", "Has Space", "UPPER", &"a".repeat(33)] {
        assert!(
            pin(conn, "s", bad, program, &session.snapshot()).is_err(),
            "{bad:?}"
        );
    }
    assert!(pin(conn, "s", "ok", "not a program", &session.snapshot()).is_err());
    pin(conn, "s", "ok", program, &session.snapshot()).unwrap();
    assert!(
        pin(conn, "s", "ok", program, &session.snapshot())
            .unwrap_err()
            .to_string()
            .contains("already pinned")
    );
    // The same name is free in another space.
    pin(conn, "t", "ok", program, &session.snapshot()).unwrap();
    for n in 0..MAX_PINNED - 1 {
        pin(conn, "s", &format!("app-{n}"), program, &session.snapshot()).unwrap();
    }
    assert!(
        pin(conn, "s", "one-too-many", program, &session.snapshot())
            .unwrap_err()
            .to_string()
            .contains("pinned apps")
    );
}

#[test]
fn rename_and_unpin_by_name_or_id() {
    let db = Db::open_in_memory().unwrap();
    let conn = db.conn();
    let session = edited();
    let program = session.program().unwrap();
    let a = pin(conn, "s", "alpha", program, &session.snapshot()).unwrap();
    pin(conn, "s", "beta", program, &session.snapshot()).unwrap();

    assert!(rename(conn, "s", "alpha", "beta").is_err());
    assert!(rename(conn, "s", "alpha", "Bad Name").is_err());
    assert!(rename(conn, "s", "missing", "gamma").is_err());
    let renamed = rename(conn, "s", &a.id, "gamma").unwrap();
    assert_eq!(renamed.name, "gamma");
    assert_eq!(renamed.id, a.id);
    // Renaming to its own name is not a collision.
    rename(conn, "s", "gamma", "gamma").unwrap();
    assert!(find(conn, "s", "alpha").unwrap().is_none());

    unpin(conn, "s", "gamma").unwrap();
    assert!(find(conn, "s", "gamma").unwrap().is_none());
    assert!(unpin(conn, "s", "gamma").is_err());
    assert_eq!(list(conn, "s").unwrap().len(), 1);
    // The delete leaves a tombstone for other devices.
    let tombstones: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_tombstones WHERE table_name = 'pinned_panels' AND row_id = ?1",
            [&a.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tombstones, 1);
    // A different space cannot reach it.
    assert!(unpin(conn, "other", "beta").is_err());
}

#[test]
fn edit_mode_patch_targets_a_pinned_app_and_keeps_its_state() {
    let db = Db::open_in_memory().unwrap();
    let conn = db.conn();
    let session = edited();
    let app = pin(
        conn,
        "s",
        "budget",
        session.program().unwrap(),
        &session.snapshot(),
    )
    .unwrap();
    let patch = "root = Panel(\"Budget\", [Text(\"Spent: \" + $spent), Input($note, \"Note\"), Text(\"Limit: \" + $limit), Buttons([Button(\"Spend\", @Set($spent, $spent + 5))])])\n$limit = 100";

    let patched = super::patch(conn, "s", &app.id, patch).unwrap();
    assert!(patched.program.contains("$limit"));
    assert_eq!(patched.id, app.id);
    let reopened = find(conn, "s", "budget").unwrap().unwrap();
    assert_eq!(reopened.program, patched.program);
    let mut opened = Session::new(Document::parse(&reopened.program).unwrap()).unwrap();
    opened
        .restore(&PanelSnapshot::decode(&reopened.record).unwrap())
        .unwrap();
    assert_eq!(opened.value("note"), "groceries", "existing state survives");
    assert_eq!(opened.value("limit"), "100");

    // A bad patch is atomic, and the `edit` marker line is tolerated by the tool.
    let before = find(conn, "s", "budget").unwrap().unwrap();
    assert!(super::patch(conn, "s", "budget", "root = Panel(\"X\", [Unknown()])").is_err());
    assert_eq!(find(conn, "s", "budget").unwrap().unwrap(), before);
    assert!(super::patch(conn, "s", "missing", patch).is_err());
}

#[test]
fn the_model_tool_lists_reads_and_patches() {
    let path = temp_db_path();
    let db = Db::open(&path).unwrap();
    let session = edited();
    let app = pin(
        db.conn(),
        "s",
        "budget",
        session.program().unwrap(),
        &session.snapshot(),
    )
    .unwrap();

    let listed: serde_json::Value =
        serde_json::from_str(&tool(&path, "s", "list", &json!({})).unwrap()).unwrap();
    assert_eq!(listed[0]["name"], "budget");
    assert_eq!(listed[0]["id"], app.id.as_str());
    assert_eq!(listed[0]["title"], "Budget");
    assert_eq!(tool(&path, "other", "list", &json!({})).unwrap(), "[]");

    let read = tool(&path, "s", "read", &json!({"app": "budget"})).unwrap();
    assert!(read.contains("root = Panel"), "{read}");
    assert!(read.contains("groceries"), "{read}");

    let patch_text = "edit p1-1\n$limit = 7\nroot = Panel(\"Budget\", [Text(\"Spent: \" + $spent), Input($note, \"Note\"), Text(\"Limit: \" + $limit)])";
    let done = tool(
        &path,
        "s",
        "patch",
        &json!({"app": app.id, "patch": patch_text}),
    )
    .unwrap();
    assert!(done.starts_with("patched budget"), "{done}");
    let read = tool(&path, "s", "read", &json!({"app": "budget"})).unwrap();
    assert!(read.contains("$limit"), "{read}");

    assert!(tool(&path, "s", "patch", &json!({"app": "budget"})).is_err());
    assert!(tool(&path, "s", "read", &json!({"app": "nope"})).is_err());
    assert!(tool(&path, "s", "bogus", &json!({})).is_err());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn both_tables_sync_and_pinned_deletes_propagate() {
    use crate::space::Space;
    use crate::sync::{TABLES, apply_changeset, build_changeset};

    for table in ["panel_states", "pinned_panels"] {
        assert!(TABLES.iter().any(|spec| spec.name == table), "{table}");
    }
    let root = std::env::temp_dir().join(format!("nexus-panels-sync-{}", Uuid::new_v4()));
    let space = Space { root: root.clone() };
    let (a, b) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
    let session = edited();
    let app = pin(
        a.conn(),
        "s",
        "budget",
        session.program().unwrap(),
        &session.snapshot(),
    )
    .unwrap();
    save_state(
        a.conn(),
        "chat",
        "k-0",
        &session.snapshot().encode().unwrap(),
    )
    .unwrap();

    let cs = build_changeset(&a, None, "a").unwrap();
    let tables: Vec<_> = cs.rows.iter().map(|row| row.table.as_str()).collect();
    assert!(tables.contains(&"panel_states") && tables.contains(&"pinned_panels"));
    apply_changeset(&b, &space, &cs, None).unwrap();
    assert_eq!(find(b.conn(), "s", "budget").unwrap().unwrap(), app);
    assert_eq!(load_states(b.conn(), "chat").unwrap().len(), 1);

    // Last writer wins on a pinned app.
    let patched = super::patch(
        a.conn(),
        "s",
        "budget",
        "$limit = 3\nroot = Panel(\"Budget\", [Text($limit)])",
    )
    .unwrap();
    let cs = build_changeset(&a, None, "a").unwrap();
    apply_changeset(&b, &space, &cs, None).unwrap();
    assert_eq!(
        find(b.conn(), "s", "budget").unwrap().unwrap().program,
        patched.program
    );

    // Unpinning on A removes it from B.
    unpin(a.conn(), "s", "budget").unwrap();
    let cs = build_changeset(&a, None, "a").unwrap();
    apply_changeset(&b, &space, &cs, None).unwrap();
    assert!(find(b.conn(), "s", "budget").unwrap().is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn the_tool_is_offered_only_to_a_terminal_ui_and_patching_is_a_write() {
    use crate::tools::{FilesCtx, ToolBox, is_read_only_tool};

    let path = temp_db_path();
    let db = Db::open(&path).unwrap();
    let space = db.default_space_id().unwrap();
    let session = edited();
    pin(
        db.conn(),
        &space,
        "budget",
        session.program().unwrap(),
        &session.snapshot(),
    )
    .unwrap();
    let mut toolbox = ToolBox::new(
        path.with_file_name("skills"),
        None,
        None,
        "auto".into(),
        vec![],
        None,
        Some(FilesCtx {
            db_path: path.clone(),
            space_id: space,
            embedder: None,
        }),
        None,
    );
    assert!(!toolbox.defs().iter().any(|tool| tool.name == "panel_app"));
    assert_eq!(
        toolbox.run("panel_app", r#"{"action":"list"}"#).await.1,
        "blocked"
    );
    toolbox.interactive_ui = true;
    let def = toolbox
        .defs()
        .into_iter()
        .find(|tool| tool.name == "panel_app")
        .expect("offered to the terminal UI");
    assert_eq!(
        def.parameters["properties"]["action"]["enum"],
        json!(["list", "read", "patch"])
    );
    let (text, _) = toolbox.run("panel_app", r#"{"action":"list"}"#).await;
    assert!(text.contains("budget"), "{text}");
    let patch = json!({"action":"patch","app":"budget","patch":"$limit = 9\nroot = Panel(\"Budget\", [Text($limit)])"});
    let (text, _) = toolbox.run("panel_app", &patch.to_string()).await;
    assert!(text.starts_with("patched budget"), "{text}");
    let (text, _) = toolbox
        .run("panel_app", r#"{"action":"patch","app":"budget"}"#)
        .await;
    assert!(text.contains("required"), "{text}");

    assert!(is_read_only_tool("panel_app", r#"{"action":"list"}"#));
    assert!(is_read_only_tool(
        "panel_app",
        r#"{"action":"read","app":"x"}"#
    ));
    assert!(!is_read_only_tool("panel_app", &patch.to_string()));
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
