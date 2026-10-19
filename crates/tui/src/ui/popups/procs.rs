use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::app_view::AppView;
use nexus_core::app::Popup;

use super::chrome;

pub fn render(f: &mut Frame, app: &AppView) {
    let area = crate::ui::centered(f.area(), chrome::WIDE.0, chrome::WIDE.1);
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "procs"),
        "↑↓ scroll/select · enter output/list · k kill · esc close",
        app,
        true,
        chrome::Tone::Normal,
    );
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    let message = if app.procs_kill_confirm.is_some() {
        "k again to kill"
    } else {
        "this session first · processes stop when Nexus exits"
    };
    f.render_widget(
        Paragraph::new(message).style(Style::default().fg(app.theme.fg_dim)),
        parts[0],
    );
    if let Some(id) = &app.procs_output {
        let text = app.procs.last_output(id).unwrap_or_else(|e| e.to_string());
        let max = text
            .lines()
            .count()
            .saturating_sub(usize::from(parts[1].height));
        let scroll = app.procs_scroll.min(u16::try_from(max).unwrap_or(u16::MAX));
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(app.theme.fg))
                .scroll((scroll, 0)),
            parts[1],
        );
        return;
    }
    let session = app.session.as_ref().map_or("", |s| s.id.as_str());
    let rows = app.procs.list(session);
    let items = if rows.is_empty() {
        vec![chrome::empty_placeholder(
            "no background processes",
            &app.theme,
        )]
    } else {
        rows.iter()
            .map(|p| {
                ListItem::new(chrome::fit_line(
                    Line::from(format!("{} · {}", p.header(), p.last_line)),
                    usize::from(parts[1].width.saturating_sub(3)),
                ))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.procs_selected.min(rows.len() - 1)));
    }
    chrome::render_list(
        f,
        chrome::standard_list(items, &app.theme),
        &mut state,
        parts[1],
        rows.len(),
        1,
        app,
    );
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let session = app.session.as_ref().map_or("", |s| s.id.as_str());
    let rows = app.procs.list(session);
    let selected = app.procs_output.clone().or_else(|| {
        rows.get(app.procs_selected.min(rows.len().saturating_sub(1)))
            .map(|p| p.id.clone())
    });
    if key.code != KeyCode::Char('k') {
        app.procs_kill_confirm = None;
    }
    match key.code {
        KeyCode::Esc => {
            app.popup = Popup::None;
            app.procs_output = None;
        }
        KeyCode::Up => {
            if app.procs_output.is_some() {
                app.procs_scroll = app.procs_scroll.saturating_sub(1);
            } else {
                app.procs_selected = app.procs_selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.procs_output.is_some() {
                app.procs_scroll = app.procs_scroll.saturating_add(1).min(199);
            } else {
                app.procs_selected = app
                    .procs_selected
                    .saturating_add(1)
                    .min(rows.len().saturating_sub(1));
            }
        }
        KeyCode::Enter => {
            app.procs_output = if app.procs_output.is_some() {
                None
            } else {
                selected
            };
            app.procs_scroll = 0;
        }
        KeyCode::Char('k') => {
            if let Some(id) = selected {
                if app.procs_kill_confirm.as_ref() == Some(&id) {
                    app.procs.kill(&id)?;
                    app.procs_kill_confirm = None;
                } else {
                    app.procs_kill_confirm = Some(id);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nexus_core::app::{App, AppCommand};
    use nexus_core::db::Db;
    use nexus_core::space::Space;
    use nexus_core::tools::ToolBox;
    use nexus_core::workspace::Workspace;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::Arc;

    #[tokio::test]
    async fn output_scroll_status_count_and_two_key_kill() {
        let root = std::env::temp_dir().join(format!("nexus-procs-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space { root: root.clone() },
        ));
        assert_eq!(app.parse_command("procs").unwrap(), AppCommand::OpenProcs);
        let mut toolbox = ToolBox::new(
            std::path::PathBuf::new(),
            None,
            None,
            "auto".into(),
            vec![],
            Some(root.join("nexus.db")),
            None,
            None,
        );
        toolbox.workspace = Some(Arc::new(Workspace::open(&root, vec![]).unwrap()));
        toolbox.procs = Arc::clone(&app.procs);
        let lines = (0..205)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let args = serde_json::json!({"action":"start", "cmd":format!("printf '%s\\n' {lines}; echo ready; sleep 30"), "name":"server", "ready_when":"ready"});
        let (result, _) = toolbox.run("proc", &args.to_string()).await;
        assert!(result.contains(" · ready ·"), "{result}");
        let id = result.split_whitespace().next().unwrap();
        let mut terminal = Terminal::new(TestBackend::new(180, 1)).unwrap();
        terminal
            .draw(|f| crate::ui::status::render_status(f, &app, f.area()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("1 procs"), "{screen}");
        let count = terminal
            .backend()
            .buffer()
            .content
            .windows(7)
            .find(|cells| cells.iter().map(|c| c.symbol()).collect::<String>() == "1 procs")
            .unwrap();
        assert!(count.iter().all(|c| c.fg == app.theme.fg_dim));
        app.popup = Popup::Procs;
        handle_key(&mut app, KeyCode::Enter.into()).unwrap();
        assert_eq!(app.procs_output.as_deref(), Some(id));
        assert!(app.procs.last_output(id).unwrap().starts_with("line6\n"));
        handle_key(&mut app, KeyCode::Down.into()).unwrap();
        assert_eq!(app.procs_scroll, 1);
        handle_key(&mut app, KeyCode::Up.into()).unwrap();
        assert_eq!(app.procs_scroll, 0);
        handle_key(&mut app, KeyCode::Char('k').into()).unwrap();
        assert_eq!(app.procs_kill_confirm.as_deref(), Some(id));
        assert_eq!(app.procs.list("")[0].state, "running");
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("k again to kill"));
        handle_key(&mut app, KeyCode::Char('k').into()).unwrap();
        assert_eq!(app.procs.list("")[0].state, "killed");
        assert!(app.procs_kill_confirm.is_none());
        handle_key(&mut app, KeyCode::Esc.into()).unwrap();
        assert_eq!(app.popup, Popup::None);
        app.procs.shutdown().await;
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
