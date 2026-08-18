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

use super::{App, OCR_ENGINES, SEARCH_PROVIDERS, VERBOSITY_LEVELS};

impl App {
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
        let embedding_changed =
            key == "embedding_model" && self.embedding_model.trim() != value.trim();
        self.apply_setting(key, value);
        if embedding_changed {
            // Vectors have no model id of their own, so retaining them after a
            // model change can make semantic search silently skip or mis-rank
            // every file. Rebuild them with the new model instead. Dropping
            // the receiver also prevents an old in-flight result from being
            // written back after the clear.
            self.embed_rx = None;
            self.db.clear_chunk_embeddings()?;
        }
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
        if key == "embedding_model" {
            self.start_embedding();
        }
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
