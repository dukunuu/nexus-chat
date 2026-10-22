//! Images inside interactive blocks open the in-terminal viewer.
use super::*;
use nexus_core::app::App;
use nexus_core::db::Db;
use nexus_core::space::Space;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

const PIC: &str = "https://img.example.invalid/pic.png";
const PIC2: &str = "https://img.example.invalid/second.png";
const SCREEN: Rect = Rect::new(0, 0, 80, 32);

fn layout(view: Value) -> String {
    json!({"version":1,"title":"Gallery","view":view}).to_string()
}

/// Two image buttons whose action sets `n` to 1.
fn tiles() -> String {
    json!({
        "version":1,"title":"Pick","state":{"n":0},
        "view":[{"type":"buttons","items":[
            {"label":"Beach","action":"pick","body":["Quiet"],"image":PIC,"alt":"Beach"},
            {"label":"City","action":"pick","body":["Loud"],"image":PIC2,"alt":"City"}
        ]}],
        "actions":{"pick":[{"op":"set","key":"n","value":1}]}
    })
    .to_string()
}

fn placement(source: &str) -> Placement {
    Placement {
        row: 0,
        width: 70,
        owner: 1,
        ordinal: 0,
        height: source_height(source, 70),
        source: source.into(),
        complete: true,
    }
}

fn draw(widgets: &mut Widgets, placements: &[Placement]) {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            widgets.begin_frame(true);
            widgets.paint(f, f.area(), 0, placements, &Theme::default(), true);
        })
        .unwrap();
}

fn press(widgets: &mut Widgets, column: u16, row: u16) -> (bool, Option<String>) {
    widgets.click(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        SCREEN,
        true,
    )
}

fn image_hits(widgets: &Widgets) -> Vec<Hit> {
    widgets
        .hits
        .iter()
        .filter(|hit| hit.image.is_some())
        .cloned()
        .collect()
}

#[test]
fn an_image_node_opens_the_viewer_without_focusing_the_document() {
    let source = layout(json!([{"type":"image","src":PIC,"alt":"A cat","rows":5}]));
    let mut widgets = Widgets::default();
    let p = placement(&source);
    draw(&mut widgets, &[p]);
    let hits = image_hits(&widgets);
    assert_eq!(hits.len(), 1);
    let rect = hits[0].rect;
    assert_eq!(rect.height, 5, "the whole declared image is clickable");
    assert_eq!(press(&mut widgets, rect.x + 3, rect.y + 2), (true, None));
    assert_eq!(
        widgets.pending_image,
        Some(PendingImage {
            owner: 1,
            ordinal: 0,
            index: 0,
            url: PIC.into()
        })
    );
    assert!(!widgets.is_focused(), "viewing is not pressing");
}

#[test]
fn an_image_button_keeps_its_action_and_exposes_the_viewer_through_its_tag() {
    let source = tiles();
    let mut widgets = Widgets::default();
    let p = placement(&source);
    draw(&mut widgets, &[p.clone()]);

    // One tag per image button, 3 cells wide, drawn in the image's corner.
    let tags = image_hits(&widgets);
    assert_eq!(tags.len(), 2);
    assert!(tags.iter().all(|t| t.rect.width == 3 && t.rect.height == 1));

    // Clicking the tag views the image and does not press the button.
    let tag = &tags[1];
    assert_eq!(
        press(&mut widgets, tag.rect.x + 1, tag.rect.y),
        (true, None)
    );
    let pending = widgets.pending_image.take().unwrap();
    assert_eq!((pending.url.as_str(), pending.index), (PIC2, 1));
    assert_eq!(
        widgets.entries[&p.key()]
            .session
            .as_ref()
            .unwrap()
            .value("n"),
        "0"
    );

    // Clicking the button anywhere else still runs its action.
    let button = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(1))
        .unwrap()
        .clone();
    let (x, y) = (button.rect.x + 1, button.rect.bottom() - 1);
    assert!(!tags.iter().any(|t| t.rect.contains(Position::new(x, y))));
    assert_eq!(press(&mut widgets, x, y), (true, None));
    assert!(widgets.pending_image.is_none());
    assert_eq!(
        widgets.entries[&p.key()]
            .session
            .as_ref()
            .unwrap()
            .value("n"),
        "1"
    );
}

#[test]
fn v_views_the_focused_image_button_and_never_types_into_inputs() {
    let source = tiles();
    let mut widgets = Widgets::default();
    let p = placement(&source);
    draw(&mut widgets, &[p]);
    assert!(widgets.focus_latest());
    // Focus the second button (Tab), then `v`.
    widgets.key(KeyCode::Tab.into(), true);
    widgets.key(KeyCode::Char('v').into(), true);
    let pending = widgets.pending_image.take().unwrap();
    assert_eq!((pending.url.as_str(), pending.index), (PIC2, 1));

    // On a document with an input, `v` is text.
    let form = json!({
        "version":1,"title":"Form","state":{"name":""},
        "view":[{"type":"image","src":PIC},{"type":"input","key":"name","label":"Name"}]
    })
    .to_string();
    let mut widgets = Widgets::default();
    let p = placement(&form);
    draw(&mut widgets, &[p.clone()]);
    assert!(widgets.focus_latest());
    widgets.key(KeyCode::Char('v').into(), true);
    assert!(widgets.pending_image.is_none());
    assert_eq!(
        widgets.entries[&p.key()]
            .session
            .as_ref()
            .unwrap()
            .value("name"),
        "v"
    );
}

/// The click regions must sit exactly where the images are drawn, for every
/// node kind that carries one.
#[test]
fn image_targets_line_up_with_the_drawn_images() {
    let documents = [
        layout(
            json!([{"type":"text","text":"above"},{"type":"image","src":PIC,"alt":"A cat","rows":5}]),
        ),
        layout(json!([{"type":"hero","title":"Trip","badge":"new","image":PIC,"alt":"Harbour"}])),
        layout(json!([{"type":"cards","items":[
            {"title":"One","text":"First","image":PIC,"alt":"One photo"},
            {"title":"A rather long card title that wraps","badge":"b","text":"Second","image":PIC2,"alt":"Two photo"},
            {"title":"Three","text":"Third"},
            {"title":"Four","text":"Fourth","image":PIC,"alt":"Four photo"}
        ]}])),
        tiles(),
    ];
    for source in documents {
        let session = Session::new(Document::parse(&source).unwrap()).unwrap();
        for width in [40u16, 80, 110] {
            let entry = Entry {
                session: Ok(Session::new(Document::parse(&source).unwrap()).unwrap()),
                focus: 0,
                error: String::new(),
                scroll: 0,
            };
            let (buffer, _) = entry.canvas(width, &Theme::default(), false, true);
            let targets = image_targets(&session, width);
            let expected = document_images(session.document());
            assert_eq!(targets.len(), expected.len(), "{source} @ {width}");
            for (target, image) in targets.iter().zip(&expected) {
                assert_eq!(target.url, image.url);
                if target.rect.is_empty() {
                    continue;
                }
                let symbol = |x: u16, y: u16| buffer[(x, y)].symbol().to_string();
                if image.button.is_some() {
                    let tag: String = (0..3)
                        .map(|dx| symbol(target.rect.x + dx, target.rect.y))
                        .collect();
                    assert_eq!(tag, " v ", "{source} @ {width}: {target:?}");
                } else {
                    // The loading placeholder starts on the image's first row.
                    assert_eq!(
                        symbol(target.rect.x, target.rect.y),
                        "\u{25a3}",
                        "{source} @ {width}: {target:?}"
                    );
                }
            }
        }
    }
}

fn app() -> AppView {
    let root = std::env::temp_dir().join(format!("nexus-ui-viewer-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let mut app = AppView::new(App::new(
        Db::open_in_memory().unwrap(),
        Some("k"),
        Space { root },
    ));
    app.core.settings.image_viewer = "terminal".into();
    app
}

fn click(app: &mut AppView, column: u16, row: u16) -> bool {
    mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        SCREEN,
    )
}

fn paint(app: &mut AppView, placements: &[Placement]) {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            app.interactive.begin_frame(true);
            app.interactive
                .paint(f, f.area(), 0, placements, &Theme::default(), true);
        })
        .unwrap();
}

#[test]
fn clicking_an_image_node_in_the_transcript_opens_the_viewer_in_history_order() {
    let mut app = app();
    let source = layout(json!([{"type":"image","src":PIC,"rows":5}]));
    let p = placement(&source);
    app.history_cache.image_at_line = vec![Some("/before.png".into()), None];
    app.history_cache.widgets = vec![Placement {
        row: 4,
        ..p.clone()
    }];
    app.history_cache.image_at_line.resize(40, None);
    app.history_cache.image_at_line[30] = Some("/after.png".into());
    paint(&mut app, &[Placement { row: 0, ..p }]);
    let rect = image_hits(&app.interactive)[0].rect;
    assert!(click(&mut app, rect.x + 2, rect.y + 1));
    assert!(app.popup == Popup::ImageViewer);
    assert_eq!(app.image_viewer.current().unwrap().target, PIC);
    let targets: Vec<_> = app
        .image_viewer
        .items()
        .iter()
        .map(|i| i.target.as_str())
        .collect();
    assert_eq!(
        targets,
        ["/before.png", PIC, "/after.png"],
        "the block's image sits between the images around it"
    );
    assert_eq!(app.image_viewer.index(), 1);
    assert!(!app.interactive.is_focused());
}

#[test]
fn an_image_button_click_acts_but_its_tag_and_v_open_the_viewer_and_esc_returns() {
    let mut app = app();
    let source = tiles();
    let p = placement(&source);
    app.history_cache.widgets = vec![p.clone()];
    paint(&mut app, &[p.clone()]);

    // The tag opens the viewer, from the focused-document state.
    let tag = image_hits(&app.interactive)[0].rect;
    assert!(click(&mut app, tag.x, tag.y));
    assert!(app.popup == Popup::ImageViewer);
    assert_eq!(app.image_viewer.current().unwrap().target, PIC);
    assert_eq!(
        app.image_viewer.return_to(),
        crate::image_viewer::Return::Nothing
    );
    app.close_image_viewer();
    assert!(app.popup == Popup::None);

    // A click on the button runs its action and focuses the document.
    let button = app
        .interactive
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .rect;
    assert!(click(&mut app, button.x + 1, button.bottom() - 1));
    assert!(
        app.popup == Popup::InteractiveUi,
        "the button, not the viewer"
    );
    assert_eq!(
        app.interactive.entries[&p.key()]
            .session
            .as_ref()
            .unwrap()
            .value("n"),
        "1"
    );
    // `v` now views the focused button's image and Esc comes back to it.
    paint(&mut app, &[p]);
    key(&mut app, KeyCode::Char('v').into());
    assert!(app.popup == Popup::ImageViewer);
    assert_eq!(app.image_viewer.current().unwrap().target, PIC);
    assert_eq!(
        app.image_viewer.return_to(),
        crate::image_viewer::Return::Interactive
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    assert!(app.interactive.is_focused(), "the viewer keeps the focus");
    crate::ui::popups::image_viewer::handle_key(&mut app, KeyCode::Esc.into());
    assert!(app.popup == Popup::InteractiveUi);
}
