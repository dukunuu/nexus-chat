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
    /// in the background — `ocr_local_install` itself flips the engine to "local" and persists it
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
    /// the host's `SetSetting` command. Unlike
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
const SETTING_KEYS: [&str; 26] = [
    "remote_images",
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
    "reply_style",
    "langsearch_key",
    "search_provider",
    "blocked_domains",
    "column_width",
    "popup_sizes",
    "image_viewer",
];

/// Narrowest and widest the conversation column can be set to, in cells.
pub const COLUMN_WIDTH_MIN: u16 = 60;
pub const COLUMN_WIDTH_MAX: u16 = 400;

/// Smallest and largest a popup dimension can be, as a percentage of the
/// screen.
pub const POPUP_SIZE_MIN: u16 = 20;
pub const POPUP_SIZE_MAX: u16 = 100;

/// Every popup whose size can be overridden — the ids `popup_sizes` accepts.
pub const POPUP_IDS: [&str; 21] = [
    "model",
    "session",
    "copy",
    "key",
    "settings",
    "space",
    "skills",
    "files",
    "apps",
    "terminal",
    "watches",
    "research",
    "swarm",
    "usage",
    "jobs",
    "harness",
    "procs",
    "login",
    "local",
    "help",
    "connectors",
];

/// `auto`, `full`, or a plain cell count in range.
pub(super) fn valid_column_width(value: &str) -> bool {
    matches!(value, "auto" | "full")
        || (!value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && value
                .parse::<u16>()
                .is_ok_and(|n| (COLUMN_WIDTH_MIN..=COLUMN_WIDTH_MAX).contains(&n)))
}

/// One `id:WWxHH` entry, if it names a known popup with in-range percentages.
fn parse_popup_entry(entry: &str) -> Option<(&'static str, u16, u16)> {
    let (id, dims) = entry.trim().split_once(':')?;
    let id = POPUP_IDS.iter().copied().find(|known| *known == id)?;
    let (w, h) = dims.split_once('x')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(w) || !digits(h) {
        return None;
    }
    let range = POPUP_SIZE_MIN..=POPUP_SIZE_MAX;
    let (w, h) = (w.parse::<u16>().ok()?, h.parse::<u16>().ok()?);
    (range.contains(&w) && range.contains(&h)).then_some((id, w, h))
}

/// The valid entries of a `popup_sizes` string; the last entry for an id wins.
fn popup_entries(sizes: &str) -> Vec<(&'static str, u16, u16)> {
    let mut entries: Vec<(&'static str, u16, u16)> = Vec::new();
    for (id, w, h) in sizes.split(',').filter_map(parse_popup_entry) {
        entries.retain(|(known, ..)| *known != id);
        entries.push((id, w, h));
    }
    entries
}

fn join_popup_entries(entries: &[(&'static str, u16, u16)]) -> String {
    entries
        .iter()
        .map(|(id, w, h)| format!("{id}:{w}x{h}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The persisted (width %, height %) for popup `id`, if it has an override.
#[must_use]
pub fn popup_size(sizes: &str, id: &str) -> Option<(u16, u16)> {
    popup_entries(sizes)
        .into_iter()
        .find(|(known, ..)| *known == id)
        .map(|(_, w, h)| (w, h))
}

/// `sizes` with popup `id` set to `size` (percentages, clamped into range), or
/// its override removed for `None`. Invalid entries are dropped on the way.
#[must_use]
pub fn with_popup_size(sizes: &str, id: &str, size: Option<(u16, u16)>) -> String {
    let mut entries = popup_entries(sizes);
    entries.retain(|(known, ..)| *known != id);
    if let (Some((w, h)), Some(id)) = (size, POPUP_IDS.iter().copied().find(|k| *k == id)) {
        let clamp = |n: u16| n.clamp(POPUP_SIZE_MIN, POPUP_SIZE_MAX);
        entries.push((id, clamp(w), clamp(h)));
    }
    join_popup_entries(&entries)
}

/// A loaded `popup_sizes` value with unknown or invalid entries dropped.
pub(super) fn sanitize_popup_sizes(value: &str) -> String {
    join_popup_entries(&popup_entries(value))
}

/// Strict check for `set_setting`: every entry must be valid (empty is fine).
fn valid_popup_sizes(value: &str) -> bool {
    value.is_empty()
        || value
            .split(',')
            .all(|entry| parse_popup_entry(entry).is_some())
}

/// Whether `value` is one `apply_setting` will actually apply for `key` —
/// constrained keys must carry valid values, so a typo'd value can't be
/// persisted as a no-op while reporting success.
fn valid_setting_value(key: &str, value: &str) -> bool {
    match key {
        "remote_images" => matches!(value, "on" | "off"),
        "show_stats" | "show_reasoning" | "hide_hints" => matches!(value, "0" | "1"),
        "temperature" => {
            value.is_empty()
                || value
                    .parse::<f32>()
                    .is_ok_and(|value| value.is_finite() && (0.0..=2.0).contains(&value))
        }
        "top_p" => {
            value.is_empty()
                || value
                    .parse::<f32>()
                    .is_ok_and(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        }
        "max_tokens" => value.is_empty() || value.parse::<u32>().is_ok_and(|value| value > 0),
        "compact_threshold" => value.parse::<u8>().is_ok_and(|value| value <= 100),
        "usage_range" => crate::db::UsageRange::CYCLE
            .iter()
            .any(|r| r.key() == value),
        "ocr_engine" => OCR_ENGINES.contains(&value),
        "verbosity" => VERBOSITY_LEVELS.contains(&value),
        "reply_style" => super::REPLY_STYLES.contains(&value),
        "image_viewer" => super::IMAGE_VIEWERS.contains(&value),
        "search_provider" => SEARCH_PROVIDERS.contains(&value),
        "column_width" => valid_column_width(value),
        "popup_sizes" => valid_popup_sizes(value),
        _ => true, // free-form strings
    }
}
