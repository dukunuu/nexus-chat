//! `/local`'s runtime selector: Ollama, MLX, LM Studio, edge0, or off. Unlike
//! `/login` there is no key to paste — picking a row writes the
//! machine-local `[local]` block and reloads the catalog. Custom endpoints
//! and discovery commands stay a config-file (or `/local <runtime> <url>`)
//! concern; this popup only switches runtimes.

use std::fmt::Write as _;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use nexus_core::provider::local::{LocalConfig, LocalRuntime};
use nexus_core::provider::serve::{RuntimeStatus, format_kb};

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
                LocalRuntime::Edge0 => "edge0 models",
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

/// The server line for a row: whether the endpoint answers, what its process
/// tree costs, and who started it. Rendered on the endpoint line so a row
/// stays three lines tall.
///
/// Ordered by urgency, because the popup is narrow enough to truncate: the
/// liveness dot, the memory, the budget warning, then ownership, and the
/// endpoint last — it is the most predictable part and the one worth losing.
fn server_line(status: Option<&RuntimeStatus>, endpoint: &str) -> (String, bool) {
    let Some(status) = status else {
        return (format!("○ {endpoint}"), false);
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(rss) = status.rss_kb {
        parts.push(format_kb(rss));
    }
    if status.over_budget {
        parts.push("⚠ over budget".to_string());
    }
    if status.managed {
        parts.push("started here".to_string());
    }
    parts.push(endpoint.to_string());
    let mut line = String::from(if status.running { '●' } else { '○' });
    let _ = write!(line, " {}", parts.join(" · "));
    (line, status.over_budget)
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = crate::ui::centered(f.area(), chrome::SMALL.0, chrome::SMALL.1);
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "🖥", "local runtime"),
        if app.local_from_login {
            "↑↓ · Enter pick · s start · x stop · r refresh · Esc back"
        } else {
            "↑↓ · Enter pick · s start · x stop · r refresh · Esc close"
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
            let status = LocalRuntime::ALL.get(i).and_then(|runtime| {
                app.core
                    .local_status
                    .iter()
                    .find(|status| status.runtime == *runtime)
            });
            let (server, warn) = if endpoint.is_empty() {
                (String::new(), false)
            } else {
                server_line(status, &endpoint)
            };
            let server_style = if warn {
                Style::default().fg(app.theme.warning)
            } else {
                dim
            };
            ListItem::new(vec![
                top,
                Line::from(Span::styled(
                    format!("  {}", chrome::truncate(&hint, width)),
                    dim,
                )),
                Line::from(Span::styled(
                    format!("  {}", chrome::truncate(&server, width)),
                    server_style,
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
        // Server lifecycle, on the row under the cursor. The "off" row has no
        // server, so these are no-ops there.
        KeyCode::Char('s') => app.local_server_key("start"),
        KeyCode::Char('x') => app.local_server_key("stop"),
        KeyCode::Char('r') => app.local_server_key("status"),
        _ => {}
    }
}
