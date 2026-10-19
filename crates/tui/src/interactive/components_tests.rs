use super::*;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

fn source(view: Value, state: Value) -> String {
    json!({"version":1,"title":"Panel","state":state,"view":view,"actions":{}}).to_string()
}
fn entry(view: Value, state: Value) -> Entry {
    Entry {
        session: Ok(Session::new(Document::parse(&source(view, state)).unwrap()).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    }
}
fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .into()
}
fn snapshot(view: Value, state: Value, expected: &[String]) -> Buffer {
    let entry = entry(view, state);
    let (canvas, _) = entry.canvas(32, &Theme::default(), true, true);
    let mut terminal = Terminal::new(TestBackend::new(32, canvas.area.height)).unwrap();
    terminal
        .draw(|f| f.buffer_mut().clone_from(&canvas))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(row_text(buffer, 0), "Panel");
    let rows: Vec<_> = (2..buffer.area.height - 1)
        .map(|y| row_text(buffer, y))
        .collect();
    assert_eq!(rows, expected);
    buffer.clone()
}

#[test]
fn kv_snapshot_aligns_dim_labels_and_normal_values() {
    let buffer = snapshot(
        json!([{"type":"kv","items":[["Name","{{name}}"],["Job","Ready"]]}]),
        json!({"name":"Ada"}),
        &["Name Ada".into(), "Job  Ready".into()],
    );
    assert_eq!(buffer[(0, 2)].fg, Theme::default().fg_dim);
    assert_eq!(buffer[(5, 2)].fg, Theme::default().fg);
}

#[test]
fn table_snapshot_shows_header_selection_and_truncated_cells() {
    let buffer = snapshot(
        json!([{"type":"table","columns":["Name","Status"],"rows_data":[["Ada","Ready"],["Longer than the column","Waiting"]],"select":"row"}]),
        json!({"row":0}),
        &[
            format!(" {:<15}Status", "Name"),
            format!("▸{:<15}Ready", "Ada"),
            format!(" {:<15}Waiting", "Longer than t…"),
        ],
    );
    assert_eq!(buffer[(1, 2)].fg, Theme::default().fg_dim);
    assert_eq!(buffer[(1, 3)].fg, Theme::default().accent);
}

#[test]
fn progress_snapshot_clamps_and_uses_accent_bar() {
    let buffer = snapshot(
        json!([{"type":"progress","label":"Done","value":5,"max":10}]),
        json!({}),
        &[format!("{:<11}{}{:>13}", "Done", "▍".repeat(8), "5/10")],
    );
    assert_eq!(buffer[(11, 2)].fg, Theme::default().accent);
    snapshot(
        json!([{"type":"progress","label":"Done","value":99,"max":10}]),
        json!({}),
        &[format!("{:<11}{} 10/10", "Done", "▍".repeat(15))],
    );
}

#[test]
fn bars_snapshot_scales_to_largest_value() {
    snapshot(
        json!([{"type":"bars","items":[{"label":"A","value":2},{"label":"B","value":4},{"label":"C","value":-1}]}]),
        json!({}),
        &[
            format!("{:<11}{}{:>12}", "A", "▍".repeat(9), "2/4"),
            format!("{:<11}{} 4/4", "B", "▍".repeat(17)),
            format!("{:<29}0/4", "C"),
        ],
    );
}

#[test]
fn bars_keep_a_shared_scale_when_value_widths_differ() {
    snapshot(
        json!([{"type":"bars","items":[{"label":"A","value":9},{"label":"B","value":10}]}]),
        json!({}),
        &[
            format!("{:<11}{}{:>7}", "A", "▍".repeat(14), "9/10"),
            format!("{:<11}{} 10/10", "B", "▍".repeat(15)),
        ],
    );
}

#[test]
fn sparkline_snapshot_uses_declared_height_and_theme_glyphs() {
    for values in [json!([0, 1, 2, 3]), json!([0, 1e-9, 2e-9, 3e-9])] {
        snapshot(
            json!([{"type":"sparkline","values":values,"rows":2}]),
            json!({}),
            &["  ▎▍".into(), " ▍▍▍".into()],
        );
    }
}

#[test]
fn checklist_snapshot_shows_independent_boolean_values() {
    let buffer = snapshot(
        json!([{"type":"checklist","items":[{"label":"{{name}}","key":"ready"},{"label":"Done","key":"done"}]}]),
        json!({"name":"Ready","ready":false,"done":true}),
        &["○ Ready".into(), "● Done".into()],
    );
    assert_eq!(buffer[(0, 2)].fg, Theme::default().accent);
    assert_eq!(buffer[(0, 3)].fg, Theme::default().fg);
}

#[test]
fn select_snapshot_displays_label_and_current_option() {
    snapshot(
        json!([{"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]}]),
        json!({"mode":"Fast"}),
        &["Mode: < Fast >".into()],
    );
}

#[test]
fn toggle_snapshot_displays_label_and_boolean() {
    snapshot(
        json!([{"type":"toggle","key":"ready","label":"{{name}}"}]),
        json!({"ready":true,"name":"Ready"}),
        &["● Ready".into()],
    );
}

#[test]
fn mixed_document_snapshot_keeps_all_components_in_fixed_slots() {
    snapshot(
        json!([
            {"type":"kv","items":[["Name","{{name}}"]]},
            {"type":"table","columns":["Name"],"rows_data":[["Ada"]]},
            {"type":"progress","label":"Done","value":1},
            {"type":"bars","items":[{"label":"A","value":1}]},
            {"type":"sparkline","values":[0,1]},
            {"type":"checklist","items":[{"label":"Ready","key":"ready"}]},
            {"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]},
            {"type":"toggle","key":"ready","label":"Ready"}
        ]),
        json!({"name":"Ada","ready":false,"mode":"Fast"}),
        &[
            "Name Ada".into(),
            "Name".into(),
            "Ada".into(),
            format!("{:<11}{} 1/1", "Done", "▍".repeat(17)),
            format!("{:<11}{} 1/1", "A", "▍".repeat(17)),
            " ▍".into(),
            "○ Ready".into(),
            "Mode: < Fast >".into(),
            "○ Ready".into(),
        ],
    );
}

fn panel() -> (Widgets, Placement) {
    let source = source(
        json!([
            {"type":"buttons","items":[{"label":"Action","action":"noop"}]},
            {"type":"input","key":"name","label":"Name"},
            {"type":"checklist","items":[{"label":"Ready","key":"ready"},{"label":"Done","key":"done"}]},
            {"type":"table","columns":["Name"],"rows_data":[["A"],["B"],["C"]],"select":"row"},
            {"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]},
            {"type":"toggle","key":"ready","label":"Ready"}
        ]),
        json!({"name":"Ada","ready":false,"done":false,"mode":"Fast","row":0}),
    );
    let mut data: Value = serde_json::from_str(&source).unwrap();
    data["actions"]["noop"] = json!([]);
    let source = data.to_string();
    let p = Placement {
        row: 0,
        width: 60,
        owner: 0,
        ordinal: 0,
        height: Document::parse(&source).unwrap().rows(),
        source,
        complete: true,
    };
    (Widgets::default(), p)
}
fn draw(widgets: &mut Widgets, p: &Placement, top: usize) {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            widgets.begin_frame(true);
            widgets.paint(
                f,
                f.area(),
                top,
                std::slice::from_ref(p),
                &Theme::default(),
                true,
            );
        })
        .unwrap();
}
fn click_control(widgets: &mut Widgets, control: usize, row: u16) {
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(control))
        .unwrap();
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x,
        row: hit.rect.y + row,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        widgets.click(mouse, Rect::new(0, 0, 80, 32), true),
        (true, None)
    );
}
fn value(widgets: &Widgets, p: &Placement, key: &str) -> String {
    widgets.entries[&p.key()]
        .session
        .as_ref()
        .unwrap()
        .value(key)
}

#[test]
fn focus_order_and_keys_include_new_controls_without_stealing_input() {
    let (mut widgets, p) = panel();
    draw(&mut widgets, &p, 0);
    assert!(widgets.focus_latest());
    let controls = widgets.entries[&p.key()].controls();
    assert!(matches!(
        &controls[..],
        [
            Control::Action(_),
            Control::Input(_),
            Control::Toggle(_),
            Control::Toggle(_),
            Control::Table(..),
            Control::Select(_),
            Control::Toggle(_)
        ]
    ));
    for expected in 1..7 {
        widgets.key(KeyCode::Tab.into(), true);
        assert_eq!(widgets.entries[&p.key()].focus, expected);
        match expected {
            1 => {
                widgets.key(KeyCode::Char('!').into(), true);
            }
            2 => {
                widgets.key(KeyCode::Char(' ').into(), true);
            }
            3 => {
                widgets.key(KeyCode::Enter.into(), true);
            }
            4 => {
                for _ in 0..5 {
                    widgets.key(KeyCode::Down.into(), true);
                }
                assert_eq!(value(&widgets, &p, "row"), "2");
                for _ in 0..5 {
                    widgets.key(KeyCode::Up.into(), true);
                }
                assert_eq!(value(&widgets, &p, "row"), "0");
            }
            5 => {
                widgets.key(KeyCode::Left.into(), true);
                assert_eq!(value(&widgets, &p, "mode"), "Full");
                widgets.key(KeyCode::Right.into(), true);
                assert_eq!(value(&widgets, &p, "mode"), "Fast");
            }
            6 => {
                widgets.key(KeyCode::Enter.into(), true);
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(value(&widgets, &p, "name"), "Ada!");
    assert_eq!(value(&widgets, &p, "ready"), "false");
    assert_eq!(value(&widgets, &p, "done"), "true");
    widgets.key(KeyCode::Tab.into(), true);
    assert_eq!(widgets.entries[&p.key()].focus, 0);
}

#[test]
fn mouse_toggles_checklist_and_toggle_cycles_select_and_selects_table_rows() {
    let (mut widgets, p) = panel();
    draw(&mut widgets, &p, 0);
    click_control(&mut widgets, 3, 0);
    assert_eq!(value(&widgets, &p, "done"), "true");
    click_control(&mut widgets, 5, 0);
    assert_eq!(value(&widgets, &p, "mode"), "Full");
    click_control(&mut widgets, 5, 0);
    assert_eq!(value(&widgets, &p, "mode"), "Fast");
    click_control(&mut widgets, 6, 0);
    assert_eq!(value(&widgets, &p, "ready"), "true");
    click_control(&mut widgets, 4, 3);
    assert_eq!(value(&widgets, &p, "row"), "2");
    // The table starts at document row 10; clipping away header and first data row
    // must not make a click on the next visible row select the wrong index.
    draw(&mut widgets, &p, 12);
    click_control(&mut widgets, 4, 0);
    assert_eq!(value(&widgets, &p, "row"), "1");
}

#[test]
fn labels_are_sanitized_and_narrow_component_views_are_safe() {
    assert_eq!(safe("A\u{1b}\t\rB\u{7f}\u{fe0f}"), "AB");
    snapshot(
        json!([{"type":"toggle","key":"ready","label":"Ready\u{fe0f}"}]),
        json!({"ready":true}),
        &["● Ready".into()],
    );
    assert_eq!(clipped("宽宽宽", 5), "宽宽…");
    let entry = entry(
        json!([
            {"type":"kv","items":[["Long label","Value"]]},
            {"type":"table","columns":["A","B"],"rows_data":[["Wide","Cell"]],"select":"row"},
            {"type":"progress","label":"Progress","value":1},
            {"type":"bars","items":[{"label":"A","value":1}]},
            {"type":"sparkline","values":[0,0,0]},
            {"type":"checklist","items":[{"label":"A","key":"ready"}]},
            {"type":"select","key":"mode","label":"Mode","options":["A","B"]},
            {"type":"toggle","key":"ready","label":"A"}
        ]),
        json!({"row":0,"ready":false,"mode":"A"}),
    );
    for width in [0, 1, 2, 4, 8] {
        entry.canvas(width, &Theme::default(), true, true);
    }
}

#[test]
fn legacy_game_matches_the_original_widget_buffer() {
    let document = Document::parse(include_str!(
        "../../../core/tests/fixtures/interactive/rps.json"
    ))
    .unwrap();
    let entry = Entry {
        session: Ok(Session::new(document).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    };
    let theme = Theme::default();
    let mut expected = Buffer::empty(Rect::new(0, 0, 70, 17));
    expected.set_style(expected.area, theme.background_style());
    Paragraph::new("Rock · Paper · Scissors")
        .style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .render(Rect::new(0, 0, 70, 1), &mut expected);
    for (y, text, color) in [
        (2, "You: Pick a move · Computer: ?", theme.accent),
        (3, "Ready", theme.accent),
        (4, "Wins 0 · Losses 0 · Draws 0", theme.fg_dim),
        (
            16,
            "F2 interact · click controls · Esc back from focus",
            theme.fg_dim,
        ),
    ] {
        Paragraph::new(text)
            .style(Style::default().fg(color))
            .render(Rect::new(0, y, 70, 1), &mut expected);
    }
    let bodies: [&[&str]; 3] = [
        &[
            "   _______",
            "--'   ____)",
            "     (_____)",
            "     (____)",
            "--.__(___)",
            "beats scissors",
        ],
        &[
            "   _______",
            "--'   ____)____",
            "         ______)",
            "        _______)",
            "--.__________)",
            "beats rock",
        ],
        &[
            "   _______",
            "--'   ____)____",
            "      _________)",
            "     (____)",
            "--.__(___)",
            "beats paper",
        ],
    ];
    let buttons: Vec<_> = ["Rock", "Paper", "Scissors"]
        .iter()
        .enumerate()
        .map(|(action, label)| chrome::Button {
            action,
            label,
            body: bodies[action],
            selected: false,
            enabled: true,
        })
        .collect();
    chrome::render_buttons(&mut expected, Rect::new(0, 5, 70, 8), &buttons, &theme);
    chrome::render_buttons(
        &mut expected,
        Rect::new(0, 13, 70, 3),
        &[chrome::Button {
            action: 3,
            label: "Reset score",
            body: &[],
            selected: false,
            enabled: true,
        }],
        &theme,
    );
    let (actual, _) = entry.canvas(70, &theme, false, true);
    let mut terminal = Terminal::new(TestBackend::new(70, 17)).unwrap();
    terminal
        .draw(|f| f.buffer_mut().clone_from(&actual))
        .unwrap();
    assert_eq!(terminal.backend().buffer(), &expected);
}

#[test]
fn legacy_counter_matches_the_original_widget_buffer() {
    let document = Document::parse(super::tests::COUNTER).unwrap();
    let entry = Entry {
        session: Ok(Session::new(document).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    };
    let theme = Theme::default();
    let mut expected = Buffer::empty(Rect::new(0, 0, 70, 13));
    expected.set_style(expected.area, theme.background_style());
    Paragraph::new("Counter")
        .style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .render(Rect::new(0, 0, 70, 1), &mut expected);
    Paragraph::new("Count: 0")
        .style(Style::default().fg(theme.accent))
        .render(Rect::new(0, 2, 70, 1), &mut expected);
    let buttons = [
        chrome::Button {
            action: 0,
            label: "Add one",
            body: &[],
            selected: false,
            enabled: true,
        },
        chrome::Button {
            action: 1,
            label: "Reset",
            body: &[],
            selected: false,
            enabled: true,
        },
    ];
    chrome::render_buttons(&mut expected, Rect::new(0, 3, 70, 3), &buttons, &theme);
    chrome::popup_block_focused("Name", &theme, false, chrome::Tone::Normal)
        .render(Rect::new(0, 6, 70, 3), &mut expected);
    expected.set_style(Rect::new(2, 7, 66, 1), Style::default().fg(theme.fg));
    chrome::render_buttons(
        &mut expected,
        Rect::new(0, 9, 70, 3),
        &[chrome::Button {
            action: 3,
            label: "Submit result [review]",
            body: &[],
            selected: false,
            enabled: true,
        }],
        &theme,
    );
    Paragraph::new("F2 interact · click controls · Esc back from focus")
        .style(Style::default().fg(theme.fg_dim))
        .render(Rect::new(0, 12, 70, 1), &mut expected);
    let (actual, _) = entry.canvas(70, &theme, false, true);
    // Includes every character, foreground/background and modifier, not just text.
    let mut terminal = Terminal::new(TestBackend::new(70, 13)).unwrap();
    terminal
        .draw(|f| f.buffer_mut().clone_from(&actual))
        .unwrap();
    assert_eq!(terminal.backend().buffer(), &expected);
}
