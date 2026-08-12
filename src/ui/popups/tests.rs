use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{App, ChatNotification, CopyOption, SettingsField, SettingsRow};
use crate::db::Db;
use crate::space::Space;

fn test_app() -> App {
    let db = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join(format!("nexus-popup-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    App::new(db, Some("k".into()), Space { root })
}

fn render_to_string(width: u16, height: u16, render: impl FnOnce(&mut ratatui::Frame)) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(render).unwrap();
    let backend = terminal.backend();
    let buffer = backend.buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn copy_popup_uses_rounded_border_and_standard_marker() {
    let mut app = test_app();
    app.copy_options = vec![CopyOption {
        label: "Copy one".into(),
        text: "one".into(),
    }];
    let screen = render_to_string(80, 24, |f| super::copy::render(f, &app));

    assert!(screen.contains('╭'), "{screen}");
    assert!(screen.contains('▸'), "{screen}");
}

#[test]
fn usage_popup_renders_dashboard_with_cache_bars_and_tables() {
    let mut app = test_app();
    app.db
        .log_usage(
            "OpenRouter",
            "anthropic/claude-3.5-sonnet",
            1000,
            120,
            700,
            200,
            Some(0.0042),
            true,
            None,
            None,
        )
        .unwrap();
    app.db
        .log_usage(
            "Codex",
            "gpt-5.1-codex",
            100,
            10,
            0,
            0,
            None,
            false,
            None,
            None,
        )
        .unwrap();
    app.open_usage_popup();

    let screen = render_to_string(96, 40, |f| super::usage::render(f, &app));

    // Section markers + headers.
    assert!(screen.contains("by backend"), "{screen}");
    assert!(screen.contains("most used models"), "{screen}");
    assert!(screen.contains("recent requests"), "{screen}");
    // Hero summary: request count, cache fraction, cache-write count.
    assert!(screen.contains("2 requests"), "{screen}");
    assert!(screen.contains("cache writes"), "{screen}");
    assert!(screen.contains("% of prompt served from cache"), "{screen}");
    // Both backends with their glyphs; cache bar glyphs present in rows.
    assert!(screen.contains("OpenRouter"), "{screen}");
    assert!(screen.contains("Codex"), "{screen}");
    assert!(screen.contains('█'), "{screen}");
    assert!(screen.contains('░'), "{screen}");
    // Models ranked; cost formatting for known vs unknown prices.
    assert!(screen.contains("claude-3.5-sonnet"), "{screen}");
    assert!(screen.contains("$0.0042"), "{screen}");
}

#[test]
fn hidden_hints_do_not_show_in_popup_titles() {
    let mut app = test_app();
    app.settings.hide_hints = true;
    let screen = render_to_string(80, 24, |f| super::copy::render(f, &app));

    assert!(!screen.contains("Enter"), "{screen}");
    assert!(!screen.contains("Esc"), "{screen}");
}

#[test]
fn research_live_popup_shows_agent_lifecycle_and_activity() {
    let mut app = test_app();
    // The popup renders from the job-level stage-row mirror, kept in sync by
    // `mirror_stage` on every Stage update.
    app.research_stage_rows = vec![
        "planner: done — proposed 4 questions".to_string(),
        "searcher r1 1/4: working — Searching the web for rust runtimes".to_string(),
        "verifier: error — cached source unavailable".to_string(),
    ];

    let screen = render_to_string(100, 30, |f| super::research_live::render(f, &app));

    assert!(screen.contains("research agents"), "{screen}");
    assert!(screen.contains("✓ planner"), "{screen}");
    assert!(screen.contains("● searcher r1 1/4"), "{screen}");
    assert!(screen.contains("Searching the web"), "{screen}");
    assert!(screen.contains("× verifier"), "{screen}");
}

#[test]
fn research_live_popup_shows_queued_steers_until_picked_up() {
    let mut app = test_app();
    app.research_steer_log = vec![(1, "look into X".to_string()), (2, "also Y".to_string())];
    // Steer #1 was already picked up by the pipeline (acknowledged); steer
    // #2 is still queued and must show in the popup.
    app.research_steer_acked = std::collections::HashSet::from([1]);
    app.research_stage_rows = vec!["steer #1: look into X".to_string()];

    let screen = render_to_string(100, 30, |f| super::research_live::render(f, &app));

    assert!(screen.contains("queued steers"), "{screen}");
    assert!(screen.contains("● also Y"), "{screen}");
    // Picked-up steers appear as stage rows, not in the queued list.
    assert!(!screen.contains("● look into X"), "{screen}");
    assert!(screen.contains("○ steer #1"), "{screen}");
}

#[test]
fn research_live_popup_queued_status_ignores_steer_text_shape() {
    let mut app = test_app();
    app.research_steer_log = vec![
        (1, "a: b".to_string()),
        (2, "a".to_string()),
        (3, "same".to_string()),
        (4, "same".to_string()),
        (5, "100% done".to_string()),
    ];
    // Positions 1, 3, 5 are acknowledged (drained by the pipeline); 2 and 4
    // are still queued. Keying by position — never by steer text — keeps
    // duplicate, prefix-of-each-other, and LIKE-wildcard text from
    // collapsing acknowledgements.
    app.research_steer_acked = std::collections::HashSet::from([1, 3, 5]);
    app.research_stage_rows = vec![
        "steer #1: a: b".to_string(),
        "steer #3: same".to_string(),
        "steer #5: 100% done".to_string(),
    ];

    let screen = render_to_string(100, 30, |f| super::research_live::render(f, &app));

    assert!(screen.contains("queued steers"), "{screen}");
    // Position 2 ("a") and position 4 (the second "same") are still queued.
    assert!(screen.contains("● a"), "{screen}");
    assert!(screen.contains("● same"), "{screen}");
    // Picked-up steers (positions 1, 3, 5) show as rows, never as queued.
    assert!(!screen.contains("● a: b"), "{screen}");
    assert!(!screen.contains("● 100% done"), "{screen}");
    assert!(screen.contains("○ steer #1"), "{screen}");
    assert!(screen.contains("○ steer #5"), "{screen}");
}

#[test]
fn research_live_popup_uses_the_job_sessions_rows_from_any_session() {
    let mut app = test_app();
    // The job runs in session A while the user views session B: the popup
    // renders from the job-level stage-row mirror (`research_stage_rows`),
    // which `on_research_done` fills for every Stage update regardless of
    // what's viewed — no db read and no dependence on the viewed session.
    app.research_running = Some(("job-session".to_string(), "topic".to_string()));
    app.session = Some(
        app.db
            .create_session("other", "a/one", &app.active_space.id, "chat")
            .unwrap(),
    );
    app.research_stage_rows = vec![
        "steer #1: look into X".to_string(),
        "planner: done — proposed 4 questions".to_string(),
    ];
    app.research_steer_log = vec![(1, "look into X".to_string()), (2, "also Y".to_string())];
    app.research_steer_acked = std::collections::HashSet::from([1]);

    let screen = render_to_string(100, 30, |f| super::research_live::render(f, &app));

    // The job's rows came through the mirror (the viewed session has none).
    assert!(screen.contains("○ steer #1"), "{screen}");
    assert!(screen.contains("✓ planner"), "{screen}");
    // Acknowledgements are job-global: steer #1 was drained, steer #2 is
    // still queued.
    assert!(screen.contains("● also Y"), "{screen}");
    assert!(!screen.contains("● look into X"), "{screen}");
}

#[test]
fn settings_popup_renders_selected_field_detail() {
    let mut app = test_app();
    let rows = app.settings_rows();
    app.settings_selected = rows
        .iter()
        .position(|r| matches!(r, SettingsRow::Field(SettingsField::ShowStats)))
        .unwrap();
    let screen = render_to_string(100, 30, |f| super::settings::render(f, &app));

    assert!(screen.contains("model · TPS footer"), "{screen}");
}

#[test]
fn completed_chat_notification_is_rendered_as_a_click_target() {
    let mut app = test_app();
    let session = app
        .db
        .create_session("background chat", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.notifications.push_back(ChatNotification {
        session_id: session.id,
        title: "background chat".into(),
        text: "response complete".into(),
        success: true,
    });

    let screen = render_to_string(100, 30, |f| crate::ui::render(f, &mut app));

    assert!(screen.contains("background chat"), "{screen}");
    assert_eq!(app.notification_areas.len(), 1);
}

#[test]
fn new_session_welcome_screen_drops_the_previous_sessions_click_layout() {
    let mut app = test_app();
    let sid = app
        .db
        .create_session("old", "a/one", &app.active_space.id, "chat")
        .unwrap()
        .id;
    app.db
        .add_assistant_message(
            &sid,
            "see https://example.com/old-link",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    app.session = app.db.get_session(&sid).unwrap();
    app.messages = app.db.load_messages(&sid).unwrap();

    // Render the old session: the history area is clickable now (a click
    // anywhere inside it maps to a recorded line — which is how a stale
    // snapshot would resolve the old URL).
    render_to_string(100, 30, |f| crate::ui::render(f, &mut app));
    assert!(
        app.sel.pos_at(10, 5).is_some(),
        "old session layout recorded"
    );

    // Start a new session and render the welcome screen.
    app.run_command("new").unwrap(); // /new
    render_to_string(100, 30, |f| crate::ui::render(f, &mut app));

    // The layout snapshot is empty — clicking where the old URL used to be
    // must not resolve to anything (it would otherwise open the old link).
    assert_eq!(app.sel.pos_at(10, 5), None);
    // And the selection itself is cleared by /new.
    assert!(app.sel.selected_text().is_none());
}
