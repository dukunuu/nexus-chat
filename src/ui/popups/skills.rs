use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::App;

use super::chrome;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SkillsMode;
    let area = crate::ui::centered(f.area(), 56, 50);

    let items: Vec<ListItem> = app
        .skills
        .iter()
        .map(|s| {
            ListItem::new(Line::from(Span::styled(
                s.name.clone(),
                Style::default().fg(app.theme.fg),
            )))
        })
        .collect();

    let title = match app.skills_mode {
        SkillsMode::Install => chrome::input_title(
            app,
            "install",
            &app.skills_edit,
            "owner/repo/path · Enter install · Esc cancel",
        ),
        SkillsMode::ConfirmRemove => {
            let name = app
                .skills
                .get(app.skills_selected)
                .map(|s| s.name.as_str())
                .unwrap_or("");
            chrome::confirm_title(
                app,
                format!("remove \"{name}\"?"),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        SkillsMode::Browse => chrome::hinted_title(
            app,
            "skills",
            "Ctrl+N install from GitHub · Ctrl+D remove · Ctrl+E edit",
        ),
    };

    let inner = chrome::render_frame(f, area, title, &app.theme, true);
    let desc = app
        .skills
        .get(app.skills_selected)
        .map(|s| s.description.as_str())
        .unwrap_or("");
    let (list_area, detail_area) = chrome::split_with_detail(inner, desc);

    let list = chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.skills.is_empty() {
        state.select(Some(app.skills_selected.min(app.skills.len() - 1)));
    }
    f.render_stateful_widget(list, list_area, &mut state);
    chrome::render_detail(f, detail_area, desc, &app.theme);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
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
