pub mod openrouter;

use serde::{Deserialize, Serialize};

/// A tool the model may call, in `OpenAI` function-calling shape.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// One call the model made to a tool, accumulated from streamed fragments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments string, as the model streamed it.
    pub arguments: String,
}

/// Which configured backend a `Model` came from — every backend's models
/// are merged into one list (`App::models`), so this is how a pick routes
/// back to the right one at request time.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum BackendTag {
    OpenRouter,
    OpenAi,
    OpencodeGo,
    Codex,
}

impl BackendTag {
    /// Human-readable backend name, used in usage analytics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::OpenAi => "OpenAI",
            Self::OpencodeGo => "OpenCode Go",
            Self::Codex => "Codex",
        }
    }

    /// Prefix used to key favorites/last-used/current-model/etc. for this
    /// backend's models — bare (no prefix) for ``OpenRouter`` so existing
    /// users' saved data keeps working untouched; the other three are
    /// visually tagged since their raw ids can collide (e.g. two "gpt-4.1"s).
    pub const fn key_prefix(self) -> &'static str {
        match self {
            Self::OpenRouter => "",
            Self::OpenAi => "openai:",
            Self::OpencodeGo => "opencode:",
            Self::Codex => "codex:",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::OpenAi => "OpenAI",
            Self::OpencodeGo => "OpenCode Go",
            Self::Codex => "Codex",
        }
    }
}

/// A reasoning/thinking effort value a model accepts, in cycle order.
/// Provider catalogs expose different subsets per model, so this enum covers
/// every effort currently present in those catalogs. `None` is the explicit
/// wire-level disable value; absence from `ChatParams` still means "do not
/// send a reasoning parameter".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    /// Wire value sent to the provider (and stored in `model_prefs`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Every known wire value in UI cycle order: enabled tiers from least to
    /// most thinking, then explicit disable when the model accepts it.
    pub const CYCLE_ORDER: &'static [Self] = &[
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
        Self::None,
    ];

    /// The values most reasoning models accept when no richer metadata exists.
    pub const STANDARD: &'static [Self] = &[Self::Low, Self::Medium, Self::High];

    /// Models that add a `minimal` tier to the standard set.
    pub const WITH_MINIMAL: &'static [Self] = &[Self::Minimal, Self::Low, Self::Medium, Self::High];

    /// Models that can be explicitly disabled and add an `xhigh` tier.
    pub const WITH_XHIGH_AND_NONE: &'static [Self] =
        &[Self::Low, Self::Medium, Self::High, Self::XHigh, Self::None];

    /// Models that additionally expose a top-level `max` tier.
    pub const WITH_MAX_XHIGH_AND_NONE: &'static [Self] = &[
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
        Self::None,
    ];

    /// Models whose only configurable reasoning tier is `high`.
    pub const HIGH_ONLY: &'static [Self] = &[Self::High];
}

/// Per-token catalog prices in USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    pub prompt: f64,
    pub completion: f64,
    /// Discounted cache-read price. `None` means cache reads use the regular
    /// prompt price (or the catalog does not expose a separate rate).
    pub cache_read: Option<f64>,
    /// Cache-write price. `None` means writes use the regular prompt price.
    pub cache_write: Option<f64>,
}

/// A model offered by the provider, as shown in the picker.
#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// The reasoning effort values this model accepts, in Ctrl+T cycle order.
    /// Empty = the model has no reasoning/thinking mode at all.
    pub reasoning_efforts: Vec<ReasoningEffort>,
    /// Context window size in tokens, if the provider reports it.
    pub context_length: Option<u64>,
    /// Whether the model accepts image input (`architecture.input_modalities`).
    pub supports_images: bool,
    /// Whether the model generates image output (`architecture.output_modalities`).
    pub supports_image_generation: bool,
    /// Whether the model is listed by ``OpenRouter``'s dedicated video catalog.
    pub supports_video_generation: bool,
    /// Which backend this model came from.
    pub backend: BackendTag,
    /// USD per 1M tokens from the catalog (`OpenRouter` only; other backends
    /// report no prices). `None` = cost unknown.
    pub pricing: Option<ModelPricing>,
}

/// Sampling + reasoning parameters for a completion request.
#[derive(Debug, Clone, Default)]
pub struct ChatParams {
    /// Reasoning effort wire value (for example `minimal`, `low`, `high`,
    /// `xhigh`, `max`, or explicit `none`), as stored per model. Rust `None`
    /// means do not send the parameter at all.
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// A message sent to the completions API. `tool_calls` (assistant requesting
/// tools) and `tool_call_id` (a tool's result) are only set on those two
/// message shapes; wire format follows the `OpenAI` function-calling schema.
/// `images` (data URLs) are set for user messages with attachments when the
/// active model supports vision; when non-empty, `content` serializes as an
/// `OpenAI` vision parts array instead of a plain string.
#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub images: Vec<String>,
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }
}

#[derive(Serialize)]
struct Function<'a> {
    name: &'a str,
    arguments: &'a str,
}

#[derive(Serialize)]
struct Wire<'a> {
    id: &'a str,
    r#type: &'static str,
    function: Function<'a>,
}

impl Serialize for ChatMessage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("role", &self.role)?;
        if self.images.is_empty() {
            map.serialize_entry("content", &self.content)?;
        } else {
            // OpenAI vision shape: text part (if any) + one image_url part per image.
            let mut parts: Vec<serde_json::Value> = Vec::new();
            if !self.content.is_empty() {
                parts.push(serde_json::json!({ "type": "text", "text": self.content }));
            }
            for url in &self.images {
                parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": url } }));
            }
            map.serialize_entry("content", &parts)?;
        }
        if let Some(calls) = &self.tool_calls {
            let wire: Vec<Wire> = calls
                .iter()
                .map(|c| Wire {
                    id: &c.id,
                    r#type: "function",
                    function: Function {
                        name: &c.name,
                        arguments: &c.arguments,
                    },
                })
                .collect();
            map.serialize_entry("tool_calls", &wire)?;
        }
        if let Some(id) = &self.tool_call_id {
            map.serialize_entry("tool_call_id", id)?;
        }
        map.end()
    }
}

/// Events emitted while a completion streams. Delivered over an mpsc channel so
/// the UI event loop can interleave them with keypresses.
/// Exact token accounting reported by the provider at end of stream.
#[derive(Debug, Clone, Copy)]
// _tokens postfix is the unit — removing it would make the fields ambiguous.
#[allow(clippy::struct_field_names)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Prompt tokens served from the provider's prompt cache (cache reads).
    pub cache_read_tokens: u64,
    /// Prompt tokens written into the cache on this request (cache writes).
    pub cache_creation_tokens: u64,
    /// Provider-reported request cost in USD. `OpenRouter` includes it in the
    /// final usage object; `OpenCode` Zen/Go reports it in a trailing streamed
    /// chunk (`"cost":"0.0012"`). `None` when the provider omits cost.
    pub cost: Option<f64>,
}

impl Usage {
    /// Fraction of this request's prompt served from cache, 0.0..=1.0.
    /// `None` when the provider reported no usage or a zero prompt.
    #[allow(clippy::cast_precision_loss)] // token counts are too large for u32; a ratio loses nothing meaningful
    pub fn cache_hit_rate(&self) -> Option<f64> {
        if self.prompt_tokens == 0 {
            None
        } else {
            Some((self.cache_read_tokens as f64 / self.prompt_tokens as f64).clamp(0.0, 1.0))
        }
    }
}

/// Rebuild the tool-result dedup state — `(tool, arguments) → latest full
/// result` — from a wire history produced by `build_history`. Both that
/// replay and the live tool loop apply the same rule ("keep the first full
/// copy of a (tool, arguments, result) triple; replace later identical
/// copies with `tool_result_unchanged_note`"), and the loop seeds its map
/// from the incoming history so its compression decisions match what the
/// next turn's replay will rebuild — which keeps the prompt-cache prefix
/// continuous across turns. Note messages (marked with
/// `TOOL_RESULT_OMITTED_PREFIX`) never update the map, exactly as in the
/// replay.
pub fn seed_tool_result_dedup(
    messages: &[ChatMessage],
) -> std::collections::HashMap<(String, String), String> {
    use std::collections::HashMap;
    let mut seen: HashMap<(String, String), String> = HashMap::new();
    // Assistant tool_calls arrive immediately before their tool results, in
    // order (build_history emits one pair per row; the loop emits the call
    // batch then its results) — a FIFO queue pairs them up.
    let mut pending: std::collections::VecDeque<(String, String)> =
        std::collections::VecDeque::new();
    for msg in messages {
        if let Some(calls) = &msg.tool_calls {
            for call in calls {
                pending.push_back((call.name.clone(), call.arguments.clone()));
            }
        } else if msg.role == "tool"
            && let Some((name, args)) = pending.pop_front()
            && !msg
                .content
                .starts_with(crate::tools::TOOL_RESULT_OMITTED_PREFIX)
        {
            seen.insert((name, args), msg.content.clone());
        }
    }
    seen
}

#[derive(Debug)]
pub enum StreamEvent {
    /// A chunk of the visible answer.
    Token(String),
    /// A chunk of the model's reasoning/thinking (shown separately).
    Reasoning(String),
    /// Exact token counts (arrives near end when usage accounting is on).
    Usage(Usage),
    /// A tool is about to run (e.g. "Searching the web…"), shown next to the
    /// thinking spinner while the model waits on the result.
    Status(String),
    /// A tool finished: shown (and persisted) as its own transcript block.
    ToolCall {
        name: String,
        arguments: String,
        result: String,
    },
    Done,
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_tool_result_dedup_reconstructs_replay_state() {
        // A build_history-style wire history: full v1, note (dup), full v2
        // (file changed), note (dup of v2).
        let pair = |id: &str, name: &str, args: &str, content: &str| {
            vec![
                ChatMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    tool_calls: Some(vec![ToolCall {
                        id: id.into(),
                        name: name.into(),
                        arguments: args.into(),
                    }]),
                    tool_call_id: None,
                    images: Vec::new(),
                },
                ChatMessage {
                    role: "tool".into(),
                    content: content.into(),
                    tool_calls: None,
                    tool_call_id: Some(id.into()),
                    images: Vec::new(),
                },
            ]
        };
        let args = r#"{"name":"a.txt"}"#;
        let mut msgs = pair("c0", "read_file", args, "v1");
        msgs.extend(pair(
            "c1",
            "read_file",
            args,
            crate::tools::tool_result_unchanged_note("read_file", args).as_str(),
        ));
        msgs.extend(pair("c2", "read_file", args, "v2"));
        msgs.extend(pair(
            "c3",
            "read_file",
            args,
            crate::tools::tool_result_unchanged_note("read_file", args).as_str(),
        ));
        let seen = seed_tool_result_dedup(&msgs);
        // Notes must never overwrite the map: the latest full result wins.
        assert_eq!(
            seen.get(&("read_file".to_string(), args.to_string())),
            Some(&"v2".to_string())
        );
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn seed_tool_result_dedup_keeps_latest_full_per_call() {
        let msgs = vec![
            ChatMessage {
                role: "assistant".into(),
                content: String::new(),
                tool_calls: Some(vec![ToolCall {
                    id: "a".into(),
                    name: "search".into(),
                    arguments: r#"{"query":"x"}"#.into(),
                }]),
                tool_call_id: None,
                images: Vec::new(),
            },
            ChatMessage {
                role: "tool".into(),
                content: "hits-a".into(),
                tool_calls: None,
                tool_call_id: Some("a".into()),
                images: Vec::new(),
            },
        ];
        let seen = seed_tool_result_dedup(&msgs);
        assert_eq!(
            seen.get(&("search".to_string(), r#"{"query":"x"}"#.to_string())),
            Some(&"hits-a".to_string())
        );
    }

    #[test]
    fn chat_message_serializes_string_content_when_no_images() {
        let m = ChatMessage::text("user", "hi");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"], "hi");
        assert!(v.get("tool_calls").is_none());
    }

    #[test]
    fn chat_message_serializes_parts_when_images_present() {
        let mut m = ChatMessage::text("user", "what is this?");
        m.images = vec!["data:image/png;base64,AAAA".into()];
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "what is this?");
        assert_eq!(v["content"][1]["type"], "image_url");
        assert_eq!(
            v["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
    }

    #[test]
    fn chat_message_with_tool_calls_still_serializes_them() {
        let m = ChatMessage {
            role: "assistant".into(),
            content: "".into(),
            tool_calls: Some(vec![ToolCall {
                id: "c1".into(),
                name: "web_search".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
            images: Vec::new(),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["tool_calls"][0]["function"]["name"], "web_search");
        assert_eq!(v["tool_calls"][0]["type"], "function");
    }
}
