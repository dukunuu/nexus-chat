//! Standing research: `watches` re-run their topic's research on an
//! interval, with no daemon — `due_watches` is checked once on app startup.

use chrono::{DateTime, Utc};

use crate::db::Watch;

/// Watches whose interval has elapsed since their last run (or that have
/// never run) as of `now`.
pub(crate) fn due_watches(watches: &[Watch], now: DateTime<Utc>) -> Vec<Watch> {
    watches
        .iter()
        .filter(|w| match &w.last_run_at {
            None => true,
            Some(t) => DateTime::parse_from_rfc3339(t)
                .map(|last| {
                    now.signed_duration_since(last) >= chrono::Duration::hours(w.interval_hours)
                })
                .unwrap_or(true),
        })
        .cloned()
        .collect()
}

/// A "## What changed since last run" section prepended to a watch's new
/// report: lists newly-seen sources (by URL) not cited in the previous
/// report. Does not diff prose — an LLM-generated summary of what changed
/// is out of scope for this pass (YAGNI: a source-level diff is what a
/// user actually scans for first).
pub(crate) fn diff_section(
    previous_report: &str,
    new_report: &str,
    new_sources: &[String],
) -> String {
    let _ = (previous_report, new_report); // reserved for a future prose diff; unused today
    let mut out = String::from("## What changed since last run\n\n");
    if new_sources.is_empty() {
        out.push_str("No new sources since the last run.\n");
    } else {
        out.push_str("New sources:\n");
        for s in new_sources {
            out.push_str(&format!("- {s}\n"));
        }
    }
    out
}

/// New (not-previously-cited) sources in `new_report` vs `previous_citations`
/// — a plain set difference over normalized URLs.
pub(crate) fn new_sources_since(new_report: &str, previous_citations: &[String]) -> Vec<String> {
    let previous: std::collections::HashSet<String> = previous_citations
        .iter()
        .map(|u| crate::tools::normalize_url(u))
        .collect();
    crate::citations::parse_citations(new_report)
        .into_iter()
        .map(|(_, url)| url)
        .filter(|url| !previous.contains(&crate::tools::normalize_url(url)))
        .collect()
}

impl super::App {
    pub(crate) fn open_watch_picker(&mut self) -> anyhow::Result<()> {
        self.watches_cache = self.db.list_watches(&self.active_space.id)?;
        self.watch_selected = 0;
        self.popup = super::Popup::Watch;
        Ok(())
    }

    pub(crate) fn move_watch_selection(&mut self, delta: i32) {
        self.watch_selected =
            super::clamp_cursor(self.watch_selected, self.watches_cache.len(), delta);
    }

    /// `/watch <topic>` with no existing watch of that exact topic in this
    /// space: create one (fixed 24h interval) plus its own research
    /// session, and kick off the first run immediately (ungated).
    pub(crate) fn create_watch(&mut self, topic: &str) {
        if topic.is_empty() {
            self.status = "usage: /watch <topic>".to_string();
            return;
        }
        self.start_research_with_gate(topic, false);
        let Some(session) = &self.session else {
            self.status = "could not start watch: no session created".to_string();
            return;
        };
        match self
            .db
            .create_watch(&self.active_space.id, topic, 24, &session.id)
        {
            Ok(_) => self.status = format!("watching: {topic} (every 24h)"),
            Err(e) => self.status = format!("watch creation failed: {e}"),
        }
    }

    /// Enter on the watch picker: jump to the watch's own research session,
    /// mirroring `confirm_session`'s session-switch shape (load messages, set
    /// `self.session`, refresh the toolbox, reset scroll/context state).
    pub(crate) fn confirm_watch_session(&mut self) -> anyhow::Result<()> {
        if let Some(w) = self.watches_cache.get(self.watch_selected).cloned()
            && let Some(s) = self.db.get_session(&w.session_id)?
        {
            self.messages = self.db.load_messages(&s.id)?;
            self.unread.remove(&s.id);
            self.current_model = Some(s.model.clone());
            self.status = format!("switched to: {}", s.title);
            self.web_mode = s.web_mode;
            self.session = Some(s);
            self.refresh_toolbox();
            self.context_total = None;
            self.scroll = 0;
            self.clear_image_state();
        }
        self.popup = super::Popup::None;
        Ok(())
    }

    pub(crate) fn delete_selected_watch(&mut self) {
        if let Some(w) = self.watches_cache.get(self.watch_selected).cloned() {
            let _ = self.db.delete_watch(&w.id);
            self.watches_cache.retain(|x| x.id != w.id);
            self.watch_selected = self
                .watch_selected
                .min(self.watches_cache.len().saturating_sub(1));
            self.status = format!("deleted watch: {}", w.topic);
        }
    }

    /// Startup hook: re-run every due watch (across all spaces) in the
    /// background, ungated. Best-effort — a watch whose research job can't
    /// start (e.g. no model configured) is silently skipped; it'll be
    /// retried on the next app open since `last_run_at` isn't touched.
    pub(crate) fn run_due_watches(&mut self) {
        let Ok(all) = self.db.list_all_watches() else {
            return;
        };
        let due = due_watches(&all, chrono::Utc::now());
        for w in due {
            // A watch may belong to a space other than whatever's currently
            // active (`due_watches` spans every space) — `start_research_with_gate`
            // reads `self.active_space` for the toolbox/file paths and
            // `save_research_report`'s destination, so it must be switched to
            // the watch's own space for the run, same as its session.
            let Ok(spaces) = self.db.list_spaces() else {
                continue;
            };
            let Some(space_row) = spaces.into_iter().find(|s| s.id == w.space_id) else {
                continue;
            };
            let restore_space = self.active_space.clone();
            let restore_session = self.session.clone();
            let restore_messages = std::mem::take(&mut self.messages);
            if let Ok(Some(s)) = self.db.get_session(&w.session_id) {
                let prior_session_id = s.id.clone();
                self.active_space = space_row;
                self.session = Some(s);
                self.start_research_with_gate(&w.topic, false);
                // `start_research_with_gate` only allows one job at a time —
                // for the 2nd+ due watch in this loop, `research_rx.is_some()`
                // is still set from the first watch's job (it isn't cleared
                // until that job's background task finishes, long after this
                // synchronous loop returns), so the guard fires and the call
                // above is a no-op: `self.session` is left exactly as set on
                // the line above (the watch's *prior* session), unchanged.
                // Only when a new session was actually created do we know the
                // job really started — compare ids to tell those cases apart,
                // and leave a not-actually-run watch untouched (still due) for
                // the next startup rather than falsely marking it caught up.
                if let Some(new_session) = self.session.as_ref()
                    && new_session.id != prior_session_id
                {
                    let _ = self.db.set_watch_session(&w.id, &new_session.id);
                    let _ = self.db.touch_watch(&w.id, &chrono::Utc::now().to_rfc3339());
                }
            }
            self.active_space = restore_space;
            self.session = restore_session;
            self.messages = restore_messages;
            self.refresh_toolbox();
        }
    }

    /// `Some(urls)` if `session_id` is a watch's session and it has a prior
    /// run (citations already indexed from an earlier `save_research_report`
    /// call); `Ok(None)` for a first run or a non-watch session — either way
    /// means "no diff section". Takes the report's own `space_id` rather than
    /// reading `self.active_space`: this runs from `on_research_done`, which
    /// fires asynchronously and may land well after the user (or
    /// `run_due_watches`, which restores it right after spawning the job) has
    /// switched the active space away from the one this job actually ran in.
    pub(crate) fn previous_citations_for_watch_session(
        &self,
        session_id: &str,
        space_id: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let Some(w) = self
            .db
            .list_all_watches()?
            .into_iter()
            .find(|w| w.session_id == session_id)
        else {
            return Ok(None);
        };
        // Scope to this watch's own prior report(s): `save_research_report`
        // names every report it saves `research-<slug>-<timestamp>.md` where
        // `slug = slugify(topic)`, and a watch's topic (hence its slug) is
        // stable across re-runs. Filtering `report_file` to that prefix keeps
        // an unrelated `/research` session elsewhere in the same space from
        // suppressing a source as "not new" for this watch.
        let slug = super::sessions::slugify(&w.topic);
        let prefix = format!("research-{slug}-");
        let rows = self.db.search_citations(space_id, Some(&prefix))?;
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|(report_file, _, _)| {
                report_file.starts_with(&prefix)
                    && report_file
                        .chars()
                        .nth(prefix.len())
                        .map_or(false, |c| c.is_ascii_digit())
            })
            .collect();
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(rows.into_iter().map(|(_, url, _)| url).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root =
            std::env::temp_dir().join(format!("nexus-watches-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k".into()), space)
    }

    #[tokio::test]
    async fn run_due_watches_repoints_the_watch_at_its_new_session() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        let space_id = a.active_space.id.clone();

        // The watch's original session, from some earlier run.
        let first_session =
            a.db.create_session("first run", "openai/gpt-5-mini", &space_id)
                .unwrap();
        let watch_id =
            a.db.create_watch(&space_id, "rust async runtimes", 24, &first_session.id)
                .unwrap();

        a.run_due_watches();

        let updated =
            a.db.list_all_watches()
                .unwrap()
                .into_iter()
                .find(|w| w.id == watch_id)
                .unwrap();
        assert_ne!(
            updated.session_id, first_session.id,
            "run_due_watches should repoint the watch at the session its re-run actually used, \
             not leave it pinned to the first run's session forever"
        );
        assert!(updated.last_run_at.is_some());
    }

    #[tokio::test]
    async fn run_due_watches_only_touches_the_watch_whose_job_actually_started() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        let space_id = a.active_space.id.clone();

        let first_session =
            a.db.create_session("first run", "openai/gpt-5-mini", &space_id)
                .unwrap();
        let second_session =
            a.db.create_session("second run", "openai/gpt-5-mini", &space_id)
                .unwrap();
        let watch_a =
            a.db.create_watch(&space_id, "rust async runtimes", 24, &first_session.id)
                .unwrap();
        let watch_b =
            a.db.create_watch(&space_id, "wasm gc proposal", 24, &second_session.id)
                .unwrap();

        // Only one research job can run at a time — the pipeline's guard
        // (`research_rx.is_some()`) fires for the 2nd+ watch in this
        // synchronous startup loop, since nothing clears `research_rx` until
        // a background job later completes. So watch_a's run actually
        // starts (fresh session, repointed + touched); watch_b's call is a
        // no-op (guard fires) and must be left exactly as it was — still due
        // — for the next startup.
        a.run_due_watches();

        let updated_a =
            a.db.list_all_watches()
                .unwrap()
                .into_iter()
                .find(|w| w.id == watch_a)
                .unwrap();
        let updated_b =
            a.db.list_all_watches()
                .unwrap()
                .into_iter()
                .find(|w| w.id == watch_b)
                .unwrap();

        assert_ne!(
            updated_a.session_id, first_session.id,
            "watch_a's job actually started, so it should be repointed at its new session"
        );
        assert!(
            updated_a.last_run_at.is_some(),
            "watch_a's job actually started, so it should be touched"
        );

        assert_eq!(
            updated_b.session_id, second_session.id,
            "watch_b's job never started (guard fired) — it must not be repointed"
        );
        assert!(
            updated_b.last_run_at.is_none(),
            "watch_b's job never started (guard fired) — touching it would falsely mark it caught up \
             and make it silently skip a full interval"
        );
    }

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
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T00:00:00+00:00")
            .unwrap()
            .to_utc();
        assert_eq!(due_watches(&[w], now).len(), 1);
    }

    #[test]
    fn watch_run_recently_is_not_due() {
        let w = watch("topic", 24, Some("2026-07-07T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T05:00:00+00:00")
            .unwrap()
            .to_utc();
        assert!(due_watches(&[w], now).is_empty());
    }

    #[test]
    fn watch_past_its_interval_is_due() {
        let w = watch("topic", 24, Some("2026-07-06T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T01:00:00+00:00")
            .unwrap()
            .to_utc();
        assert_eq!(due_watches(&[w], now).len(), 1);
    }

    #[test]
    fn diff_section_lists_new_sources_when_present() {
        let section = diff_section(
            "# Old Report\nOld body.",
            "# New Report\nNew body.",
            &["https://new-source.example".to_string()],
        );
        assert!(
            section.contains("What changed since last run"),
            "{section:?}"
        );
        assert!(
            section.contains("https://new-source.example"),
            "{section:?}"
        );
    }

    #[test]
    fn diff_section_empty_new_sources_still_produces_a_header() {
        let section = diff_section("old", "new", &[]);
        assert!(section.contains("What changed since last run"));
        assert!(!section.contains("New sources"));
    }

    #[test]
    fn new_sources_since_filters_out_previously_cited_urls() {
        let new_report =
            "Body [1][2].\n\n## Sources\n1. https://old.example/a\n2. https://fresh.example/b\n";
        let previous = vec!["https://old.example/a".to_string()];
        let new_sources = new_sources_since(new_report, &previous);
        assert_eq!(new_sources, vec!["https://fresh.example/b".to_string()]);
    }

    #[test]
    fn previous_citations_for_watch_session_is_scoped_to_the_watchs_own_reports() {
        let a = test_app();
        let space_id = a.active_space.id.clone();
        let session =
            a.db.create_session("rust async runtimes", "openai/gpt-5-mini", &space_id)
                .unwrap();
        let watch_id =
            a.db.create_watch(&space_id, "rust async runtimes", 24, &session.id)
                .unwrap();
        let _ = watch_id;

        // This watch's own prior report (named per `save_research_report`'s
        // `research-<slug>-<timestamp>.md` scheme, slug derived from its topic).
        let slug = super::super::sessions::slugify("rust async runtimes");
        a.db.add_citations(
            &space_id,
            &format!("research-{slug}-20260101-000000.md"),
            &[("https://own-report.example".to_string(), None)],
        )
        .unwrap();

        // An unrelated report elsewhere in the *same space* (a plain
        // `/research` session, or another watch's topic) must not pollute
        // this watch's diff.
        a.db.add_citations(
            &space_id,
            "research-some-other-topic-20260101-000000.md",
            &[("https://unrelated.example".to_string(), None)],
        )
        .unwrap();

        let prev = a
            .previous_citations_for_watch_session(&session.id, &space_id)
            .unwrap();
        let prev = prev.expect("watch session with prior citations should yield Some");
        assert_eq!(prev, vec!["https://own-report.example".to_string()]);
        assert!(
            !prev.contains(&"https://unrelated.example".to_string()),
            "an unrelated citation elsewhere in the space must not suppress a source as \
             already-cited for this watch: {prev:?}"
        );
    }

    #[test]
    fn new_sources_since_normalizes_urls_before_comparing() {
        // Trailing slash / scheme case differences shouldn't count as "new".
        let new_report = "Body [1].\n\n## Sources\n1. https://Old.example/a/\n";
        let previous = vec!["https://old.example/a".to_string()];
        assert!(new_sources_since(new_report, &previous).is_empty());
    }

    #[test]
    fn previous_citations_for_watch_session_prefix_collision_doesnt_match_longer_slugs() {
        let a = test_app();
        let space_id = a.active_space.id.clone();

        // Two watches: one with topic "rust", another with "rust async".
        // Their slugs are "rust" and "rust-async" respectively.
        let session_rust =
            a.db.create_session("rust", "openai/gpt-5-mini", &space_id)
                .unwrap();
        let _watch_rust =
            a.db.create_watch(&space_id, "rust", 24, &session_rust.id)
                .unwrap();

        let session_rust_async =
            a.db.create_session("rust async", "openai/gpt-5-mini", &space_id)
                .unwrap();
        let _watch_rust_async =
            a.db.create_watch(&space_id, "rust async", 24, &session_rust_async.id)
                .unwrap();

        // Add citations to the "rust async" watch (the one with the longer slug).
        // Its report file starts with "research-rust-async-" then the timestamp.
        let slug_rust_async = super::super::sessions::slugify("rust async");
        a.db.add_citations(
            &space_id,
            &format!("research-{slug_rust_async}-20260101-000000.md"),
            &[("https://rust-async-report.example".to_string(), None)],
        )
        .unwrap();

        // The "rust" watch should NOT see the "rust async" watch's citations,
        // even though "research-rust-async-..." starts with "research-rust-".
        // With the bug unfixed, the "rust" watch would incorrectly pull in the
        // "rust-async" watch's citations since "research-rust-async-20260101-000000.md"
        // starts with the prefix "research-rust-".
        let prev_rust = a
            .previous_citations_for_watch_session(&session_rust.id, &space_id)
            .unwrap();
        assert!(
            prev_rust.is_none(),
            "rust watch should return None (no prior citations for itself), \
             not see rust-async watch's citations (would show prefix collision bug): {prev_rust:?}"
        );
    }
}
