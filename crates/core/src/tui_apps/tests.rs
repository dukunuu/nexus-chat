use super::*;
use crate::tools::{ToolBox, is_read_only_tool};

const TINY: &str = r#"
fn init() { #{ count: 0, input: "", active: false, choice: "a", row: 0, internal: [1,2] } }
fn update(s, e) {
    if e.type == "button" { s.count += 1; }
    if e.type == "input" { s[e.key] = e.value; }
    if e.type == "tick" { s.count += 10; }
    s
}
fn view(s) { #{ title: "Counter", view: [
    #{ type: "text", text: "Count {{count}}" },
    #{ type: "buttons", items: [#{label:"Add", action:"add"}] },
    #{ type: "input", key:"input", label:"Name" },
    #{ type: "toggle", key:"active", label:"Active" },
    #{ type: "select", key:"choice", label:"Choice", options:["a", "b"] },
    #{ type: "table", columns:["N"], rows_data:[["one"],["two"]], select:"row" }
] } }
"#;

#[test]
fn contract_round_trip_and_scalar_bindings() {
    let mut runner = Runner::new(TINY).unwrap();
    runner
        .update(&json!({"type":"button", "action":"add"}))
        .unwrap();
    runner
        .update(&json!({"type":"input", "key":"input", "value":"hello"}))
        .unwrap();
    let view = runner.view(60).unwrap();
    assert_eq!(view.session.text("Count {{count}}").unwrap(), "Count 1");
    assert_eq!(view.session.value("input"), "hello");
    assert!(runner.state.contains_key("internal"));
    assert!(view.session.text("{{internal}}").is_err());
    for source in [
        TINY.replace("Count {{count}}", "{{missing}}"),
        TINY.replace("key:\"active\"", "key:\"count\""),
        TINY.replace("type: \"text\"", "type: \"unknown\""),
    ] {
        assert!(Runner::new(&source).unwrap().view(60).is_err());
    }
    assert!(Runner::new("fn init() { 1 }").is_err());
    assert!(
        Runner::new("fn init() { #{} } fn update(s,e) { 1 }")
            .unwrap()
            .update(&json!({"type":"tick"}))
            .is_ok()
    );
    let mut bad = Runner::new("fn init() { #{} } fn update(s,e) { 1 }").unwrap();
    assert!(bad.update(&json!({"type":"key", "key":"a"})).is_err());
}

#[test]
fn ticks_are_explicit_and_bounded_below() {
    for (field, expected) in [
        ("", 0),
        ("tick_ms: 99,", 0),
        ("tick_ms: 100,", 10),
        ("tick_ms: \"100\",", 0),
    ] {
        let source = TINY.replace("count: 0,", &format!("{field} count: 0,"));
        let mut runner = Runner::new(&source).unwrap();
        runner.update(&json!({"type":"tick"})).unwrap();
        assert_eq!(
            runner.view(60).unwrap().session.value("count"),
            expected.to_string()
        );
    }
    for event in [
        json!({"type":"key", "key":"esc"}),
        json!({"type":"file"}),
        json!({"type":"tick", "path":"secret"}),
        json!({"type":"input", "key":"x", "value":"\u{1b}"}),
    ] {
        assert!(validate_event(&event).is_err());
    }
}

fn canvas_source(canvas: Value) -> String {
    format!(
        "fn init() {{ #{{}} }} fn view(s) {{ {} }}",
        json!({"title":"Canvas", "view":[canvas]})
            .to_string()
            .replace('{', "#{")
    )
}

#[test]
fn canvas_validation_and_display_budget() {
    let good =
        json!({"type":"canvas", "rows":16, "cells":[[0,0,"X","accent"],[59,15,"!","error"]]});
    let runner = Runner::new(&canvas_source(good.clone())).unwrap();
    assert_eq!(runner.view(60).unwrap().rows(0), 16);
    assert!(runner.view(59).is_err());
    for cell in [
        json!([60, 0, "x", "normal"]),
        json!([0, 16, "x", "normal"]),
        json!([-1, 0, "x", "normal"]),
        json!([0, 0, "xx", "normal"]),
        json!([0, 0, "\n", "normal"]),
        json!([0, 0, "\u{1b}", "normal"]),
        json!([0, 0, "\u{200b}", "normal"]),
        json!([0, 0, "\u{301}", "normal"]),
        json!([0, 0, "x", "invalid"]),
    ] {
        let mut canvas = good.clone();
        canvas["cells"] = json!([cell]);
        assert!(
            Runner::new(&canvas_source(canvas))
                .unwrap()
                .view(60)
                .is_err()
        );
    }
    for rows in [0, 17] {
        let mut canvas = good.clone();
        canvas["rows"] = json!(rows);
        assert!(
            Runner::new(&canvas_source(canvas))
                .unwrap()
                .view(60)
                .is_err()
        );
    }
}

#[test]
fn sandbox_limits_and_no_host_facilities() {
    let started = std::time::Instant::now();
    let error = Runner::new("fn init() { loop {} }")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("Too many operations"), "{error}");
    for source in [
        "fn f(n) { f(n+1) } fn init() { f(0) }",
        "fn init() { let s = \"x\"; loop { s += s; } }",
        "fn init() { let a = []; loop { a.push(0); } }",
        "fn init() { let m = #{}; for n in 0..257 { m[n.to_string()] = n; } m }",
        "fn init() { eval(\"40+2\") }",
        "import \"/etc/passwd\" as file; fn init() { #{} }",
        "fn init() { read_file(\"/etc/passwd\") }",
        "fn init() { getenv(\"HOME\") }",
        "fn init() { exec(\"echo unsafe\") }",
    ] {
        assert!(Runner::new(source).is_err(), "accepted: {source}");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let mut runner = Runner::new("fn init() { #{} } fn update(s,e) { loop {} }").unwrap();
    assert!(runner.update(&json!({"type":"key", "key":"x"})).is_err());
    let runner = Runner::new("fn init() { #{} } fn view(s) { loop {} }").unwrap();
    assert!(runner.view(60).is_err());
    let source = format!(
        "fn init() {{ #{{n: {}1{}}} }}",
        "(".repeat(65),
        ")".repeat(65)
    );
    assert!(Runner::new(&source).is_err());
}

#[test]
fn print_and_debug_are_captured_and_capped() {
    let runner = Runner::new("fn init() { print(\"hello\"); debug(\"world\"); #{x:0} }").unwrap();
    assert!(runner.log().contains("hello"));
    assert!(runner.log().contains("world"));
    let runner =
        Runner::new("fn init() { for n in 0..2000 { print(\"0123456789\"); } #{} }").unwrap();
    assert!(runner.log().len() <= 16 * 1024);
}

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexus-tui-app-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn toolbox(&self) -> ToolBox {
        let mut toolbox = ToolBox::new(
            self.root.join("skills"),
            None,
            None,
            "auto".into(),
            vec![],
            None,
            None,
            None,
        );
        toolbox.interactive_ui = true;
        toolbox.space_tui_apps_dir = self.root.join("tui_apps");
        toolbox
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn tool_files_hash_edits_names_and_classification() {
    let fixture = Fixture::new();
    let mut toolbox = fixture.toolbox();
    assert!(toolbox.defs().iter().any(|tool| tool.name == "tui_app"));
    toolbox.interactive_ui = false;
    assert!(!toolbox.defs().iter().any(|tool| tool.name == "tui_app"));
    assert_eq!(
        toolbox.run("tui_app", r#"{"action":"list"}"#).await.1,
        "blocked"
    );
    toolbox.interactive_ui = true;
    let call = |action, extra: Value| {
        let mut args = extra;
        args["action"] = json!(action);
        args.to_string()
    };
    let result = toolbox
        .run(
            "tui_app",
            &call(
                "write",
                json!({"name":"counter", "content":"first\nsecond\n"}),
            ),
        )
        .await
        .0;
    assert!(result.starts_with("wrote"), "{result}");
    let read = toolbox
        .run("tui_app", &call("read", json!({"name":"counter"})))
        .await
        .0;
    let hash = read
        .lines()
        .next()
        .unwrap()
        .split_once(':')
        .unwrap()
        .1
        .split_once('\t')
        .unwrap()
        .0;
    let edited = toolbox
        .run(
            "tui_app",
            &call(
                "edit",
                json!({"name":"counter", "edits":[{"hash":hash,"new":"changed"}]}),
            ),
        )
        .await
        .0;
    assert!(edited.starts_with("edited"), "{edited}");
    let read = toolbox
        .run("tui_app", &call("read", json!({"name":"counter"})))
        .await
        .0;
    assert!(read.contains("changed") && read.contains("second"));
    assert_eq!(
        toolbox.run("tui_app", &call("list", json!({}))).await.0,
        "[\"counter\"]"
    );
    for name in ["", "../escape", "a/b", "UPPER", "a_b", &"a".repeat(33)] {
        for action in ["read", "write", "edit", "preview", "open"] {
            let result = toolbox
                .run(
                    "tui_app",
                    &call(
                        action,
                        json!({"name":name,"content":"bad","edits":[{"hash":"h"}]}),
                    ),
                )
                .await
                .0;
            assert!(
                !result.starts_with("wrote") && !result.starts_with("opening"),
                "{result}"
            );
        }
        assert!(!valid_name(name));
    }
    for action in ["list", "read", "preview", "write", "edit", "open"] {
        assert_eq!(
            is_read_only_tool("tui_app", &call(action, json!({}))),
            ["list", "read", "preview"].contains(&action)
        );
    }
    let definition = toolbox
        .defs()
        .into_iter()
        .find(|def| def.name == "tui_app")
        .unwrap();
    assert!(definition.description.split_whitespace().count() <= 120);
}

#[cfg(unix)]
#[tokio::test]
async fn tool_rejects_symlinks_outside_the_space() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let toolbox = fixture.toolbox();
    std::fs::create_dir_all(toolbox.space_tui_apps_dir.join("evil")).unwrap();
    std::fs::write(outside.root.join("app.rhai"), "secret").unwrap();
    symlink(
        outside.root.join("app.rhai"),
        toolbox.space_tui_apps_dir.join("evil/app.rhai"),
    )
    .unwrap();
    for action in ["read", "write", "edit", "preview", "open", "list"] {
        let args =
            json!({"action":action,"name":"evil","content":"overwrite","edits":[{"hash":"x"}]})
                .to_string();
        let result = toolbox.run("tui_app", &args).await.0;
        assert!(result.contains("escapes"), "{result}");
    }
    assert_eq!(
        std::fs::read_to_string(outside.root.join("app.rhai")).unwrap(),
        "secret"
    );
    std::fs::remove_dir_all(&toolbox.space_tui_apps_dir).unwrap();
    symlink(&outside.root, &toolbox.space_tui_apps_dir).unwrap();
    assert!(app_path(&toolbox.space_tui_apps_dir, "test").is_err());
}

#[tokio::test]
async fn opening_installs_the_bundled_example_without_overwriting() {
    let fixture = Fixture::new();
    let toolbox = fixture.toolbox();
    let args = r#"{"action":"open","name":"rps"}"#;
    assert_eq!(toolbox.run("tui_app", args).await.0, "opening rps");
    assert_eq!(*toolbox.pending_tui_app.lock().unwrap(), Some("rps".into()));
    let path = toolbox.space_tui_apps_dir.join("rps/app.rhai");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), RPS);
    std::fs::write(&path, TINY).unwrap();
    toolbox.run("tui_app", args).await;
    assert_eq!(std::fs::read_to_string(path).unwrap(), TINY);
    let mut runner = Runner::new(RPS).unwrap();
    for _ in 0..3 {
        runner
            .update(&json!({"type":"button", "action":"rock"}))
            .unwrap();
    }
    let view = runner.view(60).unwrap();
    for key in ["wins", "losses", "draws"] {
        assert_eq!(view.session.value(key), "1");
    }
    assert_eq!(view.session.value("rounds"), "3");
}
