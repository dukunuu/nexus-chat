use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::{
    ChatMessage, ChatParams, Model, ReasoningEffort, StreamEvent, ToolCall, ToolDef, Usage,
};
use crate::tools::ToolBox;

const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const CODEX_BASE: &str = "https://chatgpt.com/backend-api";
/// The general Zen catalog: free-tier and pay-per-token models, plus
/// whatever a Go subscription adds. This is the default/fallback base.
const OPENCODE_ZEN_BASE: &str = "https://opencode.ai/zen/v1";
/// The flat-fee Go-subscription bundle — a *different* endpoint from Zen's
/// general catalog, even though it's the same account key. Requests for a
/// Go-bundled model must go here, not to Zen general, or they'd be billed
/// per-token instead of covered by the flat $10/mo.
const OPENCODE_GO_BASE: &str = "https://opencode.ai/zen/go/v1";
/// Prefix on a Model id (from `list_models`) marking it as a flat-fee Go
/// model rather than a general Zen one — stripped before it's ever sent to
/// the API; only used to pick which of the two bases above to hit.
const OPENCODE_GO_PREFIX: &str = "go:";

/// Context windows for OpenCode Zen's general catalog (`/zen/v1/models`),
/// keyed by raw model id. The Zen `/models` endpoint returns no context
/// metadata (only id/created/owned_by), so without this table every OpenCode
/// model would show no context size. Values mirror the models.dev catalog
/// that opencode itself uses for these ids.
const OPENCODE_ZEN_CONTEXT: &[(&str, u64)] = &[
    ("big-pickle", 200_000),
    ("claude-fable-5", 1_000_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-opus-4-1", 200_000),
    ("claude-opus-4-5", 200_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-sonnet-4", 1_000_000),
    ("claude-sonnet-4-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
    ("deepseek-v4-flash-free", 200_000),
    ("deepseek-v4-pro", 1_000_000),
    ("gemini-3-flash", 1_048_576),
    ("gemini-3.1-pro", 1_048_576),
    ("gemini-3.5-flash", 1_048_576),
    ("gemini-3.5-flash-lite", 1_048_576),
    ("gemini-3.6-flash", 1_048_576),
    ("glm-5", 204_800),
    ("glm-5.1", 204_800),
    ("glm-5.2", 1_000_000),
    ("gpt-5", 400_000),
    ("gpt-5-codex", 400_000),
    ("gpt-5-nano", 400_000),
    ("gpt-5.1", 400_000),
    ("gpt-5.1-codex", 400_000),
    ("gpt-5.1-codex-max", 400_000),
    ("gpt-5.1-codex-mini", 400_000),
    ("gpt-5.2", 400_000),
    ("gpt-5.2-codex", 400_000),
    ("gpt-5.3-codex", 400_000),
    ("gpt-5.3-codex-spark", 128_000),
    ("gpt-5.4", 1_050_000),
    ("gpt-5.4-mini", 400_000),
    ("gpt-5.4-nano", 400_000),
    ("gpt-5.4-pro", 1_050_000),
    ("gpt-5.5", 1_050_000),
    ("gpt-5.5-pro", 1_050_000),
    ("gpt-5.6-luna", 1_050_000),
    ("gpt-5.6-sol", 1_050_000),
    ("gpt-5.6-terra", 1_050_000),
    ("grok-4.5", 500_000),
    ("grok-build-0.1", 256_000),
    ("kimi-k2.5", 262_144),
    ("kimi-k2.6", 262_144),
    ("kimi-k2.7-code", 262_144),
    ("kimi-k3", 1_048_576),
    ("laguna-s-2.1-free", 256_000),
    ("ling-3.0-flash-free", 262_144),
    ("longcat-2.0-free", 1_000_000),
    ("minimax-m2.5", 204_800),
    ("minimax-m2.7", 204_800),
    ("minimax-m3", 512_000),
    ("mimo-v2.5-free", 200_000),
    ("nemotron-3-ultra-free", 1_000_000),
    ("north-mini-code-free", 256_000),
    ("qwen3.5-plus", 262_144),
    ("qwen3.6-plus", 262_144),
];

/// Context windows for the flat-fee Go bundle (`/zen/go/v1/models`), keyed by
/// raw model id. Kept separate from `OPENCODE_ZEN_CONTEXT` because the *same
/// id* can have a different window on each endpoint (e.g. qwen3.6-plus:
/// 262k on Zen general vs 1M on Go; minimax-m3: 512k vs 1M).
const OPENCODE_GO_CONTEXT: &[(&str, u64)] = &[
    ("deepseek-v4-flash", 1_000_000),
    ("deepseek-v4-pro", 1_000_000),
    ("glm-5", 202_752),
    ("glm-5.1", 202_752),
    ("glm-5.2", 1_000_000),
    ("gpt-5.6-luna", 1_050_000),
    ("grok-4.5", 500_000),
    ("hy3", 256_000),
    ("hy3-preview", 256_000),
    ("kimi-k2.5", 262_144),
    ("kimi-k2.6", 262_144),
    ("kimi-k2.7-code", 262_144),
    ("kimi-k3", 1_048_576),
    ("mimo-v2-omni", 262_144),
    ("mimo-v2-pro", 1_048_576),
    ("mimo-v2.5", 1_000_000),
    ("mimo-v2.5-pro", 1_048_576),
    ("minimax-m2.5", 204_800),
    ("minimax-m2.7", 204_800),
    ("minimax-m3", 1_000_000),
    ("qwen3.5-plus", 262_144),
    ("qwen3.6-plus", 1_000_000),
    ("qwen3.7-max", 1_000_000),
    ("qwen3.7-plus", 1_000_000),
    ("qwen3.8-max", 1_000_000),
];
/// Hard cap on tool round-trips per response, so a model that keeps calling
/// tools can't loop forever. The default for interactive chat; background
/// jobs (e.g. deep-research searcher agents) pass their own smaller budget.
pub(crate) const MAX_TOOL_ITERS: usize = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderFlavor {
    OpenRouter,
    OpenAi,
    OpenAiCodex,
    /// OpenCode Go: a low-cost subscription bundling several open coding
    /// models behind a fully OpenAI-compatible endpoint — no special
    /// request/response handling needed, unlike Codex.
    OpencodeGo,
}

#[derive(Clone)]
pub struct OpenRouter {
    client: reqwest::Client,
    key: String,
    flavor: ProviderFlavor,
}

/// Everything needed to generate a video: model, prompt, params, optional
/// frame/reference images, and provider-specific options keyed by provider
/// slug (e.g. `{"alibaba": {"parameters": {"video": "data:..."}}}`),
/// passed through as `provider.options` in the request body.
pub struct VideoRequest {
    pub model: String,
    pub prompt: String,
    pub duration: u32,
    pub resolution: String,
    pub aspect_ratio: String,
    pub generate_audio: bool,
    pub first_frame: Option<Vec<u8>>,
    pub last_frame: Option<Vec<u8>>,
    pub input_references: Vec<Vec<u8>>,
    pub seed: Option<i32>,
    pub provider_options: Option<serde_json::Value>,
}

impl ProviderFlavor {
    fn base(self) -> &'static str {
        match self {
            ProviderFlavor::OpenRouter => OPENROUTER_BASE,
            ProviderFlavor::OpenAi => OPENAI_BASE,
            ProviderFlavor::OpenAiCodex => CODEX_BASE,
            ProviderFlavor::OpencodeGo => OPENCODE_ZEN_BASE,
        }
    }

    /// The reasoning effort values this model accepts, in cycle order.
    /// Empty = the model has no reasoning/thinking mode.
    ///
    /// A catalog-provided `reasoning.supported_efforts` list is authoritative
    /// and wins for every backend that exposes it. Family rules are only
    /// fallbacks for catalogs that report the `reasoning` parameter without
    /// enumerating its accepted values.
    fn reasoning_efforts(self, m: &ModelEntry) -> Vec<ReasoningEffort> {
        use ReasoningEffort as E;
        if let Some(supported) = m
            .reasoning
            .as_ref()
            .map(|r| r.supported_efforts.as_slice())
            .filter(|values| !values.is_empty())
        {
            return E::CYCLE_ORDER
                .iter()
                .copied()
                .filter(|effort| supported.iter().any(|value| value == effort.as_str()))
                .collect();
        }
        match self {
            ProviderFlavor::OpenRouter => {
                if !m.supported_parameters.iter().any(|p| p == "reasoning") {
                    return Vec::new();
                }
                if m.id.starts_with("anthropic/") {
                    E::WITH_MINIMAL.to_vec()
                } else {
                    E::STANDARD.to_vec()
                }
            }
            ProviderFlavor::OpenAi => {
                let id = m.id.as_str();
                if id == "gpt-5-pro" || id.starts_with("gpt-5-pro-") {
                    E::HIGH_ONLY.to_vec()
                } else if id == "gpt-5"
                    || id.starts_with("gpt-5-20")
                    || id.starts_with("gpt-5-mini")
                    || id.starts_with("gpt-5-nano")
                {
                    E::WITH_MINIMAL.to_vec()
                } else if ["gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5"]
                    .iter()
                    .any(|prefix| id.starts_with(prefix))
                {
                    E::WITH_XHIGH_AND_NONE.to_vec()
                } else if id.starts_with("gpt-5.6") {
                    E::WITH_MAX_XHIGH_AND_NONE.to_vec()
                } else if id
                    .strip_prefix('o')
                    .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    || id.starts_with("gpt-5")
                {
                    E::STANDARD.to_vec()
                } else {
                    Vec::new()
                }
            }
            // Codex and the Go bundle are reasoning-capable chat catalogs.
            // Their catalog metadata wins above when present; this standard
            // set is only for entries that expose no per-model detail.
            ProviderFlavor::OpenAiCodex | ProviderFlavor::OpencodeGo => E::STANDARD.to_vec(),
        }
    }

    fn supports_images(self, m: &ModelEntry) -> Option<bool> {
        match self {
            ProviderFlavor::OpenRouter => None,
            ProviderFlavor::OpenAi => {
                let id = m.id.as_str();
                Some(
                    id.contains("gpt-4o")
                        || id.contains("gpt-4.1")
                        || id.starts_with("gpt-5")
                        || id.starts_with("o3")
                        || id.starts_with("o4"),
                )
            }
            ProviderFlavor::OpenAiCodex => Some(true),
            ProviderFlavor::OpencodeGo => None,
        }
    }

    fn supports_image_generation(self, m: &ModelEntry) -> Option<bool> {
        match self {
            // OpenRouter reports output_modalities in the catalog — use that.
            ProviderFlavor::OpenRouter => None,
            // Only dall-e models on OpenAI.
            ProviderFlavor::OpenAi => Some(m.id.contains("dall-e")),
            // Codex and Go don't support image generation.
            ProviderFlavor::OpenAiCodex => Some(false),
            ProviderFlavor::OpencodeGo => Some(false),
        }
    }

    fn add_stream_usage(self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        match self {
            // Ask OpenRouter for exact token accounting in the final chunk.
            ProviderFlavor::OpenRouter => {
                obj.insert("usage".into(), serde_json::json!({ "include": true }));
            }
            // OpenAI (and OpenCode Go, which mirrors OpenAI's API shape)
            // put the equivalent switch under stream_options.
            ProviderFlavor::OpenAi | ProviderFlavor::OpencodeGo => {
                obj.insert(
                    "stream_options".into(),
                    serde_json::json!({ "include_usage": true }),
                );
            }
            ProviderFlavor::OpenAiCodex => {}
        }
    }

    fn add_reasoning_effort(
        self,
        obj: &mut serde_json::Map<String, serde_json::Value>,
        effort: &str,
    ) {
        match self {
            ProviderFlavor::OpenRouter => {
                obj.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
            }
            ProviderFlavor::OpenAi | ProviderFlavor::OpencodeGo => {
                obj.insert("reasoning_effort".into(), serde_json::json!(effort));
            }
            ProviderFlavor::OpenAiCodex => {
                obj.insert(
                    "reasoning".into(),
                    serde_json::json!({ "effort": effort, "summary": "auto" }),
                );
            }
        }
    }
}

fn looks_like_openrouter_key(key: &str) -> bool {
    key.trim_start().starts_with("sk-or-")
}

fn looks_like_codex_token(key: &str) -> bool {
    crate::config::codex_account_id(key).is_ok()
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ImageModelsResponse {
    data: Vec<ImageModelEntry>,
}

#[derive(Deserialize)]
struct ImageModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    architecture: Option<Architecture>,
}

#[derive(Deserialize)]
struct VideoModelsResponse {
    data: Vec<VideoModelEntry>,
}

#[derive(Deserialize)]
struct VideoModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Clone)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    reasoning: Option<ModelReasoningEntry>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    architecture: Option<Architecture>,
}

#[derive(Deserialize, Clone)]
struct ModelReasoningEntry {
    #[serde(default)]
    supported_efforts: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct Architecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

/// Whether the catalog entry accepts image input.
fn entry_supports_images(e: &ModelEntry) -> bool {
    e.architecture
        .as_ref()
        .is_some_and(|a| a.input_modalities.iter().any(|m| m == "image"))
}

/// Whether the catalog entry generates image output.
fn entry_supports_image_gen(e: &ModelEntry) -> bool {
    // The API's output_modalities field is authoritative when populated.
    if e.architecture
        .as_ref()
        .is_some_and(|a| a.output_modalities.iter().any(|m| m == "image"))
    {
        return true;
    }
    // Fallback: match known image-generation model name patterns.
    // These are specific enough to avoid catching chat models from the same
    // provider, unlike broad provider-prefix matching.
    let id = &e.id;
    id.contains("/flux")
        || id.contains("dall-e")
        || id.contains("/stable-diffusion")
        || id == "recraft-20b"
        || id.starts_with("recraft-v")
        || id.contains("/imagen")
        || id.contains("/pixart")
        || id.contains("/playground-v")
        || id == "luma-photon"
        || id.starts_with("luma/")
        || id.starts_with("ideogram/")
        || id.contains("/sdxl")
        || id.contains("hyper-sd")
}

fn entry_supports_video_gen(e: &ModelEntry) -> bool {
    e.architecture
        .as_ref()
        .is_some_and(|a| a.output_modalities.iter().any(|m| m == "video"))
}

fn merge_generation_models(models: &mut Vec<Model>, additions: Vec<Model>) {
    for addition in additions {
        if let Some(existing) = models
            .iter_mut()
            .find(|m| m.backend == addition.backend && m.id == addition.id)
        {
            existing.supports_images |= addition.supports_images;
            existing.supports_image_generation |= addition.supports_image_generation;
            existing.supports_video_generation |= addition.supports_video_generation;
            if existing.name == existing.id && addition.name != addition.id {
                existing.name = addition.name;
            }
        } else {
            models.push(addition);
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
}

impl OpenRouter {
    pub fn from_key_auto(key: String) -> Self {
        if looks_like_openrouter_key(&key) {
            Self::openrouter_flavor(key)
        } else if looks_like_codex_token(&key) {
            Self::openai_codex(key)
        } else {
            Self::openai(key)
        }
    }

    pub fn openrouter_flavor(key: String) -> Self {
        OpenRouter {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpenRouter,
        }
    }

    pub fn openai(key: String) -> Self {
        OpenRouter {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpenAi,
        }
    }

    pub fn opencode_go(key: String) -> Self {
        OpenRouter {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpencodeGo,
        }
    }

    pub fn openai_codex(key: String) -> Self {
        OpenRouter {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpenAiCodex,
        }
    }

    pub fn backend_tag(&self) -> crate::provider::BackendTag {
        match self.flavor {
            ProviderFlavor::OpenRouter => crate::provider::BackendTag::OpenRouter,
            ProviderFlavor::OpenAi => crate::provider::BackendTag::OpenAi,
            ProviderFlavor::OpenAiCodex => crate::provider::BackendTag::Codex,
            ProviderFlavor::OpencodeGo => crate::provider::BackendTag::OpencodeGo,
        }
    }

    /// Video gen is OpenRouter-only (not OpenAI/Codex/OpenCode).
    pub fn is_openrouter(&self) -> bool {
        self.flavor == ProviderFlavor::OpenRouter
    }

    /// Generate a video via OpenRouter's `/api/v1/videos` endpoint.
    /// Submits the job, polls every 10s (up to 6 min), downloads on completion.
    /// Returns `(mp4_bytes, cost_in_usd)`.
    pub async fn generate_video(&self, req: VideoRequest) -> Result<(Vec<u8>, f64)> {
        let VideoRequest {
            model,
            prompt,
            duration,
            resolution,
            aspect_ratio,
            generate_audio,
            first_frame,
            last_frame,
            input_references,
            seed,
            provider_options,
        } = req;
        if !self.is_openrouter() {
            anyhow::bail!("video generation only available on the OpenRouter backend");
        }
        let (base, model_id) = self.opencode_route(&model);
        let (duration, resolution, aspect_ratio) =
            normalize_video_params(&model_id, duration, &resolution, &aspect_ratio);
        check_video_params(&model_id, duration, &resolution, &aspect_ratio)?;

        use base64::Engine;

        let mut body = serde_json::json!({
            "model": model_id,
            "prompt": prompt,
            "duration": duration,
            "resolution": resolution,
            "aspect_ratio": aspect_ratio,
            "generate_audio": generate_audio,
        });

        let mut frames = Vec::new();
        if let Some(data) = first_frame {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            let mime = Self::detect_image_mime(&data);
            frames.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") },
                "frame_type": "first_frame",
            }));
        }
        if let Some(data) = last_frame {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            let mime = Self::detect_image_mime(&data);
            frames.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") },
                "frame_type": "last_frame",
            }));
        }
        if !frames.is_empty() {
            body["frame_images"] = serde_json::Value::Array(frames);
        }

        if !input_references.is_empty() {
            let refs: Vec<serde_json::Value> = input_references
                .iter()
                .map(|data| {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                    let mime = Self::detect_image_mime(data);
                    serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{mime};base64,{b64}") }
                    })
                })
                .collect();
            body["input_references"] = serde_json::Value::Array(refs);
        }

        if let Some(s) = seed {
            body["seed"] = serde_json::json!(s);
        }

        if let Some(po) = provider_options {
            body["provider"] = serde_json::json!({ "options": po });
        }

        let raw_resp = self
            .client
            .post(format!("{base}/videos"))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("video generation request")?;
        let resp: serde_json::Value = if !raw_resp.status().is_success() {
            let status = raw_resp.status();
            let body_text = raw_resp.text().await.unwrap_or_default();
            anyhow::bail!("video generation submission failed (HTTP {status}): {body_text}");
        } else {
            raw_resp
                .json::<serde_json::Value>()
                .await
                .context("parsing video submission response")?
        };

        let job_id = resp
            .get("id")
            .and_then(|i| i.as_str())
            .context("no job id in video generation response")?
            .to_string();

        let poll_url = format!("{base}/videos/{job_id}");
        for _attempt in 0..36 {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let status = self
                .client
                .get(&poll_url)
                .bearer_auth(&self.key)
                .send()
                .await
                .context("video status poll")?
                .error_for_status()
                .context("video status poll failed")?
                .json::<serde_json::Value>()
                .await
                .context("parsing video status response")?;

            match status.get("status").and_then(|s| s.as_str()) {
                Some("completed") => {
                    let cost = status
                        .pointer("/usage/cost")
                        .and_then(|c| c.as_f64())
                        .unwrap_or(0.0);

                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                    let content = self
                        .client
                        .get(format!("{base}/videos/{job_id}/content?index=0"))
                        .bearer_auth(&self.key)
                        .send()
                        .await
                        .context("video download request")?
                        .error_for_status()
                        .context("video download failed")?
                        .bytes()
                        .await
                        .context("reading video content")?;

                    return Ok((content.to_vec(), cost));
                }
                Some("failed") => {
                    let err = status
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown error");
                    anyhow::bail!("video generation failed: {err}");
                }
                Some("cancelled") | Some("expired") => {
                    anyhow::bail!("video generation was cancelled or expired");
                }
                _ => {}
            }
        }

        anyhow::bail!("video generation timed out after 6 minutes");
    }

    pub fn default_utility_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "google/gemini-2.5-flash-lite",
            ProviderFlavor::OpenAi => "gpt-4.1-mini",
            ProviderFlavor::OpenAiCodex => "gpt-5.4-mini",
            ProviderFlavor::OpencodeGo => "deepseek-v4-flash",
        }
    }

    pub fn default_research_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "google/gemini-2.5-flash",
            ProviderFlavor::OpenAi => "gpt-4.1",
            ProviderFlavor::OpenAiCodex => "gpt-5.5",
            ProviderFlavor::OpencodeGo => "kimi-k2.7-code",
        }
    }

    pub fn default_escalation_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "anthropic/claude-sonnet-4.5",
            ProviderFlavor::OpenAi => "gpt-4.1",
            ProviderFlavor::OpenAiCodex => "gpt-5.5",
            ProviderFlavor::OpencodeGo => "deepseek-v4-pro",
        }
    }

    pub fn default_embedding_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "openai/text-embedding-3-small",
            ProviderFlavor::OpenAi => "text-embedding-3-small",
            ProviderFlavor::OpenAiCodex => "",
            // Go's bundled models are all chat/coding models, no embeddings.
            ProviderFlavor::OpencodeGo => "",
        }
    }

    pub fn default_video_gen_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "google/veo-3.1",
            _ => "",
        }
    }

    pub fn default_image_gen_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "openai/gpt-image-2",
            ProviderFlavor::OpenAi => "dall-e-3",
            ProviderFlavor::OpenAiCodex => "",
            ProviderFlavor::OpencodeGo => "",
        }
    }

    /// Fetch the live model catalog. No hardcoded list except Codex, whose
    /// subscription endpoint does not expose the normal OpenAI models catalog.
    pub async fn list_models(&self) -> Result<Vec<Model>> {
        if self.flavor == ProviderFlavor::OpenAiCodex {
            // Codex-only models — deliberately not merged with OpenRouter's
            // catalog (switch backends with Ctrl+P to see that instead): a few hundred OpenRouter entries used to bury these
            // 7 alphabetically, making it look like Codex had no models.
            return Ok(vec![
                Model {
                    id: "gpt-5.3-codex-spark".into(),
                    name: "GPT-5.3 Codex Spark".into(),
                    reasoning_efforts: ReasoningEffort::STANDARD.to_vec(),
                    context_length: Some(128_000),
                    supports_images: false,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
                Model {
                    id: "gpt-5.4".into(),
                    name: "GPT-5.4".into(),
                    reasoning_efforts: ReasoningEffort::WITH_XHIGH_AND_NONE.to_vec(),
                    context_length: Some(272_000),
                    supports_images: true,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
                Model {
                    id: "gpt-5.4-mini".into(),
                    name: "GPT-5.4 mini".into(),
                    reasoning_efforts: ReasoningEffort::WITH_XHIGH_AND_NONE.to_vec(),
                    context_length: Some(272_000),
                    supports_images: true,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
                Model {
                    id: "gpt-5.5".into(),
                    name: "GPT-5.5".into(),
                    reasoning_efforts: ReasoningEffort::WITH_XHIGH_AND_NONE.to_vec(),
                    context_length: Some(272_000),
                    supports_images: true,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
                Model {
                    id: "gpt-5.6-sol".into(),
                    name: "GPT-5.6 Sol".into(),
                    reasoning_efforts: ReasoningEffort::WITH_MAX_XHIGH_AND_NONE.to_vec(),
                    context_length: Some(1_000_000),
                    supports_images: true,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
                Model {
                    id: "gpt-5.6-terra".into(),
                    name: "GPT-5.6 Terra".into(),
                    reasoning_efforts: ReasoningEffort::WITH_MAX_XHIGH_AND_NONE.to_vec(),
                    context_length: Some(1_000_000),
                    supports_images: true,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
                Model {
                    id: "gpt-5.6-luna".into(),
                    name: "GPT-5.6 Luna".into(),
                    reasoning_efforts: ReasoningEffort::WITH_MAX_XHIGH_AND_NONE.to_vec(),
                    context_length: Some(1_000_000),
                    supports_images: true,
                    supports_image_generation: false,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::Codex,
                },
            ]);
        }
        if self.flavor == ProviderFlavor::OpencodeGo {
            // Two distinct catalogs behind the same account key: Zen
            // general (free + pay-per-token) and the flat-fee Go bundle.
            // Fetch both and show them together — Go entries tagged so
            // requests route (and bill) correctly; see `opencode_route`.
            let (zen, go) = tokio::join!(
                self.fetch_models_from(OPENCODE_ZEN_BASE),
                self.fetch_models_from(OPENCODE_GO_BASE),
            );
            // Zen general is the primary catalog — a failure there is a
            // real problem (bad key, network) and should surface. Go's
            // bundle can legitimately 403 for an account without that
            // subscription; treat it as "no Go models" rather than an error.
            let zen = zen?;
            let go = go.unwrap_or_default();
            let go_ids: std::collections::HashSet<&str> =
                go.iter().map(|m| m.id.as_str()).collect();
            let mut models: Vec<Model> = zen
                .into_iter()
                // A model covered by the flat-fee Go bundle is strictly
                // better than its metered Zen-general twin — don't show both.
                .filter(|m| !go_ids.contains(m.id.as_str()))
                .collect();
            models.extend(go.into_iter().map(|mut m| {
                m.id = format!("{OPENCODE_GO_PREFIX}{}", m.id);
                m
            }));
            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }
        let mut models = self.fetch_models_from(self.flavor.base()).await?;
        if self.flavor == ProviderFlavor::OpenRouter {
            // The general catalog is not authoritative for generation models.
            // Merge OpenRouter's dedicated catalogs into the existing picker.
            let (images, videos) =
                tokio::join!(self.fetch_image_models(), self.fetch_video_models(),);
            merge_generation_models(&mut models, images.unwrap_or_default());
            merge_generation_models(&mut models, videos.unwrap_or_default());
        }
        Ok(models)
    }

    async fn fetch_image_models(&self) -> Result<Vec<Model>> {
        let response = self
            .client
            .get(format!("{OPENROUTER_BASE}/images/models"))
            .bearer_auth(&self.key)
            .send()
            .await
            .context("requesting image model list")?
            .error_for_status()
            .context("image model list request failed")?
            .json::<ImageModelsResponse>()
            .await
            .context("parsing image model list")?;

        Ok(response
            .data
            .into_iter()
            .map(|m| {
                let id = m.id;
                Model {
                    name: m.name.unwrap_or_else(|| id.clone()),
                    reasoning_efforts: Vec::new(),
                    context_length: None,
                    supports_images: m
                        .architecture
                        .as_ref()
                        .is_some_and(|a| a.input_modalities.iter().any(|v| v == "image")),
                    supports_image_generation: true,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::OpenRouter,
                    id,
                }
            })
            .collect())
    }

    async fn fetch_video_models(&self) -> Result<Vec<Model>> {
        let response = self
            .client
            .get(format!("{OPENROUTER_BASE}/videos/models"))
            .bearer_auth(&self.key)
            .send()
            .await
            .context("requesting video model list")?
            .error_for_status()
            .context("video model list request failed")?
            .json::<VideoModelsResponse>()
            .await
            .context("parsing video model list")?;

        Ok(response
            .data
            .into_iter()
            .map(|m| {
                let id = m.id;
                Model {
                    name: m.name.unwrap_or_else(|| id.clone()),
                    reasoning_efforts: Vec::new(),
                    context_length: None,
                    supports_images: false,
                    supports_image_generation: false,
                    supports_video_generation: true,
                    backend: crate::provider::BackendTag::OpenRouter,
                    id,
                }
            })
            .collect())
    }

    /// GET `{base}/models` and map the response into our `Model` type using
    /// this instance's flavor for reasoning/image-support inference.
    async fn fetch_models_from(&self, base: &str) -> Result<Vec<Model>> {
        let resp = self
            .client
            .get(format!("{base}/models"))
            .bearer_auth(&self.key)
            .send()
            .await
            .context("requesting model list")?
            .error_for_status()
            .context("model list request failed")?
            .json::<ModelsResponse>()
            .await
            .context("parsing model list")?;

        let mut models: Vec<Model> = resp
            .data
            .into_iter()
            .map(|m| {
                let reasoning_efforts = self.flavor.reasoning_efforts(&m);
                let supports_images = self
                    .flavor
                    .supports_images(&m)
                    .unwrap_or_else(|| entry_supports_images(&m));
                let supports_image_generation = self
                    .flavor
                    .supports_image_generation(&m)
                    .unwrap_or_else(|| entry_supports_image_gen(&m));
                let supports_video_generation =
                    self.flavor == ProviderFlavor::OpenRouter && entry_supports_video_gen(&m);
                Model {
                    name: m.name.unwrap_or_else(|| m.id.clone()),
                    reasoning_efforts,
                    context_length: m.context_length,
                    id: m.id,
                    supports_images,
                    supports_image_generation,
                    supports_video_generation,
                    backend: self.backend_tag(),
                }
            })
            .collect();
        // The OpenCode Zen endpoints return no context metadata at all; fill
        // in the known windows per endpoint (the same id can differ between
        // the general catalog and the Go bundle — see the table comments).
        // An API-provided value always wins over the fallback table.
        for m in &mut models {
            if m.context_length.is_none() {
                m.context_length = Self::opencode_context_fallback(base, &m.id);
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Fallback context window for an OpenCode model id, keyed by endpoint
    /// (Zen general vs Go bundle). `None` for non-OpenCode bases and for ids
    /// not in the table.
    fn opencode_context_fallback(base: &str, id: &str) -> Option<u64> {
        let table = match base {
            OPENCODE_ZEN_BASE => OPENCODE_ZEN_CONTEXT,
            OPENCODE_GO_BASE => OPENCODE_GO_CONTEXT,
            _ => return None,
        };
        table.iter().find(|(tid, _)| *tid == id).map(|(_, c)| *c)
    }

    /// One-shot, non-streaming completion. Used for short utility calls like
    /// generating a session topic/slug. Returns the assistant's message text.
    pub async fn complete(&self, model: &str, messages: Vec<ChatMessage>) -> Result<String> {
        // ChatGPT's Codex endpoint is stream-oriented: non-streaming requests
        // are rejected or can return no usable output. Utility jobs (titles,
        // memory, compaction, research metadata) still need a one-shot string,
        // so collect the same proven SSE path used by interactive chat.
        if self.flavor == ProviderFlavor::OpenAiCodex {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let finish = self
                .run_codex_stream(model, &messages, &ChatParams::default(), &[], &tx)
                .await?;
            drop(tx);

            let mut text = String::new();
            let mut error = None;
            while let Ok(event) = rx.try_recv() {
                match event {
                    StreamEvent::Token(token) => text.push_str(&token),
                    StreamEvent::Error(message) => error = Some(message),
                    _ => {}
                }
            }
            if matches!(finish, Finish::Errored) {
                anyhow::bail!(error.unwrap_or_else(|| "Codex completion failed".to_string()));
            }
            return Ok(text);
        }

        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        self.post_completion(body).await
    }

    /// POST a completions body and pull the first choice's message text.
    async fn post_completion(&self, body: serde_json::Value) -> Result<String> {
        if let Some(delegate) = self.openrouter_delegate_for_body(&body) {
            return Box::pin(delegate.post_completion(body)).await;
        }
        if self.flavor == ProviderFlavor::OpenAiCodex {
            let body = chat_body_to_codex_body(&body, false, &[]);
            let v = self
                .client
                .post(format!("{}/codex/responses", self.flavor.base()))
                .headers(self.codex_headers(false)?)
                .json(&body)
                .send()
                .await
                .context("Codex completion request")?
                .error_for_status()
                .context("Codex completion failed")?
                .json::<serde_json::Value>()
                .await
                .context("parsing Codex completion")?;
            return Ok(codex_response_text(&v));
        }
        let mut body = body;
        let base = if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
            let (base, real_model) = self.opencode_route(model);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".into(), serde_json::json!(real_model));
            }
            base
        } else {
            self.flavor.base()
        };
        let v = self
            .client
            .post(format!("{base}/chat/completions"))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("completion request")?
            .error_for_status()
            .context("completion failed")?
            .json::<serde_json::Value>()
            .await
            .context("parsing completion")?;
        Ok(v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(wire_text)
            .unwrap_or_default())
    }

    /// One-shot, non-streaming vision call: describe `image_data_url` with `model`.
    pub async fn describe_image(&self, model: &str, image_data_url: &str) -> Result<String> {
        self.post_completion(vision_body(model, image_data_url))
            .await
    }

    /// Generate an image via OpenRouter's dedicated `/api/v1/images` endpoint.
    /// Returns `(decoded_bytes, file_extension)` — the caller should use the
    /// returned extension (e.g. `"png"`, `"jpg"`, `"webp"`) for the saved file.
    pub async fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        size: &str,
        image_data: Option<&[u8]>,
    ) -> Result<(Vec<u8>, String)> {
        if let Some(delegate) = self.openrouter_delegate_for_model(model) {
            return Box::pin(delegate.generate_image(model, prompt, size, image_data)).await;
        }
        if self.flavor == ProviderFlavor::OpenAiCodex {
            anyhow::bail!("image generation not supported on Codex");
        }
        let (base, model) = self.opencode_route(model);

        let mut body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
        });
        if self.flavor == ProviderFlavor::OpenAi {
            // DALL-E uses OpenAI's legacy pixel-size parameter.
            body["size"] = serde_json::json!(size);
        } else {
            // OpenRouter's image catalog uses normalized capabilities. The
            // old 1024x1024 size is not accepted by several newer models.
            body["aspect_ratio"] = serde_json::json!(Self::image_aspect_ratio(size));
            if model.starts_with("google/gemini-3") {
                body["resolution"] = serde_json::json!(Self::image_resolution(size));
            }
            if model.starts_with("openai/gpt-image") || model.starts_with("openai/gpt-5-image") {
                body["quality"] = serde_json::json!("high");
            }
        }
        if let Some(img) = image_data {
            let b64 = base64::engine::general_purpose::STANDARD.encode(img);
            let mime = Self::detect_image_mime(img);
            body["input_references"] = serde_json::json!([{
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") }
            }]);
        }
        let v = self
            .client
            .post(format!("{base}/images"))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("image generation request")?
            .error_for_status()
            .context("image generation failed")?
            .json::<serde_json::Value>()
            .await
            .context("parsing image generation response")?;
        let data = v
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .context("no image data in response")?;
        let b64 = data
            .get("b64_json")
            .and_then(|b| b.as_str())
            .context("no b64_json field")?;
        let media_type = data
            .get("media_type")
            .and_then(|m| m.as_str())
            .unwrap_or("image/png");
        let ext = match media_type {
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "image/gif" => "gif",
            "image/svg+xml" => "svg",
            _ => "png",
        };
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .context("base64 decode")?;
        Ok((bytes, ext.to_string()))
    }

    /// Detect image MIME type from magic bytes.
    fn detect_image_mime(data: &[u8]) -> &'static str {
        if data.len() < 4 {
            return "image/png";
        }
        if data[0] == 0x89 && data[1] == b'P' && data[2] == b'N' && data[3] == b'G' {
            "image/png"
        } else if data[0] == 0xFF && data[1] == 0xD8 {
            "image/jpeg"
        } else if data[0] == b'G' && data[1] == b'I' && data[2] == b'F' {
            "image/gif"
        } else if data[0] == b'R' && data[1] == b'I' && data[2] == b'F' && data[3] == b'F' {
            "image/webp"
        } else {
            "image/png"
        }
    }

    fn image_aspect_ratio(size: &str) -> &'static str {
        match size {
            "1024x1792" => "9:16",
            "1792x1024" => "16:9",
            _ => "1:1",
        }
    }

    fn image_resolution(size: &str) -> &'static str {
        let max_dimension = size
            .split('x')
            .filter_map(|value| value.parse::<u32>().ok())
            .max()
            .unwrap_or(1024);
        if max_dimension >= 3000 {
            "4K"
        } else if max_dimension >= 1800 {
            "2K"
        } else {
            "1K"
        }
    }

    /// One-shot, non-streaming vision call: transcribe a scanned page image.
    pub async fn ocr_page(&self, model: &str, image_data_url: &str) -> Result<String> {
        self.post_completion(ocr_body(model, image_data_url)).await
    }

    /// Embed `inputs` with `model` (OpenAI-format /embeddings endpoint);
    /// returns one vector per input, in order.
    pub async fn embed(&self, model: &str, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if let Some(delegate) = self.openrouter_delegate_for_model(model) {
            return Box::pin(delegate.embed(model, inputs)).await;
        }
        let (base, model) = self.opencode_route(model);
        let v = self
            .client
            .post(format!("{base}/embeddings"))
            .bearer_auth(&self.key)
            .json(&serde_json::json!({ "model": model, "input": inputs }))
            .send()
            .await
            .context("embeddings request")?
            .error_for_status()
            .context("embeddings failed")?
            .json::<serde_json::Value>()
            .await
            .context("parsing embeddings")?;
        let data = v
            .get("data")
            .and_then(|d| d.as_array())
            .context("embeddings response has no data")?;
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let emb = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .context("embeddings item has no vector")?;
            out.push(
                emb.iter()
                    .filter_map(|x| x.as_f64())
                    .map(|f| f as f32)
                    .collect(),
            );
        }
        Ok(out)
    }

    /// Start a streaming completion. Spawns a task that pushes tokens over the
    /// returned channel; the UI loop drains it alongside keypresses. If the
    /// model calls a tool, the task runs it via `toolbox` and continues the
    /// conversation, bounded by `max_tool_iters` round-trips.
    pub fn stream_chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<ToolBox>,
        max_tool_iters: usize,
    ) -> (
        mpsc::UnboundedReceiver<StreamEvent>,
        tokio::task::AbortHandle,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = this
                .run_chat_loop(model, messages, params, tools, toolbox, max_tool_iters, &tx)
                .await
            {
                let _ = tx.send(StreamEvent::Error(e.to_string()));
            }
        });
        (rx, task.abort_handle())
    }

    /// One chat round-trip plus tool iterations. Args mix request data
    /// (model/messages/params/tools) with execution plumbing (toolbox,
    /// budget, event sink) — a struct would group unrelated concerns.
    #[allow(clippy::too_many_arguments)]
    async fn run_chat_loop(
        &self,
        model: String,
        mut messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<ToolBox>,
        max_tool_iters: usize,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        for iter in 0..=max_tool_iters {
            // On the final allowed iteration, omit tools and tell the model
            // why — otherwise it writes the tool call as plain text.
            let send_tools: &[ToolDef] = if iter < max_tool_iters { &tools } else { &[] };
            if iter == max_tool_iters {
                messages.push(ChatMessage::text(
                    "system",
                    "Tool budget exhausted for this turn. Do not attempt further tool calls; \
                     answer now with the information you already have.",
                ));
            }
            match self
                .run_stream(&model, &messages, &params, send_tools, tx)
                .await?
            {
                Finish::Errored => return Ok(()),
                Finish::Done => {
                    let _ = tx.send(StreamEvent::Done);
                    return Ok(());
                }
                Finish::ToolCalls(calls, content) => {
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_calls: Some(calls.clone()),
                        tool_call_id: None,
                        images: Vec::new(),
                    });
                    for call in &calls {
                        let _ = tx.send(StreamEvent::Status("Running tool…".to_string()));
                        let (result, status) = toolbox.run(&call.name, &call.arguments).await;
                        let _ = tx.send(StreamEvent::Status(status));
                        let _ = tx.send(StreamEvent::ToolCall {
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            result: result.clone(),
                        });
                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: result.clone(),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                            images: Vec::new(),
                        });
                        // If the model supports images, load image references
                        // from the tool result and inject them as a user
                        // message so the vision model can see them on the
                        // next stream request.
                        if toolbox.supports_images
                            && let Some(imgs) =
                                extract_tool_images(&result, &toolbox.space_files_dir)
                        {
                            messages.push(ChatMessage {
                                role: "user".to_string(),
                                content: imgs.description,
                                tool_calls: None,
                                tool_call_id: None,
                                images: imgs.urls,
                            });
                        }
                    }
                    let remaining = max_tool_iters - (iter + 1);
                    if let Some(m) = messages.last_mut() {
                        m.content.push_str(&format!(
                            "\n\n[{remaining} tool round-trips left this turn — plan accordingly]"
                        ));
                    }
                }
            }
        }
        let _ = tx.send(StreamEvent::Done);
        Ok(())
    }

    /// One request/response over SSE. Doesn't send `Done` itself — the caller
    /// decides whether another tool round-trip follows.
    async fn run_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        params: &ChatParams,
        tools: &[ToolDef],
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<Finish> {
        if let Some(delegate) = self.openrouter_delegate_for_model(model) {
            return Box::pin(delegate.run_stream(model, messages, params, tools, tx)).await;
        }
        if self.flavor == ProviderFlavor::OpenAiCodex {
            return self
                .run_codex_stream(model, messages, params, tools, tx)
                .await;
        }
        let (base, model) = self.opencode_route(model);
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });
        let obj = body.as_object_mut().expect("body is a json object");
        self.flavor.add_stream_usage(obj);
        if let Some(effort) = &params.reasoning_effort {
            self.flavor.add_reasoning_effort(obj, effort);
        }
        if let Some(t) = params.temperature {
            obj.insert("temperature".into(), serde_json::json!(t));
        }
        if let Some(p) = params.top_p {
            obj.insert("top_p".into(), serde_json::json!(p));
        }
        if let Some(m) = params.max_tokens {
            obj.insert("max_tokens".into(), serde_json::json!(m));
        }
        if !tools.is_empty() {
            let wire: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                    })
                })
                .collect();
            obj.insert("tools".into(), serde_json::json!(wire));
        }
        let request = self
            .client
            .post(format!("{base}/chat/completions"))
            .bearer_auth(&self.key)
            .json(&body);

        let mut es = EventSource::new(request).context("opening SSE stream")?;
        let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let mut content_acc = String::new();
        while let Some(event) = es.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(msg)) => {
                    if msg.data == "[DONE]" {
                        break;
                    }
                    let (content, reasoning) = parse_delta(&msg.data);
                    if let Some(r) = reasoning
                        && !r.is_empty()
                    {
                        let _ = tx.send(StreamEvent::Reasoning(r));
                    }
                    if let Some(token) = content
                        && !token.is_empty()
                    {
                        content_acc.push_str(&token);
                        let _ = tx.send(StreamEvent::Token(token));
                    }
                    accumulate_tool_calls(&mut tool_calls, &msg.data);
                    if let Some(usage) = parse_usage(&msg.data) {
                        let _ = tx.send(StreamEvent::Usage(usage));
                    }
                }
                Err(reqwest_eventsource::Error::StreamEnded) => break,
                // These two variants carry the actual HTTP response, but
                // their `Display` is close to useless ("Invalid header
                // value: \"\"" when Content-Type is simply absent — no
                // status, no body shown). Read the response through instead
                // so the real reason (an auth/entitlement error page, a
                // rate limit, etc.) reaches the user rather than a
                // header-parsing artifact.
                Err(reqwest_eventsource::Error::InvalidStatusCode(_, response))
                | Err(reqwest_eventsource::Error::InvalidContentType(_, response)) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    let msg = format!("request failed ({status}): {}", truncate_error_body(&body));
                    let _ = tx.send(StreamEvent::Error(msg));
                    es.close();
                    return Ok(Finish::Errored);
                }
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string()));
                    es.close();
                    return Ok(Finish::Errored);
                }
            }
        }
        // Trust accumulated tool calls, not finish_reason: some providers
        // stream tool_calls but finish with "stop" (or no finish chunk at
        // all); dropping the calls there kills the turn silently.
        if !tool_calls.is_empty() {
            Ok(Finish::ToolCalls(
                tool_calls.into_values().collect(),
                content_acc,
            ))
        } else {
            Ok(Finish::Done)
        }
    }

    /// For OpenCode Go: pick the base to hit and the raw model id to send,
    /// stripping the `go:` tag `list_models` adds to flat-fee Go models. A
    /// no-op (returns the flavor's default base, id unchanged) for every
    /// other flavor and for untagged (general Zen) OpenCode ids.
    fn opencode_route(&self, model: &str) -> (&'static str, String) {
        if self.flavor == ProviderFlavor::OpencodeGo
            && let Some(stripped) = model.strip_prefix(OPENCODE_GO_PREFIX)
        {
            return (OPENCODE_GO_BASE, stripped.to_string());
        }
        (self.flavor.base(), model.to_string())
    }

    fn openrouter_delegate_for_model(&self, model: &str) -> Option<OpenRouter> {
        if self.flavor == ProviderFlavor::OpenAiCodex && model.contains('/') {
            crate::config::load_openrouter_key_only().map(OpenRouter::openrouter_flavor)
        } else {
            None
        }
    }

    fn openrouter_delegate_for_body(&self, body: &serde_json::Value) -> Option<OpenRouter> {
        let model = body.get("model").and_then(|m| m.as_str())?;
        self.openrouter_delegate_for_model(model)
    }

    fn codex_headers(&self, sse: bool) -> Result<reqwest::header::HeaderMap> {
        let account_id = crate::config::codex_account_id(&self.key)?;
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.key).parse()?,
        );
        h.insert("chatgpt-account-id", account_id.parse()?);
        h.insert("originator", "nexus-chat".parse()?);
        h.insert(reqwest::header::USER_AGENT, "nexus-chat".parse()?);
        h.insert("OpenAI-Beta", "responses=experimental".parse()?);
        h.insert(reqwest::header::CONTENT_TYPE, "application/json".parse()?);
        if sse {
            h.insert(reqwest::header::ACCEPT, "text/event-stream".parse()?);
        }
        Ok(h)
    }

    async fn run_codex_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        params: &ChatParams,
        tools: &[ToolDef],
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<Finish> {
        let mut body = serde_json::json!({
            "model": model,
            "store": false,
            "stream": true,
            "instructions": codex_instructions(messages),
            "input": codex_input(messages),
            "text": { "verbosity": "low" },
            "include": ["reasoning.encrypted_content"],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
        });
        let obj = body.as_object_mut().unwrap();
        if let Some(t) = params.temperature {
            obj.insert("temperature".into(), serde_json::json!(t));
        }
        if let Some(effort) = &params.reasoning_effort {
            obj.insert(
                "reasoning".into(),
                serde_json::json!({ "effort": effort, "summary": "auto" }),
            );
        }
        if !tools.is_empty() {
            obj.insert("tools".into(), serde_json::json!(codex_tools(tools)));
        }
        // Not reqwest_eventsource here: it gates the whole response on a
        // strict Content-Type == "text/event-stream" check, and ChatGPT's
        // backend has been observed sending a genuine SSE body (starting
        // with a real "event: response.created" line) under a Content-Type
        // that check doesn't accept — the crate then reports the situation
        // as an opaque "invalid header value" and discards the (valid) body
        // entirely. Reading the response as a raw byte stream and splitting
        // on blank lines ourselves doesn't care what Content-Type says.
        let response = self
            .client
            .post(format!("{}/codex/responses", self.flavor.base()))
            .headers(self.codex_headers(true)?)
            .json(&body)
            .send()
            .await
            .context("sending Codex request")?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let msg = format!("request failed ({status}): {}", truncate_error_body(&text));
            let _ = tx.send(StreamEvent::Error(msg));
            return Ok(Finish::Errored);
        }

        let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let mut content_acc = String::new();
        let mut buf = String::new();
        let mut stream = response.bytes_stream();
        'stream: while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading Codex stream")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // SSE events are separated by a blank line; a "data:" line's
            // content (possibly spread across several, joined by \n per the
            // spec — Codex never does this in practice, but handle it) is
            // the payload we care about.
            while let Some(end) = buf.find("\n\n") {
                let event_block: String = buf.drain(..end + 2).collect();
                let data = sse_event_data(&event_block);
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    break 'stream;
                }
                if let Some(token) = codex_text_delta(&data)
                    && !token.is_empty()
                {
                    content_acc.push_str(&token);
                    let _ = tx.send(StreamEvent::Token(token));
                }
                if let Some(r) = codex_reasoning_delta(&data)
                    && !r.is_empty()
                {
                    let _ = tx.send(StreamEvent::Reasoning(r));
                }
                accumulate_codex_tool_calls(&mut tool_calls, &data);
                if let Some(usage) = codex_usage(&data) {
                    let _ = tx.send(StreamEvent::Usage(usage));
                }
            }
        }
        if !tool_calls.is_empty() {
            Ok(Finish::ToolCalls(
                tool_calls.into_values().collect(),
                content_acc,
            ))
        } else {
            Ok(Finish::Done)
        }
    }
}

enum Finish {
    Done,
    ToolCalls(Vec<ToolCall>, String),
    Errored,
}

/// Cap an error response body shown to the user — a WAF/error page can be
/// arbitrarily large HTML, and the status line has no room for it anyway.
fn truncate_error_body(body: &str) -> String {
    const MAX: usize = 300;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty body)".to_string();
    }
    let mut truncated: String = trimmed.chars().take(MAX).collect();
    if trimmed.chars().count() > MAX {
        truncated.push('…');
    }
    truncated
}

/// Pull `(content, reasoning)` deltas out of one SSE data chunk. OpenRouter puts
/// thinking tokens in `delta.reasoning`, separate from the visible `delta.content`.
fn parse_delta(data: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return (None, None);
    };
    let Some(delta) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
    else {
        return (None, None);
    };
    let field = |name: &str| delta.get(name).and_then(|x| x.as_str()).map(str::to_string);
    (field("content"), field("reasoning"))
}

/// Merge one SSE chunk's `delta.tool_calls` fragments into the running
/// per-call accumulator, keyed by the call's `index` (ids/names arrive once,
/// `arguments` streams in pieces that must be concatenated in order).
fn accumulate_tool_calls(acc: &mut BTreeMap<usize, ToolCall>, data: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let Some(calls) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("tool_calls"))
        .and_then(|t| t.as_array())
    else {
        return;
    };
    for call in calls {
        let idx = call.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
        let entry = acc.entry(idx).or_default();
        if let Some(id) = call.get("id").and_then(|i| i.as_str())
            && !id.is_empty()
        {
            entry.id = id.to_string();
        }
        if let Some(func) = call.get("function") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str())
                && !name.is_empty()
            {
                entry.name = name.to_string();
            }
            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                entry.arguments.push_str(args);
            }
        }
    }
}

/// Pull the `usage` object from an SSE chunk, if present.
fn parse_usage(data: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let u = v.get("usage")?;
    let get = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Some(Usage {
        prompt_tokens: get("prompt_tokens"),
        completion_tokens: get("completion_tokens"),
        total_tokens: get("total_tokens"),
    })
}

fn codex_instructions(messages: &[ChatMessage]) -> String {
    let s = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if s.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        s
    }
}

fn codex_input(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter(|m| m.role != "system")
        .flat_map(|m| match m.role.as_str() {
            // A model may request several tools in parallel. Every subsequent
            // function_call_output must have a matching function_call item;
            // emitting only calls[0] makes Codex reject the other outputs.
            "assistant" if m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty()) => m
                .tool_calls
                .as_ref()
                .unwrap()
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments,
                    })
                })
                .collect::<Vec<_>>(),
            "assistant" => vec![serde_json::json!({
                "role": "assistant",
                "content": [{ "type": "output_text", "text": m.content, "annotations": [] }],
            })],
            "tool" => vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.as_deref().unwrap_or_default(),
                "output": m.content,
            })],
            _ => {
                let mut content = Vec::new();
                if !m.content.is_empty() {
                    content.push(serde_json::json!({ "type": "input_text", "text": m.content }));
                }
                for image_url in &m.images {
                    content.push(serde_json::json!({ "type": "input_image", "detail": "auto", "image_url": image_url }));
                }
                vec![serde_json::json!({ "role": "user", "content": content })]
            }
        })
        .collect()
}

fn codex_tools(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                "strict": false,
            })
        })
        .collect()
}

fn chat_body_to_codex_body(
    body: &serde_json::Value,
    stream: bool,
    tools: &[ToolDef],
) -> serde_json::Value {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-5.1-codex-mini");
    let messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let instructions = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .filter_map(|m| wire_text(m.get("content")?))
        .collect::<Vec<_>>()
        .join("\n\n");
    let input = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
        .map(|m| {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            if role == "assistant" {
                return serde_json::json!({
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": wire_text(m.get("content").unwrap_or(&serde_json::Value::Null)).unwrap_or_default(), "annotations": [] }],
                });
            }
            let mut content = Vec::new();
            match m.get("content") {
                Some(serde_json::Value::String(s)) => content.push(serde_json::json!({ "type": "input_text", "text": s })),
                Some(serde_json::Value::Array(parts)) => {
                    for p in parts {
                        match p.get("type").and_then(|t| t.as_str()) {
                            Some("text") => content.push(serde_json::json!({ "type": "input_text", "text": p.get("text").and_then(|t| t.as_str()).unwrap_or_default() })),
                            Some("image_url") => content.push(serde_json::json!({ "type": "input_image", "detail": "auto", "image_url": p.get("image_url").and_then(|i| i.get("url")).and_then(|u| u.as_str()).unwrap_or_default() })),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            serde_json::json!({ "role": "user", "content": content })
        })
        .collect::<Vec<_>>();
    let mut out = serde_json::json!({
        "model": model,
        "store": false,
        "stream": stream,
        "instructions": if instructions.is_empty() { "You are a helpful assistant." } else { &instructions },
        "input": input,
        "text": { "verbosity": "low" },
    });
    if !tools.is_empty() {
        out.as_object_mut()
            .unwrap()
            .insert("tools".into(), serde_json::json!(codex_tools(tools)));
    }
    out
}

fn wire_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn codex_response_text(v: &serde_json::Value) -> String {
    if let Some(s) = v.get("output_text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    v.get("output")
        .and_then(|o| o.as_array())
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("content")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|c| {
            c.get("text")
                .or_else(|| c.get("refusal"))
                .and_then(|t| t.as_str())
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Extract an SSE event block's `data:` payload — possibly spread across
/// several `data:` lines, joined by `\n` per the SSE spec (Codex never
/// actually does this, but it costs nothing to handle). Ignores any
/// `event:`/`id:`/comment lines in the block; we key off the JSON payload's
/// own `"type"` field instead of the SSE `event:` name.
fn sse_event_data(event_block: &str) -> String {
    event_block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n")
}

fn codex_text_delta(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    match v.get("type")?.as_str()? {
        "response.output_text.delta" | "response.refusal.delta" => {
            v.get("delta")?.as_str().map(str::to_string)
        }
        _ => None,
    }
}

fn codex_reasoning_delta(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    match v.get("type")?.as_str()? {
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            v.get("delta")?.as_str().map(str::to_string)
        }
        _ => None,
    }
}

fn accumulate_codex_tool_calls(acc: &mut BTreeMap<usize, ToolCall>, data: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("response.output_item.added") => {
            let Some(item) = v.get("item") else { return };
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                return;
            }
            let idx = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            let entry = acc.entry(idx).or_default();
            entry.id = item
                .get("call_id")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            entry.name = item
                .get("name")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            entry.arguments.push_str(
                item.get("arguments")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default(),
            );
        }
        Some("response.function_call_arguments.delta") => {
            let idx = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            acc.entry(idx)
                .or_default()
                .arguments
                .push_str(v.get("delta").and_then(|s| s.as_str()).unwrap_or_default());
        }
        Some("response.function_call_arguments.done") => {
            let idx = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            if let Some(args) = v.get("arguments").and_then(|s| s.as_str()) {
                acc.entry(idx).or_default().arguments = args.to_string();
            }
        }
        Some("response.output_item.done") => {
            let Some(item) = v.get("item") else { return };
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                return;
            }
            let idx = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            let entry = acc.entry(idx).or_default();
            entry.id = item
                .get("call_id")
                .and_then(|s| s.as_str())
                .unwrap_or(&entry.id)
                .to_string();
            entry.name = item
                .get("name")
                .and_then(|s| s.as_str())
                .unwrap_or(&entry.name)
                .to_string();
            entry.arguments = item
                .get("arguments")
                .and_then(|s| s.as_str())
                .unwrap_or(&entry.arguments)
                .to_string();
        }
        _ => {}
    }
}

fn codex_usage(data: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let response = v.get("response")?;
    let u = response.get("usage")?;
    let prompt_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let completion_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: u
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(prompt_tokens + completion_tokens),
    })
}

/// Request body for a one-shot image-understanding call: a text part with the
/// instruction plus the image as a data-URL content part (OpenAI vision shape).
/// Shared page-transcription instructions (OpenRouter VLMs and local Ollama).
pub(crate) const OCR_PROMPT: &str = "Transcribe this scanned page to plain text, faithfully and completely. \
     Output ONLY the transcription — no commentary, no markdown fences. \
     Preserve the natural reading order; vertical Japanese text reads in \
     columns from right to left. Transcribe the body text only: skip \
     furigana/ruby annotations (the small kana printed above or beside \
     kanji). Render tables as plain text rows. If the page contains no \
     text, output nothing.";

fn ocr_body(model: &str, image_data_url: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        // Page transcriptions run long; don't let a provider default clip them.
        "max_tokens": 8000,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": OCR_PROMPT },
                { "type": "image_url", "image_url": { "url": image_data_url } },
            ],
        }],
    })
}

fn vision_body(model: &str, image_data_url: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text",
                  "text": "Describe this image so another AI model can reason about it without seeing it. \
                           Cover: what it is (screenshot, chart, photo, diagram…), overall layout and structure, \
                           the key entities and how they relate, ALL visible text verbatim (preserve code, \
                           tables, and labels as markdown), and any notable visual details (colors, states, \
                           highlights, errors). Be thorough but do not speculate beyond what is visible." },
                { "type": "image_url", "image_url": { "url": image_data_url } },
            ],
        }],
    })
}

/// Validate basic video generation parameters against known model capabilities.
/// Returns `Ok` if the params look valid, or a descriptive error message if not.
/// This is a best-effort check — the API is the final authority.
pub(crate) fn normalize_video_params(
    model: &str,
    duration: u32,
    resolution: &str,
    aspect_ratio: &str,
) -> (u32, String, String) {
    let mut duration = duration;
    let mut resolution = resolution.to_string();
    let mut aspect_ratio = aspect_ratio.to_string();

    if model.starts_with("minimax/hailuo-3") {
        resolution = "2K".to_string();
    } else if model.starts_with("minimax/hailuo-2.3") {
        resolution = "1080p".to_string();
        aspect_ratio = "16:9".to_string();
    } else if model.starts_with("runway/gen-4.5") {
        resolution = "720p".to_string();
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("kwaivgi/kling-v3.0") || model.starts_with("kwaivgi/kling-video-o1")
    {
        resolution = "720p".to_string();
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16" | "1:1") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("x-ai/grok-imagine-video") {
        if !matches!(resolution.as_str(), "480p" | "720p" | "1080p") {
            resolution = "720p".to_string();
        }
    } else if model.starts_with("openai/sora-2-pro") {
        if !matches!(duration, 4 | 8 | 12 | 16 | 20) {
            duration = nearest_duration(duration, &[4, 8, 12, 16, 20]);
        }
        if !matches!(resolution.as_str(), "720p" | "1080p") {
            resolution = "1080p".to_string();
        }
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("alibaba/wan-2.6") {
        if !matches!(duration, 5 | 10) {
            duration = nearest_duration(duration, &[5, 10]);
        }
        if !matches!(resolution.as_str(), "720p" | "1080p") {
            resolution = "1080p".to_string();
        }
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    } else if model.starts_with("google/veo-3.1") {
        if !matches!(duration, 4 | 6 | 8) {
            duration = nearest_duration(duration, &[4, 6, 8]);
        }
        if !matches!(resolution.as_str(), "720p" | "1080p" | "4K") {
            resolution = "1080p".to_string();
        }
        if !matches!(aspect_ratio.as_str(), "16:9" | "9:16") {
            aspect_ratio = "16:9".to_string();
        }
    }

    (duration, resolution, aspect_ratio)
}

fn nearest_duration(requested: u32, allowed: &[u32]) -> u32 {
    *allowed
        .iter()
        .min_by_key(|candidate| candidate.abs_diff(requested))
        .unwrap_or(&requested)
}

fn check_video_params(
    model: &str,
    duration: u32,
    resolution: &str,
    aspect_ratio: &str,
) -> anyhow::Result<()> {
    // Model-specific duration limits. Keys are model slugs (or prefixes).
    struct Cap {
        max_dur: u32,
        res_ok: bool,
        ar_ok: bool,
    }
    let caps = |model: &str| -> Option<Cap> {
        if model.starts_with("google/veo-3.1-lite")
            || model.starts_with("google/veo-3.1-fast")
            || model.starts_with("google/veo-3.1")
        {
            Some(Cap {
                max_dur: 8,
                res_ok: matches!(resolution, "720p" | "1080p" | "4K"),
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16"),
            })
        } else if model.starts_with("alibaba/wan-2.7") {
            Some(Cap {
                max_dur: 10,
                res_ok: matches!(resolution, "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("bytedance/seedance-2.0") {
            Some(Cap {
                max_dur: 15,
                res_ok: true,
                ar_ok: true,
            })
        } else if model.starts_with("kwaivgi/kling-v3.0")
            || model.starts_with("kwaivgi/kling-video-o1")
        {
            Some(Cap {
                max_dur: 15,
                res_ok: resolution == "720p",
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16" | "1:1"),
            })
        } else if model.starts_with("openai/sora-2-pro") {
            Some(Cap {
                max_dur: 20,
                res_ok: matches!(resolution, "720p" | "1080p"),
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16"),
            })
        } else if model.starts_with("runway/gen-4.5") {
            Some(Cap {
                max_dur: 10,
                res_ok: resolution == "720p",
                ar_ok: matches!(aspect_ratio, "16:9" | "9:16"),
            })
        } else if model.starts_with("x-ai/grok-imagine-video-1.5") {
            Some(Cap {
                max_dur: 15,
                res_ok: matches!(resolution, "480p" | "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("minimax/hailuo-3") {
            Some(Cap {
                max_dur: 15,
                res_ok: resolution == "2K",
                ar_ok: true,
            })
        } else if model.starts_with("bytedance/seedance-1-5-pro") {
            Some(Cap {
                max_dur: 12,
                res_ok: matches!(resolution, "480p" | "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("minimax/hailuo-2.3") {
            Some(Cap {
                max_dur: 10,
                res_ok: resolution == "1080p",
                ar_ok: aspect_ratio == "16:9",
            })
        } else if model.starts_with("alibaba/happyhorse") {
            Some(Cap {
                max_dur: 15,
                res_ok: matches!(resolution, "720p" | "1080p"),
                ar_ok: true,
            })
        } else if model.starts_with("x-ai/grok-imagine-video") {
            Some(Cap {
                max_dur: 15,
                res_ok: matches!(resolution, "480p" | "720p"),
                ar_ok: true,
            })
        } else {
            None // unknown model, skip validation
        }
    };
    if let Some(c) = caps(model) {
        if duration > c.max_dur {
            anyhow::bail!(
                "model {model} does not support duration {duration}s — max {max}s. Change the model in /config.",
                max = c.max_dur
            );
        }
        if !c.res_ok {
            anyhow::bail!(
                "model {model} may not support resolution {resolution}. Change the model in /config."
            );
        }
        if !c.ar_ok {
            anyhow::bail!(
                "model {model} may not support aspect ratio {aspect_ratio}. Change the model in /config."
            );
        }
    }
    Ok(())
}

/// Scan a tool result string for `![...](file)` image references, load the
/// files from `files_dir`, resize large images to avoid hitting API size
/// limits, and return data URLs for the synthetic user message.
/// Returns `None` when no references found.
fn extract_tool_images(result: &str, files_dir: &std::path::Path) -> Option<ImagesForTool> {
    use base64::Engine;
    let mut urls: Vec<String> = Vec::new();
    let mut descs: Vec<String> = Vec::new();
    let mut rest = result;
    while let Some(start) = rest.find("![") {
        if let Some(end) = rest[start..].find(')') {
            let inner = &rest[start + 2..start + end];
            if let Some((desc, file)) = inner.split_once("](") {
                let path = files_dir.join(file);
                if let Ok(bytes) = std::fs::read(&path) {
                    let resized = resize_for_api(&bytes, files_dir);
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&resized);
                    let ext = if resized.len() < bytes.len() {
                        "jpg"
                    } else {
                        "png"
                    };
                    let mime = match ext {
                        "jpg" | "jpeg" => "image/jpeg",
                        "png" => "image/png",
                        "gif" => "image/gif",
                        "webp" => "image/webp",
                        _ => "image/png",
                    };
                    urls.push(format!("data:{mime};base64,{b64}"));
                    descs.push(desc.to_string());
                }
            }
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    if urls.is_empty() {
        return None;
    }
    let description = format!("The tool returned this image: {}", descs.join(", "));
    Some(ImagesForTool { urls, description })
}

/// Return a JPEG-compressed version of the image if it exceeds 1 MB, so the
/// API request doesn't get rejected for being too large. Keeps the original
/// if it's already small enough.
fn resize_for_api(bytes: &[u8], _files_dir: &std::path::Path) -> Vec<u8> {
    // ponytail: 1 MB threshold — large enough for readable text, small
    // enough to avoid 400s from provider byte limits.
    if bytes.len() < 1_000_000 {
        return bytes.to_vec();
    }
    if let Ok(img) = image::load_from_memory(bytes) {
        let (w, h) = (img.width(), img.height());
        let max_dim = 1024u32;
        if w <= max_dim && h <= max_dim {
            return bytes.to_vec();
        }
        let (nw, nh) = if w > h {
            (max_dim, (h * max_dim / w).max(1))
        } else {
            ((w * max_dim / h).max(1), max_dim)
        };
        let small = img.resize(nw, nh, image::imageops::FilterType::Lanczos3);
        let mut out = Vec::new();
        if small
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .is_ok()
        {
            return out;
        }
    }
    bytes.to_vec()
}

struct ImagesForTool {
    urls: Vec<String>,
    description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codex_catalog_matches_current_chatgpt_models() {
        let models = OpenRouter::openai_codex("token".into())
            .list_models()
            .await
            .unwrap();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();

        assert_eq!(
            ids,
            [
                "gpt-5.3-codex-spark",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.5",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
            ]
        );
        assert!(models.iter().all(|model| {
            model.backend == crate::provider::BackendTag::Codex
                && !model.reasoning_efforts.is_empty()
        }));
    }

    #[test]
    fn opencode_context_fallback_covers_every_live_catalog_id() {
        // The live endpoints serve exactly these ids today; each must have a
        // known context window, or that model silently shows no context size
        // again. When opencode adds a model, add it to the table first.
        let zen_ids = [
            "big-pickle",
            "claude-fable-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-sonnet-4",
            "claude-haiku-4-5",
            "gemini-3.6-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.5-flash",
            "gemini-3.1-pro",
            "gemini-3-flash",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-pro",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.3-codex-spark",
            "gpt-5.3-codex",
            "gpt-5.2",
            "gpt-5.2-codex",
            "gpt-5.1",
            "gpt-5.1-codex-max",
            "gpt-5.1-codex",
            "gpt-5.1-codex-mini",
            "gpt-5",
            "gpt-5-codex",
            "gpt-5-nano",
            "grok-build-0.1",
            "grok-4.5",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "glm-5.2",
            "glm-5.1",
            "glm-5",
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.5",
            "kimi-k3",
            "kimi-k2.7-code",
            "kimi-k2.6",
            "kimi-k2.5",
            "qwen3.6-plus",
            "qwen3.5-plus",
            "deepseek-v4-flash-free",
            "mimo-v2.5-free",
            "ling-3.0-flash-free",
            "nemotron-3-ultra-free",
            "north-mini-code-free",
            "laguna-s-2.1-free",
            "longcat-2.0-free",
        ];
        let go_ids = [
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.5",
            "kimi-k3",
            "kimi-k2.7-code",
            "kimi-k2.6",
            "kimi-k2.5",
            "glm-5.2",
            "glm-5.1",
            "glm-5",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "qwen3.7-max",
            "qwen3.8-max",
            "qwen3.7-plus",
            "qwen3.6-plus",
            "qwen3.5-plus",
            "mimo-v2-pro",
            "mimo-v2-omni",
            "mimo-v2.5-pro",
            "mimo-v2.5",
            "hy3",
            "hy3-preview",
            "gpt-5.6-luna",
            "grok-4.5",
        ];
        for id in zen_ids {
            assert!(
                OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, id).is_some(),
                "no context window for Zen general model {id}"
            );
        }
        for id in go_ids {
            assert!(
                OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, id).is_some(),
                "no context window for Go bundle model {id}"
            );
        }
    }

    #[test]
    fn opencode_context_fallback_differs_per_endpoint_and_ignores_other_flavors() {
        // Same id, different window on each endpoint — the tables must stay
        // separate or Go models get the wrong (smaller) limit.
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, "qwen3.6-plus"),
            Some(262_144)
        );
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, "qwen3.6-plus"),
            Some(1_000_000)
        );
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, "minimax-m3"),
            Some(512_000)
        );
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, "minimax-m3"),
            Some(1_000_000)
        );
        // Fallback is OpenCode-only: no table for other bases, unknown ids
        // yield nothing, and a go: tag is never part of the lookup.
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENROUTER_BASE, "deepseek-v4-pro"),
            None
        );
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, "nope"),
            None
        );
        assert_eq!(
            OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, "go:hy3"),
            None
        );
    }

    #[test]
    fn reasoning_efforts_prefer_catalog_metadata_then_use_backend_fallbacks() {
        let entry = |id: &str, parameters: &[&str], efforts: &[&str]| ModelEntry {
            id: id.to_string(),
            name: None,
            supported_parameters: parameters.iter().map(|value| value.to_string()).collect(),
            reasoning: (!efforts.is_empty()).then(|| ModelReasoningEntry {
                supported_efforts: efforts.iter().map(|value| value.to_string()).collect(),
            }),
            context_length: None,
            architecture: None,
        };

        let or = OpenRouter::openrouter_flavor("k".into());
        // Catalog metadata is authoritative, normalized into UI cycle order,
        // and can expose sparse sets plus explicit disable/max tiers.
        let sparse = entry("google/gemini", &[], &["high", "minimal"]);
        assert_eq!(
            or.flavor.reasoning_efforts(&sparse),
            vec![ReasoningEffort::Minimal, ReasoningEffort::High]
        );
        let full = entry(
            "openai/gpt-next",
            &["reasoning"],
            &["max", "xhigh", "high", "medium", "low", "none"],
        );
        assert_eq!(
            or.flavor.reasoning_efforts(&full),
            ReasoningEffort::WITH_MAX_XHIGH_AND_NONE.to_vec()
        );

        // Without an enumerated set, OpenRouter still uses the catalog's
        // supported-parameter gate and the conservative family fallback.
        let claude = entry("anthropic/claude-sonnet-4.5", &["reasoning"], &[]);
        assert_eq!(
            or.flavor.reasoning_efforts(&claude),
            ReasoningEffort::WITH_MINIMAL.to_vec()
        );
        let claude_no_param = entry("anthropic/claude-sonnet-4.5", &[], &[]);
        assert!(or.flavor.reasoning_efforts(&claude_no_param).is_empty());
        let generic = entry("deepseek/reasoner", &["reasoning"], &[]);
        assert_eq!(
            or.flavor.reasoning_efforts(&generic),
            ReasoningEffort::STANDARD.to_vec()
        );

        // Direct OpenAI has no rich /models metadata, so known families use
        // concrete fallbacks rather than assigning every GPT-5 the same set.
        let oa = OpenRouter::openai("k".into());
        assert_eq!(
            oa.flavor.reasoning_efforts(&entry("gpt-5", &[], &[])),
            ReasoningEffort::WITH_MINIMAL.to_vec()
        );
        assert_eq!(
            oa.flavor.reasoning_efforts(&entry("gpt-5-pro", &[], &[])),
            ReasoningEffort::HIGH_ONLY.to_vec()
        );
        assert_eq!(
            oa.flavor.reasoning_efforts(&entry("gpt-5.4", &[], &[])),
            ReasoningEffort::WITH_XHIGH_AND_NONE.to_vec()
        );
        let o3 = entry("o3", &[], &[]);
        assert_eq!(
            oa.flavor.reasoning_efforts(&o3),
            ReasoningEffort::STANDARD.to_vec()
        );
        let gpt41 = entry("gpt-4.1", &[], &[]);
        assert!(oa.flavor.reasoning_efforts(&gpt41).is_empty());

        // Metadata also wins on OpenCode; absent metadata retains its broad
        // compatibility fallback because that catalog is not authoritative.
        let go = OpenRouter::opencode_go("k".into());
        assert_eq!(
            go.flavor.reasoning_efforts(&sparse),
            vec![ReasoningEffort::Minimal, ReasoningEffort::High]
        );
        assert_eq!(
            go.flavor.reasoning_efforts(&gpt41),
            ReasoningEffort::STANDARD.to_vec()
        );
    }

    #[test]
    fn opencode_go_reports_its_own_backend_base_and_defaults() {
        let p = OpenRouter::opencode_go("k".into());
        assert_eq!(p.backend_tag().display_name(), "OpenCode Go");
        assert_eq!(p.flavor.base(), "https://opencode.ai/zen/v1");
        assert!(!p.default_utility_model().is_empty());
        assert!(!p.default_research_model().is_empty());
        assert!(!p.default_escalation_model().is_empty());
        // No embedding models on Go — feature stays disabled by default.
        assert_eq!(p.default_embedding_model(), "");
    }

    #[test]
    fn opencode_route_sends_go_tagged_models_to_the_go_base_untagged() {
        let p = OpenRouter::opencode_go("k".into());
        let (base, model) = p.opencode_route("go:deepseek-v4-pro");
        assert_eq!(base, "https://opencode.ai/zen/go/v1");
        assert_eq!(model, "deepseek-v4-pro");
    }

    #[test]
    fn opencode_route_sends_untagged_models_to_zen_general() {
        let p = OpenRouter::opencode_go("k".into());
        let (base, model) = p.opencode_route("deepseek-v4-flash-free");
        assert_eq!(base, "https://opencode.ai/zen/v1");
        assert_eq!(model, "deepseek-v4-flash-free");
    }

    #[test]
    fn opencode_route_is_a_no_op_for_other_flavors() {
        // A "go:"-prefixed id is meaningless outside OpenCode Go — must not
        // be stripped or rerouted for another flavor.
        let p = OpenRouter::openrouter_flavor("k".into());
        let (base, model) = p.opencode_route("go:whatever");
        assert_eq!(base, "https://openrouter.ai/api/v1");
        assert_eq!(model, "go:whatever");
    }

    #[test]
    fn sse_event_data_joins_multiple_data_lines() {
        let block = "event: response.created\ndata: {\"a\":1}\ndata: more\n\n";
        assert_eq!(sse_event_data(block), "{\"a\":1}\nmore");
    }

    #[test]
    fn sse_event_data_ignores_non_data_lines() {
        let block = "id: 5\nevent: ping\n\n";
        assert_eq!(sse_event_data(block), "");
    }

    #[test]
    fn sse_event_data_handles_done_sentinel() {
        let block = "event: done\ndata: [DONE]\n\n";
        assert_eq!(sse_event_data(block), "[DONE]");
    }

    #[test]
    fn truncate_error_body_reports_empty_body_explicitly() {
        assert_eq!(truncate_error_body(""), "(empty body)");
        assert_eq!(truncate_error_body("   \n  "), "(empty body)");
    }

    #[test]
    fn truncate_error_body_passes_short_text_through() {
        assert_eq!(truncate_error_body("  access denied  "), "access denied");
    }

    #[test]
    fn truncate_error_body_caps_long_text_with_ellipsis() {
        let long = "x".repeat(1000);
        let out = truncate_error_body(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 301); // 300 chars + the ellipsis marker
    }

    #[test]
    fn ocr_body_has_prompt_image_and_token_budget() {
        let body = ocr_body("google/gemini-2.5-flash-lite", "data:image/png;base64,AAAA");
        assert_eq!(body["model"], "google/gemini-2.5-flash-lite");
        assert_eq!(body["stream"], false);
        // Generous output budget — page transcriptions are long.
        assert!(body["max_tokens"].as_u64().unwrap() >= 8000);
        let content = &body["messages"][0]["content"];
        let prompt = content[0]["text"].as_str().unwrap();
        assert!(
            prompt.contains("furigana"),
            "prompt must say to skip furigana"
        );
        assert!(
            prompt.contains("right to left"),
            "prompt must cover vertical text"
        );
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn vision_body_has_image_url_content_part() {
        let body = vision_body("google/gemini-2.5-flash-lite", "data:image/png;base64,AAAA");
        assert_eq!(body["model"], "google/gemini-2.5-flash-lite");
        assert_eq!(body["stream"], false);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert!(
            content[0]["text"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("describe this image")
        );
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn extracts_content_delta() {
        let data = r#"{"choices":[{"delta":{"content":"Hel"}}]}"#;
        let (content, reasoning) = parse_delta(data);
        assert_eq!(content.as_deref(), Some("Hel"));
        assert_eq!(reasoning, None);
    }

    #[test]
    fn extracts_reasoning_delta() {
        let data = r#"{"choices":[{"delta":{"reasoning":"Let me think"}}]}"#;
        let (content, reasoning) = parse_delta(data);
        assert_eq!(content, None);
        assert_eq!(reasoning.as_deref(), Some("Let me think"));
    }

    #[test]
    fn empty_delta_yields_none() {
        let data = r#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert_eq!(parse_delta(data), (None, None));
    }

    #[test]
    fn junk_yields_none() {
        assert_eq!(parse_delta("not json"), (None, None));
    }

    #[test]
    fn parses_usage() {
        let data = r#"{"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":40,"total_tokens":160}}"#;
        let u = parse_usage(data).unwrap();
        assert_eq!(u.prompt_tokens, 120);
        assert_eq!(u.completion_tokens, 40);
        assert_eq!(u.total_tokens, 160);
        assert!(parse_usage(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).is_none());
    }

    #[test]
    fn accumulates_tool_call_fragments_across_chunks() {
        let mut acc = BTreeMap::new();
        accumulate_tool_calls(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"web_search","arguments":""}}]}}]}"#,
        );
        accumulate_tool_calls(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\""}}]}}]}"#,
        );
        accumulate_tool_calls(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"rust\"}"}}]}}]}"#,
        );
        let call = acc.get(&0).unwrap();
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments, r#"{"query":"rust"}"#);
    }

    #[test]
    fn accumulates_multiple_parallel_tool_calls_by_index() {
        let mut acc = BTreeMap::new();
        accumulate_tool_calls(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"a","function":{"name":"skill","arguments":"{}"}},
                {"index":1,"id":"b","function":{"name":"web_search","arguments":"{}"}}
            ]}}]}"#,
        );
        assert_eq!(acc.len(), 2);
        assert_eq!(acc[&0].name, "skill");
        assert_eq!(acc[&1].name, "web_search");
    }

    #[test]
    fn codex_input_emits_every_parallel_function_call_before_outputs() {
        let messages = vec![
            ChatMessage {
                role: "assistant".into(),
                tool_calls: Some(vec![
                    ToolCall {
                        id: "call_a".into(),
                        name: "first".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "call_b".into(),
                        name: "second".into(),
                        arguments: r#"{"value":2}"#.into(),
                    },
                ]),
                ..Default::default()
            },
            ChatMessage {
                role: "tool".into(),
                content: "first result".into(),
                tool_call_id: Some("call_a".into()),
                ..Default::default()
            },
            ChatMessage {
                role: "tool".into(),
                content: "second result".into(),
                tool_call_id: Some("call_b".into()),
                ..Default::default()
            },
        ];

        let input = codex_input(&messages);
        assert_eq!(input.len(), 4);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "call_a");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_b");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_a");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_b");
    }

    #[test]
    fn request_body_omits_tools_key_when_empty() {
        let body = serde_json::json!({ "model": "m", "messages": Vec::<ChatMessage>::new(), "stream": true });
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn parses_input_modalities_into_supports_images() {
        let json = r#"{"data":[
            {"id":"a/vision","architecture":{"input_modalities":["text","image"]}},
            {"id":"b/text","architecture":{"input_modalities":["text"]}},
            {"id":"c/legacy"}
        ]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let flags: Vec<bool> = resp.data.iter().map(entry_supports_images).collect();
        assert_eq!(flags, vec![true, false, false]);
    }

    #[test]
    fn defaults_use_current_quality_generation_models() {
        let provider = OpenRouter::openrouter_flavor("key".into());
        assert_eq!(provider.default_image_gen_model(), "openai/gpt-image-2");
        assert_eq!(provider.default_video_gen_model(), "google/veo-3.1");
    }

    #[test]
    fn image_size_maps_to_normalized_generation_capabilities() {
        assert_eq!(OpenRouter::image_aspect_ratio("1024x1024"), "1:1");
        assert_eq!(OpenRouter::image_aspect_ratio("1024x1792"), "9:16");
        assert_eq!(OpenRouter::image_aspect_ratio("1792x1024"), "16:9");
        assert_eq!(OpenRouter::image_resolution("1792x1024"), "1K");
        assert_eq!(OpenRouter::image_resolution("2048x2048"), "2K");
    }

    #[test]
    fn normalizes_model_specific_video_defaults() {
        let (_, resolution, aspect_ratio) =
            normalize_video_params("minimax/hailuo-3", 8, "720p", "16:9");
        assert_eq!(resolution, "2K");
        assert_eq!(aspect_ratio, "16:9");

        let (duration, resolution, _) =
            normalize_video_params("openai/sora-2-pro", 6, "720p", "16:9");
        assert_eq!(duration, 4);
        assert_eq!(resolution, "720p");
    }
}
