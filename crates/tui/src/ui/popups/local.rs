//! `/local`'s runtime selector: Ollama, MLX, LM Studio, or off. Unlike
//! `/login` there is no key to paste — picking a row writes the
//! machine-local `[local]` block and reloads the catalog. Custom endpoints
//! and discovery commands stay a config-file (or `/local <runtime> <url>`)
//! concern; this popup only switches runtimes.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use nexus_core::provider::local::{LocalConfig, LocalRuntime};

use crate::app_view::AppView;

use super::chrome;

/// The runtime rows, plus a trailing "off" row.
pub const ROW_COUNT: usize = LocalRuntime::ALL.len() + 1;

/// One row: label, the discovery command picking it runs, the endpoint it
/// would talk to, and whether it's the active runtime. Both hints are shown
/// because either can be the reason a pick doesn't work.
fn row(index: usize, current: Option<&LocalConfig>) -> (String, String, String, bool) {
    match LocalRuntime::ALL.get(index) {
        Some(&runtime) => (
            runtime.label().to_string(),
            match runtime {
                LocalRuntime::Ollama => "ollama list",
                LocalRuntime::Mlx => "scans the Hugging Face cache",
                LocalRuntime::Lmstudio => "lms ls --json",
            }
            .to_string(),
            LocalConfig::for_runtime(runtime, current)
                .endpoint()
                .to_string(),
            current.is_some_and(|config| config.provider == runtime),
        ),
        None => (
            "Off".to_string(),
            "disable local inference".to_string(),
            String::new(),
            current.is_none(),
        ),
    }
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = crate::ui::centered(f.area(), chrome::SMALL.0, chrome::SMALL.1);
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "🖥", "local runtime"),
        if app.local_from_login {
            "↑↓ · PgUp/Dn · Enter pick · Esc back"
        } else {
            "↑↓ · PgUp/Dn · Enter pick · Esc close"
        },
        app,
        true,
        chrome::Tone::Normal,
    );

    let dim = Style::default().fg(app.theme.fg_dim);
    let current = app.core.saved.local.as_ref();
    let items: Vec<ListItem> = (0..ROW_COUNT)
        .map(|i| {
            let (name, hint, endpoint, active) = row(i, current);
            let chip = if active {
                Span::styled("✓ active", Style::default().fg(app.theme.success))
            } else {
                Span::styled("○", dim)
            };
            let width = area.width.saturating_sub(6) as usize;
            let name = chrome::truncate(
                &name,
                width.saturating_sub(chip.content.chars().count() + 2),
            );
            let pad = width.saturating_sub(name.chars().count() + chip.content.chars().count() + 2);
            let top = Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::raw(" ".repeat(pad)),
                chip,
            ]);
            ListItem::new(vec![
                top,
                Line::from(Span::styled(
                    format!("  {}", chrome::truncate(&hint, width)),
                    dim,
                )),
                Line::from(Span::styled(
                    format!("  {}", chrome::truncate(&endpoint, width)),
                    dim,
                )),
            ])
        })
        .collect();

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(Some(app.local_selected));
    chrome::render_list(f, list, &mut state, inner, ROW_COUNT, 3, &app.theme);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match key.code {
        KeyCode::Esc if app.local_from_login => app.popup = nexus_core::app::Popup::Login,
        KeyCode::Esc => app.popup = nexus_core::app::Popup::None,
        KeyCode::Up => app.move_local_selection(-1),
        KeyCode::Down => app.move_local_selection(1),
        KeyCode::Enter => app.confirm_local_selection(),
        _ => {}
    }
}
