//! The raw `tokio` HTTP/SSE server for `nexus host`.
//!
//! This deliberately stays framework-free. The daemon is local-first, the
//! route set is small, and using one parser makes the authentication and body
//! limits easy to audit. The app itself lives in [`AppActor`]; connections
//! never hold a lock across `App::next_event().await`.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::app::{App, AppCommand, AppEvent, CoreSnapshot};
use crate::provider::BackendTag;
use crate::provider::openrouter;
use crate::sync::Changeset;
use crate::tools::ToolExecutor;

use super::wire::{WireBackendTag, WireEvent, WireModel};

const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_API_BODY_BYTES: usize = 10 * 1024 * 1024;
const MAX_GATEWAY_BODY_BYTES: usize = 64 * 1024 * 1024;
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);

/// Configuration for a local host listener.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// TCP port on loopback. `0` asks the OS for an ephemeral port (useful in
    /// tests); the CLI defaults this to `8643`.
    pub port: u16,
    /// Bearer token required by `/v1/*` and the `/apps/*` proxy.
    pub token: String,
    /// Optional upstream override used by hermetic gateway tests. Production
    /// callers leave this `None`, selecting the provider's canonical base.
    gateway_base: Option<String>,
}

impl HostConfig {
    /// Construct a listener configuration.
    pub fn new(port: u16, token: impl Into<String>) -> Self {
        Self {
            port,
            token: token.into(),
            gateway_base: None,
        }
    }

    /// Route all gateway flavors to a local mock upstream. Intended for
    /// network-free integration tests, not normal hosting.
    #[must_use]
    pub fn with_gateway_base(mut self, base: impl Into<String>) -> Self {
        self.gateway_base = Some(base.into());
        self
    }
}

/// A backend route selected for one gateway request. The API handler keeps
/// this object private so an upstream key can never be serialized or returned
/// in an error response.
#[derive(Clone)]
struct GatewayRoute {
    tag: BackendTag,
    model: String,
    key: String,
    account_id: Option<String>,
}

/// A usage fragment observed while forwarding one provider response.
#[derive(Debug, Default)]
struct GatewayUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cost: Option<f64>,
}

impl GatewayUsage {
    fn observe(&mut self, bytes: &[u8], streaming: bool) {
        if streaming {
            let text = String::from_utf8_lossy(bytes);
            for line in text.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data != "[DONE]" {
                    self.observe_json(data);
                }
            }
        } else if let Ok(text) = std::str::from_utf8(bytes) {
            self.observe_json(text);
        }
    }

    fn observe_json(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(usage) = value.get("usage").and_then(serde_json::Value::as_object) else {
            return;
        };
        let number = |key: &str| {
            usage
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let nested = |group: &str, key: &str| {
            usage
                .get(group)
                .and_then(|value| value.get(key))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        self.prompt_tokens = self.prompt_tokens.max(number("prompt_tokens"));
        self.completion_tokens = self.completion_tokens.max(number("completion_tokens"));
        self.cache_read_tokens = self.cache_read_tokens.max(
            nested("prompt_tokens_details", "cached_tokens")
                .max(nested("input_tokens_details", "cached_tokens"))
                .max(number("cache_read_input_tokens")),
        );
        self.cache_creation_tokens = self.cache_creation_tokens.max(
            nested("prompt_tokens_details", "cache_write_tokens")
                .max(nested("input_tokens_details", "cache_write_tokens"))
                .max(number("cache_creation_input_tokens")),
        );
        self.cost = usage.get("cost").and_then(json_f64).or(self.cost);
    }
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .filter(|number| number.is_finite())
}

/// A running host server. It owns the listener and app actor; dropping it
/// aborts both background tasks, while [`Self::shutdown`] gives them a clean
/// cancellation opportunity first.
pub struct HostServer {
    state: Arc<HostState>,
    shutdown: Arc<Notify>,
    accept_task: Option<JoinHandle<()>>,
    actor_task: Option<JoinHandle<()>>,
    addr: SocketAddr,
}

#[derive(Clone)]
struct HostState {
    requests: mpsc::Sender<HostRequest>,
    events: broadcast::Sender<WireEvent>,
    token: Arc<String>,
    app_server_port: Option<u16>,
    client: reqwest::Client,
    gateway_base: Option<String>,
}

impl HostServer {
    /// Bind a loopback host listener and start the app actor.
    pub async fn bind(app: App, config: HostConfig) -> Result<Self> {
        if config.token.trim().is_empty() {
            bail!("host token must not be empty");
        }
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .with_context(|| format!("binding host listener on 127.0.0.1:{}", config.port))?;
        let addr = listener
            .local_addr()
            .context("reading host listener address")?;
        let app_server_port = app
            .app_server
            .as_ref()
            .map(crate::appserver::AppServer::port);
        let (request_tx, request_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let shutdown = Arc::new(Notify::new());
        let state = Arc::new(HostState {
            requests: request_tx,
            events: events.clone(),
            token: Arc::new(config.token),
            app_server_port,
            client: reqwest::Client::new(),
            gateway_base: config.gateway_base,
        });
        let actor_events = events;
        let actor_shutdown = shutdown.clone();
        let actor_task = tokio::spawn(async move {
            app_actor(app, request_rx, actor_events, actor_shutdown).await;
        });
        let accept_state = state.clone();
        let accept_shutdown = shutdown.clone();
        let accept_task = tokio::spawn(async move {
            accept_loop(listener, accept_state, accept_shutdown).await;
        });
        Ok(Self {
            state,
            shutdown,
            accept_task: Some(accept_task),
            actor_task: Some(actor_task),
            addr,
        })
    }

    /// The loopback address selected by the OS.
    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Subscribe to the global app event stream. Each subscriber receives
    /// events emitted after it subscribes; slow subscribers may be told they
    /// lagged and should refresh from `/v1/snapshot`.
    pub fn subscribe(&self) -> broadcast::Receiver<WireEvent> {
        self.state.events.subscribe()
    }

    /// Set or clear the public app-link base used by the app actor.
    pub async fn set_public_base(&self, base: Option<String>) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.state
            .requests
            .send(HostRequest::SetPublicBase { base, reply })
            .await
            .map_err(|_| anyhow!("host actor stopped"))?;
        let result = rx.await.map_err(|_| anyhow!("host actor stopped"))?;
        result.map_err(|error| anyhow!(error))?;
        Ok(())
    }

    /// Stop accepting connections and abort the app actor.
    pub async fn shutdown(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(task) = self.accept_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.actor_task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        if let Some(task) = self.actor_task.take() {
            task.abort();
        }
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<HostState>, shutdown: Arc<Notify>) {
    loop {
        tokio::select! {
            () = shutdown.notified() => break,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue; };
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, state).await;
                });
            }
        }
    }
}

#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct ReadError {
    status: u16,
    message: &'static str,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, ReadError> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            return Err(ReadError {
                status: 431,
                message: "request headers too large",
            });
        }
        let n = stream.read(&mut chunk).await.map_err(|_| ReadError {
            status: 400,
            message: "could not read request",
        })?;
        if n == 0 {
            return Err(ReadError {
                status: 400,
                message: "incomplete request",
            });
        }
        buffer.extend_from_slice(&chunk[..n]);
    };
    let header_text = std::str::from_utf8(&buffer[..header_end]).map_err(|_| ReadError {
        status: 400,
        message: "request headers are not UTF-8",
    })?;
    let mut lines = header_text.lines();
    let request_line = lines.next().ok_or(ReadError {
        status: 400,
        message: "missing request line",
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || target.is_empty() {
        return Err(ReadError {
            status: 400,
            message: "bad request line",
        });
    }
    let mut headers = HashMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(ReadError {
                status: 400,
                message: "bad request header",
            });
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| {
            value.parse::<usize>().map_err(|_| ReadError {
                status: 400,
                message: "invalid content length",
            })
        })
        .transpose()?
        .unwrap_or(0);
    if content_length > MAX_GATEWAY_BODY_BYTES {
        return Err(ReadError {
            status: 413,
            message: "request entity too large",
        });
    }
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err(ReadError {
            status: 501,
            message: "chunked request bodies are not supported",
        });
    }
    let body_start = header_end + 4;
    let already = buffer.len().saturating_sub(body_start).min(content_length);
    let mut body = buffer[body_start..body_start + already].to_vec();
    if already < content_length {
        body.resize(content_length, 0);
        stream
            .read_exact(&mut body[already..])
            .await
            .map_err(|_| ReadError {
                status: 400,
                message: "incomplete request body",
            })?;
    }
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

async fn handle_connection(mut stream: TcpStream, state: Arc<HostState>) -> io::Result<()> {
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(error) => return respond_text(&mut stream, error.status, error.message).await,
    };
    let (path, query) = split_target(&request.target);
    if request.method.eq_ignore_ascii_case("OPTIONS") {
        return respond_empty(&mut stream, 204).await;
    }
    let is_apps = path == "/apps" || path.starts_with("/apps/");
    let is_v1 = path == "/v1" || path.starts_with("/v1/");
    if (is_apps || is_v1) && !authorized(&request, query, &state.token, is_apps) {
        return respond_json(
            &mut stream,
            401,
            &serde_json::json!({
                "error": { "message": "unauthorized", "type": "authentication_error" }
            }),
        )
        .await;
    }

    if path == "/v1/events" {
        return handle_sse(&mut stream, &request, &state).await;
    }
    if path == "/v1/chat/completions" {
        return handle_gateway(&mut stream, &request, &state).await;
    }
    if is_apps {
        return proxy_app(&mut stream, &request, path, query, &state).await;
    }
    handle_api_route(&mut stream, &request, path, &state).await
}

fn authorized(request: &Request, query: &str, token: &str, apps: bool) -> bool {
    if let Some(value) = request.headers.get("authorization")
        && let Some((scheme, supplied)) = value.split_once(char::is_whitespace)
        && scheme.eq_ignore_ascii_case("bearer")
        && constant_time_eq(supplied.trim().as_bytes(), token.as_bytes())
    {
        return true;
    }
    apps && query_param(query, "token")
        .is_some_and(|supplied| constant_time_eq(supplied.as_bytes(), token.as_bytes()))
}

/// Constant-time equality for the host token. The length difference is folded
/// into the accumulator instead of returning early.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

#[allow(clippy::too_many_lines)]
async fn handle_api_route(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    state: &HostState,
) -> io::Result<()> {
    match (request.method.as_str(), path) {
        ("GET", "/v1/snapshot") => {
            let snapshot =
                ask_actor(&state.requests, |reply| HostRequest::Snapshot { reply }).await;
            match snapshot {
                Ok(snapshot) => respond_json(stream, 200, &snapshot).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/models") => {
            let models = ask_actor(&state.requests, |reply| HostRequest::Models { reply }).await;
            match models {
                Ok(models) => {
                    let response = ModelsResponse {
                        object: "list",
                        data: models.clone(),
                        models,
                    };
                    respond_json(stream, 200, &response).await
                }
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/backends") => {
            let backends =
                ask_actor(&state.requests, |reply| HostRequest::Backends { reply }).await;
            match backends {
                Ok(backends) => respond_json(stream, 200, &BackendsResponse { backends }).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("POST", "/v1/command") => {
            if request.body.len() > MAX_API_BODY_BYTES {
                return respond_text(stream, 413, "request entity too large").await;
            }
            let command = match serde_json::from_slice::<AppCommand>(&request.body) {
                Ok(command) => command,
                Err(error) => {
                    return respond_error(stream, 400, &format!("invalid command: {error}")).await;
                }
            };
            let result = ask_actor(&state.requests, |reply| HostRequest::Command {
                command,
                reply,
            })
            .await;
            match result {
                Ok(()) => respond_json(stream, 202, &serde_json::json!({ "ok": true })).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("POST", "/v1/sync") => {
            if request.body.len() > MAX_API_BODY_BYTES {
                return respond_text(stream, 413, "request entity too large").await;
            }
            let changeset = match serde_json::from_slice::<Changeset>(&request.body) {
                Ok(changeset) => changeset,
                Err(error) => {
                    return respond_error(stream, 400, &format!("invalid changeset: {error}"))
                        .await;
                }
            };
            let result = ask_actor(&state.requests, |reply| HostRequest::Sync {
                changeset,
                reply,
            })
            .await;
            match result {
                Ok(reply) => respond_json(stream, 200, &reply).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("GET", "/v1/tools") => {
            let defs = ask_actor(&state.requests, |reply| HostRequest::ToolDefs { reply }).await;
            match defs {
                Ok(defs) => respond_json(stream, 200, &serde_json::json!({ "tools": defs })).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("POST", "/v1/tools/run") => {
            if request.body.len() > MAX_API_BODY_BYTES {
                return respond_text(stream, 413, "request entity too large").await;
            }
            let input = match serde_json::from_slice::<ToolRunRequest>(&request.body) {
                Ok(input) => input,
                Err(error) => {
                    return respond_error(stream, 400, &format!("invalid tool request: {error}"))
                        .await;
                }
            };
            let toolbox =
                match ask_actor(&state.requests, |reply| HostRequest::Toolbox { reply }).await {
                    Ok(toolbox) => toolbox,
                    Err(error) => return respond_error(stream, 500, &error).await,
                };
            let args = match input.args {
                serde_json::Value::String(args) => args,
                value => value.to_string(),
            };
            let (result, label) = toolbox.run(&input.name, &args).await;
            respond_json(stream, 200, &ToolRunResponse { result, label }).await
        }
        ("GET", "/") => respond_text(stream, 200, "nexus host\n").await,
        _ => respond_text(stream, 404, "not found").await,
    }
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    object: &'static str,
    data: Vec<WireModel>,
    models: Vec<WireModel>,
}

#[derive(Debug, Serialize)]
struct BackendsResponse {
    backends: Vec<BackendInfo>,
}

#[derive(Debug, Serialize)]
struct BackendInfo {
    tag: WireBackendTag,
    name: &'static str,
    configured: bool,
    default_model: String,
    model_count: usize,
}

#[derive(Debug, Deserialize)]
struct ToolRunRequest {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ToolRunResponse {
    result: String,
    label: String,
}

async fn handle_sse(
    stream: &mut TcpStream,
    request: &Request,
    state: &HostState,
) -> io::Result<()> {
    if request.method != "GET" {
        return respond_text(stream, 405, "method not allowed").await;
    }
    let mut receiver = state.events.subscribe();
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\n\r\n",
        )
        .await?;
    stream.flush().await?;
    let mut heartbeat = tokio::time::interval(SSE_HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(event) => {
                    let Ok(json) = serde_json::to_string(&event) else {
                        continue;
                    };
                    if stream.write_all(format!("data: {json}\n\n").as_bytes()).await.is_err() {
                        return Ok(());
                    }
                    if stream.flush().await.is_err() {
                        return Ok(());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if stream.write_all(b": lagged; refresh /v1/snapshot\n\n").await.is_err() {
                        return Ok(());
                    }
                    let _ = stream.flush().await;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = heartbeat.tick() => {
                if stream.write_all(b": heartbeat\n\n").await.is_err() {
                    return Ok(());
                }
                if stream.flush().await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_gateway(
    stream: &mut TcpStream,
    request: &Request,
    state: &HostState,
) -> io::Result<()> {
    if request.method != "POST" {
        return respond_text(stream, 405, "method not allowed").await;
    }
    if request.body.len() > MAX_GATEWAY_BODY_BYTES {
        return respond_text(stream, 413, "request entity too large").await;
    }
    let body: serde_json::Value = match serde_json::from_slice(&request.body) {
        Ok(body) => body,
        Err(error) => {
            return respond_error(stream, 400, &format!("invalid completion request: {error}"))
                .await;
        }
    };
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if model.trim().is_empty() {
        return respond_error(stream, 400, "completion request is missing model").await;
    }
    let override_tag = request
        .headers
        .get("x-nexus-backend")
        .map(|value| parse_backend_tag(value))
        .transpose();
    let override_tag = match override_tag {
        Ok(tag) => tag,
        Err(error) => return respond_error(stream, 400, &error).await,
    };
    let route = ask_actor(&state.requests, |reply| HostRequest::GatewayRoute {
        model: model.to_string(),
        override_tag,
        reply,
    })
    .await;
    let route = match route {
        Ok(route) => route,
        Err(error) => {
            let models = ask_actor(&state.requests, |reply| HostRequest::Models { reply })
                .await
                .unwrap_or_default();
            return respond_json(
                stream,
                400,
                &serde_json::json!({
                    "error": { "message": error, "type": "invalid_request_error" },
                    "models": models,
                }),
            )
            .await;
        }
    };
    let (default_base, _raw_model) = openrouter::gateway_route(route.tag, &route.model);
    let base = state.gateway_base.as_deref().unwrap_or(default_base);
    let url = format!("{base}/chat/completions");
    let mut builder = state
        .client
        .post(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", route.key),
        )
        .body(request.body.clone());
    for (name, value) in &request.headers {
        if matches!(
            name.as_str(),
            "host" | "authorization" | "content-length" | "transfer-encoding"
        ) || name.starts_with("x-nexus-")
        {
            continue;
        }
        let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) else {
            continue;
        };
        builder = builder.header(header_name, header_value);
    }
    if route.tag == BackendTag::Codex {
        if let Some(account_id) = &route.account_id {
            builder = builder.header("chatgpt-account-id", account_id);
        }
        builder = builder
            .header("originator", "nexus-host")
            .header("OpenAI-Beta", "responses=experimental");
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            return respond_error(stream, 502, &format!("upstream request failed: {error}")).await;
        }
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let streaming = content_type
        .to_ascii_lowercase()
        .contains("text/event-stream")
        || body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    let mut upstream = response.bytes_stream();
    let mut usage = GatewayUsage::default();
    let reason = reason(status);
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    while let Some(chunk) = upstream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        usage.observe(&chunk, streaming);
        if stream.write_all(&chunk).await.is_err() {
            return Ok(());
        }
        if stream.flush().await.is_err() {
            return Ok(());
        }
    }
    let (reply, result) = oneshot::channel();
    let _ = state
        .requests
        .send(HostRequest::LogGatewayUsage {
            route,
            usage,
            reply,
        })
        .await;
    let _ = result.await;
    stream.shutdown().await
}

async fn proxy_app(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    query: &str,
    state: &HostState,
) -> io::Result<()> {
    if !matches!(
        request.method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "OPTIONS"
    ) {
        return respond_text(stream, 405, "method not allowed").await;
    }
    let Some(port) = state.app_server_port else {
        return respond_text(stream, 503, "app server unavailable").await;
    };
    let local_path = path.strip_prefix("/apps").unwrap_or("/");
    let query = query_without_token(query);
    let target = if query.is_empty() {
        format!("http://127.0.0.1:{port}{local_path}")
    } else {
        format!("http://127.0.0.1:{port}{local_path}?{query}")
    };
    let Ok(method) = reqwest::Method::from_bytes(request.method.as_bytes()) else {
        return respond_text(stream, 400, "invalid method").await;
    };
    let mut builder = state
        .client
        .request(method, target)
        .body(request.body.clone());
    for (name, value) in &request.headers {
        if matches!(
            name.as_str(),
            "host" | "authorization" | "content-length" | "transfer-encoding"
        ) || name == "x-nexus-token"
        {
            continue;
        }
        let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) else {
            continue;
        };
        builder = builder.header(header_name, header_value);
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            return respond_error(stream, 502, &format!("app proxy failed: {error}")).await;
        }
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            return respond_error(stream, 502, &format!("app response failed: {error}")).await;
        }
    };
    respond_with_content_type(
        stream,
        status,
        &content_type,
        &body,
        request.method == "HEAD",
    )
    .await
}

fn query_without_token(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| {
            let pair = *pair;
            let name = pair.split_once('=').map_or(pair, |(name, _)| name);
            !pair.is_empty() && !name.eq_ignore_ascii_case("token")
        })
        .collect::<Vec<_>>()
        .join("&")
}

async fn app_actor(
    mut app: App,
    mut requests: mpsc::Receiver<HostRequest>,
    events: broadcast::Sender<WireEvent>,
    shutdown: Arc<Notify>,
) {
    loop {
        tokio::select! {
            () = shutdown.notified() => break,
            request = requests.recv() => {
                let Some(request) = request else { break; };
                handle_actor_request(&mut app, request);
            }
            event = app.next_event() => {
                let wire = WireEvent::from(event.clone());
                apply_domain_event(&mut app, &event);
                // The handler may clear a one-shot receiver itself; this
                // backstop also prevents a closed source from returning
                // `None` forever when a remote client is the only consumer.
                clear_closed_source(&mut app, &event);
                let _ = events.send(wire);
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn handle_actor_request(app: &mut App, request: HostRequest) {
    match request {
        HostRequest::Snapshot { reply } => {
            let _ = reply.send(app.snapshot().map_err(|error| error.to_string()));
        }
        HostRequest::Models { reply } => {
            let models = app.models.iter().cloned().map(WireModel::from).collect();
            let _ = reply.send(Ok(models));
        }
        HostRequest::Backends { reply } => {
            let tags = [
                BackendTag::OpenRouter,
                BackendTag::OpenAi,
                BackendTag::OpencodeGo,
                BackendTag::Codex,
            ];
            let backends = tags
                .into_iter()
                .map(|tag| {
                    let provider = app.backends.get(tag);
                    BackendInfo {
                        tag: tag.into(),
                        name: tag.display_name(),
                        configured: app.backends.configured(tag),
                        default_model: provider
                            .map(|provider| provider.default_utility_model().to_string())
                            .unwrap_or_default(),
                        model_count: app
                            .models
                            .iter()
                            .filter(|model| model.backend == tag)
                            .count(),
                    }
                })
                .collect();
            let _ = reply.send(Ok(backends));
        }
        HostRequest::Command { command, reply } => {
            let result = app.execute(command).map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
        HostRequest::ToolDefs { reply } => {
            let _ = reply.send(Ok(app.toolbox.defs()));
        }
        HostRequest::Toolbox { reply } => {
            let _ = reply.send(Ok(app.toolbox.clone()));
        }
        HostRequest::Sync { changeset, reply } => {
            let result = (|| -> Result<Changeset, String> {
                let (_summary, cursors) =
                    crate::sync::apply_changeset(&app.db, &app.space, &changeset, None)
                        .map_err(|error| error.to_string())?;
                app.sessions_cache.clear();
                app.rescan_files();
                let mut reply_changeset = crate::sync::build_changeset(
                    &app.db,
                    Some(&changeset.device_id),
                    &crate::sync::device_name(),
                )
                .map_err(|error| error.to_string())?;
                reply_changeset.ack = Some(cursors);
                Ok(reply_changeset)
            })();
            let _ = reply.send(result);
        }
        HostRequest::SetPublicBase { base, reply } => {
            let result = if let Some(server) = app.app_server.as_mut() {
                server.set_public_base(base);
                app.refresh_toolbox();
                Ok(())
            } else {
                Err("app server is unavailable".to_string())
            };
            let _ = reply.send(result);
        }
        HostRequest::GatewayRoute {
            model,
            override_tag,
            reply,
        } => {
            let result = gateway_route(app, &model, override_tag);
            let _ = reply.send(result);
        }
        HostRequest::LogGatewayUsage {
            route,
            usage,
            reply,
        } => {
            let result = app
                .db
                .log_usage(
                    route.tag.name(),
                    &route.model,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.cache_read_tokens,
                    usage.cache_creation_tokens,
                    usage.cost,
                    usage.cost.is_some(),
                    None,
                    Some(&app.active_space.id),
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
    }
}

fn apply_domain_event(app: &mut App, event: &AppEvent) {
    match event.clone() {
        AppEvent::Stream(Some((task_id, event))) => {
            if let Err(error) = app.on_chat_event(task_id, event) {
                app.push_status(error.to_string());
            }
        }
        AppEvent::Models(result) => app.on_models_result(result),
        AppEvent::Title(result) => app.on_title_result(result),
        AppEvent::Memory(result) => app.on_memory_result(result),
        AppEvent::Compact(result) => app.on_compact_result(result),
        AppEvent::SkillInstall(result) => app.on_skill_install_result(result),
        AppEvent::Ocr(result) => app.on_ocr_done(result),
        AppEvent::Embed(result) => app.on_embed_done(result),
        AppEvent::OcrPull(result) => app.on_ocr_pull(result),
        AppEvent::Research(result) => app.on_research_done(result),
        AppEvent::ResearchTopic(result) => app.on_research_topic_derived(result),
        AppEvent::Login(result) => app.on_login_result(result),
        AppEvent::UpdateCheck(result) => app.on_update_check(result),
        AppEvent::Swarm(result) => app.on_swarm_update(result),
        AppEvent::Status(_)
        | AppEvent::ComposerSet(_)
        | AppEvent::ComposerClear
        | AppEvent::ViewportReset
        | AppEvent::HistoryInvalidated
        | AppEvent::OpenLoginPopup
        | AppEvent::Gate(_)
        | AppEvent::Stream(None) => {}
    }
}

fn clear_closed_source(app: &mut App, event: &AppEvent) {
    match event {
        AppEvent::Models(None) => app.models_rx = None,
        AppEvent::Title(None) => app.title_rx = None,
        AppEvent::Memory(None) => app.memory_rx = None,
        AppEvent::Compact(None) => app.compact_rx = None,
        AppEvent::SkillInstall(None) => app.skills_rx = None,
        AppEvent::Ocr(None) => app.ocr_rx = None,
        AppEvent::Embed(None) => app.embed_rx = None,
        AppEvent::OcrPull(None) => app.ocr_pull_rx = None,
        AppEvent::Research(None) => app.research_rx = None,
        AppEvent::ResearchTopic(None) => app.research_topic_rx = None,
        AppEvent::Login(None) => app.login_rx = None,
        AppEvent::Swarm(None) => app.swarm_rx = None,
        AppEvent::UpdateCheck(None) => app.update_rx = None,
        _ => {}
    }
}

fn gateway_route(
    app: &App,
    model: &str,
    override_tag: Option<BackendTag>,
) -> Result<GatewayRoute, String> {
    let selected = if let Some(tag) = override_tag {
        if !app.backends.configured(tag) {
            return Err(format!("backend {} is not configured", tag.display_name()));
        }
        Some((tag, model.to_string()))
    } else {
        app.models.iter().find_map(|entry| {
            let composite = crate::app::composite_id(entry);
            (entry.id == model || composite == model).then_some((entry.backend, entry.id.clone()))
        })
    };
    let Some((tag, raw_model)) = selected else {
        return Err(format!("unknown model {model:?}"));
    };
    let key = match tag {
        BackendTag::OpenRouter => app.saved.openrouter_key.clone(),
        BackendTag::OpenAi => app.saved.openai_key.clone(),
        BackendTag::OpencodeGo => app.saved.opencode_key.clone(),
        BackendTag::Codex => app
            .saved
            .codex
            .as_ref()
            .map(|credentials| credentials.access.clone()),
    }
    .ok_or_else(|| format!("backend {} is not configured", tag.display_name()))?;
    let account_id = app
        .saved
        .codex
        .as_ref()
        .map(|credentials| credentials.account_id.clone());
    Ok(GatewayRoute {
        tag,
        model: raw_model,
        key,
        account_id,
    })
}

enum HostRequest {
    Snapshot {
        reply: oneshot::Sender<Result<CoreSnapshot, String>>,
    },
    Models {
        reply: oneshot::Sender<Result<Vec<WireModel>, String>>,
    },
    Backends {
        reply: oneshot::Sender<Result<Vec<BackendInfo>, String>>,
    },
    Command {
        command: AppCommand,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ToolDefs {
        reply: oneshot::Sender<Result<Vec<crate::provider::ToolDef>, String>>,
    },
    Toolbox {
        reply: oneshot::Sender<Result<Arc<dyn ToolExecutor>, String>>,
    },
    Sync {
        changeset: Changeset,
        reply: oneshot::Sender<Result<Changeset, String>>,
    },
    SetPublicBase {
        base: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GatewayRoute {
        model: String,
        override_tag: Option<BackendTag>,
        reply: oneshot::Sender<Result<GatewayRoute, String>>,
    },
    LogGatewayUsage {
        route: GatewayRoute,
        usage: GatewayUsage,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

async fn ask_actor<T, F>(requests: &mpsc::Sender<HostRequest>, make: F) -> Result<T, String>
where
    F: FnOnce(oneshot::Sender<Result<T, String>>) -> HostRequest,
{
    let (reply, receiver) = oneshot::channel();
    requests
        .send(make(reply))
        .await
        .map_err(|_| "host actor stopped".to_string())?;
    receiver
        .await
        .map_err(|_| "host actor stopped".to_string())?
}

fn parse_backend_tag(value: &str) -> Result<BackendTag, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openrouter" | "router" => Ok(BackendTag::OpenRouter),
        "openai" => Ok(BackendTag::OpenAi),
        "opencode" | "opencode-go" | "opencode_go" | "go" => Ok(BackendTag::OpencodeGo),
        "codex" => Ok(BackendTag::Codex),
        _ => Err(format!("unknown x-nexus-backend {value:?}")),
    }
}

fn split_target(target: &str) -> (&str, &str) {
    target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query))
}

fn query_param(query: &str, wanted: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(name).eq_ignore_ascii_case(wanted)).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(if bytes[index] == b'+' {
                b' '
            } else {
                bytes[index]
            });
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

async fn respond_text(stream: &mut TcpStream, status: u16, text: &str) -> io::Result<()> {
    respond_with_content_type(
        stream,
        status,
        "text/plain; charset=utf-8",
        text.as_bytes(),
        false,
    )
    .await
}

async fn respond_error(stream: &mut TcpStream, status: u16, message: &str) -> io::Result<()> {
    respond_json(
        stream,
        status,
        &serde_json::json!({
            "error": { "message": message, "type": "invalid_request_error" }
        }),
    )
    .await
}

async fn respond_json<T: Serialize>(
    stream: &mut TcpStream,
    status: u16,
    value: &T,
) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .unwrap_or_else(|_| b"{\"error\":{\"message\":\"serialization failed\"}}".to_vec());
    respond_with_content_type(
        stream,
        status,
        "application/json; charset=utf-8",
        &body,
        false,
    )
    .await
}

async fn respond_empty(stream: &mut TcpStream, status: u16) -> io::Result<()> {
    respond_with_content_type(stream, status, "text/plain", &[], false).await
}

async fn respond_with_content_type(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\n\r\n",
        reason(status),
        body.len(),
    );
    stream.write_all(header.as_bytes()).await?;
    if !head {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Request Entity Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare_checks_length_and_bytes() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"wrong"));
        assert!(!constant_time_eq(b"token", b"token-long"));
    }

    #[test]
    fn backend_override_accepts_supported_spellings() {
        assert_eq!(parse_backend_tag("openrouter"), Ok(BackendTag::OpenRouter));
        assert_eq!(parse_backend_tag("OpenCode-Go"), Ok(BackendTag::OpencodeGo));
        assert!(parse_backend_tag("wat").is_err());
    }

    #[test]
    fn query_token_is_removed_before_app_proxy() {
        assert_eq!(query_without_token("token=a&x=1&token=b"), "x=1");
    }

    #[test]
    fn usage_observer_reads_openai_and_cache_fields() {
        let mut usage = GatewayUsage::default();
        usage.observe(
            br#"{"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3},"cost":"0.01"}}"#,
            false,
        );
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 4);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cost, Some(0.01));
    }
}
