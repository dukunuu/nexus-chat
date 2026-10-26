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

use super::{App, SEARCH_PROVIDERS, VERBOSITY_LEVELS};

impl App {
    /// Set one named setting by key, persisting it and applying it live —
    /// the host's `SetSetting` command. Unlike
    /// `load_settings` (which ignores unknown persisted rows), this fails
    /// fast: an unknown key or an invalid value for a constrained key is an
    /// error, never a silent no-op reported as success.
    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        if is_removed_setting(key) {
            bail!("{}", removed_setting_message(key));
        }
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
        if key == "ocr_images" {
            self.rescan_files();
        }
        self.push_status(format!("{key} set"));
        Ok(())
    }
}

/// Every key `set_setting` accepts — the same keys `apply_setting` (and the
/// `blocked_domains` special case) can actually apply.
const SETTING_KEYS: [&str; 20] = [
    "remote_images",
    "show_stats",
    "show_reasoning",
    "hide_hints",
    "usage_range",
    "temperature",
    "top_p",
    "max_tokens",
    "helper_model",
    "ocr_images",
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

/// Per-feature model settings that earlier builds accepted and that are now
/// automatic. Stored values are ignored on read (other devices' sync data may
/// still carry them) and rejected on set.
const REMOVED_SETTING_KEYS: [&str; 8] = [
    "memory_model",
    "transcriber_model",
    "ocr_model",
    "ocr_engine",
    "local_ocr_model",
    "embedding_model",
    "image_gen_model",
    "video_gen_model",
];

/// Whether `key` is one of the removed per-feature model settings.
#[must_use]
pub fn is_removed_setting(key: &str) -> bool {
    REMOVED_SETTING_KEYS.contains(&key)
}

/// The error for setting a removed key, naming what replaced it.
#[must_use]
pub fn removed_setting_message(key: &str) -> String {
    let now = match key {
        "memory_model" | "transcriber_model" | "ocr_model" => "use helper_model",
        "ocr_engine" | "local_ocr_model" => "OCR now picks tesseract or the helper model itself",
        "embedding_model" => "embeddings are chosen automatically",
        _ => "media generation uses the active backend's default model",
    };
    format!("setting {key} was removed: {now}")
}

/// The helper model a pre-`helper_model` database implies: its
/// `memory_model`, when `helper_model` is not set yet and the stored value was
/// a deliberate pick. Builds before this one wrote the backend's default into
/// `memory_model` the first time `/config` closed, so a value equal to a
/// default is auto, not a choice.
pub(super) fn legacy_helper_model(stored: &[(String, String)]) -> Option<String> {
    let get = |key: &str| stored.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim());
    if get("helper_model").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let legacy = get("memory_model").filter(|v| !v.is_empty())?;
    (!is_default_helper_id(legacy)).then(|| legacy.to_string())
}

/// Memory-model defaults older builds wrote into `memory_model` (before the
/// helper table moved to current models). Stored copies of these are auto too.
const PAST_HELPER_DEFAULTS: [(crate::provider::BackendTag, &str); 4] = {
    use crate::provider::BackendTag;
    [
        (BackendTag::OpenRouter, "google/gemini-2.5-flash-lite"),
        (BackendTag::OpenAi, "gpt-4.1-mini"),
        (BackendTag::Codex, "gpt-5.6-luna"),
        (BackendTag::OpencodeGo, "deepseek-v4-flash"),
    ]
};

/// Whether `id` (bare or backend-prefixed) is some backend's auto helper,
/// current or past.
fn is_default_helper_id(id: &str) -> bool {
    use crate::provider::BackendTag;
    use crate::provider::openrouter::OpenRouter;
    [
        BackendTag::OpenRouter,
        BackendTag::OpenAi,
        BackendTag::Codex,
        BackendTag::OpencodeGo,
    ]
    .into_iter()
    .map(|tag| (tag, OpenRouter::default_helper_for(tag)))
    .chain(PAST_HELPER_DEFAULTS)
    .any(|(tag, default)| id == default || id == format!("{}{default}", tag.key_prefix()))
}

/// Narrowest and widest the conversation column can be set to, in cells.
pub const COLUMN_WIDTH_MIN: u16 = 60;
pub const COLUMN_WIDTH_MAX: u16 = 400;

/// Smallest and largest a popup dimension can be, as a percentage of the
/// screen.
pub const POPUP_SIZE_MIN: u16 = 20;
pub const POPUP_SIZE_MAX: u16 = 100;

/// Every popup whose size can be overridden — the ids `popup_sizes` accepts.
pub const POPUP_IDS: [&str; 20] = [
    "model",
    "session",
    "copy",
    "key",
    "settings",
    "space",
    "skills",
    "files",
    "apps",
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
        "ocr_images" => matches!(value, "" | "true" | "false"),
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
        "verbosity" => VERBOSITY_LEVELS.contains(&value),
        "reply_style" => super::REPLY_STYLES.contains(&value),
        "image_viewer" => super::IMAGE_VIEWERS.contains(&value),
        "search_provider" => SEARCH_PROVIDERS.contains(&value),
        "column_width" => valid_column_width(value),
        "popup_sizes" => valid_popup_sizes(value),
        _ => true, // free-form strings
    }
}
