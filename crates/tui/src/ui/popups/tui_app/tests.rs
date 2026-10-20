use super::*;
use nexus_core::app::App;
use nexus_core::db::Db;
use nexus_core::space::Space;
use nexus_core::tools::ToolBox;

struct Fixture {
    app: AppView,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("nexus-tui-app-view-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let app = AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("test"),
            Space { root: root.clone() },
        ));
        Self { app, root }
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
        toolbox.tui_app_renderer = Some(preview);
        toolbox.pending_tui_app = self.app.core.pending_tui_app.clone();
        toolbox.space_tui_apps_dir = self
            .app
            .space
            .scripts_dir(&self.app.active_space.name)
            .with_file_name("tui_apps");
        toolbox
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn snapshot(app: &mut AppView) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 26)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .chunks(80)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

#[tokio::test]
async fn preview_tool_renders_grid_events_and_errors() {
    let fixture = Fixture::new();
    let toolbox = fixture.toolbox();
    let source = r#"fn init() { #{n:0} } fn update(s,e) { s.n += 1; s } fn view(s) { #{title:"Tiny", view:[#{type:"text",text:"Value {{n}}"}]} }"#;
    toolbox
        .run(
            "tui_app",
            &json!({"action":"write","name":"tiny","content":source}).to_string(),
        )
        .await;
    let result = toolbox
        .run(
            "tui_app",
            &json!({"action":"preview","name":"tiny","width":20,"height":6}).to_string(),
        )
        .await
        .0;
    assert_eq!(
        result,
        "Tiny                \n                    \nValue 0             \n                    \n                    \n                    \nLog:\n"
    );
    let result = toolbox.run("tui_app", &json!({"action":"preview","name":"tiny","events":[{"type":"key","key":"x"},{"type":"button","action":"add"}]}).to_string()).await.0;
    assert!(result.contains("Value 2"), "{result}");
    let bad = source.replace("s.n += 1;", "print(\"before failure\"); throw \"broken\";");
    toolbox
        .run(
            "tui_app",
            &json!({"action":"write","name":"tiny","content":bad}).to_string(),
        )
        .await;
    let result = toolbox
        .run(
            "tui_app",
            &json!({"action":"preview","name":"tiny","events":[{"type":"key","key":"x"}]})
                .to_string(),
        )
        .await
        .0;
    assert!(
        result.contains("broken") && result.contains("before failure"),
        "{result}"
    );
}

#[tokio::test]
async fn rps_preview_scores_canvas_and_popup_snapshot() {
    let mut fixture = Fixture::new();
    let toolbox = fixture.toolbox();
    toolbox
        .run("tui_app", r#"{"action":"open","name":"rps"}"#)
        .await;
    take_pending(&mut fixture.app);
    assert_eq!(fixture.app.popup, Popup::TuiApp);
    let result = toolbox.run("tui_app", &json!({"action":"preview","name":"rps","events":vec![json!({"type":"button","action":"rock"});3]}).to_string()).await.0;
    for text in [
        "Wins   1",
        "Losses 1",
        "Draws  1",
        "Round  3",
        " R     S",
        "Rock (r)",
        "Scissors (s)",
    ] {
        assert!(result.contains(text), "missing {text}:\n{result}");
    }
    let expected = "\n\n\n         ╭terminal app────────────────────────────────────────────────╮
         │ Rock · Paper · Scissors                                    │
         │                                                            │
         │ ╭───▸ Rock (r)────╮ ╭───Paper (p)────╮ ╭──Scissors (s)───╮ │
         │ │                 │ │                │ │                 │ │
         │ ╰─────────────────╯ ╰────────────────╯ ╰─────────────────╯ │
         │ Wins   0                                                   │
         │ Losses 0                                                   │
         │ Draws  0                                                   │
         │ Round  0                                                   │
         │ Choose a hand                                              │
         │ You / CPU                                                  │
         │  ?     ?                                                   │
         │ [ ]   [ ]                                                  │
         │ ╭─────────────────────────Reset──────────────────────────╮ │
         │ │                                                        │ │
         │ ╰────────────────────────────────────────────────────────╯ │
         │                                                            │
         │                                                            │
         ╰──────────────────── Tab focus · Enter activate · Esc close ╯";
    assert_eq!(snapshot(&mut fixture.app), expected);
}

#[test]
fn key_routing_reset_escape_and_script_error_panel() {
    let mut fixture = Fixture::new();
    fixture.app.run_command("tuiapp rps").unwrap();
    snapshot(&mut fixture.app);
    for code in [KeyCode::Char('r'), KeyCode::Char('r'), KeyCode::Char('r')] {
        handle_key(&mut fixture.app, KeyEvent::from(code));
    }
    let rendered = snapshot(&mut fixture.app);
    assert!(rendered.contains("Round  3"), "{rendered}");
    for _ in 0..3 {
        handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Tab));
    }
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Enter));
    assert!(snapshot(&mut fixture.app).contains("Round  0"));
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(fixture.app.popup, Popup::None);
    assert!(fixture.app.tui_app.is_none());
    fixture.app.tui_app = Some(Box::new(TuiApp::new(Ok(
        "fn init() { #{} } fn view(s) { throw \"broken view\"; }".into(),
    ))));
    fixture.app.popup = Popup::TuiApp;
    let rendered = snapshot(&mut fixture.app);
    assert!(
        rendered.contains("Script error") && rendered.contains("broken view"),
        "{rendered}"
    );
    assert!(deadline(&fixture.app).is_none());
}

#[test]
fn input_controls_ticks_mouse_and_approval_queue() {
    let mut fixture = Fixture::new();
    let source = r#"
        fn init() { #{name:"", enabled:false, choice:"a", row:0, tick_ms:100, ticks:0} }
        fn update(s,e) { if e.type == "input" { s[e.key] = e.value; } if e.type == "tick" { s.ticks += 1; } s }
        fn view(s) { #{ title:"Controls", view:[
            #{type:"input",key:"name",label:"Name"},
            #{type:"toggle",key:"enabled",label:"Enabled"},
            #{type:"select",key:"choice",label:"Choice",options:["a","b"]},
            #{type:"table",columns:["N"],rows_data:[["one"],["two"]],select:"row"},
            #{type:"text",text:"Ticks {{ticks}}"}
        ]} }
    "#;
    fixture.app.tui_app = Some(Box::new(TuiApp::new(Ok(source.into()))));
    fixture.app.popup = Popup::TuiApp;
    snapshot(&mut fixture.app);
    assert!(deadline(&fixture.app).is_some());
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Char('A')));
    paste(&mut fixture.app, "B");
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Tab));
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Char(' ')));
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Tab));
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Right));
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Tab));
    handle_key(&mut fixture.app, KeyEvent::from(KeyCode::Down));
    tick(&mut fixture.app);
    let rendered = snapshot(&mut fixture.app);
    assert!(
        rendered.contains("AB")
            && rendered.contains("Choice: < b >")
            && rendered.contains("Ticks 1"),
        "{rendered}"
    );
    let instance = fixture.app.tui_app.as_ref().unwrap();
    let view = instance.view.as_ref().unwrap();
    assert_eq!(view.session.value("enabled"), "true");
    assert_eq!(view.session.selected_row("row"), 1);
    let rect = instance
        .hits
        .iter()
        .find(|(_, index)| *index == 1)
        .unwrap()
        .0;
    mouse(
        &mut fixture.app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    snapshot(&mut fixture.app);
    assert_eq!(
        fixture
            .app
            .tui_app
            .as_ref()
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .session
            .value("enabled"),
        "false"
    );
    fixture.app.popup = Popup::Approval;
    *fixture.app.core.pending_tui_app.lock().unwrap() = Some("rps".into());
    take_pending(&mut fixture.app);
    assert_eq!(fixture.app.popup, Popup::Approval);
    assert!(fixture.app.core.pending_tui_app.lock().unwrap().is_some());
    fixture.app.popup = Popup::None;
    take_pending(&mut fixture.app);
    assert_eq!(fixture.app.popup, Popup::TuiApp);
}
