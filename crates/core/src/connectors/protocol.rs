//! JSON-RPC 2.0 and the slice of MCP the connectors use: `server/discover`,
//! `initialize`, `tools/list` (cursor pagination), `tools/call`, the
//! `notifications/*` we send or react to, content blocks, tool annotations
//! and the 2026 `resultType` field. Parsing is deliberately lenient: servers
//! in the wild omit optional fields, so every accessor falls back to a
//! default instead of failing the whole message.

use std::fmt;

use serde_json::{Map, Value, json};

/// The stateless revision: no `initialize`, `_meta` on every request.
pub const PROTOCOL_2026: &str = "2026-07-28";
/// The version offered when a server does not know `server/discover`.
pub const OFFER_LEGACY: &str = "2025-11-25";
/// Versions accepted from an `initialize` answer.
pub const ACCEPT_LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// `_meta` key carrying the protocol version on 2026 requests.
pub const META_PROTOCOL: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key carrying the client capabilities on 2026 requests.
pub const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// `_meta` key carrying the client identity on 2026 requests.
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";

/// A single JSON-RPC message (request, response batch member, notification).
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// JSON-RPC "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;

/// The identity nexus announces to servers.
#[must_use]
pub fn client_info() -> Value {
    json!({"name": "nexus-chat", "version": env!("CARGO_PKG_VERSION")})
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// One decoded incoming message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Response {
        id: Value,
        outcome: Result<Value, RpcError>,
    },
    Request {
        id: Value,
        method: String,
    },
    Notification {
        method: String,
    },
}

impl Message {
    /// Decode one JSON value, or a JSON-RPC batch (an array) into its
    /// members. Unrecognisable shapes are dropped.
    #[must_use]
    pub fn parse_all(value: &Value) -> Vec<Self> {
        match value {
            Value::Array(items) => items.iter().filter_map(Self::parse).collect(),
            other => Self::parse(other).into_iter().collect(),
        }
    }

    /// Decode a single message object.
    #[must_use]
    pub fn parse(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let method = object.get("method").and_then(Value::as_str);
        let id = object.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (Some(method), Some(id)) => Some(Self::Request {
                id: id.clone(),
                method: method.to_string(),
            }),
            (Some(method), None) => Some(Self::Notification {
                method: method.to_string(),
            }),
            (None, Some(id)) => {
                let outcome = match object.get("error") {
                    Some(error) => Err(RpcError {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("error")
                            .to_string(),
                        data: error.get("data").cloned(),
                    }),
                    None => Ok(object.get("result").cloned().unwrap_or(Value::Null)),
                };
                Some(Self::Response {
                    id: id.clone(),
                    outcome,
                })
            }
            (None, None) => None,
        }
    }
}

/// A request message.
#[must_use]
pub fn request(id: u64, method: &str, params: Value) -> Value {
    let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
    message["params"] = params;
    message
}

/// A notification message (no id, no answer).
#[must_use]
pub fn notification(method: &str, params: Option<Value>) -> Value {
    let mut message = json!({"jsonrpc": "2.0", "method": method});
    if let Some(params) = params {
        message["params"] = params;
    }
    message
}

/// A success answer to a server-initiated request.
#[must_use]
pub fn response_ok(id: &Value, result: Value) -> Value {
    let mut message = json!({"jsonrpc": "2.0", "id": id});
    message["result"] = result;
    message
}

/// An error answer to a server-initiated request.
#[must_use]
pub fn response_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The `_meta` object every 2026 request carries.
#[must_use]
pub fn client_meta() -> Value {
    json!({
        META_PROTOCOL: PROTOCOL_2026,
        META_CAPABILITIES: {},
        META_CLIENT_INFO: client_info(),
    })
}

/// Add the client `_meta` to a params object (creating it when absent).
#[must_use]
pub fn with_meta(params: Value) -> Value {
    let mut object = match params {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => {
            let mut map = Map::new();
            map.insert("value".into(), other);
            map
        }
    };
    let meta = object
        .entry("_meta".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let (Some(meta), Value::Object(ours)) = (meta.as_object_mut(), client_meta()) {
        for (key, value) in ours {
            meta.insert(key, value);
        }
    }
    Value::Object(object)
}

/// Tool annotations (`readOnlyHint` and friends). `None` means "unspecified";
/// the spec defaults then apply to anything that matters for safety.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Annotations {
    pub title: Option<String>,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
    /// The object exactly as the server sent it, for the definition hash.
    pub raw: Option<String>,
}

impl Annotations {
    fn parse(value: Option<&Value>) -> Self {
        let Some(object) = value.and_then(Value::as_object) else {
            return Self::default();
        };
        let flag = |key: &str| object.get(key).and_then(Value::as_bool);
        Self {
            title: object
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
            read_only: flag("readOnlyHint"),
            destructive: flag("destructiveHint"),
            idempotent: flag("idempotentHint"),
            open_world: flag("openWorldHint"),
            raw: Some(canonical_json(&Value::Object(object.clone()))),
        }
    }

    /// The tool declares itself read-only and not destructive.
    #[must_use]
    pub fn is_free(&self) -> bool {
        self.read_only == Some(true) && self.destructive != Some(true)
    }

    /// The annotations as words for an approval panel, following the spec
    /// defaults for anything left unspecified.
    #[must_use]
    pub fn words(&self) -> String {
        let mut words = Vec::new();
        if self.read_only == Some(true) {
            words.push("read-only");
        } else {
            words.push("writes");
        }
        if self.destructive == Some(true)
            || (self.read_only != Some(true) && self.destructive != Some(false))
        {
            words.push("may delete");
        }
        match self.idempotent {
            Some(true) => words.push("idempotent"),
            Some(false) => words.push("not idempotent"),
            None => {}
        }
        if self.open_world != Some(false) {
            words.push("reaches the internet");
        }
        if self.raw.is_none() {
            words.push("no annotations from the server");
        }
        words.join(" · ")
    }
}

/// One tool as listed by a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Annotations,
}

impl ToolInfo {
    fn parse(value: &Value) -> Option<Self> {
        let name = value.get("name")?.as_str()?.to_string();
        if name.is_empty() {
            return None;
        }
        let annotations = Annotations::parse(value.get("annotations"));
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| annotations.title.clone());
        Some(Self {
            name,
            title,
            description: value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_schema: value
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            annotations,
        })
    }

    /// `sha256(name + description + inputSchema + annotations)`, hex. Any
    /// change a server makes to what the model (or the user) was told about
    /// the tool changes the hash.
    #[must_use]
    pub fn definition_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for part in [
            self.name.as_str(),
            self.description.as_str(),
            canonical_json(&self.input_schema).as_str(),
            self.annotations.raw.as_deref().unwrap_or(""),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        hasher
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    }
}

/// One `tools/list` page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolsPage {
    pub tools: Vec<ToolInfo>,
    pub next_cursor: Option<String>,
    pub ttl_ms: Option<u64>,
}

impl ToolsPage {
    #[must_use]
    pub fn parse(result: &Value) -> Self {
        Self {
            tools: result
                .get("tools")
                .and_then(Value::as_array)
                .map(|tools| tools.iter().filter_map(ToolInfo::parse).collect())
                .unwrap_or_default(),
            next_cursor: result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_string),
            ttl_ms: result.get("ttlMs").and_then(Value::as_u64),
        }
    }
}

/// A content block of a `tools/call` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Image {
        data: String,
        mime: String,
    },
    Audio {
        mime: String,
        bytes: usize,
    },
    ResourceLink {
        uri: String,
        name: String,
        description: String,
    },
    Resource {
        uri: String,
        mime: String,
        text: Option<String>,
        blob: Option<String>,
    },
    Unknown(String),
}

impl Content {
    fn parse(value: &Value) -> Self {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match value.get("type").and_then(Value::as_str) {
            Some("text") => Self::Text(text("text")),
            Some("image") => Self::Image {
                data: text("data"),
                mime: text("mimeType"),
            },
            Some("audio") => Self::Audio {
                mime: text("mimeType"),
                bytes: value
                    .get("data")
                    .and_then(Value::as_str)
                    .map_or(0, |data| data.len() / 4 * 3),
            },
            Some("resource_link") => Self::ResourceLink {
                uri: text("uri"),
                name: if value.get("title").is_some() {
                    text("title")
                } else {
                    text("name")
                },
                description: text("description"),
            },
            Some("resource") => {
                let resource = value.get("resource").unwrap_or(&Value::Null);
                let field = |key: &str| {
                    resource
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                };
                Self::Resource {
                    uri: field("uri").unwrap_or_default(),
                    mime: field("mimeType").unwrap_or_default(),
                    text: field("text"),
                    blob: field("blob"),
                }
            }
            other => Self::Unknown(other.unwrap_or("unknown").to_string()),
        }
    }
}

/// How a `tools/call` ended on the protocol level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultType {
    Complete,
    /// The server wants input (elicitation); not supported before C3.
    InputRequired,
    /// Anything else the server invents.
    Other,
}

/// A decoded `tools/call` result.
#[derive(Debug, Clone, PartialEq)]
pub struct CallResult {
    pub content: Vec<Content>,
    pub structured: Option<Value>,
    pub is_error: bool,
    pub result_type: ResultType,
}

impl CallResult {
    #[must_use]
    pub fn parse(result: &Value) -> Self {
        let result_type = match result.get("resultType").and_then(Value::as_str) {
            None | Some("complete") => ResultType::Complete,
            Some("input_required") => ResultType::InputRequired,
            Some(_) => ResultType::Other,
        };
        Self {
            content: result
                .get("content")
                .and_then(Value::as_array)
                .map(|blocks| blocks.iter().map(Content::parse).collect())
                .unwrap_or_default(),
            structured: result.get("structuredContent").cloned(),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            result_type,
        }
    }
}

/// Serialize with object keys sorted, so equal values hash equally whatever
/// order a server (or the JSON library's map) emits them in.
#[must_use]
pub fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(key.clone()).to_string());
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_told_apart_and_batches_flatten() {
        let batch = json!([
            {"jsonrpc": "2.0", "id": 1, "result": {"ok": true}},
            {"jsonrpc": "2.0", "id": 2, "error": {"code": -32601, "message": "nope"}},
            {"jsonrpc": "2.0", "method": "notifications/tools/list_changed"},
            {"jsonrpc": "2.0", "id": "p", "method": "ping"},
            {"nonsense": true}
        ]);
        let messages = Message::parse_all(&batch);
        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[1],
            Message::Response { outcome: Err(error), .. } if error.code == METHOD_NOT_FOUND
        ));
        assert!(
            matches!(&messages[2], Message::Notification { method } if method.ends_with("list_changed"))
        );
        assert!(matches!(&messages[3], Message::Request { method, .. } if method == "ping"));
    }

    #[test]
    fn meta_is_added_to_params_without_losing_them() {
        let params = with_meta(json!({"name": "t", "_meta": {"x": 1}}));
        assert_eq!(params["name"], "t");
        assert_eq!(params["_meta"]["x"], 1);
        assert_eq!(params["_meta"][META_PROTOCOL], PROTOCOL_2026);
        assert_eq!(params["_meta"][META_CLIENT_INFO]["name"], "nexus-chat");
        assert!(params["_meta"][META_CAPABILITIES].is_object());
        assert_eq!(
            with_meta(Value::Null)["_meta"][META_PROTOCOL],
            PROTOCOL_2026
        );
    }

    #[test]
    fn tools_parse_with_annotations_and_hash_is_order_independent() {
        let a = ToolsPage::parse(&json!({
            "tools": [{
                "name": "create", "title": "Create", "description": "d",
                "inputSchema": {"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "number"}}},
                "annotations": {"readOnlyHint": false, "destructiveHint": true}
            }, {"name": ""}, {"nothing": 1}],
            "nextCursor": "c2", "ttlMs": 1500
        }));
        assert_eq!(a.tools.len(), 1);
        assert_eq!(a.next_cursor.as_deref(), Some("c2"));
        assert_eq!(a.ttl_ms, Some(1500));
        let b = ToolsPage::parse(&json!({
            "tools": [{
                "annotations": {"destructiveHint": true, "readOnlyHint": false},
                "inputSchema": {"properties": {"b": {"type": "number"}, "a": {"type": "string"}}, "type": "object"},
                "description": "d", "title": "Create", "name": "create"
            }]
        }));
        assert_eq!(a.tools[0].definition_hash(), b.tools[0].definition_hash());
        let mut changed = b.tools[0].clone();
        changed.description = "d2".into();
        assert_ne!(changed.definition_hash(), a.tools[0].definition_hash());
        assert_eq!(a.tools[0].definition_hash().len(), 64);
    }

    #[test]
    fn annotation_words_follow_the_spec_defaults() {
        let free = Annotations {
            read_only: Some(true),
            open_world: Some(false),
            raw: Some("{}".into()),
            ..Annotations::default()
        };
        assert!(free.is_free());
        assert_eq!(free.words(), "read-only");
        let unknown = Annotations::default();
        assert!(!unknown.is_free());
        assert_eq!(
            unknown.words(),
            "writes · may delete · reaches the internet · no annotations from the server"
        );
        let hostile = Annotations {
            read_only: Some(true),
            destructive: Some(true),
            raw: Some("{}".into()),
            ..Annotations::default()
        };
        assert!(!hostile.is_free());
    }

    #[test]
    fn conflicting_annotation_hints_still_show_the_destructive_warning() {
        let annotations = Annotations {
            read_only: Some(true),
            destructive: Some(true),
            idempotent: Some(false),
            open_world: Some(false),
            raw: Some("{}".into()),
            ..Annotations::default()
        };
        assert_eq!(
            annotations.words(),
            "read-only · may delete · not idempotent"
        );
        assert!(!annotations.is_free());
    }

    #[test]
    fn call_results_decode_every_content_type_and_default_to_complete() {
        let result = CallResult::parse(&json!({
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "image", "data": "aGk=", "mimeType": "image/png"},
                {"type": "audio", "data": "aGk=", "mimeType": "audio/mpeg"},
                {"type": "resource_link", "uri": "file:///a", "name": "a", "description": "d"},
                {"type": "resource", "resource": {"uri": "file:///b", "text": "body"}},
                {"type": "hologram"}
            ],
            "structuredContent": {"n": 1},
            "isError": true
        }));
        assert_eq!(result.content.len(), 6);
        assert!(result.is_error);
        assert_eq!(result.result_type, ResultType::Complete);
        assert!(matches!(&result.content[5], Content::Unknown(kind) if kind == "hologram"));
        assert_eq!(result.structured, Some(json!({"n": 1})));
        assert_eq!(
            CallResult::parse(&json!({"resultType": "input_required"})).result_type,
            ResultType::InputRequired
        );
    }
}
