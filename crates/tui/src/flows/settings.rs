// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;

use nexus_core::app::{
    Popup, SEARCH_PROVIDERS, SETTINGS_GROUPS, SettingsField, SettingsRow, VERBOSITY_LEVELS,
};

use crate::app_view::AppView;

impl AppView {
    pub fn open_settings(&mut self) {
        self.settings_selected = 0;
        self.settings_inputs = [
            self.core
                .settings
                .temperature
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.core
                .settings
                .top_p
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.core
                .settings
                .max_tokens
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.core.settings.compact_threshold.to_string(),
            self.core.searxng_url.clone(),
            self.core.langsearch_key.clone(),
            self.core.embedding_model.clone(),
            std::fs::read_to_string(
                self.core
                    .space
                    .blocked_domains_path(&self.core.active_space.name),
            )
            .unwrap_or_default(),
        ];
        self.push_status(
            "↑/↓ field · type to edit · Space toggles · Ctrl+E system prompt · Esc saves"
                .to_string(),
        );
        self.popup = Popup::Settings;
    }

    /// All currently-visible rows: every group header, plus each group's
    /// fields when that group isn't collapsed.
    pub fn settings_rows(&self) -> Vec<SettingsRow> {
        let mut rows = Vec::new();
        for (i, g) in SETTINGS_GROUPS.iter().enumerate() {
            rows.push(SettingsRow::Group(i));
            if !self.settings_collapsed.contains(&i) {
                rows.extend(g.fields.iter().map(|f| SettingsRow::Field(*f)));
            }
        }
        rows
    }

    pub fn settings_row(&self) -> SettingsRow {
        let rows = self.settings_rows();
        rows.get(self.settings_selected.min(rows.len().saturating_sub(1)))
            .copied()
            .unwrap_or(SettingsRow::Group(0))
    }

    /// The field under the cursor, or `None` when a (possibly collapsed)
    /// group header is selected.
    pub fn settings_field(&self) -> Option<SettingsField> {
        match self.settings_row() {
            SettingsRow::Field(f) => Some(f),
            SettingsRow::Group(_) => None,
        }
    }

    pub fn move_settings_selection(&mut self, delta: i32) {
        let n = self.settings_rows().len() as i32;
        if n == 0 {
            return;
        }
        self.settings_selected = (self.settings_selected as i32 + delta).rem_euclid(n) as usize;
    }

    /// Space (or Enter) on a group header collapses/expands it; on a field it
    /// runs that field's own toggle/cycle behavior, same as before.
    pub fn toggle_settings_field(&mut self) {
        match self.settings_row() {
            SettingsRow::Group(i) => {
                if !self.settings_collapsed.remove(&i) {
                    self.settings_collapsed.insert(i);
                }
            }
            SettingsRow::Field(field) => match field {
                SettingsField::ShowStats => {
                    self.core.settings.show_stats = !self.core.settings.show_stats;
                }
                SettingsField::ShowReasoning => {
                    self.core.settings.show_reasoning = !self.core.settings.show_reasoning;
                    self.pin_viewport_top = true;
                }
                SettingsField::HideHints => {
                    self.core.settings.hide_hints = !self.core.settings.hide_hints;
                }
                SettingsField::Verbosity => {
                    let i = VERBOSITY_LEVELS
                        .iter()
                        .position(|&l| l == self.core.verbosity)
                        .unwrap_or(1);
                    self.core.verbosity =
                        VERBOSITY_LEVELS[(i + 1) % VERBOSITY_LEVELS.len()].to_string();
                }
                SettingsField::SearchProvider => {
                    let i = SEARCH_PROVIDERS
                        .iter()
                        .position(|&p| p == self.core.search_provider)
                        .unwrap_or(0);
                    self.core.search_provider =
                        SEARCH_PROVIDERS[(i + 1) % SEARCH_PROVIDERS.len()].to_string();
                }
                SettingsField::OcrEngine => {
                    let _ = self.core.cycle_ocr_engine();
                }
                _ => {}
            },
        }
    }

    /// Expand/collapse stored reasoning traces (Ctrl+R), persisted.
    pub fn toggle_reasoning_view(&mut self) -> Result<()> {
        self.core.settings.show_reasoning = !self.core.settings.show_reasoning;
        self.pin_viewport_top = true;
        self.core.db.set_setting(
            "show_reasoning",
            if self.core.settings.show_reasoning {
                "1"
            } else {
                "0"
            },
        )?;
        let msg = if self.core.settings.show_reasoning {
            "reasoning expanded".to_string()
        } else {
            "reasoning collapsed".to_string()
        };
        self.push_status(msg);
        Ok(())
    }

    /// Type into the focused field: digits/`.` for the numeric rows, any
    /// printable char for the URL row.
    pub fn settings_input_char(&mut self, c: char) {
        let Some(i) = self.text_index() else { return };
        let numeric = !matches!(
            self.settings_field(),
            Some(
                SettingsField::SearxngUrl
                    | SettingsField::LangsearchKey
                    | SettingsField::EmbeddingModel
                    | SettingsField::BlockedDomains
            )
        );
        if numeric && !(c.is_ascii_digit() || c == '.') {
            return;
        }
        if !numeric && c.is_control() {
            return;
        }
        self.settings_inputs[i].push(c);
    }

    pub fn settings_input_backspace(&mut self) {
        if let Some(i) = self.text_index() {
            self.settings_inputs[i].pop();
        }
    }

    /// Parse the edit buffers into settings and persist everything (on Esc).
    pub fn save_settings(&mut self) -> Result<()> {
        self.core.settings.temperature = self.settings_inputs[0].trim().parse().ok();
        self.core.settings.top_p = self.settings_inputs[1].trim().parse().ok();
        self.core.settings.max_tokens = self.settings_inputs[2].trim().parse().ok();
        self.core.settings.compact_threshold =
            self.settings_inputs[3].trim().parse().unwrap_or(0).min(100);

        let stats = if self.core.settings.show_stats {
            "1"
        } else {
            "0"
        };
        let reason = if self.core.settings.show_reasoning {
            "1"
        } else {
            "0"
        };
        let hints = if self.core.settings.hide_hints {
            "1"
        } else {
            "0"
        };
        self.core.db.set_setting("show_stats", stats)?;
        self.core.db.set_setting("show_reasoning", reason)?;
        self.core.db.set_setting("hide_hints", hints)?;
        self.core
            .db
            .set_setting("temperature", self.settings_inputs[0].trim())?;
        self.core
            .db
            .set_setting("top_p", self.settings_inputs[1].trim())?;
        self.core
            .db
            .set_setting("max_tokens", self.settings_inputs[2].trim())?;
        self.core.db.set_setting(
            "compact_threshold",
            &self.core.settings.compact_threshold.to_string(),
        )?;
        self.core.memory_model = self.core.memory_model.trim().to_string();
        self.core
            .db
            .set_setting("memory_model", &self.core.memory_model)?;
        self.core.transcriber_model = self.core.transcriber_model.trim().to_string();
        self.core
            .db
            .set_setting("transcriber_model", &self.core.transcriber_model)?;
        self.core.searxng_url = self.settings_inputs[4]
            .trim()
            .trim_end_matches('/')
            .to_string();
        self.core
            .db
            .set_setting("searxng_url", &self.core.searxng_url)?;
        self.core
            .db
            .set_setting("verbosity", &self.core.verbosity)?;
        self.core.langsearch_key = self.settings_inputs[5].trim().to_string();
        self.core
            .db
            .set_setting("langsearch_key", &self.core.langsearch_key)?;
        self.core
            .db
            .set_setting("search_provider", &self.core.search_provider)?;
        self.core.ocr_model = self.core.ocr_model.trim().to_string();
        self.core
            .db
            .set_setting("ocr_model", &self.core.ocr_model)?;
        self.core
            .db
            .set_setting("ocr_engine", &self.core.ocr_engine)?;
        self.core.embedding_model = self.settings_inputs[6].trim().to_string();
        self.core
            .db
            .set_setting("embedding_model", &self.core.embedding_model)?;
        self.core.image_gen_model = self.core.image_gen_model.trim().to_string();
        self.core
            .db
            .set_setting("image_gen_model", &self.core.image_gen_model)?;
        // Per-space (not a db setting): lives next to the space's other
        // config files so it travels with the space.
        let _ = std::fs::write(
            self.core
                .space
                .blocked_domains_path(&self.core.active_space.name),
            self.settings_inputs[7].trim(),
        );
        self.core.refresh_toolbox();
        self.popup = Popup::None;
        self.push_status("settings saved".to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
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

    #[test]
    fn searxng_url_setting_persists_and_enables_search_tool() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        // search always works (DuckDuckGo fallback needs no config); only
        // the backend it uses depends on this setting.
        assert!(a.core.toolbox.defs().iter().any(|t| t.name == "search"));

        a.popup = Popup::Settings;
        a.settings_selected = 15; // SearxngUrl
        for c in "http://localhost:8080/".chars() {
            a.settings_input_char(c);
        }
        a.save_settings().unwrap();

        assert_eq!(a.core.searxng_url, "http://localhost:8080"); // trailing slash trimmed
        // The toolbox sits behind the `ToolExecutor` seam now — the URL wiring
        // itself is covered by tools::tests::new_wires_searxng_url_and_langsearch_key.
        assert!(!a.skills.iter().any(|s| s.name == "web-search")); // /web injects prompt text directly

        let reloaded = a.core.db.load_settings().unwrap();
        assert!(
            reloaded
                .iter()
                .any(|(k, v)| k == "searxng_url" && v == "http://localhost:8080")
        );

        // Reloading a fresh App from the same db picks it back up.
        let mut b = AppView::new(App::new(a.core.db, Some("k"), test_space()));
        // Re-apply persisted settings the way `App::new` would (load_settings is
        // private domain plumbing; the public path is a fresh App over the db).
        b.core.refresh_toolbox();
        assert_eq!(b.core.searxng_url, "http://localhost:8080");
    }

    #[test]
    fn verbosity_setting_persists_and_changes_the_prompt() {
        let mut a = test_app();
        a.popup = Popup::Settings;
        a.settings_selected = 4; // Verbosity
        a.toggle_settings_field(); // -> caveman
        a.save_settings().unwrap();
        assert!(a.core.system_prompt().contains("caveman-terse"));

        let reloaded = a.core.db.load_settings().unwrap();
        assert!(
            reloaded
                .iter()
                .any(|(k, v)| k == "verbosity" && v == "caveman")
        );
    }

    #[test]
    fn verbosity_cycles_through_all_three_levels() {
        let mut a = test_app();
        a.popup = Popup::Settings;
        a.settings_selected = 4; // Verbosity
        assert_eq!(a.verbosity, "concise");
        a.toggle_settings_field();
        assert_eq!(a.verbosity, "caveman");
        a.toggle_settings_field();
        assert_eq!(a.verbosity, "normal");
        a.toggle_settings_field();
        assert_eq!(a.verbosity, "concise");
    }

    #[test]
    fn collapsing_a_group_hides_its_fields_and_toggling_header_again_restores_them() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.popup = Popup::Settings;
        let rows_expanded = a.settings_rows().len();
        a.settings_selected = 0; // first group header ("Interface")
        assert!(matches!(a.settings_row(), SettingsRow::Group(0)));
        a.toggle_settings_field(); // collapse it
        let rows_collapsed = a.settings_rows().len();
        assert!(rows_collapsed < rows_expanded);
        a.toggle_settings_field(); // expand it again
        assert_eq!(a.settings_rows().len(), rows_expanded);
    }

    #[test]
    fn settings_edit_and_save_persists() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.popup = Popup::Settings;
        a.settings_selected = 1; // ShowStats (row 0 is the "Interface" group header)
        a.toggle_settings_field();
        assert!(a.core.settings.show_stats);
        a.settings_selected = 6; // Temperature (row 5 is the "Generation" group header)
        for c in "0.7".chars() {
            a.settings_input_char(c);
        }
        a.save_settings().unwrap();
        assert_eq!(a.core.settings.temperature, Some(0.7));

        // reload from db picks up the saved values
        let b = App::new(Db::open_in_memory().unwrap(), Some("k"), test_space());
        let _ = b; // separate in-memory db; just assert current instance loads its own
        let reloaded = a.core.db.load_settings().unwrap();
        assert!(
            reloaded
                .iter()
                .any(|(k, v)| k == "temperature" && v == "0.7")
        );
        assert!(reloaded.iter().any(|(k, v)| k == "show_stats" && v == "1"));
    }
}
