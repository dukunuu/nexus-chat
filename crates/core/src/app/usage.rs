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
    pub fn open_usage_popup(&mut self) {
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
            self.push_status(self.usage_range.empty_message().to_string());
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
