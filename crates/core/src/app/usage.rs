//! The `/usage` popup's domain half: aggregate token/cache/cost analytics
//! drawn from the per-request `usage_log`. Content-free — only
//! backend/model/tokens — so it works even for sessions long compacted
//! away.

use super::App;

use crate::db::{UsageByBackend, UsageByModel, UsageRange, UsageRow, UsageTotals};

/// Snapshot of the aggregates the popup renders, loaded on open (and on
/// Ctrl+R refresh).
pub struct UsageData {
    pub totals: UsageTotals,
    pub by_backend: Vec<UsageByBackend>,
    pub by_model: Vec<UsageByModel>,
    pub recent: Vec<UsageRow>,
}

impl App {
    /// Load the aggregates for the persisted range preference.
    pub fn load_usage(&self) -> UsageData {
        self.load_usage_for_range(self.usage_range)
    }

    /// Load usage analytics for a requested window without changing the
    /// persisted TUI preference. Thin clients use this for range tabs.
    pub fn load_usage_for_range(&self, range: UsageRange) -> UsageData {
        let since = range.since().map(|t| t.to_rfc3339());
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

    /// Domain half of the range cycle: persist the choice (the view switches
    /// `usage_range` and reloads).
    pub fn persist_usage_range(&mut self) {
        let _ = self.db.set_setting("usage_range", self.usage_range.key());
    }

    /// Recompute historical costs from the current catalog before rendering —
    /// rows logged before pricing existed stay accurate. Called on popup open
    /// and refresh.
    pub fn backfill_usage_costs(&mut self) {
        let _ = self.db.backfill_usage_costs();
    }
}
