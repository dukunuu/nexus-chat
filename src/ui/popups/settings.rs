use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app::{App, SettingsRow};

use super::chrome;

/// Split "label (description)" into the two display columns.
fn split_label(label: &'static str) -> (String, String) {
    match label.split_once(" (") {
        Some((name, rest)) => (name.to_string(), rest.trim_end_matches(')').to_string()),
        None => (label.to_string(), String::new()),
    }
}

// Long by design (settings renderer).
#[allow(clippy::too_many_lines)]
pub fn render(f: &mut Frame, app: &App) {
    use crate::app::{SETTINGS_GROUPS, SettingsField};
    let area = crate::ui::centered(f.area(), 64, 60);

    let dim = Style::default().fg(app.theme.fg_dim);
    let name_w = SettingsField::ALL
        .iter()
        .map(|f| split_label(f.label()).0.chars().count())
        .max()
        .unwrap_or(0);

    let toggle = |b: bool| -> Span<'static> {
        if b {
            Span::styled("● on ", Style::default().fg(app.theme.success))
        } else {
            Span::styled("○ off", dim)
        }
    };
    let numeric = |s: &str| -> Span<'static> {
        if s.trim().is_empty() {
            Span::styled("default", dim.add_modifier(Modifier::ITALIC))
        } else {
            Span::styled(s.to_string(), Style::default().fg(app.theme.accent))
        }
    };
    let value = |field: SettingsField| -> Span<'static> {
        match field {
            SettingsField::ShowStats => toggle(app.settings.show_stats),
            SettingsField::ShowReasoning => toggle(app.settings.show_reasoning),
            SettingsField::HideHints => toggle(app.settings.hide_hints),
            SettingsField::Temperature => numeric(&app.settings_inputs[0]),
            SettingsField::TopP => numeric(&app.settings_inputs[1]),
            SettingsField::MaxTokens => numeric(&app.settings_inputs[2]),
            SettingsField::MemoryModel => numeric(&app.memory_model),
            SettingsField::CompactThreshold => numeric(&app.settings_inputs[3]),
            SettingsField::SearxngUrl => numeric(&app.settings_inputs[4]),
            SettingsField::Verbosity => {
                Span::styled(app.verbosity.clone(), Style::default().fg(app.theme.accent))
            }
            SettingsField::LangsearchKey => numeric(&app.settings_inputs[5]),
            SettingsField::SearchProvider => Span::styled(
                app.search_provider.clone(),
                Style::default().fg(app.theme.accent),
            ),
            SettingsField::TranscriberModel => numeric(&app.transcriber_model),
            SettingsField::OcrModel => numeric(&app.ocr_model),
            SettingsField::ResearchModel => numeric(&app.research_model),
            SettingsField::EscalationModel => numeric(&app.escalation_model),
            SettingsField::OcrEngine => Span::styled(
                app.ocr_engine.clone(),
                Style::default().fg(app.theme.accent),
            ),
            SettingsField::EmbeddingModel => numeric(&app.settings_inputs[6]),
            SettingsField::BlockedDomains => numeric(&app.settings_inputs[7]),
            SettingsField::ImageGenModel => numeric(&app.image_gen_model),
            SettingsField::VideoGenModel => numeric(&app.video_gen_model),
        }
    };

    let rows = app.settings_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            SettingsRow::Group(i) => {
                let g = &SETTINGS_GROUPS[*i];
                let arrow = if app.settings_collapsed.contains(i) {
                    "▸"
                } else {
                    "▾"
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{arrow} {}", g.name),
                    Style::default()
                        .fg(app.theme.accent2)
                        .add_modifier(Modifier::BOLD),
                )))
            }
            SettingsRow::Field(field) => {
                let (name, _) = split_label(field.label());
                ListItem::new(Line::from(vec![
                    Span::raw(format!("  {name:<name_w$}")),
                    Span::raw("   "),
                    value(*field),
                ]))
            }
        })
        .collect();

    let desc = match app.settings_row() {
        SettingsRow::Field(f) => split_label(f.label()).1,
        SettingsRow::Group(_) => String::new(),
    };

    let hint = format!(
        "{}Space toggles/collapses · type numbers · ↑↓ · Esc saves",
        chrome::count_hint(rows.len(), "setting")
    );
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "⚙", "nerd config"),
        &hint,
        app,
        true,
    );
    let (list_area, detail_area) = chrome::split_with_detail(inner, &desc);

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.settings_selected.min(rows.len() - 1)));
    }
    f.render_stateful_widget(list, list_area, &mut state);
    chrome::render_detail(f, detail_area, &desc, &app.theme);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use crate::app::SettingsField;
    // The memory-model and transcriber-model rows are picked, not typed:
    // Enter opens the same model picker /model uses, Backspace clears it.
    let picker = matches!(
        app.settings_field(),
        Some(
            SettingsField::MemoryModel
                | SettingsField::TranscriberModel
                | SettingsField::OcrModel
                | SettingsField::ResearchModel
                | SettingsField::EscalationModel
                | SettingsField::ImageGenModel
                | SettingsField::VideoGenModel
        )
    );
    if picker {
        match key.code {
            KeyCode::Esc => app.save_settings()?,
            KeyCode::Enter => match app.settings_field() {
                Some(SettingsField::MemoryModel) => app.open_model_picker_for_memory(),
                Some(SettingsField::OcrModel) => app.open_model_picker_for_ocr(),
                Some(SettingsField::ResearchModel) => app.open_model_picker_for_research(),
                Some(SettingsField::EscalationModel) => app.open_model_picker_for_escalation(),
                Some(SettingsField::ImageGenModel) => app.open_model_picker_for_image_gen(),
                Some(SettingsField::VideoGenModel) => app.open_model_picker_for_video_gen(),
                _ => app.open_model_picker_for_transcriber(),
            },
            KeyCode::Backspace => match app.settings_field() {
                Some(SettingsField::MemoryModel) => app.clear_memory_model()?,
                Some(SettingsField::OcrModel) => app.clear_ocr_model()?,
                Some(SettingsField::ResearchModel) => app.clear_research_model()?,
                Some(SettingsField::EscalationModel) => app.clear_escalation_model()?,
                Some(SettingsField::ImageGenModel) => app.clear_image_gen_model()?,
                Some(SettingsField::VideoGenModel) => app.clear_video_gen_model()?,
                _ => app.clear_transcriber_model()?,
            },
            KeyCode::Up => app.move_settings_selection(-1),
            KeyCode::Down | KeyCode::Tab => app.move_settings_selection(1),
            _ => {}
        }
        return Ok(());
    }
    match key.code {
        // Enter on a group header toggles it; on a field it's a no-op here
        // (fields with an Enter action are handled by the `picker` branch above).
        KeyCode::Enter if matches!(app.settings_row(), SettingsRow::Group(_)) => {
            app.toggle_settings_field();
        }
        KeyCode::Esc | KeyCode::Enter => app.save_settings()?,
        KeyCode::Up => app.move_settings_selection(-1),
        KeyCode::Down | KeyCode::Tab => app.move_settings_selection(1),
        KeyCode::Char(' ') => app.toggle_settings_field(),
        KeyCode::Char(c) => app.settings_input_char(c),
        KeyCode::Backspace => app.settings_input_backspace(),
        _ => {}
    }
    Ok(())
}
