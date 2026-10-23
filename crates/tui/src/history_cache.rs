//! Wrapped-line render cache for the conversation view, keyed on (session,
//! width, display flags) so a redraw doesn't re-render the transcript's
//! markdown every frame.

use std::collections::HashMap;

use ratatui::text::Line;

/// A batch sub-card's wrapped rows, including any opened body and images.
#[derive(Clone, Debug)]
pub struct BatchRow {
    pub owner: usize,
    pub index: usize,
    pub lines: std::ops::Range<usize>,
}

#[derive(Default)]
pub struct HistoryCache {
    pub key: (Option<String>, usize, bool, bool, bool, bool, usize),
    pub msg_count: usize,
    /// Last completion/preference generation rendered in this cache.
    pub image_generation: usize,
    pub lines: Vec<Line<'static>>,
    pub owner: Vec<Option<usize>>,
    pub batch_rows: Vec<BatchRow>,
    pub code: Vec<Option<usize>>,
    pub blocks: Vec<String>,
    /// Fixed-height diagram regions alongside the text/copy metadata.
    pub diagrams: Vec<crate::diagrams::Placement>,
    /// Model-authored interactive output regions.
    pub widgets: Vec<crate::interactive::Placement>,
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
