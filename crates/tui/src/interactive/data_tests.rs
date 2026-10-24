use super::*;
use nexus_core::interactive::live as data;
use nexus_core::provider::ToolDef;
use nexus_core::tools::ToolExecutor;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

const SOURCE: &str = r#"root = Panel("Live", [Text(q.text)])
q = Query("files", {action:"read",name:"a"}, {text:"Defaults"}, 15)"#;
#[derive(Default)]
struct Fake(AtomicUsize);
impl ToolExecutor for Fake {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "files".into(),
            description: String::new(),
            parameters: json!({}),
        }]
    }
    fn is_read_only(&self, _: &str, _: &str) -> bool {
        true
    }
    fn run<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = (String, String)> + Send + 'a>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(std::future::ready((
            r#"{"text":"Loaded"}"#.into(),
            "ok".into(),
        )))
    }
    fn supports_images(&self) -> bool {
        false
    }
    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
}
fn placement(complete: bool) -> Placement {
    Placement {
        row: 0,
        width: 70,
        owner: 0,
        ordinal: 0,
        height: source_height(SOURCE, 70),
        source: SOURCE.into(),
        complete,
    }
}
fn draw(widgets: &mut Widgets, placement: &Placement, top: usize) {
    Terminal::new(TestBackend::new(80, 24))
        .unwrap()
        .draw(|frame| {
            widgets.begin_frame(false);
            widgets.paint(
                frame,
                frame.area(),
                top,
                &[placement.clone()],
                &Theme::default(),
                true,
            );
        })
        .unwrap();
}
fn snapshot(entry: &Entry) -> Vec<String> {
    let (buffer, _) = entry.canvas(44, &Theme::default(), false, false);
    let body: Vec<_> = (1..buffer.area.height - 1)
        .map(|y| {
            (1..buffer.area.width - 1)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    let bottom = (0..buffer.area.width)
        .map(|x| buffer[(x, buffer.area.height - 1)].symbol())
        .collect::<String>();
    let hint = bottom
        .trim_matches(|ch: char| ch.is_whitespace() || "╰╯─".contains(ch))
        .to_string();
    body.into_iter().chain([hint]).collect()
}

#[tokio::test]
async fn loading_and_loaded_query_panel_snapshots() {
    let mut session = Session::new(Document::parse(SOURCE).unwrap()).unwrap();
    let now = Instant::now();
    session.load_data(now);
    let call = session.next_data_call(now, &mut 0).unwrap().unwrap();
    let mut entry = Entry {
        session: Ok(session),
        focus: 0,
        error: String::new(),
        scroll: 0,
    };
    assert_eq!(snapshot(&entry), ["Defaults", "Loading data"]);
    let outcome = data::execute(
        &call,
        "p1-1",
        Arc::new(Fake::default()),
        None,
        &nexus_core::trace::Scrubber::new(Vec::new()),
    )
    .await;
    entry
        .session
        .as_mut()
        .unwrap()
        .finish_data_call(&call, &outcome, now)
        .unwrap();
    assert_eq!(snapshot(&entry), ["Loaded", "F2 keyboard"]);
}

#[tokio::test]
async fn view_scheduler_keeps_streams_and_restored_panels_inert_pauses_and_logs_cards() {
    let root = std::env::temp_dir().join(format!("nexus-live-data-{}", uuid::Uuid::new_v4()));
    let core = nexus_core::app::App::new(
        nexus_core::db::Db::open_in_memory().unwrap(),
        None,
        nexus_core::space::Space { root: root.clone() },
    );
    let mut app = AppView::new(core);
    let row = app
        .db
        .create_session("data", "model", &app.active_space.id, "chat")
        .unwrap();
    app.core.switch_to_session_by_id(&row.id).unwrap();
    let executor = Arc::new(Fake::default());
    app.core.toolbox = executor.clone();
    let preview = placement(false);
    draw(&mut app.interactive, &preview, 0);
    live::tick(&mut app);
    assert!(app.interactive.entries.is_empty());
    let panel = placement(true);
    draw(&mut app.interactive, &panel, 0);
    live::tick(&mut app);
    assert_eq!(
        executor.0.load(Ordering::Relaxed),
        0,
        "restored panels need user activation"
    );
    assert!(app.interactive.focus_latest());
    app.interactive.window_focus(false);
    live::tick(&mut app);
    assert!(app.interactive.data_running.is_none());
    app.interactive.window_focus(true);
    app.interactive.unfocus();
    draw(&mut app.interactive, &panel, 100);
    live::tick(&mut app);
    assert!(
        app.interactive.data_running.is_none(),
        "off-screen queries pause"
    );
    draw(&mut app.interactive, &panel, 0);
    live::tick(&mut app);
    assert!(app.interactive.data_running.is_some());
    live::tick(&mut app);
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    live::tick(&mut app);
    assert_eq!(
        executor.0.load(Ordering::Relaxed),
        1,
        "only one call may run at once"
    );
    assert!(app.interactive.data_running.is_none());
    let cards: Vec<_> = app
        .messages
        .iter()
        .filter(|m| m.role == "tool_call")
        .collect();
    assert_eq!(cards.len(), 1);
    let content: serde_json::Value = serde_json::from_str(&cards[0].content).unwrap();
    assert_eq!(content["panel"], "p1-1");
    assert_eq!(content["name"], "files");
    assert!(content["result"].as_str().unwrap().contains("Loaded"));
    assert_eq!(
        app.db
            .load_messages(&row.id)
            .unwrap()
            .iter()
            .filter(|m| m.role == "tool_call")
            .count(),
        1
    );
    assert_eq!(
        snapshot(&app.interactive.entries[&panel.key()]),
        ["Loaded", "F2 keyboard"]
    );
    app.interactive.clear();
    assert_eq!(
        app.interactive.data_calls[&row.id], 1,
        "viewport resets cannot reset the session budget"
    );
    drop(app);
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}
