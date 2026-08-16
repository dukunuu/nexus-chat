//! Phase 4 `nexus host` — the daemon surface.
//!
//! One subcommand turns the current machine into the hub: an `HTTP`/`SSE`
//! session API, a byte-passthrough provider gateway, worker routes, and
//! tunnel/sleep-guard process management. The wire types the daemon speaks
//! are defined here in [`wire`]; the router (`api.rs`), gateway
//! (`gateway.rs`), and process management land in later Phase 4 steps.

pub mod wire;
