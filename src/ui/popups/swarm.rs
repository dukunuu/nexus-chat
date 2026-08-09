use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, SwarmPopupMode};

use super::chrome;

pub fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::WIDE.1);

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = if app.swarm_cache.is_empty() {
        vec![chrome::empty_placeholder(
            "no personas yet — Ctrl+N to add one, or send a message and 3 will be suggested",
            &app.theme,
        )]
    } else {
        app.swarm_cache
            .iter()
            .map(|p| {
                let blurb: String = p.blurb.chars().take(60).collect();
                ListItem::new(Line::from(vec![
                    Span::styled(p.name.clone(), Style::default().fg(app.theme.fg)),
                    Span::styled(format!("  {}", p.model), dim),
                    Span::styled(format!("  {blurb}"), dim),
                ]))
            })
            .collect()
    };

    let on = app.session.as_ref().is_some_and(|s| s.swarm_mode);
    let title = match app.swarm_popup_mode {
        SwarmPopupMode::ConfirmDelete => {
            let name = app
                .swarm_cache
                .get(app.swarm_selected)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            chrome::danger_title(
                app,
                format!("remove persona \"{name}\"?"),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        SwarmPopupMode::Browse => chrome::popup_title(
            app,
            "👥",
            format!("swarm — {}", if on { "ON" } else { "OFF" }),
        ),
    };
    let hint = match app.swarm_popup_mode {
        SwarmPopupMode::ConfirmDelete => "Ctrl+D confirm · Esc cancel".to_string(),
        SwarmPopupMode::Browse if app.swarm_cache.is_empty() => {
            "Ctrl+N add · Ctrl+G toggle · Ctrl+X stop".to_string()
        }
        SwarmPopupMode::Browse => format!(
            "{}↑↓ · Enter edit · Ctrl+N add · Ctrl+G toggle · Ctrl+M model · Ctrl+D remove · Ctrl+X stop",
            chrome::count_hint(app.swarm_cache.len(), "persona")
        ),
    };
    let tone = if app.swarm_popup_mode == SwarmPopupMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_hinted(f, area, title, &hint, app, true, tone);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.swarm_cache.is_empty() {
        state.select(Some(app.swarm_selected.min(app.swarm_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{ConfirmDeleteAction, classify_confirm_delete_key};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match app.swarm_popup_mode {
        SwarmPopupMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.swarm_remove_row()?,
            Some(ConfirmDeleteAction::No) => app.swarm_popup_mode = SwarmPopupMode::Browse,
            None => {}
        },
        SwarmPopupMode::Browse => {
            if key.code == KeyCode::Char('m') && ctrl {
                if !app.swarm_cache.is_empty() {
                    app.open_model_picker_for_swarm_persona(app.swarm_selected);
                }
                return Ok(());
            }
            match key.code {
                KeyCode::Esc => app.popup = crate::app::Popup::None,
                KeyCode::Up => app.move_swarm_selection(-1),
                KeyCode::Down => app.move_swarm_selection(1),
                KeyCode::Char('g') if ctrl => app.toggle_swarm_mode()?,
                KeyCode::Char('n') if ctrl => app.queue_swarm_persona_editor(true)?,
                KeyCode::Enter if !app.swarm_cache.is_empty() => {
                    app.queue_swarm_persona_editor(false)?;
                }
                KeyCode::Char('e') if ctrl && !app.swarm_cache.is_empty() => {
                    app.queue_swarm_persona_editor(false)?;
                }
                KeyCode::Char('x') if ctrl && app.swarm_rx.is_some() => app.stop_swarm(),
                KeyCode::Char('d') if ctrl && !app.swarm_cache.is_empty() => {
                    app.swarm_popup_mode = SwarmPopupMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
