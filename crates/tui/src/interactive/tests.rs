use super::*;
use ratatui::{Terminal, backend::TestBackend};

pub(crate) const COUNTER: &str =
    include_str!("../../../core/tests/fixtures/interactive/counter.json");
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
fn draw(widgets: &mut Widgets, placements: &[Placement], top: usize) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            widgets.begin_frame(false);
            widgets.paint(f, f.area(), top, placements, &Theme::default(), true);
        })
        .unwrap();
    terminal
}
fn text(terminal: &Terminal<TestBackend>) -> String {
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
#[test]
fn inline_controls_update_only_their_own_document() {
    let mut widgets = Widgets::default();
    let p = placement(COUNTER, 1);
    assert!(text(&draw(&mut widgets, &[p.clone()], 0)).contains("Count: 0"));
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .clone();
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x,
        row: hit.rect.y,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        widgets.click(mouse, Rect::new(0, 0, 80, 32), false),
        (true, None)
    );
    assert!(text(&draw(&mut widgets, &[p.clone()], 0)).contains("Count: 1"));
    let other = placement(COUNTER, 2);
    assert!(text(&draw(&mut widgets, &[other], 0)).contains("Count: 0"));
    assert!(text(&draw(&mut widgets, &[p], 0)).contains("Count: 1"));
}
#[test]
fn focused_inputs_and_keys_are_local_and_submission_is_explicit() {
    let mut widgets = Widgets::default();
    let p = placement(COUNTER, 0);
    draw(&mut widgets, &[p.clone()], 0);
    assert!(widgets.focus_latest());
    widgets.key(KeyCode::Enter.into(), false);
    widgets.key(KeyCode::Tab.into(), false);
    widgets.key(KeyCode::Tab.into(), false);
    widgets.key(KeyCode::Char('A').into(), false);
    widgets.paste("da");
    assert_eq!(
        widgets.entries[&p.key()]
            .session
            .as_ref()
            .unwrap()
            .value("name"),
        "Ada"
    );
    widgets.key(KeyCode::Tab.into(), false);
    assert_eq!(widgets.key(KeyCode::Enter.into(), false).1, None);
    assert_eq!(
        widgets.key(KeyCode::Enter.into(), true).1,
        Some("UI response — Counter\nAda chose 1".into())
    );
    assert!(widgets.key(KeyCode::Esc.into(), true).0);
    assert!(widgets.focused.is_none());
}
#[test]
fn incomplete_invalid_clipped_and_revised_documents_are_safe() {
    let mut widgets = Widgets::default();
    let mut p = placement(COUNTER, 0);
    p.complete = false;
    draw(&mut widgets, &[p.clone()], 0);
    assert!(widgets.entries.is_empty());
    p.complete = true;
    let before = draw(&mut widgets, &[p.clone()], 6);
    assert!(text(&before).contains("Submit result ↵"));
    assert!(!text(&before).contains("Count: 0"));
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control.is_some())
        .unwrap()
        .clone();
    let m = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x,
        row: hit.rect.y,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        widgets.click(m, Rect::new(0, 0, 20, 10), true),
        (false, None)
    );
    p.source = "{bad".into();
    p.height = 6;
    assert!(text(&draw(&mut widgets, &[p], 0)).contains("unavailable"));
    widgets.clear();
    assert!(widgets.entries.is_empty());
    assert!(widgets.hits.is_empty());
}

#[test]
fn measured_reservations_match_the_drawn_card_at_actual_widths() {
    for source in [
        COUNTER.to_string(),
        include_str!("../../../core/tests/fixtures/interactive/rps.json").into(),
        COUNTER.replace(
            "Count: {{count}}",
            &"supporting content wraps inside the card. ".repeat(12),
        ),
    ] {
        for width in [60, 80, 120] {
            let rendered = crate::ui::markdown::render(
                &format!("```nexus-ui\n{source}\n```"),
                width,
                crate::ui::markdown::MdColors::default(),
            );
            let p = &rendered.widgets[0];
            let entry = Entry {
                session: Ok(Session::new(Document::parse(&source).unwrap()).unwrap()),
                focus: 0,
                error: String::new(),
                scroll: 0,
            };
            let (canvas, hits) = entry.canvas(
                u16::try_from(width).unwrap(),
                &Theme::default(),
                false,
                true,
            );
            assert_eq!(p.height, canvas.area.height);
            assert_eq!(rendered.lines.len(), usize::from(p.height));
            assert!(
                hits.iter()
                    .all(|(rect, _)| rect.intersection(canvas.area) == *rect)
            );
            let rows: Vec<_> = (0..canvas.area.height)
                .map(|y| {
                    (0..canvas.area.width)
                        .map(|x| canvas[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect();
            assert!(rows.last().unwrap().starts_with('╰'));
            assert!(rows.last().unwrap().ends_with('╯'));
            if source.contains("supporting content") {
                assert!(rows.iter().filter(|row| row.contains("supporting")).count() > 1);
            }
        }
    }
}

#[test]
fn pill_tile_input_and_expand_clicks_share_the_keyboard_state() {
    for source in [
        COUNTER,
        include_str!("../../../core/tests/fixtures/interactive/rps.json"),
    ] {
        let mut widgets = Widgets::default();
        let p = placement(source, 0);
        let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
        terminal
            .draw(|f| {
                widgets.begin_frame(true);
                widgets.paint(
                    f,
                    f.area(),
                    0,
                    std::slice::from_ref(&p),
                    &Theme::default(),
                    true,
                );
            })
            .unwrap();
        let controls = widgets.entries[&p.key()].controls();
        for control in 0..controls.len() + 1 {
            let hit = widgets
                .hits
                .iter()
                .find(|hit| hit.control == Some(control))
                .cloned()
                .unwrap();
            assert!(
                widgets
                    .click(
                        MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            column: hit.rect.x,
                            row: hit.rect.y,
                            modifiers: KeyModifiers::NONE
                        },
                        Rect::new(0, 0, 80, 32),
                        true
                    )
                    .0
            );
            assert_eq!(widgets.focused, Some(p.key()));
            assert_eq!(widgets.entries[&p.key()].focus, control);
            if source != COUNTER && control < 3 {
                assert_eq!(
                    widgets.entries[&p.key()]
                        .session
                        .as_ref()
                        .unwrap()
                        .value("you"),
                    ["Rock", "Paper", "Scissors"][control]
                );
            }
            if source == COUNTER && control == 2 {
                widgets.paste("Ada");
                assert_eq!(
                    widgets.entries[&p.key()]
                        .session
                        .as_ref()
                        .unwrap()
                        .value("name"),
                    "Ada"
                );
            }
        }
        assert!(widgets.expanded);
        widgets.unfocus();
        widgets.focus_latest();
        // Re-entering keeps the expand control selected.
        widgets.key(KeyCode::Enter.into(), false);
        assert!(widgets.expanded);
    }
}

#[test]
fn pinned_card_keeps_state_hits_and_composer_clear_until_escape_or_session_reset() {
    let mut app = app();
    app.set_input("composer stays visible");
    app.messages[0]
        .content
        .push_str(&format!("\n\n{}", "Following conversation\n\n".repeat(40)));
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    app.scroll = app.max_scroll;
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    focus(&mut app);
    key(&mut app, KeyCode::Enter.into());
    for _ in 0..app.max_scroll.div_ceil(3) {
        assert!(mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 5,
                modifiers: KeyModifiers::NONE
            },
            Rect::new(0, 0, 80, 32)
        ));
    }
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    let viewport = app.interactive.layout.as_ref().unwrap().0;
    assert_eq!(
        terminal.backend().buffer()[(viewport.x + 2, viewport.y)].symbol(),
        "╭"
    );
    let pinned = text(&terminal);
    assert!(pinned.contains("pinned · Esc to unpin"), "{pinned}");
    assert!(pinned.contains("Count: 1"));
    assert!(pinned.contains("composer stays visible"));
    assert!(
        app.interactive
            .hits
            .iter()
            .all(|hit| hit.rect.bottom() <= app.input_inner.y - 1)
    );
    let hit = app
        .interactive
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .clone();
    mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.rect.x,
            row: hit.rect.y,
            modifiers: KeyModifiers::NONE,
        },
        Rect::new(0, 0, 80, 32),
    );
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(text(&terminal).contains("Count: 2"));
    key(&mut app, KeyCode::Esc.into());
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(!text(&terminal).contains("pinned · Esc"));
    app.scroll = app.max_scroll;
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    focus(&mut app);
    app.apply_event(&nexus_core::app::AppEvent::ViewportReset);
    assert!(app.interactive.focused.is_none());
    assert!(!app.interactive.pinned);
}

#[test]
fn enlarged_card_never_covers_the_composer_and_edits_reflow_the_transcript() {
    let mut app = app();
    app.messages[0].content = format!(
        "```nexus-ui\n{}\n```\n\nAfter the card",
        COUNTER.replace("Count: {{count}}", "Name: {{name}}")
    );
    app.set_input("keep this draft");
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    let before = app.history_cache.widgets[0].height;
    focus(&mut app);
    key(&mut app, KeyCode::Tab.into());
    key(&mut app, KeyCode::Tab.into());
    app.interactive.paste(&"long name ".repeat(30));
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(app.history_cache.widgets[0].height > before);
    assert!(text(&terminal).contains("After the card"));
    key(&mut app, KeyCode::Tab.into());
    key(&mut app, KeyCode::Tab.into());
    key(&mut app, KeyCode::Enter.into());
    assert!(app.interactive.expanded);
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(text(&terminal).contains("keep this draft"));
    assert!(
        app.interactive
            .hits
            .iter()
            .all(|hit| hit.rect.bottom() <= app.input_inner.y - 1)
    );
}

#[test]
fn instance_budget_reports_a_fallback_instead_of_growing_without_bound() {
    let mut widgets = Widgets::default();
    for owner in 0..64 {
        draw(&mut widgets, &[placement(COUNTER, owner)], 0);
    }
    let last = draw(&mut widgets, &[placement(COUNTER, 64)], 0);
    assert_eq!(widgets.entries.len(), 64);
    assert!(text(&last).contains("instance limit reached"));
    assert!(widgets.hits.is_empty());
}

#[test]
fn arbitrary_game_artwork_and_controls_use_the_same_renderer() {
    let mut widgets = Widgets::default();
    let source = include_str!("../../../core/tests/fixtures/interactive/rps.json");
    let p = placement(source, 0);
    let screen = text(&draw(&mut widgets, &[p], 0));
    for label in [
        "Rock · Paper · Scissors",
        "Wins 0",
        "Losses 0",
        "Draws 0",
        "beats scissors",
        "beats rock",
        "beats paper",
        "Reset score",
    ] {
        assert!(screen.contains(label), "{screen}");
    }
    assert_eq!(
        widgets
            .hits
            .iter()
            .filter(|hit| hit.control.is_some())
            .count(),
        5 // Four actions and the expand affordance.
    );
}

#[test]
fn dense_button_groups_reflow_without_losing_controls() {
    let labels = ["One", "Two", "Three", "Four", "Five", "Six"];
    let buttons: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| chrome::Button {
            action: index,
            label,
            body: &[],
            selected: false,
            enabled: true,
            primary: false,
        })
        .collect();
    let area = Rect::new(0, 0, 40, 3);
    let mut buffer = Buffer::empty(area);
    let hits = chrome::render_buttons(&mut buffer, area, &buttons, &Theme::default());
    assert_eq!(hits.len(), 6);
    for (rect, _) in hits {
        assert!(!rect.is_empty());
        assert_eq!(rect.intersection(area), rect);
    }
}

fn app() -> AppView {
    let mut app = AppView::new(nexus_core::app::App::new(
        nexus_core::db::Db::open_in_memory().unwrap(),
        None,
        nexus_core::space::Space {
            root: std::env::temp_dir().join(format!("nexus-ui-test-{}", uuid::Uuid::new_v4())),
        },
    ));
    app.messages.push(nexus_core::db::Message {
        role: "assistant".into(),
        content: format!("```nexus-ui\n{}\n```", COUNTER.trim()),
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

#[test]
fn model_authored_submissions_require_review_and_never_overwrite_a_draft() {
    let mut app = app();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(
        text(&terminal).contains("Count: 0"),
        "compact widgets must fit a normal terminal"
    );
    focus(&mut app);
    key(&mut app, KeyCode::Tab.into());
    key(&mut app, KeyCode::Tab.into());
    app.paste("/quit");
    key(&mut app, KeyCode::Tab.into());
    key(&mut app, KeyCode::Enter.into());
    assert_eq!(app.popup, Popup::None);
    assert_eq!(app.input_text(), "UI response — Counter\n/quit chose 0");
    assert!(!app.should_quit);
    assert!(!app.is_streaming());
    assert_eq!(app.messages.len(), 1);
    let draft = app.input_text();
    review(&mut app, Some("another model-authored message".into()));
    assert_eq!(app.input_text(), draft);
}

#[test]
fn user_code_is_not_an_executable_ui_and_tiny_views_are_safe() {
    let mut app = app();
    app.messages[0].role = "user".into();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(app.history_cache.widgets.is_empty());
    assert!(app.interactive.entries.is_empty());
    for width in [0, 1, 4, 8, 12] {
        let mut terminal = Terminal::new(TestBackend::new(width, 4)).unwrap();
        let mut widgets = Widgets::default();
        terminal
            .draw(|f| {
                widgets.begin_frame(false);
                widgets.paint(
                    f,
                    f.area(),
                    0,
                    &[placement(COUNTER, 0)],
                    &Theme::default(),
                    true,
                );
                if widgets.focus_latest() {
                    widgets.popup(f, &Theme::default(), f.area(), false);
                }
            })
            .unwrap();
    }
}
