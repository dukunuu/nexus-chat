//! The `/usage` popup's domain half: aggregate token/cache/cost analytics
//! drawn from the per-request `usage_log`. Content-free — only
//! backend/model/tokens — so it works even for sessions long compacted
//! away. The popup's cursor/range flow lives in the view layer.

use super::App;

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
    /// Load the aggregates for the currently selected range (the view owns
    /// the cursor; the range is a persisted core preference).
    pub fn load_usage(&self) -> UsageData {
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
