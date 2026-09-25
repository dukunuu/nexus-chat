use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app_view::AppView;
use nexus_core::app::{App, ChatNotification, CopyOption, SettingsField, SettingsRow};
use nexus_core::db::Db;
use nexus_core::space::Space;

fn test_app() -> AppView {
    let db = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join(format!("nexus-popup-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    AppView::new(App::new(db, Some("k"), Space { root }))
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
fn local_popup_lists_runtimes_with_discovery_and_endpoint() {
    let mut app = test_app();
    app.core.saved.local = Some(nexus_core::provider::local::LocalConfig {
        provider: nexus_core::provider::local::LocalRuntime::Mlx,
        endpoint: None,
        list_command: None,
        memory_budget_mb: None,
    });
    app.open_local_popup();
    let screen = render_to_string(80, 24, |f| super::local::render(f, &app));

    assert!(screen.contains("local runtime"), "{screen}");
    assert!(screen.contains("ollama list"), "{screen}");
    assert!(screen.contains("http://localhost:8080/v1"), "{screen}");
    assert!(screen.contains("✓ active"), "{screen}");
    // The picker opens on the configured runtime, not on row 0.
    assert_eq!(app.local_selected, 1);

    // Rows past the fold scroll into view with their own discovery hint, and
    // a surveyed row shows what its server is costing.
    app.core.local_status = vec![nexus_core::provider::serve::RuntimeStatus {
        runtime: nexus_core::provider::local::LocalRuntime::Edge0,
        endpoint: "http://localhost:8000/v1".into(),
        port: Some(8000),
        running: true,
        managed: true,
        rss_kb: Some(4300 * 1024),
        over_budget: true,
        budget_kb: Some(3000 * 1024),
    }];
    app.local_selected = 2;
    let screen = render_to_string(80, 24, |f| super::local::render(f, &app));
    assert!(screen.contains("mlx-serve list"), "{screen}");
    assert!(screen.contains("http://localhost:11234/v1"), "{screen}");

    app.local_selected = 4;
    let screen = render_to_string(80, 24, |f| super::local::render(f, &app));
    assert!(screen.contains("edge0 models"), "{screen}");
    // Urgency order: the size and the warning survive the truncation that
    // eats the endpoint at this width.
    assert!(screen.contains("● 4.2 GB · ⚠ over budget"), "{screen}");
    // A runtime with no survey row yet reads as down rather than blank.
    assert!(screen.contains("○ http://localhost:1234/v1"), "{screen}");
    assert!(screen.contains("s start"), "{screen}");
    // Given the room, ownership and the endpoint follow the warning.
    let wide = render_to_string(120, 30, |f| super::local::render(f, &app));
    assert!(
        wide.contains("● 4.2 GB · ⚠ over budget · started here · http://localhost:80"),
        "{wide}"
    );

    // Esc closes a bare `/local`, but steps back when `/login` opened it.
    let esc = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Esc);
    super::local::handle_key(&mut app, esc);
    assert_eq!(app.popup, nexus_core::app::Popup::None);
    app.open_login_popup();
    for _ in 0..4 {
        app.move_login_selection(1);
    }
    app.confirm_login_selection();
    assert_eq!(app.popup, nexus_core::app::Popup::Local);
    super::local::handle_key(&mut app, esc);
    assert_eq!(app.popup, nexus_core::app::Popup::Login);
}

#[test]
fn login_popup_offers_the_local_runtime_row() {
    let mut app = test_app();
    app.open_login_popup();
    // The list scrolls; the local row is the last one, past Codex.
    for _ in 0..4 {
        app.move_login_selection(1);
    }
    assert_eq!(app.login_selected, 4);
    app.move_login_selection(1);
    assert_eq!(app.login_selected, 4, "selection must stay in range");

    let screen = render_to_string(80, 30, |f| super::login::render(f, &app));
    assert!(screen.contains("Local runtime"), "{screen}");
    assert!(screen.contains("Ollama / MLX"), "{screen}");
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
            Default::default(),
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
            Default::default(),
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
#[cfg(test)]
mod usage_render_tests {
    use super::super::usage::render;
    use crate::app_view::AppView;
    use nexus_core::app::{App, usage::UsageData};
    use nexus_core::db::{Db, UsageByBackend, UsageByModel, UsageRow, UsageTotals};
    use nexus_core::space::Space;

    fn populated_app() -> AppView {
        let db = Db::open_in_memory().unwrap();
        let space = Space {
            root: std::env::temp_dir().join(format!("nexus-usage-{}", uuid::Uuid::new_v4())),
        };
        let mut app = AppView::new(App::new(db, Some("k"), space));
        app.usage_data = Some(UsageData {
            totals: UsageTotals {
                requests: 28_172,
                prompt_tokens: 5_900_000,
                completion_tokens: 144_000,
                cache_read_tokens: 3_300_000,
                cache_creation_tokens: 2_600_000,
                rated_prompt_tokens: 5_900_000,
                rated_cache_read_tokens: 3_300_000,
                cost: 6.1438,
            },
            by_backend: vec![
                UsageByBackend {
                    backend: "OpenCode Go".into(),
                    requests: 28_125,
                    prompt_tokens: 741_000,
                    completion_tokens: 24_800,
                    cache_read_tokens: 481_650,
                    rated_prompt_tokens: 741_000,
                    rated_cache_read_tokens: 481_650,
                    cost: 0.1108,
                },
                UsageByBackend {
                    backend: "OpenRouter".into(),
                    requests: 47,
                    prompt_tokens: 5_100_000,
                    completion_tokens: 119_100,
                    cache_read_tokens: 2_805_000,
                    rated_prompt_tokens: 5_100_000,
                    rated_cache_read_tokens: 2_805_000,
                    cost: 6.0330,
                },
            ],
            by_model: vec![
                UsageByModel {
                    model: "go:deepseek-v3-Flash".into(),
                    requests: 28_125,
                    prompt_tokens: 741_000,
                    completion_tokens: 24_800,
                    cache_read_tokens: 481_650,
                    rated_prompt_tokens: 741_000,
                    rated_cache_read_tokens: 481_650,
                    cost: 0.1108,
                },
                UsageByModel {
                    model: "deepseek/deepseek-v3-Flash-0724".into(),
                    requests: 47,
                    prompt_tokens: 5_100_000,
                    completion_tokens: 119_100,
                    cache_read_tokens: 2_805_000,
                    rated_prompt_tokens: 5_100_000,
                    rated_cache_read_tokens: 2_805_000,
                    cost: 6.0330,
                },
            ],
            recent: (0..3)
                .map(|i| UsageRow {
                    created_at: format!("2026-08-12T23:{:02}:00Z", 15 - i),
                    backend: "OpenRouter".into(),
                    // Overflow-prone values: "122.2k→672" (10 chars) and
                    // "120.1k→2.1k" (11 chars) exceed a 9-cell field.
                    model: format!("deepseek/deepseek-v4-flash-0731 ({i})"),
                    prompt_tokens: [122_221, 120_086, 118_690][i],
                    completion_tokens: [672, 2126, 592][i],
                    cache_read_tokens: 118_784,
                    cache_creation_tokens: 0,
                    prompt_convention: Default::default(),
                    cost: Some(0.0095),
                })
                .collect(),
        });
        app
    }

    /// Render the popup alone and return the buffer rows as strings.
    fn render_rows(app: &AppView) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// Char position of `needle` — byte positions are unusable here: the
    /// rows mix 1- and 3-byte glyphs (spaces, │, ●), so byte offsets would
    /// drift by the wide-glyph byte count.
    fn char_pos(line: &str, needle: &str) -> Option<usize> {
        line.find(needle).map(|b| line[..b].chars().count())
    }

    /// Column check: in `header`, the label's right edge must equal the
    /// value's right edge in `row` (right-aligned columns), with the cached
    /// column's "%" riding one cell past the label like the rows' "65%".
    fn assert_column(header: &str, row: &str, label: &str, value: &str) {
        let label_end = char_pos(header, label).map_or_else(
            || {
                panic!("header missing {label:?}: {header}");
            },
            |i| i + label.len(),
        );
        let value_end = char_pos(row, value).map_or_else(
            || {
                panic!("row missing {value:?}: {row}");
            },
            |i| i + value.len(),
        );
        assert_eq!(
            label_end, value_end,
            "column {label:?} ({value:?}) misaligned\nheader: {header}\nrow:    {row}"
        );
    }

    /// At 80×24 the popup has ~13 inner rows: the backend rows must survive,
    /// and wide lines end in an ellipsis rather than a mid-word cut.
    #[test]
    fn small_terminal_keeps_backend_rows_and_ellipsizes_overflow() {
        let app = populated_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let screen: String = (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                row + "\n"
            })
            .collect();
        assert!(
            screen.contains("OpenCode"),
            "backend rows missing:\n{screen}"
        );
        assert!(
            screen.contains("OpenRouter"),
            "backend rows missing:\n{screen}"
        );
        assert!(screen.contains('…'), "overflow not ellipsized:\n{screen}");
    }

    #[test]
    fn backend_header_columns_align_with_rows() {
        let app = populated_app();
        let rows = render_rows(&app);
        let header = rows
            .iter()
            .find(|r| r.contains("req") && r.contains("cached") && r.contains("cost"))
            .unwrap();
        let row = rows.iter().find(|r| r.contains("28125")).unwrap();
        // The "● " marker must have a 2-space header counterpart.
        assert_eq!(
            char_pos(header, "backend"),
            char_pos(row, "OpenCode"),
            "name column misaligned"
        );
        assert_column(header, row, "req", "28125");
        assert_column(header, row, "prompt", "741.0k");
        assert_column(header, row, "out", "24.8k");
        assert_column(header, row, "cached", "65"); // "%" rides one past
        assert_column(header, row, "cost", "$0.1108");
    }

    #[test]
    fn models_header_columns_align_with_rows() {
        let app = populated_app();
        let rows = render_rows(&app);
        let header = rows
            .iter()
            .find(|r| r.contains("model") && r.contains("req") && r.contains("cached"))
            .unwrap();
        let short = rows.iter().find(|r| r.contains("go:deepseek")).unwrap();
        let long = rows
            .iter()
            .find(|r| r.contains("deepseek/deepseek-v3-Flash-07"))
            .unwrap();
        // Different name lengths must not move the numeric columns.
        // Right-aligned: the values' right edges must coincide.
        let edge = |line: &str, needle: &str| char_pos(line, needle).map(|c| c + needle.len());
        assert_eq!(edge(short, "28125"), edge(long, "47"), "req column drifts");
        assert_column(header, short, "req", "28125");
        assert_column(header, short, "prompt", "741.0k");
        assert_column(header, short, "out", "24.8k");
        assert_column(header, short, "cached", "65");
        assert_column(header, short, "cost", "$0.1108");
    }

    #[test]
    fn recent_request_rows_share_columns() {
        let app = populated_app();
        let rows = render_rows(&app);
        let mut recent: Vec<&String> = rows
            .iter()
            .filter(|r| r.contains("→") && r.contains("\u{2588}"))
            .collect();
        assert!(recent.len() >= 2, "need recent rows, got {}", recent.len());
        let first = recent.remove(0);
        for row in &recent {
            // The "→" sits inside the right-aligned tokens field, so its
            // position varies with the token string length — the column
            // anchors are the field's right edge (the cache bar) and the
            // percent/cost columns after it.
            assert_eq!(
                char_pos(first, "████████"),
                char_pos(row, "████████"),
                "cache bar column drifts\nfirst: {first}\nrow:   {row}"
            );
            assert_eq!(
                first.rfind('%').map(|b| first[..b].chars().count()),
                row.rfind('%').map(|b| row[..b].chars().count()),
                "cache% column drifts\nfirst: {first}\nrow:   {row}"
            );
            assert_eq!(
                char_pos(first, "$0.0095"),
                char_pos(row, "$0.0095"),
                "cost column drifts\nfirst: {first}\nrow:   {row}"
            );
        }
    }
}

/// Watch-picker key-flow tests: TUI keys (ui/popups/watches.rs) driving core
/// state through the popup.
mod watch_popup_tests {
    use crate::app_view::AppView;
    use nexus_core::app::{App, WatchMode};
    use nexus_core::db::Db;
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-watch-test-{}", uuid::Uuid::new_v4())),
        }
    }

    #[test]
    fn watch_picker_ctrl_d_confirms_with_a_second_press() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        let space = a.active_space.id.clone();
        let session = a.db.create_session("watch", "a/b", &space, "chat").unwrap();
        let _ =
            a.db.create_watch(&space, "rust async", 24, &session.id)
                .unwrap();
        a.open_watch_picker().unwrap();

        crate::ui::popups::watches::handle_key(
            &mut a,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('d'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        )
        .unwrap();
        assert_eq!(a.watch_mode, WatchMode::ConfirmDelete);

        crate::ui::popups::watches::handle_key(
            &mut a,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('d'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        )
        .unwrap();
        assert_eq!(a.watch_mode, WatchMode::Browse);
        assert!(a.watches_cache.is_empty());
        assert!(a.db.list_watches(&space).unwrap().is_empty());
    }

    #[test]
    fn watch_picker_escape_cancels_delete_confirmation() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        let space = a.active_space.id.clone();
        let session = a.db.create_session("watch", "a/b", &space, "chat").unwrap();
        let _ =
            a.db.create_watch(&space, "rust async", 24, &session.id)
                .unwrap();
        a.open_watch_picker().unwrap();
        a.watch_mode = WatchMode::ConfirmDelete;

        crate::ui::popups::watches::handle_key(
            &mut a,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::empty(),
            ),
        )
        .unwrap();

        assert_eq!(a.watch_mode, WatchMode::Browse);
        assert_eq!(a.watches_cache.len(), 1);
    }
}

/// The popups' Ctrl+X handlers close the popup themselves:
/// `stop_research`/`stop_swarm` are domain calls and never touch view state.
mod stop_closes_popup_tests {
    use crate::app_view::AppView;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexus_core::app::{App, Popup};
    use nexus_core::db::Db;
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-stop-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn ctrl_x() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
    }

    #[test]
    fn research_live_ctrl_x_closes_the_popup() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.popup = Popup::ResearchLive;
        a.core.research_live_input = "a steer".to_string();

        crate::ui::popups::research_live::handle_key(&mut a, ctrl_x());

        assert_eq!(a.popup, Popup::None);
    }

    #[test]
    fn swarm_ctrl_x_closes_the_popup() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        // The Ctrl+X arm is gated on a running turn; hand it a live channel.
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        a.core.swarm_rx = Some(rx);
        a.popup = Popup::Swarm;

        crate::ui::popups::swarm::handle_key(&mut a, ctrl_x()).unwrap();

        assert_eq!(a.popup, Popup::None);
        assert!(a.core.swarm_rx.is_none()); // the turn was stopped
    }
}

fn picker_app(ids: &[&str]) -> AppView {
    let mut app = test_app();
    app.core.models = ids
        .iter()
        .map(|id| nexus_core::provider::Model {
            id: (*id).into(),
            name: (*id).into(),
            reasoning_efforts: Vec::new(),
            context_length: Some(262_144),
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: nexus_core::provider::BackendTag::OpenRouter,
            pricing: None,
        })
        .collect();
    app.open_model_picker();
    app
}

#[test]
fn model_picker_available_panel_uses_its_own_width() {
    // Near-identical long ids stay distinguishable: the wide panel sizes rows
    // to its own width, not the favorites column's.
    let ids = [
        "aion-labs/aion-2.0-reasoning-preview",
        "aion-labs/aion-2.0-reasoning-mini",
    ];
    let mut app = picker_app(&ids);
    let screen = render_to_string(150, 40, |f| super::model::render(f, &mut app));
    for id in ids {
        assert!(screen.contains(id), "{id} truncated:\n{screen}");
    }
    // Six-char context sizes fit their column whole.
    assert!(screen.contains("262.1k"), "context size clipped:\n{screen}");
}

#[test]
fn model_picker_click_row_maps_to_the_row_drawn_there() {
    let ids = ["a/alpha", "b/bravo", "c/charlie"];
    let mut app = picker_app(&ids);
    let screen = render_to_string(150, 40, |f| super::model::render(f, &mut app));
    let (_, avail_outer) =
        super::model::model_popup_areas(ratatui::layout::Rect::new(0, 0, 150, 40));
    let inner = super::model::list_inner(avail_outer);
    let available = app.available_models();
    for id in ids {
        let row = screen
            .lines()
            .position(|l| l.contains(id))
            .unwrap_or_else(|| panic!("{id} not drawn:\n{screen}"));
        // Same arithmetic as the mouse handler in events.rs.
        let index = app.avail_offset + (row - inner.y as usize);
        assert_eq!(available[index].id, id, "clicking {id} picks another row");
    }
}

#[test]
fn help_popup_lists_keys_and_every_command() {
    let mut app = test_app();
    app.execute(nexus_core::app::AppCommand::OpenHelp).unwrap();
    assert!(app.popup == nexus_core::app::Popup::Help);
    // Tall enough to show everything without scrolling.
    let screen = render_to_string(140, 120, |f| super::help::render(f, &mut app));
    assert!(screen.contains("composer"), "{screen}");
    assert!(screen.contains("Shift/Ctrl+Enter"), "{screen}");
    for c in nexus_core::app::COMMANDS {
        assert!(
            screen.contains(&format!("/{}", c.name)),
            "/{} missing:\n{screen}",
            c.name
        );
    }
}

#[test]
fn help_popup_scroll_clamps_to_content() {
    let mut app = test_app();
    app.open_help();
    app.help_scroll = u16::MAX;
    let screen = render_to_string(100, 30, |f| super::help::render(f, &mut app));
    // Scrolled to the end: the last command is visible, not a blank pane.
    let last = nexus_core::app::COMMANDS.last().unwrap().name;
    assert!(screen.contains(&format!("/{last}")), "{screen}");
    assert!(app.help_scroll < u16::MAX);
}

#[test]
fn settings_popup_masks_the_langsearch_key() {
    let mut app = test_app();
    app.open_settings();
    app.settings_inputs[5] = "sk-test0123456789abcdef".into();
    let screen = render_to_string(120, 60, |f| super::settings::render(f, &mut app));
    assert!(!screen.contains("sk-test0123456789abcdef"), "{screen}");
    assert!(!screen.contains("sk-test"), "{screen}");
    assert!(screen.contains("••••••cdef"), "{screen}");
}

#[test]
fn status_bar_names_the_model_and_keeps_the_space_tag_beside_the_gauge() {
    let mut app = test_app();
    app.core.models = vec![nexus_core::provider::Model {
        id: "org/Big-Model-7B".into(),
        name: "Big Model".into(),
        reasoning_efforts: Vec::new(),
        context_length: Some(128_000),
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: nexus_core::provider::BackendTag::Local,
        pricing: None,
    }];
    app.core.current_model = Some("local:org/Big-Model-7B".into());
    app.core.active_space.name = "work".into();
    app.settings.show_stats = true;
    app.status = "ready".into();
    let screen = render_to_string(100, 1, |f| {
        crate::ui::render_status(f, &app, f.area());
    });
    assert!(screen.contains("[work] Big-Model-7B · local"), "{screen}");
    assert!(!screen.contains("local:org/"), "{screen}");
    assert!(screen.contains("ready"), "{screen}");

    // A status too long for the bar ends in an ellipsis instead of a cut.
    app.status = "x".repeat(200);
    let screen = render_to_string(100, 1, |f| {
        crate::ui::render_status(f, &app, f.area());
    });
    assert!(screen.trim_end().ends_with('…'), "{screen}");
}

#[test]
fn short_model_label_keeps_openrouter_suffixes() {
    assert_eq!(
        crate::ui::short_model_label("openai/gpt-oss-20b:free"),
        "gpt-oss-20b:free"
    );
    assert_eq!(
        crate::ui::short_model_label("codex:gpt-5.5"),
        "gpt-5.5 · codex"
    );
    assert_eq!(
        crate::ui::short_model_label("local:mlx-community/Qwen3-8B"),
        "Qwen3-8B · local"
    );
}

#[test]
fn status_line_expires_after_its_ttl() {
    let mut app = test_app();
    app.apply_event(&nexus_core::app::AppEvent::Status("saved".into()));
    app.expire_status();
    assert_eq!(app.status, "saved", "fresh status stays");
    app.status_at = Some(std::time::Instant::now() - crate::app_view::STATUS_TTL);
    app.expire_status();
    assert!(app.status.is_empty());

    // Errors linger longer.
    app.apply_event(&nexus_core::app::AppEvent::Status("error: boom".into()));
    app.status_at = Some(std::time::Instant::now() - crate::app_view::STATUS_TTL);
    app.expire_status();
    assert_eq!(app.status, "error: boom");
}

#[test]
fn an_open_popup_dims_the_screen_behind_it_but_not_itself() {
    use ratatui::style::Modifier;
    let mut app = test_app();
    app.open_help();
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    // Corner cell: behind the popup. Center cell: inside it.
    assert!(buffer[(0, 0)].modifier.contains(Modifier::DIM));
    assert!(!buffer[(50, 20)].modifier.contains(Modifier::DIM));
}

#[test]
fn file_names_truncate_in_the_middle_to_keep_the_slug_and_extension() {
    let name = "b0db75e4-2ccb-4a53-817e-6934389f1d66-description_for_reasoning_model_what.png";
    let short = super::chrome::truncate_middle(name, 34);
    assert_eq!(short.chars().count(), 34);
    assert!(short.starts_with("b0db75e4"), "{short}");
    assert!(short.ends_with("model_what.png"), "{short}");
    assert_eq!(super::chrome::truncate_middle("short.png", 34), "short.png");
}

#[test]
fn fit_line_ellipsizes_across_spans() {
    use ratatui::text::{Line, Span};
    let line = Line::from(vec![Span::raw("abc"), Span::raw("defgh")]);
    let fitted = super::chrome::fit_line(line.clone(), 6);
    assert_eq!(fitted.to_string(), "abcde…");
    assert_eq!(
        super::chrome::fit_line(line.clone(), 8).to_string(),
        "abcdefgh"
    );
    assert_eq!(super::chrome::fit_line(line, 0).to_string(), "");
}

#[test]
fn session_picker_leads_with_the_title_and_marks_the_open_session() {
    let mut app = test_app();
    let space = app.core.active_space.id.clone();
    let a = app
        .core
        .db
        .create_session(
            "Japanese Fluency Roadmap",
            "local:org/Big-Model-7B",
            &space,
            "chat",
        )
        .unwrap();
    app.core
        .db
        .create_session(
            "Pomodoro Effectiveness Research",
            "a/one",
            &space,
            "research",
        )
        .unwrap();
    app.core.session = Some(a);
    app.open_session_picker().unwrap();
    let screen = render_to_string(100, 40, |f| super::session::render(f, &mut app));
    let title_row = screen
        .lines()
        .find(|l| l.contains("Japanese Fluency Roadmap"))
        .unwrap_or_else(|| panic!("{screen}"));
    let when = crate::ui::fmt_created(&app.core.sessions_cache[0].created_at);
    assert!(
        title_row.contains(&when),
        "date beside the title:\n{screen}"
    );
    assert!(screen.contains("Big-Model-7B · local · open"), "{screen}");
    // Emoji-marked rows (🔬) still end inside the frame, not over the border.
    let research_row = screen.lines().find(|l| l.contains("Pomodoro")).unwrap();
    assert!(research_row.trim_end().ends_with('│'), "{research_row}");
}

#[test]
fn enter_in_skills_arms_the_highlighted_skill() {
    let mut app = test_app();
    app.core.skills = vec![nexus_core::skills::Skill {
        name: "grill-me".into(),
        description: "stress-test a plan".into(),
        dir: std::path::PathBuf::from("/home/u/.claude/skills/grill-me"),
    }];
    app.popup = nexus_core::app::Popup::Skills;
    let screen = render_to_string(100, 30, |f| super::skills::render(f, &app));
    assert!(screen.contains(".claude"), "source tag:\n{screen}");
    super::skills::handle_key(
        &mut app,
        crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
    );
    assert!(app.popup == nexus_core::app::Popup::None);
    assert_eq!(app.core.forced_skill.as_deref(), Some("grill-me"));
}

#[test]
fn files_popup_title_shows_every_tab() {
    let mut app = test_app();
    app.popup = nexus_core::app::Popup::Files;
    for tab in [
        nexus_core::app::FilesTab::Files,
        nexus_core::app::FilesTab::Images,
        nexus_core::app::FilesTab::Scripts,
    ] {
        app.files_tab = tab;
        let screen = render_to_string(100, 30, |f| super::files::render(f, &app));
        assert!(screen.contains("files · images · scripts"), "{screen}");
    }
}
