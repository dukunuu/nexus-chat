use anyhow::Result;

use super::{
    App, Popup, SEARCH_PROVIDERS, SETTINGS_GROUPS, SettingsField, SettingsRow, VERBOSITY_LEVELS,
};

impl App {
    pub(super) fn open_settings(&mut self) {
        self.settings_selected = 0;
        self.settings_inputs = [
            self.settings
                .temperature
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.settings
                .top_p
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.settings
                .max_tokens
                .map(|v| v.to_string())
                .unwrap_or_default(),
            self.settings.compact_threshold.to_string(),
            self.searxng_url.clone(),
            self.langsearch_key.clone(),
            self.embedding_model.clone(),
            std::fs::read_to_string(self.space.blocked_domains_path(&self.active_space.name))
                .unwrap_or_default(),
        ];
        self.status = "↑/↓ field · type to edit · Space toggles · Ctrl+E system prompt · Esc saves"
            .to_string();
        self.popup = Popup::Settings;
    }

    /// All currently-visible rows: every group header, plus each group's
    /// fields when that group isn't collapsed.
    pub(crate) fn settings_rows(&self) -> Vec<SettingsRow> {
        let mut rows = Vec::new();
        for (i, g) in SETTINGS_GROUPS.iter().enumerate() {
            rows.push(SettingsRow::Group(i));
            if !self.settings_collapsed.contains(&i) {
                rows.extend(g.fields.iter().map(|f| SettingsRow::Field(*f)));
            }
        }
        rows
    }

    pub(crate) fn settings_row(&self) -> SettingsRow {
        let rows = self.settings_rows();
        rows.get(self.settings_selected.min(rows.len().saturating_sub(1)))
            .copied()
            .unwrap_or(SettingsRow::Group(0))
    }

    /// The field under the cursor, or `None` when a (possibly collapsed)
    /// group header is selected.
    pub(crate) fn settings_field(&self) -> Option<SettingsField> {
        match self.settings_row() {
            SettingsRow::Field(f) => Some(f),
            SettingsRow::Group(_) => None,
        }
    }

    pub(crate) fn move_settings_selection(&mut self, delta: i32) {
        let n = self.settings_rows().len() as i32;
        if n == 0 {
            return;
        }
        self.settings_selected = (self.settings_selected as i32 + delta).rem_euclid(n) as usize;
    }

    /// Space (or Enter) on a group header collapses/expands it; on a field it
    /// runs that field's own toggle/cycle behavior, same as before.
    pub(crate) fn toggle_settings_field(&mut self) {
        match self.settings_row() {
            SettingsRow::Group(i) => {
                if !self.settings_collapsed.remove(&i) {
                    self.settings_collapsed.insert(i);
                }
            }
            SettingsRow::Field(field) => match field {
                SettingsField::ShowStats => self.settings.show_stats = !self.settings.show_stats,
                SettingsField::ShowReasoning => {
                    self.settings.show_reasoning = !self.settings.show_reasoning
                }
                SettingsField::HideHints => self.settings.hide_hints = !self.settings.hide_hints,
                SettingsField::Verbosity => {
                    let i = VERBOSITY_LEVELS
                        .iter()
                        .position(|&l| l == self.verbosity)
                        .unwrap_or(1);
                    self.verbosity = VERBOSITY_LEVELS[(i + 1) % VERBOSITY_LEVELS.len()].to_string();
                }
                SettingsField::SearchProvider => {
                    let i = SEARCH_PROVIDERS
                        .iter()
                        .position(|&p| p == self.search_provider)
                        .unwrap_or(0);
                    self.search_provider =
                        SEARCH_PROVIDERS[(i + 1) % SEARCH_PROVIDERS.len()].to_string();
                }
                SettingsField::OcrEngine => {
                    let _ = self.cycle_ocr_engine();
                }
                _ => {}
            },
        }
    }

    /// Advance the OCR engine auto → tesseract → vlm → local → auto,
    /// persisted. Cycling into "local" pulls the configured model via ollama
    /// in the background (formerly the separate `/ocr-local` command) —
    /// `ocr_local_install` itself flips the engine to "local" and persists it
    /// once the pull actually succeeds, so a failed pull doesn't leave the
    /// engine silently pointed at a model that was never fetched.
    pub(crate) fn cycle_ocr_engine(&mut self) -> Result<()> {
        let i = super::OCR_ENGINES
            .iter()
            .position(|&e| e == self.ocr_engine)
            .unwrap_or(0);
        let next = super::OCR_ENGINES[(i + 1) % super::OCR_ENGINES.len()];
        if next == "local" {
            self.ocr_local_install("");
            return Ok(());
        }
        self.ocr_engine = next.to_string();
        self.db.set_setting("ocr_engine", &self.ocr_engine)?;
        Ok(())
    }

    /// Expand/collapse stored reasoning traces (Ctrl+R), persisted.
    pub(crate) fn toggle_reasoning_view(&mut self) -> Result<()> {
        self.settings.show_reasoning = !self.settings.show_reasoning;
        self.db.set_setting(
            "show_reasoning",
            if self.settings.show_reasoning {
                "1"
            } else {
                "0"
            },
        )?;
        self.status = if self.settings.show_reasoning {
            "reasoning expanded".to_string()
        } else {
            "reasoning collapsed".to_string()
        };
        Ok(())
    }

    /// Type into the focused field: digits/`.` for the numeric rows, any
    /// printable char for the URL row.
    pub(crate) fn settings_input_char(&mut self, c: char) {
        let Some(i) = self.text_index() else { return };
        let numeric = !matches!(
            self.settings_field(),
            Some(SettingsField::SearxngUrl)
                | Some(SettingsField::LangsearchKey)
                | Some(SettingsField::EmbeddingModel)
                | Some(SettingsField::BlockedDomains)
        );
        if numeric && !(c.is_ascii_digit() || c == '.') {
            return;
        }
        if !numeric && c.is_control() {
            return;
        }
        self.settings_inputs[i].push(c);
    }

    pub(crate) fn settings_input_backspace(&mut self) {
        if let Some(i) = self.text_index() {
            self.settings_inputs[i].pop();
        }
    }

    /// Parse the edit buffers into settings and persist everything (on Esc).
    pub(crate) fn save_settings(&mut self) -> Result<()> {
        self.settings.temperature = self.settings_inputs[0].trim().parse().ok();
        self.settings.top_p = self.settings_inputs[1].trim().parse().ok();
        self.settings.max_tokens = self.settings_inputs[2].trim().parse().ok();
        self.settings.compact_threshold =
            self.settings_inputs[3].trim().parse().unwrap_or(0).min(100);

        let stats = if self.settings.show_stats { "1" } else { "0" };
        let reason = if self.settings.show_reasoning {
            "1"
        } else {
            "0"
        };
        let hints = if self.settings.hide_hints { "1" } else { "0" };
        self.db.set_setting("show_stats", stats)?;
        self.db.set_setting("show_reasoning", reason)?;
        self.db.set_setting("hide_hints", hints)?;
        self.db
            .set_setting("temperature", self.settings_inputs[0].trim())?;
        self.db
            .set_setting("top_p", self.settings_inputs[1].trim())?;
        self.db
            .set_setting("max_tokens", self.settings_inputs[2].trim())?;
        self.db.set_setting(
            "compact_threshold",
            &self.settings.compact_threshold.to_string(),
        )?;
        self.memory_model = self.memory_model.trim().to_string();
        self.db.set_setting("memory_model", &self.memory_model)?;
        self.transcriber_model = self.transcriber_model.trim().to_string();
        self.db
            .set_setting("transcriber_model", &self.transcriber_model)?;
        self.searxng_url = self.settings_inputs[4]
            .trim()
            .trim_end_matches('/')
            .to_string();
        self.db.set_setting("searxng_url", &self.searxng_url)?;
        self.db.set_setting("verbosity", &self.verbosity)?;
        self.langsearch_key = self.settings_inputs[5].trim().to_string();
        self.db
            .set_setting("langsearch_key", &self.langsearch_key)?;
        self.db
            .set_setting("search_provider", &self.search_provider)?;
        self.ocr_model = self.ocr_model.trim().to_string();
        self.db.set_setting("ocr_model", &self.ocr_model)?;
        self.db.set_setting("ocr_engine", &self.ocr_engine)?;
        self.research_model = self.research_model.trim().to_string();
        self.db
            .set_setting("research_model", &self.research_model)?;
        self.escalation_model = self.escalation_model.trim().to_string();
        self.db
            .set_setting("escalation_model", &self.escalation_model)?;
        self.embedding_model = self.settings_inputs[6].trim().to_string();
        self.db
            .set_setting("embedding_model", &self.embedding_model)?;
        // Per-space (not a db setting): lives next to the space's other
        // config files so it travels with the space.
        let _ = std::fs::write(
            self.space.blocked_domains_path(&self.active_space.name),
            self.settings_inputs[7].trim(),
        );
        self.refresh_toolbox();
        self.popup = Popup::None;
        self.status = "settings saved".to_string();
        Ok(())
    }
}
