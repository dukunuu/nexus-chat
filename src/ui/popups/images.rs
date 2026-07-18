use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, ImagesMode};

pub(crate) fn render(f: &mut Frame, app: &App) {
    let area = crate::ui::centered(f.area(), 64, 60);
    let dim = Style::default().fg(app.theme.fg_dim);

    let items: Vec<ListItem> = app
        .images_cache
        .iter()
        .map(|img| {
            let created = crate::ui::fmt_created(&img.modified);
            ListItem::new(Line::from(vec![
                Span::styled(img.name.clone(), Style::default().fg(app.theme.fg)),
                Span::styled(format!("  {}", crate::app::human_size(img.size as i64)), dim),
                Span::styled(format!("  {created}"), dim),
            ]))
        })
        .collect();

    let title = match app.images_mode {
        ImagesMode::ConfirmDelete => {
            let name = app
                .images_cache
                .get(app.images_selected)
                .map(|i| i.name.clone())
                .unwrap_or_default();
            super::chrome::danger_title(
                app,
                format!("remove \"{name}\"?"),
                "Ctrl+D confirm · Esc cancel",
            )
        }
        ImagesMode::Browse => super::chrome::hinted_title(
            app,
            "images",
            "Enter open · Ctrl+D remove",
        ),
    };

    let inner = super::chrome::render_frame(f, area, title, &app.theme, true);
    let list = super::chrome::standard_list(items);
    let mut state = ListState::default();
    if !app.images_cache.is_empty() {
        state.select(Some(app.images_selected.min(app.images_cache.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, classify_browse_key, classify_confirm_delete_key,
    };
    match app.images_mode {
        ImagesMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_images_delete()?,
            Some(ConfirmDeleteAction::No) => app.images_mode = ImagesMode::Browse,
            None => {}
        },
        ImagesMode::Browse => {
            if key.code == KeyCode::Enter && !app.images_cache.is_empty() {
                app.open_selected_image();
                return Ok(());
            }
            match classify_browse_key(key, false, false) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_images_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_images_selection(1),
                Some(BrowseAction::ConfirmDelete) if !app.images_cache.is_empty() => {
                    app.images_mode = ImagesMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
