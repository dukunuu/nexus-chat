//! nexus-chat-core (lib crate `nexus_core`): the engine behind the `nexus`
//! TUI and (later) the nexus host API. Owns all domain logic — sessions, research pipeline, provider
//! clients, tools, files, skills, `SQLite` state — with no knowledge of the
//! terminal UI. Phase 2e moved every piece of view state (composer, popup
//! chrome, render caches, theme) into the TUI crate's `AppView`.
//!
//! Doc-lint allows: the domain surface the TUI/CLI/host drive directly
//! (`Db`, provider, config, `App`'s event handlers) is still pub, so the
//! per-item doc lints would be noise until the Phase 4 host API pass
//! privatizes it. They're crate-scoped deliberately — the 2e goal (zero
//! TUI deps) is unaffected.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

pub mod app;
pub mod app_templates;
pub mod appserver;
pub mod citations;
pub mod config;
pub mod db;
pub mod extract;
pub mod host;
pub mod markdown;
pub mod provider;
pub mod skills;
pub mod space;
pub mod sync;
pub mod tools;
pub mod update;

use anyhow::Result;

/// One bootstrap for every frontend (TUI, CLI, Phase 4 host): credentials →
/// space → db → appserver → toolbox. This is what `main.rs` and
/// `cli.rs::build_app` both used to hand-roll.
pub async fn boot(saved: config::SavedCreds) -> Result<app::App> {
    // A single bootstrap key just seeds App::new's "reasonable defaults"
    // guess (utility model strings); rebuild_all_backends below populates
    // every configured backend regardless of which one this picked.
    let key = config::first_configured(&saved).map(|(_, k)| k);
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let mut app = app::App::new(db, key.as_deref(), space);
    app.saved = saved;
    app.rebuild_all_backends();
    app.app_server = appserver::AppServer::start(app.space.spaces_root()).await;
    app.refresh_toolbox();
    Ok(app)
}
