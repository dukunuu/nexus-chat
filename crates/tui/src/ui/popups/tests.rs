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
            "morning · agent · interval · enabled · never run",
            "▸ fix tests · agent · until_done · enabled · parked",
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
        class: None,
        detail: None,
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
        screen.contains("y allow once · a allow all edits this session · n deny"),
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
    // Below the preview: a spacer, the pinned key row, then the border.
    assert!(screen.lines().nth(marker_row + 3).unwrap().contains('╰'));
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
        assert!(screen.contains("scroll to the end to enable y"), "{screen}");
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
        assert!(
            !screen.contains("scroll to the end to enable y"),
            "{screen}"
        );
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
    // The snapshot wraps at the old fixed 112-cell column; `auto` is wider now.
    app.settings.column_width = "112".into();
    app.pending_approval_request.as_mut().unwrap().action = "3 calls".into();
    app.sync_approval_popup();
    let screen = render_to_string(120, 40, |f| super::approval::render(f, &app));
    assert!(screen.contains(" approve "), "{screen}");
    assert!(
        screen.contains("y approve all · a + session · r one by one · n deny all"),
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
            " The agent wants to make these 3 calls together. Each is shown below; y approves all of them, r lets you",
            " decide one by one.",
            " ▍ 1  edit one  +1 −1",
            " 1 − old",
            " 1 + new",
            " ▍ 2  write two",
            " ▍ 3  shell · run",
            " $ cargo test",
            " y approve all · a + session · r one by one · n deny all",
        ]
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_edit_at_80_columns_has_colored_diff_and_class_badge() {
    let (mut app, _receiver) = approval_app("fs", "edit src/app.rs (+2 −1)\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -41,2 +41,3 @@\n let cfg = load()?;\n-let n = cfg.limit;\n+let n = cfg.limit.max(1);\n+let m = n * 2;".into());
    let root = app.space.root.clone();
    app.approval_request.as_mut().unwrap().class = Some("write in workspace".into());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| super::approval::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let snapshot: Vec<_> = (0..24)
        .filter_map(|y| {
            let row: String = (0..80).map(|x| buffer[(x, y)].symbol()).collect();
            let (_, body) = row.split_once('│')?;
            let (body, _) = body.rsplit_once('│')?;
            let body = body.trim();
            (!body.is_empty()).then(|| body.to_string())
        })
        .collect();
    assert_eq!(
        snapshot,
        [
            "edit src/app.rs  +2 −1                                  write in workspace",
            "The agent wants to change src/app.rs: 2 lines added, 1 line removed.",
            "41   let cfg = load()?;",
            "42 − let n = cfg.limit;",
            "42 + let n = cfg.limit.max(1);",
            "43 + let m = n * 2;",
            "y allow once · a allow all edits this session · n deny",
        ]
    );
    for (symbol, color) in [("+", app.theme.success), ("−", app.theme.error)] {
        let cells: Vec<_> = buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() == symbol)
            .collect();
        assert!(cells.len() >= 2);
        assert!(cells.iter().all(|cell| cell.fg == color));
    }
    let badge = buffer
        .content
        .iter()
        .find(|cell| cell.symbol() == "w")
        .unwrap();
    assert_eq!(badge.fg, app.theme.fg_dim);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_shell_warning_is_above_bold_command_and_delay_dims_keys() {
    let (mut app, _receiver) = approval_app(
        "shell",
        "could not parse safely: computed command word\n$ \"$COMMAND\"\nin sub".into(),
    );
    let root = app.space.root.clone();
    app.approval_request.as_mut().unwrap().class = Some("runs a command".into());
    app.approval_delay = std::time::Duration::from_secs(10);
    app.approval_opened_at = Some(std::time::Instant::now());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| super::approval::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let warning = buffer
        .content
        .iter()
        .position(|cell| cell.symbol() == "c" && cell.fg == app.theme.warning)
        .unwrap();
    let command = buffer
        .content
        .iter()
        .position(|cell| cell.symbol() == "$")
        .unwrap();
    assert!(warning < command);
    assert!(
        buffer.content[command]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "y"
                && cell.modifier.contains(ratatui::style::Modifier::DIM))
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn approval_auto_evidence_preserves_diff_colors_and_escaped_controls() {
    let (app, _receiver) = approval_app(
        "fs",
        "in_scope 0.50 · risk: uncertain\nedit file (+1 −1)\ndiff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\x1b[2J".into(),
    );
    let root = app.space.root.clone();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| super::approval::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let screen = render_to_string(80, 24, |frame| super::approval::render(frame, &app));
    assert!(screen.contains("edit file  +1 −1"));
    assert!(screen.contains("in_scope 0.50 · risk: uncertain"));
    assert!(screen.contains("new\\u{1b}[2J"));
    assert!(!screen.contains("--- a/file"));
    let escapes: Vec<_> = buffer
        .content
        .iter()
        .filter(|cell| cell.symbol() == "\\")
        .collect();
    assert!(!escapes.is_empty());
    assert!(escapes.iter().all(|cell| cell.fg == app.theme.fg_dim));
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "n" && cell.fg == app.theme.success)
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
            vec![
                "shell · run",
                "The agent wants to run this command:",
                "$ cargo test",
                "in crates/core",
                "y allow once · a allow all commands this session · n deny",
            ],
        ),
        (
            "fs",
            "write",
            "write file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
            vec![
                "write file  +1 −1",
                "The agent wants to replace the contents of file: 1 line added, 1 line removed.",
                "1 − old",
                "1 + new",
                "y allow once · a allow all edits this session · n deny",
            ],
        ),
    ] {
        app.approval_request = Some(nexus_core::tools::ApprovalRequest {
            class: None,
            detail: None,
            tool: tool.into(),
            action: action.into(),
            summary: summary.into(),
            allow_session: true,
        });
        let screen = render_to_string(120, 40, |f| super::approval::render(f, &app));
        assert!(screen.contains(" approve "), "{screen}");
        assert!(
            screen.contains(if tool == "fs" {
                "y allow once · a allow all edits this session · n deny"
            } else {
                "y allow once · a allow all commands this session · n deny"
            }),
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
        "Submit result ↵",
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
    assert!(screen.contains("Ollama / llama.cpp / MLX"), "{screen}");
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
fn settings_popup_has_one_model_row_that_defaults_to_auto() {
    let mut app = test_app();
    app.open_settings();
    // Tall enough that every row is on screen.
    let screen = render_to_string(120, 60, |f| super::settings::render(f, &app));

    let model_rows: Vec<&str> = screen
        .lines()
        .filter(|line| line.to_lowercase().contains("model"))
        .collect();
    assert_eq!(
        model_rows.len(),
        1,
        "exactly one model row on screen:\n{screen}"
    );
    assert!(
        model_rows[0].contains("helper model") && model_rows[0].contains("auto"),
        "{screen}"
    );
    for gone in [
        "memory model",
        "image model",
        "OCR",
        "embedding",
        "image gen",
        "video gen",
        "Voice & Vision",
        "Image Generation",
        "Video Generation",
    ] {
        assert!(!screen.contains(gone), "{gone} should be gone:\n{screen}");
    }

    // A picked helper model replaces "auto" on the row.
    app.helper_model = "x/helper".into();
    let screen = render_to_string(120, 60, |f| super::settings::render(f, &app));
    assert!(screen.contains("x/helper"), "{screen}");
}

#[test]
fn settings_helper_row_picks_with_enter_and_returns_to_auto_with_backspace() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    app.core.models = vec![nexus_core::provider::Model {
        id: "a/one".into(),
        name: "One".into(),
        reasoning_efforts: Vec::new(),
        context_length: None,
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: nexus_core::provider::BackendTag::OpenRouter,
        pricing: None,
    }];
    select_settings_field(&mut app, SettingsField::HelperModel);

    press(&mut app, KeyCode::Enter);
    assert!(app.popup == nexus_core::app::Popup::Model);
    assert!(app.model_pick_target == nexus_core::app::ModelPickTarget::Helper);
    let screen = render_to_string(160, 30, |f| super::model::render(f, &mut app));
    assert!(screen.contains("picking helper model"), "{screen}");

    app.pick_model_at(nexus_core::app::ModelPanel::Available, 0)
        .unwrap();
    assert_eq!(app.helper_model, "a/one");
    assert!(app.popup == nexus_core::app::Popup::Settings);

    select_settings_field(&mut app, SettingsField::HelperModel);
    press(&mut app, KeyCode::Backspace);
    assert!(app.helper_model.is_empty(), "Backspace returns to auto");
    assert_eq!(app.db.setting("helper_model").as_deref(), Some(""));
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
        super::model::model_popup_areas(&app, ratatui::layout::Rect::new(0, 0, 150, 40));
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
fn truncation_measures_cjk_in_display_columns() {
    use crate::ui::style::width;
    use ratatui::text::{Line, Span};
    let title = "日本語の長いセッションタイトル";
    let cut = super::chrome::truncate(title, 9);
    assert_eq!(cut, "日本語の…");
    assert!(width(&cut) <= 9);
    let middle = super::chrome::truncate_middle("写真_東京タワーと富士山.png", 16);
    assert!(width(&middle) <= 16, "{middle}");
    assert!(
        middle.starts_with("写真") && middle.ends_with(".png"),
        "{middle}"
    );
    let fitted = super::chrome::fit_line(Line::from(vec![Span::raw("ab"), Span::raw("日本語")]), 6);
    assert_eq!(fitted.to_string(), "ab日…");
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
    let col = crate::ui::reading_column(ratatui::layout::Rect::new(0, 0, 120, 30), "auto");
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

#[test]
fn large_write_preview_scrolls_through_the_whole_detail() {
    use crossterm::event::KeyCode;
    let body: String = (1..=120)
        .map(|n| format!("+line {n:03}\n"))
        .collect::<String>();
    let detail = format!(
        "write src/big.rs (+120 −0)\n--- /dev/null\n+++ b/src/big.rs\n@@ -0,0 +1,120 @@\n{}",
        body.trim_end()
    );
    let (mut app, _receiver) =
        approval_app("fs", "write src/big.rs (+120 −0)\n… 80 more lines".into());
    let root = app.space.root.clone();
    {
        let request = app.approval_request.as_mut().unwrap();
        request.action = "write".into();
        request.detail = Some(detail);
    }
    let screen = render_to_string(80, 30, |f| super::approval::render(f, &app));
    assert!(
        screen.contains("The agent wants to create src/big.rs with 120 lines."),
        "{screen}"
    );
    assert!(screen.contains("lines 1–"), "{screen}");
    assert!(!screen.contains("line 120"), "{screen}");
    super::approval::handle_key(&mut app, KeyCode::End.into()).unwrap();
    let screen = render_to_string(80, 30, |f| super::approval::render(f, &app));
    assert!(screen.contains("line 120"), "{screen}");
    super::approval::handle_key(&mut app, KeyCode::Home.into()).unwrap();
    assert_eq!(app.approval_scroll.get(), 0);
    super::approval::handle_key(&mut app, KeyCode::Char(' ').into()).unwrap();
    assert!(app.approval_scroll.get() > 1, "space pages down");
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

fn select_settings_field(app: &mut AppView, field: SettingsField) {
    app.open_settings();
    let rows = app.settings_rows();
    app.settings_selected = rows
        .iter()
        .position(|r| matches!(r, SettingsRow::Field(f) if *f == field))
        .unwrap();
}

fn press(app: &mut AppView, code: crossterm::event::KeyCode) {
    super::settings::handle_key(app, crossterm::event::KeyEvent::from(code)).unwrap();
}

#[test]
fn settings_column_width_row_cycles_and_persists() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    select_settings_field(&mut app, SettingsField::ColumnWidth);
    let screen = render_to_string(100, 30, |f| super::settings::render(f, &app));
    assert!(screen.contains("column width"), "{screen}");
    assert!(screen.contains("auto"), "{screen}");

    // Enter, Space and → step forward; ← steps back. Esc is the only save/close.
    let mut seen = vec![app.settings.column_width.clone()];
    for code in [
        KeyCode::Enter,
        KeyCode::Char(' '),
        KeyCode::Right,
        KeyCode::Enter,
        KeyCode::Char(' '),
        KeyCode::Right,
    ] {
        press(&mut app, code);
        seen.push(app.settings.column_width.clone());
    }
    assert_eq!(
        seen,
        ["auto", "full", "100", "140", "180", "220", "auto"],
        "the cycle wraps"
    );
    assert!(
        app.popup == nexus_core::app::Popup::Settings,
        "Enter cycles, it does not close"
    );
    press(&mut app, KeyCode::Left);
    assert_eq!(app.settings.column_width, "220");
    let stored = app.db.load_settings().unwrap();
    assert!(
        stored
            .iter()
            .any(|(k, v)| k == "column_width" && v == "220")
    );

    // Saving the popup leaves the width alone.
    press(&mut app, KeyCode::Esc);
    assert!(app.popup == nexus_core::app::Popup::None);
    assert_eq!(app.settings.column_width, "220");
}

#[test]
fn settings_column_width_row_shows_a_dragged_number_and_steps_off_it_to_auto() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    app.core.set_setting("column_width", "157").unwrap();
    select_settings_field(&mut app, SettingsField::ColumnWidth);
    let screen = render_to_string(100, 30, |f| super::settings::render(f, &app));
    assert!(screen.contains("157"), "{screen}");
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.settings.column_width, "auto");
    app.core.set_setting("column_width", "157").unwrap();
    press(&mut app, KeyCode::Left);
    assert_eq!(app.settings.column_width, "auto");
}

#[test]
fn the_model_popup_hit_test_follows_its_resized_rect() {
    let ids = ["a/alpha", "b/bravo", "c/charlie"];
    let screen_rect = ratatui::layout::Rect::new(0, 0, 150, 40);
    let mut app = picker_app(&ids);
    app.core.set_setting("popup_sizes", "model:60x50").unwrap();
    let (fav, avail) = super::model::model_popup_areas(&app, screen_rect);
    let outer = fav.union(avail);
    assert_eq!(outer, crate::ui::centered(screen_rect, 60, 50));
    assert_ne!(outer, crate::ui::centered(screen_rect, 82, 74));

    let screen = render_to_string(150, 40, |f| super::model::render(f, &mut app));
    assert_eq!(app.popup_hit.get().map(|hit| hit.rect), Some(outer));
    let inner = super::model::list_inner(avail);
    let available = app.available_models();
    for id in ids {
        let row = screen
            .lines()
            .position(|l| l.contains(id))
            .unwrap_or_else(|| panic!("{id} not drawn:\n{screen}"));
        let index = app.avail_offset + (row - inner.y as usize);
        assert_eq!(available[index].id, id, "clicking {id} picks another row");
    }
}

#[test]
fn every_popup_renders_at_the_minimum_resize_size_without_panicking() {
    use nexus_core::app::Popup;
    // 40x10 is the floor a drag can reach (a smaller terminal gets the same).
    for (w, h) in [(40, 10), (30, 8), (60, 14)] {
        for popup in [
            Popup::Model,
            Popup::Session,
            Popup::Copy,
            Popup::Key,
            Popup::Settings,
            Popup::Space,
            Popup::Context,
            Popup::Skills,
            Popup::Files,
            Popup::Apps,
            Popup::Watch,
            Popup::ResearchLive,
            Popup::Swarm,
            Popup::Usage,
            Popup::Jobs,
            Popup::Harness,
            Popup::Procs,
            Popup::Login,
            Popup::Local,
            Popup::Help,
            Popup::Connectors,
        ] {
            let mut app = test_app();
            app.core
                .set_setting(
                    "popup_sizes",
                    "model:20x20,session:20x20,copy:20x20,key:20x20,settings:20x20,space:20x20,\
                     skills:20x20,files:20x20,apps:20x20,watches:20x20,\
                     research:20x20,swarm:20x20,usage:20x20,jobs:20x20,harness:20x20,\
                     procs:20x20,login:20x20,local:20x20,help:20x20,connectors:20x20",
                )
                .unwrap();
            app.popup = popup;
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        }
    }
}

#[test]
fn help_popup_tells_you_the_borders_and_column_edge_can_be_dragged() {
    let mut app = test_app();
    app.execute(nexus_core::app::AppCommand::OpenHelp).unwrap();
    let screen = render_to_string(140, 120, |f| super::help::render(f, &mut app));
    assert!(
        screen.contains(
            "drag a popup's border to resize it · drag the column edge to widen the conversation"
        ),
        "{screen}"
    );
}

// ----- /connectors ---------------------------------------------------------

fn body_lines(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter_map(|line| {
            let (_, body) = line.split_once('│')?;
            let (body, _) = body.rsplit_once('│')?;
            let body = body.trim();
            (!body.is_empty()).then(|| body.to_string())
        })
        .collect()
}

fn tool_info(
    name: &str,
    description: &str,
    annotations: nexus_core::connectors::protocol::Annotations,
) -> nexus_core::connectors::protocol::ToolInfo {
    nexus_core::connectors::protocol::ToolInfo {
        name: name.into(),
        title: None,
        description: description.into(),
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
        annotations,
    }
}

fn connectors_app() -> AppView {
    use nexus_core::connectors::protocol::Annotations;
    use nexus_core::connectors::{ConnectorConfig, Status};
    let mut app = test_app();
    app.settings.hide_hints = false;
    let registry = app.connectors.clone();
    registry
        .add(
            "linear",
            ConnectorConfig::remote("https://mcp.linear.app/mcp"),
        )
        .unwrap();
    registry
        .add(
            "github",
            ConnectorConfig::remote("https://api.githubcopilot.com/mcp/"),
        )
        .unwrap();
    registry
        .add(
            "notes",
            ConnectorConfig::local(vec!["npx".into(), "-y".into(), "notes-server".into()]),
        )
        .unwrap();
    registry
        .add(
            "flaky",
            ConnectorConfig::remote("https://flaky.example/mcp"),
        )
        .unwrap();
    registry
        .add(
            "quiet",
            ConnectorConfig::remote("https://quiet.example/mcp"),
        )
        .unwrap();
    let read_only = Annotations {
        read_only: Some(true),
        open_world: Some(false),
        raw: Some("{}".into()),
        ..Annotations::default()
    };
    let writes = Annotations {
        read_only: Some(false),
        destructive: Some(true),
        raw: Some("{}".into()),
        ..Annotations::default()
    };
    registry.force_state(
        "linear",
        Status::Connected { tools: 3 },
        vec![
            tool_info("get_issue", "Fetch one issue by id.", read_only.clone()),
            tool_info(
                "create_issue",
                "Create an issue.",
                Annotations {
                    destructive: Some(false),
                    ..writes.clone()
                },
            ),
            tool_info("delete_issue", "Delete an issue for good.", writes),
        ],
    );
    registry.force_state(
        "github",
        Status::NeedsAuth {
            resource_metadata: None,
        },
        Vec::new(),
    );
    registry.force_state(
        "notes",
        Status::Connected { tools: 1 },
        vec![tool_info("add", "Add a note.", read_only)],
    );
    registry.force_state(
        "flaky",
        Status::Error("tools/list failed: timed out after 20s".into()),
        Vec::new(),
    );
    registry.set_enabled("quiet", false).unwrap();
    app
}

fn conn_press(app: &mut AppView, code: crossterm::event::KeyCode) {
    super::connectors::handle_key(app, crossterm::event::KeyEvent::from(code));
}

fn type_text(app: &mut AppView, text: &str) {
    for c in text.chars() {
        conn_press(app, crossterm::event::KeyCode::Char(c));
    }
}

#[test]
fn connectors_list_shows_status_dots_tool_counts_and_the_selected_detail() {
    let mut app = connectors_app();
    app.execute(nexus_core::app::AppCommand::OpenConnectors)
        .unwrap();
    assert_eq!(app.popup, nexus_core::app::Popup::Connectors);
    let screen = render_to_string(120, 30, |f| super::connectors::render(f, &app));
    assert!(screen.contains(" connectors "), "{screen}");
    assert_eq!(
        body_lines(&screen),
        [
            "▸ ✗ flaky  · hosted · error",
            "⚠ github · hosted · sign-in needed",
            "● linear · hosted · 3 tools · connected",
            "● notes  · local · 1 tool · connected",
            "○ quiet  · hosted · off",
            "──────────────────────────────────────────────────────────────────────────────────────────",
            "https://flaky.example/mcp",
            "error: tools/list failed: timed out after 20s",
        ]
    );
    assert!(
        screen.contains("5 connectors · ⏎ tools · a add"),
        "{screen}"
    );
    // Moving the cursor changes the detail strip.
    conn_press(&mut app, crossterm::event::KeyCode::Down);
    let screen = render_to_string(120, 30, |f| super::connectors::render(f, &app));
    assert!(
        screen.contains("https://api.githubcopilot.com/mcp/"),
        "{screen}"
    );
    assert!(screen.contains("sign in"), "{screen}");
    conn_press(&mut app, crossterm::event::KeyCode::Esc);
    assert_eq!(app.popup, nexus_core::app::Popup::None);
}

#[test]
fn connectors_empty_state_points_at_add_and_import() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    super::connectors::open(&mut app);
    let screen = render_to_string(120, 24, |f| super::connectors::render(f, &app));
    assert!(screen.contains("no connectors yet"), "{screen}");
    assert!(screen.contains("a adds one, i imports"), "{screen}");
    // Keys on an empty list are harmless.
    for code in [
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyCode::Char('d'),
        crossterm::event::KeyCode::Char('e'),
        crossterm::event::KeyCode::Char('l'),
        crossterm::event::KeyCode::Down,
    ] {
        conn_press(&mut app, code);
    }
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
}

#[test]
fn connectors_enter_opens_tools_with_badges_and_keys_set_the_policy() {
    use crossterm::event::KeyCode;
    let mut app = connectors_app();
    super::connectors::open(&mut app);
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::Tools);
    let screen = render_to_string(120, 30, |f| super::connectors::render(f, &app));
    let lines = body_lines(&screen);
    assert_eq!(
        lines[..3],
        [
            "▸ ask    create_issue · writes · reaches the internet",
            "ask    delete_issue · writes · may delete · reaches the internet",
            "free   get_issue · read-only",
        ]
        .map(String::from)
    );
    assert!(screen.contains("connectors · linear"), "{screen}");
    assert!(
        screen.contains("the model sees it as linear__create_issue"),
        "{screen}"
    );
    // allow, then deny, then back to the default.
    conn_press(&mut app, KeyCode::Char('a'));
    let screen = render_to_string(120, 30, |f| super::connectors::render(f, &app));
    assert!(
        body_lines(&screen)[0].contains("allow  create_issue"),
        "{screen}"
    );
    assert_eq!(
        app.connectors
            .config_of("linear")
            .unwrap()
            .policy("create_issue"),
        Some(nexus_core::connectors::Policy::Allow)
    );
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Char('d'));
    let screen = render_to_string(120, 30, |f| super::connectors::render(f, &app));
    assert!(
        body_lines(&screen)[1].contains("deny   delete_issue"),
        "{screen}"
    );
    // A denied tool is no longer offered to the model.
    assert!(
        !app.connectors
            .exposure()
            .defs
            .iter()
            .any(|d| d.name == "linear__delete_issue")
    );
    conn_press(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.connectors
            .config_of("linear")
            .unwrap()
            .policy("delete_issue"),
        None
    );
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Char('s'));
    assert_eq!(
        app.connectors
            .config_of("linear")
            .unwrap()
            .policy("get_issue"),
        Some(nexus_core::connectors::Policy::Ask)
    );
    conn_press(&mut app, KeyCode::Esc);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
}

#[test]
fn connectors_e_toggles_a_connector_off_and_on() {
    use crossterm::event::KeyCode;
    let mut app = connectors_app();
    super::connectors::open(&mut app);
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Down);
    assert!(app.connectors.infos()[2].enabled);
    conn_press(&mut app, KeyCode::Char('e'));
    let screen = render_to_string(120, 30, |f| super::connectors::render(f, &app));
    assert!(screen.contains("linear · hosted · off"), "{screen}");
    assert!(!app.connectors.infos()[2].enabled);
    conn_press(&mut app, KeyCode::Char('e'));
    assert!(app.connectors.infos()[2].enabled);
}

#[test]
fn connectors_add_flow_masks_the_token_and_stores_it_in_the_keychain() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    app.settings.hide_hints = false;
    super::connectors::open(&mut app);
    conn_press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::Add);
    // An empty target does not advance.
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.connectors_popup.add.step,
        super::connectors::AddStep::Target
    );
    type_text(&mut app, "https://mcp.acme-tools.dev/mcp");
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(app.connectors_popup.add.name, "acme-tools");
    let screen = render_to_string(100, 24, |f| super::connectors::render(f, &app));
    assert!(screen.contains("name: acme-tools"), "{screen}");
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.connectors_popup.add.step,
        super::connectors::AddStep::Auth
    );
    conn_press(&mut app, KeyCode::Tab);
    assert!(app.connectors_popup.add.bearer);
    conn_press(&mut app, KeyCode::Enter);
    type_text(&mut app, "sk-live-TOP-SECRET");
    let screen = render_to_string(100, 24, |f| super::connectors::render(f, &app));
    assert!(screen.contains("token: ******************"), "{screen}");
    assert!(!screen.contains("TOP-SECRET"), "{screen}");
    // Paste lands in the token field too, newline stripped.
    super::connectors::paste(&mut app, "-pasted\n");
    assert!(app.connectors_popup.add.token.ends_with("-pasted"));
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
    let config = app.connectors.config_of("acme-tools").unwrap();
    assert_eq!(config.auth, nexus_core::connectors::AuthKind::Bearer);
    assert_eq!(
        app.connectors
            .vault()
            .get("acme-tools", "bearer")
            .as_deref(),
        Some("sk-live-TOP-SECRET-pasted")
    );
    let screen = render_to_string(100, 24, |f| super::connectors::render(f, &app));
    assert!(screen.contains("acme-tools"), "{screen}");
    assert!(!screen.contains("TOP-SECRET"), "{screen}");
    assert!(!format!("{config:?}").contains("TOP-SECRET"));
}

#[test]
fn connectors_add_flow_handles_local_commands_bad_names_and_back() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    super::connectors::open(&mut app);
    conn_press(&mut app, KeyCode::Char('a'));
    type_text(
        &mut app,
        "npx -y @modelcontextprotocol/server-filesystem ~/notes",
    );
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(app.connectors_popup.add.name, "notes");
    // A bad name stays on the step; Esc steps back; a local command skips auth.
    for _ in 0..5 {
        conn_press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "Bad Name");
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.connectors_popup.add.step,
        super::connectors::AddStep::Name
    );
    conn_press(&mut app, KeyCode::Esc);
    assert_eq!(
        app.connectors_popup.add.step,
        super::connectors::AddStep::Target
    );
    conn_press(&mut app, KeyCode::Enter);
    for _ in 0..8 {
        conn_press(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "notes");
    // Local commands only run from the terminal UI.
    app.interactive_ui = true;
    conn_press(&mut app, KeyCode::Enter);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
    let config = app.connectors.config_of("notes").unwrap();
    assert_eq!(
        config.command,
        [
            "npx",
            "-y",
            "@modelcontextprotocol/server-filesystem",
            "~/notes"
        ]
    );
    assert_eq!(app.connectors.infos()[0].name, "notes");
}

#[test]
fn connectors_remove_asks_first_and_deletes_the_keychain_entries() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = connectors_app();
    app.connectors
        .vault()
        .set("linear", "bearer", "tok-linear-123")
        .unwrap();
    super::connectors::open(&mut app);
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Down);
    conn_press(&mut app, KeyCode::Char('d'));
    let screen = render_to_string(120, 24, |f| super::connectors::render(f, &app));
    assert!(
        screen.contains("remove \"linear\" and its keychain entries?"),
        "{screen}"
    );
    // Anything but a confirmation backs out.
    conn_press(&mut app, KeyCode::Esc);
    assert_eq!(app.connectors.infos().len(), 5);
    conn_press(&mut app, KeyCode::Char('d'));
    super::connectors::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.connectors.infos().len(), 4);
    assert!(
        app.connectors
            .infos()
            .iter()
            .all(|info| info.name != "linear")
    );
    assert_eq!(app.connectors.vault().get("linear", "bearer"), None);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
}

#[test]
fn connectors_import_lists_found_servers_with_checkboxes() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    app.settings.hide_hints = false;
    let dir = app.space.root.clone();
    let desktop = dir.join("claude_desktop_config.json");
    std::fs::write(
        &desktop,
        r#"{"mcpServers": {
            "filesystem": {"command": "npx", "args": ["-y", "server-filesystem"], "env": {"API_KEY": "sk-secret-import"}},
            "linear": {"url": "https://mcp.linear.app/mcp"}
        }}"#,
    )
    .unwrap();
    app.connectors
        .add(
            "lin",
            nexus_core::connectors::ConnectorConfig::remote("https://mcp.linear.app/mcp"),
        )
        .unwrap();
    let paths = nexus_core::connectors::import::ImportPaths {
        claude_desktop: Some(desktop),
        ..Default::default()
    };
    app.popup = nexus_core::app::Popup::Connectors;
    super::connectors::open_import(&mut app, &paths);
    let screen = render_to_string(120, 24, |f| super::connectors::render(f, &app));
    assert_eq!(
        body_lines(&screen)[..2],
        [
            "▸ [x] filesystem · Claude Desktop · npx",
            "[=] linear · Claude Desktop · https://mcp.linear.app/mcp · already added",
        ]
        .map(String::from)
    );
    assert!(screen.contains("import · 1 selected"), "{screen}");
    assert!(
        screen.contains("1 secret will move to your keychain"),
        "{screen}"
    );
    assert!(!screen.contains("sk-secret-import"), "{screen}");
    conn_press(&mut app, KeyCode::Char(' '));
    assert!(!app.connectors_popup.import[0].checked);
    conn_press(&mut app, KeyCode::Char('a'));
    assert!(app.connectors_popup.import[0].checked);
    conn_press(&mut app, KeyCode::Esc);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
}

#[test]
fn connectors_import_applies_the_displayed_fixture_selection() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    app.interactive_ui = true;
    let root = app.space.root.clone();
    let fixture = root.join("mcp-import.json");
    std::fs::write(&fixture, r#"{"mcpServers":{"fixture-only":{"command":"fixture-server","env":{"API_KEY":"fake-import-secret"}}}}"#).unwrap();
    let paths = nexus_core::connectors::import::ImportPaths {
        cursor: Some(fixture),
        ..Default::default()
    };
    super::connectors::open_import(&mut app, &paths);
    conn_press(&mut app, KeyCode::Enter);
    let config = app.connectors.config_of("fixture-only").unwrap();
    assert_eq!(config.command, ["fixture-server"]);
    assert_eq!(config.env["API_KEY"], "keychain:env-API_KEY");
    assert_eq!(
        app.connectors
            .vault()
            .get("fixture-only", "env-API_KEY")
            .as_deref(),
        Some("fake-import-secret")
    );
    assert_eq!(app.connectors.infos().len(), 1);
    assert_eq!(app.connectors_popup.mode, super::connectors::Mode::List);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn connectors_render_at_80_columns_and_tiny_sizes_without_panicking() {
    use nexus_core::app::Popup;
    for mode in [
        super::connectors::Mode::List,
        super::connectors::Mode::Tools,
        super::connectors::Mode::Add,
        super::connectors::Mode::ConfirmRemove,
        super::connectors::Mode::Import,
    ] {
        for (w, h) in [(80, 24), (40, 10), (30, 8)] {
            let mut app = connectors_app();
            app.popup = Popup::Connectors;
            app.connectors_popup.selected = 2;
            app.connectors_popup.mode = mode;
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        }
    }
}

#[test]
fn connector_approvals_offer_session_and_always_and_warn_about_changed_definitions() {
    use crossterm::event::{KeyCode, KeyEvent};
    use nexus_core::tools::Decision;
    let summary = "writes · may delete\n\n{\n  \"title\": \"Fix it\"\n}".to_string();
    let (mut app, mut receiver) = approval_app("linear", summary);
    let root = app.space.root.clone();
    {
        let request = app.pending_approval_request.as_mut().unwrap();
        request.action = "create_issue".into();
        request.class = Some("connector".into());
    }
    app.sync_approval_popup();
    let screen = render_to_string(100, 24, |f| super::approval::render(f, &app));
    let lines = body_lines(&screen);
    assert!(
        lines[0].starts_with("linear · create_issue"),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with("connector"), "{}", lines[0]);
    assert_eq!(
        lines[1],
        "The model wants to use create_issue from your linear connector. Check what it will send:"
    );
    assert!(screen.contains("writes · may delete"), "{screen}");
    assert!(screen.contains("\"title\": \"Fix it\""), "{screen}");
    assert_eq!(
        lines.last().unwrap(),
        "y allow once · a this tool for the session · A always allow · n deny"
    );
    // The open delay applies to the new key as well.
    app.approval_delay = std::time::Duration::from_millis(400);
    app.approval_opened_at = Some(std::time::Instant::now());
    super::approval::handle_key(&mut app, KeyEvent::from(KeyCode::Char('A'))).unwrap();
    assert!(receiver.try_recv().is_err());
    app.approval_opened_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    super::approval::handle_key(&mut app, KeyEvent::from(KeyCode::Char('A'))).unwrap();
    assert_eq!(receiver.try_recv().unwrap(), Decision::AllowAlways);

    // A changed definition is called out in the warning color.
    let (mut changed, _receiver) = approval_app(
        "linear",
        "this tool's definition changed since you allowed it\nwrites\n\n{}".into(),
    );
    {
        let request = changed.pending_approval_request.as_mut().unwrap();
        request.action = "create_issue".into();
        request.class = Some("connector".into());
    }
    changed.sync_approval_popup();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|f| super::approval::render(f, &changed))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let cell = buffer
        .content
        .iter()
        .find(|cell| cell.symbol() == "d" && cell.fg == changed.theme.warning);
    assert!(
        cell.is_some(),
        "the changed-definition line uses the warning color"
    );
    drop(app);
    drop(changed);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn only_connector_approvals_offer_always() {
    use crossterm::event::{KeyCode, KeyEvent};
    let (mut app, mut receiver) = approval_app("shell", "$ true".into());
    let root = app.space.root.clone();
    app.approval_opened_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    super::approval::handle_key(&mut app, KeyEvent::from(KeyCode::Char('A'))).unwrap();
    assert!(
        receiver.try_recv().is_err(),
        "A does nothing for a shell request"
    );
    let screen = render_to_string(100, 24, |f| super::approval::render(f, &app));
    assert!(!screen.contains("always allow"), "{screen}");
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

fn viewer_png(name: &str, width: u32, height: u32) -> String {
    let dir = std::env::temp_dir().join(format!("nexus-viewer-snap-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x * 3 % 256) as u8, (y * 6 % 256) as u8, 150])
    })
    .save(&path)
    .unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn image_viewer_chrome_shows_title_footer_and_hints_for_a_generated_png() {
    use crate::image_viewer::{Anchor, Return};
    let pic = viewer_png("sunset.png", 64, 32);
    let mut app = test_app();
    app.core.settings.image_viewer = "terminal".into();
    app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
    assert!(app.popup == nexus_core::app::Popup::ImageViewer);
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    let lines: Vec<_> = screen.lines().collect();
    assert!(
        lines[0].starts_with("\u{256d}") && lines[0].contains(" sunset.png "),
        "{screen}"
    );
    // The footer row sits just above the bottom border, which carries the hints.
    let footer = lines[lines.len() - 2];
    let size = crate::image_viewer::human_bytes(std::fs::metadata(&pic).unwrap().len());
    assert!(
        footer.contains(&format!("64x32 \u{b7} PNG \u{b7} {size} \u{b7} fit")),
        "{footer}"
    );
    assert!(
        !footer.contains('/'),
        "a lone image shows no counter: {footer}"
    );
    let hints = lines[lines.len() - 1];
    for hint in [
        "\u{2190}\u{2192} prev/next",
        "f 1:1",
        "o system viewer",
        "c copy",
        "Esc close",
    ] {
        assert!(hints.contains(hint), "{hint}: {hints}");
    }
    assert!(
        screen.contains('\u{2580}'),
        "the picture is drawn:\n{screen}"
    );
}

#[test]
fn image_viewer_footer_counts_images_and_names_the_one_to_one_mode() {
    use crate::image_viewer::{Anchor, Return};
    let a = viewer_png("a.png", 20, 10);
    let b = viewer_png("b.png", 20, 10);
    let mut app = test_app();
    app.core.settings.image_viewer = "terminal".into();
    app.history_cache.image_at_line = vec![Some(a.clone()), None, Some(b)];
    app.open_image_target(&a, &Anchor::Line(0), Return::Nothing);
    super::image_viewer::handle_key(&mut app, crossterm::event::KeyCode::Char('f').into());
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    let footer = screen.lines().nth(22).unwrap();
    assert!(
        footer.contains("\u{b7} 1:1") && footer.contains("1/2"),
        "{footer}"
    );
    assert!(
        screen.lines().nth(23).unwrap().contains("f fit"),
        "{screen}"
    );
    super::image_viewer::handle_key(&mut app, crossterm::event::KeyCode::Right.into());
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    assert!(
        screen.lines().next().unwrap().contains(" b.png "),
        "{screen}"
    );
    assert!(screen.lines().nth(22).unwrap().contains("2/2"), "{screen}");
}

#[test]
fn settings_image_viewer_row_cycles_persists_and_round_trips() {
    let mut app = test_app();
    select_settings_field(&mut app, SettingsField::ImageViewer);
    let screen = render_to_string(110, 40, |f| super::settings::render(f, &app));
    assert!(screen.contains("image viewer"), "{screen}");
    assert_eq!(app.settings.image_viewer, "auto");
    let mut seen = vec![app.settings.image_viewer.clone()];
    for _ in 0..3 {
        press(&mut app, crossterm::event::KeyCode::Char(' '));
        seen.push(app.settings.image_viewer.clone());
    }
    assert_eq!(seen, ["auto", "terminal", "system", "auto"]);
    press(&mut app, crossterm::event::KeyCode::Char(' '));
    let stored = app.db.load_settings().unwrap();
    assert!(
        stored
            .iter()
            .any(|(k, v)| k == "image_viewer" && v == "terminal")
    );
    assert_eq!(
        crate::image_viewer::Preference::parse(&app.settings.image_viewer),
        crate::image_viewer::Preference::Terminal
    );
}

fn open_viewer_on(app: &mut AppView, names: &[&str]) -> Vec<String> {
    use crate::image_viewer::{Anchor, Return};
    let pics: Vec<String> = names.iter().map(|n| viewer_png(n, 40, 20)).collect();
    app.core.settings.image_viewer = "terminal".into();
    app.history_cache.image_at_line = pics.iter().flat_map(|p| [Some(p.clone()), None]).collect();
    app.open_image_target(&pics[0], &Anchor::Line(0), Return::Nothing);
    assert!(app.popup == nexus_core::app::Popup::ImageViewer);
    pics
}

#[test]
fn image_viewer_action_menu_lists_every_action_with_its_key() {
    use crossterm::event::KeyCode;
    let mut app = test_app();
    open_viewer_on(&mut app, &["menu.png"]);
    super::image_viewer::handle_key(&mut app, KeyCode::Char('.').into());
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    assert!(screen.contains(" actions "), "{screen}");
    for (label, key) in [
        ("Copy image", "y"),
        ("Copy path", "c"),
        ("Save to files", "s"),
        ("Save to Downloads", "S"),
        ("Reveal", "r"),
        ("Open in default app", "o"),
        ("Attach to message", "a"),
    ] {
        let row = screen
            .lines()
            .find(|line| line.contains(label))
            .unwrap_or_else(|| panic!("{label} is listed:\n{screen}"));
        let cell = row
            .split('\u{2502}')
            .find(|part| part.contains(label))
            .unwrap();
        assert!(
            cell.trim_end().ends_with(key),
            "{label} shows its key {key}: {row}"
        );
    }
    let first = screen.lines().find(|l| l.contains("Copy image")).unwrap();
    assert!(
        first.contains("\u{25b8} Copy image"),
        "selection marker: {first}"
    );
    assert!(screen.contains("Enter run"), "{screen}");
    // The menu sits in the shared rounded chrome.
    assert!(
        screen.contains('\u{256d}') && screen.contains('\u{256f}'),
        "{screen}"
    );

    super::image_viewer::handle_key(&mut app, KeyCode::Down.into());
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    assert!(screen.contains("\u{25b8} Copy path"), "{screen}");
    assert!(!screen.contains("\u{25b8} Copy image"), "{screen}");
}

#[test]
fn image_viewer_hints_and_footer_adapt_to_the_width() {
    let mut app = test_app();
    open_viewer_on(&mut app, &["sunset.png"]);
    let wide = render_to_string(140, 24, |f| super::image_viewer::render(f, &mut app));
    let lines: Vec<_> = wide.lines().collect();
    let hints = lines[lines.len() - 1];
    for hint in [
        ". menu",
        "y copy image",
        "c copy path",
        "s save",
        "+/- zoom",
        "r reveal",
    ] {
        assert!(hints.contains(hint), "{hint}: {hints}");
    }
    let footer = lines[lines.len() - 2];
    assert!(
        footer.contains("40x20 \u{b7} PNG") && footer.contains("fit 100%"),
        "{footer}"
    );
    assert!(
        footer.trim_end().ends_with("sunset.png \u{2502}") || footer.contains("sunset.png"),
        "{footer}"
    );

    let narrow = render_to_string(44, 12, |f| super::image_viewer::render(f, &mut app));
    let lines: Vec<_> = narrow.lines().collect();
    let hints = lines[lines.len() - 1];
    assert!(
        hints.contains("Esc close"),
        "the essentials survive: {hints}"
    );
    assert!(!hints.contains("y copy image"), "{hints}");
    let footer = lines[lines.len() - 2];
    assert!(footer.contains("40x20"), "size is the last to go: {footer}");
    assert!(
        !footer.contains("sunset.png"),
        "source goes first: {footer}"
    );
}

#[test]
fn image_viewer_shows_a_chevron_only_where_the_pointer_would_turn_the_page() {
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    let mut app = test_app();
    open_viewer_on(&mut app, &["one.png", "two.png"]);
    let hover = |app: &mut AppView, column, row| {
        super::image_viewer::mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    };
    render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    let body = app.image_viewer.body();
    // First image: nothing to go back to on the left, a chevron on the right.
    hover(&mut app, body.x + 2, body.y + 3);
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    assert!(
        !screen.contains('\u{2039}') && !screen.contains('\u{203a}'),
        "{screen}"
    );
    hover(&mut app, body.right() - 2, body.y + 3);
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    assert!(
        screen.contains('\u{203a}') && !screen.contains('\u{2039}'),
        "{screen}"
    );
    hover(&mut app, body.x + body.width / 2, body.y + 3);
    let screen = render_to_string(100, 24, |f| super::image_viewer::render(f, &mut app));
    assert!(!screen.contains('\u{203a}'), "{screen}");
}
