// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::{Result, bail};

use super::{
    App, OCR_ENGINES, Popup, SEARCH_PROVIDERS, SETTINGS_GROUPS, SettingsField, SettingsRow,
    VERBOSITY_LEVELS,
};

impl App {
    pub fn open_settings(&mut self) {
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
                SettingsField::ShowStats => self.settings.show_stats = !self.settings.show_stats,
                SettingsField::ShowReasoning => {
                    self.settings.show_reasoning = !self.settings.show_reasoning;
                    self.pin_viewport_top = true;
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
    pub fn cycle_ocr_engine(&mut self) -> Result<()> {
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
    pub fn toggle_reasoning_view(&mut self) -> Result<()> {
        self.settings.show_reasoning = !self.settings.show_reasoning;
        self.pin_viewport_top = true;
        self.db.set_setting(
            "show_reasoning",
            if self.settings.show_reasoning {
                "1"
            } else {
                "0"
            },
        )?;
        self.push_status(if self.settings.show_reasoning {
            "reasoning expanded".to_string()
        } else {
            "reasoning collapsed".to_string()
        });
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
        self.embedding_model = self.settings_inputs[6].trim().to_string();
        self.db
            .set_setting("embedding_model", &self.embedding_model)?;
        self.image_gen_model = self.image_gen_model.trim().to_string();
        self.db
            .set_setting("image_gen_model", &self.image_gen_model)?;
        // Per-space (not a db setting): lives next to the space's other
        // config files so it travels with the space.
        let _ = std::fs::write(
            self.space.blocked_domains_path(&self.active_space.name),
            self.settings_inputs[7].trim(),
        );
        self.refresh_toolbox();
        self.popup = Popup::None;
        self.push_status("settings saved".to_string());
        Ok(())
    }

    /// Set one named setting by key, persisting it and applying it live —
    /// the `SetSetting` command the host (and later the TUI) uses. Unlike
    /// `load_settings` (which ignores unknown persisted rows), this fails
    /// fast: an unknown key or an invalid value for a constrained key is an
    /// error, never a silent no-op reported as success.
    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        if !SETTING_KEYS.contains(&key) {
            bail!("unknown setting: {key}");
        }
        if !valid_setting_value(key, value) {
            bail!("invalid value for {key}: {value:?}");
        }
        self.apply_setting(key, value);
        if key == "blocked_domains" {
            // Per-space (not a db setting): lives next to the space's other
            // config files so it travels with the space.
            std::fs::write(
                self.space.blocked_domains_path(&self.active_space.name),
                value,
            )?;
        } else {
            self.db.set_setting(key, value)?;
        }
        self.refresh_toolbox();
        self.push_status(format!("{key} set"));
        Ok(())
    }
}

/// Every key `set_setting` accepts — the same keys `apply_setting` (and the
/// `blocked_domains` special case) can actually apply.
const SETTING_KEYS: [&str; 21] = [
    "show_stats",
    "show_reasoning",
    "hide_hints",
    "usage_range",
    "temperature",
    "top_p",
    "max_tokens",
    "memory_model",
    "transcriber_model",
    "ocr_model",
    "ocr_engine",
    "local_ocr_model",
    "embedding_model",
    "image_gen_model",
    "video_gen_model",
    "compact_threshold",
    "searxng_url",
    "verbosity",
    "langsearch_key",
    "search_provider",
    "blocked_domains",
];

/// Whether `value` is one `apply_setting` will actually apply for `key` —
/// constrained keys must carry valid values, so a typo'd value can't be
/// persisted as a no-op while reporting success.
fn valid_setting_value(key: &str, value: &str) -> bool {
    match key {
        "show_stats" | "show_reasoning" | "hide_hints" => matches!(value, "0" | "1"),
        "temperature" | "top_p" => value.parse::<f32>().is_ok(),
        "max_tokens" => value.parse::<u32>().is_ok(),
        "compact_threshold" => value.parse::<u8>().is_ok(),
        "usage_range" => crate::db::UsageRange::CYCLE
            .iter()
            .any(|r| r.key() == value),
        "ocr_engine" => OCR_ENGINES.contains(&value),
        "verbosity" => VERBOSITY_LEVELS.contains(&value),
        "search_provider" => SEARCH_PROVIDERS.contains(&value),
        _ => true, // free-form strings
    }
}
