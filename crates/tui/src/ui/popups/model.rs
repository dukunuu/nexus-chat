use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, ListState};

use crate::app_view::AppView;
use crate::ui::popups::chrome;
use nexus_core::app::{ModelPanel, Popup};
use nexus_core::provider::Model;

pub fn render(f: &mut Frame, app: &mut AppView) {
    let (fav_outer, avail_outer) = model_popup_areas(f.area());

    let fav_focused = app.model_focus == ModelPanel::Favorites;
    // Short title + the pick target as a footer hint, so the model picker
    // reads the same from any feature that opens it.
    let picking = match app.model_pick_target {
        nexus_core::app::ModelPickTarget::Memory => "picking memory model",
        nexus_core::app::ModelPickTarget::Transcriber => "picking image model",
        nexus_core::app::ModelPickTarget::Ocr => "picking OCR model",
        nexus_core::app::ModelPickTarget::Session => "picking chat model",
        nexus_core::app::ModelPickTarget::SwarmPersona(_) => "picking persona model",
        nexus_core::app::ModelPickTarget::ImageGen => "picking image gen model",
        nexus_core::app::ModelPickTarget::VideoGen => "picking video gen model",
    };
    let fav_title = chrome::popup_title(app, "★", "favorites");
    let fav_hint = if app.favorite_models().is_empty() {
        "no favorites yet — Ctrl+S in the available list".to_string()
    } else {
        format!(
            "{}↑↓ · Ctrl+S unfav · Ctrl+T reason",
            chrome::count_hint(app.favorite_models().len(), "favorite")
        )
    };

    // Favorites column. Each panel draws its frame through the shared chrome
    // (so the border sits on the same rect the mouse hit-tests) and sizes
    // its rows to its own width.
    let theme = app.theme;
    let fav_inner = chrome::render_hinted(
        f,
        fav_outer,
        fav_title,
        &fav_hint,
        app,
        fav_focused,
        chrome::Tone::Normal,
    );
    let fav_items = model_items(app, &app.favorite_models(), row_width(fav_inner));
    let fav_total = app.favorite_models().len();
    // Core keeps selection as plain indices; the widget state is render-local.
    let mut fav_state = ListState::default();
    fav_state.select(Some(app.fav_selected));
    chrome::render_list(
        f,
        chrome::standard_list(fav_items, &theme),
        &mut fav_state,
        fav_inner,
        fav_total,
        1,
        app,
    );
    // The widget's scroll offset is render state; stash it for click mapping.
    app.fav_offset = fav_state.offset();

    // Available column (with the search box in the title).
    let backend = app.model_backend_filter_label();
    // Show which effort values the focused model accepts, so the Ctrl+T
    // cycle is predictable before pressing it (e.g. Claude's extra minimal).
    let hint = match app.focused_reasoning_hint() {
        Some(accepts) => accepts.clone(),
        None => String::new(),
    };
    let avail_hint = if app.available_models().is_empty() {
        "no models match — type to clear the filter, Ctrl+P to switch backend".to_string()
    } else {
        format!(
            "{}type to search · Ctrl+P backend · Ctrl+S fav · Ctrl+T reason · {hint}",
            chrome::count_hint(app.available_models().len(), "model")
        )
    };
    let avail_title = chrome::filter_title(
        app,
        "▦",
        format!("available [{backend}] — {picking}"),
        &app.model_filter,
    );
    let avail_inner = chrome::render_hinted(
        f,
        avail_outer,
        avail_title,
        &avail_hint,
        app,
        !fav_focused,
        chrome::Tone::Normal,
    );
    let avail_items = model_items(app, &app.available_models(), row_width(avail_inner));
    let avail_total = app.available_models().len();
    let mut avail_state = ListState::default();
    avail_state.select(Some(app.avail_selected));
    chrome::render_list(
        f,
        chrome::standard_list(avail_items, &theme),
        &mut avail_state,
        avail_inner,
        avail_total,
        1,
        app,
    );
    app.avail_offset = avail_state.offset();
}

/// Row text width inside a panel: the inner rect minus the scrollbar gutter
/// and the `▸ ` highlight symbol.
fn row_width(inner: Rect) -> usize {
    inner.width.saturating_sub(3) as usize
}

/// Trailing columns every model row reserves: the vision glyph (" ⊡") and
/// the context window (" {:>6}" — `humanize` yields up to six chars, 131.1k).
const VISION_W: usize = 2;
const CTX_W: usize = 7;

fn model_items(app: &AppView, models: &[&Model], width: usize) -> Vec<ListItem<'static>> {
    models
        .iter()
        .map(|m| {
            let mut id = nexus_core::app::composite_id(m);
            let marker = if app.favorites.contains(&id) {
                "★ "
            } else if app.last_used.contains_key(&id) {
                "• "
            } else {
                "  "
            };
            // Reasoning badge: [r:high] if set, [r] if supported but off.
            let badge = match app.reasoning_of(&id) {
                Some("none") => "  [r:off]".to_string(),
                Some(effort) => format!("  [r:{effort}]"),
                None if !m.reasoning_efforts.is_empty() => "  [r]".to_string(),
                None => String::new(),
            };
            // Every row reserves the same trailing columns — vision glyph,
            // then the context window — so they line up right-aligned.
            let name_w = width.saturating_sub(VISION_W + CTX_W);
            id = chrome::truncate(&id, name_w.saturating_sub(2 + badge.chars().count()));
            let name = format!("{marker}{id}{badge}");
            let pad = name_w.saturating_sub(name.chars().count());
            let dim = Style::default().fg(app.theme.fg_dim);
            let vision = if m.supports_images { " ⊡" } else { "  " };
            let ctx = m.context_length.map_or_else(
                || " ".repeat(CTX_W),
                |c| format!(" {:>6}", crate::ui::humanize(c)),
            );
            let spans = vec![
                Span::raw(format!("{name}{}", " ".repeat(pad))),
                Span::styled(vision, dim),
                Span::styled(ctx, dim),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect()
}

/// Outer rects of the model picker's two columns (Favorites, Available).
/// Shared by the renderer and the mouse hit-tester so they always agree.
pub fn model_popup_areas(screen: Rect) -> (Rect, Rect) {
    let outer = crate::ui::centered(screen, 82, 74);
    let cols =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).split(outer);
    (cols[0], cols[1])
}

/// The clickable list area inside a bordered popup column.
pub fn list_inner(outer: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(outer)
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Ctrl+P narrows noisy provider catalogs; Ctrl+S favorites; Ctrl+T cycles reasoning effort.
        KeyCode::Char('p') if ctrl => app.cycle_model_backend_filter(),
        KeyCode::Char('s') if ctrl => app.toggle_favorite_focused()?,
        KeyCode::Char('t') if ctrl => app.cycle_reasoning_focused()?,
        // Cancelling a memory-model pick returns to /config, same as picking one.
        KeyCode::Esc => {
            app.popup = match app.model_pick_target {
                nexus_core::app::ModelPickTarget::Memory
                | nexus_core::app::ModelPickTarget::Transcriber
                | nexus_core::app::ModelPickTarget::Ocr
                | nexus_core::app::ModelPickTarget::ImageGen
                | nexus_core::app::ModelPickTarget::VideoGen => Popup::Settings,
                nexus_core::app::ModelPickTarget::SwarmPersona(_) => Popup::Swarm,
                nexus_core::app::ModelPickTarget::Session => Popup::None,
            };
        }
        KeyCode::Enter => app.confirm_model()?,
        // Tab now owns panel-switching — Left/Right/Home/End/Delete/Ctrl+A/C/X/V
        // are claimed by the filter box below (cursor move, select, clipboard).
        KeyCode::Tab => app.toggle_model_focus(),
        KeyCode::Up => app.move_model_selection(-1),
        KeyCode::Down => app.move_model_selection(1),
        KeyCode::Backspace => {
            app.model_filter.backspace();
            app.reset_model_selection();
        }
        KeyCode::Char(c) if !ctrl => {
            app.model_filter.insert_char(c);
            app.reset_model_selection();
        }
        _ if app.model_filter.key(key, &mut app.clipboard) => {}
        _ => {}
    }
    Ok(())
}
