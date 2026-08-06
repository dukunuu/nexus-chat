pub mod openrouter;

use serde::{Deserialize, Serialize};

/// A tool the model may call, in OpenAI function-calling shape.
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
    /// Prefix used to key favorites/last-used/current-model/etc. for this
    /// backend's models — bare (no prefix) for OpenRouter so existing
    /// users' saved data keeps working untouched; the other three are
    /// visually tagged since their raw ids can collide (e.g. two "gpt-4.1"s).
    pub fn key_prefix(self) -> &'static str {
        match self {
            BackendTag::OpenRouter => "",
            BackendTag::OpenAi => "openai:",
            BackendTag::OpencodeGo => "opencode:",
            BackendTag::Codex => "codex:",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            BackendTag::OpenRouter => "OpenRouter",
            BackendTag::OpenAi => "OpenAI",
            BackendTag::OpencodeGo => "OpenCode Go",
            BackendTag::Codex => "Codex",
        }
    }
}

/// A model offered by the provider, as shown in the picker.
#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// Whether the model accepts a `reasoning` parameter (thinking effort).
    pub supports_reasoning: bool,
    /// Context window size in tokens, if the provider reports it.
    pub context_length: Option<u64>,
    /// Whether the model accepts image input (`architecture.input_modalities`).
    pub supports_images: bool,
    /// Whether the model generates image output (`architecture.output_modalities`).
    pub supports_image_generation: bool,
    /// Whether the model is listed by OpenRouter's dedicated video catalog.
    pub supports_video_generation: bool,
    /// Which backend this model came from.
    pub backend: BackendTag,
}

/// Sampling + reasoning parameters for a completion request.
#[derive(Debug, Clone, Default)]
pub struct ChatParams {
    /// Reasoning effort: "low" | "medium" | "high". None = don't send it.
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// A message sent to the completions API. `tool_calls` (assistant requesting
/// tools) and `tool_call_id` (a tool's result) are only set on those two
/// message shapes; wire format follows the OpenAI function-calling schema.
/// `images` (data URLs) are set for user messages with attachments when the
/// active model supports vision; when non-empty, `content` serializes as an
/// OpenAI vision parts array instead of a plain string.
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
        ChatMessage {
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
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
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
