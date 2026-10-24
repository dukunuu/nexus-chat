use super::*;
use ratatui::{Terminal, backend::TestBackend};

const SOURCE: &str = r#"root = Panel("A better weekend", [heading, metrics, toggle, actions], "rounded", "accent")
$ready = false
$count = 0
heading = Hero("Make space for something good", "A small plan. A fresh perspective.", "WEEKEND EDIT")
metrics = Stats([Stat("3", "days away"), Stat("2", "new places")])
toggle = Toggle($ready, "I'm ready")
actions = Buttons([Button("Add a day", @Set($count, $count + 1), true)])"#;

fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn text_and_elements_appear_mid_statement_without_exposing_source() {
    let source = "root = Panel(\"Weekend\", [heading, next])\nheading = Hero(\"A little advent";
    let document = Document::preview(source);
    let buffer = streaming_buffer(document, 80, &Theme::default());
    let rendered = text(&buffer);
    assert!(rendered.contains("A little advent"), "{rendered}");
    assert!(rendered.contains("composing · 1 element"), "{rendered}");
    assert!(!rendered.contains("F2"));
    assert!(!rendered.contains("root ="));
    assert!(!rendered.contains("Hero("));
    assert!(!rendered.contains("generating UI"));
    assert_eq!(source_height(source, 80), buffer.area.height);
}

#[test]
fn every_streamed_revision_is_inert_and_uses_one_disposable_parser() {
    let mut terminal = Terminal::new(TestBackend::new(82, 40)).unwrap();
    let mut widgets = Widgets::default();
    for end in SOURCE
        .char_indices()
        .map(|(index, _)| index)
        .chain([SOURCE.len()])
    {
        let source = &SOURCE[..end];
        let placement = Placement {
            row: 0,
            width: 80,
            owner: 7,
            ordinal: 0,
            height: source_height(source, 80),
            source: source.into(),
            complete: false,
        };
        terminal
            .draw(|frame| {
                widgets.begin_frame(true);
                widgets.paint(
                    frame,
                    frame.area(),
                    0,
                    std::slice::from_ref(&placement),
                    &Theme::default(),
                    true,
                );
            })
            .unwrap();
        assert!(
            widgets.entries.is_empty(),
            "previews must not consume panel-instance slots"
        );
        assert!(widgets.hits.is_empty());
        assert!(!widgets.focus_latest());
        assert!(widgets.previews.len() <= 1);
    }
    let placement = Placement {
        row: 0,
        width: 80,
        owner: 7,
        ordinal: 0,
        height: source_height(SOURCE, 80),
        source: SOURCE.into(),
        complete: true,
    };
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
    assert!(widgets.previews.is_empty());
    assert_eq!(widgets.entries.len(), 1);
    assert!(!widgets.hits.is_empty());
    assert!(widgets.focus_latest());
    let entry = widgets.entries.values_mut().next().unwrap();
    assert_eq!(entry.session.as_ref().unwrap().value("count"), "0");
    assert_eq!(entry.activate(1, true, true), None);
    assert_eq!(entry.session.as_ref().unwrap().value("count"), "1");
}

#[test]
fn preview_clipping_and_tiny_widths_are_safe() {
    for width in [0, 1, 2, 4, 8, 30, 80] {
        for source in ["", "root = Panel(\"Pending\", [title])", SOURCE] {
            let buffer = streaming_buffer(Document::preview(source), width, &Theme::default());
            assert_eq!(buffer.area.width, width);
            assert_eq!(buffer.area.height, source_height(source, width));
        }
    }
    let mut widgets = Widgets::default();
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
    let placement = Placement {
        row: 0,
        width: 38,
        owner: 0,
        ordinal: 0,
        height: source_height(SOURCE, 38),
        source: SOURCE.into(),
        complete: false,
    };
    terminal
        .draw(|frame| {
            widgets.begin_frame(false);
            widgets.paint(
                frame,
                frame.area(),
                3,
                &[placement],
                &Theme::default(),
                true,
            );
        })
        .unwrap();
    assert!(widgets.hits.is_empty());
}

#[test]
fn refined_cards_and_badges_use_theme_surfaces_not_hardcoded_colors() {
    let source = "root = Panel(\"Demo\", [Hero(\"Explore\", null, \"NEW\"), Cards([Card(\"Stay curious\", \"One small discovery every day\", \"TIP\")])])";
    let session = Session::new(Document::parse(source).unwrap()).unwrap();
    let theme = Theme::default();
    let entry = Entry {
        session: Ok(session),
        focus: 0,
        error: String::new(),
        scroll: 0,
    };
    let (buffer, _) = entry.canvas(80, &theme, false, true);
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "N" && cell.bg == theme.accent2 && cell.fg == theme.bg)
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "T" && cell.bg == theme.accent2 && cell.fg == theme.bg),
        "card badges keep their own foreground/background inside the grid"
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "╭" && cell.bg == theme.raised)
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "S" && cell.bg == theme.raised)
    );
}
