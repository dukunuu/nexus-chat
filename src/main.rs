mod app;
mod appserver;
mod citations;
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

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let saved = config::load_all_providers().await?;
    let key = saved
        .openrouter_key
        .clone()
        .or_else(|| saved.openai_key.clone())
        .or_else(|| saved.codex.as_ref().map(|c| c.access.clone()));
    let space = space::Space::open()?;
    let space_root = space.spaces_root();
    let db = db::Db::open(&space.db_path())?;

    let mut app = app::App::new(db, key, space);
    app.saved = saved;
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
