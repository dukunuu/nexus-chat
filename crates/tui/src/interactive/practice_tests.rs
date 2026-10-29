use super::*;
use ratatui::{Terminal, backend::TestBackend};

const QUIZ: &str = r#"root = Panel("Interview", [quiz])
$pick = ""
$code = "def rev(xs):"
quiz = Quiz([
  Question("Which lookup is O(1) on average?", [Choice($pick, "Pick one", ["Hash map", "Sorted list"], "Hash map")], "Hash map", "Hashing finds the bucket directly."),
  Question("Reverse a list in place", [Editor($code, "Python", "python", 4)])
])"#;

fn placed(source: &str) -> (Widgets, Placement) {
    let p = Placement {
        row: 0,
        width: 60,
        owner: 0,
        ordinal: 0,
        height: source_height(source, 60),
        source: source.into(),
        complete: true,
    };
    (Widgets::default(), p)
}
fn draw(widgets: &mut Widgets, p: &Placement) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            widgets.begin_frame(true);
            widgets.paint(
                f,
                f.area(),
                0,
                std::slice::from_ref(p),
                &Theme::default(),
                true,
            );
        })
        .unwrap();
    terminal.backend().buffer().clone()
}
fn rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim()
                .trim_matches('│')
                .trim()
                .to_string()
        })
        .filter(|row| !row.is_empty())
        .collect()
}
fn session<'a>(widgets: &'a Widgets, p: &Placement) -> &'a Session {
    widgets.entries[&p.key()].session.as_ref().unwrap()
}
fn press(widgets: &mut Widgets, code: KeyCode) {
    widgets.key(code.into(), true);
}

#[test]
fn quiz_pages_questions_marks_choices_and_reveals_answers_in_a_fixed_height() {
    let (mut widgets, p) = placed(QUIZ);
    let first = rows(&draw(&mut widgets, &p));
    assert!(
        first
            .iter()
            .any(|row| row.starts_with("‹ Question 1 of 2 ›") && row.ends_with("0/1 right")),
        "{first:#?}"
    );
    assert!(
        first
            .iter()
            .any(|row| row == "Which lookup is O(1) on average?")
    );
    assert!(first.iter().any(|row| row == "○ Hash map"));
    assert!(first.iter().any(|row| row == "[ Show answer ]"));
    assert!(!first.iter().any(|row| row.contains("Hashing finds")));
    assert!(widgets.focus_latest());
    assert!(matches!(
        &widgets.entries[&p.key()].controls()[..],
        [
            Control::Quiz(0),
            Control::Reveal(0),
            Control::Choice(1, 0),
            Control::Choice(1, 1)
        ]
    ));
    press(&mut widgets, KeyCode::Tab);
    press(&mut widgets, KeyCode::Tab);
    press(&mut widgets, KeyCode::Tab);
    press(&mut widgets, KeyCode::Enter);
    assert_eq!(session(&widgets, &p).value("pick"), "Sorted list");
    let picked = rows(&draw(&mut widgets, &p));
    assert!(picked.iter().any(|row| row == "✓ Hash map"), "{picked:#?}");
    assert!(picked.iter().any(|row| row == "✗ Sorted list"));
    widgets.entries.get_mut(&p.key()).unwrap().focus = 1;
    press(&mut widgets, KeyCode::Enter);
    let revealed = rows(&draw(&mut widgets, &p));
    assert!(revealed.iter().any(|row| row == "[ Hide answer ]"));
    assert!(revealed.iter().any(|row| row == "Hash map"));
    assert!(
        revealed
            .iter()
            .any(|row| row == "Hashing finds the bucket directly.")
    );
    let height = source_height(QUIZ, 60);
    widgets.entries.get_mut(&p.key()).unwrap().focus = 0;
    press(&mut widgets, KeyCode::Right);
    assert_eq!(session(&widgets, &p).question(0), 1);
    let second = rows(&draw(&mut widgets, &p));
    assert!(
        second
            .iter()
            .any(|row| row.starts_with("‹ Question 2 of 2 ›"))
    );
    assert!(second.iter().any(|row| row == "Reverse a list in place"));
    assert!(!second.iter().any(|row| row.contains("Show answer")));
    assert_eq!(
        widgets.entries[&p.key()]
            .canvas(60, &Theme::default(), true, true)
            .0
            .area
            .height,
        height
    );
}

#[test]
fn editor_takes_keys_until_esc_with_indent_newlines_and_paste() {
    let source = "root = Panel(\"Practice\", [code, Text(\"after\")])\n$code = \"def rev(xs):\"\ncode = Editor($code, \"Solution\", \"python\", 4)";
    let (mut widgets, p) = placed(source);
    draw(&mut widgets, &p);
    assert!(widgets.focus_latest());
    press(&mut widgets, KeyCode::Enter);
    assert!(session(&widgets, &p).editor(0).editing);
    press(&mut widgets, KeyCode::Enter);
    for ch in "return xs[::-1]".chars() {
        press(&mut widgets, KeyCode::Char(ch));
    }
    press(&mut widgets, KeyCode::Enter);
    press(&mut widgets, KeyCode::Backspace);
    widgets.paste("# p\tq\r\n");
    press(&mut widgets, KeyCode::Tab);
    press(&mut widgets, KeyCode::Char('v'));
    assert_eq!(
        session(&widgets, &p).value("code"),
        "def rev(xs):\n    return xs[::-1]\n# p    q\n    v"
    );
    let buffer = draw(&mut widgets, &p);
    let shown = rows(&buffer);
    assert!(
        shown.iter().any(|row| row == "1 def rev(xs):"),
        "{shown:#?}"
    );
    assert!(shown.iter().any(|row| row == "2     return xs[::-1]"));
    assert!(
        shown
            .iter()
            .any(|row| row.starts_with("Solution") && row.ends_with("Esc done · Tab indent"))
    );
    let theme = Theme::default();
    assert!(
        (0..buffer.area.height).any(|y| (0..buffer.area.width)
            .any(|x| buffer[(x, y)].bg == theme.accent && buffer[(x, y)].fg == theme.bg)),
        "the caret is drawn"
    );
    press(&mut widgets, KeyCode::Esc);
    assert!(!session(&widgets, &p).editor(0).editing);
    assert!(widgets.is_focused(), "the first Esc only stops editing");
    press(&mut widgets, KeyCode::Tab);
    assert!(widgets.is_focused());
    press(&mut widgets, KeyCode::Esc);
    assert!(!widgets.is_focused());
}

#[test]
fn clicking_an_editor_line_places_the_caret_there() {
    let source = "root = Panel(\"Practice\", [code])\n$code = \"first\\nsecond line\"\ncode = Editor($code, \"Solution\", null, 4)";
    let (mut widgets, p) = placed(source);
    draw(&mut widgets, &p);
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .clone();
    // Row 0 is the label; the gutter is "1 " wide, so x+2+3 is column 3.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x + 2 + 3,
        row: hit.rect.y + 2,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        widgets.click(mouse, Rect::new(0, 0, 80, 32), true),
        (true, None)
    );
    let state = session(&widgets, &p).editor(0);
    assert!(state.editing);
    assert_eq!(
        nexus_core::interactive::line_col(&session(&widgets, &p).value("code"), state.cursor),
        (1, 3)
    );
}

#[test]
fn editor_lines_are_highlighted_one_per_source_line() {
    let theme = Theme::default();
    let lines = practice::highlighted("def f():\n    return 1\n", "python", &theme);
    let text: Vec<_> = lines
        .iter()
        .map(|line| crate::ui::markdown::line_text(line))
        .collect();
    assert_eq!(text, ["def f():", "    return 1", ""]);
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.style.fg.is_some_and(|fg| fg != theme.fg)),
        "{:?}",
        lines[0]
    );
    assert_eq!(practice::highlighted("", "rust", &theme).len(), 1);
    let fenced = practice::highlighted("```\nx", "md", &theme);
    assert_eq!(
        fenced
            .iter()
            .map(|line| crate::ui::markdown::line_text(line))
            .collect::<Vec<_>>(),
        ["```", "x"]
    );
}
