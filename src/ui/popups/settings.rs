use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::SettingsField;
    use ratatui::widgets::BorderType;
    let area = crate::ui::centered(f.area(), 60, 46);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
    // Split "label (description)" into the two display columns.
    let split = |label: &'static str| -> (String, String) {
        match label.split_once(" (") {
            Some((name, rest)) => (name.to_string(), rest.trim_end_matches(')').to_string()),
            None => (label.to_string(), String::new()),
        }
    };
    let name_w = SettingsField::ALL
        .iter()
        .map(|f| split(f.label()).0.chars().count())
        .max()
        .unwrap_or(0);

    // The value cell: a pill for toggles, the typed number (or "default") otherwise.
    let toggle = |b: bool| -> Span<'static> {
        if b {
            Span::styled("● on ", Style::default().fg(Color::Green))
        } else {
            Span::styled("○ off", dim)
        }
    };
    let numeric = |s: &str| -> Span<'static> {
        if s.trim().is_empty() {
            Span::styled("default", dim.add_modifier(Modifier::ITALIC))
        } else {
            Span::styled(s.to_string(), Style::default().fg(Color::Cyan))
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
            SettingsField::Verbosity => Span::styled(app.verbosity.clone(), Style::default().fg(Color::Cyan)),
            SettingsField::LangsearchKey => numeric(&app.settings_inputs[5]),
            SettingsField::SearchProvider => {
                Span::styled(app.search_provider.clone(), Style::default().fg(Color::Cyan))
            }
        }
    };

    let items: Vec<ListItem> = SettingsField::ALL
        .iter()
        .map(|f| {
            let (name, desc) = split(f.label());
            let mut spans = vec![
                Span::styled(format!("{name:<name_w$}"), Style::default().fg(Color::White)),
                Span::raw("   "),
                value(*f),
            ];
            if !desc.is_empty() {
                spans.push(Span::styled(format!("   {desc}"), dim));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title(Span::styled(
                    crate::ui::hint_title(app, " nerd config ", "nerd config — Space toggles · type numbers · Esc saves"),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
        )
        // No inverse bar — a cyan arrow + bold row keeps the value colors readable.
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.settings_selected));
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use crate::app::SettingsField;
    // The memory-model row is picked, not typed: Enter opens the same model
    // picker /model uses, Backspace clears it (disabling extraction).
    if app.settings_field() == SettingsField::MemoryModel {
        match key.code {
            KeyCode::Esc => app.save_settings()?,
            KeyCode::Enter => app.open_model_picker_for_memory(),
            KeyCode::Backspace => app.clear_memory_model()?,
            KeyCode::Up => app.move_settings_selection(-1),
            KeyCode::Down | KeyCode::Tab => app.move_settings_selection(1),
            _ => {}
        }
        return Ok(());
    }
    match key.code {
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
