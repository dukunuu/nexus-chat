use anyhow::Result;

use super::{App, WorkMode};
use crate::db::Session;

impl App {
    pub fn activate_notification(&mut self, index: usize) -> Result<()> {
        let Some(notification) = self.notifications.remove(index) else {
            return Ok(());
        };
        self.switch_to_session_by_id(&notification.session_id)
    }

    // --- commands ---

    /// Clear back to a blank conversation. Doesn't touch the db — a session
    /// row is only created lazily on the first message actually sent (same as
    /// the very first message of the app), so `/new` without typing anything
    /// doesn't leave an empty "new chat" behind in the session list.
    pub fn new_session(&mut self) {
        self.session = None;
        self.reset_work_mode();
        self.refresh_toolbox();
        self.messages.clear();
        self.mask_through = 0;
        self.refresh_memory_snapshot();
        // A selection points into the old session's wrapped lines — a stale
        // one would mis-highlight the new chat and resolve links against the
        // wrong messages; the view clears it on `ViewportReset`.
        self.context_total = None;
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        self.push_status("new chat — send a message to start it".to_string());
    }

    /// Back to chat with no workspace: a new or different conversation never
    /// inherits the previous one's directory.
    pub(super) fn reset_work_mode(&mut self) {
        self.work_mode = WorkMode::Chat;
        self.workspace = None;
    }

    fn restore_work_mode(&mut self) {
        self.reset_work_mode();
        if self.incognito {
            return;
        }
        let Some(root) = self
            .session
            .as_ref()
            .and_then(|session| self.db.session_workspace(&session.id))
        else {
            return;
        };
        match self.open_workspace(Some(&root.to_string_lossy())) {
            Ok(workspace) => {
                self.work_mode = if self.session.as_ref().is_some_and(|session| {
                    self.db.session_workspace_mode(&session.id).as_deref() == Some("auto")
                }) {
                    WorkMode::AgentAuto
                } else {
                    WorkMode::Agent
                };
                self.workspace = Some(workspace);
            }
            Err(_) => self.push_status(format!(
                "workspace {} is not available on this device · /agent <path> to bind another",
                root.display()
            )),
        }
    }

    /// Switch to a session by its id. Used by session-link navigation (Ctrl+O),
    /// notification clicks, the `ResolveSession` command, and the session
    /// picker's confirm (via the view layer).
    pub fn switch_to_session_by_id(&mut self, id: &str) -> Result<()> {
        let Some(s) = self
            .db
            .get_session(id)?
            .or_else(|| self.sessions_cache.iter().find(|s| s.id == id).cloned())
        else {
            self.push_status(format!("session not found: {id}"));
            return Ok(());
        };
        self.messages = self.db.load_messages(&s.id)?;
        self.mask_through = if self.incognito {
            0
        } else {
            self.db.session_mask(&s.id)
        };
        self.unread.remove(&s.id);
        self.notifications.retain(|n| n.session_id != s.id);
        self.push_status(format!("switched to: {}", s.title));
        self.current_model = Some(s.model.clone());
        self.web_mode = s.web_mode;
        self.session = Some(s);
        self.restore_work_mode();
        self.refresh_memory_snapshot();
        self.backfill_compaction_row();
        self.restore_survey_gate_prompt();
        self.refresh_toolbox();
        self.context_total = None;
        // Selection + scroll point into the previous session's lines; the
        // view resets them on `ViewportReset`.
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        // Opening an old session should be enough to trigger auto-compaction
        // once its catalog/context window is available. `on_models_result`
        // repeats this check when the model fetch races the session switch.
        self.maybe_compact();
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod procs_tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;
    use crate::workspace::procs::Context;
    use std::sync::Arc;

    #[tokio::test]
    async fn headless_ask_cleans_up_even_on_error_and_allows_another_turn() {
        let root = std::env::temp_dir().join(format!("nexus-procs-ask-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space { root: root.clone() },
        );
        let workspace = Arc::new(crate::workspace::Workspace::open(&root, vec![]).unwrap());
        app.workspace = Some(Arc::clone(&workspace));
        app.work_mode = WorkMode::Agent;
        app.current_model = None;
        for turn in 0..2 {
            let result = app
                .procs
                .action(
                    Context {
                        workspace: &workspace,
                        db_path: Some(&root.join("nexus.db")),
                        confinement: None,
                        session_id: "session",
                        allow_unparseable: false,
                    },
                    "proc_start",
                    r#"{"cmd":"sleep 30","name":"server","wait_s":0}"#,
                )
                .await;
            assert!(
                result.contains("started (not yet ready)"),
                "turn {turn}: {result}"
            );
            if turn == 1 {
                assert!(
                    app.toolbox
                        .run("proc", r#"{"action":"list"}"#)
                        .await
                        .0
                        .contains("server · running")
                );
            }
            let old = Arc::clone(&app.procs);
            assert!(
                app.ask_headless(
                    "question".into(),
                    super::super::headless::TurnOpts {
                        stream: false,
                        quiet: true
                    }
                )
                .await
                .is_err()
            );
            assert_eq!(old.running_count(), 0);
            assert!(!Arc::ptr_eq(&old, &app.procs));
        }
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn processes_survive_new_switches_and_toolbox_refreshes() {
        let root =
            std::env::temp_dir().join(format!("nexus-procs-sessions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let data = root.join("data");
        let workspace_root = root.join("workspace");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&workspace_root).unwrap();
        let mut app = App::new(Db::open_in_memory().unwrap(), None, Space { root: data });
        let first = app
            .db
            .create_session("first", "model", &app.active_space.id, "chat")
            .unwrap();
        let second = app
            .db
            .create_session("second", "model", &app.active_space.id, "chat")
            .unwrap();
        app.switch_to_session_by_id(&first.id).unwrap();
        app.run_command(&format!("agent {}", workspace_root.display()))
            .unwrap();
        let workspace = Arc::clone(app.workspace.as_ref().unwrap());
        for (session, name) in [(&first.id, "server"), (&second.id, "watcher")] {
            let args = serde_json::json!({"cmd":"echo ready; sleep 30", "name":name, "ready_when":"ready"});
            let result = app
                .procs
                .action(
                    Context {
                        workspace: &workspace,
                        db_path: Some(&root.join("nexus.db")),
                        confinement: None,
                        session_id: session,
                        allow_unparseable: false,
                    },
                    "proc_start",
                    &args.to_string(),
                )
                .await;
            assert!(result.contains(" · ready ·"), "{result}");
        }
        app.new_session();
        assert_eq!(app.procs.running_count(), 2);
        app.switch_to_session_by_id(&second.id).unwrap();
        assert_eq!(app.procs.list(&second.id)[0].name, "watcher");
        app.switch_to_session_by_id(&first.id).unwrap();
        let result = app.toolbox.run("proc", r#"{"action":"list"}"#).await.0;
        assert!(result.contains("server · running"), "{result}");
        assert!(result.contains("watcher · running"), "{result}");
        assert_eq!(app.procs.list(&first.id)[0].name, "server");
        app.procs.shutdown().await;
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Best fuzzy score of `needle` against a session's title, slug, and uuid.
pub fn session_score(s: &Session, needle: &str) -> Option<i32> {
    use crate::app::fuzzy_score;
    let mut best = fuzzy_score(&s.title, needle);
    let upd = |best: &mut Option<i32>, cand: Option<i32>| {
        if let Some(c) = cand {
            *best = Some(best.map_or(c, |b| b.max(c)));
        }
    };
    if let Some(slug) = &s.slug {
        upd(&mut best, fuzzy_score(slug, needle).map(|v| v + 2));
    }
    upd(&mut best, fuzzy_score(&s.id, needle));
    best
}

/// Parse the model's topic reply into `(topic, slug)`. Tolerates surrounding prose
/// or code fences by extracting the first `{...}` and reading the two fields.
pub fn parse_topic(text: &str) -> Option<(String, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let json = text.get(start..=end)?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let topic = v.get("topic").and_then(|t| t.as_str())?.trim();
    if topic.is_empty() {
        return None;
    }
    let raw_slug = v.get("id").and_then(|s| s.as_str()).unwrap_or(topic);
    Some((topic.to_string(), slugify(raw_slug)))
}

/// Normalise to a short kebab-case slug: lowercase, `[a-z0-9-]`, max 5 words.
pub fn slugify(s: &str) -> String {
    let slug = s
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "chat".to_string()
    } else {
        slug
    }
}
