//! Scripted apps share the terminal event loop and the nexus-ui renderer.

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nexus_core::app::Popup;
use nexus_core::interactive::{Node, Tone};
use nexus_core::tui_apps::{Cell, Runner, View};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal, backend::TestBackend};
use serde_json::{Value, json};

use crate::app_view::AppView;
use crate::interactive::{
    card_buffer, card_height, node_regions, render_nodes, safe, stepper_arrows,
};
use crate::theme::Theme;

use super::chrome;

/// Disposable popup state: nothing survives closing the app.
pub struct TuiApp {
    runner: Option<Runner>,
    view: Option<View>,
    error: String,
    focus: usize,
    width: u16,
    dirty: bool,
    hits: Vec<(Rect, usize)>,
    next_tick: Option<Instant>,
}

#[derive(Clone)]
enum Control {
    Button(String),
    Input(String),
    Toggle(String),
    Select(usize, String),
    Stepper(usize, String),
    Table(usize, String),
}

impl TuiApp {
    fn new(source: Result<String>) -> Self {
        let runner = source.and_then(|source| Runner::new(&source));
        let error = runner
            .as_ref()
            .err()
            .map_or_else(String::new, ToString::to_string);
        let mut app = Self {
            runner: runner.ok(),
            view: None,
            error,
            focus: 0,
            width: 0,
            dirty: true,
            hits: Vec::new(),
            next_tick: None,
        };
        app.schedule_tick();
        app
    }

    fn schedule_tick(&mut self) {
        self.next_tick = if self.error.is_empty() {
            self.runner
                .as_ref()
                .and_then(Runner::tick_ms)
                .and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)))
        } else {
            None
        };
    }

    fn refresh(&mut self, width: u16) {
        if !self.error.is_empty() || (!self.dirty && self.width == width) {
            return;
        }
        self.width = width;
        self.dirty = false;
        match self.runner.as_ref().expect("initialized app").view(width) {
            Ok(view) => self.view = Some(view),
            Err(error) => {
                self.error = error.to_string();
                self.next_tick = None;
            }
        }
        self.focus = self.focus.min(self.controls().len().saturating_sub(1));
    }

    fn dispatch(&mut self, event: &Value) {
        if !self.error.is_empty() {
            return;
        }
        if let Err(error) = self.runner.as_mut().expect("initialized app").update(event) {
            self.error = error.to_string();
        }
        self.dirty = true;
        self.schedule_tick();
    }

    fn controls(&self) -> Vec<Control> {
        let Some(view) = &self.view else {
            return Vec::new();
        };
        view.session
            .document()
            .nodes()
            .iter()
            .enumerate()
            .flat_map(|(index, node)| match node {
                Node::Buttons { items, .. } => items
                    .iter()
                    .map(|button| Control::Button(button.action.clone()))
                    .collect(),
                Node::Input { key, .. } => vec![Control::Input(key.clone())],
                Node::Toggle { key, .. } => vec![Control::Toggle(key.clone())],
                Node::Checklist { items } => items
                    .iter()
                    .map(|item| Control::Toggle(item.key.clone()))
                    .collect(),
                Node::Select { key, .. } => vec![Control::Select(index, key.clone())],
                Node::Stepper { key, .. } => vec![Control::Stepper(index, key.clone())],
                Node::Table {
                    select: Some(key), ..
                } => vec![Control::Table(index, key.clone())],
                _ => Vec::new(),
            })
            .collect()
    }

    fn activate(&mut self, code: KeyCode) -> bool {
        let Some(control) = self.controls().get(self.focus).cloned() else {
            return false;
        };
        let Some(view) = &mut self.view else {
            return false;
        };
        let session = &mut view.session;
        let activated = matches!(code, KeyCode::Enter | KeyCode::Char(' '));
        let mut event = None;
        let result = match control {
            Control::Button(action) if activated => {
                event = Some(json!({"type":"button", "action":action}));
                Ok(())
            }
            Control::Input(key) if matches!(code, KeyCode::Char(_) | KeyCode::Backspace) => {
                let mut value = session.value(&key);
                match code {
                    KeyCode::Char(ch) if !ch.is_control() => value.push(ch),
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    _ => return false,
                }
                event = Some(json!({"type":"input", "key":key, "value":value}));
                session.input(&key, value)
            }
            Control::Toggle(key) if activated => session.toggle(&key).map(|()| {
                event =
                    Some(json!({"type":"input", "key":key, "value":session.value(&key) == "true"}));
            }),
            Control::Select(index, key)
                if activated || matches!(code, KeyCode::Left | KeyCode::Right) =>
            {
                session.cycle(index, code != KeyCode::Left).map(|()| {
                    event = Some(json!({"type":"input", "key":key, "value":session.value(&key)}));
                })
            }
            Control::Stepper(index, key) if matches!(code, KeyCode::Left | KeyCode::Right) => {
                session.step(index, code == KeyCode::Right).and_then(|()| {
                    event = Some(json!({
                        "type":"input", "key":key,
                        "value":session.number(&json!({"get":key}))?
                    }));
                    Ok(())
                })
            }
            Control::Table(index, key) if matches!(code, KeyCode::Up | KeyCode::Down) => {
                let row = session.selected_row(&key);
                session
                    .select_row(
                        index,
                        if code == KeyCode::Down {
                            row.saturating_add(1)
                        } else {
                            row.saturating_sub(1)
                        },
                    )
                    .map(|()| {
                        event = Some(
                            json!({"type":"input", "key":key, "value":session.selected_row(&key)}),
                        );
                    })
            }
            _ => return false,
        };
        match result {
            Ok(()) => {
                if let Some(event) = event {
                    self.dispatch(&event);
                }
            }
            Err(error) => {
                self.error = error.to_string();
                self.next_tick = None;
            }
        }
        true
    }
}

/// Open the requested app, installing the bundled RPS only if it is missing.
pub fn open(app: &mut AppView, name: &str) {
    let root = app
        .space
        .scripts_dir(&app.active_space.name)
        .with_file_name("tui_apps");
    // A pinned panel answers to its name when no script app does.
    if name != "rps"
        && !nexus_core::tui_apps::app_path(&root, name).is_ok_and(|path| path.is_file())
        && crate::interactive::open_pinned(app, name)
    {
        return;
    }
    app.tui_app = Some(Box::new(TuiApp::new(nexus_core::tui_apps::load(
        &root, name, true,
    ))));
    app.popup = Popup::TuiApp;
    app.sel.clear();
}

/// Consume a tool request after events, without stealing an approval popup.
pub fn take_pending(app: &mut AppView) {
    if app.popup == Popup::Approval {
        return;
    }
    let name = app.core.pending_tui_app.lock().expect("pending app").take();
    if let Some(name) = name {
        open(app, &name);
    }
    if app.popup != Popup::TuiApp {
        app.tui_app = None;
    }
}

/// Deadline polled by the existing tokio select loop, never a second loop.
pub fn deadline(app: &AppView) -> Option<Instant> {
    if app.popup != Popup::TuiApp {
        return None;
    }
    app.tui_app.as_ref()?.next_tick
}

/// Deliver one tick and reschedule using the updated state's opt-in interval.
pub fn tick(app: &mut AppView) {
    if app.popup == Popup::TuiApp
        && let Some(instance) = &mut app.tui_app
    {
        instance.dispatch(&json!({"type":"tick"}));
    }
}

/// Keyboard controls stay local; Escape disposes the in-memory instance.
pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.tui_app = None;
        app.popup = Popup::None;
        return;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }
    let Some(instance) = &mut app.tui_app else {
        return;
    };
    if instance.dirty {
        instance.refresh(instance.width.max(60));
    }
    if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
        let count = instance.controls().len();
        if count > 0 {
            instance.focus = (instance.focus
                + if key.code == KeyCode::Tab {
                    1
                } else {
                    count - 1
                })
                % count;
        }
        return;
    }
    if instance.activate(key.code) {
        return;
    }
    let key: String = match key.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(ch) if ch.is_ascii_alphanumeric() => ch.to_string(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Enter => "enter".into(),
        _ => return,
    };
    instance.dispatch(&json!({"type":"key", "key":key}));
}

/// Paste is an input event, not code or a host command.
pub fn paste(app: &mut AppView, text: &str) {
    let Some(instance) = &mut app.tui_app else {
        return;
    };
    if let Some(Control::Input(key)) = instance.controls().get(instance.focus) {
        let value = format!(
            "{}{}",
            instance
                .view
                .as_ref()
                .expect("input view")
                .session
                .value(key),
            safe(text).replace('\n', "")
        );
        instance.dispatch(&json!({"type":"input", "key":key, "value":value}));
    }
}

/// Click the same control regions produced by the shared component renderer.
pub fn mouse(app: &mut AppView, event: MouseEvent) {
    let Some(instance) = &mut app.tui_app else {
        return;
    };
    if event.kind == MouseEventKind::Down(MouseButton::Left)
        && let Some((rect, control)) = instance
            .hits
            .iter()
            .find(|(rect, _)| rect.contains(Position::new(event.column, event.row)))
    {
        instance.focus = *control;
        let code = if let Some(Control::Stepper(node, _)) = instance.controls().get(*control) {
            let session = &instance.view.as_ref().expect("stepper view").session;
            let Some(arrows) = stepper_arrows(&session.document().nodes()[*node], session, *rect)
            else {
                return;
            };
            let Some(direction) = arrows
                .iter()
                .position(|rect| rect.contains(Position::new(event.column, event.row)))
            else {
                return;
            };
            if direction == 0 {
                KeyCode::Left
            } else {
                KeyCode::Right
            }
        } else {
            KeyCode::Enter
        };
        instance.activate(code);
    }
}

fn color(tone: Tone, theme: &Theme) -> ratatui::style::Color {
    match tone {
        Tone::Normal => theme.fg,
        Tone::Muted => theme.fg_dim,
        Tone::Accent => theme.accent,
        Tone::Success => theme.success,
        Tone::Warning => theme.warning,
        Tone::Error => theme.error,
    }
}

fn paint(
    view: &View,
    width: u16,
    theme: &Theme,
    focus: Option<usize>,
) -> (Buffer, Vec<(Rect, usize)>) {
    let mut regions = node_regions(&view.session, width);
    for (index, canvas) in &view.canvases {
        let old_height = regions[*index].height;
        regions[*index].height = canvas.rows;
        for region in &mut regions[index + 1..] {
            region.y = region.y - old_height + canvas.rows;
        }
    }
    let mut target = card_buffer(
        width,
        card_height(&regions),
        view.session.document().title(),
        theme,
        if focus.is_some() {
            "Tab next · Enter press · Esc done"
        } else {
            "F2 keyboard"
        },
    );
    let hits = render_nodes(&view.session, &regions, theme, focus, true, &mut target);
    for (index, canvas) in &view.canvases {
        let region = regions[*index];
        for Cell(x, row, text, tone) in &canvas.cells {
            if *x < region.width {
                target.set_stringn(
                    region.x + x,
                    region.y + row,
                    safe(text),
                    usize::from(region.width - x),
                    Style::default().fg(color(*tone, theme)),
                );
            }
        }
    }
    (target, hits)
}

fn copy(buffer: &Buffer, target: &mut Buffer, area: Rect) {
    for y in 0..area.height.min(buffer.area.height) {
        for x in 0..area.width.min(buffer.area.width) {
            target[(area.x + x, area.y + y)] = buffer[(x, y)].clone();
        }
    }
}

/// Headless rendering uses exactly the same paint path as the popup.
pub fn preview(view: &View, width: u16, height: u16) -> Result<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    let (buffer, _) = paint(view, width, &Theme::default(), None);
    terminal.draw(|frame| copy(&buffer, frame.buffer_mut(), Rect::new(0, 0, width, height)))?;
    Ok(terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Render a validated view, or a nonfatal error panel in the shared popup chrome.
pub fn render(frame: &mut Frame, app: &mut AppView) {
    let area = chrome::popup_area(
        app,
        frame.area(),
        "terminal",
        (chrome::WIDE.0, chrome::TALL.1),
    );
    let inner = chrome::render_hinted(
        frame,
        area,
        "terminal app",
        "Tab focus · Enter activate · Esc close",
        app,
        true,
        chrome::Tone::Normal,
    );
    let Some(instance) = &mut app.tui_app else {
        return;
    };
    instance.refresh(inner.width);
    if !instance.error.is_empty() {
        instance.hits.clear();
        frame.render_widget(
            Paragraph::new(format!("Script error\n{}", safe(&instance.error)))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(app.theme.error)),
            inner,
        );
    } else if let Some(view) = &instance.view {
        let (buffer, hits) = paint(view, inner.width, &app.theme, Some(instance.focus));
        copy(&buffer, frame.buffer_mut(), inner);
        instance.hits = hits
            .into_iter()
            .filter_map(|(rect, control)| {
                let rect = Rect::new(inner.x + rect.x, inner.y + rect.y, rect.width, rect.height)
                    .intersection(inner);
                (!rect.is_empty()).then_some((rect, control))
            })
            .collect();
    }
}

#[cfg(test)]
#[path = "tui_app/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tui_app/stepper_tests.rs"]
mod stepper_tests;
