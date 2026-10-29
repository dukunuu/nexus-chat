//! The full-screen image viewer popup: the picture at its zoom with the
//! terminal's real graphics protocol (via `ratatui-image`), a metadata footer,
//! the keys and mouse gestures that move, zoom and act on it, and the action
//! menu. All decoding and encoding happens in `crate::image_viewer`, off the
//! draw path; the actions themselves live in `crate::image_viewer_actions`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nexus_core::app::Popup;
use ratatui::Frame;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::app_view::AppView;
use crate::image_viewer::{Source, Zoom, display_name, source_of, video_for};
use crate::image_viewer_actions::{Action, DOUBLE_CLICK, Drag, Menu, menu_rect};
use crate::ui::style::{self, glyph};

use super::chrome;

/// Cells one arrow press pans in 1:1; Shift pans [`FAST`] times as far.
const PAN_CELLS: i32 = 4;
const FAST: i32 = 4;

/// The hint bar's pieces, best first; the bar keeps as many as fit in order
/// of importance and prints them in reading order.
fn hint(app: &AppView, video: bool, width: u16) -> String {
    let (pan_x, pan_y) = app.image_viewer.can_pan();
    let mut parts: Vec<(u8, usize, String)> = Vec::new();
    let mut add = |priority: u8, text: &str| {
        let order = parts.len();
        parts.push((priority, order, text.to_string()));
    };
    if video {
        add(11, "Enter play");
    }
    if pan_x || pan_y {
        add(9, "arrows/hjkl pan (Shift faster)");
        add(8, "n/p prev/next");
    } else {
        add(9, "\u{2190}\u{2192} prev/next");
    }
    add(
        8,
        if app.image_viewer.is_fit() {
            "f 1:1"
        } else {
            "f fit"
        },
    );
    add(7, "o system viewer");
    add(7, "c copy path");
    add(6, "y copy image");
    add(5, ". menu");
    add(4, "s save");
    add(3, "+/- zoom");
    add(2, "r reveal");
    add(1, "a attach");
    add(10, "Esc close");
    let budget = usize::from(width).saturating_sub(4);
    let mut ranked: Vec<_> = parts.iter().collect();
    ranked.sort_by_key(|(priority, order, _)| (std::cmp::Reverse(*priority), *order));
    let mut used = 0;
    let mut kept = Vec::new();
    for (_, order, text) in ranked {
        let cost = crate::ui::style::width(text)
            + if kept.is_empty() {
                0
            } else {
                crate::ui::style::width(style::SEP)
            };
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(*order);
    }
    kept.sort_unstable();
    kept.into_iter()
        .map(|order| parts[order].2.as_str())
        .collect::<Vec<_>>()
        .join(style::SEP)
}

fn message(source: &Source) -> &'static str {
    match source {
        Source::Loading => "loading\u{2026}",
        Source::Failed => "couldn't load this image",
        Source::Off => "remote images are off \u{b7} press o to open the URL in your browser",
        Source::Missing => "file not found",
        Source::Ready(_) => "decoding\u{2026}",
    }
}

/// The footer's fields, least important dropped until the line fits `width`.
fn footer_line(app: &AppView, video: bool, width: usize) -> Vec<(String, u8)> {
    let mut fields = app.image_viewer.footer_fields();
    if video {
        fields.insert(0, ("video thumbnail".into(), 10));
    }
    let sep = crate::ui::style::width(style::SEP);
    let total = |fields: &[(String, u8)]| -> usize {
        fields
            .iter()
            .map(|(text, _)| crate::ui::style::width(text))
            .sum::<usize>()
            + sep * fields.len().saturating_sub(1)
    };
    while fields.len() > 1 && total(&fields) > width {
        let weakest = fields
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, priority))| *priority)
            .map(|(index, _)| index)
            .unwrap_or_default();
        fields.remove(weakest);
    }
    fields
}

pub fn render(f: &mut Frame, app: &mut AppView) {
    app.pump_image_viewer();
    if app.popup != Popup::ImageViewer {
        return;
    }
    let Some(item) = app.image_viewer.current().cloned() else {
        app.popup = Popup::None;
        return;
    };
    let area = f.area();
    let video = video_for(&item.target).is_some();
    let name = chrome::truncate_middle(
        &display_name(&item.target),
        usize::from(area.width.saturating_sub(8)),
    );
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, name),
        &hint(app, video, area.width),
        app,
        true,
        chrome::Tone::Normal,
    );
    if inner.height < 2 || inner.width < 4 {
        return;
    }
    let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
    let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    app.image_viewer.pointer.body = body;
    app.image_viewer.pointer.screen = area;

    let source = source_of(&item.target);
    app.image_viewer.drive(&source, body);
    app.pump_image_viewer();
    if app.popup != Popup::ImageViewer {
        return;
    }

    if let Some(protocol) = app.image_viewer.protocol_ready() {
        let size = protocol.size();
        let (w, h) = (size.width.min(body.width), size.height.min(body.height));
        let rect = Rect::new(
            body.x + (body.width - w) / 2,
            body.y + (body.height - h) / 2,
            w,
            h,
        );
        f.render_widget(ratatui_image::Image::new(protocol), rect);
    } else {
        let text = format!("{} {}", glyph::MEDIA, message(&source));
        f.render_widget(
            Paragraph::new(Line::from(style::meta(&app.theme, text))).alignment(Alignment::Center),
            Rect::new(body.x, body.y + body.height / 2, body.width, 1),
        );
    }

    let count = app.image_viewer.items().len();
    let counter = (count > 1).then(|| format!("{}/{count} ", app.image_viewer.index() + 1));
    let room = usize::from(footer.width).saturating_sub(
        counter
            .as_ref()
            .map_or(0, |c| crate::ui::style::width(c) + 1),
    );
    let mut spans = Vec::new();
    for (index, (field, _)) in footer_line(app, video, room).into_iter().enumerate() {
        if index > 0 {
            spans.push(style::sep(&app.theme));
        }
        spans.push(style::meta(&app.theme, field));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), footer);
    if let Some(counter) = counter {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                counter,
                Style::default().fg(app.theme.accent),
            )))
            .alignment(Alignment::Right),
            footer,
        );
    }
    render_chevron(f, app, body);
    render_menu(f, app, area);
}

// ── Edge zones and chevrons ───────────────────────────────────────────────

/// Which neighbour a click at `column` goes to: the outer third of the window
/// on each side, or just three columns when the picture is wider than the
/// window (so a pan never ends in an accidental page turn).
fn edge_step(app: &AppView, body: Rect, column: u16, row: u16) -> Option<isize> {
    if app.image_viewer.items().len() < 2
        || !body.contains(Position::new(column, row))
        || body.width < 9
    {
        return None;
    }
    let zone = if app.image_viewer.can_pan().0 {
        3
    } else {
        body.width / 3
    };
    if column < body.x + zone {
        (app.image_viewer.index() > 0).then_some(-1)
    } else if column >= body.right() - zone {
        (app.image_viewer.index() + 1 < app.image_viewer.items().len()).then_some(1)
    } else {
        None
    }
}

/// A small chevron mid-height at the edge being hovered, so the click target
/// is discoverable without drawing over the picture the rest of the time.
fn render_chevron(f: &mut Frame, app: &AppView, body: Rect) {
    let Some((column, row)) = app.image_viewer.pointer.hover else {
        return;
    };
    if app.image_viewer.pointer.menu.is_some() {
        return;
    }
    let Some(step) = edge_step(app, body, column, row) else {
        return;
    };
    let (glyph, x) = if step < 0 {
        (glyph::PREV, body.x + 1)
    } else {
        (glyph::NEXT, body.right() - 2)
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            glyph,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(x, body.y + body.height / 2, 1, 1),
    );
}

// ── The action menu ───────────────────────────────────────────────────────

fn render_menu(f: &mut Frame, app: &AppView, screen: Rect) {
    let Some(menu) = app.image_viewer.pointer.menu else {
        return;
    };
    let area = menu_rect(screen, menu.at);
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "actions"),
        "Enter run \u{b7} Esc close",
        app,
        true,
        chrome::Tone::Normal,
    );
    // The highlight marker takes two columns and the list a scrollbar gutter.
    let width = usize::from(inner.width).saturating_sub(3);
    let items: Vec<ListItem> = Action::ALL
        .iter()
        .map(|action| {
            let label = action.label();
            let gap = width.saturating_sub(crate::ui::style::width(label) + action.key().len());
            ListItem::new(Line::from(vec![
                Span::raw(label),
                Span::raw(" ".repeat(gap)),
                style::meta(&app.theme, action.key()),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(menu.selected));
    chrome::render_list(
        f,
        chrome::standard_list(items, &app.theme),
        &mut state,
        inner,
        Action::ALL.len(),
        1,
        app,
    );
}

fn close_menu(app: &mut AppView) {
    app.image_viewer.pointer.menu = None;
}

fn move_menu(app: &mut AppView, delta: isize) {
    if let Some(menu) = &mut app.image_viewer.pointer.menu {
        menu.selected = menu
            .selected
            .saturating_add_signed(delta)
            .min(Action::ALL.len() - 1);
    }
}

fn run_menu(app: &mut AppView, index: usize) {
    close_menu(app);
    if let Some(action) = Action::ALL.get(index).copied() {
        app.viewer_action(action);
    }
}

/// The menu row under a cell, if the cell is on one.
fn menu_row(app: &AppView, menu: Menu, column: u16, row: u16) -> Option<usize> {
    let area = menu_rect(app.image_viewer.pointer.screen, menu.at);
    let inner = Rect::new(
        area.x + 1 + chrome::PAD,
        area.y + 1,
        area.width.saturating_sub(2 + 2 * chrome::PAD),
        area.height.saturating_sub(2),
    );
    inner
        .contains(Position::new(column, row))
        .then(|| usize::from(row - inner.y))
        .filter(|index| *index < Action::ALL.len())
}

// ── Mouse ─────────────────────────────────────────────────────────────────

/// The viewer owns every mouse event while it is open: drag pans a picture
/// larger than the window, the wheel zooms around the pointer, a double-click
/// toggles fit / 100%, a click on an edge goes to the neighbouring image, and
/// a right-click opens the action menu.
pub fn mouse(app: &mut AppView, m: MouseEvent) {
    let pos = (m.column, m.row);
    app.image_viewer.pointer.hover = Some(pos);
    if let Some(menu) = app.image_viewer.pointer.menu {
        menu_mouse(app, m, menu);
        return;
    }
    let body = app.image_viewer.pointer.body;
    let inside = body.contains(Position::new(m.column, m.row));
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) if inside => {
            app.image_viewer.pointer.drag = Some(Drag {
                from: pos,
                last: pos,
                moved: false,
            });
        }
        MouseEventKind::Drag(MouseButton::Left) => drag(app, pos),
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(drag) = app.image_viewer.pointer.drag.take()
                && !drag.moved
            {
                click(app, pos);
            }
        }
        MouseEventKind::Down(MouseButton::Right) if inside => {
            app.open_viewer_menu(Some(pos));
        }
        MouseEventKind::ScrollUp if inside => app.image_viewer.zoom_step(1, Some(pos)),
        MouseEventKind::ScrollDown if inside => app.image_viewer.zoom_step(-1, Some(pos)),
        _ => {}
    }
}

fn drag(app: &mut AppView, pos: (u16, u16)) {
    let Some(mut drag) = app.image_viewer.pointer.drag else {
        return;
    };
    if pos != drag.from {
        drag.moved = true;
    }
    let (can_x, can_y) = app.image_viewer.can_pan();
    let (cell_w, cell_h) = app.image_viewer.cell_px();
    let (dx, dy) = (
        i32::from(pos.0) - i32::from(drag.last.0),
        i32::from(pos.1) - i32::from(drag.last.1),
    );
    // Grabbing the picture: it follows the pointer, so the window moves the
    // other way.
    app.image_viewer.pan_px(
        if can_x { -dx * cell_w } else { 0 },
        if can_y { -dy * cell_h } else { 0 },
    );
    drag.last = pos;
    app.image_viewer.pointer.drag = Some(drag);
}

fn click(app: &mut AppView, pos: (u16, u16)) {
    let body = app.image_viewer.pointer.body;
    if let Some(step) = edge_step(app, body, pos.0, pos.1) {
        app.image_viewer.pointer.last_click = None;
        app.image_viewer.step(step);
        return;
    }
    let double = app
        .image_viewer
        .pointer
        .last_click
        .is_some_and(|(at, last)| at.elapsed() <= DOUBLE_CLICK && last == pos);
    if double {
        app.image_viewer.pointer.last_click = None;
        app.image_viewer.toggle_zoom_at(pos);
    } else {
        app.image_viewer.pointer.last_click = Some((std::time::Instant::now(), pos));
    }
}

fn menu_mouse(app: &mut AppView, m: MouseEvent, menu: Menu) {
    let row = menu_row(app, menu, m.column, m.row);
    match m.kind {
        MouseEventKind::Moved => {
            if let (Some(row), Some(open)) = (row, &mut app.image_viewer.pointer.menu) {
                open.selected = row;
            }
        }
        MouseEventKind::Down(MouseButton::Left) => match row {
            Some(row) => run_menu(app, row),
            None => close_menu(app),
        },
        MouseEventKind::Down(MouseButton::Right) => {
            close_menu(app);
            app.open_viewer_menu(Some((m.column, m.row)));
        }
        MouseEventKind::ScrollUp => move_menu(app, -1),
        MouseEventKind::ScrollDown => move_menu(app, 1),
        _ => {}
    }
}

// ── Keys ──────────────────────────────────────────────────────────────────

/// An arrow-style move: pans a picture larger than the window on that axis,
/// otherwise steps to the previous/next image.
fn arrow(app: &mut AppView, dx: i32, dy: i32, fast: bool) {
    let (pan_x, pan_y) = app.image_viewer.can_pan();
    let reach = PAN_CELLS * if fast { FAST } else { 1 };
    if (dx != 0 && pan_x) || (dy != 0 && pan_y) {
        app.image_viewer.pan_by(dx * reach, dy * reach);
    } else {
        app.image_viewer.step(if dx + dy < 0 { -1 } else { 1 });
    }
}

/// Keys while the menu is open. Returns whether the key was used; any other
/// key closes the menu and then does its usual thing, so the hotkeys shown in
/// the menu work from it.
fn menu_key(app: &mut AppView, key: KeyEvent) -> bool {
    let Some(menu) = app.image_viewer.pointer.menu else {
        return false;
    };
    match key.code {
        KeyCode::Esc | KeyCode::Char('.' | 'q') => close_menu(app),
        KeyCode::Up | KeyCode::Char('k') => move_menu(app, -1),
        KeyCode::Down | KeyCode::Char('j') => move_menu(app, 1),
        KeyCode::Home => move_menu(app, -100),
        KeyCode::End => move_menu(app, 100),
        KeyCode::Enter => run_menu(app, menu.selected),
        _ => {
            close_menu(app);
            return false;
        }
    }
    true
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    if menu_key(app, key) {
        return;
    }
    // Ctrl+C (and Cmd+C where the terminal forwards it) copies the picture.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && key.code == KeyCode::Char('c')
    {
        app.viewer_action(Action::CopyImage);
        return;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return;
    }
    let fast = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_image_viewer(),
        KeyCode::Char('f') => app.image_viewer.toggle_fit(),
        KeyCode::Char('+' | '=') => app.image_viewer.zoom_step(1, None),
        KeyCode::Char('-' | '_') => app.image_viewer.zoom_step(-1, None),
        KeyCode::Char('0') => app.image_viewer.set_zoom(Zoom::Fit, None),
        KeyCode::Char('1') => app.image_viewer.set_zoom(Zoom::Percent(100), None),
        KeyCode::Char('o') => app.viewer_action(Action::Open),
        KeyCode::Char('c') => app.viewer_action(Action::CopyPath),
        KeyCode::Char('y') => app.viewer_action(Action::CopyImage),
        KeyCode::Char('s') => app.viewer_action(Action::SaveToFiles),
        KeyCode::Char('S') => app.viewer_action(Action::SaveToDownloads),
        KeyCode::Char('r') => app.viewer_action(Action::Reveal),
        KeyCode::Char('a') => app.viewer_action(Action::Attach),
        KeyCode::Char('.') => app.open_viewer_menu(None),
        KeyCode::Enter => {
            if app
                .image_viewer
                .current()
                .is_some_and(|item| video_for(&item.target).is_some())
            {
                app.open_viewer_item_externally();
            }
        }
        KeyCode::Left | KeyCode::Char('h') => arrow(app, -1, 0, fast),
        KeyCode::Right | KeyCode::Char('l') => arrow(app, 1, 0, fast),
        KeyCode::Up | KeyCode::Char('k') => arrow(app, 0, -1, fast),
        KeyCode::Down | KeyCode::Char('j') => arrow(app, 0, 1, fast),
        KeyCode::Char('H') => arrow(app, -1, 0, true),
        KeyCode::Char('L') => arrow(app, 1, 0, true),
        KeyCode::Char('K') => arrow(app, 0, -1, true),
        KeyCode::Char('J') => arrow(app, 0, 1, true),
        KeyCode::Char('n' | ']' | ' ') | KeyCode::PageDown => {
            app.image_viewer.step(1);
        }
        KeyCode::Char('p' | '[') | KeyCode::PageUp => {
            app.image_viewer.step(-1);
        }
        KeyCode::Home | KeyCode::Char('g') => app.image_viewer.go(0),
        KeyCode::End | KeyCode::Char('G') => app.image_viewer.go(usize::MAX),
        _ => {}
    }
}
