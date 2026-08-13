//! The `/usage` popup's flow half: open/refresh/scroll state. The aggregate
//! load and the range preference stay in core.

use nexus_core::app::Popup;

use crate::app_view::AppView;

impl AppView {
    pub fn open_usage_popup(&mut self) {
        // Recompute historical costs from the current catalog before
        // rendering — rows logged before pricing existed stay accurate.
        self.core.backfill_usage_costs();
        self.usage_data = Some(self.core.load_usage());
        self.usage_scroll = 0;
        self.popup = Popup::Usage;
        if self
            .usage_data
            .as_ref()
            .is_some_and(|d| d.totals.requests == 0)
        {
            let msg = self.core.usage_range.empty_message().to_string();
            self.push_status(msg);
        }
    }

    pub fn refresh_usage(&mut self) {
        self.core.backfill_usage_costs();
        self.usage_data = Some(self.core.load_usage());
    }

    /// Switch the dashboard's time window (`←/→` in the popup), persist the
    /// choice, and reload. `dir < 0` steps backwards through the cycle.
    pub fn cycle_usage_range(&mut self, dir: i32) {
        self.core.usage_range = if dir >= 0 {
            self.core.usage_range.next()
        } else {
            self.core.usage_range.prev()
        };
        self.core.persist_usage_range();
        self.usage_scroll = 0;
        self.refresh_usage();
    }

    /// Scroll the recent-requests list. Returns the new cursor for the UI.
    pub fn scroll_usage(&mut self, delta: i32) {
        let Some(data) = &self.usage_data else { return };
        let max = data.recent.len().saturating_sub(1);
        self.usage_scroll = if delta >= 0 {
            self.usage_scroll
                .saturating_add(delta.unsigned_abs() as usize)
                .min(max)
        } else {
            self.usage_scroll
                .saturating_sub(delta.unsigned_abs() as usize)
        };
    }
}
