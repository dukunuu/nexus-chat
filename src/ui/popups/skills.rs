use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SkillsMode;
    let area = crate::ui::centered(f.area(), 56, 50);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = app
        .skills
        .iter()
        .map(|s| {
            ListItem::new(Line::from(vec![
                Span::styled(s.name.clone(), Style::default().fg(Color::White)),
                Span::styled(format!("  {}", s.description), dim),
            ]))
        })
        .collect();

    let title = match app.skills_mode {
        SkillsMode::Install => {
            format!(" install: {}▏  owner/repo/path  (Enter install · Esc cancel) ", app.skills_edit)
        }
        SkillsMode::ConfirmRemove => {
            let name = app.skills.get(app.skills_selected).map(|s| s.name.as_str()).unwrap_or("");
            format!(" remove \"{name}\"? (Ctrl+D confirm · Esc cancel) ")
        }
        SkillsMode::Browse => {
            let keys = if app.settings.hide_hints {
                ""
            } else {
                "  (Ctrl+N install from GitHub · Ctrl+D remove · Ctrl+E edit)"
            };
            format!(" skills{keys} ")
        }
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.skills.is_empty() {
        state.select(Some(app.skills_selected.min(app.skills.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    use super::{ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key, classify_edit_key};
    use crate::app::SkillsMode;
    match app.skills_mode {
        SkillsMode::Install => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.skills_mode = SkillsMode::Browse,
            Some(EditAction::Save) => app.confirm_skill_install(),
            Some(EditAction::Backspace) => {
                app.skills_edit.pop();
            }
            Some(EditAction::Push(c)) => app.skills_edit.push(c),
            None => {}
        },
        SkillsMode::ConfirmRemove => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_skill_remove(),
            Some(ConfirmDeleteAction::No) => app.skills_mode = SkillsMode::Browse,
            None => {}
        },
        // skills' Browse mode has no Enter binding, and (unlike session/space)
        // no text filter — Ctrl+R rename is unsupported and plain
        // chars/Backspace are intentionally left as no-ops, matching the
        // original match arm's fallthrough to `_ => {}`.
        SkillsMode::Browse => match classify_browse_key(key, true, false) {
            Some(super::BrowseAction::Close) => app.popup = crate::app::Popup::None,
            Some(super::BrowseAction::MoveUp) => app.move_skills_selection(-1),
            Some(super::BrowseAction::MoveDown) => app.move_skills_selection(1),
            Some(super::BrowseAction::Create) => app.start_skill_install(),
            Some(super::BrowseAction::ConfirmDelete) => app.start_skill_remove(),
            _ => {}
        },
    }
}
