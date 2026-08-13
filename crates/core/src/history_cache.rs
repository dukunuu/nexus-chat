//! Wrapped-line render cache for the conversation view. The TUI re-renders
//! markdown for the whole transcript every frame unless the wrapped result
//! is cached keyed on (session, width, display flags); this cache holds that
//! result. It lives in core because the `App` struct owns it (and core
//! methods invalidate it on session switches and display-flag toggles), but
//! only the TUI crate ever populates it.

use std::collections::HashMap;

use ratatui::text::Line;

#[derive(Default)]
pub struct HistoryCache {
    // Fields are pub only because the TUI crate populates the cache during
    // render (Phase 2a interim); when 2e moves the cache into the TUI crate
    // they become private again.
    pub key: (Option<String>, usize, bool, bool, bool, bool, usize),
    pub msg_count: usize,
    pub lines: Vec<Line<'static>>,
    pub owner: Vec<Option<usize>>,
    pub code: Vec<Option<usize>>,
    pub blocks: Vec<String>,
    pub plain: Vec<String>,
    /// Maps rendered line index -> image path for click-to-open.
    pub image_at_line: Vec<Option<String>>,
    /// Cache of rendered half-block image lines by (path, width) — avoids
    /// re-decoding image files every frame.
    pub image_cache: HashMap<(String, usize), Vec<Line<'static>>>,
    /// Calendar day ("2026-08-08") of the last cached message, for the
    /// `── Today ──` day dividers.
    pub last_day: Option<String>,
}
