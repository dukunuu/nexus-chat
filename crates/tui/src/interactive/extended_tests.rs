use super::*;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

fn source(node: Value, state: Value) -> String {
    json!({"version":1,"title":"Demo","view":[node],"state":state,"actions":{"save":[{"op":"send","text":"Name={{name}}"}]}}).to_string()
}
fn entry(node: Value, state: Value) -> Entry {
    Entry {
        session: Ok(Session::new(Document::parse(&source(node, state)).unwrap()).unwrap()),
        focus: 0,
        error: String::new(),
        scroll: 0,
    }
}
fn content(entry: &Entry, width: u16) -> (String, Buffer) {
    let (canvas, _) = entry.canvas(width, &Theme::default(), true, true);
    let mut terminal = Terminal::new(TestBackend::new(width, canvas.area.height)).unwrap();
    terminal
        .draw(|frame| frame.buffer_mut().clone_from(&canvas))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let regions = node_regions(entry.session.as_ref().unwrap(), width);
    let text = regions
        .iter()
        .flat_map(|area| {
            (area.y..area.bottom()).map(|y| {
                (area.x..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    (text, buffer)
}
fn tabs() -> Value {
    json!({"type":"tabs","key":"tab","items":[{"label":"A","view":[{"type":"text","text":"One"}]},{"label":"B","view":[{"type":"text","text":"Two"}]}]})
}

#[test]
fn tabs_two_state_snapshots_keep_the_same_height_and_hit_identity() {
    let mut entry = entry(tabs(), json!({"tab":0}));
    assert_eq!(content(&entry, 24).0, " A         B\nOne");
    let height = entry
        .canvas(24, &Theme::default(), true, true)
        .0
        .area
        .height;
    assert!(entry.navigate(KeyCode::Right));
    assert_eq!(content(&entry, 24).0, " A         B\nTwo");
    assert_eq!(
        entry
            .canvas(24, &Theme::default(), true, true)
            .0
            .area
            .height,
        height
    );
    assert_eq!(entry.session.as_ref().unwrap().value("tab"), "1");
    let (_, buffer) = content(&entry, 24);
    assert_eq!(buffer[(13, 2)].fg, Theme::default().accent);
}

#[test]
fn accordion_snapshot_reserves_collapsed_children_and_supports_arrows() {
    let mut entry = entry(
        json!({"type":"accordion","items":[{"title":"More","view":[{"type":"text","text":"Details"}]}]}),
        json!({}),
    );
    assert_eq!(content(&entry, 24).0, "▸ More\n");
    assert!(entry.navigate(KeyCode::Right));
    assert_eq!(content(&entry, 24).0, "▾ More\nDetails");
    assert!(entry.navigate(KeyCode::Left));
    assert_eq!(content(&entry, 24).0, "▸ More\n");
}

#[test]
fn every_chart_kind_has_wide_and_narrow_backend_snapshots() {
    let mut snapshots = Vec::new();
    for kind in ["line", "area", "scatter", "bar", "pie"] {
        for width in [20, 36] {
            let entry = entry(
                json!({"type":"chart","kind":kind,"series":[{"name":"A","values":[0,2,1],"tone":"success"}],"labels":["Jan","Feb","Mar"],"rows":6}),
                json!({}),
            );
            let (text, buffer) = content(&entry, width);
            assert!(text.contains("● A"));
            assert!(text.contains("Jan") && text.contains("Mar"));
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.fg == Theme::default().success)
            );
            snapshots.push(format!("{kind}/{width}\n{text}"));
        }
    }
    assert_eq!(
        snapshots.join("\n---\n"),
        include_str!("chart_snapshots.txt").trim_end()
    );
}

#[test]
fn charts_handle_zero_negative_fractional_and_tiny_canvases() {
    for kind in ["line", "area", "scatter", "bar", "pie"] {
        for values in [
            json!([0, 0]),
            json!([0.001, 0.002]),
            if kind == "pie" {
                json!([1, 2])
            } else {
                json!([-2, -1])
            },
        ] {
            let entry = entry(
                json!({"type":"chart","kind":kind,"series":[{"name":"A","values":values}]}),
                json!({}),
            );
            for width in [4, 5, 8, 12, 40] {
                let _ = content(&entry, width);
            }
        }
    }
}

#[test]
fn rich_table_snapshots_show_sort_page_search_and_original_selection() {
    let mut entry = entry(
        json!({"type":"table","columns":["Name"],"rows_data":[["Zoe"],["Ada"],["Bob"]],"select":"row","sortable":true,"page_size":1,"search":true}),
        json!({"row":0}),
    );
    assert_eq!(
        content(&entry, 36).0,
        "/ Search:\n Name\n▸Zoe\n‹ 1/3 › · PgUp/PgDn"
    );
    assert!(entry.navigate(KeyCode::Char('s')));
    assert_eq!(
        content(&entry, 36).0,
        "/ Search:\n Name ▴\n Ada\n‹ 1/3 › · PgUp/PgDn"
    );
    assert!(entry.navigate(KeyCode::PageDown));
    assert_eq!(
        content(&entry, 36).0,
        "/ Search:\n Name ▴\n Bob\n‹ 2/3 › · PgUp/PgDn"
    );
    assert!(entry.navigate(KeyCode::Char('/')));
    entry.edit("zo", false);
    assert_eq!(
        content(&entry, 36).0,
        "/ Search: zo▁\n Name ▴\n▸Zoe\n‹ 1/1 › · PgUp/PgDn"
    );
}

#[test]
fn form_submit_snapshots_and_actions_respect_required_values_and_send_readiness() {
    let mut entry = entry(
        json!({"type":"form","title":"Profile","fields":[{"type":"input","key":"name","label":"Name","rules":{"required":true}}],"submit_label":"Save","action":"save"}),
        json!({"name":""}),
    );
    assert_eq!(content(&entry, 36).0, "Profile\nName\n\n[ Save ↵ ]");
    assert!(entry.activate(0, true, true).is_none());
    assert!(entry.error.contains("required"));
    entry.focus = 1;
    entry.edit("Ada", false);
    assert_eq!(content(&entry, 36).0, "Profile\nName  Ada▁\n\n[ Save ↵ ]");
    assert!(entry.activate(0, false, true).is_none());
    assert!(entry.error.contains("draft"));
    assert_eq!(
        entry.activate(0, true, true).as_deref(),
        Some("UI response — Demo\nName=Ada")
    );
}

#[test]
fn timeline_rating_badge_meter_slider_and_highlighted_code_snapshots() {
    let timeline = entry(
        json!({"type":"timeline","items":[{"time":"09:00","title":"Ready","text":"Details","tone":"success"},{"time":"10:00","title":"Done"}]}),
        json!({}),
    );
    assert_eq!(
        content(&timeline, 36).0,
        "● 09:00 Ready\n│ Details\n● 10:00 Done"
    );
    let rating = entry(
        json!({"type":"rating","key":"score","label":"Score"}),
        json!({"score":2}),
    );
    assert_eq!(content(&rating, 36).0, "Score: ★★○○○");
    let badge = entry(
        json!({"type":"badge","text":"Ready","tone":"success"}),
        json!({}),
    );
    assert_eq!(content(&badge, 36).0, "[Ready]");
    let meter = entry(
        json!({"type":"meter","label":"Done","value":2,"max":4,"tone":"warning"}),
        json!({}),
    );
    let (text, buffer) = content(&meter, 24);
    assert_eq!(text, "██████Done 2/4");
    assert_eq!(buffer[(2, 2)].fg, Theme::default().warning);
    let slider = entry(
        json!({"type":"slider","key":"n","label":"N","min":0,"max":4,"step":1}),
        json!({"n":2}),
    );
    assert_eq!(content(&slider, 24).0, "N: 2   ──────●──────");
    let mut code = entry(
        json!({"type":"code","source":"let x = 1;","lang":"rust"}),
        json!({}),
    );
    let (text, buffer) = content(&code, 36);
    assert_eq!(text, "rust  [Copy]\nlet x = 1;");
    assert_ne!(
        buffer[(2, 3)].fg,
        buffer[(10, 3)].fg,
        "keywords and numbers are highlighted"
    );
    code.activate(0, true, true);
    assert_eq!(
        code.session.as_mut().unwrap().take_copy().as_deref(),
        Some("let x = 1;")
    );
}

fn draw(widgets: &mut Widgets, source: &str) {
    let placement = Placement {
        row: 0,
        width: 40,
        owner: 0,
        ordinal: 0,
        height: source_height(source, 40),
        source: source.into(),
        complete: true,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
    terminal
        .draw(|frame| {
            widgets.begin_frame(true);
            widgets.paint(
                frame,
                frame.area(),
                0,
                &[placement],
                &Theme::default(),
                true,
            );
        })
        .unwrap();
}
fn click(widgets: &mut Widgets, control: usize, x: u16, y: u16) -> Option<String> {
    let area = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(control))
        .unwrap()
        .rect;
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + x,
        row: area.y + y,
        modifiers: KeyModifiers::NONE,
    };
    let (handled, message) = widgets.click(event, Rect::new(0, 0, 80, 40), true);
    assert!(handled);
    message
}
fn session(widgets: &Widgets) -> &Session {
    widgets
        .entries
        .values()
        .next()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
}

#[test]
fn tabs_accordion_rating_and_slider_route_mouse_and_keyboard() {
    let mut widgets = Widgets::default();
    let src = source(tabs(), json!({"tab":0}));
    draw(&mut widgets, &src);
    click(&mut widgets, 0, 30, 0);
    assert_eq!(session(&widgets).value("tab"), "1");
    widgets.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), true);
    assert_eq!(session(&widgets).value("tab"), "0");

    let mut widgets = Widgets::default();
    let src = source(
        json!({"type":"accordion","items":[{"title":"More","view":[{"type":"text","text":"Details"}]}]}),
        json!({}),
    );
    draw(&mut widgets, &src);
    click(&mut widgets, 0, 0, 0);
    assert!(session(&widgets).section_open(0, 0));
    widgets.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), true);
    assert!(!session(&widgets).section_open(0, 0));

    let mut widgets = Widgets::default();
    let src = source(
        json!({"type":"rating","key":"n","label":"Score","max":5}),
        json!({"n":0}),
    );
    draw(&mut widgets, &src);
    click(&mut widgets, 0, 9, 0);
    assert_eq!(session(&widgets).value("n"), "3");
    widgets.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), true);
    assert_eq!(session(&widgets).value("n"), "4");

    let mut widgets = Widgets::default();
    let src = source(
        json!({"type":"slider","key":"n","label":"N","min":0,"max":10,"step":1}),
        json!({"n":0}),
    );
    draw(&mut widgets, &src);
    let width = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .rect
        .width;
    click(&mut widgets, 0, width - 1, 0);
    assert_eq!(session(&widgets).value("n"), "10");
    widgets.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), true);
    assert_eq!(session(&widgets).value("n"), "9");
}

#[test]
fn code_copy_routes_mouse_and_keyboard_without_touching_the_clipboard() {
    let mut widgets = Widgets::default();
    let src = source(
        json!({"type":"code","source":"let x = 1;","lang":"rust"}),
        json!({}),
    );
    draw(&mut widgets, &src);
    click(&mut widgets, 0, 2, 0);
    let current = widgets
        .entries
        .values_mut()
        .next()
        .unwrap()
        .session
        .as_mut()
        .unwrap();
    assert_eq!(current.take_copy().as_deref(), Some("let x = 1;"));
    widgets.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), true);
    let current = widgets
        .entries
        .values_mut()
        .next()
        .unwrap()
        .session
        .as_mut()
        .unwrap();
    assert_eq!(current.take_copy().as_deref(), Some("let x = 1;"));
}

#[test]
fn nested_controls_and_images_use_stable_addresses_across_hidden_tabs() {
    let mut entry = entry(
        json!({"type":"tabs","items":[{"label":"A","view":[{"type":"image","src":"https://example.invalid/a.png","rows":4}]},{"label":"B","view":[{"type":"image","src":"https://example.invalid/b.png","rows":4},{"type":"buttons","items":[{"label":"Save","action":"save"}]}]}]}),
        json!({"name":"Ada"}),
    );
    assert_eq!(
        image_targets(entry.session.as_ref().unwrap(), 40)[0].index,
        0
    );
    entry.navigate(KeyCode::Right);
    let session = entry.session.as_ref().unwrap();
    assert_eq!(document_images(session.document()).len(), 2);
    assert_eq!(image_targets(session, 40)[0].index, 1);
    assert_eq!(entry.controls().len(), 2);
    assert_eq!(
        entry.activate(1, true, true).as_deref(),
        Some("UI response — Demo\nName=Ada")
    );
}

#[test]
fn table_search_digits_are_text_not_answer_shortcuts() {
    let src = json!({"version":1,"title":"Search","view":[{"type":"table","columns":["Name"],"rows_data":[["Item 1"]],"search":true},{"type":"buttons","items":[{"label":"Answer","action":"answer"}]}],"actions":{"answer":[{"op":"answer"}]}}).to_string();
    let mut widgets = Widgets::default();
    draw(&mut widgets, &src);
    click(&mut widgets, 0, 0, 0);
    assert_eq!(
        widgets.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE), true),
        (false, None)
    );
    assert_eq!(session(&widgets).table_state(0).query, "1");
}

#[test]
fn multiseries_donut_has_a_legend_and_two_themed_segments() {
    let entry = entry(
        json!({"type":"chart","kind":"pie","series":[{"name":"Used","values":[1],"tone":"success"},{"name":"Free","values":[3],"tone":"warning"}],"rows":8}),
        json!({}),
    );
    let (text, buffer) = content(&entry, 40);
    assert!(text.contains("Used 1") && text.contains("Free 3"));
    let segment_count = |tone| {
        buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() == glyph::SECTION && cell.fg == tone)
            .count()
    };
    assert!(segment_count(Theme::default().success) > 0);
    assert!(segment_count(Theme::default().warning) > segment_count(Theme::default().success));
}

#[test]
fn table_mouse_header_footer_search_and_form_mouse_submission_work() {
    let mut widgets = Widgets::default();
    let src = source(
        json!({"type":"table","columns":["Name"],"rows_data":[["Zoe"],["Ada"]],"sortable":true,"page_size":1,"search":true,"select":"row"}),
        json!({"row":0}),
    );
    draw(&mut widgets, &src);
    click(&mut widgets, 0, 3, 1);
    assert_eq!(session(&widgets).table_rows(0), [1, 0]);
    click(&mut widgets, 0, 1, 2);
    assert_eq!(session(&widgets).value("row"), "1");
    click(&mut widgets, 0, 30, 3);
    assert_eq!(session(&widgets).table_state(0).page, 1);
    click(&mut widgets, 0, 2, 0);
    widgets.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), true);
    assert_eq!(session(&widgets).table_rows(0), [1]);

    let mut widgets = Widgets::default();
    let src = source(
        json!({"type":"form","title":"F","fields":[{"type":"input","key":"name","label":"Name","rules":{"required":true}}],"submit_label":"Save","action":"save"}),
        json!({"name":""}),
    );
    draw(&mut widgets, &src);
    assert!(click(&mut widgets, 0, 1, 0).is_none());
    click(&mut widgets, 1, 0, 0);
    widgets.paste("Ada");
    assert_eq!(
        click(&mut widgets, 0, 1, 0).as_deref(),
        Some("UI response — Demo\nName=Ada")
    );
}
