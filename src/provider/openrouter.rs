use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::{ChatMessage, ChatParams, Model, StreamEvent, ToolCall, ToolDef, Usage};
use crate::tools::ToolBox;

const BASE: &str = "https://openrouter.ai/api/v1";
/// Hard cap on tool round-trips per response, so a model that keeps calling
/// tools can't loop forever.
const MAX_TOOL_ITERS: usize = 5;

#[derive(Clone)]
pub struct OpenRouter {
    client: reqwest::Client,
    key: String,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    context_length: Option<u64>,
}

impl OpenRouter {
    pub fn new(key: String) -> Self {
        OpenRouter {
            client: reqwest::Client::new(),
            key,
        }
    }

    /// Fetch the live model catalog. No hardcoded list.
    pub async fn list_models(&self) -> Result<Vec<Model>> {
        let resp = self
            .client
            .get(format!("{BASE}/models"))
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
            .map(|m| Model {
                name: m.name.unwrap_or_else(|| m.id.clone()),
                supports_reasoning: m.supported_parameters.iter().any(|p| p == "reasoning"),
                context_length: m.context_length,
                id: m.id,
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// One-shot, non-streaming completion. Used for short utility calls like
    /// generating a session topic/slug. Returns the assistant's message text.
    pub async fn complete(&self, model: &str, messages: Vec<ChatMessage>) -> Result<String> {
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        self.post_completion(body).await
    }

    /// POST a completions body and pull the first choice's message text.
    async fn post_completion(&self, body: serde_json::Value) -> Result<String> {
        let v = self
            .client
            .post(format!("{BASE}/chat/completions"))
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
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// One-shot, non-streaming vision call: transcribe `image_data_url` with `model`.
    #[allow(dead_code)] // used from Task 11 of the filesets plan; remove with first caller
    pub async fn transcribe_image(&self, model: &str, image_data_url: &str) -> Result<String> {
        self.post_completion(vision_body(model, image_data_url)).await
    }

    /// Start a streaming completion. Spawns a task that pushes tokens over the
    /// returned channel; the UI loop drains it alongside keypresses. If the
    /// model calls a tool, the task runs it via `toolbox` and continues the
    /// conversation, bounded by `MAX_TOOL_ITERS` round-trips.
    pub fn stream_chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<ToolBox>,
    ) -> mpsc::UnboundedReceiver<StreamEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.run_chat_loop(model, messages, params, tools, toolbox, &tx).await
            {
                let _ = tx.send(StreamEvent::Error(e.to_string()));
            }
        });
        rx
    }

    async fn run_chat_loop(
        &self,
        model: String,
        mut messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<ToolBox>,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        for iter in 0..=MAX_TOOL_ITERS {
            // On the final allowed iteration, omit tools so the model is
            // forced to answer with whatever it has instead of looping.
            let send_tools: &[ToolDef] = if iter < MAX_TOOL_ITERS { &tools } else { &[] };
            match self.run_stream(&model, &messages, &params, send_tools, tx).await? {
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
                    });
                    for call in &calls {
                        let (result, status) = toolbox.run(&call.name, &call.arguments).await;
                        let _ = tx.send(StreamEvent::Status(status));
                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: result,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
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
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            // Ask OpenRouter for exact token accounting in the final chunk.
            "usage": { "include": true },
        });
        let obj = body.as_object_mut().expect("body is a json object");
        if let Some(effort) = &params.reasoning_effort {
            obj.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
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
            .post(format!("{BASE}/chat/completions"))
            .bearer_auth(&self.key)
            .json(&body);

        let mut es = EventSource::new(request).context("opening SSE stream")?;
        let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let mut content_acc = String::new();
        let mut finish_reason: Option<String> = None;
        while let Some(event) = es.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(msg)) => {
                    if msg.data == "[DONE]" {
                        break;
                    }
                    let (content, reasoning) = parse_delta(&msg.data);
                    if let Some(r) = reasoning
                        && !r.is_empty() {
                            let _ = tx.send(StreamEvent::Reasoning(r));
                        }
                    if let Some(token) = content
                        && !token.is_empty() {
                            content_acc.push_str(&token);
                            let _ = tx.send(StreamEvent::Token(token));
                        }
                    accumulate_tool_calls(&mut tool_calls, &msg.data);
                    if let Some(fr) = parse_finish_reason(&msg.data) {
                        finish_reason = Some(fr);
                    }
                    if let Some(usage) = parse_usage(&msg.data) {
                        let _ = tx.send(StreamEvent::Usage(usage));
                    }
                }
                Err(reqwest_eventsource::Error::StreamEnded) => break,
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string()));
                    es.close();
                    return Ok(Finish::Errored);
                }
            }
        }
        if finish_reason.as_deref() == Some("tool_calls") && !tool_calls.is_empty() {
            Ok(Finish::ToolCalls(tool_calls.into_values().collect(), content_acc))
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

/// Pull `(content, reasoning)` deltas out of one SSE data chunk. OpenRouter puts
/// thinking tokens in `delta.reasoning`, separate from the visible `delta.content`.
fn parse_delta(data: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return (None, None);
    };
    let Some(delta) = v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("delta"))
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
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { return };
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
            && !id.is_empty() {
                entry.id = id.to_string();
            }
        if let Some(func) = call.get("function") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str())
                && !name.is_empty() {
                    entry.name = name.to_string();
                }
            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                entry.arguments.push_str(args);
            }
        }
    }
}

/// Pull `choices[0].finish_reason` out of an SSE chunk, if present.
fn parse_finish_reason(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("choices")?.get(0)?.get("finish_reason")?.as_str().map(str::to_string)
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

/// Request body for a one-shot image-transcription call: a text part with the
/// instruction plus the image as a data-URL content part (OpenAI vision shape).
fn vision_body(model: &str, image_data_url: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text",
                  "text": "Transcribe this image faithfully. Reproduce all visible text verbatim \
                           (preserve code, tables, and structure as markdown). If parts are not \
                           text, describe them briefly in [brackets]. Output only the transcription." },
                { "type": "image_url", "image_url": { "url": image_data_url } },
            ],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_body_has_image_url_content_part() {
        let body = vision_body("google/gemini-2.5-flash-lite", "data:image/png;base64,AAAA");
        assert_eq!(body["model"], "google/gemini-2.5-flash-lite");
        assert_eq!(body["stream"], false);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert!(content[0]["text"].as_str().unwrap().to_lowercase().contains("transcribe"));
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
    fn parses_finish_reason() {
        assert_eq!(
            parse_finish_reason(r#"{"choices":[{"finish_reason":"tool_calls"}]}"#),
            Some("tool_calls".to_string())
        );
        assert_eq!(parse_finish_reason(r#"{"choices":[{"delta":{}}]}"#), None);
    }

    #[test]
    fn request_body_omits_tools_key_when_empty() {
        let body = serde_json::json!({ "model": "m", "messages": Vec::<ChatMessage>::new(), "stream": true });
        assert!(body.get("tools").is_none());
    }
}
