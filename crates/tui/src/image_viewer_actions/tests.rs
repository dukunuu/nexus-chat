//! The viewer's pointer gestures, clipboard, file actions and drops. Nothing
//! here touches the real clipboard, file manager or home folder: each seam is
//! replaced by a recording fake, and every file lives in a temp dir.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nexus_core::app::{App, Popup};
use nexus_core::db::Db;
use nexus_core::space::Space;

use super::*;
use crate::image_viewer::{Anchor, Return, ZOOM_STEPS, Zoom, anchored_pan, next_zoom_percent};
use crate::ui::popups::image_viewer as popup;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Clip {
    Text(String),
    Image {
        width: usize,
        height: usize,
        bytes: usize,
    },
}

#[derive(Clone, Default)]
struct Fake {
    log: Arc<Mutex<Vec<Clip>>>,
    fail: bool,
}

impl Clipboard for Fake {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        if self.fail {
            return Err("denied".into());
        }
        self.log.lock().unwrap().push(Clip::Text(text.into()));
        Ok(())
    }

    fn set_image(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> Result<(), String> {
        if self.fail {
            return Err("denied".into());
        }
        self.log.lock().unwrap().push(Clip::Image {
            width,
            height,
            bytes: rgba.len(),
        });
        Ok(())
    }
}

type Commands = Arc<Mutex<Vec<Vec<OsString>>>>;

struct Rig {
    app: AppView,
    clips: Arc<Mutex<Vec<Clip>>>,
    commands: Commands,
    downloads: PathBuf,
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nexus-viewer-actions-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn png_file(dir: &Path, name: &str, width: u32, height: u32) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x * 4 % 256) as u8, (y * 8 % 256) as u8, 120])
    })
    .save(&path)
    .unwrap();
    path.to_string_lossy().into_owned()
}

fn rig() -> Rig {
    let root = temp_dir();
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let mut app = AppView::new(App::new(
        Db::open_in_memory().unwrap(),
        Some("k"),
        Space { root },
    ));
    app.core.settings.image_viewer = "terminal".into();
    let fake = Fake::default();
    let clips = Arc::clone(&fake.log);
    app.image_viewer.hooks.clipboard = Some(Box::new(fake));
    let commands = Commands::default();
    let sink = Arc::clone(&commands);
    app.image_viewer.hooks.launcher = Launcher::new(move |argv| {
        sink.lock().unwrap().push(argv.to_vec());
        Ok(())
    });
    let downloads = temp_dir().join("Downloads");
    app.image_viewer.hooks.downloads = Some(downloads.clone());
    Rig {
        app,
        clips,
        commands,
        downloads,
    }
}

fn open(app: &mut AppView, target: &str) {
    app.open_image_target(target, &Anchor::ByTarget, Return::Nothing);
    assert!(app.popup == Popup::ImageViewer);
}

fn draw(app: &mut AppView, width: u16, height: u16) {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| popup::render(f, app)).unwrap();
}

fn status(app: &mut AppView) -> String {
    while let Some(event) = app.pop_pending_event() {
        app.apply_event(&event);
    }
    app.status.clone()
}

fn press(app: &mut AppView, code: KeyCode) {
    popup::handle_key(app, KeyEvent::from(code));
}

fn chord(app: &mut AppView, code: KeyCode, modifiers: KeyModifiers) {
    popup::handle_key(app, KeyEvent::new(code, modifiers));
}

fn pointer(app: &mut AppView, kind: MouseEventKind, column: u16, row: u16) {
    popup::mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn click(app: &mut AppView, column: u16, row: u16) {
    pointer(app, MouseEventKind::Down(MouseButton::Left), column, row);
    pointer(app, MouseEventKind::Up(MouseButton::Left), column, row);
}

/// Three images in the transcript, the middle one open, drawn once.
fn gallery(rig: &mut Rig, width: u16, height: u16) -> Vec<String> {
    let dir = temp_dir();
    let pics: Vec<String> = ["a.png", "b.png", "c.png"]
        .iter()
        .map(|name| png_file(&dir, name, 40, 20))
        .collect();
    rig.app.history_cache.image_at_line = vec![
        Some(pics[0].clone()),
        None,
        Some(pics[1].clone()),
        None,
        Some(pics[2].clone()),
    ];
    rig.app
        .open_image_target(&pics[1], &Anchor::Line(2), Return::Nothing);
    draw(&mut rig.app, width, height);
    pics
}

// ── small pure pieces ────────────────────────────────────────────────────

#[test]
fn reveal_argv_is_built_per_os_without_a_shell() {
    let path = Path::new("/home/me/pics/cat.png");
    assert_eq!(
        reveal_argv(Os::Mac, path),
        ["open", "-R", "/home/me/pics/cat.png"].map(OsString::from)
    );
    assert_eq!(
        reveal_argv(Os::Linux, path),
        ["xdg-open", "/home/me/pics"].map(OsString::from)
    );
    assert_eq!(
        reveal_argv(Os::Windows, Path::new(r"C:\Users\me\cat.png")),
        ["explorer", r"/select,C:\Users\me\cat.png"].map(OsString::from)
    );
    // A name with spaces and shell metacharacters stays one argument.
    let odd = Path::new("/tmp/a b; rm -rf ~/c.png");
    assert_eq!(reveal_argv(Os::Mac, odd).len(), 3);
    assert_eq!(reveal_argv(Os::Mac, odd)[2], OsString::from(odd));
}

#[test]
fn names_are_made_safe_for_markdown_and_filesystems() {
    assert_eq!(safe_name("My Photo (1).png"), "My_Photo__1_.png");
    assert_eq!(safe_name("..hidden"), "hidden");
    assert_eq!(safe_name("a/b\\c.png"), "a_b_c.png");
    assert_eq!(safe_name(""), "image");
    assert_eq!(
        safe_name("\u{441}\u{43d}\u{438}\u{43c}\u{43e}\u{43a}.png"),
        "\u{441}\u{43d}\u{438}\u{43c}\u{43e}\u{43a}.png"
    );
    assert!(safe_name(&"x".repeat(500)).len() <= 100);
}

#[test]
fn saving_never_overwrites_and_numbers_the_copies() {
    let src_dir = temp_dir();
    let source = png_file(&src_dir, "cat.png", 8, 8);
    let dir = temp_dir().join("nested").join("images");
    let first = save_unique(Path::new(&source), &dir, "cat.png").unwrap();
    let second = save_unique(Path::new(&source), &dir, "cat.png").unwrap();
    let third = save_unique(Path::new(&source), &dir, "cat.png").unwrap();
    let names: Vec<_> = [first, second, third]
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["cat.png", "cat-2.png", "cat-3.png"]);
    let original = std::fs::read(&source).unwrap();
    for name in names {
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), original);
    }
    // A name that already exists with different content is kept as it was.
    std::fs::write(dir.join("keep.png"), b"mine").unwrap();
    let kept = save_unique(Path::new(&source), &dir, "keep.png").unwrap();
    assert_eq!(kept.file_name().unwrap(), "keep-2.png");
    assert_eq!(std::fs::read(dir.join("keep.png")).unwrap(), b"mine");
}

#[test]
fn a_url_is_saved_under_its_last_segment_with_the_cached_extension() {
    let cached = Path::new("/cache/images/abcdef.png");
    assert_eq!(
        saved_name("https://img.example.com/a/b/Sunset.JPG?x=1", cached),
        "Sunset.png"
    );
    assert_eq!(
        saved_name("https://img.example.com/", cached),
        "img.example.com.png"
    );
    assert_eq!(saved_name("/home/me/my pic.png", cached), "my_pic.png");
}

#[test]
fn zoom_steps_clamp_between_25_and_800_percent() {
    assert_eq!(next_zoom_percent(37.0, 1), 50);
    assert_eq!(next_zoom_percent(37.0, -1), 25);
    assert_eq!(next_zoom_percent(100.0, 1), 150);
    assert_eq!(next_zoom_percent(100.0, -1), 75);
    assert_eq!(next_zoom_percent(800.0, 1), 800);
    assert_eq!(next_zoom_percent(25.0, -1), 25);
    assert_eq!(next_zoom_percent(99.6, 1), 150, "within a hair of a step");
    assert_eq!(ZOOM_STEPS.first(), Some(&25));
    assert_eq!(ZOOM_STEPS.last(), Some(&800));
}

#[test]
fn zooming_keeps_the_point_under_the_pointer_fixed() {
    // A 1000 px axis in a 400 px window, shown at 100% and centered.
    let (decoded, view) = (1000, 400);
    // Pointer 100 px into the window: image point 400. At 200% the window must
    // start at 700 (so 700 + 100 = 800 = 400 * 2): pan = 700 - (2000 - 400) / 2.
    let pan = anchored_pan(decoded, view, 0, 100.0, 1.0, 2.0);
    assert_eq!(pan, -100);
    // Zooming at the exact center keeps the center.
    assert_eq!(anchored_pan(decoded, view, 0, 200.0, 1.0, 2.0), 0);
    // A pointer at the window's edge over image point 300 puts that point at
    // the edge: the window starts at 2400, i.e. pan = 2400 - (8000 - 400) / 2.
    assert_eq!(anchored_pan(decoded, view, 0, 0.0, 1.0, 8.0), -1400);
    // Zooming on the very first pixel can't scroll past the image's edge: the
    // result is the extreme pan, half the overflow.
    assert_eq!(
        anchored_pan(decoded, view, -300, 0.0, 1.0, 8.0),
        -(8000 - 400) / 2
    );
    // And no pan ever exceeds that bound, wherever the pointer is.
    for pointer in [0.0, 150.0, 399.0] {
        for pan in [-300, 0, 300] {
            let result = anchored_pan(decoded, view, pan, pointer, 1.0, 4.0);
            assert!(result.abs() <= (4000 - 400) / 2, "{pointer} {pan} {result}");
        }
    }
    // Zooming back out below the window size recenters.
    assert_eq!(anchored_pan(decoded, view, 250, 100.0, 2.0, 0.25), 0);
    // A previous pan is part of the starting point: zoom in then out at the
    // same pointer returns to where it started.
    let into = anchored_pan(decoded, view, 60, 120.0, 1.0, 2.0);
    let back = anchored_pan(decoded, view, into, 120.0, 2.0, 1.0);
    assert!((back - 60).abs() <= 1, "{back}");
}

// ── gestures on a drawn viewer ───────────────────────────────────────────

fn big_viewer(rig: &mut Rig) -> String {
    let pic = png_file(&temp_dir(), "big.png", 400, 300);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 40, 12);
    pic
}

#[test]
fn the_wheel_steps_the_zoom_around_the_pointer_and_clamps() {
    let mut rig = rig();
    big_viewer(&mut rig);
    let app = &mut rig.app;
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
    let body = app.image_viewer.body();
    let (column, row) = (body.x + 4, body.y + 2);

    pointer(app, MouseEventKind::ScrollUp, column, row);
    let Zoom::Percent(first) = app.image_viewer.zoom() else {
        panic!("a wheel step leaves fit");
    };
    assert!(ZOOM_STEPS.contains(&first), "{first}");
    let mut seen = vec![first];
    for _ in 0..12 {
        pointer(app, MouseEventKind::ScrollUp, column, row);
        draw(app, 40, 12);
        if let Zoom::Percent(p) = app.image_viewer.zoom() {
            seen.push(p);
        }
    }
    assert_eq!(seen.last(), Some(&800), "{seen:?}");
    assert!(
        seen.windows(2).all(|pair| pair[0] <= pair[1]),
        "monotonic: {seen:?}"
    );
    for _ in 0..12 {
        pointer(app, MouseEventKind::ScrollDown, column, row);
    }
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(25));
    // Ctrl+wheel zooms too.
    popup::mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column,
            row,
            modifiers: KeyModifiers::CONTROL,
        },
    );
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(50));
}

#[test]
fn a_wheel_zoom_keeps_the_image_point_under_the_pointer() {
    let mut rig = rig();
    big_viewer(&mut rig);
    let app = &mut rig.app;
    press(app, KeyCode::Char('1'));
    draw(app, 40, 12);
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(100));
    let body = app.image_viewer.body();
    let (cell_w, cell_h) = app.image_viewer.cell_px();
    // Pointer in the window's upper left quarter.
    let (column, row) = (body.x + 6, body.y + 2);
    let window_x = |app: &AppView| {
        // Image x under the pointer at 100%: window origin + pointer offset.
        let g = app.image_viewer.percent().unwrap();
        assert!((g - 100.0).abs() < 0.01);
        400.0 / 2.0 - f64::from(body.width) * f64::from(cell_w) / 2.0
            + f64::from(app.image_viewer.pan().0)
            + (f64::from(column - body.x) + 0.5) * f64::from(cell_w)
    };
    let before = window_x(app);
    pointer(app, MouseEventKind::ScrollUp, column, row);
    draw(app, 40, 12);
    let Zoom::Percent(p) = app.image_viewer.zoom() else {
        panic!()
    };
    assert_eq!(p, 150);
    let view_w = f64::from(body.width) * f64::from(cell_w);
    let disp = 400.0 * 1.5;
    let origin = (disp - view_w) / 2.0 + f64::from(app.image_viewer.pan().0);
    let after = (origin + (f64::from(column - body.x) + 0.5) * f64::from(cell_w)) / 1.5;
    assert!(
        (after - before).abs() <= 2.0,
        "image x under the pointer moved from {before} to {after}"
    );
    let _ = cell_h;
}

#[test]
fn dragging_pans_within_the_image_bounds_and_is_not_a_click() {
    let mut rig = rig();
    big_viewer(&mut rig);
    let app = &mut rig.app;
    press(app, KeyCode::Char('1'));
    draw(app, 40, 12);
    assert_eq!(app.image_viewer.can_pan(), (true, true));
    let body = app.image_viewer.body();
    let (cell_w, cell_h) = app.image_viewer.cell_px();
    let (x, y) = (body.x + 10, body.y + 3);

    // Drag the picture 3 cells right and 1 down: the window moves left / up.
    pointer(app, MouseEventKind::Down(MouseButton::Left), x, y);
    pointer(app, MouseEventKind::Drag(MouseButton::Left), x + 3, y + 1);
    pointer(app, MouseEventKind::Up(MouseButton::Left), x + 3, y + 1);
    assert_eq!(
        app.image_viewer.pan(),
        (-3 * i32::from(cell_w as u16), -i32::from(cell_h as u16))
    );
    assert!(
        app.image_viewer.pointer.last_click.is_none(),
        "a drag is no click"
    );

    // A huge drag stops at the image's edge once the next frame clamps it.
    pointer(app, MouseEventKind::Down(MouseButton::Left), body.x, y);
    pointer(
        app,
        MouseEventKind::Drag(MouseButton::Left),
        body.right() - 1,
        y,
    );
    pointer(
        app,
        MouseEventKind::Up(MouseButton::Left),
        body.right() - 1,
        y,
    );
    draw(app, 40, 12);
    let view_w = i32::from(body.width) * cell_w;
    let half = (400 - view_w) / 2;
    assert_eq!(
        app.image_viewer.pan().0,
        -half,
        "left edge reached, not passed"
    );
    // And the other way.
    pointer(
        app,
        MouseEventKind::Down(MouseButton::Left),
        body.right() - 1,
        y,
    );
    pointer(app, MouseEventKind::Drag(MouseButton::Left), body.x, y);
    pointer(app, MouseEventKind::Up(MouseButton::Left), body.x, y);
    pointer(
        app,
        MouseEventKind::Down(MouseButton::Left),
        body.right() - 1,
        y,
    );
    pointer(app, MouseEventKind::Drag(MouseButton::Left), body.x, y);
    pointer(app, MouseEventKind::Up(MouseButton::Left), body.x, y);
    draw(app, 40, 12);
    assert_eq!(app.image_viewer.pan().0, half);
}

#[test]
fn dragging_a_picture_that_fits_changes_nothing() {
    let mut rig = rig();
    big_viewer(&mut rig);
    let app = &mut rig.app;
    assert_eq!(app.image_viewer.can_pan(), (false, false));
    let body = app.image_viewer.body();
    pointer(
        app,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 8,
        body.y + 3,
    );
    pointer(
        app,
        MouseEventKind::Drag(MouseButton::Left),
        body.x + 12,
        body.y + 4,
    );
    pointer(
        app,
        MouseEventKind::Up(MouseButton::Left),
        body.x + 12,
        body.y + 4,
    );
    assert_eq!(app.image_viewer.pan(), (0, 0));
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
}

#[test]
fn a_double_click_toggles_fit_and_100_percent() {
    let mut rig = rig();
    big_viewer(&mut rig);
    let app = &mut rig.app;
    let body = app.image_viewer.body();
    let (x, y) = (body.x + body.width / 2, body.y + body.height / 2);

    click(app, x, y);
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit, "one click does nothing");
    click(app, x, y);
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(100));
    draw(app, 40, 12);
    click(app, x, y);
    click(app, x, y);
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
    // Two clicks far apart are not a double-click.
    click(app, x, y);
    click(app, x + 5, y);
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
    // Nor are two clicks that are too slow.
    app.image_viewer.pointer.last_click = Some((
        std::time::Instant::now() - DOUBLE_CLICK - std::time::Duration::from_millis(50),
        (x, y),
    ));
    click(app, x, y);
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
}

#[test]
fn clicking_the_outer_thirds_goes_to_the_previous_and_next_image() {
    let mut rig = rig();
    let pics = gallery(&mut rig, 90, 20);
    let app = &mut rig.app;
    let body = app.image_viewer.body();
    let row = body.y + 2;
    let at = |app: &AppView| app.image_viewer.current().unwrap().target.clone();
    assert_eq!(at(app), pics[1]);

    // The middle third is the picture: nothing navigates.
    click(app, body.x + body.width / 2, row);
    assert_eq!(at(app), pics[1]);
    click(app, body.x + 2, row);
    assert_eq!(at(app), pics[0], "left edge -> previous");
    click(app, body.x + 2, row);
    assert_eq!(at(app), pics[0], "stops at the first");
    click(app, body.right() - 3, row);
    click(app, body.right() - 3, row);
    click(app, body.right() - 3, row);
    assert_eq!(at(app), pics[2], "right edge -> next, stops at the last");
    // Two edge clicks in a row are page turns, never a zoom.
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
}

#[test]
fn a_zoomed_picture_only_turns_pages_from_a_narrow_edge() {
    let mut rig = rig();
    let dir = temp_dir();
    let pics: Vec<String> = ["a.png", "b.png"]
        .iter()
        .map(|name| png_file(&dir, name, 400, 300))
        .collect();
    rig.app.history_cache.image_at_line = vec![Some(pics[0].clone()), None, Some(pics[1].clone())];
    rig.app
        .open_image_target(&pics[0], &Anchor::Line(0), Return::Nothing);
    draw(&mut rig.app, 40, 12);
    press(&mut rig.app, KeyCode::Char('1'));
    draw(&mut rig.app, 40, 12);
    let app = &mut rig.app;
    assert!(app.image_viewer.can_pan().0);
    let body = app.image_viewer.body();
    click(app, body.right() - 8, body.y + 2);
    assert_eq!(
        app.image_viewer.index(),
        0,
        "inside the picture: no page turn"
    );
    click(app, body.right() - 2, body.y + 2);
    assert_eq!(app.image_viewer.index(), 1, "the 3-column edge still turns");
}

#[test]
fn keys_zoom_in_out_fit_and_one_to_one() {
    let mut rig = rig();
    big_viewer(&mut rig);
    let app = &mut rig.app;
    press(app, KeyCode::Char('+'));
    let Zoom::Percent(_) = app.image_viewer.zoom() else {
        panic!("+ leaves fit");
    };
    press(app, KeyCode::Char('1'));
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(100));
    draw(app, 40, 12);
    press(app, KeyCode::Char('='));
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(150));
    press(app, KeyCode::Char('-'));
    press(app, KeyCode::Char('-'));
    assert_eq!(app.image_viewer.zoom(), Zoom::Percent(75));
    press(app, KeyCode::Char('0'));
    assert_eq!(app.image_viewer.zoom(), Zoom::Fit);
    assert_eq!(app.image_viewer.pan(), (0, 0));
}

// ── clipboard ────────────────────────────────────────────────────────────

#[test]
fn y_copies_the_decoded_pixels_and_c_copies_the_path() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 40, 20);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    press(&mut rig.app, KeyCode::Char('y'));
    assert_eq!(
        *rig.clips.lock().unwrap(),
        [Clip::Image {
            width: 40,
            height: 20,
            bytes: 40 * 20 * 4
        }]
    );
    assert_eq!(status(&mut rig.app), "copied image 40\u{d7}20");
    press(&mut rig.app, KeyCode::Char('c'));
    assert_eq!(rig.clips.lock().unwrap()[1], Clip::Text(pic));
    assert_eq!(status(&mut rig.app), "copied path");
    assert!(rig.app.popup == Popup::ImageViewer, "copying keeps it open");
}

#[test]
fn ctrl_c_and_cmd_c_copy_the_image_too() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 16, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    chord(&mut rig.app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    chord(&mut rig.app, KeyCode::Char('c'), KeyModifiers::SUPER);
    let clips = rig.clips.lock().unwrap();
    assert_eq!(clips.len(), 2);
    assert!(matches!(
        clips[0],
        Clip::Image {
            width: 16,
            height: 8,
            ..
        }
    ));
    assert_eq!(clips[0], clips[1]);
}

#[test]
fn a_downscaled_photo_is_copied_from_the_original_at_full_size() {
    let mut rig = rig();
    // 4000 px wide: the viewer keeps a 3072 px copy, the clipboard gets 4000.
    let pic = png_file(&temp_dir(), "wide.png", 4000, 200);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    assert!(rig.app.image_viewer.meta().unwrap().shown.is_some());
    press(&mut rig.app, KeyCode::Char('y'));
    assert_eq!(
        *rig.clips.lock().unwrap(),
        [Clip::Image {
            width: 4000,
            height: 200,
            bytes: 4000 * 200 * 4
        }]
    );
    assert_eq!(status(&mut rig.app), "copied image 4000\u{d7}200");
}

#[test]
fn beyond_32_megapixels_the_downscaled_copy_is_used_and_says_so() {
    use crate::image_viewer::{Decoded, Meta};
    let decoded = Decoded {
        path: PathBuf::from("/no/such/file.png"),
        image: Arc::new(image::DynamicImage::new_rgba8(30, 10)),
        meta: Meta {
            width: 9000,
            height: 6000,
            format: "PNG".into(),
            bytes: 1,
            shown: Some((30, 10)),
        },
    };
    let (width, height, rgba, real) = clipboard_pixels(&decoded);
    assert_eq!(
        (width, height, rgba.len(), real),
        (30, 10, 30 * 10 * 4, false)
    );
    // An SVG has no pixels of its own: its raster counts as the real thing.
    let mut svg = decoded.clone();
    svg.meta.format = "SVG".into();
    svg.meta.width = 40;
    svg.meta.height = 20;
    assert!(clipboard_pixels(&svg).3);
}

#[test]
fn clipboard_failures_are_a_short_status_not_an_error() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 8, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    rig.app.image_viewer.hooks.clipboard = Some(Box::new(Fake {
        fail: true,
        ..Fake::default()
    }));
    press(&mut rig.app, KeyCode::Char('y'));
    assert_eq!(status(&mut rig.app), "couldn't copy the image: denied");
    press(&mut rig.app, KeyCode::Char('c'));
    assert_eq!(status(&mut rig.app), "couldn't copy: denied");
    // A headless session has no clipboard at all.
    rig.app.image_viewer.hooks.clipboard = None;
    rig.app.clipboard = None;
    press(&mut rig.app, KeyCode::Char('y'));
    assert_eq!(
        status(&mut rig.app),
        "couldn't copy the image: no clipboard here"
    );
    assert!(rig.app.popup == Popup::ImageViewer);
}

#[test]
fn copying_before_the_image_has_loaded_says_so() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 8, 8);
    open(&mut rig.app, &pic);
    press(&mut rig.app, KeyCode::Char('y'));
    assert_eq!(status(&mut rig.app), "the image isn't loaded yet");
    assert!(rig.clips.lock().unwrap().is_empty());
}

// ── files ────────────────────────────────────────────────────────────────

#[test]
fn s_saves_a_unique_copy_into_the_space_files() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "cat.png", 8, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    press(&mut rig.app, KeyCode::Char('s'));
    assert_eq!(status(&mut rig.app), "saved to files/cat.png");
    press(&mut rig.app, KeyCode::Char('s'));
    assert_eq!(status(&mut rig.app), "saved to files/cat-2.png");
    let dir = rig
        .app
        .core
        .space
        .files_dir(&rig.app.core.active_space.name);
    assert_eq!(
        std::fs::read(dir.join("cat-2.png")).unwrap(),
        std::fs::read(&pic).unwrap()
    );
}

#[test]
fn shift_s_saves_to_downloads_with_a_unique_name() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "cat.png", 8, 8);
    open(&mut rig.app, &pic);
    chord(&mut rig.app, KeyCode::Char('S'), KeyModifiers::SHIFT);
    let note = status(&mut rig.app);
    assert!(
        note.starts_with("saved to ") && note.ends_with("cat.png"),
        "{note}"
    );
    chord(&mut rig.app, KeyCode::Char('S'), KeyModifiers::SHIFT);
    assert!(status(&mut rig.app).ends_with("cat-2.png"));
    assert!(rig.downloads.join("cat.png").is_file());
    assert!(rig.downloads.join("cat-2.png").is_file());
}

#[test]
fn a_remote_image_is_saved_and_revealed_from_the_cache() {
    let mut rig = rig();
    let fetcher = nexus_core::remote_image::Fetcher::new(temp_dir());
    crate::app_view::use_fetcher_for_tests(&fetcher);
    let url = "https://img.example.invalid/albums/sunset.jpg";
    let mut png = Vec::new();
    image::RgbImage::from_pixel(12, 6, image::Rgb([9, 8, 7]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    fetcher.seed(url, &png).unwrap();
    open(&mut rig.app, url);
    draw(&mut rig.app, 80, 24);

    press(&mut rig.app, KeyCode::Char('s'));
    assert_eq!(status(&mut rig.app), "saved to files/sunset.png");
    let saved = rig
        .app
        .core
        .space
        .files_dir(&rig.app.core.active_space.name)
        .join("sunset.png");
    assert_eq!(std::fs::read(saved).unwrap(), png);

    press(&mut rig.app, KeyCode::Char('c'));
    assert_eq!(
        rig.clips.lock().unwrap().last(),
        Some(&Clip::Text(url.into()))
    );
    assert_eq!(status(&mut rig.app), "copied URL");

    press(&mut rig.app, KeyCode::Char('r'));
    let commands = rig.commands.lock().unwrap();
    let revealed = Path::new(&commands[0].last().unwrap().clone()).to_path_buf();
    drop(commands);
    let cached = match source_of(url) {
        Source::Ready(path) => path,
        other => panic!("{other:?}"),
    };
    let expected = reveal_argv(Os::current(), &std::path::absolute(&cached).unwrap());
    assert_eq!(rig.commands.lock().unwrap()[0], expected);
    let _ = revealed;
}

#[test]
fn r_reveals_the_file_with_the_platform_argv() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "cat.png", 8, 8);
    open(&mut rig.app, &pic);
    press(&mut rig.app, KeyCode::Char('r'));
    assert_eq!(
        *rig.commands.lock().unwrap(),
        [reveal_argv(Os::current(), Path::new(&pic))]
    );
    assert_eq!(status(&mut rig.app), "revealed cat.png");
}

#[test]
fn saving_a_missing_or_unloaded_image_is_a_status() {
    let mut rig = rig();
    open(&mut rig.app, "/no/such/dir/ghost.png");
    press(&mut rig.app, KeyCode::Char('s'));
    assert_eq!(status(&mut rig.app), "there is no file for this image");
    press(&mut rig.app, KeyCode::Char('r'));
    assert!(rig.commands.lock().unwrap().is_empty());
    // Remote images switched off.
    open_remote_off(&mut rig);
    press(&mut rig.app, KeyCode::Char('s'));
    assert!(status(&mut rig.app).starts_with("remote images are off"));
}

fn open_remote_off(rig: &mut Rig) {
    rig.app.close_image_viewer();
    open(&mut rig.app, "https://img.example.invalid/none.png");
}

// ── attach ───────────────────────────────────────────────────────────────

#[test]
fn a_attaches_a_copy_to_the_draft_and_closes_the_viewer() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "cat photo.png", 8, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    press(&mut rig.app, KeyCode::Char('a'));
    assert!(rig.app.popup == Popup::None, "the viewer closes");
    assert!(rig.app.image_viewer.items().is_empty());
    assert_eq!(rig.app.input_text(), "![cat_photo](cat_photo.png)");
    let saved = rig
        .app
        .core
        .space
        .files_dir(&rig.app.core.active_space.name)
        .join("cat_photo.png");
    assert_eq!(std::fs::read(saved).unwrap(), std::fs::read(&pic).unwrap());
    assert_eq!(status(&mut rig.app), "attached cat_photo.png");
}

#[test]
fn attaching_a_space_file_references_it_in_place_after_the_draft_text() {
    let mut rig = rig();
    let files = rig
        .app
        .core
        .space
        .files_dir(&rig.app.core.active_space.name);
    let pic = png_file(&files, "mine.png", 8, 8);
    rig.app.set_input("what is this?");
    rig.app
        .open_image_target(&pic, &Anchor::ByTarget, Return::Files);
    press(&mut rig.app, KeyCode::Char('a'));
    assert_eq!(rig.app.input_text(), "what is this? ![mine](mine.png)");
    assert!(
        rig.app.popup == Popup::None,
        "not back in the files popup: the draft is what matters"
    );
    assert!(
        !files.join("mine-2.png").exists(),
        "no needless copy of a file the space already has"
    );
}

// ── dropping a file on the viewer ────────────────────────────────────────

#[test]
fn dropping_an_image_path_opens_it_instead_of_reaching_the_composer() {
    let mut rig = rig();
    let dir = temp_dir();
    let first = png_file(&dir, "first.png", 8, 8);
    let dropped = png_file(&dir, "dropped one.png", 24, 12);
    open(&mut rig.app, &first);
    rig.app.paste(&format!("'{dropped}'"));
    assert_eq!(rig.app.image_viewer.current().unwrap().target, dropped);
    assert_eq!(rig.app.image_viewer.items().len(), 2);
    assert!(rig.app.popup == Popup::ImageViewer);
    assert_eq!(rig.app.input_text(), "", "the composer saw nothing");
    assert_eq!(status(&mut rig.app), "opened dropped one.png");
    // Terminals that escape spaces instead of quoting work too.
    let escaped = dropped.replace(' ', "\\ ");
    rig.app.paste(&format!("file://{escaped}"));
    assert_eq!(rig.app.image_viewer.items().len(), 3);
    // Esc then goes back, and the first image is still reachable.
    press(&mut rig.app, KeyCode::Home);
    assert_eq!(rig.app.image_viewer.current().unwrap().target, first);
}

#[test]
fn dropping_something_that_is_not_an_image_is_ignored_with_a_status() {
    let mut rig = rig();
    let dir = temp_dir();
    let first = png_file(&dir, "first.png", 8, 8);
    let notes = dir.join("notes.txt");
    std::fs::write(&notes, "hello").unwrap();
    open(&mut rig.app, &first);
    rig.app.paste(&notes.to_string_lossy());
    assert_eq!(rig.app.image_viewer.current().unwrap().target, first);
    assert_eq!(rig.app.image_viewer.items().len(), 1);
    assert_eq!(
        status(&mut rig.app),
        "notes.txt: not an image \u{2014} ignored"
    );
    assert_eq!(rig.app.input_text(), "");
    // Plain text pasted onto the viewer is ignored without a word.
    rig.app.status.clear();
    rig.app.paste("just some words");
    assert_eq!(status(&mut rig.app), "");
    assert_eq!(rig.app.input_text(), "");
}

// ── the action menu ──────────────────────────────────────────────────────

#[test]
fn the_dot_key_opens_a_menu_that_runs_the_chosen_action() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 8, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    press(&mut rig.app, KeyCode::Char('.'));
    assert!(rig.app.image_viewer.pointer.menu.is_some());
    press(&mut rig.app, KeyCode::Down);
    press(&mut rig.app, KeyCode::Enter);
    assert!(rig.app.image_viewer.pointer.menu.is_none());
    assert_eq!(
        rig.clips.lock().unwrap()[0],
        Clip::Text(pic.clone()),
        "item 2 is Copy path"
    );

    // Esc closes only the menu; the next Esc closes the viewer.
    press(&mut rig.app, KeyCode::Char('.'));
    press(&mut rig.app, KeyCode::Esc);
    assert!(rig.app.image_viewer.pointer.menu.is_none());
    assert!(rig.app.popup == Popup::ImageViewer);
    // The hotkeys shown in the menu work from it.
    press(&mut rig.app, KeyCode::Char('.'));
    press(&mut rig.app, KeyCode::Char('y'));
    assert!(rig.app.image_viewer.pointer.menu.is_none());
    assert!(matches!(rig.clips.lock().unwrap()[1], Clip::Image { .. }));
    // Selection stops at the ends.
    press(&mut rig.app, KeyCode::Char('.'));
    for _ in 0..20 {
        press(&mut rig.app, KeyCode::Char('j'));
    }
    assert_eq!(
        rig.app.image_viewer.pointer.menu.unwrap().selected,
        Action::ALL.len() - 1
    );
}

#[test]
fn right_click_opens_the_menu_at_the_pointer_and_a_click_picks_a_row() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 8, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    pointer(
        &mut rig.app,
        MouseEventKind::Down(MouseButton::Right),
        10,
        5,
    );
    let menu = rig.app.image_viewer.pointer.menu.expect("menu opened");
    assert_eq!(menu.at, (10, 5));
    draw(&mut rig.app, 80, 24);
    let area = menu_rect(rig.app.image_viewer.pointer.screen, menu.at);
    assert_eq!((area.x, area.y), (10, 5));
    // First list row sits under the top border; the second is Copy path.
    let row = area.y + 2;
    let column = area.x + 4;
    pointer(
        &mut rig.app,
        MouseEventKind::Down(MouseButton::Left),
        column,
        row,
    );
    assert!(rig.app.image_viewer.pointer.menu.is_none());
    assert_eq!(rig.clips.lock().unwrap()[0], Clip::Text(pic));
    // A click outside closes it without running anything.
    pointer(
        &mut rig.app,
        MouseEventKind::Down(MouseButton::Right),
        10,
        5,
    );
    pointer(
        &mut rig.app,
        MouseEventKind::Down(MouseButton::Left),
        70,
        20,
    );
    assert!(rig.app.image_viewer.pointer.menu.is_none());
    assert_eq!(rig.clips.lock().unwrap().len(), 1);
}

#[test]
fn the_menu_is_pushed_back_on_screen_near_the_corner() {
    let screen = Rect::new(0, 0, 80, 24);
    let area = menu_rect(screen, (79, 23));
    assert_eq!(area.right(), 80);
    assert_eq!(area.bottom(), 24);
    let tiny = menu_rect(Rect::new(0, 0, 20, 5), (3, 3));
    assert!(tiny.right() <= 20 && tiny.bottom() <= 5);
}

#[test]
fn ctrl_c_in_the_viewer_never_quits_or_closes() {
    let mut rig = rig();
    let pic = png_file(&temp_dir(), "p.png", 8, 8);
    open(&mut rig.app, &pic);
    draw(&mut rig.app, 80, 24);
    // The event loop's Ctrl+C handler routes here.
    rig.app.viewer_action(Action::CopyImage);
    assert!(rig.app.popup == Popup::ImageViewer && !rig.app.should_quit);
    assert_eq!(rig.clips.lock().unwrap().len(), 1);
}
