//! The `/swarm` popup's flow half: roster cursor/mode state and the $EDITOR
//! persona handoff. `start_swarm_turn`/`stop_swarm`/`on_swarm_update` and the
//! roster persistence stay in core.

use anyhow::Result;

use nexus_core::app::{PendingEditor, Popup, SwarmPopupMode};
use nexus_core::db::Persona;

use crate::app_view::AppView;

impl AppView {
    pub fn open_swarm_popup(&mut self) {
        let Some(session) = &self.core.session else {
            self.push_status("start a chat first, then /swarm".to_string());
            return;
        };
        self.core.swarm_cache = self
            .core
            .db
            .list_swarm_personas(&session.id)
            .unwrap_or_default();
        self.swarm_selected = 0;
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.popup = Popup::Swarm;
    }

    pub fn move_swarm_selection(&mut self, delta: i32) {
        self.swarm_selected =
            nexus_core::app::clamp_cursor(self.swarm_selected, self.core.swarm_cache.len(), delta);
    }

    /// Queue the selected persona (or a new row) as a small structured file
    /// for `$EDITOR`. Format: `name`, `model`, `---`, then free-form blurb.
    pub fn queue_swarm_persona_editor(&mut self, new: bool) -> Result<()> {
        if new {
            self.core.swarm_cache.push(Persona {
                name: String::new(),
                model: self.core.current_model.clone().unwrap_or_default(),
                blurb: String::new(),
            });
            self.swarm_selected = self.core.swarm_cache.len() - 1;
        }
        let Some(persona) = self.core.swarm_cache.get(self.swarm_selected) else {
            return Ok(());
        };
        let path =
            std::env::temp_dir().join(format!("nexus-chat-persona-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            format!(
                "name: {}\nmodel: {}\n---\n{}\n",
                persona.name, persona.model, persona.blurb
            ),
        )?;
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.pending_editor = Some(PendingEditor::Persona(path));
        self.push_status("opening persona in $EDITOR…".to_string());
        Ok(())
    }

    /// Apply a persona file after `$EDITOR` exits. Leaving a newly-created row
    /// unnamed cancels it; malformed existing edits leave the roster intact.
    pub fn apply_swarm_persona_editor(&mut self, path: &std::path::Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let _ = std::fs::remove_file(path);
        match nexus_core::app::parse_persona_editor(&text) {
            Ok(persona) => {
                if let Some(row) = self.core.swarm_cache.get_mut(self.swarm_selected) {
                    *row = persona;
                }
                self.core.save_swarm_roster()?;
                self.clamp_swarm_selection();
                self.push_status("persona saved".to_string());
            }
            Err(e) => {
                if self
                    .core
                    .swarm_cache
                    .get(self.swarm_selected)
                    .is_some_and(|p| p.name.trim().is_empty())
                {
                    self.core.swarm_cache.remove(self.swarm_selected);
                    self.core.save_swarm_roster()?;
                    self.clamp_swarm_selection();
                }
                self.push_status(format!("persona edit ignored: {e}"));
            }
        }
        Ok(())
    }

    pub fn swarm_remove_row(&mut self) -> Result<()> {
        if self.swarm_selected < self.core.swarm_cache.len() {
            self.core.swarm_cache.remove(self.swarm_selected);
        }
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.core.save_swarm_roster()?;
        self.clamp_swarm_selection();
        Ok(())
    }

    fn clamp_swarm_selection(&mut self) {
        self.swarm_selected = self
            .swarm_selected
            .min(self.core.swarm_cache.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::app::{AppCommand, ModelPickTarget};
    use nexus_core::db::Db;
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-flow-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn test_app() -> AppView {
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("sk-or-test-key"),
            test_space(),
        ))
    }

    #[tokio::test]
    async fn swarm_popup_add_edit_remove_and_toggle_persist_to_db() {
        let mut a = test_app();
        a.core.current_model = Some("a/one".into());
        a.execute(AppCommand::Send {
            text: "hi".to_string(),
        })
        .unwrap();
        let sid = a.session.as_ref().unwrap().id.clone();

        a.run_command("swarm").unwrap();
        assert_eq!(a.popup, Popup::Swarm);
        assert!(a.core.swarm_cache.is_empty());

        a.queue_swarm_persona_editor(true).unwrap();
        let PendingEditor::Persona(path) = a.pending_editor.take().unwrap() else {
            panic!("expected persona editor request");
        };
        std::fs::write(
            &path,
            "name: Skeptic\nmodel: a/one\n---\npokes holes in every claim\n",
        )
        .unwrap();
        a.apply_swarm_persona_editor(&path).unwrap();
        assert_eq!(a.core.swarm_cache.len(), 1);
        assert_eq!(a.core.swarm_cache[0].name, "Skeptic");
        assert_eq!(a.core.swarm_cache[0].model, "a/one");
        assert_eq!(a.core.swarm_cache[0].blurb, "pokes holes in every claim");

        // Persisted immediately, not just held in the popup's cache.
        let stored = a.db.list_swarm_personas(&sid).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].blurb, "pokes holes in every claim");

        assert!(!a.session.as_ref().unwrap().swarm_mode);
        a.toggle_swarm_mode().unwrap();
        assert!(a.session.as_ref().unwrap().swarm_mode);
        assert!(a.db.get_session(&sid).unwrap().unwrap().swarm_mode);

        a.swarm_remove_row().unwrap();
        assert!(a.core.swarm_cache.is_empty());
        assert!(a.db.list_swarm_personas(&sid).unwrap().is_empty());
    }

    #[test]
    fn swarm_persona_round_trips_through_external_editor_file() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let session =
            a.db.create_session("swarm", "a/one", &space, "chat")
                .unwrap();
        a.core.session = Some(session);
        a.core.swarm_cache = vec![nexus_core::db::Persona {
            name: "Skeptic".into(),
            model: "a/one".into(),
            blurb: "pokes holes".into(),
        }];

        a.queue_swarm_persona_editor(false).unwrap();
        let PendingEditor::Persona(path) = a.pending_editor.take().unwrap() else {
            panic!("expected persona editor request");
        };
        let template = std::fs::read_to_string(&path).unwrap();
        assert!(template.contains("name: Skeptic"));
        assert!(template.contains("model: a/one"));
        std::fs::write(
            &path,
            "name: Critic\nmodel: codex:gpt-5.4-mini\n---\nchecks evidence\n",
        )
        .unwrap();
        a.apply_swarm_persona_editor(&path).unwrap();

        assert_eq!(a.core.swarm_cache[0].name, "Critic");
        assert_eq!(a.core.swarm_cache[0].model, "codex:gpt-5.4-mini");
        assert_eq!(a.core.swarm_cache[0].blurb, "checks evidence");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn swarm_persona_model_picker_stays_open_while_catalog_loads() {
        let mut a = test_app();
        a.models.clear();
        a.popup = Popup::Swarm;
        a.core.swarm_cache.push(nexus_core::db::Persona {
            name: "Skeptic".into(),
            model: "a/one".into(),
            blurb: "pokes holes".into(),
        });

        a.open_model_picker_for_swarm_persona(0);

        assert_eq!(a.popup, Popup::Model);
        assert!(matches!(
            a.model_pick_target,
            ModelPickTarget::SwarmPersona(0)
        ));
        assert!(a.last_status().contains("loading models"));
    }

    #[tokio::test]
    async fn swarm_add_row_then_cancel_without_naming_drops_the_blank_row() {
        let mut a = test_app();
        a.core.current_model = Some("a/one".into());
        a.execute(AppCommand::Send {
            text: "hi".to_string(),
        })
        .unwrap();

        a.run_command("swarm").unwrap();
        a.queue_swarm_persona_editor(true).unwrap();
        assert_eq!(a.core.swarm_cache.len(), 1);
        let PendingEditor::Persona(path) = a.pending_editor.take().unwrap() else {
            panic!("expected persona editor request");
        };
        a.apply_swarm_persona_editor(&path).unwrap(); // unchanged blank name cancels
        assert!(a.core.swarm_cache.is_empty());
    }
}
