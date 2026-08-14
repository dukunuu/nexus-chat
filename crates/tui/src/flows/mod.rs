//! Popup flow logic moved from core by Phase 2e: the cursor/mode/edit-state
//! methods for each popup, operating on `AppView` (view fields resolve to the
//! view, domain fields/methods fall through the `Deref` to `App`). Domain
//! halves — db reads, disk ops, background jobs — stay in
//! `nexus_core::app::*`.

pub mod apps;
pub mod copy;
pub mod files;
pub mod images;
pub mod models;
pub mod scripts;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod spaces;
pub mod swarm;
pub mod usage;
pub mod watches;
