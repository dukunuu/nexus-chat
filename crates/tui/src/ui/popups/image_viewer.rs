//! The full-screen image viewer popup: the image fit to the window with the
//! terminal's real graphics protocol (via `ratatui-image`), a metadata footer,
//! and the keys that move between the conversation's images. All decoding and
//! encoding happens in `crate::image_viewer`, off the draw path.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;
use crate::image_viewer::{Source, display_name, source_of, video_for};
use crate::ui::style::{self, glyph};

use super::chrome;

/// Cells one arrow press pans in 1:1; Shift pans [`FAST`] times as far.
const PAN_CELLS: i32 = 4;
const FAST: i32 = 4;

fn hint(app: &AppView, video: bool) -> String {
    let (pan_x, pan_y) = app.image_viewer.can_pan();
    let mut parts = Vec::new();
    if video {
        parts.push("Enter play");
    }
    if pan_x || pan_y {
        parts.push("arrows/hjkl pan (Shift faster)");
        parts.push("n/p prev/next");
    } else {
        parts.push("\u{2190}\u{2192} prev/next");
    }
    parts.push(if app.image_viewer.is_fit() {
        "f 1:1"
    } else {
        "f fit"
    });
    parts.push("o system viewer");
    parts.push("c copy");
    parts.push("Esc close");
    parts.join(style::SEP)
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
        &hint(app, video),
        app,
        true,
        chrome::Tone::Normal,
    );
    if inner.height < 2 || inner.width < 4 {
        return;
    }
    let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
    let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);

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

    let mut fields = app.image_viewer.info_fields();
    if video {
        fields.insert(0, "video thumbnail".into());
    }
    let mut spans = Vec::new();
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            spans.push(style::sep(&app.theme));
        }
        spans.push(style::meta(&app.theme, field));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), footer);
    let count = app.image_viewer.items().len();
    if count > 1 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{}/{count} ", app.image_viewer.index() + 1),
                Style::default().fg(app.theme.accent),
            )))
            .alignment(Alignment::Right),
            footer,
        );
    }
}

/// An arrow-style move: pans a 1:1 view that is larger than the window on
/// that axis, otherwise steps to the previous/next image.
fn arrow(app: &mut AppView, dx: i32, dy: i32, fast: bool) {
    let (pan_x, pan_y) = app.image_viewer.can_pan();
    let reach = PAN_CELLS * if fast { FAST } else { 1 };
    if (dx != 0 && pan_x) || (dy != 0 && pan_y) {
        app.image_viewer.pan_by(dx * reach, dy * reach);
    } else {
        app.image_viewer.step(if dx + dy < 0 { -1 } else { 1 });
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }
    let fast = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_image_viewer(),
        KeyCode::Char('f') => app.image_viewer.toggle_fit(),
        KeyCode::Char('o') => app.open_viewer_item_externally(),
        KeyCode::Char('c') => {
            if let Some(item) = app.image_viewer.current().cloned() {
                app.copy_text(&item.target);
            }
        }
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
