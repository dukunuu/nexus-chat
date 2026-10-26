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

fn log(app: &AppView, session: &str, cost: Option<f64>) {
    app.db
        .log_usage(
            "OpenRouter",
            "test-model",
            100_000,
            2_000,
            80_000,
            0,
            Default::default(),
            cost,
            cost.is_some(),
            Some(session),
            Some(&app.active_space.id),
        )
        .unwrap();
}

fn with_session(app: &mut AppView) -> String {
    let session = app
        .db
        .create_session("costs", "model", &app.active_space.id, "chat")
        .unwrap();
    let id = session.id.clone();
    app.session = Some(session);
    id
}

#[test]
fn context_shows_the_real_tools_estimate() {
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
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
    );
    assert!(app.popup == Popup::None);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn session_spend_is_one_summary_not_a_request_list() {
    let mut app = fixture();
    let session = with_session(&mut app);
    for _ in 0..200 {
        log(&app, &session, Some(0.01));
    }
    log(&app, "another-session", Some(9.0));
    app.popup = Popup::Context;
    let text = screen(&app);
    let line = text.lines().find(|l| l.contains("Session")).unwrap();
    assert!(line.contains("$2.0000 · 200 requests"), "{text}");
    let h = crate::ui::humanize;
    assert!(
        text.contains(&format!(
            "{} in · {} cached · {} out",
            h(20_000_000),
            h(16_000_000),
            h(400_000)
        )),
        "{text}"
    );
    assert!(!text.contains("test-model"), "{text}");
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn session_spend_names_unpriced_requests() {
    let mut app = fixture();
    let session = with_session(&mut app);
    log(&app, &session, None);
    app.popup = Popup::Context;
    assert!(screen(&app).contains("no cost reported · 1 request"));
    log(&app, &session, Some(0.5));
    assert!(screen(&app).contains("$0.5000 + 1 unpriced · 2 requests"));
    std::fs::remove_dir_all(&app.space.root).unwrap();
}
