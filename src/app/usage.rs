//! The `/usage` popup: token, prompt-cache, and cost analytics drawn from
//! the per-request `usage_log`. Content-free — only backend/model/tokens —
//! so it works even for sessions long compacted away.

use super::{App, Popup};

use crate::db::{UsageByBackend, UsageByModel, UsageRow, UsageTotals};

/// Snapshot of the aggregates the popup renders, loaded on open (and on
/// Ctrl+R refresh).
pub struct UsageData {
    pub totals: UsageTotals,
    pub by_backend: Vec<UsageByBackend>,
    pub by_model: Vec<UsageByModel>,
    pub recent: Vec<UsageRow>,
}

impl App {
    pub(crate) fn open_usage_popup(&mut self) {
        // Recompute historical costs from the current catalog before
        // rendering — rows logged before pricing existed stay accurate.
        let _ = self.db.backfill_usage_costs();
        self.usage_data = Some(self.load_usage());
        self.usage_scroll = 0;
        self.popup = Popup::Usage;
        if self
            .usage_data
            .as_ref()
            .is_some_and(|d| d.totals.requests == 0)
        {
            self.status = self.usage_range.empty_message().to_string();
        }
    }

    pub fn refresh_usage(&mut self) {
        let _ = self.db.backfill_usage_costs();
        self.usage_data = Some(self.load_usage());
    }

    /// Switch the dashboard's time window (`←/→` in the popup), persist the
    /// choice, and reload. `dir < 0` steps backwards through the cycle.
    pub fn cycle_usage_range(&mut self, dir: i32) {
        self.usage_range = if dir >= 0 {
            self.usage_range.next()
        } else {
            self.usage_range.prev()
        };
        let _ = self.db.set_setting("usage_range", self.usage_range.key());
        self.usage_scroll = 0;
        self.refresh_usage();
    }

    /// Scroll the recent-requests list. Returns the new cursor for the UI.
    pub fn scroll_usage(&mut self, delta: i32) {
        let Some(data) = &self.usage_data else { return };
        let max = data.recent.len().saturating_sub(1);
        self.usage_scroll = if delta >= 0 {
            self.usage_scroll.saturating_add(delta as usize).min(max)
        } else {
            self.usage_scroll
                .saturating_sub(delta.unsigned_abs() as usize)
        };
    }

    fn load_usage(&self) -> UsageData {
        let since = self.usage_range.since().map(|t| t.to_rfc3339());
        UsageData {
            totals: self.db.usage_totals(since.as_deref()).unwrap_or_default(),
            by_backend: self
                .db
                .usage_by_backend(since.as_deref())
                .unwrap_or_default(),
            by_model: self
                .db
                .usage_by_model(10, since.as_deref())
                .unwrap_or_default(),
            recent: self
                .db
                .usage_recent(200, since.as_deref())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, UsageByBackend, UsageByModel, UsageRow, UsageTotals};
    use crate::ui::popups::usage::render;

    fn populated_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let space = crate::space::Space {
            root: std::env::temp_dir().join(format!("nexus-usage-{}", uuid::Uuid::new_v4())),
        };
        let mut app = App::new(db, Some("k".into()), space);
        app.usage_data = Some(UsageData {
            totals: UsageTotals {
                requests: 28_172,
                prompt_tokens: 5_900_000,
                completion_tokens: 144_000,
                cache_read_tokens: 3_300_000,
                cache_creation_tokens: 2_600_000,
                cost: 6.1438,
            },
            by_backend: vec![
                UsageByBackend {
                    backend: "OpenCode Go".into(),
                    requests: 28_125,
                    prompt_tokens: 741_000,
                    completion_tokens: 24_800,
                    cache_read_tokens: 481_650,
                    cost: 0.1108,
                },
                UsageByBackend {
                    backend: "OpenRouter".into(),
                    requests: 47,
                    prompt_tokens: 5_100_000,
                    completion_tokens: 119_100,
                    cache_read_tokens: 2_805_000,
                    cost: 6.0330,
                },
            ],
            by_model: vec![
                UsageByModel {
                    model: "go:deepseek-v3-Flash".into(),
                    requests: 28_125,
                    prompt_tokens: 741_000,
                    completion_tokens: 24_800,
                    cache_read_tokens: 481_650,
                    cost: 0.1108,
                },
                UsageByModel {
                    model: "deepseek/deepseek-v3-Flash-0724".into(),
                    requests: 47,
                    prompt_tokens: 5_100_000,
                    completion_tokens: 119_100,
                    cache_read_tokens: 2_805_000,
                    cost: 6.0330,
                },
            ],
            recent: (0..3)
                .map(|i| UsageRow {
                    created_at: format!("2026-08-12T23:{:02}:00Z", 15 - i),
                    backend: "OpenRouter".into(),
                    // Realistic overflow-prone values: "122.2k→672" (10
                    // chars) and "120.1k→2.1k" (11 chars) exceed a 9-cell
                    // field and used to shove the trailing columns around.
                    model: format!("deepseek/deepseek-v4-flash-0731 ({i})"),
                    prompt_tokens: [122_221, 120_086, 118_690][i],
                    completion_tokens: [672, 2126, 592][i],
                    cache_read_tokens: 118_784,
                    cost: Some(0.0095),
                })
                .collect(),
        });
        app
    }

    /// Render the popup alone and return the buffer rows as strings.
    fn render_rows(app: &App) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// Char position of `needle` — byte positions are unusable here: the
    /// rows mix 1- and 3-byte glyphs (spaces, │, ●), so byte offsets would
    /// drift by the wide-glyph byte count.
    fn char_pos(line: &str, needle: &str) -> Option<usize> {
        line.find(needle).map(|b| line[..b].chars().count())
    }

    /// Column check: in `header`, the label's right edge must equal the
    /// value's right edge in `row` (right-aligned columns), with the cached
    /// column's "%" riding one cell past the label like the rows' "65%".
    fn assert_column(header: &str, row: &str, label: &str, value: &str) {
        let label_end = char_pos(header, label)
            .map(|i| i + label.len())
            .unwrap_or_else(|| {
                panic!("header missing {label:?}: {header}");
            });
        let value_end = char_pos(row, value)
            .map(|i| i + value.len())
            .unwrap_or_else(|| {
                panic!("row missing {value:?}: {row}");
            });
        assert_eq!(
            label_end, value_end,
            "column {label:?} ({value:?}) misaligned\nheader: {header}\nrow:    {row}"
        );
    }

    #[test]
    fn backend_header_columns_align_with_rows() {
        let app = populated_app();
        let rows = render_rows(&app);
        let header = rows
            .iter()
            .find(|r| r.contains("req") && r.contains("cached") && r.contains("cost"))
            .unwrap();
        let row = rows.iter().find(|r| r.contains("28125")).unwrap();
        // The "● " marker must have a 2-space header counterpart.
        assert_eq!(
            char_pos(header, "backend"),
            char_pos(row, "OpenCode"),
            "name column misaligned"
        );
        assert_column(header, row, "req", "28125");
        assert_column(header, row, "prompt", "741.0k");
        assert_column(header, row, "out", "24.8k");
        assert_column(header, row, "cached", "65"); // "%" rides one past
        assert_column(header, row, "cost", "$0.1108");
    }

    #[test]
    fn models_header_columns_align_with_rows() {
        let app = populated_app();
        let rows = render_rows(&app);
        let header = rows
            .iter()
            .find(|r| r.contains("model") && r.contains("req") && r.contains("cached"))
            .unwrap();
        let short = rows.iter().find(|r| r.contains("go:deepseek")).unwrap();
        let long = rows
            .iter()
            .find(|r| r.contains("deepseek/deepseek-v3-Flash-07"))
            .unwrap();
        // Different name lengths must not move the numeric columns.
        // Right-aligned: the values' right edges must coincide.
        let edge = |line: &str, needle: &str| char_pos(line, needle).map(|c| c + needle.len());
        assert_eq!(edge(short, "28125"), edge(long, "47"), "req column drifts");
        assert_column(header, short, "req", "28125");
        assert_column(header, short, "prompt", "741.0k");
        assert_column(header, short, "out", "24.8k");
        assert_column(header, short, "cached", "65");
        assert_column(header, short, "cost", "$0.1108");
    }

    #[test]
    fn recent_request_rows_share_columns() {
        let app = populated_app();
        let rows = render_rows(&app);
        let mut recent: Vec<&String> = rows
            .iter()
            .filter(|r| r.contains("→") && r.contains("\u{2588}"))
            .collect();
        assert!(recent.len() >= 2, "need recent rows, got {}", recent.len());
        let first = recent.remove(0);
        for row in &recent {
            // The "→" sits inside the right-aligned tokens field, so its
            // position varies with the token string length — the column
            // anchors are the field's right edge (the cache bar) and the
            // percent/cost columns after it.
            assert_eq!(
                char_pos(first, "████████"),
                char_pos(row, "████████"),
                "cache bar column drifts\nfirst: {first}\nrow:   {row}"
            );
            assert_eq!(
                first.rfind('%').map(|b| first[..b].chars().count()),
                row.rfind('%').map(|b| row[..b].chars().count()),
                "cache% column drifts\nfirst: {first}\nrow:   {row}"
            );
            assert_eq!(
                char_pos(first, "$0.0095"),
                char_pos(row, "$0.0095"),
                "cost column drifts\nfirst: {first}\nrow:   {row}"
            );
        }
    }
}
