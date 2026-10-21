//! Mouse resizing: drag the conversation column's edge to widen or narrow it,
//! drag a popup's border to resize it. Both resize symmetrically around the
//! screen center (the column and popups stay centered), update live as the
//! pointer moves, and persist on release through `set_setting` — a double
//! click on the same edge resets to the default.

// Casts here are on terminal-bounded values (u16 cell counts and percentages)
// — never on unbounded user data.
#![allow(clippy::cast_possible_truncation)]
use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::app_view::AppView;
use crate::ui::popups::chrome::{MIN_POPUP_H, MIN_POPUP_W};
use nexus_core::app::{
    COLUMN_WIDTH_MAX, COLUMN_WIDTH_MIN, POPUP_SIZE_MAX, POPUP_SIZE_MIN, Popup, popup_size,
    with_popup_size,
};

/// Two presses on the same edge within this window reset it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What a press grabbed: the reading column, or one popup (by its id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    Column,
    Popup(&'static str),
}

/// A resize in progress.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub grip: Grip,
    /// Which axes follow the pointer (a popup's vertical edges change its
    /// width, horizontal edges its height, corners both).
    wide: bool,
    tall: bool,
    /// The popup's size in percent of the screen while dragging.
    size: (u16, u16),
    /// Whether the pointer has moved since the press (a bare click persists
    /// nothing).
    moved: bool,
}

/// Route a mouse event to a resize. Returns whether it was consumed; presses
/// that miss every edge fall through to the usual handlers.
pub fn mouse(app: &mut AppView, m: MouseEvent, screen: Rect) -> bool {
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => press(app, m, screen),
        MouseEventKind::Drag(MouseButton::Left) if app.drag.is_some() => {
            drag(app, m, screen);
            true
        }
        MouseEventKind::Up(MouseButton::Left) if app.drag.is_some() => {
            release(app);
            true
        }
        _ => false,
    }
}

/// Whether column `x` is a gutter cell within reach of the column's edge.
fn on_column_edge(column: Rect, screen: Rect, x: u16) -> bool {
    let left = x < column.x && x + 2 >= column.x;
    let right = x >= column.right() && x <= column.right() + 1 && x < screen.right();
    left || right
}

fn press(app: &mut AppView, m: MouseEvent, screen: Rect) -> bool {
    let (grip, wide, tall, default) = if app.popup == Popup::None {
        let column = crate::ui::reading_column(screen, &app.settings.column_width);
        if column.width >= screen.width || !on_column_edge(column, screen, m.column) {
            return false;
        }
        (Grip::Column, true, false, (0, 0))
    } else {
        let Some(hit) = app.popup_hit.get() else {
            return false;
        };
        let rect = hit.rect;
        if !rect.contains(Position::new(m.column, m.row)) {
            return false;
        }
        // Only the border cells grab; everything inside keeps its own clicks.
        let wide = m.column == rect.x || m.column + 1 == rect.right();
        let tall = m.row == rect.y || m.row + 1 == rect.bottom();
        if !wide && !tall {
            return false;
        }
        (Grip::Popup(hit.id), wide, tall, hit.default)
    };
    let double = matches!(
        app.edge_click,
        Some((at, last)) if last == grip && at.elapsed() <= DOUBLE_CLICK
    );
    if double {
        app.edge_click = None;
        app.drag = None;
        reset(app, grip);
        return true;
    }
    app.edge_click = Some((Instant::now(), grip));
    let size = match grip {
        Grip::Popup(id) => popup_size(&app.settings.popup_sizes, id).unwrap_or(default),
        Grip::Column => (0, 0),
    };
    app.drag = Some(Drag {
        grip,
        wide,
        tall,
        size,
        moved: false,
    });
    true
}

/// Put the grabbed column or popup back to its default size.
fn reset(app: &mut AppView, grip: Grip) {
    let result = match grip {
        Grip::Column => app.core.set_setting("column_width", "auto"),
        Grip::Popup(id) => {
            let sizes = with_popup_size(&app.settings.popup_sizes, id, None);
            app.core.set_setting("popup_sizes", &sizes)
        }
    };
    match result {
        Ok(()) => app.push_status(match grip {
            Grip::Column => "column auto".to_string(),
            Grip::Popup(_) => "popup size reset".to_string(),
        }),
        Err(error) => app.push_status(format!("error: {error:#}")),
    }
}

/// `cells` of `total` as a whole percent, rounded.
fn percent(cells: u16, total: u16) -> u16 {
    let total = u32::from(total.max(1));
    ((u32::from(cells) * 100 + total / 2) / total) as u16
}

fn drag(app: &mut AppView, m: MouseEvent, screen: Rect) {
    let Some(mut state) = app.drag else {
        return;
    };
    // Symmetric around the screen center: the edge follows the pointer.
    let span = |pointer: u16, origin: u16, total: u16| 2 * pointer.abs_diff(origin + total / 2);
    match state.grip {
        Grip::Column => {
            let room = screen.width.saturating_sub(2);
            let low = COLUMN_WIDTH_MIN.min(room);
            let high = room.min(COLUMN_WIDTH_MAX);
            let width = span(m.column, screen.x, screen.width).clamp(low, high);
            app.settings.column_width = width.to_string();
            app.push_status(format!("column {width}"));
        }
        Grip::Popup(id) => {
            let pct = |cells: u16, min: u16, total: u16| {
                percent(cells.clamp(min.min(total), total), total)
                    .clamp(POPUP_SIZE_MIN, POPUP_SIZE_MAX)
            };
            if state.wide {
                let cells = span(m.column, screen.x, screen.width);
                state.size.0 = pct(cells, MIN_POPUP_W, screen.width);
            }
            if state.tall {
                let cells = span(m.row, screen.y, screen.height);
                state.size.1 = pct(cells, MIN_POPUP_H, screen.height);
            }
            app.settings.popup_sizes =
                with_popup_size(&app.settings.popup_sizes, id, Some(state.size));
        }
    }
    state.moved = true;
    app.drag = Some(state);
}

fn release(app: &mut AppView) {
    let Some(state) = app.drag.take() else {
        return;
    };
    if !state.moved {
        return;
    }
    let (key, value) = match state.grip {
        Grip::Column => ("column_width", app.settings.column_width.clone()),
        Grip::Popup(_) => ("popup_sizes", app.settings.popup_sizes.clone()),
    };
    match app.core.set_setting(key, &value) {
        Ok(()) if state.grip == Grip::Column => app.push_status(format!("column {value}")),
        Ok(()) => {}
        Err(error) => app.push_status(format!("error: {error:#}")),
    }
}

/// While the column is being dragged, a handle on each edge shows what the
/// pointer has hold of.
pub fn paint_column_handles(f: &mut Frame, app: &AppView, column: Rect) {
    if !matches!(
        app.drag,
        Some(Drag {
            grip: Grip::Column,
            ..
        })
    ) {
        return;
    }
    let screen = f.area();
    let edges = [column.x.checked_sub(1), Some(column.right())];
    for x in edges.into_iter().flatten().filter(|x| *x < screen.right()) {
        for y in screen.y..screen.bottom() {
            f.buffer_mut()[(x, y)]
                .set_symbol("│")
                .set_style(Style::default().fg(app.theme.border));
        }
    }
}

/// While a popup is being resized, its border takes the accent color and the
/// bottom-right hint becomes the live `W×H` (in cells).
pub fn paint_popup_resize(f: &mut Frame, app: &AppView) {
    let Some(Drag {
        grip: Grip::Popup(_),
        ..
    }) = app.drag
    else {
        return;
    };
    let Some(hit) = app.popup_hit.get() else {
        return;
    };
    let rect = hit.rect;
    if rect.width < 4 || rect.height < 2 {
        return;
    }
    let accent = Style::default().fg(app.theme.accent);
    let buf = f.buffer_mut();
    let bottom = rect.bottom() - 1;
    // The hint lives on the bottom border: swap it for plain border, then the size.
    for x in rect.x + 1..rect.right() - 1 {
        buf[(x, bottom)].set_symbol("─");
    }
    let label = format!(" {}×{} ", rect.width, rect.height);
    let start = (rect.right() - 1).saturating_sub(1 + label.chars().count() as u16);
    for (i, ch) in label.chars().enumerate() {
        let x = start + i as u16;
        if x > rect.x {
            buf[(x, bottom)].set_char(ch);
        }
    }
    let is_border = |symbol: &str| matches!(symbol, "─" | "│" | "╭" | "╮" | "╰" | "╯");
    let mut edge = |x: u16, y: u16| {
        let cell = &mut buf[(x, y)];
        if is_border(cell.symbol()) {
            cell.set_style(accent);
        }
    };
    for x in rect.x..rect.right() {
        edge(x, rect.y);
        edge(x, bottom);
    }
    for y in rect.y..rect.bottom() {
        edge(rect.x, y);
        edge(rect.right() - 1, y);
    }
    // The size label is not a border glyph; color it with the border.
    for x in start..start + label.chars().count() as u16 {
        if x > rect.x {
            buf[(x, bottom)].set_style(accent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use nexus_core::app::App;
    use nexus_core::db::Db;
    use nexus_core::space::Space;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn test_app() -> AppView {
        let root = std::env::temp_dir().join(format!("nexus-resize-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("k"),
            Space { root },
        ))
    }

    fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn down(app: &mut AppView, screen: Rect, x: u16, y: u16) -> bool {
        mouse(
            app,
            event(MouseEventKind::Down(MouseButton::Left), x, y),
            screen,
        )
    }

    fn move_to(app: &mut AppView, screen: Rect, x: u16, y: u16) -> bool {
        mouse(
            app,
            event(MouseEventKind::Drag(MouseButton::Left), x, y),
            screen,
        )
    }

    fn up(app: &mut AppView, screen: Rect, x: u16, y: u16) -> bool {
        mouse(
            app,
            event(MouseEventKind::Up(MouseButton::Left), x, y),
            screen,
        )
    }

    /// Draw one frame and return the screen as text.
    fn draw(app: &mut AppView, screen: Rect) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
        terminal.draw(|f| crate::ui::render(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..screen.height)
            .map(|y| (0..screen.width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// Apply the status lines core queued, as the event loop does.
    fn settle(app: &mut AppView) {
        while let Some(event) = app.pop_pending_event() {
            app.apply_event(&event);
        }
    }

    fn stored(app: &AppView, key: &str) -> Option<String> {
        app.core
            .db
            .load_settings()
            .unwrap()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    fn column_of(app: &AppView, screen: Rect) -> Rect {
        crate::ui::reading_column(screen, &app.settings.column_width)
    }

    const WIDE: Rect = Rect::new(0, 0, 200, 40);

    #[test]
    fn pressing_next_to_a_column_edge_drags_the_width_symmetrically() {
        let mut app = test_app();
        let column = column_of(&app, WIDE);
        assert_eq!((column.x, column.width), (15, 170));

        // Left gutter next to the edge grabs it; the drag follows the pointer.
        assert!(down(&mut app, WIDE, column.x - 1, 10));
        assert!(move_to(&mut app, WIDE, 40, 10));
        assert_eq!(app.settings.column_width, "120"); // 2 * |40 - 100|
        let column = column_of(&app, WIDE);
        assert_eq!((column.x, column.width), (40, 120));
        settle(&mut app);
        assert!(app.status.contains("column 120"), "{}", app.status);

        // The right edge mirrors it: 2 * |150 - 100|.
        assert!(move_to(&mut app, WIDE, 150, 10));
        assert_eq!(app.settings.column_width, "100");
        assert!(up(&mut app, WIDE, 150, 10));
        assert!(app.drag.is_none());
        assert_eq!(stored(&app, "column_width").as_deref(), Some("100"));
    }

    #[test]
    fn dragging_clamps_between_sixty_and_the_screen() {
        let mut app = test_app();
        assert!(down(&mut app, WIDE, 14, 10));
        move_to(&mut app, WIDE, 99, 10);
        assert_eq!(app.settings.column_width, "60", "never narrower than 60");
        move_to(&mut app, WIDE, 0, 10);
        assert_eq!(
            app.settings.column_width, "198",
            "never wider than the screen"
        );
        up(&mut app, WIDE, 0, 10);
        assert_eq!(stored(&app, "column_width").as_deref(), Some("198"));

        // A huge terminal still caps at the setting's own maximum.
        let huge = Rect::new(0, 0, 500, 40);
        let mut app = test_app();
        let column = column_of(&app, huge);
        assert!(down(&mut app, huge, column.right() + 1, 3));
        move_to(&mut app, huge, 499, 3);
        assert_eq!(app.settings.column_width, "400");
    }

    #[test]
    fn a_bare_click_on_the_edge_changes_and_saves_nothing() {
        let mut app = test_app();
        assert!(down(&mut app, WIDE, 14, 5));
        assert!(up(&mut app, WIDE, 14, 5));
        assert_eq!(app.settings.column_width, "auto");
        assert_eq!(stored(&app, "column_width"), None);
    }

    #[test]
    fn double_clicking_the_edge_resets_to_auto() {
        let mut app = test_app();
        app.core.set_setting("column_width", "120").unwrap();
        let column = column_of(&app, WIDE);
        assert_eq!(column.width, 120);
        assert!(down(&mut app, WIDE, column.x - 1, 5));
        assert!(up(&mut app, WIDE, column.x - 1, 5));
        assert!(down(&mut app, WIDE, column.x - 1, 5));
        assert!(app.drag.is_none(), "the second press resets, not drags");
        assert_eq!(app.settings.column_width, "auto");
        assert_eq!(stored(&app, "column_width").as_deref(), Some("auto"));
        up(&mut app, WIDE, column.x - 1, 5);
    }

    #[test]
    fn presses_elsewhere_in_the_gutter_or_the_column_are_left_alone() {
        let mut app = test_app();
        // Far gutter, inside the column, and the far side of the screen.
        for x in [0, 5, 12, 16, 100, 184, 188, 199] {
            assert!(!down(&mut app, WIDE, x, 10), "x={x}");
        }
        assert!(app.drag.is_none());
        assert!(!mouse(
            &mut app,
            event(MouseEventKind::ScrollUp, 14, 10),
            WIDE
        ));
    }

    #[test]
    fn no_column_drag_when_a_popup_is_open_or_the_column_fills_the_screen() {
        let mut app = test_app();
        app.popup = Popup::Help;
        assert!(!down(&mut app, WIDE, 14, 10), "popup open");
        app.popup = Popup::None;

        let narrow = Rect::new(0, 0, 40, 20);
        let column = column_of(&app, narrow);
        assert_eq!(column.width, narrow.width);
        assert!(!down(&mut app, narrow, 0, 5), "column fills the screen");
        assert!(!down(&mut app, narrow, 39, 5));
    }

    #[test]
    fn the_full_width_margin_is_a_grab_handle_too() {
        let mut app = test_app();
        app.core.set_setting("column_width", "full").unwrap();
        let screen = Rect::new(0, 0, 100, 30);
        assert!(down(&mut app, screen, 0, 4), "left margin");
        move_to(&mut app, screen, 20, 4);
        assert_eq!(app.settings.column_width, "60");
        up(&mut app, screen, 20, 4);
        assert!(down(&mut app, screen, 80, 4), "right of a 60-wide column");
    }

    #[test]
    fn a_column_drag_rewraps_history_like_a_terminal_resize() {
        let mut app = test_app();
        app.messages.push(nexus_core::db::Message {
            role: "assistant".into(),
            content: "word ".repeat(80),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
        let screen = Rect::new(0, 0, 200, 40);
        draw(&mut app, screen);
        let before = app.history_cache.key.1;
        assert_eq!(before, 169, "the column minus the scrollbar gutter");

        let column = column_of(&app, screen);
        assert!(down(&mut app, screen, column.x - 1, 10));
        move_to(&mut app, screen, 50, 10);
        draw(&mut app, screen);
        assert_eq!(app.history_cache.key.1, 99, "wrapped to the live width");
    }

    #[test]
    fn handles_mark_both_edges_while_dragging_only() {
        let mut app = test_app();
        let screen = Rect::new(0, 0, 200, 20);
        let at = |rows: &[String], x: usize, y: usize| rows[y].chars().nth(x).unwrap();
        let rows = draw(&mut app, screen);
        assert_ne!(at(&rows, 14, 3), '│');

        let column = column_of(&app, screen);
        assert!(down(&mut app, screen, column.x - 1, 3));
        move_to(&mut app, screen, 40, 3);
        let rows = draw(&mut app, screen);
        let column = column_of(&app, screen);
        for y in [0, 3, 19] {
            assert_eq!(at(&rows, usize::from(column.x) - 1, y), '│');
            assert_eq!(at(&rows, usize::from(column.right()), y), '│');
        }
        up(&mut app, screen, 40, 3);
        let rows = draw(&mut app, screen);
        let column = column_of(&app, screen);
        assert_ne!(at(&rows, usize::from(column.x) - 1, 3), '│');
    }

    // ----- popups -----

    const SCREEN: Rect = Rect::new(0, 0, 120, 40);

    /// A settings popup, drawn once so its rect is recorded.
    fn settings_app() -> AppView {
        let mut app = test_app();
        app.open_settings();
        draw(&mut app, SCREEN);
        app
    }

    fn hit(app: &AppView) -> Rect {
        app.popup_hit.get().expect("a popup was drawn").rect
    }

    fn size(app: &AppView) -> Option<(u16, u16)> {
        popup_size(&app.settings.popup_sizes, "settings")
    }

    #[test]
    fn each_edge_and_corner_changes_only_its_own_axes() {
        let rect = hit(&settings_app());
        let (mid_x, mid_y) = (rect.x + rect.width / 2, rect.y + rect.height / 2);
        let (right, bottom) = (rect.right() - 1, rect.bottom() - 1);
        // (grab point, drag target, expected (w%, h%)). The default is 64x60.
        let cases = [
            ("left edge", (rect.x, mid_y), (20, 20), (67, 60)),
            ("right edge", (right, mid_y), (100, 33), (67, 60)),
            ("top edge", (mid_x, rect.y), (60, 4), (64, 80)),
            ("bottom edge", (mid_x, bottom), (70, 38), (64, 90)),
            ("top-left corner", (rect.x, rect.y), (30, 4), (50, 80)),
            ("top-right corner", (right, rect.y), (90, 4), (50, 80)),
            ("bottom-left corner", (rect.x, bottom), (30, 38), (50, 90)),
            ("bottom-right corner", (right, bottom), (90, 38), (50, 90)),
        ];
        for (name, grab, to, expected) in cases {
            let mut app = settings_app();
            assert!(down(&mut app, SCREEN, grab.0, grab.1), "{name}");
            assert!(move_to(&mut app, SCREEN, to.0, to.1), "{name}");
            assert_eq!(size(&app), Some(expected), "{name}");
            assert!(up(&mut app, SCREEN, to.0, to.1));
        }
    }

    #[test]
    fn the_drag_is_symmetric_and_clamped_to_the_minimum_and_the_screen() {
        let mut app = settings_app();
        let rect = hit(&app);
        assert!(down(&mut app, SCREEN, rect.x, rect.y + 5));
        // Past the center and almost on it: never under 40 cells wide.
        move_to(&mut app, SCREEN, 59, rect.y + 5);
        draw(&mut app, SCREEN);
        let small = hit(&app);
        assert_eq!(small.width, 40);
        assert_eq!(small.x, (SCREEN.width - 40) / 2, "still centered");
        // Off the screen edge: exactly the screen.
        move_to(&mut app, SCREEN, 0, rect.y + 5);
        draw(&mut app, SCREEN);
        assert_eq!(hit(&app).width, SCREEN.width);
        up(&mut app, SCREEN, 0, 0);

        // Height has its own floor of ten rows.
        let mut app = settings_app();
        let rect = hit(&app);
        assert!(down(&mut app, SCREEN, rect.x + 10, rect.y));
        move_to(&mut app, SCREEN, rect.x + 10, 20);
        draw(&mut app, SCREEN);
        assert_eq!(hit(&app).height, 10);
        move_to(&mut app, SCREEN, rect.x + 10, 0);
        draw(&mut app, SCREEN);
        assert_eq!(hit(&app).height, SCREEN.height);
    }

    #[test]
    fn releasing_persists_the_percentages_and_a_new_session_reads_them() {
        let mut app = settings_app();
        let rect = hit(&app);
        assert!(down(&mut app, SCREEN, rect.x, rect.y));
        move_to(&mut app, SCREEN, 20, 5);
        assert_eq!(
            stored(&app, "popup_sizes"),
            None,
            "nothing is written until release"
        );
        up(&mut app, SCREEN, 20, 5);
        // The corner moved both axes: 2*|20-60| = 80 of 120 columns, and
        // 2*|5-20| = 30 of 40 rows.
        assert_eq!(
            stored(&app, "popup_sizes").as_deref(),
            Some("settings:67x75")
        );
        assert_eq!(app.settings.popup_sizes, "settings:67x75");

        // Applied on the next render, and again after a reload.
        draw(&mut app, SCREEN);
        let resized = hit(&app);
        assert_eq!(resized, crate::ui::centered(SCREEN, 67, 75));
        assert!(resized.width >= 79 && resized.height == 30, "{resized:?}");
        let mut reloaded = test_app();
        reloaded
            .core
            .set_setting("popup_sizes", "settings:67x75")
            .unwrap();
        reloaded.open_settings();
        draw(&mut reloaded, SCREEN);
        assert_eq!(hit(&reloaded), resized);
    }

    #[test]
    fn a_saved_size_stays_usable_when_the_terminal_shrinks() {
        let mut app = test_app();
        app.core
            .set_setting("popup_sizes", "settings:20x20")
            .unwrap();
        app.open_settings();
        let small = Rect::new(0, 0, 100, 30);
        draw(&mut app, small);
        let rect = hit(&app);
        assert_eq!((rect.width, rect.height), (40, 10));
        // And on a screen smaller than the minimum it is the screen.
        let tiny = Rect::new(0, 0, 30, 8);
        draw(&mut app, tiny);
        assert_eq!((hit(&app).width, hit(&app).height), (30, 8));
    }

    #[test]
    fn double_clicking_a_border_resets_that_popup_to_its_default() {
        let mut app = test_app();
        app.core
            .set_setting("popup_sizes", "settings:90x90,help:70x70")
            .unwrap();
        app.open_settings();
        draw(&mut app, SCREEN);
        let default = crate::ui::centered(SCREEN, 64, 60);
        let rect = hit(&app);
        assert_ne!(rect, default);
        assert!(down(&mut app, SCREEN, rect.x, rect.y + 4));
        assert!(up(&mut app, SCREEN, rect.x, rect.y + 4));
        assert!(down(&mut app, SCREEN, rect.x, rect.y + 4));
        up(&mut app, SCREEN, rect.x, rect.y + 4);
        assert_eq!(
            app.settings.popup_sizes, "help:70x70",
            "only this popup resets"
        );
        assert_eq!(stored(&app, "popup_sizes").as_deref(), Some("help:70x70"));
        draw(&mut app, SCREEN);
        assert_eq!(hit(&app), default);
    }

    #[test]
    fn presses_inside_the_border_fall_through_to_the_popup() {
        let mut app = settings_app();
        let rect = hit(&app);
        for (x, y) in [
            (rect.x + 1, rect.y + 3),
            (rect.x + 5, rect.y + 1),
            (rect.right() - 2, rect.bottom() - 2),
            (rect.x + rect.width / 2, rect.y + rect.height / 2),
        ] {
            assert!(!down(&mut app, SCREEN, x, y), "({x}, {y})");
        }
        // Outside the popup entirely is not a border either.
        assert!(!down(&mut app, SCREEN, 1, 1));
        assert!(app.drag.is_none());
    }

    #[test]
    fn the_border_turns_accent_and_the_hint_shows_the_size_while_resizing() {
        let mut app = settings_app();
        // Distinguish the idle border from the accent (they match in some themes).
        app.theme.border = ratatui::style::Color::Blue;
        let rect = hit(&app);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let idle = terminal.backend().buffer()[(rect.x, rect.y + 6)].fg;
        assert_ne!(idle, app.theme.accent);

        assert!(down(&mut app, SCREEN, rect.x, rect.y + 6));
        move_to(&mut app, SCREEN, 20, rect.y + 6);
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let resized = hit(&app);
        let buffer = terminal.backend().buffer();
        for (x, y) in [
            (resized.x, resized.y + 6),
            (resized.right() - 1, resized.y + 6),
            (resized.x + 4, resized.y),
            (resized.x + 4, resized.bottom() - 1),
        ] {
            assert_eq!(buffer[(x, y)].fg, app.theme.accent, "({x}, {y})");
        }
        let bottom: String = (resized.x..resized.right())
            .map(|x| buffer[(x, resized.bottom() - 1)].symbol())
            .collect();
        assert!(
            bottom.contains(&format!("{}×{}", resized.width, resized.height)),
            "{bottom}"
        );

        up(&mut app, SCREEN, 20, rect.y + 6);
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let after = terminal.backend().buffer();
        assert_ne!(after[(hit(&app).x, hit(&app).y + 6)].fg, app.theme.accent);
    }

    #[test]
    fn a_resized_settings_popup_draws_its_rows_at_the_new_size() {
        // The picture for the changelog: drag the left border out and the
        // top border up, then print what the popup looks like.
        let mut app = settings_app();
        let rect = hit(&app);
        assert!(down(&mut app, SCREEN, rect.x, rect.y));
        move_to(&mut app, SCREEN, 10, 3);
        up(&mut app, SCREEN, 10, 3);
        let rows = draw(&mut app, SCREEN);
        let resized = hit(&app);
        assert!(resized.width > rect.width && resized.height > rect.height);
        let picture = rows.join("\n");
        assert!(picture.contains("nerd config"), "{picture}");
        println!("{picture}");
    }
}
