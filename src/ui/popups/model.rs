use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

use crate::app::{App, ModelPanel, Popup};
use crate::provider::Model;
use crate::ui::popups::chrome;

pub(crate) fn render(f: &mut Frame, app: &mut App) {
    let (fav_outer, avail_outer) = model_popup_areas(f.area());
    f.render_widget(Clear, fav_outer);
    f.render_widget(Clear, avail_outer);

    let fav_focused = app.model_focus == ModelPanel::Favorites;
    let fav_title = match app.model_pick_target {
        crate::app::ModelPickTarget::Memory => {
            chrome::hinted_title(app, "★ Favorites — picking memory model", "")
        }
        crate::app::ModelPickTarget::Transcriber => {
            chrome::hinted_title(app, "★ Favorites — picking image model", "")
        }
        crate::app::ModelPickTarget::Ocr => {
            chrome::hinted_title(app, "★ Favorites — picking OCR model", "")
        }
        crate::app::ModelPickTarget::Research => {
            chrome::hinted_title(app, "★ Favorites — picking research model", "")
        }
        crate::app::ModelPickTarget::Escalation => {
            chrome::hinted_title(app, "★ Favorites — picking escalation model", "")
        }
        crate::app::ModelPickTarget::Session => chrome::hinted_title(app, "★ Favorites", ""),
        crate::app::ModelPickTarget::SwarmPersona(_) => {
            chrome::hinted_title(app, "★ Favorites — picking persona model", "")
        }
        crate::app::ModelPickTarget::ImageGen => {
            chrome::hinted_title(app, "★ Favorites — picking image gen model", "")
        }
        crate::app::ModelPickTarget::VideoGen => {
            chrome::hinted_title(app, "★ Favorites — picking video gen model", "")
        }
    };

    // Favorites column.
    let fav_items = model_items(app, &app.favorite_models());
    let fav_list = panel_list(fav_items, fav_title, fav_focused, &app.theme);
    f.render_stateful_widget(fav_list, fav_outer, &mut app.fav_state);

    // Available column (with the search box in the title).
    let avail_items = model_items(app, &app.available_models());
    let backend = app.model_backend_filter_label();
    // Show which effort values the focused model accepts, so the Ctrl+T
    // cycle is predictable before pressing it (e.g. Claude's extra minimal).
    let hint = match app.focused_reasoning_hint() {
        Some(accepts) => format!("Ctrl+P switch backend · Ctrl+S fav · Ctrl+T reason · {accepts}"),
        None => "Ctrl+P switch backend · Ctrl+S fav · Ctrl+T reason".to_string(),
    };
    let avail_list = panel_list(
        avail_items,
        chrome::input_title(
            app,
            format!("Available [{backend}] search"),
            app.model_filter.to_string(),
            &hint,
        ),
        !fav_focused,
        &app.theme,
    );
    f.render_stateful_widget(avail_list, avail_outer, &mut app.avail_state);
}

fn model_items(app: &App, models: &[&Model]) -> Vec<ListItem<'static>> {
    models
        .iter()
        .map(|m| {
            let id = crate::app::composite_id(m);
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
            let mut spans = vec![Span::raw(format!("{marker}{id}{badge}"))];
            // Vision glyph (dim) for models with image support.
            if m.supports_images {
                spans.push(Span::styled(" ⊡", Style::default().fg(app.theme.fg_dim)));
            }
            // Context window (dim, right-aligned) so the available size is
            // visible before picking — OpenCode Zen models included.
            if let Some(ctx) = m.context_length {
                spans.push(Span::styled(
                    format!(" {:>5}", crate::ui::humanize(ctx)),
                    Style::default().fg(app.theme.fg_dim),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect()
}

fn panel_list<'a>(
    items: Vec<ListItem<'a>>,
    title: Line<'a>,
    focused: bool,
    theme: &crate::theme::Theme,
) -> List<'a> {
    chrome::standard_list(items).block(chrome::popup_block_focused(title, theme, focused))
}

/// Outer rects of the model picker's two columns (Favorites, Available).
/// Shared by the renderer and the mouse hit-tester so they always agree.
pub(crate) fn model_popup_areas(screen: Rect) -> (Rect, Rect) {
    let outer = crate::ui::centered(screen, 82, 74);
    let cols =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).split(outer);
    (cols[0], cols[1])
}

/// The clickable list area inside a bordered popup column.
pub(crate) fn list_inner(outer: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(outer)
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Ctrl+P narrows noisy provider catalogs; Ctrl+S favorites; Ctrl+T cycles reasoning effort.
        KeyCode::Char('p') if ctrl => app.cycle_model_backend_filter(),
        KeyCode::Char('s') if ctrl => app.toggle_favorite_focused()?,
        KeyCode::Char('t') if ctrl => app.cycle_reasoning_focused()?,
        // Cancelling a memory-model pick returns to /config, same as picking one.
        KeyCode::Esc => {
            app.popup = match app.model_pick_target {
                crate::app::ModelPickTarget::Memory
                | crate::app::ModelPickTarget::Transcriber
                | crate::app::ModelPickTarget::Ocr
                | crate::app::ModelPickTarget::Research
                | crate::app::ModelPickTarget::Escalation
                | crate::app::ModelPickTarget::ImageGen
                | crate::app::ModelPickTarget::VideoGen => Popup::Settings,
                crate::app::ModelPickTarget::SwarmPersona(_) => Popup::Swarm,
                crate::app::ModelPickTarget::Session => Popup::None,
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
