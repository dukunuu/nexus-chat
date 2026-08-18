mod app_view;
mod cli;
mod composer;
mod events;
mod filter_input;
mod flows;
mod history_cache;
mod selection;
mod theme;
mod ui;

use anyhow::Result;
use nexus_core::config;

use app_view::AppView;

#[tokio::main]
async fn main() -> Result<()> {
    let (continue_session, command) = cli::parse();
    // A subcommand (ask/usage/sessions/…) runs headless; no subcommand boots
    // the TUI. `--continue` only makes sense for the interactive launcher.
    if let Some(cmd) = command {
        if continue_session {
            anyhow::bail!("--continue can only be used when launching the TUI");
        }
        return cli::run(cmd).await;
    }

    let saved = config::load_all_providers().await?;
    let app = nexus_core::boot(saved).await?;
    let mut app = AppView::new(app);

    let mut terminal = ratatui::init();
    // Capture mouse so the model picker is clickable and the terminal doesn't do
    // its own screen-wide text selection (composer selection is Shift/Ctrl+arrows,
    // copied with Ctrl+C). Bracketed paste delivers native paste as one event.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    );
    // Enhanced keys so the terminal reports Ctrl+Backspace etc. distinctly
    // (without this, most terminals send the same byte for Backspace).
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
        let _ = crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    app.init(); // fetch models if a key is already present
    if continue_session {
        // Explicit `--continue` wins over a stale `nexus open` handoff file.
        let _ = std::fs::remove_file(app.space.root.join("pending-open"));
        if let Some((space_id, session)) = app.db.latest_session()? {
            if app.active_space.id != space_id
                && let Some(row) = app.db.list_spaces()?.into_iter().find(|s| s.id == space_id)
            {
                app.set_active_space(row);
            }
            let _ = app.execute(nexus_core::app::AppCommand::ResolveSession { id: session.id });
        } else {
            app.push_status("no previous session — send a message to start one".to_string());
        }
    } else {
        // `nexus open <session>` handoff: the CLI wrote a pending-open file —
        // jump straight into that session (switching to its space first).
        let handoff = app.space.root.join("pending-open");
        if let Ok(id) = std::fs::read_to_string(&handoff) {
            let _ = std::fs::remove_file(&handoff);
            let id = id.trim();
            if !id.is_empty()
                && let Ok((space_id, session)) = cli::resolve_session(&app.db, id)
                && let Ok(rows) = app.db.list_spaces()
                && let Some(row) = rows.into_iter().find(|s| s.id == space_id)
            {
                app.set_active_space(row);
                let _ = app.execute(nexus_core::app::AppCommand::ResolveSession { id: session.id });
            }
        }
    }
    app.spawn_update_check(); // once a day: is a newer release out? — auto-installs it in the background
    app.run_due_watches(); // re-run any standing research watches that are due
    let result = events::run(app, &mut terminal).await;
    if enhanced {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
    result
}
