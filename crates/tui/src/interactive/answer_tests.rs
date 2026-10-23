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
    assert!(text(&terminal).contains("1  B — Paris"));
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

fn nook() -> Value {
    let mut data = quiz("I want to tell you about yesterday's warmest moment.");
    data["title"] = json!("A little memory nook");
    data["view"] = json!([
        {"type":"hero","title":"A little memory nook",
         "subtitle":"Pick one thread, and we'll follow it gently."},
        {"type":"buttons","items":[
            {"label":"I want to tell you about yesterday's warmest moment.","action":"pick"},
            {"label":"I want to tell you about dinner with her brother.","action":"pick"},
            {"label":"I want to tell you about our growing closeness.","action":"pick"}
        ]}
    ]);
    data
}

fn entry(data: &Value) -> Entry {
    Entry {
        session: Ok(Session::new(Document::parse(&data.to_string()).unwrap()).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    }
}

fn rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

#[test]
fn choice_list_card_at_80_columns_has_chips_labels_and_right_edge_arrows() {
    let theme = Theme::default();
    let (canvas, hits) = entry(&nook()).canvas(80, &theme, false, true);
    let frame = |text: &str| format!("│{text:<78}│");
    let expected = [
        format!("╭{} ⤢ ╮", "─".repeat(75)),
        frame(""),
        frame(" A little memory nook"),
        frame(" Pick one thread, and we'll follow it gently."),
        frame(""),
        format!(
            "│ {:<75}→ │",
            " 1  I want to tell you about yesterday's warmest moment."
        ),
        format!(
            "│ {:<75}→ │",
            " 2  I want to tell you about dinner with her brother."
        ),
        format!(
            "│ {:<75}→ │",
            " 3  I want to tell you about our growing closeness."
        ),
        frame(""),
        format!("╰{} 1–3 pick · F2 keyboard ╯", "─".repeat(54)),
    ];
    assert_eq!(rows(&canvas), expected);
    assert_eq!(canvas[(2, 2)].fg, theme.accent2);
    assert!(canvas[(2, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(canvas[(2, 3)].fg, theme.fg_dim);
    for (item, y) in (5..8).enumerate() {
        assert_eq!(hits[item], (Rect::new(2, y, 76, 1), item));
        assert_eq!(canvas[(3, y)].bg, theme.control);
        assert_ne!(canvas[(3, y)].bg, theme.bg);
        assert_eq!(canvas[(6, y)].fg, theme.fg);
        assert_eq!(canvas[(77, y)].fg, theme.fg_dim);
    }
    let (focused, _) = entry(&nook()).canvas(80, &theme, true, true);
    assert_eq!(focused[(6, 5)].fg, theme.accent);
    for x in 5..78 {
        assert_eq!(focused[(x, 5)].bg, theme.selection);
    }
    assert_eq!(focused[(6, 6)].bg, theme.bg);
    assert!(rows(&focused).last().unwrap().contains("1–3 pick"));
}

#[test]
fn long_choices_wrap_under_the_label_and_reserve_exactly_the_drawn_height() {
    let mut data = nook();
    data["view"][1]["items"][1]["label"] = json!(
        "I want to tell you about dinner with her brother and the long walk home afterwards."
    );
    for width in [60, 80, 120] {
        let source = data.to_string();
        let rendered = crate::ui::markdown::render(
            &format!("```nexus-ui\n{source}\n```"),
            usize::from(width),
            crate::ui::markdown::MdColors::default(),
        );
        let (canvas, hits) = entry(&data).canvas(width, &Theme::default(), false, true);
        assert_eq!(rendered.widgets[0].height, canvas.area.height);
        assert_eq!(rendered.lines.len(), usize::from(canvas.area.height));
        assert_eq!(source_height(&source, width), canvas.area.height);
        assert_eq!(hits[1].0.width, width - 4);
        assert_eq!(hits[1].0.x, 2);
        if width == 80 {
            assert_eq!(hits[1].0, Rect::new(2, 6, 76, 2));
            let text = rows(&canvas);
            assert!(text[6].starts_with("│  2  I want to tell you about dinner"));
            assert!(text[7].starts_with("│     home afterwards."));
            assert_eq!(canvas[(77, 6)].symbol(), "→");
            assert_eq!(canvas[(77, 7)].symbol(), " ");
            assert_eq!(hits[2].0.y, 8);
        }
    }
}

#[test]
fn choice_numbers_stop_at_nine_across_groups_and_labels_are_escaped() {
    let mut data = quiz("Option");
    let items: Vec<_> = (1..=8)
        .map(|i| json!({"label":format!("Option {i}"),"action":"pick"}))
        .collect();
    data["view"] = json!([
        {"type":"buttons","items":items},
        {"type":"buttons","items":[
            {"label":"Nine\u{fe0f}","action":"pick"},
            {"label":"Ten","action":"pick"}
        ]}
    ]);
    let mut entry = entry(&data);
    let (canvas, hits) = entry.canvas(80, &Theme::default(), false, true);
    let text = rows(&canvas);
    assert!(text[usize::from(hits[8].0.y)].starts_with("│  9  Nine"));
    assert!(text.iter().all(|row| !row.contains('\u{fe0f}')));
    assert!(text[usize::from(hits[9].0.y)].starts_with("│     Ten"));
    assert!(text.last().unwrap().contains("1–9 pick"));
    assert_eq!(entry.choices().len(), 10);
    entry.focus = 9;
    let mut widgets = Widgets::default();
    let key = Key {
        owner: 0,
        ordinal: 0,
        source: data.to_string(),
    };
    widgets.entries.insert(key.clone(), entry);
    widgets.focused = Some(key);
    assert_eq!(
        widgets.key(KeyCode::Char('9').into(), true).1,
        Some("Nine\u{fe0f}".into())
    );
}

#[test]
fn answer_buttons_with_bodies_keep_the_tile_layout() {
    let mut data = quiz("Tile");
    data["view"][0]["items"][0]["body"] = json!(["Description"]);
    let entry = entry(&data);
    assert!(entry.choices().is_empty());
    let (canvas, hits) = entry.canvas(80, &Theme::default(), false, true);
    assert!(rows(&canvas).iter().any(|row| row.contains("┌")));
    assert!(hits[0].0.height > 1);
    assert!(rows(&canvas).last().unwrap().contains("F2 keyboard"));
    assert!(!rows(&canvas).last().unwrap().contains("pick"));
}

#[tokio::test(flavor = "current_thread")]
async fn numbered_and_arrow_key_choices_use_the_same_guarded_send_path() {
    for codes in [
        vec![KeyCode::Char('2')],
        vec![KeyCode::Down, KeyCode::Enter],
        vec![KeyCode::Up, KeyCode::Enter],
    ] {
        let mut app = app(&quiz("First"));
        app.set_input("keep my draft");
        draw(&mut app);
        // Digits have no effect until the card owns keyboard input.
        app.interactive.key(KeyCode::Char('2').into(), true);
        assert_eq!(app.messages.len(), 1);
        focus(&mut app);
        for code in codes {
            key(&mut app, code.into());
        }
        assert_eq!(app.messages.last().unwrap().role, "user");
        assert_eq!(app.messages.last().unwrap().content, "Next question");
        assert_eq!(app.input_text(), "keep my draft");
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
        drain_events(&mut app);
        abort_requests(&mut app);
    }
    let mut app = app(&quiz("First"));
    app.core.send_message("previous message".into()).unwrap();
    drain_events(&mut app);
    app.set_input("keep my draft");
    draw(&mut app);
    focus(&mut app);
    let before = app.messages.len();
    key(&mut app, KeyCode::Char('2').into());
    drain_events(&mut app);
    assert_eq!(app.messages.len(), before);
    assert_eq!(app.input_text(), "keep my draft");
    assert_eq!(app.status, "wait for the reply to finish, then pick again");
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
    assert_eq!(session.value("city"), "Paris");
    abort_requests(&mut app);
}

#[tokio::test(flavor = "current_thread")]
async fn choice_clicks_include_the_chip_blank_space_arrow_and_wrapped_continuation() {
    let label =
        "I want to tell you about dinner with her brother and the long walk home afterwards.";
    for (x_offset, y_offset) in [(0, 0), (3, 0), (75, 0), (0, 1), (75, 1)] {
        let mut app = app(&quiz(label));
        app.set_input("draft");
        draw(&mut app);
        let hit = app
            .interactive
            .hits
            .iter()
            .find(|hit| hit.control == Some(0))
            .unwrap()
            .clone();
        assert!(hit.rect.height > 1);
        assert!(mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.rect.x + x_offset.min(hit.rect.width - 1),
                row: hit.rect.y + y_offset,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 80, 32),
        ));
        assert_eq!(app.messages.last().unwrap().content, label);
        assert_eq!(app.input_text(), "draft");
        drain_events(&mut app);
        abort_requests(&mut app);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn number_choices_are_blocked_while_a_user_command_is_running() {
    let mut app = app(&quiz("First"));
    // `!` commands are an agent-mode feature.
    app.work_mode = nexus_core::app::WorkMode::Agent;
    app.execute(AppCommand::RunUserCommand { cmd: "true".into() })
        .unwrap();
    // No await: the command task cannot start or touch the filesystem.
    assert!(app.user_command_running());
    drain_events(&mut app);
    app.set_input("draft");
    draw(&mut app);
    focus(&mut app);
    key(&mut app, KeyCode::Char('2').into());
    drain_events(&mut app);
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.input_text(), "draft");
    assert_eq!(
        app.status,
        "wait for the command to finish, then pick again"
    );
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
}
