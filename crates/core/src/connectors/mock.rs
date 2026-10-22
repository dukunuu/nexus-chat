//! Test-only MCP servers: one protocol brain (`Mock::respond`) behind two
//! doors -- an in-process HTTP server on 127.0.0.1 (JSON or SSE bodies, a
//! 2026-style flavor, a 2025-style flavor, a 401 flavor) and a stdio server
//! the test executable runs when re-launched with a marker environment
//! variable. No npx, no network, no real keychain.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

/// How the server negotiates.
#[derive(Clone, Debug)]
pub enum Flavor {
    /// Answers `server/discover`; stateless; requires `_meta`.
    Modern,
    /// Only knows `initialize`; sessions; `discover` is an HTTP 404.
    Legacy { version: &'static str },
    /// Only knows `initialize`; `discover` is a JSON-RPC method-not-found.
    LegacyRpcError { version: &'static str },
}

/// One request the server saw.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: Value,
}

/// One tool definition as JSON.
pub fn tool(name: &str, description: &str, annotations: Option<Value>) -> Value {
    let mut tool = json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
    });
    if let Some(annotations) = annotations {
        tool["annotations"] = annotations;
    }
    tool
}

/// A tiny valid-looking PNG payload (not a real image; the tests never decode it).
pub const PNG_B64: &str = "iVBORw0KGgo=";

/// The tools every mock offers.
pub fn standard_tools() -> Vec<Value> {
    let read = || Some(json!({"readOnlyHint": true, "openWorldHint": false}));
    vec![
        tool("echo", "Echo the arguments back.", read()),
        tool("mixed", "Return every content type.", read()),
        tool("image", "Return an image.", read()),
        tool("fail", "Always reports an error.", read()),
        tool("structured", "Return only structured content.", read()),
        tool("slow", "Takes a long time.", read()),
        tool("input", "Asks for input.", read()),
        tool("rpc_error", "Fails at the protocol level.", read()),
        tool(
            "write_note",
            "Write a note.",
            Some(json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false})),
        ),
        tool(
            "delete_note",
            "Delete a note.",
            Some(json!({"readOnlyHint": false, "destructiveHint": true})),
        ),
        tool(
            "sneaky",
            "Says it is read-only but also destructive.",
            Some(json!({"readOnlyHint": true, "destructiveHint": true})),
        ),
        tool("unannotated", "No annotations at all.", None),
        tool(
            "mutate",
            "Changes a tool definition and announces it.",
            Some(json!({"readOnlyHint": false})),
        ),
    ]
}

/// The protocol brain.
pub struct Mock {
    pub flavor: Flavor,
    pub tools: Mutex<Vec<Value>>,
    pub page_size: usize,
    pub ttl_ms: Option<u64>,
    pub bearer: Option<String>,
    pub list_status: Option<u16>,
    pub rpc_error_message: String,
    /// Answer these methods as SSE streams.
    pub sse_methods: Mutex<HashSet<String>>,
    /// Expire the legacy session after this many post-initialize requests.
    pub expire_session_after: Option<usize>,
    pub seen: Mutex<Vec<Seen>>,
    pub calls: Mutex<Vec<(String, Value)>>,
    pub list_count: AtomicUsize,
    pub sessions: AtomicUsize,
    after_init: AtomicUsize,
    pending_notifications: Mutex<Vec<Value>>,
}

impl Mock {
    pub fn new(flavor: Flavor) -> Self {
        Self {
            flavor,
            tools: Mutex::new(standard_tools()),
            page_size: 5,
            ttl_ms: None,
            bearer: None,
            list_status: None,
            rpc_error_message: "backend exploded".into(),
            sse_methods: Mutex::default(),
            expire_session_after: None,
            seen: Mutex::default(),
            calls: Mutex::default(),
            list_count: AtomicUsize::new(0),
            sessions: AtomicUsize::new(0),
            after_init: AtomicUsize::new(0),
            pending_notifications: Mutex::default(),
        }
    }

    pub fn methods(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect()
    }

    pub fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    pub fn find(&self, method: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.method == method)
            .cloned()
            .collect()
    }

    fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
    }

    fn ok(id: &Value, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    /// Answer one JSON-RPC message with zero or more messages (pending
    /// notifications first, then the response).
    pub fn respond(&self, message: &Value) -> Vec<Value> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(id) = message.get("id").filter(|id| !id.is_null()) else {
            return Vec::new();
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let modern = matches!(self.flavor, Flavor::Modern);
        let response = match method {
            "server/discover" if modern => Self::ok(
                id,
                json!({"supportedVersions": ["2026-07-28"], "serverInfo": {"name": "mock", "version": "1"}, "capabilities": {"tools": {}}}),
            ),
            "server/discover" => Self::rpc_error(id, -32601, "Method not found"),
            "initialize" if !modern => {
                let version = match self.flavor {
                    Flavor::Legacy { version } | Flavor::LegacyRpcError { version } => version,
                    Flavor::Modern => unreachable!(),
                };
                self.after_init.store(0, Ordering::SeqCst);
                Self::ok(
                    id,
                    json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": true}}, "serverInfo": {"name": "mock", "version": "1"}}),
                )
            }
            "initialize" => Self::rpc_error(id, -32601, "Method not found"),
            "ping" => Self::ok(id, json!({})),
            "tools/list" => {
                self.list_count.fetch_add(1, Ordering::SeqCst);
                let tools = self.tools.lock().unwrap().clone();
                let start: usize = params
                    .get("cursor")
                    .and_then(Value::as_str)
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                let end = (start + self.page_size).min(tools.len());
                let mut result = json!({"tools": tools[start.min(tools.len())..end]});
                if end < tools.len() {
                    result["nextCursor"] = json!(end.to_string());
                }
                if let Some(ttl) = self.ttl_ms {
                    result["ttlMs"] = json!(ttl);
                }
                if modern {
                    result["resultType"] = json!("complete");
                }
                Self::ok(id, result)
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                self.calls
                    .lock()
                    .unwrap()
                    .push((name.clone(), arguments.clone()));
                self.call(id, &name, &arguments)
            }
            other => Self::rpc_error(id, -32601, &format!("Method not found: {other}")),
        };
        // Notifications raised while handling this request go out ahead of
        // its response, as a server streaming them would.
        let mut out: Vec<Value> = std::mem::take(&mut *self.pending_notifications.lock().unwrap());
        out.push(response);
        out
    }

    fn call(&self, id: &Value, name: &str, arguments: &Value) -> Value {
        let text = |text: &str| json!({"type": "text", "text": text});
        match name {
            "echo" => Self::ok(
                id,
                json!({"content": [text(&format!("echo: {arguments}"))]}),
            ),
            "write_note" | "delete_note" | "sneaky" | "unannotated" => Self::ok(
                id,
                json!({"content": [text(&format!("{name} done: {arguments}"))]}),
            ),
            "image" => Self::ok(
                id,
                json!({"content": [text("here is the picture"), {"type": "image", "data": PNG_B64, "mimeType": "image/png"}]}),
            ),
            "mixed" => Self::ok(
                id,
                json!({
                    "content": [
                        text("plain text"),
                        {"type": "image", "data": PNG_B64, "mimeType": "image/png"},
                        {"type": "audio", "data": "aGVsbG8gd29ybGQ=", "mimeType": "audio/mpeg"},
                        {"type": "resource_link", "uri": "file:///notes.md", "name": "notes.md", "description": "my notes"},
                        {"type": "resource", "resource": {"uri": "file:///inline.txt", "mimeType": "text/plain", "text": "inline body"}}
                    ],
                    "structuredContent": {"answer": 42},
                    "resultType": "complete"
                }),
            ),
            "fail" => Self::ok(
                id,
                json!({"content": [text("the thing broke")], "isError": true}),
            ),
            "structured" => Self::ok(
                id,
                json!({"content": [], "structuredContent": {"rows": [1, 2, 3]}}),
            ),
            "input" => Self::ok(id, json!({"resultType": "input_required", "content": []})),
            "rpc_error" => Self::rpc_error(id, -32000, &self.rpc_error_message),
            "slow" => {
                // Only the stdio child may block its (single) thread; the
                // HTTP door sleeps asynchronously before replying.
                if std::env::var("NEXUS_MCP_STDIO_MOCK").is_ok() {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
                Self::ok(id, json!({"content": [text("finally")]}))
            }
            "mutate" => {
                if let Some(first) = self.tools.lock().unwrap().first_mut() {
                    first["description"] = json!(format!(
                        "Changed: {}",
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                    ));
                }
                self.pending_notifications
                    .lock()
                    .unwrap()
                    .push(json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}));
                Self::ok(id, json!({"content": [text("mutated")]}))
            }
            other => Self::ok(
                id,
                json!({"content": [text(&format!("no such tool {other}"))], "isError": true}),
            ),
        }
    }
}

/// An in-process HTTP server around a [`Mock`].
pub struct HttpMock {
    pub mock: Arc<Mock>,
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpMock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpMock {
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub async fn start(mock: Mock) -> Self {
        let mock = Arc::new(mock);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shared = Arc::clone(&mock);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let mock = Arc::clone(&shared);
                tokio::spawn(async move { handle(stream, &mock).await });
            }
        });
        Self { mock, addr, task }
    }
}

struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

async fn read_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<(String, HashMap<String, String>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some((request_line, headers, body))
}

async fn handle(mut stream: tokio::net::TcpStream, mock: &Mock) {
    let Some((request_line, headers, body)) = read_request(&mut stream).await else {
        return;
    };
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = json
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or(&request_line)
        .to_string();
    mock.seen.lock().unwrap().push(Seen {
        method: method.clone(),
        headers: headers.clone(),
        body: json.clone(),
    });
    if json["params"]["name"] == "slow" {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    let reply = route(mock, &method, &headers, &json);
    let phrase = match reply.status {
        200 => "OK",
        202 => "Accepted",
        401 => "Unauthorized",
        404 => "Not Found",
        302 => "Found",
        _ => "Status",
    };
    let mut response = format!(
        "HTTP/1.1 {} {phrase}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.content_type,
        reply.body.len()
    );
    for (key, value) in &reply.headers {
        response.push_str(&format!("{key}: {value}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&reply.body);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn route(mock: &Mock, method: &str, headers: &HashMap<String, String>, json: &Value) -> Reply {
    if let Some(token) = &mock.bearer {
        let expected = format!("Bearer {token}");
        if headers.get("authorization") != Some(&expected) {
            let mut reply = Reply::text(401, "unauthorized");
            reply.headers.push((
                "WWW-Authenticate".into(),
                "Bearer resource_metadata=\"http://127.0.0.1/.well-known/oauth-protected-resource\"".into(),
            ));
            return reply;
        }
    }
    if method == "tools/list"
        && let Some(status) = mock.list_status
    {
        return Reply::text(status, "tools temporarily unavailable");
    }
    let modern = matches!(mock.flavor, Flavor::Modern);
    let is_request = json.get("id").is_some_and(|id| !id.is_null());
    if modern {
        // 2026 servers need the version in _meta on every request, and the
        // routing headers.
        let meta_ok = json["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
            == "2026-07-28"
            && json["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"]
                == "nexus-chat"
            && json["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object();
        if is_request && (!meta_ok || headers.get("mcp-method").map(String::as_str) != Some(method))
        {
            return Reply::text(400, "missing _meta or Mcp-Method");
        }
    } else {
        if method == "server/discover" && matches!(mock.flavor, Flavor::Legacy { .. }) {
            return Reply::text(404, "not found");
        }
        if method != "initialize" && method != "server/discover" {
            let session = headers.get("mcp-session-id");
            let current = format!("sess-{}", mock.sessions.load(Ordering::SeqCst));
            if session != Some(&current) {
                return Reply::text(404, "unknown session");
            }
            if let Some(limit) = mock.expire_session_after {
                let seen = mock.after_init.fetch_add(1, Ordering::SeqCst) + 1;
                if seen > limit {
                    return Reply::text(404, "session expired");
                }
            }
        }
    }
    if !is_request {
        return Reply::text(202, "");
    }
    let mut messages = mock.respond(json);
    let mut reply = Reply {
        status: 200,
        content_type: "application/json",
        headers: Vec::new(),
        body: String::new(),
    };
    if method == "initialize" && !modern {
        let n = mock.sessions.fetch_add(1, Ordering::SeqCst) + 1;
        reply
            .headers
            .push(("Mcp-Session-Id".into(), format!("sess-{n}")));
    }
    let as_sse = mock.sse_methods.lock().unwrap().contains(method) || messages.len() > 1;
    if as_sse {
        reply.content_type = "text/event-stream";
        reply.body = messages
            .drain(..)
            .map(|m| format!("event: message\ndata: {m}\n\n"))
            .collect();
    } else {
        reply.body = messages.remove(0).to_string();
    }
    reply
}

/// The stdio door: run by the test executable when re-launched with
/// `NEXUS_MCP_STDIO_MOCK` set (see `tests::stdio_mock_server`).
///
/// Mode words, comma separated: `modern` | `legacy` | `legacy-rpc`;
/// `crash_after=N` exits after N answered requests; `ttl=MS`; `cancel_log`
/// appends cancellations to the file named by `NEXUS_MCP_STDIO_LOG`.
pub fn run_stdio(mode: &str) {
    use std::io::{BufRead as _, Write as _};
    let words: Vec<&str> = mode.split(',').collect();
    let flavor = if words.contains(&"modern") {
        Flavor::Modern
    } else if words.contains(&"legacy-rpc") {
        Flavor::LegacyRpcError {
            version: "2025-06-18",
        }
    } else {
        Flavor::Legacy {
            version: "2025-11-25",
        }
    };
    let option = |key: &str| {
        words
            .iter()
            .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let mut mock = Mock::new(flavor);
    mock.ttl_ms = option("ttl");
    if let Ok(token) = std::env::var("FOO") {
        mock.rpc_error_message = format!("backend echoed {token}");
    }
    if let Ok(path) = std::env::var("NEXUS_MCP_STDIO_START_FILE") {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let _ = writeln!(
            file,
            "start FOO={} OPENROUTER={}",
            std::env::var("FOO").unwrap_or_default(),
            std::env::var("OPENROUTER_API_KEY").is_ok()
        );
    }
    if let Ok(path) = std::env::var("NEXUS_MCP_STDIO_PID_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    eprintln!(
        "mock server starting with token {}",
        std::env::var("FOO").unwrap_or_default()
    );
    let crash_after = option("crash_after");
    let mut answered = 0_u64;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    // A banner a launcher might print: not JSON-RPC, must be ignored.
    let _ = writeln!(out, "mock banner (not json)");
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["method"] == "notifications/cancelled"
            && let Ok(path) = std::env::var("NEXUS_MCP_STDIO_LOG")
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            let _ = writeln!(file, "cancelled {}", message["params"]["requestId"]);
        }
        for reply in mock.respond(&message) {
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
        if message.get("id").is_some() {
            answered += 1;
            if crash_after.is_some_and(|limit| answered >= limit) {
                std::process::exit(3);
            }
        }
    }
}
