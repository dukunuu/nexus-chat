//! Saved panel state and pinned panel apps: the domain half. The terminal UI
//! decides which panel is focused and how a pinned app is shown; this half
//! only talks to the durable db, so `nexus host` and the CLI can list, rename
//! and unpin apps through the same commands.

use std::collections::HashMap;

use anyhow::{Result, bail};

use super::App;
use crate::interactive::PanelSnapshot;
use crate::panels::{self, Pinned};

impl App {
    /// Saved panel records of a conversation, by panel key.
    /// # Errors
    /// Returns database failures.
    pub fn panel_states(&self, session_id: &str) -> Result<HashMap<String, String>> {
        panels::load_states(self.db.conn(), session_id)
    }

    /// Save one panel's record. Incognito conversations never reach the db.
    /// # Errors
    /// Returns database failures.
    pub fn save_panel_state(&self, session_id: &str, panel_key: &str, record: &str) -> Result<()> {
        if self.incognito {
            return Ok(());
        }
        panels::save_state(self.db.conn(), session_id, panel_key, record)
    }

    /// Panels pinned as apps in the active space.
    /// # Errors
    /// Returns database failures.
    pub fn pinned_panels(&self) -> Result<Vec<Pinned>> {
        panels::list(self.db.conn(), &self.active_space.id)
    }

    /// A pinned app by id or name.
    /// # Errors
    /// Returns database failures.
    pub fn pinned_panel(&self, reference: &str) -> Result<Option<Pinned>> {
        panels::find(self.db.conn(), &self.active_space.id, reference)
    }

    /// Pin a panel's program and state, choosing a free name from its title
    /// when none is given.
    /// # Errors
    /// Rejects incognito conversations, invalid programs and taken names.
    pub fn pin_panel(
        &mut self,
        name: Option<&str>,
        program: &str,
        snapshot: &PanelSnapshot,
    ) -> Result<Pinned> {
        if self.incognito {
            bail!("pinning is not available in incognito mode");
        }
        let conn = self.db.conn();
        let name = if let Some(name) = name {
            name.to_string()
        } else {
            let title = crate::interactive::Document::parse(program)?
                .title()
                .to_string();
            panels::suggest_name(conn, &self.active_space.id, &title)?
        };
        let app = panels::pin(conn, &self.active_space.id, &name, program, snapshot)?;
        self.push_status(format!(
            "pinned panel as {} · /apps or /tuiapp {}",
            app.name, app.name
        ));
        Ok(app)
    }

    /// Save the state a pinned app was left in.
    /// # Errors
    /// Returns database failures.
    pub fn save_pinned_state(&self, id: &str, record: &str) -> Result<()> {
        panels::save_pinned_state(self.db.conn(), id, record)
    }

    /// Remove a pinned app.
    /// # Errors
    /// Rejects unknown apps.
    pub fn unpin_panel(&mut self, reference: &str) -> Result<()> {
        let app = panels::unpin(self.db.conn(), &self.active_space.id, reference)?;
        self.push_status(format!("unpinned {}", app.name));
        Ok(())
    }

    /// Apply an edit-mode patch to a pinned app (what the model's `panel_app`
    /// tool does, for frontends that edit by hand).
    /// # Errors
    /// Returns the validation failure for an invalid patch.
    pub fn patch_panel(&mut self, reference: &str, patch: &str) -> Result<Pinned> {
        panels::patch(self.db.conn(), &self.active_space.id, reference, patch)
    }

    /// Rename a pinned app.
    /// # Errors
    /// Rejects unknown apps and invalid or taken names.
    pub fn rename_panel(&mut self, from: &str, to: &str) -> Result<()> {
        let app = panels::rename(self.db.conn(), &self.active_space.id, from, to)?;
        self.push_status(format!("renamed to {}", app.name));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::AppCommand;
    use crate::app::tests::app_with_key;
    use crate::interactive::{Document, Session};

    fn snapshot() -> (String, crate::interactive::PanelSnapshot) {
        let program = "root = Panel(\"Weekly Budget!\", [Text($n)])\n$n = 1".to_string();
        let session = Session::new(Document::parse(&program).unwrap()).unwrap();
        (program, session.snapshot())
    }

    #[test]
    fn commands_parse_and_the_catalog_lists_them() {
        let a = app_with_key();
        assert!(crate::app::COMMANDS.iter().any(|c| c.name == "pin-panel"));
        assert_eq!(
            a.parse_command("pin-panel").unwrap(),
            AppCommand::PinPanel { name: None }
        );
        assert_eq!(
            a.parse_command("pin-panel  budget").unwrap(),
            AppCommand::PinPanel {
                name: Some("budget".into())
            }
        );
        assert_eq!(
            a.parse_command("pin").unwrap(),
            AppCommand::PinPanel { name: None }
        );
        assert_eq!(
            a.parse_command("unpin-panel budget").unwrap(),
            AppCommand::UnpinPanel {
                name: "budget".into()
            }
        );
        assert!(a.parse_command("unpin-panel").is_err());
        assert_eq!(
            a.parse_command("pin-panel unpin budget").unwrap(),
            AppCommand::UnpinPanel {
                name: "budget".into()
            }
        );
        for rename in ["rename-panel a b", "pin-panel rename a b"] {
            assert_eq!(
                a.parse_command(rename).unwrap(),
                AppCommand::RenamePanel {
                    from: "a".into(),
                    to: "b".into()
                },
                "{rename}"
            );
        }
        assert!(a.parse_command("rename-panel a").is_err());
        assert!(a.parse_command("pin-panel a b").is_err());
    }

    #[test]
    fn pin_rename_and_unpin_run_through_the_command_seam() {
        let mut a = app_with_key();
        let (program, snapshot) = snapshot();
        let app = a.pin_panel(None, &program, &snapshot).unwrap();
        assert_eq!(app.name, "weekly-budget");
        assert_eq!(a.pinned_panels().unwrap().len(), 1);
        assert!(
            a.pin_panel(Some("weekly-budget"), &program, &snapshot)
                .is_err()
        );

        a.execute(AppCommand::RenamePanel {
            from: "weekly-budget".into(),
            to: "budget".into(),
        })
        .unwrap();
        assert_eq!(a.pinned_panel(&app.id).unwrap().unwrap().name, "budget");
        assert!(
            a.execute(AppCommand::RenamePanel {
                from: "nope".into(),
                to: "x".into()
            })
            .is_err()
        );

        a.execute(AppCommand::UnpinPanel {
            name: "budget".into(),
        })
        .unwrap();
        assert!(a.pinned_panels().unwrap().is_empty());
        assert!(
            a.execute(AppCommand::UnpinPanel {
                name: "budget".into()
            })
            .is_err()
        );
    }

    #[test]
    fn incognito_never_pins_or_saves() {
        let mut a = app_with_key();
        let (program, snapshot) = snapshot();
        a.incognito = true;
        assert!(a.pin_panel(None, &program, &snapshot).is_err());
        a.save_panel_state("s", "k", "{\"v\":1}").unwrap();
        assert!(a.panel_states("s").unwrap().is_empty());
        a.incognito = false;
        a.save_panel_state("s", "k", "{\"v\":1}").unwrap();
        assert_eq!(a.panel_states("s").unwrap().len(), 1);
    }
}
