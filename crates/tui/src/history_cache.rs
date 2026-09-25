//! Wrapped-line render cache for the conversation view, keyed on (session,
//! width, display flags) so a redraw doesn't re-render the transcript's
//! markdown every frame.

use std::collections::HashMap;

use ratatui::text::Line;

#[derive(Default)]
pub struct HistoryCache {
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
