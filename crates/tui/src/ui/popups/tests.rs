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
                cost: 6.1438,
            },
            by_backend: vec![
                UsageByBackend {
                    backend: "OpenCode Go".into(),
                    requests: 28_125,
                    prompt_tokens: 741_000,
                    completion_tokens: 24_800,
                    cache_read_tokens: 481_650,
                    cost: 0.1108,
                },
                UsageByBackend {
                    backend: "OpenRouter".into(),
                    requests: 47,
                    prompt_tokens: 5_100_000,
                    completion_tokens: 119_100,
                    cache_read_tokens: 2_805_000,
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
                    cost: 0.1108,
                },
                UsageByModel {
                    model: "deepseek/deepseek-v3-Flash-0724".into(),
                    requests: 47,
                    prompt_tokens: 5_100_000,
                    completion_tokens: 119_100,
                    cache_read_tokens: 2_805_000,
                    cost: 6.0330,
                },
            ],
            recent: (0..3)
                .map(|i| UsageRow {
                    created_at: format!("2026-08-12T23:{:02}:00Z", 15 - i),
                    backend: "OpenRouter".into(),
                    // Realistic overflow-prone values: "122.2k→672" (10
                    // chars) and "120.1k→2.1k" (11 chars) exceed a 9-cell
                    // field and used to shove the trailing columns around.
                    model: format!("deepseek/deepseek-v4-flash-0731 ({i})"),
                    prompt_tokens: [122_221, 120_086, 118_690][i],
                    completion_tokens: [672, 2126, 592][i],
                    cache_read_tokens: 118_784,
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

/// Watch-picker key-flow tests. The picker's `handle_key` lives in this
/// crate (ui/popups/watches.rs); the state it drives lives in core — this
/// is the first seam test: TUI keys driving core state through the popup.
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

/// 2e regression tests: `stop_research`/`stop_swarm` no longer own the popup
/// (it's view state), so the popups' Ctrl+X handlers must close it
/// themselves — the old core methods did, and the refactor dropped it.
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
