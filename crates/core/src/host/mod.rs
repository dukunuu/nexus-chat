//! Phase 4 `nexus host` — the daemon surface.
//!
//! One subcommand turns the current machine into the hub: an `HTTP`/`SSE`
//! session API, a byte-passthrough provider gateway, worker routes, and
//! tunnel/sleep-guard process management. The wire types the daemon speaks
//! are defined here in [`wire`]; the router, app actor, and `OpenAI`-wire
//! gateway live in `api.rs`, sidecar lifecycles in [`process`], and named
//! tunnel provisioning in [`cloudflare`].

mod api;
pub mod cloudflare;
pub mod process;
pub mod wire;

pub use api::{HostConfig, HostServer};
