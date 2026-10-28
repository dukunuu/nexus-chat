//! The `/library` popup: a snapshot of the list and of each mode, and every key.
//! Dialogs are scripted and the studio window is `cat`, so nothing opens.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::{App, Popup};
use nexus_core::canvas::dialogs::Scripted;
use nexus_core::db::Db;
use nexus_core::deck::Deck;
use nexus_core::doc::Doc;
use nexus_core::space::Space;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app_view::AppView;

struct Fixture {
    app: AppView,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let db = Db::open_in_memory().unwrap();
        let root =
            std::env::temp_dir().join(format!("nexus-library-popup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let root = root.canonicalize().unwrap();
        let app = AppView::new(App::new(db, Some("k"), Space { root: root.clone() }));
        Self { app, root }
    }

    fn files(&self) -> PathBuf {
        self.app.space.files_dir(&self.app.active_space.name)
    }

    fn put(&self, relative: &str, bytes: &[u8], age_seconds: u64) -> PathBuf {
        let path = self.files().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(age_seconds))
            .unwrap();
        path
    }

    fn deck(&self, relative: &str, title: &str, age_seconds: u64) -> PathBuf {
        self.put(
            relative,
            &serde_json::to_vec_pretty(&Deck::titled(title)).unwrap(),
            age_seconds,
        )
    }

    fn doc(&self, relative: &str, title: &str, age_seconds: u64) -> PathBuf {
        let mut doc = Doc::from_design(title, &nexus_core::design::Design::default());
        doc.blocks =
            nexus_core::doc::markdown::parse_document("# T\n\nBody.\n\nMore.\n", true).blocks;
        let doc = Doc::check(&serde_json::to_string(&doc).unwrap()).unwrap();
        self.put(
            relative,
            &serde_json::to_vec_pretty(&doc).unwrap(),
            age_seconds,
        )
    }

    /// Three files, oldest to newest: a deck, a doc and a deck.
    fn library(&mut self) {
        self.deck("decks/roadmap.deck.json", "Roadmap 2027", 5 * 86_400 + 600);
        self.doc("docs/memo.doc.json", "Q3 memo", 26 * 3600);
        self.deck("decks/launch.deck.json", "Launch review", 3 * 3600 + 300);
    }

    fn open(&mut self) {
        self.app.run_command("library").unwrap();
    }

    fn key(&mut self, code: KeyCode) {
        crate::ui::popups::library::handle_key(&mut self.app, KeyEvent::from(code));
    }

    fn press(&mut self, c: char) {
        self.key(KeyCode::Char(c));
    }

    fn screen(&self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::popups::library::render(frame, &self.app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// The newest status line: what the popup said through the event seam.
    fn status(&mut self) -> String {
        while let Some(event) = self.app.pop_pending_event() {
            self.app.apply_event(&event);
        }
        self.app.status.clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The framed body of a popup: each line between the borders, trimmed, with
/// empty ones dropped.
fn body(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter_map(|line| {
            let (_, text) = line.split_once('│')?;
            let (text, _) = text.rsplit_once('│')?;
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .collect()
}

#[test]
fn library_is_a_command_with_a_catalog_entry() {
    let mut f = Fixture::new();
    assert!(
        nexus_core::app::COMMANDS
            .iter()
            .any(|c| c.name == "library" && !c.desc.is_empty())
    );
    assert_eq!(
        f.app.core.parse_command("library").unwrap(),
        nexus_core::app::AppCommand::OpenLibrary
    );
    assert_eq!(
        f.app.core.parse_command("decks").unwrap(),
        nexus_core::app::AppCommand::OpenLibrary
    );
    f.app.run_command("library").unwrap();
    assert_eq!(f.app.popup, Popup::Library);
    // Headless consumers (CLI, host) never open it, and nothing happens.
    f.app
        .core
        .execute(nexus_core::app::AppCommand::OpenLibrary)
        .unwrap();
}

#[test]
fn the_popup_lists_kind_title_count_and_time_newest_first() {
    let mut f = Fixture::new();
    f.library();
    f.open();
    let screen = f.screen(100, 22);
    println!("{screen}");
    assert!(screen.contains(" library "), "{screen}");
    assert_eq!(
        body(&screen),
        [
            "▸ deck  Launch review",
            "1 slide · 3 hours ago",
            "doc   Q3 memo",
            "3 blocks · yesterday · nexus",
            "deck  Roadmap 2027",
            "1 slide · 5 days ago",
        ]
    );
    assert!(
        screen.contains("3 files · ↑↓ · enter open in studio · e export · d delete"),
        "{screen}"
    );
}

#[test]
fn an_empty_library_says_so() {
    let mut f = Fixture::new();
    f.open();
    let screen = f.screen(100, 12);
    assert!(
        screen.contains("no decks or docs yet — ask the model to draft one"),
        "{screen}"
    );
    assert!(screen.contains("esc close"), "{screen}");
    // The keys that act on a file do nothing without one.
    f.press('e');
    f.press('d');
    f.key(KeyCode::Enter);
    assert_eq!(f.app.library.mode, crate::ui::popups::library::Mode::Browse);
}

#[test]
fn the_cursor_moves_and_clamps() {
    let mut f = Fixture::new();
    f.library();
    f.open();
    f.key(KeyCode::Up);
    assert_eq!(f.app.library.selected, 0);
    f.key(KeyCode::Down);
    f.press('j');
    assert_eq!(f.app.library.selected, 2);
    f.key(KeyCode::Down);
    assert_eq!(f.app.library.selected, 2, "the end clamps");
    f.press('k');
    assert_eq!(f.app.library.selected, 1);
    f.key(KeyCode::Home);
    assert_eq!(f.app.library.selected, 0);
    f.key(KeyCode::End);
    assert_eq!(f.app.library.selected, 2);
    f.key(KeyCode::PageUp);
    assert_eq!(f.app.library.selected, 0);
    f.key(KeyCode::PageDown);
    assert_eq!(f.app.library.selected, 2);
    let screen = f.screen(100, 22);
    assert!(screen.contains("▸ deck  Roadmap 2027"), "{screen}");
}

#[test]
fn escape_closes_and_the_list_is_read_again_on_reopen() {
    let mut f = Fixture::new();
    f.library();
    f.open();
    f.key(KeyCode::Esc);
    assert_eq!(f.app.popup, Popup::None);
    f.deck("decks/new.deck.json", "Brand new", 1);
    f.open();
    assert_eq!(f.app.library.entries.len(), 4);
    assert_eq!(f.app.library.entries[0].title, "Brand new");
}

#[test]
fn enter_opens_the_file_in_studio() {
    let mut f = Fixture::new();
    f.library();
    f.app.attach_fake_studio();
    f.open();
    f.key(KeyCode::Down);
    f.key(KeyCode::Enter);
    assert_eq!(f.app.popup, Popup::None);
    assert!(
        f.app
            .studio_last_file()
            .is_some_and(|path| path.ends_with("docs/memo.doc.json"))
    );
    assert_eq!(f.status(), "studio focused");
}

#[test]
fn e_asks_for_a_format_then_exports_through_the_save_dialog() {
    let mut f = Fixture::new();
    f.library();
    let out = f.root.join("Desktop");
    std::fs::create_dir_all(&out).unwrap();
    let dialogs = Scripted::new([Some(out.join("review.pptx")), Some(out.join("memo"))]);
    f.app.dialogs = Box::new(dialogs.clone());
    f.open();
    f.press('e');
    let screen = f.screen(100, 22);
    assert!(screen.contains("export \"Launch review\" as"), "{screen}");
    assert!(
        screen.contains("p pptx · f pdf · g png zip · h html · j json · esc back"),
        "{screen}"
    );
    // A key that is no format does nothing; Escape goes back.
    f.press('z');
    assert_eq!(
        f.app.library.mode,
        crate::ui::popups::library::Mode::ExportAs
    );
    f.key(KeyCode::Esc);
    assert_eq!(f.app.library.mode, crate::ui::popups::library::Mode::Browse);
    assert_eq!(f.app.popup, Popup::Library, "Escape went back, not out");

    f.press('e');
    f.press('p');
    assert_eq!(&std::fs::read(out.join("review.pptx")).unwrap()[..2], b"PK");
    assert!(f.status().starts_with("exported to "), "{}", f.status());
    let asked = dialogs.requests();
    assert!(asked[0].save);
    assert_eq!(asked[0].name, "Launch review.pptx");
    assert_eq!(asked[0].filters, ["PowerPoint"]);
    assert!(asked[0].start.is_none());

    // A doc offers its own formats; the Save dialog now starts where the last one ended.
    f.key(KeyCode::Down);
    f.press('e');
    let screen = f.screen(100, 22);
    assert!(
        screen.contains("w docx · f pdf · m md · h html · esc back"),
        "{screen}"
    );
    f.press('w');
    assert_eq!(&std::fs::read(out.join("memo.docx")).unwrap()[..2], b"PK");
    assert_eq!(dialogs.requests()[1].name, "Q3 memo.docx");
    assert_eq!(dialogs.requests()[1].start.as_deref(), Some(out.as_path()));
    // The popup stays open on the same row.
    assert_eq!(f.app.popup, Popup::Library);
    assert_eq!(f.app.library.selected, 1);
}

#[test]
fn a_cancelled_dialog_exports_nothing() {
    let mut f = Fixture::new();
    f.library();
    f.app.dialogs = Box::new(Scripted::new([None]));
    f.open();
    f.press('e');
    f.press('p');
    assert_eq!(f.status(), "export cancelled");
    assert!(!f.files().join("exports").exists());
}

#[test]
fn without_a_desktop_session_the_export_goes_to_the_exports_folder() {
    let mut f = Fixture::new();
    f.library();
    let dialogs = Scripted::unavailable();
    f.app.dialogs = Box::new(dialogs.clone());
    f.open();
    f.press('e');
    f.press('f');
    assert!(dialogs.requests().is_empty(), "no dialog was asked for");
    let exported = f.files().join("exports/launch-review.pdf");
    assert_eq!(&std::fs::read(exported).unwrap()[..4], b"%PDF");
    let status = f.status();
    assert!(
        status.starts_with("no desktop session for a Save dialog — exported to "),
        "{status}"
    );
}

#[test]
fn d_confirms_before_moving_the_file_to_the_trash() {
    let mut f = Fixture::new();
    f.library();
    f.open();
    f.key(KeyCode::Down);
    f.press('d');
    let screen = f.screen(100, 22);
    assert!(
        screen.contains("move \"Q3 memo\" to the trash folder?"),
        "{screen}"
    );
    assert!(screen.contains("y confirm · esc cancel"), "{screen}");
    // Anything but a yes backs out and nothing is deleted.
    f.press('x');
    assert_eq!(
        f.app.library.mode,
        crate::ui::popups::library::Mode::ConfirmDelete
    );
    f.key(KeyCode::Esc);
    assert_eq!(f.app.library.mode, crate::ui::popups::library::Mode::Browse);
    assert!(f.files().join("docs/memo.doc.json").is_file());
    f.press('d');
    f.press('n');
    assert!(f.files().join("docs/memo.doc.json").is_file());
    f.press('d');
    f.press('y');
    assert!(!f.files().join("docs/memo.doc.json").exists());
    assert!(
        f.status()
            .starts_with("moved \"Q3 memo\" to the trash folder"),
        "{}",
        f.status()
    );
    assert_eq!(f.app.library.entries.len(), 2);
    assert_eq!(f.app.library.selected, 1);
    assert!(std::fs::read_dir(f.files().join(".trash")).unwrap().count() == 1);
    // Ctrl+D confirms too, like the other popups.
    f.press('d');
    crate::ui::popups::library::handle_key(
        &mut f.app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );
    assert_eq!(f.app.library.entries.len(), 1);
}

#[test]
fn studio_without_a_recent_file_opens_the_library_instead() {
    let mut f = Fixture::new();
    f.library();
    f.app.run_command("studio").unwrap();
    let status = f.status();
    assert_eq!(f.app.popup, Popup::Library, "{status}");
    assert_eq!(f.app.library.entries.len(), 3);
    // With a file named, studio opens it (here the fake window takes the message).
    f.app.popup = Popup::None;
    f.app.attach_fake_studio();
    f.app.run_command("studio decks/launch.deck.json").unwrap();
    assert_eq!(f.app.popup, Popup::None);
    assert!(
        f.app
            .studio_last_file()
            .unwrap()
            .ends_with("decks/launch.deck.json")
    );
    // And now there is a recent file, so a bare /studio focuses it.
    f.app.run_command("studio").unwrap();
    assert_eq!(f.app.popup, Popup::None);
    assert_eq!(f.status(), "studio focused");
}

#[test]
fn incognito_has_no_library() {
    let mut f = Fixture::new();
    f.library();
    f.app.core.incognito = true;
    f.app.run_command("library").unwrap();
    assert_eq!(f.app.popup, Popup::None);
    assert_eq!(f.status(), "library isn't available in incognito mode");
}

#[test]
fn the_list_records_where_each_row_is_drawn_for_mouse_clicks() {
    // Clicks reach the popup as keys (`handle_popup_mouse`) through the list's
    // hit area: rendering records three lines per entry.
    let mut f = Fixture::new();
    f.library();
    f.open();
    let screen = f.screen(100, 22);
    assert!(!screen.is_empty());
    let hit = f
        .app
        .list_hit
        .get()
        .expect("the list records where it drew");
    assert_eq!(hit.item_lines, 3);
    assert_eq!(hit.total, 3);
    assert_eq!(hit.selected, Some(0));
}
