use super::persist::url_prompt;
use super::*;
use nexus_core::app::{App, AppCommand, AppsMode};
use nexus_core::db::Db;
use nexus_core::space::Space;
use ratatui::{Terminal, backend::TestBackend};
use std::sync::{Arc, Mutex};

const COUNTER: &str = "root = Panel(\"Weekly Counter\", [Text(\"Count: \" + $count), Input($name, \"Name\"), Buttons([Button(\"Add\", @Set($count, $count + 1))])])\n$count = 0\n$name = \"\"";

fn test_app() -> AppView {
    let db = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join(format!("nexus-persist-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let mut app = AppView::new(App::new(db, Some("k"), Space { root }));
    let session = app
        .core
        .db
        .create_session("chat", "model", &app.core.active_space.id, "chat")
        .unwrap();
    app.core.session = Some(session);
    app
}
fn placement(source: &str, owner: usize) -> Placement {
    Placement {
        row: 0,
        width: 70,
        owner,
        ordinal: 0,
        height: source_height(source, 70),
        source: source.into(),
        complete: true,
    }
}
fn draw(app: &mut AppView, placements: &[Placement]) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            crate::interactive::sync_saved(app);
            app.interactive.begin_frame(true);
            app.interactive
                .paint(f, f.area(), 0, placements, &Theme::default(), true);
        })
        .unwrap();
    screen(&terminal)
}
fn screen(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                + "\n"
        })
        .collect()
}
fn press(app: &mut AppView, code: KeyCode) {
    app.popup = Popup::InteractiveUi;
    key(app, KeyEvent::new(code, KeyModifiers::NONE));
}
fn status(app: &mut AppView) -> String {
    while let Some(event) = app.pop_pending_event() {
        app.apply_event(&event);
    }
    app.status.clone()
}
fn type_text(app: &mut AppView, text: &str) {
    for ch in text.chars() {
        press(app, KeyCode::Char(ch));
    }
}
fn value(app: &AppView, p: &Placement, name: &str) -> String {
    app.interactive.entries[&p.key()]
        .session
        .as_ref()
        .unwrap()
        .value(name)
}
/// Paint, focus the newest panel and type into its first input.
fn edit_name(app: &mut AppView, placements: &[Placement], text: &str) {
    draw(app, placements);
    assert!(app.interactive.focus_latest());
    type_text(app, text);
    flush(app);
}
/// A new process: no widgets, same database.
fn restart(app: &mut AppView) {
    app.interactive = Widgets::default();
    app.popup = Popup::None;
}

#[test]
fn panel_state_is_saved_per_panel_and_restored_after_a_restart() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    edit_name(&mut app, std::slice::from_ref(&p), "Ada");
    assert_eq!(value(&app, &p, "name"), "Ada");

    restart(&mut app);
    draw(&mut app, std::slice::from_ref(&p));
    assert_eq!(value(&app, &p, "name"), "Ada");
    assert_eq!(value(&app, &p, "count"), "0");
    // Restoring changes nothing, so nothing is rewritten.
    assert!(app.interactive.take_dirty().is_empty());
}

#[test]
fn identical_panels_in_one_conversation_keep_separate_state() {
    let mut app = test_app();
    let (a, b) = (placement(COUNTER, 0), placement(COUNTER, 1));
    let both = [a.clone(), b.clone()];
    draw(&mut app, &both);
    // `focus_latest` picks the last visible panel: the second one.
    assert!(app.interactive.focus_latest());
    type_text(&mut app, "second");
    flush(&mut app);
    assert_eq!(app.core.panel_states("x").unwrap().len(), 0);
    let id = app.core.session.as_ref().unwrap().id.clone();
    assert_eq!(app.core.panel_states(&id).unwrap().len(), 1);

    restart(&mut app);
    draw(&mut app, &both);
    assert_eq!(value(&app, &a, "name"), "");
    assert_eq!(value(&app, &b, "name"), "second");
}

#[test]
fn state_belongs_to_its_conversation_and_incognito_saves_nothing() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    edit_name(&mut app, std::slice::from_ref(&p), "kept");
    let first = app.core.session.clone().unwrap();

    // A different conversation does not see it.
    let other = app
        .core
        .db
        .create_session("other", "model", &app.core.active_space.id, "chat")
        .unwrap();
    app.core.session = Some(other);
    restart(&mut app);
    draw(&mut app, std::slice::from_ref(&p));
    assert_eq!(value(&app, &p, "name"), "");
    app.core.session = Some(first);
    restart(&mut app);
    draw(&mut app, std::slice::from_ref(&p));
    assert_eq!(value(&app, &p, "name"), "kept");

    let mut app = test_app();
    app.core.incognito = true;
    edit_name(&mut app, std::slice::from_ref(&p), "secret");
    let id = app.core.session.as_ref().unwrap().id.clone();
    assert!(app.core.panel_states(&id).unwrap().is_empty());
}

#[test]
fn a_changed_or_corrupt_record_never_blocks_the_panel() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    let id = app.core.session.as_ref().unwrap().id.clone();
    let key = nexus_core::interactive::panel_key(COUNTER, 0);
    for bad in [
        "not json",
        "{\"v\":9}",
        "{\"v\":1,\"state\":{\"count\":\"text\"}}",
    ] {
        app.core.save_panel_state(&id, &key, bad).unwrap();
        restart(&mut app);
        draw(&mut app, std::slice::from_ref(&p));
        assert_eq!(value(&app, &p, "count"), "0", "{bad}");
    }
}

#[test]
fn p_pins_the_focused_panel_but_types_into_inputs() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    draw(&mut app, std::slice::from_ref(&p));
    assert!(app.interactive.focus_latest());
    // The input has focus first: `p` is just a letter.
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(value(&app, &p, "name"), "p");
    assert!(app.core.pinned_panels().unwrap().is_empty());
    // On the button it pins, state and all.
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('p'));
    let pinned = app.core.pinned_panels().unwrap();
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].name, "weekly-counter");
    assert_eq!(pinned[0].title, "Weekly Counter");
    let record = nexus_core::interactive::PanelSnapshot::decode(&pinned[0].record).unwrap();
    assert_eq!(record.state["count"].as_f64(), Some(1.0));
    assert_eq!(record.state["name"], "p");
    // Pinning again picks a free name.
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.core.pinned_panels().unwrap().len(), 2);
}

#[test]
fn the_command_pins_a_named_app_renames_and_unpins() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    draw(&mut app, std::slice::from_ref(&p));
    app.run_command("pin-panel counter").unwrap();
    assert_eq!(app.core.pinned_panels().unwrap()[0].name, "counter");
    app.run_command("pin-panel counter").unwrap();
    assert_eq!(app.core.pinned_panels().unwrap().len(), 1, "name is taken");
    app.run_command("rename-panel counter tally").unwrap();
    assert_eq!(app.core.pinned_panels().unwrap()[0].name, "tally");
    app.run_command("pin-panel unpin tally").unwrap();
    assert!(app.core.pinned_panels().unwrap().is_empty());
    // With nothing on screen there is nothing to pin.
    let mut empty = test_app();
    empty.run_command("pin-panel").unwrap();
    assert!(empty.core.pinned_panels().unwrap().is_empty());
}

fn pin(app: &mut AppView, p: &Placement) {
    draw(app, std::slice::from_ref(p));
    assert!(app.interactive.focus_latest());
    app.run_command("pin-panel counter").unwrap();
    app.interactive.unfocus();
    app.popup = Popup::None;
}

#[test]
fn a_pinned_app_opens_full_size_with_its_state_and_keeps_changes() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    draw(&mut app, std::slice::from_ref(&p));
    assert!(app.interactive.focus_latest());
    type_text(&mut app, "Ada");
    app.run_command("pin-panel counter").unwrap();
    app.interactive.unfocus();

    // Open it from /apps, as a person would.
    app.open_apps_popup();
    assert_eq!(app.pinned_apps.len(), 1);
    let rendered = apps_screen(&app);
    assert!(rendered.contains("counter"), "{rendered}");
    assert!(rendered.contains("panel · Weekly Counter"), "{rendered}");
    assert!(rendered.contains("Ctrl+R rename"), "{rendered}");
    assert_eq!(app.apps_selected, 0);
    crate::ui::popups::apps::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    )
    .unwrap();
    assert_eq!(app.popup, Popup::InteractiveUi);
    assert!(app.interactive.focused_app());
    assert!(app.interactive.expanded);

    // Full-size view, with the saved state, even before any transcript layout.
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| render_popup(f, &mut app)).unwrap();
    let view = screen(&terminal);
    assert!(view.contains("Weekly Counter"), "{view}");
    assert!(view.contains("Count: 0"), "{view}");
    let focused = app.interactive.focused.clone().unwrap();
    let name = |app: &AppView| {
        app.interactive.entries[&focused]
            .session
            .as_ref()
            .unwrap()
            .value("name")
    };
    assert_eq!(name(&app), "Ada");

    // Press the button, close, reopen: the new state is there.
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.popup, Popup::None);
    flush(&mut app);
    assert!(open_pinned(&mut app, "counter"));
    let focused = app.interactive.focused.clone().unwrap();
    let session = app.interactive.entries[&focused].session.as_ref().unwrap();
    assert_eq!(session.value("count"), "1");
    assert_eq!(session.value("name"), "Ada");
    // The opened app is not the transcript panel it came from.
    assert_eq!(value(&app, &p, "count"), "0");

    // It also opens by name from the command line.
    app.interactive.unfocus();
    app.run_command("tuiapp counter").unwrap();
    assert_eq!(app.popup, Popup::InteractiveUi);
    assert!(!open_pinned(&mut app, "missing"));
}

fn apps_screen(app: &AppView) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|f| crate::ui::popups::apps::render(f, app))
        .unwrap();
    screen(&terminal)
}

#[test]
fn the_apps_popup_renames_and_unpins_panels() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    pin(&mut app, &p);
    app.open_apps_popup();
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let handle = crate::ui::popups::apps::handle_key;

    handle(&mut app, ctrl('r')).unwrap();
    assert!(app.apps_mode == AppsMode::RenamePanel);
    assert!(apps_screen(&app).contains("rename counter to"));
    for _ in 0.."counter".len() {
        handle(&mut app, plain(KeyCode::Backspace)).unwrap();
    }
    for ch in "tally".chars() {
        handle(&mut app, plain(KeyCode::Char(ch))).unwrap();
    }
    handle(&mut app, plain(KeyCode::Enter)).unwrap();
    assert!(app.apps_mode == AppsMode::Browse);
    assert_eq!(app.core.pinned_panels().unwrap()[0].name, "tally");
    assert!(apps_screen(&app).contains("tally"));

    // A bad name keeps the prompt open with a reason.
    handle(&mut app, ctrl('r')).unwrap();
    app.apps_edit = "Not Valid".into();
    handle(&mut app, plain(KeyCode::Enter)).unwrap();
    assert!(app.apps_mode == AppsMode::RenamePanel);
    handle(&mut app, plain(KeyCode::Esc)).unwrap();
    assert!(app.apps_mode == AppsMode::Browse);

    handle(&mut app, ctrl('d')).unwrap();
    assert!(app.apps_mode == AppsMode::ConfirmDelete);
    assert!(apps_screen(&app).contains("unpin panel app \"tally\""));
    handle(&mut app, ctrl('d')).unwrap();
    assert!(app.core.pinned_panels().unwrap().is_empty());
    assert!(app.pinned_apps.is_empty());
    assert!(app.apps_mode == AppsMode::Browse);
}

#[test]
fn an_edit_patch_to_a_pinned_app_shows_on_the_next_open() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    pin(&mut app, &p);
    let patch = "root = Panel(\"Weekly Counter\", [Text(\"Count: \" + $count), Text(\"Limit: \" + $limit), Input($name, \"Name\"), Buttons([Button(\"Add\", @Set($count, $count + 1))])])\n$limit = 9";
    let pinned = app.core.pinned_panels().unwrap().remove(0);
    app.core.patch_panel(&pinned.id, patch).unwrap();
    assert!(open_pinned(&mut app, "counter"));
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| render_popup(f, &mut app)).unwrap();
    assert!(screen(&terminal).contains("Limit: 9"));
}

fn visit_source() -> String {
    "root = Panel(\"Links\", [Buttons([Button(\"Docs\", @OpenUrl(\"https://docs.example.com/a/b\")), Button(\"Other\", @OpenUrl(\"https://other.example.org\"))])])".into()
}
fn opened_urls(widgets: &mut Widgets) -> Arc<Mutex<Vec<String>>> {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&opened);
    widgets.set_opener(Box::new(move |url| {
        sink.lock().unwrap().push(url.to_string());
        Ok(())
    }));
    opened
}
fn prompt(app: &AppView) -> String {
    let key = app.interactive.focused.clone().unwrap();
    let (buffer, _) = app.interactive.entries[&key].canvas(90, &Theme::default(), true, true);
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn open_url_names_the_host_and_waits_for_confirmation() {
    let mut app = test_app();
    let source = visit_source();
    let p = placement(&source, 0);
    draw(&mut app, std::slice::from_ref(&p));
    let opened = opened_urls(&mut app.interactive);
    assert!(app.interactive.focus_latest());
    press(&mut app, KeyCode::Enter);
    let shown = prompt(&app);
    assert!(
        shown.contains("Open https://docs.example.com/a/b? Enter confirm"),
        "{shown}"
    );
    assert!(shown.contains("a always docs.example.com"), "{shown}");
    assert!(opened.lock().unwrap().is_empty());
    // Other keys are inert while a URL waits.
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Tab);
    assert!(opened.lock().unwrap().is_empty());
    press(&mut app, KeyCode::Esc);
    assert!(opened.lock().unwrap().is_empty());
    assert!(status(&mut app).contains("cancelled"));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(*opened.lock().unwrap(), ["https://docs.example.com/a/b"]);
}

#[test]
fn always_allow_is_per_host_for_this_run_only() {
    let mut app = test_app();
    let source = visit_source();
    let p = placement(&source, 0);
    draw(&mut app, std::slice::from_ref(&p));
    let opened = opened_urls(&mut app.interactive);
    assert!(app.interactive.focus_latest());
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(opened.lock().unwrap().len(), 1);
    let said = status(&mut app);
    assert!(said.contains("docs.example.com allowed"), "{said}");

    // The same host now opens at once.
    press(&mut app, KeyCode::Enter);
    assert_eq!(opened.lock().unwrap().len(), 2);
    assert!(!app.interactive.pending_url_focused());
    // Another host still asks.
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter);
    assert_eq!(opened.lock().unwrap().len(), 2);
    assert!(app.interactive.pending_url_focused());
    assert!(prompt(&app).contains("a always other.example.org"));
    press(&mut app, KeyCode::Esc);

    // The allowance survives a viewport reset but not a new run.
    app.interactive.clear();
    draw(&mut app, std::slice::from_ref(&p));
    assert!(app.interactive.focus_latest());
    press(&mut app, KeyCode::Enter);
    assert_eq!(opened.lock().unwrap().len(), 3);
}

#[test]
fn a_long_url_is_shortened_but_never_its_host() {
    let url = format!("https://docs.example.com/{}", "segment/".repeat(30));
    for width in [40, 60, 80, 120, 400] {
        let line = url_prompt(&url, width);
        assert!(
            line.starts_with("Open https://docs.example.com"),
            "{width}: {line}"
        );
        assert!(line.chars().count() <= width.max(60), "{width}: {line}");
        assert!(line.contains("Enter"), "{width}: {line}");
        // The hints shrink before the host does; the whole line fits the card.
        if width >= 100 {
            assert!(
                line.contains("a always docs.example.com"),
                "{width}: {line}"
            );
        }
        if width < 400 {
            assert!(line.contains('…'), "{width}: {line}");
        }
    }
    assert_eq!(
        url_prompt("https://example.com", 120),
        "Open https://example.com? Enter confirm · a always example.com · Esc cancel"
    );
    // A lookalike spelling is shown as the host the browser would really use.
    let line = url_prompt("https://bücher.example/x", 120);
    assert!(line.contains("xn--bcher-kva.example"), "{line}");
}

#[test]
fn only_https_urls_can_be_authored() {
    for url in [
        "http://example.com",
        "https://user:pw@example.com",
        "ftp://x.test",
    ] {
        let source =
            format!("root = Panel(\"Bad\", [Buttons([Button(\"Go\", @OpenUrl(\"{url}\"))])])");
        assert!(Document::parse(&source).is_err(), "{url}");
    }
}

#[test]
fn commands_are_domain_commands_the_host_can_send() {
    let mut app = test_app();
    let p = placement(COUNTER, 0);
    pin(&mut app, &p);
    app.core
        .execute(AppCommand::RenamePanel {
            from: "counter".into(),
            to: "tally".into(),
        })
        .unwrap();
    app.core
        .execute(AppCommand::UnpinPanel {
            name: "tally".into(),
        })
        .unwrap();
    assert!(app.core.pinned_panels().unwrap().is_empty());
}
