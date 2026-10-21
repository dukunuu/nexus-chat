use super::*;
use nexus_core::app::App;
use nexus_core::db::Db;
use nexus_core::space::Space;

const SOURCE: &str = r#"
    fn init() { #{days:5, ready:false, saved:0} }
    fn update(s,e) {
        if e.type == "input" { s[e.key] = e.value; }
        if e.type == "button" { s.saved += 1; }
        s
    }
    fn view(s) { #{title:"Trip", view:[
        #{type:"toggle",key:"ready",label:"Ready"},
        #{type:"stepper",key:"days",label:"Days",min:1,max:7,step:2},
        #{type:"buttons",items:[#{label:"Save",action:"save"}]},
        #{type:"text",text:"Saved {{saved}}"}
    ]} }
"#;

fn draw(app: &mut AppView) {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
}

fn value(app: &AppView, key: &str) -> String {
    app.tui_app
        .as_ref()
        .unwrap()
        .view
        .as_ref()
        .unwrap()
        .session
        .value(key)
}

#[test]
fn rhai_stepper_keys_clicks_and_adjacent_controls_share_numeric_state() {
    let root = std::env::temp_dir().join(format!("nexus-stepper-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut app = AppView::new(App::new(
        Db::open_in_memory().unwrap(),
        Some("test"),
        Space { root: root.clone() },
    ));
    app.tui_app = Some(Box::new(TuiApp::new(Ok(SOURCE.into()))));
    app.popup = Popup::TuiApp;
    draw(&mut app);
    handle_key(&mut app, KeyCode::Enter.into());
    handle_key(&mut app, KeyCode::Tab.into());
    handle_key(&mut app, KeyCode::Left.into());
    draw(&mut app);
    assert_eq!(value(&app, "days"), "3");
    assert_eq!(value(&app, "ready"), "true");
    handle_key(&mut app, KeyCode::Right.into());
    draw(&mut app);
    assert_eq!(value(&app, "days"), "5");
    for (offset, expected) in [(10, "7"), (10, "7"), (6, "5"), (0, "5")] {
        let rect = app
            .tui_app
            .as_ref()
            .unwrap()
            .hits
            .iter()
            .find(|(_, control)| *control == 1)
            .unwrap()
            .0;
        mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + offset,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        draw(&mut app);
        assert_eq!(value(&app, "days"), expected);
    }
    handle_key(&mut app, KeyCode::Tab.into());
    handle_key(&mut app, KeyCode::Enter.into());
    draw(&mut app);
    assert_eq!(value(&app, "saved"), "1");
    assert!(app.tui_app.as_ref().unwrap().error.is_empty());
    let runner = Runner::new(SOURCE).unwrap();
    let rendered = preview(&runner.view(80).unwrap(), 80, 16).unwrap();
    assert!(rendered.contains("Days  ◀ 5 ▶"));
    std::fs::remove_dir_all(root).unwrap();
}
