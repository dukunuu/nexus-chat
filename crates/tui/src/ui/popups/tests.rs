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
fn jobs_popup_two_jobs_and_parked_run_snapshot() {
    use nexus_core::jobs::{Job, JobRun, JobSpec};

    let mut app = test_app();
    let device = app.db.device_id().unwrap();
    app.jobs_popup.jobs = [("morning", "interval"), ("fix tests", "until_done")]
        .into_iter()
        .map(|(name, trigger)| Job {
            id: name.into(),
            space_id: app.active_space.id.clone(),
            session_id: String::new(),
            device_id: device.clone(),
            spec: JobSpec {
                name: name.into(),
                trigger_kind: trigger.into(),
                ..JobSpec::default()
            },
            enabled: 1,
            last_run_at: None,
            updated_at: "2026-01-01T00:00:00Z".into(),
        })
        .collect();
    app.jobs_popup.selected = 1;
    app.jobs_popup.runs.push(JobRun {
        id: "run".into(),
        job_id: "fix tests".into(),
        device_id: device,
        started_at: "2026-01-01T00:00:00Z".into(),
        finished_at: Some("2026-01-01T00:01:00Z".into()),
        status: "parked".into(),
        iters: 1,
        cost_usd: 0.01,
        branch: Some("nexus/job/fix/run".into()),
        summary: "parked: needs approval for fs write fixed".into(),
        updated_at: "2026-01-01T00:01:00Z".into(),
    });
    let screen = render_to_string(120, 30, |frame| super::jobs::render(frame, &app));
    assert!(screen.contains(" jobs "), "{screen}");
    let snapshot: Vec<_> = screen
        .lines()
        .filter_map(|line| {
            let (_, body) = line.split_once('│')?;
            let (body, _) = body.rsplit_once('│')?;
            let body = body.trim();
            (!body.is_empty()).then_some(body)
        })
        .collect();
    assert_eq!(
        snapshot,
        [
            "morning · interval · enabled · never run",
            "▸ fix tests · until_done · enabled · parked",
            "nexus/job/fix/run",
            "parked: needs approval for fs write fixed",
        ]
    );
    let root = app.space.root.clone();
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn procs_popup_two_processes_snapshot() {
    use nexus_core::tools::ToolBox;
    use nexus_core::workspace::Workspace;

    let app = test_app();
    let root = app.space.root.clone();
    let mut toolbox = ToolBox::new(
        std::path::PathBuf::new(),
        None,
        None,
        "auto".into(),
        vec![],
        Some(root.join("nexus.db")),
        None,
        None,
    );
    toolbox.workspace = Some(std::sync::Arc::new(Workspace::open(&root, vec![]).unwrap()));
    toolbox.procs = std::sync::Arc::clone(&app.procs);
    for (session, name, line) in [
        ("", "server", "listening"),
        ("other", "watcher", "watching"),
    ] {
        toolbox.session_id = session.into();
        let args = serde_json::json!({"action":"start", "cmd":format!("echo {line}; sleep 30"), "name":name, "ready_when":line});
        let (result, _) = toolbox.run("proc", &args.to_string()).await;
        assert!(result.contains(" · ready ·"), "{result}");
    }
    let screen = render_to_string(120, 30, |f| super::procs::render(f, &app));
    assert!(screen.contains(" procs "), "{screen}");
    let snapshot: Vec<_> = screen
        .lines()
        .filter_map(|line| {
            let (_, body) = line.split_once('│')?;
            let (body, _) = body.rsplit_once('│')?;
            let body = body.trim();
            if body.is_empty() {
                return None;
            }
            Some(if let Some((name, rest)) = body.split_once(" · running ") {
                let (_, output) = rest.split_once(" · ").unwrap();
                format!("{name} · running <uptime> · {output}")
            } else {
                body.to_string()
            })
        })
        .collect();
    assert_eq!(
        snapshot,
        [
            "this session first · processes stop when Nexus exits",
            "▸ server · running <uptime> · listening",
            "watcher · running <uptime> · watching",
        ]
    );
    app.procs.shutdown().await;
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

fn approval_app(
    tool: &str,
    summary: String,
) -> (
    AppView,
    tokio::sync::oneshot::Receiver<nexus_core::tools::Decision>,
) {
    let mut app = test_app();
    app.settings.hide_hints = false;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.pending_approval = Some(sender);
    app.pending_approval_request = Some(nexus_core::tools::ApprovalRequest {
        tool: tool.into(),
        action: if tool == "shell" { "run" } else { "edit" }.into(),
        summary,
        allow_session: true,
    });
    app.sync_approval_popup();
    (app, receiver)
}

#[test]
fn harness_popup_three_rows_snapshot() {
    use nexus_core::db::{FailureRow, HarnessSummary};

    let mut app = test_app();
    let root = app.space.root.clone();
    app.settings.hide_hints = false;
    let now = chrono::Utc::now();
    app.harness = Some(HarnessSummary {
        calls: 40,
        failures: 20,
        rows: [
            ("scripts", "edit", "stale_edit", 14, 150),
            ("shell", "run", "blocked", 4, 5),
            ("fs", "read", "not_found", 2, 3000),
        ]
        .into_iter()
        .map(|(tool, action, kind, count, minutes)| FailureRow {
            tool: tool.into(),
            action: action.into(),
            error_kind: kind.into(),
            count,
            last_at: (now - chrono::Duration::minutes(minutes) - chrono::Duration::seconds(30))
                .to_rfc3339(),
            last_session_id: "session".into(),
        })
        .collect(),
    });
    let screen = render_to_string(120, 30, |f| super::harness::render(f, &app));
    assert!(screen.contains(" harness "), "{screen}");
    assert!(
        screen.contains("↑↓ select · enter open the latest transcript · esc close"),
        "{screen}"
    );
    let snapshot: Vec<_> = screen
        .lines()
        .filter_map(|line| {
            let (_, body) = line.split_once('│')?;
            let (body, _) = body.rsplit_once('│')?;
            let body = body.trim();
            (!body.is_empty()).then_some(body)
        })
        .collect();
    assert_eq!(
        snapshot,
        [
            "40 calls · 20 failed (50%) · last 7 days",
            "▸ scripts edit · stale_edit ×14 · 2h ago",
            "shell run · blocked ×4 · 5m ago",
            "fs read · not_found ×2 · 2d ago",
        ]
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| super::harness::render(f, &app)).unwrap();
    let counts: Vec<_> = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .filter(|cell| cell.symbol() == "×")
        .collect();
    assert_eq!(counts.len(), 3);
    assert!(counts.iter().all(|cell| cell.fg == app.theme.accent));
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn harness_popup_empty_state_snapshots() {
    use nexus_core::db::HarnessSummary;

    let mut app = test_app();
    let root = app.space.root.clone();
    for (incognito, calls, expected) in [
        (false, 0, "no tool failures in the last 7 days"),
        (true, 0, "nothing recorded in incognito"),
        (true, 5, "no tool failures in the last 7 days"),
    ] {
        app.incognito = incognito;
        app.harness = Some(HarnessSummary {
            calls,
            failures: 0,
            rows: Vec::new(),
        });
        let screen = render_to_string(120, 30, |f| super::harness::render(f, &app));
        let snapshot: Vec<_> = screen
            .lines()
            .filter_map(|line| {
                let (_, body) = line.split_once('│')?;
                let (body, _) = body.rsplit_once('│')?;
                let body = body.trim();
                (!body.is_empty()).then_some(body)
            })
            .collect();
        assert_eq!(
            snapshot,
            [
                format!("{calls} calls · 0 failed (0%) · last 7 days"),
                expected.to_string(),
            ]
        );
    }
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_escapes_terminal_controls_in_the_header_and_command() {
    let (mut app, _receiver) = approval_app("shell", "$ echo \x1b[2K\rhidden\tend\nnext".into());
    let root = app.space.root.clone();
    app.pending_approval_request.as_mut().unwrap().action = "run\x7f\n".into();
    app.sync_approval_popup();
    let screen = render_to_string(120, 40, |f| super::approval::render(f, &app));
    assert!(screen.contains("shell · run\\u{7f}\\u{a}"), "{screen}");
    assert!(
        screen.contains("$ echo \\u{1b}[2K\\u{d}hidden    end"),
        "{screen}"
    );
    assert!(!screen.chars().any(|c| c.is_control() && c != '\n'));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| super::approval::render(f, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let escapes: Vec<_> = buffer
        .content
        .iter()
        .filter(|cell| cell.symbol() == "\\")
        .collect();
    assert!(!escapes.is_empty());
    assert!(escapes.iter().all(|cell| cell.fg == app.theme.fg_dim));
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_overflow_is_counted_scrollable_and_reset_for_each_request() {
    let summary = (0..40)
        .map(|i| format!("+row {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (mut app, _receiver) = approval_app("fs", summary.clone());
    let root = app.space.root.clone();
    let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
    assert!(
        screen.contains("↑↓ scroll · y allow · a allow for this session · n deny"),
        "{screen}"
    );
    let (marker_row, marker) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("more lines"))
        .unwrap();
    let remaining: usize = marker
        .split_once('…')
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let shown = screen.lines().filter(|line| line.contains("+row ")).count();
    assert_eq!(shown + remaining, 40);
    assert!(remaining > 0);
    assert!(screen.lines().nth(marker_row + 1).unwrap().contains('╰'));
    for (key, offset) in [
        (crossterm::event::KeyCode::Down, 1),
        (crossterm::event::KeyCode::Char('j'), 2),
        (crossterm::event::KeyCode::Up, 1),
        (crossterm::event::KeyCode::Char('k'), 0),
    ] {
        super::approval::handle_key(&mut app, key.into()).unwrap();
        assert_eq!(app.approval_scroll.get(), offset);
    }
    while app.approval_scroll.get() < app.approval_max_scroll.get() {
        super::approval::handle_key(&mut app, crossterm::event::KeyCode::PageDown.into()).unwrap();
        render_to_string(120, 24, |f| super::approval::render(f, &app));
    }
    let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
    assert!(screen.contains("+row 39"), "{screen}");
    assert!(!screen.contains("more lines"), "{screen}");
    super::approval::handle_key(&mut app, crossterm::event::KeyCode::PageUp.into()).unwrap();
    assert!(app.approval_scroll.get() < app.approval_max_scroll.get());
    let opened = app.approval_opened_at.unwrap();
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    app.pending_approval = Some(sender);
    app.pending_approval_request.as_mut().unwrap().summary =
        format!("edit another file\n{summary}");
    app.sync_approval_popup();
    assert_eq!(app.approval_scroll.get(), 0);
    assert!(!app.approval_seen_end.get());
    assert!(app.approval_opened_at.unwrap() >= opened);
    render_to_string(120, 24, |f| super::approval::render(f, &app));
    assert!(!app.approval_seen_end.get());
    super::approval::handle_key(&mut app, crossterm::event::KeyCode::Char('a').into()).unwrap();
    assert_eq!(
        receiver.try_recv().unwrap(),
        nexus_core::tools::Decision::AllowForSession
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn queued_approvals_show_their_position_and_reset_the_guards_at_the_head() {
    use crossterm::event::KeyCode;
    use nexus_core::tools::Decision;
    use std::time::{Duration, Instant};

    let command = format!("$ echo {}\necho LAST_COMMAND", "payload ".repeat(500));
    let (mut app, mut first) = approval_app("shell", command);
    let root = app.space.root.clone();
    let request = app.pending_approval_request.clone().unwrap();
    let mut queued = Vec::new();
    for _ in 0..2 {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app.approval_queue.push_back((request.clone(), sender));
        queued.push(receiver);
    }
    let mut third = queued.pop().unwrap();
    let second = queued.pop().unwrap();
    let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
    assert!(screen.contains(" approve · 1 of 3 "), "{screen}");
    // Read the first to its end and let its open delay pass.
    let read_to_end = |app: &mut AppView| {
        while app.approval_scroll.get() < app.approval_max_scroll.get() {
            super::approval::handle_key(app, KeyCode::PageDown.into()).unwrap();
            render_to_string(120, 24, |f| super::approval::render(f, app));
        }
        app.approval_delay = Duration::from_millis(400);
        app.approval_opened_at = Some(Instant::now() - Duration::from_secs(1));
    };
    read_to_end(&mut app);
    super::approval::handle_key(&mut app, KeyCode::Char('y').into()).unwrap();
    assert_eq!(first.try_recv().unwrap(), Decision::Allow);
    // The identical second request reaches the head with fresh guards.
    app.sync_approval_popup();
    let assert_fresh = |app: &AppView| {
        assert_eq!(app.popup, nexus_core::app::Popup::Approval);
        assert_eq!(app.approval_request.as_ref(), Some(&request));
        assert_eq!(app.approval_scroll.get(), 0);
        assert!(!app.approval_seen_end.get());
        assert!(
            app.approval_opened_at
                .is_some_and(|opened| opened.elapsed() < Duration::from_secs(1))
        );
    };
    assert_fresh(&app);
    let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
    assert!(screen.contains(" approve · 1 of 2 "), "{screen}");
    read_to_end(&mut app);
    // Its tool call going away shows the third, again with fresh guards.
    drop(second);
    app.sync_approval_popup();
    assert_fresh(&app);
    let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
    assert!(screen.contains(" approve "), "{screen}");
    assert!(!screen.contains("1 of"), "{screen}");
    app.approval_opened_at = Some(Instant::now() - Duration::from_secs(1));
    super::approval::handle_key(&mut app, KeyCode::Char('y').into()).unwrap();
    assert!(third.try_recv().is_err(), "the unread command stays locked");
    read_to_end(&mut app);
    super::approval::handle_key(&mut app, KeyCode::Char('y').into()).unwrap();
    assert_eq!(third.try_recv().unwrap(), Decision::Allow);
    app.sync_approval_popup();
    assert_eq!(app.popup, nexus_core::app::Popup::None);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_requires_the_end_of_a_wrapped_shell_command_before_allowing() {
    use crossterm::event::KeyCode;
    use nexus_core::tools::Decision;

    for (key, decision) in [('y', Decision::Allow), ('a', Decision::AllowForSession)] {
        let (mut app, mut receiver) = approval_app(
            "shell",
            format!("$ echo {}\necho LAST_COMMAND", "payload ".repeat(500)),
        );
        let root = app.space.root.clone();
        let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
        assert!(screen.contains("scroll to the end to allow"), "{screen}");
        assert!(screen.contains("more lines"), "{screen}");
        super::approval::handle_key(&mut app, KeyCode::Char(key).into()).unwrap();
        assert!(receiver.try_recv().is_err());
        assert!(app.pending_approval.is_some());
        while app.approval_scroll.get() < app.approval_max_scroll.get() {
            super::approval::handle_key(&mut app, KeyCode::PageDown.into()).unwrap();
            render_to_string(120, 24, |f| super::approval::render(f, &app));
        }
        let screen = render_to_string(120, 24, |f| super::approval::render(f, &app));
        assert!(screen.contains("echo LAST_COMMAND"), "{screen}");
        assert!(!screen.contains("scroll to the end to allow"), "{screen}");
        super::approval::handle_key(&mut app, KeyCode::PageUp.into()).unwrap();
        render_to_string(120, 24, |f| super::approval::render(f, &app));
        super::approval::handle_key(&mut app, KeyCode::Char(key).into()).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), decision);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn approval_ignores_allow_keys_during_the_open_delay_but_never_delays_denial() {
    use crossterm::event::KeyCode;
    use nexus_core::tools::Decision;
    use std::time::{Duration, Instant};

    let (mut app, mut receiver) = approval_app("shell", "$ true".into());
    let root = app.space.root.clone();
    render_to_string(120, 24, |f| super::approval::render(f, &app));
    app.approval_delay = Duration::from_millis(400);
    app.approval_opened_at = Some(Instant::now() + Duration::from_secs(1));
    for key in ['y', 'a'] {
        super::approval::handle_key(&mut app, KeyCode::Char(key).into()).unwrap();
        assert!(receiver.try_recv().is_err());
        assert!(app.pending_approval.is_some());
    }
    app.approval_opened_at = Some(Instant::now() - Duration::from_millis(401));
    super::approval::handle_key(&mut app, KeyCode::Char('y').into()).unwrap();
    assert_eq!(receiver.try_recv().unwrap(), Decision::Allow);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
    for key in [KeyCode::Char('n'), KeyCode::Esc] {
        let (mut app, mut receiver) = approval_app("shell", "$ true".into());
        let root = app.space.root.clone();
        app.approval_delay = Duration::from_millis(400);
        super::approval::handle_key(&mut app, key.into()).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), Decision::Deny);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn approval_popup_combined_wave_snapshot() {
    let (mut app, _receiver) = approval_app(
        "wave",
        "2 edits, 1 command\n1. fs edit\n  edit one\n  --- a/one\n  +++ b/one\n  @@ -1 +1 @@\n  -old\n  +new\n2. fs write\n  write two\n3. shell run\n  $ cargo test".into(),
    );
    let root = app.space.root.clone();
    app.pending_approval_request.as_mut().unwrap().action = "3 calls".into();
    app.sync_approval_popup();
    let screen = render_to_string(120, 40, |f| super::approval::render(f, &app));
    assert!(screen.contains(" approve "), "{screen}");
    assert!(
        screen.contains("y approve all · a all for this session · r review each · n deny all"),
        "{screen}"
    );
    let snapshot: Vec<_> = screen
        .lines()
        .filter_map(|line| {
            let (_, body) = line.split_once('│')?;
            let (body, _) = body.rsplit_once('│')?;
            let body = body.trim_end();
            (!body.trim().is_empty()).then_some(body)
        })
        .collect();
    assert_eq!(
        snapshot,
        [
            " approve · 3 calls",
            " 2 edits, 1 command",
            " 1. fs edit",
            "   edit one",
            "   --- a/one",
            "   +++ b/one",
            "   @@ -1 +1 @@",
            "   -old",
            "   +new",
            " 2. fs write",
            "   write two",
            " 3. shell run",
            "   $ cargo test",
        ]
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_popup_shell_and_diff_snapshots() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.settings.hide_hints = false;
    app.popup = nexus_core::app::Popup::Approval;
    for (tool, action, summary, expected) in [
        (
            "shell",
            "run",
            "$ cargo test\nin crates/core",
            vec!["shell · run", "$ cargo test", "in crates/core"],
        ),
        (
            "fs",
            "write",
            "write file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
            vec![
                "fs · write",
                "write file",
                "--- a/file",
                "+++ b/file",
                "@@ -1 +1 @@",
                "-old",
                "+new",
            ],
        ),
    ] {
        app.approval_request = Some(nexus_core::tools::ApprovalRequest {
            tool: tool.into(),
            action: action.into(),
            summary: summary.into(),
            allow_session: true,
        });
        let screen = render_to_string(120, 40, |f| super::approval::render(f, &app));
        assert!(screen.contains(" approve "), "{screen}");
        assert!(
            screen.contains("y allow · a allow for this session · n deny"),
            "{screen}"
        );
        let snapshot: Vec<_> = screen
            .lines()
            .filter_map(|line| {
                let (_, body) = line.split_once('│')?;
                let (body, _) = body.rsplit_once('│')?;
                let body = body.trim();
                (!body.is_empty()).then_some(body)
            })
            .collect();
        assert_eq!(snapshot, expected);
    }
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_status_tag_is_mode_specific_and_left_truncates_long_roots() {
    let mut app = test_app();
    let root = std::env::temp_dir()
        .join(format!("nexus-status-{}", uuid::Uuid::new_v4()))
        .join("a-long-parent-directory/deep-project");
    std::fs::create_dir_all(&root).unwrap();
    let chat = render_to_string(96, 1, |f| {
        crate::ui::status::render_status(f, &app, f.area());
    });
    assert!(!chat.contains("agent ·"));
    app.run_command(&format!("agent {}", root.display()))
        .unwrap();
    let agent = render_to_string(96, 1, |f| {
        crate::ui::status::render_status(f, &app, f.area());
    });
    assert!(agent.contains("agent · …"), "{agent}");
    assert!(agent.contains("deep-project"), "{agent}");
    assert!(agent.contains(crate::ui::style::glyph::TOOL), "{agent}");
    app.run_command("chat").unwrap();
    let chat = render_to_string(96, 1, |f| {
        crate::ui::status::render_status(f, &app, f.area());
    });
    assert!(!chat.contains("agent ·"));
    std::fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
}

#[test]
fn agent_status_abbreviates_the_home_directory() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let mut app = test_app();
    app.workspace = Some(std::sync::Arc::new(
        nexus_core::workspace::Workspace::open(std::path::Path::new(&home), Vec::new()).unwrap(),
    ));
    app.work_mode = nexus_core::app::WorkMode::Agent;
    let status = render_to_string(96, 1, |f| {
        crate::ui::status::render_status(f, &app, f.area());
    });
    assert!(status.contains("agent · ~"), "{status}");
}

#[test]
fn model_authored_ui_is_inline_and_focusable_without_game_commands() {
    let mut app = test_app();
    app.incognito = true;
    app.set_input("keep my draft");
    let source = include_str!("../../../../core/tests/fixtures/interactive/counter.json").trim();
    app.messages.push(nexus_core::db::Message {
        role: "assistant".into(),
        content: format!("Before\n\n```nexus-ui\n{source}\n```\n\nAfter"),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    let screen = render_to_string(100, 40, |f| crate::ui::render(f, &mut app));
    for label in [
        "Before",
        "Counter",
        "Count: 0",
        "Add one",
        "Name",
        "Submit result [review]",
        "After",
    ] {
        assert!(screen.contains(label), "{screen}");
    }
    assert!(
        !screen.contains("\"version\""),
        "valid UI should not dump JSON"
    );
    let block = app.history_cache.code[app.history_cache.widgets[0].row].unwrap();
    assert_eq!(app.history_cache.blocks[block], source);
    crate::interactive::focus(&mut app);
    assert_eq!(app.popup, nexus_core::app::Popup::InteractiveUi);
    crate::interactive::key(&mut app, crossterm::event::KeyCode::Enter.into());
    let screen = render_to_string(100, 40, |f| crate::ui::render(f, &mut app));
    assert!(screen.contains("Count: 1"), "{screen}");
    assert_eq!(app.input_text(), "keep my draft");
    assert_eq!(app.messages.len(), 1);
    crate::interactive::key(&mut app, crossterm::event::KeyCode::Esc.into());
    assert_eq!(app.popup, nexus_core::app::Popup::None);
    let screen = render_to_string(100, 40, |f| crate::ui::render(f, &mut app));
    assert!(
        screen.contains("Count: 1"),
        "focus should share inline state: {screen}"
    );
    app.apply_event(&nexus_core::app::AppEvent::ViewportReset);
    let screen = render_to_string(100, 40, |f| crate::ui::render(f, &mut app));
    assert!(screen.contains("Count: 0"), "{screen}");
}

#[test]
fn local_popup_lists_runtimes_with_discovery_and_endpoint() {
    let mut app = test_app();
    app.core.saved.local = Some(nexus_core::provider::local::LocalConfig {
        provider: nexus_core::provider::local::LocalRuntime::Mlx,
        endpoint: None,
        list_command: None,
        memory_budget_mb: None,
        context_length: None,
        kv_cache_q8: false,
        parallel_tool_calls: None,
        tier: None,
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
        wide.contains("● 4.2 GB · ⚠ over budget · started here · http://localhos…"),
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
    assert!(screen.contains("⟳ searcher r1 1/4"), "{screen}");
    assert!(screen.contains("Searching the web"), "{screen}");
    assert!(screen.contains("✗ verifier"), "{screen}");
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
        crate::ui::status::render_status(f, &app, f.area());
    });
    assert!(
        screen.contains("◆ Big-Model-7B · local · ⌂ work"),
        "{screen}"
    );
    assert!(!screen.contains("local:org/"), "{screen}");
    assert!(screen.contains("ready"), "{screen}");
    assert!(
        screen.contains('▱'),
        "context meter on the right:\n{screen}"
    );

    // A status too long for the bar ends in an ellipsis instead of a cut.
    app.status = "x".repeat(200);
    let screen = render_to_string(100, 1, |f| {
        crate::ui::status::render_status(f, &app, f.area());
    });
    assert!(screen.contains('…'), "{screen}");
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
        crate::ui::short_model_label("opencode:go:deepseek-v4-flash"),
        "deepseek-v4-flash · go"
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
    let mut app = test_app();
    app.open_help();
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    // Corner cell: behind the popup, receded. Center cell: inside it.
    assert_eq!(buf[(0, 0)].fg, app.theme.border_dim);
    let inside = (0..100)
        .map(|x| buf[(x, 20)].fg)
        .filter(|c| *c != app.theme.border_dim)
        .count();
    assert!(inside > 0, "popup text stays bright");
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

#[test]
fn status_badge_keeps_the_backend_tag_when_shortened() {
    let long = "Ternary-Bonsai-2-27B-mlx-2bit · local";
    let fitted = crate::ui::status::fit_badge("", long, 24);
    assert_eq!(fitted.chars().count(), 24);
    assert!(fitted.ends_with(" · local"), "{fitted}");
    assert_eq!(
        crate::ui::status::fit_badge("[work] ", "gpt-5.5", 40),
        "[work] gpt-5.5"
    );
}

#[test]
fn context_popup_fits_its_content_and_ctrl_g_closes_it() {
    let mut app = test_app();
    app.popup = nexus_core::app::Popup::Context;
    let screen = render_to_string(100, 40, |f| super::context::render(f, &app));
    assert!(screen.contains("unknown window"), "{screen}");
    assert!(screen.contains('█'), "per-bucket bars:\n{screen}");
    let framed_rows = screen.lines().filter(|l| l.contains('│')).count();
    assert!(
        framed_rows < 14,
        "sized to content, not 40% of the screen:\n{screen}"
    );

    let ctrl_g = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('g'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    super::context::handle_key(&mut app, ctrl_g);
    assert!(app.popup == nexus_core::app::Popup::None);
}

#[test]
fn research_steer_input_ignores_ctrl_chords() {
    let mut app = test_app();
    app.popup = nexus_core::app::Popup::ResearchLive;
    let key = |c, m| crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(c), m);
    super::research_live::handle_key(&mut app, key('a', crossterm::event::KeyModifiers::CONTROL));
    super::research_live::handle_key(&mut app, key('b', crossterm::event::KeyModifiers::NONE));
    assert_eq!(app.core.research_live_input, "b");
}

// Async: an empty catalog makes the picker kick off a model fetch.
#[tokio::test]
async fn plain_m_opens_the_swarm_persona_model_picker() {
    let mut app = test_app();
    app.core.swarm_cache = vec![nexus_core::db::Persona {
        name: "Critic".into(),
        model: "a/one".into(),
        blurb: "pokes holes".into(),
    }];
    app.popup = nexus_core::app::Popup::Swarm;
    super::swarm::handle_key(
        &mut app,
        crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('m')),
    )
    .unwrap();
    assert!(matches!(
        app.core.model_pick_target,
        nexus_core::app::ModelPickTarget::SwarmPersona(0)
    ));
}

#[test]
fn edit_fields_ignore_ctrl_and_alt_chords() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |m| KeyEvent::new(KeyCode::Char('a'), m);
    assert!(super::classify_edit_key(key(KeyModifiers::CONTROL)).is_none());
    assert!(super::classify_edit_key(key(KeyModifiers::ALT)).is_none());
    assert!(matches!(
        super::classify_edit_key(key(KeyModifiers::SHIFT)),
        Some(super::EditAction::Push('a'))
    ));
}

#[test]
fn notification_toasts_hug_their_content() {
    let mut app = test_app();
    app.core.notifications.push_back(ChatNotification {
        session_id: "s1".into(),
        title: "Japanese Fluency Roadmap".into(),
        text: "response complete".into(),
        success: true,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    let (rect, _) = app.notification_areas[0];
    assert!(
        rect.width < 64,
        "toast sized to its text, got {}",
        rect.width
    );
    let col = crate::ui::reading_column(ratatui::layout::Rect::new(0, 0, 120, 30));
    assert_eq!(
        rect.x + rect.width,
        col.right() - 1,
        "flush with the column's edge, clear of the scrollbar gutter"
    );
    let buf = terminal.backend().buffer();
    let row: String = (rect.x..rect.x + rect.width)
        .map(|x| buf[(x, rect.y)].symbol())
        .collect();
    assert!(
        row.starts_with("▎✓ Japanese Fluency Roadmap · response complete"),
        "{row}"
    );
}

#[test]
fn wrapped_list_items_hang_under_their_text() {
    let lines = crate::ui::cards::wrap_plain(
        " 1. Which specific outcomes or metrics of effectiveness matter here?\n- a bullet that wraps onto a second line",
        30,
    );
    assert!(lines[0].starts_with(" 1. Which"), "{lines:#?}");
    assert!(
        lines[1].starts_with("    ") && !lines[1].starts_with("     "),
        "{lines:#?}"
    );
    let bullet = lines
        .iter()
        .position(|l| l.starts_with("- a bullet"))
        .unwrap();
    assert!(
        lines[bullet + 1].starts_with("  ") && !lines[bullet + 1].starts_with("   "),
        "{lines:#?}"
    );
}
