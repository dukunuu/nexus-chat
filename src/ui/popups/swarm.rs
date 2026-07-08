use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState};

use crate::app::{App, SwarmPopupMode};

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 78, 66);
    f.render_widget(Clear, area);

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
            format!(" remove persona \"{name}\"? (Ctrl+D confirm · Esc cancel) ")
        }
        SwarmPopupMode::EditName => " persona name — Enter save · Esc cancel ".to_string(),
        SwarmPopupMode::EditBlurb => " persona blurb — Enter save · Esc cancel ".to_string(),
        SwarmPopupMode::Browse => format!(
            " swarm mode is {} — Ctrl+G turns it on/off · Ctrl+N adds a persona · \
             Ctrl+R renames · Ctrl+B edits the blurb · Ctrl+M sets the model · \
             Ctrl+D removes ",
            if on { "ON" } else { "OFF" }
        ),
    };

    let list = List::new(items)
        .block(chrome::popup_block(title, &app.theme))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.swarm_cache.is_empty() {
        state.select(Some(app.swarm_selected.min(app.swarm_cache.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);

    if matches!(
        app.swarm_popup_mode,
        SwarmPopupMode::EditName | SwarmPopupMode::EditBlurb
    ) {
        let edit_area = crate::ui::centered(f.area(), 60, 3);
        f.render_widget(Clear, edit_area);
        let label = match app.swarm_popup_mode {
            SwarmPopupMode::EditName => " name ",
            _ => " blurb ",
        };
        let p = ratatui::widgets::Paragraph::new(app.swarm_edit.as_str())
            .block(chrome::popup_block(label.to_string(), &app.theme));
        f.render_widget(p, edit_area);
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{ConfirmDeleteAction, EditAction, classify_confirm_delete_key, classify_edit_key};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match app.swarm_popup_mode {
        SwarmPopupMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.swarm_remove_row()?,
            Some(ConfirmDeleteAction::No) => app.swarm_popup_mode = SwarmPopupMode::Browse,
            None => {}
        },
        SwarmPopupMode::EditName | SwarmPopupMode::EditBlurb => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.swarm_cancel_edit()?,
            Some(EditAction::Save) => app.swarm_confirm_edit()?,
            Some(EditAction::Backspace) => {
                app.swarm_edit.pop();
            }
            Some(EditAction::Push(c)) => app.swarm_edit.push(c),
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
                KeyCode::Char('n') if ctrl => app.swarm_add_row(),
                KeyCode::Char('r') if ctrl && !app.swarm_cache.is_empty() => {
                    app.swarm_start_edit_name()
                }
                KeyCode::Char('b') if ctrl && !app.swarm_cache.is_empty() => {
                    app.swarm_start_edit_blurb()
                }
                KeyCode::Char('d') if ctrl && !app.swarm_cache.is_empty() => {
                    app.swarm_popup_mode = SwarmPopupMode::ConfirmDelete
                }
                _ => {}
            }
        }
    }
    Ok(())
}
