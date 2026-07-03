use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    use crate::app::SkillsMode;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match app.skills_mode {
        SkillsMode::Install => match key.code {
            KeyCode::Esc => app.skills_mode = SkillsMode::Browse,
            KeyCode::Enter => app.confirm_skill_install(),
            KeyCode::Backspace => {
                app.skills_edit.pop();
            }
            KeyCode::Char(c) => app.skills_edit.push(c),
            _ => {}
        },
        SkillsMode::ConfirmRemove => match key.code {
            KeyCode::Char('d') if ctrl => app.confirm_skill_remove(),
            KeyCode::Esc => app.skills_mode = SkillsMode::Browse,
            _ => {}
        },
        SkillsMode::Browse => match key.code {
            KeyCode::Esc => app.popup = crate::app::Popup::None,
            KeyCode::Up => app.move_skills_selection(-1),
            KeyCode::Down => app.move_skills_selection(1),
            KeyCode::Char('n') if ctrl => app.start_skill_install(),
            KeyCode::Char('d') if ctrl => app.start_skill_remove(),
            _ => {}
        },
    }
}
