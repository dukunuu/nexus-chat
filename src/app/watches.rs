//! Standing research: `watches` re-run their topic's research on an
//! interval, with no daemon — `due_watches` is checked once on app startup.

use chrono::{DateTime, Utc};

use crate::db::Watch;

/// Watches whose interval has elapsed since their last run (or that have
/// never run) as of `now`.
#[allow(dead_code)]  // Task 11 will call this from the startup hook.
pub(crate) fn due_watches(watches: &[Watch], now: DateTime<Utc>) -> Vec<Watch> {
    watches
        .iter()
        .filter(|w| match &w.last_run_at {
            None => true,
            Some(t) => DateTime::parse_from_rfc3339(t)
                .map(|last| now.signed_duration_since(last) >= chrono::Duration::hours(w.interval_hours))
                .unwrap_or(true),
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(topic: &str, interval_hours: i64, last_run_at: Option<&str>) -> Watch {
        Watch {
            id: "w1".to_string(),
            space_id: "space-1".to_string(),
            topic: topic.to_string(),
            interval_hours,
            session_id: "sess-1".to_string(),
            last_run_at: last_run_at.map(str::to_string),
        }
    }

    #[test]
    fn never_run_watch_is_always_due() {
        let w = watch("topic", 24, None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T00:00:00+00:00").unwrap().to_utc();
        assert_eq!(due_watches(&[w], now).len(), 1);
    }

    #[test]
    fn watch_run_recently_is_not_due() {
        let w = watch("topic", 24, Some("2026-07-07T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T05:00:00+00:00").unwrap().to_utc();
        assert!(due_watches(&[w], now).is_empty());
    }

    #[test]
    fn watch_past_its_interval_is_due() {
        let w = watch("topic", 24, Some("2026-07-06T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T01:00:00+00:00").unwrap().to_utc();
        assert_eq!(due_watches(&[w], now).len(), 1);
    }
}
