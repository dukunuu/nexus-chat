mod app;
mod appserver;
mod citations;
mod cli;
mod config;
mod db;
mod events;
mod extract;
mod input;
mod markdown;
mod provider;
mod selection;
mod skills;
mod space;
mod theme;
mod tools;
mod ui;
mod update;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // A subcommand (ask/usage/sessions/…) runs headless; no subcommand boots
    // the TUI.
    if let Some(cmd) = cli::parse() {
        return cli::run(cmd).await;
    }

    let saved = config::load_all_providers().await?;
    // A single bootstrap key just seeds App::new's "reasonable defaults"
    // guess (utility model strings); rebuild_all_backends below populates
    // every configured backend regardless of which one this picked.
    let key = config::first_configured(&saved).map(|(_, k)| k);
    let space = space::Space::open()?;
    let space_root = space.spaces_root();
    let db = db::Db::open(&space.db_path())?;

    let mut app = app::App::new(db, key.as_deref(), space);
    app.saved = saved;
    app.rebuild_all_backends();
    app.app_server = appserver::AppServer::start(space_root).await;
    app.refresh_toolbox();

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
            let _ = app.switch_to_session_by_id(&session.id);
        }
    }
    app.spawn_update_check(); // once a day: is a newer release out?
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
