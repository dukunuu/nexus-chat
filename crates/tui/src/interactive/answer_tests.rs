use super::*;
use nexus_core::app::AppEvent;
use nexus_core::provider::{BackendTag, openrouter::OpenRouter};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

fn quiz(label: &str) -> Value {
    json!({
        "version": 1, "title": "Capital?",
        "state": {"city": "Paris", "picked": false},
        "view": [{"type":"buttons","items":[
            {"label":label,"action":"pick"},
            {"label":"Next question","action":"pick"}
        ]}],
        "actions": {
            "pick":[
                {"op":"set","key":"picked","value":true},
                {"op":"answer"},
                {"op":"set","key":"city","value":"Rome"}
            ],
            "review":[{"op":"send","text":"{{city}}"}],
            "local":[{"op":"set","key":"picked","value":false}]
        }
    })
}

fn app(data: &Value) -> AppView {
    let mut app = AppView::new(nexus_core::app::App::new(
        nexus_core::db::Db::open_in_memory().unwrap(),
        None,
        nexus_core::space::Space {
            root: std::env::temp_dir().join(format!("nexus-answer-test-{}", uuid::Uuid::new_v4())),
        },
    ));
    app.backends.set(
        BackendTag::Local,
        OpenRouter::local("http://127.0.0.1:9/v1").unwrap(),
    );
    app.current_model = Some("answer-test".into());
    app.messages.push(nexus_core::db::Message {
        role: "assistant".into(),
        content: format!("```nexus-ui\n{data}\n```"),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    app
}

fn draw(app: &mut AppView) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal.draw(|f| crate::ui::render(f, app)).unwrap();
    terminal
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn click_answer(app: &mut AppView) {
    let hit = app
        .interactive
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .clone();
    assert!(mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.rect.x,
            row: hit.rect.y,
            modifiers: KeyModifiers::NONE,
        },
        Rect::new(0, 0, 80, 32),
    ));
}

fn drain_events(app: &mut AppView) {
    while let Some(event) = app.pop_pending_event() {
        assert!(!matches!(
            event,
            AppEvent::ComposerSet(_) | AppEvent::ComposerClear
        ));
        app.apply_event(&event);
    }
}

fn abort_requests(app: &mut AppView) {
    // These current-thread tests never yield, so provider tasks cannot run.
    for task in app.chat_tasks.values() {
        task.abort.abort();
    }
    app.chat_tasks.clear();
}

#[tokio::test(flavor = "current_thread")]
async fn clicking_answers_sends_the_visible_label_runs_steps_and_preserves_drafts() {
    let mut app = app(&quiz("  B — {{city}}  "));
    app.set_input("existing draft");
    let terminal = draw(&mut app);
    assert!(text(&terminal).contains("B — Paris   →"));
    click_answer(&mut app);
    assert_eq!(app.messages.last().unwrap().role, "user");
    assert_eq!(app.messages.last().unwrap().content, "  B — Paris  ");
    assert_eq!(app.input_text(), "existing draft");
    assert_eq!(app.popup, Popup::None);
    let session = app
        .interactive
        .entries
        .values()
        .next()
        .unwrap()
        .session
        .as_ref()
        .unwrap();
    assert_eq!(session.value("picked"), "true");
    assert_eq!(session.value("city"), "Rome");
    let saved = app
        .db
        .load_messages(&app.session.as_ref().unwrap().id)
        .unwrap();
    assert_eq!(saved.last().unwrap().content, "  B — Paris  ");
    drain_events(&mut app);
    assert_eq!(app.input_text(), "existing draft");
    abort_requests(&mut app);
}

#[tokio::test(flavor = "current_thread")]
async fn keyboard_answers_are_plain_user_messages_not_commands() {
    for label in ["/quit", "!echo not-a-command", "Next question"] {
        let mut app = app(&quiz(label));
        draw(&mut app);
        focus(&mut app);
        key(&mut app, KeyCode::Enter.into());
        assert_eq!(app.messages.last().unwrap().role, "user");
        assert_eq!(app.messages.last().unwrap().content, label);
        assert!(!app.should_quit);
        drain_events(&mut app);
        abort_requests(&mut app);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_answers_only_show_the_wait_status() {
    let mut app = app(&quiz("B — {{city}}"));
    app.core.send_message("previous message".into()).unwrap();
    app.set_input("keep this draft");
    drain_events(&mut app);
    draw(&mut app);
    let before = app.messages.len();
    click_answer(&mut app);
    assert_eq!(app.messages.len(), before);
    assert_eq!(app.chat_task_count(), 1);
    assert_eq!(app.input_text(), "keep this draft");
    let session = app
        .interactive
        .entries
        .values()
        .next()
        .unwrap()
        .session
        .as_ref()
        .unwrap();
    assert_eq!(session.value("picked"), "false");
    drain_events(&mut app);
    assert_eq!(app.status, "wait for the reply to finish, then pick again");
    focus(&mut app);
    key(&mut app, KeyCode::Enter.into());
    drain_events(&mut app);
    assert_eq!(app.status, "wait for the reply to finish, then pick again");
    assert_eq!(app.messages.len(), before);
    assert_eq!(app.input_text(), "keep this draft");
    abort_requests(&mut app);
}

#[test]
fn invalid_rendered_labels_and_send_preconditions_never_restore_over_a_draft() {
    for city in ["two\nlines".to_string(), "é".repeat(61)] {
        let mut data = quiz("{{city}}");
        data["state"]["city"] = json!(city);
        let mut app = app(&data);
        app.set_input("keep this draft");
        draw(&mut app);
        click_answer(&mut app);
        assert_eq!(app.messages.len(), 1);
        drain_events(&mut app);
        assert_eq!(app.input_text(), "keep this draft");
    }
    for no_model in [false, true] {
        let mut app = app(&quiz("B — Paris"));
        if no_model {
            app.current_model = None;
        } else {
            app.backends = nexus_core::app::Backends::default();
        }
        app.set_input("keep this draft");
        draw(&mut app);
        click_answer(&mut app);
        assert_eq!(app.messages.len(), 1);
        drain_events(&mut app);
        assert_eq!(app.input_text(), "keep this draft");
    }
}

#[test]
fn answer_suffixes_and_primary_resolution_distinguish_review_and_equal_options() {
    let mut data = quiz("B — {{city}}");
    let document = Document::parse(&data.to_string()).unwrap();
    assert_eq!(primary_button(&document), None);
    let session = Session::new(document).unwrap();
    let Node::Buttons { items, .. } = &session.document().nodes()[0] else {
        panic!("buttons");
    };
    assert_eq!(button_label(&session, &items[0]), "B — Paris →");
    data["view"][0]["items"][1]["primary"] = json!(true);
    assert_eq!(
        primary_button(&Document::parse(&data.to_string()).unwrap()),
        Some((0, 1))
    );
    data["view"][0]["items"][1]["primary"] = json!(false);
    data["view"][0]["items"][1]["action"] = json!("local");
    assert_eq!(
        primary_button(&Document::parse(&data.to_string()).unwrap()),
        Some((0, 0))
    );
    data["view"].as_array_mut().unwrap().push(json!({
        "type":"buttons","items":[{"label":"Review","action":"review"}]
    }));
    data["view"][0]["items"][1]["action"] = json!("pick");
    let document = Document::parse(&data.to_string()).unwrap();
    assert_eq!(primary_button(&document), Some((1, 0)));
    let session = Session::new(document).unwrap();
    let Node::Buttons { items, .. } = &session.document().nodes()[1] else {
        panic!("buttons");
    };
    assert_eq!(button_label(&session, &items[0]), "Review ↵");
    assert!(text(&draw(&mut app(&data))).contains("Review ↵"));
}
