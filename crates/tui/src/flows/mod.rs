//! Popup flow logic: the cursor/mode/edit-state methods for each popup,
//! operating on `AppView`. Domain halves — db reads, disk ops, background
//! jobs — live in `nexus_core::app::*`.

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
