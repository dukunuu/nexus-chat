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

/// A model offered by the provider, as shown in the picker.
#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// Whether the model accepts a `reasoning` parameter (thinking effort).
    pub supports_reasoning: bool,
    /// Context window size in tokens, if the provider reports it.
    pub context_length: Option<u64>,
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
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_tool_calls")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage { role: role.into(), content: content.into(), ..Default::default() }
    }
}

fn serialize_tool_calls<S: serde::Serializer>(
    calls: &Option<Vec<ToolCall>>,
    s: S,
) -> Result<S::Ok, S::Error> {
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
    let calls = calls.as_ref().expect("skip_serializing_if guards this");
    let wire: Vec<Wire> = calls
        .iter()
        .map(|c| Wire { id: &c.id, r#type: "function", function: Function { name: &c.name, arguments: &c.arguments } })
        .collect();
    wire.serialize(s)
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
    Done,
    Error(String),
}
