use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, SwarmPopupMode};

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 78, 66);

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = if app.swarm_cache.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "no personas yet — press Ctrl+N to add one, or just send a message and 3 will be suggested for you",
            dim,
        )))]
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
        SwarmPopupMode::Browse => chrome::hinted_title(
            app,
            format!("swarm mode is {}", if on { "ON" } else { "OFF" }),
            "Enter/Ctrl+E edit in $EDITOR · Ctrl+N add in $EDITOR · Ctrl+G toggle · Ctrl+M model picker · Ctrl+D remove · Ctrl+X stop",
        ),
    };

    let inner = chrome::render_frame(f, area, title, &app.theme, true);
    let list = chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.swarm_cache.is_empty() {
        state.select(Some(app.swarm_selected.min(app.swarm_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
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
                    app.queue_swarm_persona_editor(false)?
                }
                KeyCode::Char('e') if ctrl && !app.swarm_cache.is_empty() => {
                    app.queue_swarm_persona_editor(false)?
                }
                KeyCode::Char('x') if ctrl && app.swarm_rx.is_some() => app.stop_swarm(),
                KeyCode::Char('d') if ctrl && !app.swarm_cache.is_empty() => {
                    app.swarm_popup_mode = SwarmPopupMode::ConfirmDelete
                }
                _ => {}
            }
        }
    }
    Ok(())
}
