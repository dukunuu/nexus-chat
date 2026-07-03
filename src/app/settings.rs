use anyhow::Result;

use super::{App, Popup, SettingsField, SEARCH_PROVIDERS, VERBOSITY_LEVELS};

impl App {
    pub(super) fn open_settings(&mut self) {
        self.settings_selected = 0;
        self.settings_inputs = [
            self.settings.temperature.map(|v| v.to_string()).unwrap_or_default(),
            self.settings.top_p.map(|v| v.to_string()).unwrap_or_default(),
            self.settings.max_tokens.map(|v| v.to_string()).unwrap_or_default(),
            self.settings.compact_threshold.to_string(),
            self.searxng_url.clone(),
            self.langsearch_key.clone(),
        ];
        self.status = "↑/↓ field · type to edit · Space toggles · Ctrl+E system prompt · Esc saves".to_string();
        self.popup = Popup::Settings;
    }

    pub(crate) fn settings_field(&self) -> SettingsField {
        SettingsField::ALL[self.settings_selected]
    }

    pub(crate) fn move_settings_selection(&mut self, delta: i32) {
        let n = SettingsField::ALL.len() as i32;
        self.settings_selected = (self.settings_selected as i32 + delta).rem_euclid(n) as usize;
    }

    pub(crate) fn toggle_settings_field(&mut self) {
        match self.settings_field() {
            SettingsField::ShowStats => self.settings.show_stats = !self.settings.show_stats,
            SettingsField::ShowReasoning => {
                self.settings.show_reasoning = !self.settings.show_reasoning
            }
            SettingsField::HideHints => self.settings.hide_hints = !self.settings.hide_hints,
            SettingsField::Verbosity => {
                let i = VERBOSITY_LEVELS.iter().position(|&l| l == self.verbosity).unwrap_or(1);
                self.verbosity = VERBOSITY_LEVELS[(i + 1) % VERBOSITY_LEVELS.len()].to_string();
            }
            SettingsField::SearchProvider => {
                let i = SEARCH_PROVIDERS.iter().position(|&p| p == self.search_provider).unwrap_or(0);
                self.search_provider = SEARCH_PROVIDERS[(i + 1) % SEARCH_PROVIDERS.len()].to_string();
            }
            _ => {}
        }
    }

    /// Expand/collapse stored reasoning traces (Ctrl+R), persisted.
    pub(crate) fn toggle_reasoning_view(&mut self) -> Result<()> {
        self.settings.show_reasoning = !self.settings.show_reasoning;
        self.db.set_setting(
            "show_reasoning",
            if self.settings.show_reasoning { "1" } else { "0" },
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
        let numeric = !matches!(self.settings_field(), SettingsField::SearxngUrl | SettingsField::LangsearchKey);
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
        let reason = if self.settings.show_reasoning { "1" } else { "0" };
        let hints = if self.settings.hide_hints { "1" } else { "0" };
        self.db.set_setting("show_stats", stats)?;
        self.db.set_setting("show_reasoning", reason)?;
        self.db.set_setting("hide_hints", hints)?;
        self.db.set_setting("temperature", self.settings_inputs[0].trim())?;
        self.db.set_setting("top_p", self.settings_inputs[1].trim())?;
        self.db.set_setting("max_tokens", self.settings_inputs[2].trim())?;
        self.db.set_setting("compact_threshold", &self.settings.compact_threshold.to_string())?;
        self.memory_model = self.memory_model.trim().to_string();
        self.db.set_setting("memory_model", &self.memory_model)?;
        self.transcriber_model = self.transcriber_model.trim().to_string();
        self.db.set_setting("transcriber_model", &self.transcriber_model)?;
        self.searxng_url = self.settings_inputs[4].trim().trim_end_matches('/').to_string();
        self.db.set_setting("searxng_url", &self.searxng_url)?;
        self.db.set_setting("verbosity", &self.verbosity)?;
        self.langsearch_key = self.settings_inputs[5].trim().to_string();
        self.db.set_setting("langsearch_key", &self.langsearch_key)?;
        self.db.set_setting("search_provider", &self.search_provider)?;
        self.refresh_toolbox();
        self.popup = Popup::None;
        self.status = "settings saved".to_string();
        Ok(())
    }
}
