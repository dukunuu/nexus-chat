use super::*;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

fn source(days: i32) -> String {
    json!({
        "version":1,"title":"Planner","state":{"days":days,"ready":false},
        "view":[
            {"type":"toggle","key":"ready","label":"Ready"},
            {"type":"stepper","key":"days","label":"Days","min":1,"max":7,"step":2},
            {"type":"buttons","items":[{"label":"Save","action":"save"}]}
        ],"actions":{"save":[]}
    })
    .to_string()
}

fn entry(source: &str) -> Entry {
    Entry {
        session: Ok(Session::new(Document::parse(source).unwrap()).unwrap()),
        focus: 1,
        error: String::new(),
        scroll: 0,
    }
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

#[test]
fn stepper_renders_one_row_at_80_columns_with_dim_label_and_bound_arrows() {
    let theme = Theme::default();
    for (days, expected, left, right) in [
        (5, "5", theme.fg, theme.fg),
        (1, "1", theme.fg_dim, theme.fg),
        (7, "7", theme.fg, theme.fg_dim),
    ] {
        let entry = entry(&source(days));
        let (buffer, hits) = entry.canvas(80, &theme, false, true);
        assert!(row(&buffer, 4).contains(&format!("Days  ◀ {expected} ▶")));
        assert_eq!(buffer[(2, 4)].fg, theme.fg_dim);
        assert_eq!(buffer[(8, 4)].fg, left);
        assert_eq!(buffer[(12, 4)].fg, right);
        assert!(buffer[(10, 4)].modifier.contains(Modifier::BOLD));
        assert_eq!(hits[1], (Rect::new(2, 4, 76, 1), 1));
    }
    let entry = entry(&source(1));
    let (buffer, _) = entry.canvas(80, &theme, true, true);
    assert_eq!(buffer[(8, 4)].fg, theme.fg_dim);
    assert_eq!(buffer[(12, 4)].fg, theme.accent);
    assert!(buffer[(10, 4)].modifier.contains(Modifier::UNDERLINED));
}

fn placement() -> Placement {
    let source = source(5);
    Placement {
        row: 0,
        width: 80,
        owner: 0,
        ordinal: 0,
        height: source_height(&source, 80),
        source,
        complete: true,
    }
}

fn draw(widgets: &mut Widgets, placement: &Placement) {
    let mut terminal = Terminal::new(TestBackend::new(82, 32)).unwrap();
    terminal
        .draw(|frame| {
            widgets.begin_frame(false);
            widgets.paint(
                frame,
                frame.area(),
                0,
                std::slice::from_ref(placement),
                &Theme::default(),
                true,
            );
        })
        .unwrap();
}

fn days(widgets: &Widgets, placement: &Placement) -> String {
    widgets.entries[&placement.key()]
        .session
        .as_ref()
        .unwrap()
        .value("days")
}

#[test]
fn tab_focus_and_left_right_keys_adjust_locally_without_wrapping_or_messages() {
    let p = placement();
    let mut widgets = Widgets::default();
    draw(&mut widgets, &p);
    assert!(widgets.focus_latest());
    assert!(matches!(
        &widgets.entries[&p.key()].controls()[..],
        [Control::Toggle(_), Control::Stepper(1), Control::Action(_)]
    ));
    widgets.key(KeyCode::Tab.into(), false);
    assert_eq!(widgets.entries[&p.key()].focus, 1);
    for (code, expected) in [
        (KeyCode::Right, "7"),
        (KeyCode::Right, "7"),
        (KeyCode::Left, "5"),
        (KeyCode::Left, "3"),
        (KeyCode::Left, "1"),
        (KeyCode::Left, "1"),
        (KeyCode::Enter, "1"),
        (KeyCode::Char(' '), "1"),
    ] {
        assert_eq!(widgets.key(code.into(), false), (false, None));
        assert_eq!(days(&widgets, &p), expected);
        assert_eq!(widgets.entries[&p.key()].focus, 1);
    }
    for expected in [2, 3, 0] {
        widgets.key(KeyCode::Tab.into(), false);
        assert_eq!(widgets.entries[&p.key()].focus, expected);
    }
}

fn click(widgets: &mut Widgets, offset: u16, screen: Rect) {
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(1))
        .unwrap();
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x + offset,
        row: hit.rect.y,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(widgets.click(event, screen, false), (true, None));
}

#[test]
fn clicks_only_adjust_on_arrows_inline_and_in_the_expanded_view() {
    let p = placement();
    let mut widgets = Widgets::default();
    draw(&mut widgets, &p);
    let screen = Rect::new(0, 0, 82, 32);
    for (offset, expected) in [(0, "5"), (8, "5"), (10, "7"), (10, "7"), (6, "5")] {
        click(&mut widgets, offset, screen);
        assert_eq!(days(&widgets, &p), expected);
        assert_eq!(widgets.entries[&p.key()].focus, 1);
        draw(&mut widgets, &p);
    }
    let mut terminal = Terminal::new(TestBackend::new(90, 32)).unwrap();
    terminal
        .draw(|frame| {
            widgets.popup(frame, &Theme::default(), Rect::new(3, 4, 80, 20), false);
        })
        .unwrap();
    click(&mut widgets, 6, Rect::new(0, 0, 90, 32));
    assert_eq!(days(&widgets, &p), "3");
}

#[test]
fn reserved_height_matches_drawing_even_for_narrow_steppers() {
    let source = source(5);
    let entry = entry(&source);
    for width in [0, 1, 2, 4, 8, 12, 40, 80] {
        let (buffer, _) = entry.canvas(width, &Theme::default(), false, true);
        assert_eq!(source_height(&source, width), buffer.area.height);
        let regions = node_regions(entry.session.as_ref().unwrap(), width);
        assert_eq!(regions[1].height, 1);
        assert_eq!(regions[1].bottom() + 4, buffer.area.height);
    }
}

#[test]
fn prompt_app_has_an_80_column_render_and_state_driven_progress() {
    let source = nexus_core::interactive::INSTRUCTIONS
        .split("```nexus-ui\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    let mut entry = entry(source);
    let (buffer, _) = entry.canvas(80, &Theme::default(), false, true);
    let card_row = |content: &str| format!("│{content:<78}│");
    let expected = [
        format!("╭─ Trip planner {} ⤢ ╮", "─".repeat(60)),
        card_row(""),
        card_row(" Days  ◀ 5 ▶"),
        card_row(""),
        card_row(" ● Beach (2 days)"),
        card_row(" ○ Hiking (3 days)"),
        card_row(""),
        card_row(&format!(
            " {:<26}{}{:>32}",
            "Planned days",
            "▍".repeat(18),
            "2/5"
        )),
        card_row(""),
        card_row("  Save plan ↵ "),
        card_row(""),
        format!("╰{} F2 keyboard ╯", "─".repeat(65)),
    ];
    assert_eq!(buffer.area.height, 12);
    let actual: Vec<_> = (0..buffer.area.height).map(|y| row(&buffer, y)).collect();
    assert_eq!(actual.join("\n"), expected.join("\n"));
    assert_eq!(source_height(source, 80), buffer.area.height);
    let session = entry.session.as_mut().unwrap();
    session.toggle("hiking").unwrap();
    session.step(0, true).unwrap();
    let (updated, _) = entry.canvas(80, &Theme::default(), false, true);
    assert!(row(&updated, 2).contains("Days  ◀ 6 ▶"));
    assert!(row(&updated, 7).contains("5/6"));
}
