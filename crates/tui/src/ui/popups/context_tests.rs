use super::*;
use crossterm::event::KeyModifiers;
use nexus_core::app::App;
use nexus_core::db::Db;
use nexus_core::space::Space;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn fixture() -> AppView {
    let root = std::env::temp_dir().join(format!("nexus-context-costs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    AppView::new(App::new(
        Db::open_in_memory().unwrap(),
        None,
        Space { root },
    ))
}

fn screen(app: &AppView) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 22)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..22 {
        for x in 0..110 {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[test]
fn context_snapshot_has_real_tools_estimate_and_empty_session_state() {
    let mut app = fixture();
    app.popup = Popup::Context;
    let text = screen(&app);
    let tools = text.lines().find(|line| line.contains("Tools")).unwrap();
    let expected = u64::try_from(
        serde_json::to_string(&app.chat_tool_defs())
            .unwrap()
            .chars()
            .count()
            / 4,
    )
    .unwrap();
    assert_eq!(app.context_breakdown().tools_tokens, expected);
    assert!(tools.contains(&crate::ui::humanize(expected)), "{text}");
    assert!(text.contains("No requests in this session."), "{text}");
    assert!(text.contains("Total: 0 requests"), "{text}");
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
    );
    assert!(app.popup == Popup::None);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn session_requests_render_oldest_first_and_scroll_through_hundreds() {
    let mut app = fixture();
    let session = app
        .db
        .create_session("costs", "model", &app.active_space.id, "chat")
        .unwrap();
    for index in 0..200 {
        let id = app
            .db
            .log_usage(
                "OpenRouter",
                "test-model",
                100,
                20,
                40,
                0,
                Default::default(),
                Some(0.01),
                true,
                Some(&session.id),
                Some(&app.active_space.id),
            )
            .unwrap();
        app.db
            .set_usage_kind(id, &format!("request-{index:03}"))
            .unwrap();
        if index == 0 {
            app.db.add_usage_tool(id, "fs").unwrap();
        }
    }
    app.session = Some(session);
    app.popup = Popup::Context;
    let first = screen(&app);
    assert!(first.contains("request-000 · fs"), "{first}");
    assert!(first.contains("100 / 40 / 20  $0.0100"), "{first}");
    assert!(!first.contains("request-199"));
    for _ in 0..100 {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        );
        let _ = screen(&app);
    }
    let last = screen(&app);
    assert!(last.contains("request-199"), "{last}");
    assert!(
        last.contains("Total: 200 requests · 24000 tokens · $2.0000"),
        "{last}"
    );
    let end = app.context_scroll.get();
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.context_scroll.get(), end - 1);
    handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.context_scroll.get(), end);
    handle_key(&mut app, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.context_scroll.get(), end - 10);
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.popup == Popup::None);
    assert_eq!(app.context_scroll.get(), 0);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}
