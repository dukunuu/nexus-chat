//! nexus-chat-core (lib crate `nexus_core`): the engine behind the `nexus`
//! TUI, CLI, and `nexus host` API. Owns all domain logic — sessions, research
//! pipeline, provider clients, tools, files, skills, `SQLite` state — with no
//! knowledge of the terminal UI.
//!
//! Doc-lint allows: the domain surface the frontends drive directly (`Db`,
//! provider, config, `App`'s event handlers) is pub, and per-item error/panic
//! docs on all of it would be noise.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

pub mod app;
pub mod app_templates;
pub mod appserver;
pub mod canvas;
pub mod citations;
pub mod config;
pub mod connectors;
pub mod db;
pub mod decider;
pub mod deck;
pub mod design;
pub mod doc;
pub mod eval;
pub mod extract;
pub mod host;
pub mod interactive;
pub mod jobs;
pub mod markdown;
pub mod panels;
pub mod prompts;
pub mod provider;
pub mod remote_image;
pub mod skills;
pub mod space;
pub mod subagent;
pub mod sync;
pub mod theme_colors;
pub mod tools;
pub mod trace;
pub mod train;
pub mod update;
pub mod usershell;
pub mod workspace;

use anyhow::Result;

/// One bootstrap for every frontend (TUI, CLI, host): credentials → space →
/// db → appserver → toolbox.
pub async fn boot(saved: config::SavedCreds) -> Result<app::App> {
    // A single bootstrap key just seeds App::new's "reasonable defaults"
    // guess (utility model strings); rebuild_all_backends below populates
    // every configured backend regardless of which one this picked.
    let key = config::first_configured(&saved).map(|(_, k)| k);
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    if space.traces_dir().join("index.db").exists() {
        let _ = (|| -> Result<()> {
            let mut statement = db.conn().prepare("SELECT id FROM sessions")?;
            let existing = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
            let store =
                trace::TraceStore::open(&space.traces_dir(), trace::Scrubber::new(Vec::new()))?;
            store.prune_missing_sessions(&existing)?;
            Ok(())
        })();
    }
    let prompt_note = config::migrate_system_prompt_overlay();
    let mut app = app::App::new(db, key.as_deref(), space);
    app.system_prompt_overlay = config::load_system_prompt_overlay();
    if let Some(note) = prompt_note {
        app.push_status(note);
    }
    app.saved = saved;
    // Connectors connect in the background; the TUI never waits for them.
    app.connectors =
        connectors::Registry::system(&app.space.root, Some(app.connector_notes_tx.clone()), true);
    match app.connectors.load() {
        Ok(problems) => {
            for problem in problems {
                app.push_status(format!("connector config: {problem}"));
            }
        }
        Err(error) => app.push_status(format!("connectors not loaded: {error:#}")),
    }
    app.rebuild_all_backends();
    app.discover_embeddings();
    app.app_server = appserver::AppServer::start(app.space.spaces_root()).await;
    app.refresh_toolbox();
    Ok(app)
}
