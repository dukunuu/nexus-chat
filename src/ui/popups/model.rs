use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

use crate::app::{App, ModelPanel, Popup};
use crate::provider::Model;

pub(crate) fn render(f: &mut Frame, app: &mut App) {
    let (fav_outer, avail_outer) = model_popup_areas(f.area());
    f.render_widget(Clear, fav_outer);
    f.render_widget(Clear, avail_outer);

    let fav_focused = app.model_focus == ModelPanel::Favorites;
    let fav_title = match app.model_pick_target {
        crate::app::ModelPickTarget::Memory => " ★ Favorites — picking memory model ",
        crate::app::ModelPickTarget::Transcriber => " ★ Favorites — picking transcriber model ",
        crate::app::ModelPickTarget::Session => " ★ Favorites ",
    };

    // Favorites column.
    let fav_items = model_items(app, &app.favorite_models());
    let fav_list = panel_list(fav_items, fav_title, fav_focused);
    f.render_stateful_widget(fav_list, fav_outer, &mut app.fav_state);

    // Available column (with the search box in the title).
    let avail_items = model_items(app, &app.available_models());
    let title = if app.settings.hide_hints {
        format!(" Available — search: {}▏ ", app.model_filter)
    } else {
        format!(
            " Available — search: {}▏  (Ctrl+S fav · Ctrl+T reason) ",
            app.model_filter
        )
    };
    let avail_list = panel_list(avail_items, &title, !fav_focused);
    f.render_stateful_widget(avail_list, avail_outer, &mut app.avail_state);
}

fn model_items(app: &App, models: &[&Model]) -> Vec<ListItem<'static>> {
    models
        .iter()
        .map(|m| {
            let marker = if app.favorites.contains(&m.id) {
                "★ "
            } else if app.last_used.contains_key(&m.id) {
                "• "
            } else {
                "  "
            };
            // Reasoning badge: [r:high] if set, [r] if supported but off.
            let badge = match app.reasoning_of(&m.id) {
                Some(effort) => format!("  [r:{effort}]"),
                None if m.supports_reasoning => "  [r]".to_string(),
                None => String::new(),
            };
            ListItem::new(format!("{marker}{}{badge}", m.id))
        })
        .collect()
}

fn panel_list<'a>(items: Vec<ListItem<'a>>, title: &str, focused: bool) -> List<'a> {
    let border = if focused { Color::Cyan } else { Color::DarkGray };
    List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title.to_string()),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ")
}

/// Outer rects of the model picker's two columns (Favorites, Available).
/// Shared by the renderer and the mouse hit-tester so they always agree.
pub(crate) fn model_popup_areas(screen: Rect) -> (Rect, Rect) {
    let outer = crate::ui::centered(screen, 82, 74);
    let cols = Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)])
        .split(outer);
    (cols[0], cols[1])
}

/// The clickable list area inside a bordered popup column.
pub(crate) fn list_inner(outer: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(outer)
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Ctrl+S favorites the focused model; Ctrl+T cycles reasoning effort.
        KeyCode::Char('s') if ctrl => app.toggle_favorite_focused()?,
        KeyCode::Char('t') if ctrl => app.cycle_reasoning_focused()?,
        // Cancelling a memory-model pick returns to /config, same as picking one.
        KeyCode::Esc => {
            app.popup = match app.model_pick_target {
                crate::app::ModelPickTarget::Memory | crate::app::ModelPickTarget::Transcriber => Popup::Settings,
                crate::app::ModelPickTarget::Session => Popup::None,
            };
        }
        KeyCode::Enter => app.confirm_model()?,
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => app.toggle_model_focus(),
        KeyCode::Up => app.move_model_selection(-1),
        KeyCode::Down => app.move_model_selection(1),
        KeyCode::Backspace => {
            app.model_filter.pop();
            app.reset_model_selection();
        }
        // Plain characters build the search filter; modified ones are ignored.
        KeyCode::Char(c) if !ctrl => {
            app.model_filter.push(c);
            app.reset_model_selection();
        }
        _ => {}
    }
    Ok(())
}
