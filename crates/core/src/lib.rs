//! nexus-core: the engine behind the `nexus` TUI and (later) the nexus
//! host API. Owns all domain logic — sessions, research pipeline, provider
//! clients, tools, files, skills, `SQLite` state — with no knowledge of the
//! terminal UI.
//!
//! During Phase 2 (workspace split) the `App` struct still carries view
//! state and a few TUI-typed fields (composer, theme, render caches);
//! step 2e moves those into the TUI crate.
//!
//! Interim lint allows (removed in 2c/2e): the whole `App` surface is `pub`
//! so the TUI crate can drive it directly. Once commands + accessors land,
//! these items go private again and the doc lints re-apply.
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
pub mod filter_input;
pub mod history_cache;
pub mod input;
pub mod markdown;
pub mod provider;
pub mod selection;
pub mod skills;
pub mod space;
pub mod theme;
pub mod tools;
pub mod update;
