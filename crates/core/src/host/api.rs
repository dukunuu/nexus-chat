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
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::app::{App, AppCommand, AppEvent, CoreSnapshot};
use crate::appserver::AppRegistry;
use crate::db::UsageRange;
use crate::provider::openrouter;
use crate::provider::{BackendTag, Usage};
use crate::sync::Changeset;
use crate::tools::ToolExecutor;

use super::wire::{WireBackendTag, WireEvent, WireModel, public_model_id};

const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_API_BODY_BYTES: usize = 10 * 1024 * 1024;
const MAX_GATEWAY_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// How much of a request body is read (and reserved) at a time.
const BODY_CHUNK_BYTES: usize = 64 * 1024;
/// Ceiling on the bytes held back while looking for a provider usage frame.
const MAX_USAGE_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(15);
const ACTOR_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_HEADERS_TIMEOUT: Duration = Duration::from_secs(20);
const UPSTREAM_CHUNK_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_APP_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 128;
/// CORS block shared by every response, including the `OPTIONS` preflight.
/// `Access-Control-Allow-Methods` is required: without it a browser rejects
/// the preflight for `POST /v1/command` and `POST /v1/chat/completions`
/// (JSON bodies are never "simple" requests).
const CORS_HEADERS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS\r\nAccess-Control-Allow-Headers: Authorization, Content-Type, X-Nexus-Backend, X-Nexus-Blob-Hash, X-Content-SHA256\r\nAccess-Control-Max-Age: 600\r\n";

/// Configuration for a local host listener.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// TCP port on loopback. `0` asks the OS for an ephemeral port (useful in
    /// tests); the CLI defaults this to `8643`.
    pub port: u16,
    /// Bearer token required by `/v1/*`; registered public app UUID routes do
    /// not embed or issue this token.
    pub token: String,
    /// Optional upstream override used by hermetic gateway tests. Production
    /// callers leave this `None`, selecting the provider's canonical base.
    gateway_base: Option<String>,
    web_dir: Option<std::path::PathBuf>,
}

impl HostConfig {
    /// Construct a listener configuration.
    pub fn new(port: u16, token: impl Into<String>) -> Self {
        Self {
            port,
            token: token.into(),
            gateway_base: None,
            web_dir: None,
        }
    }

    /// Serve a built web client from this directory at the host origin.
    #[must_use]
    pub fn with_web_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.web_dir = Some(path.into());
        self
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

/// Usage observed while forwarding one provider response.
///
/// This owns only the gateway's own problem — reassembling `SSE` frames from
/// arbitrary socket chunks — and hands the JSON to the same parser the
/// in-process client uses, so a gateway request and a direct request account
/// for the identical response identically.
#[derive(Debug, Default)]
struct GatewayUsage {
    usage: Option<Usage>,
    /// Bytes not yet forming a complete `SSE` line (or the whole body of a
    /// non-streaming response). Chunk boundaries fall wherever the socket
    /// puts them, so parsing each chunk in isolation would drop the usage
    /// frame whenever it straddles two of them.
    buffer: Vec<u8>,
}

impl GatewayUsage {
    fn observe(&mut self, bytes: &[u8], streaming: bool) {
        if self.buffer.len() + bytes.len() > MAX_USAGE_BUFFER_BYTES {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        if !streaming {
            // One JSON document; it can only be parsed once it is complete.
            return;
        }
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=position).collect();
            self.observe_line(&String::from_utf8_lossy(&line));
        }
    }

    /// Parse whatever is left once the upstream body ends.
    fn finish(&mut self, streaming: bool) {
        let rest = std::mem::take(&mut self.buffer);
        if rest.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&rest).into_owned();
        if streaming {
            self.observe_line(&text);
        } else {
            self.observe_json(text.trim());
        }
    }

    fn observe_line(&mut self, line: &str) {
        let Some(data) = line.trim_end().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data != "[DONE]" {
            self.observe_json(data);
        }
    }

    fn observe_json(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(usage) = value.get("usage") else {
            return;
        };
        if let Some(next) = openrouter::parse_usage_value(usage) {
            openrouter::merge_usage(&mut self.usage, next);
        }
    }

    /// The accounting to log, or an all-zero record when the upstream never
    /// reported any.
    fn into_usage(self) -> Usage {
        self.usage.unwrap_or_default()
    }
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
    app_registry: Option<AppRegistry>,
    client: reqwest::Client,
    connections: Arc<tokio::sync::Semaphore>,
    gateway_base: Option<String>,
    web_dir: Option<std::path::PathBuf>,
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
        let app_registry = app
            .app_server
            .as_ref()
            .map(|server| server.registry().clone());
        let (request_tx, request_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let shutdown = Arc::new(Notify::new());
        let host_token = Arc::new(config.token);
        let state = Arc::new(HostState {
            requests: request_tx,
            events: events.clone(),
            token: host_token.clone(),
            app_server_port,
            app_registry,
            client: reqwest::Client::builder()
                .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
                .pool_max_idle_per_host(8)
                .pool_idle_timeout(Duration::from_secs(30))
                .build()
                .context("building host upstream client")?,
            connections: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            gateway_base: config.gateway_base,
            web_dir: config.web_dir,
        });
        let actor_events = events;
        let actor_shutdown = shutdown.clone();
        let actor_task = tokio::spawn(async move {
            app_actor(app, request_rx, actor_events, actor_shutdown, host_token).await;
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
                let Ok(permit) = state.connections.clone().try_acquire_owned() else {
                    let mut stream = stream;
                    let _ = respond_text(&mut stream, 503, "host connection limit reached").await;
                    continue;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = handle_connection(stream, state).await;
                });
            }
        }
    }
}

#[derive(Debug)]
struct RequestHead {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    content_length: usize,
    /// Body bytes read together with the headers. Keeping this prefix avoids
    /// losing pipelined data when authentication rejects the request.
    prefix: Vec<u8>,
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

async fn read_request_head(stream: &mut TcpStream) -> Result<RequestHead, ReadError> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let remaining = MAX_HEADER_BYTES.saturating_sub(buffer.len());
        if remaining == 0 {
            return Err(ReadError {
                status: 431,
                message: "request headers too large",
            });
        }
        let read_len = remaining.min(chunk.len());
        let n = stream
            .read(&mut chunk[..read_len])
            .await
            .map_err(|_| ReadError {
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
    let prefix = buffer[body_start..body_start + already].to_vec();
    Ok(RequestHead {
        method,
        target,
        headers,
        content_length,
        prefix,
    })
}

/// Read `content_length` body bytes, `prefix` already having arrived with the
/// headers. The buffer grows with the bytes that actually turn up rather than
/// reserving the declared length up front. Callers authenticate before
/// invoking this for protected routes.
async fn read_body(
    stream: &mut TcpStream,
    prefix: &[u8],
    content_length: usize,
) -> Result<Vec<u8>, ReadError> {
    let incomplete = ReadError {
        status: 400,
        message: "incomplete request body",
    };
    let mut body = Vec::with_capacity(content_length.min(BODY_CHUNK_BYTES).max(prefix.len()));
    body.extend_from_slice(prefix);
    while body.len() < content_length {
        let start = body.len();
        let want = (content_length - start).min(BODY_CHUNK_BYTES);
        body.resize(start + want, 0);
        let n = stream
            .read(&mut body[start..])
            .await
            .map_err(|_| ReadError {
                status: incomplete.status,
                message: incomplete.message,
            })?;
        body.truncate(start + n);
        if n == 0 {
            return Err(incomplete);
        }
    }
    Ok(body)
}

async fn handle_connection(mut stream: TcpStream, state: Arc<HostState>) -> io::Result<()> {
    let head =
        match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_request_head(&mut stream)).await {
            Ok(Ok(head)) => head,
            Ok(Err(error)) => return respond_text(&mut stream, error.status, error.message).await,
            Err(_) => return respond_text(&mut stream, 408, "request timed out").await,
        };
    let (path, query) = split_target(&head.target);
    let path = path.to_string();
    let query = query.to_string();
    if head.method.eq_ignore_ascii_case("OPTIONS") {
        return respond_empty(&mut stream, 204).await;
    }
    let is_apps = path == "/apps" || path.starts_with("/apps/");
    let is_v1 = path == "/v1" || path.starts_with("/v1/");
    if (is_apps || is_v1)
        && !authorized(
            &head,
            &state.token,
            is_apps,
            state.app_registry.as_ref(),
            &path,
        )
    {
        return respond_json(
            &mut stream,
            401,
            &serde_json::json!({
                "error": { "message": "unauthorized", "type": "authentication_error" }
            }),
        )
        .await;
    }
    if head.content_length > MAX_GATEWAY_BODY_BYTES {
        return respond_text(&mut stream, 413, "request entity too large").await;
    }
    let body = match tokio::time::timeout(
        REQUEST_READ_TIMEOUT,
        read_body(&mut stream, &head.prefix, head.content_length),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => return respond_text(&mut stream, error.status, error.message).await,
        Err(_) => return respond_text(&mut stream, 408, "request timed out").await,
    };
    let request = Request {
        method: head.method,
        target: head.target,
        headers: head.headers,
        body,
    };

    if path == "/v1/events" {
        return handle_sse(&mut stream, &request, &state).await;
    }
    if path == "/v1/chat/completions" {
        return handle_gateway(&mut stream, &request, &state).await;
    }
    if is_apps {
        // Public app URLs are capabilities in their own right: the registry
        // UUID is high entropy, while the host bearer token never appears in
        // the URL, cookie jar, referrer, or proxy logs.
        return proxy_app(&mut stream, &request, &path, &query, &state).await;
    }
    if !is_v1 && path != "/.well-known/nexus" {
        return serve_web(&mut stream, &request, &path, state.web_dir.as_deref()).await;
    }
    handle_api_route(&mut stream, &request, &path, &state).await
}

async fn serve_web(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    root: Option<&std::path::Path>,
) -> io::Result<()> {
    if !matches!(request.method.as_str(), "GET" | "HEAD") {
        return respond_text(stream, 405, "method not allowed").await;
    }
    let Some(root) = root else {
        return respond_text(
            stream,
            404,
            "Web client unavailable. Build web/ and set NEXUS_WEB_DIR to its dist directory.",
        )
        .await;
    };
    let decoded = percent_decode(path);
    let relative = decoded.trim_start_matches('/');
    if relative
        .split('/')
        .any(|part| part == ".." || part.starts_with('.'))
        || relative.contains('\\')
    {
        return respond_text(stream, 404, "not found").await;
    }
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return respond_text(stream, 404, "web build not found").await;
    };
    let target = root.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    let Ok(target) = tokio::fs::canonicalize(target).await else {
        return respond_text(stream, 404, "not found").await;
    };
    if !target.starts_with(&root) {
        return respond_text(stream, 404, "not found").await;
    }
    let Ok(metadata) = tokio::fs::metadata(&target).await else {
        return respond_text(stream, 404, "not found").await;
    };
    if !metadata.is_file() || metadata.len() > MAX_APP_RESPONSE_BYTES as u64 {
        return respond_text(stream, 404, "not found").await;
    }
    match tokio::fs::read(&target).await {
        Ok(bytes) => {
            respond_full(
                stream,
                200,
                crate::appserver::mime_for(&target),
                "X-Content-Type-Options: nosniff\r\n",
                &bytes,
                request.method == "HEAD",
            )
            .await
        }
        Err(_) => respond_text(stream, 404, "not found").await,
    }
}

fn authorized(
    request: &RequestHead,
    token: &str,
    apps: bool,
    registry: Option<&AppRegistry>,
    path: &str,
) -> bool {
    if let Some(value) = request.headers.get("authorization")
        && let Some((scheme, supplied)) = value.split_once(char::is_whitespace)
        && scheme.eq_ignore_ascii_case("bearer")
        && constant_time_eq(supplied.trim().as_bytes(), token.as_bytes())
    {
        return true;
    }
    apps && known_app_path(registry, path)
}

fn known_app_path(registry: Option<&AppRegistry>, path: &str) -> bool {
    let Some(registry) = registry else {
        return false;
    };
    let Some(rest) = path.strip_prefix("/apps/") else {
        return false;
    };
    let Some(uuid) = rest.split('/').next() else {
        return false;
    };
    !uuid.is_empty() && registry.lookup(uuid).is_some()
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
    if path == "/v1/media/action" && request.method == "POST" {
        let value = serde_json::from_slice::<serde_json::Value>(&request.body).unwrap_or_default();
        if value["action"] == "run_script" {
            let query = split_target(&request.target).1;
            let space_id = query_param(query, "space_id").unwrap_or_default();
            let name = value["name"].as_str().unwrap_or_default().to_string();
            let Ok(args) = serde_json::from_value::<Vec<String>>(
                value
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            ) else {
                return respond_error(stream, 400, "invalid script arguments").await;
            };
            let job = ask_actor(&state.requests, |reply| HostRequest::ScriptJob {
                space_id,
                name,
                args,
                reply,
            })
            .await;
            let result = match job {
                Ok(job) => super::jobs::run(job)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            };
            return match result {
                Ok(value) => respond_json(stream, 200, &value).await,
                Err(error) => respond_error(stream, 400, &error).await,
            };
        }
    }
    if path.starts_with("/v1/sessions/") && matches!(request.method.as_str(), "PATCH" | "DELETE") {
        let id = percent_decode(path.trim_start_matches("/v1/sessions/"));
        if id.is_empty() || id.contains('/') {
            return respond_error(stream, 400, "invalid session id").await;
        }
        let body = serde_json::from_slice::<serde_json::Value>(&request.body).unwrap_or_default();
        let space_id = body["space_id"]
            .as_str()
            .map(str::to_string)
            .or_else(|| query_param(split_target(&request.target).1, "space_id"))
            .unwrap_or_default();
        let title = (request.method == "PATCH")
            .then(|| body["title"].as_str().unwrap_or_default().to_string());
        let result = ask_actor(&state.requests, |reply| HostRequest::SessionEdit {
            space_id,
            id,
            title,
            reply,
        })
        .await;
        return match result {
            Ok(value) => respond_json(stream, 200, &value).await,
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if path == "/v1/sync/bundle" {
        if !matches!(request.method.as_str(), "GET" | "POST") {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let input = (request.method == "POST").then(|| request.body.clone());
        let result = ask_actor(&state.requests, |reply| HostRequest::Bundle {
            input,
            reply,
        })
        .await;
        return match result {
            Ok(bytes) => {
                respond_full(
                    stream,
                    200,
                    "application/zip",
                    "Content-Disposition: attachment; filename=nexus-sync.zip\r\n",
                    &bytes,
                    false,
                )
                .await
            }
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if path == "/v1/media/blob" {
        if request.method != "GET" {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let query = split_target(&request.target).1.to_string();
        let result = ask_actor(&state.requests, |reply| HostRequest::MediaBlob {
            query,
            reply,
        })
        .await;
        return match result {
            Ok(blob) => {
                respond_full(
                    stream,
                    200,
                    &blob.mime,
                    "Content-Security-Policy: sandbox\r\nX-Content-Type-Options: nosniff\r\n",
                    &blob.bytes,
                    false,
                )
                .await
            }
            Err(error) => respond_error(stream, 404, &error).await,
        };
    }
    if (request.method == "GET" && matches!(path, "/v1/sync" | "/v1/sync/state"))
        || matches!(
            path,
            "/v1/settings"
                | "/v1/settings/document"
                | "/v1/login"
                | "/v1/attachments"
                | "/v1/models/refresh"
                | "/v1/media"
                | "/v1/media/action"
        )
        || ["/v1/swarm", "/v1/skills", "/v1/watches", "/v1/research"]
            .iter()
            .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    {
        if request.body.len() > MAX_API_BODY_BYTES
            && !(path == "/v1/media" && request.method == "PUT")
        {
            return respond_text(stream, 413, "request entity too large").await;
        }
        let route = path.to_string();
        let method = request.method.clone();
        let query = split_target(&request.target).1.to_string();
        let body = request.body.clone();
        let result = ask_actor(&state.requests, |reply| HostRequest::Surface {
            route,
            method,
            query,
            body,
            reply,
        })
        .await;
        return match result {
            Ok(value) => respond_json(stream, 200, &value).await,
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if matches!(path, "/v1/apps" | "/v1/apps/files" | "/v1/apps/source") {
        if request.method != "GET"
            && !(request.method == "PUT" && path == "/v1/apps/source")
            && !(path == "/v1/apps" && matches!(request.method.as_str(), "POST" | "DELETE"))
        {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let query = split_target(&request.target).1;
        let space_id = query_param(query, "space_id");
        let name = query_param(query, "name").unwrap_or_default();
        let file = query_param(query, "path").unwrap_or_default();
        let edit = if request.method == "PUT" {
            match serde_json::from_slice::<super::apps::Edit>(&request.body) {
                Ok(edit) => Some(edit),
                Err(_) => return respond_error(stream, 400, "invalid source edit").await,
            }
        } else {
            None
        };
        let route = path.to_string();
        let method = request.method.clone();
        let result = ask_actor(&state.requests, |reply| HostRequest::Apps {
            route,
            method,
            space_id,
            name,
            file,
            edit,
            reply,
        })
        .await;
        return match result {
            Ok(value) => respond_json(stream, 200, &value).await,
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if path == "/v1/sync/blob"
        || path.starts_with("/v1/sync/blob/")
        || path.starts_with("/v1/sync/blobs/")
    {
        return handle_sync_blob(stream, request, path, state).await;
    }
    if let Some(session_id) = session_messages_path(path) {
        if request.method != "GET" {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let messages = ask_actor(&state.requests, |reply| HostRequest::Messages {
            session_id,
            reply,
        })
        .await;
        return match messages {
            Ok(messages) => {
                respond_json(stream, 200, &serde_json::json!({ "messages": messages })).await
            }
            Err(error) => respond_error(stream, 404, &error).await,
        };
    }
    match (request.method.as_str(), path) {
        ("GET", "/.well-known/nexus") => {
            let discovery = DiscoveryResponse {
                host_id: host_discovery_id(&state.token),
                product: "nexus-chat",
                version: env!("CARGO_PKG_VERSION"),
                api: "/v1",
                authentication: "bearer",
            };
            respond_json(stream, 200, &discovery).await
        }
        ("POST", "/v1/spaces") | ("PUT", "/v1/files") => {
            let query = split_target(&request.target).1;
            let name = query_param(query, "name").unwrap_or_default();
            let space_id = query_param(query, "space_id");
            let bytes = (path == "/v1/files").then(|| request.body.clone());
            let result = ask_actor(&state.requests, |reply| HostRequest::WorkspaceWrite {
                name,
                space_id,
                bytes,
                reply,
            })
            .await;
            match result {
                Ok(()) => respond_json(stream, 201, &serde_json::json!({ "ok": true })).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("GET", "/v1/spaces" | "/v1/files") => {
            let files = path == "/v1/files";
            let space_id = query_param(split_target(&request.target).1, "space_id");
            let result = ask_actor(&state.requests, |reply| HostRequest::Workspace {
                files,
                space_id,
                reply,
            })
            .await;
            match result {
                Ok(value) => respond_json(stream, 200, &value).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("GET", "/v1/snapshot") => {
            let snapshot =
                ask_actor(&state.requests, |reply| HostRequest::Snapshot { reply }).await;
            match snapshot {
                Ok(snapshot) => respond_json(stream, 200, &snapshot).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/usage") => {
            let range = query_param(split_target(&request.target).1, "range")
                .map(|value| UsageRange::from_key(&value))
                .unwrap_or_default();
            let usage =
                ask_actor(&state.requests, |reply| HostRequest::Usage { range, reply }).await;
            match usage {
                Ok(usage) => respond_json(stream, 200, &usage).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/models") => {
            let models = ask_actor(&state.requests, |reply| HostRequest::Models { reply }).await;
            match models {
                Ok(models) => {
                    let response = ModelsResponse {
                        object: "list",
                        data: models.into_iter().map(OpenAiModel::from).collect(),
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
            let input = match parse_tool_rpc_request(&request.body) {
                Ok(input) => input,
                Err(error) => {
                    let response = ToolRpcResponse::error(error.id, error.error);
                    return respond_json(stream, 400, &response).await;
                }
            };
            let space_id = query_param(split_target(&request.target).1, "space_id");
            let toolbox = match ask_actor(&state.requests, |reply| HostRequest::Toolbox {
                space_id,
                reply,
            })
            .await
            {
                Ok(toolbox) => toolbox,
                Err(error) => {
                    let response = ToolRpcResponse::error(input.id, ToolRpcError::internal(error));
                    return respond_json(stream, 200, &response).await;
                }
            };
            let args = match input.args {
                serde_json::Value::String(args) => args,
                value => value.to_string(),
            };
            let (result, label) = toolbox.run(&input.name, &args).await;
            respond_json(
                stream,
                200,
                &ToolRpcResponse::success(input.id, ToolRunResult { result, label }),
            )
            .await
        }
        ("GET", "/") => respond_text(stream, 200, "nexus host\n").await,
        _ => respond_text(stream, 404, "not found").await,
    }
}

#[derive(Debug, Serialize)]
struct DiscoveryResponse {
    host_id: String,
    product: &'static str,
    version: &'static str,
    api: &'static str,
    authentication: &'static str,
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    object: &'static str,
    data: Vec<OpenAiModel>,
}

/// `OpenAI` `/v1/models` fields, with Nexus routing metadata retained as
/// additive fields for clients that want backend-aware pickers.
#[derive(Debug, Serialize)]
struct OpenAiModel {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
    backend: WireBackendTag,
    name: String,
    reasoning_efforts: Vec<super::wire::WireReasoningEffort>,
    context_length: Option<u64>,
    supports_images: bool,
    supports_image_generation: bool,
    supports_video_generation: bool,
    pricing: Option<super::wire::WireModelPricing>,
}

impl From<WireModel> for OpenAiModel {
    fn from(model: WireModel) -> Self {
        let owned_by = match model.backend {
            WireBackendTag::OpenRouter => "openrouter",
            WireBackendTag::OpenAi => "openai",
            WireBackendTag::OpencodeGo => "opencode-go",
            WireBackendTag::Codex => "codex",
            WireBackendTag::Local => "local",
        };
        Self {
            id: model.id,
            object: "model",
            created: 0,
            owned_by: owned_by.to_string(),
            backend: model.backend,
            name: model.name,
            reasoning_efforts: model.reasoning_efforts,
            context_length: model.context_length,
            supports_images: model.supports_images,
            supports_image_generation: model.supports_image_generation,
            supports_video_generation: model.supports_video_generation,
            pricing: model.pricing,
        }
    }
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
    /// Whether `/v1/chat/completions` can route this backend. Always true;
    /// kept in the wire shape so existing clients keep parsing `/v1/backends`.
    gateway_supported: bool,
    /// Stable explanation when a backend cannot be routed. Always `None`
    /// today — retained alongside `gateway_supported` for the same reason.
    gateway_error: Option<&'static str>,
    default_model: String,
    model_count: usize,
}

#[derive(Debug)]
struct ToolRpcRequest {
    id: serde_json::Value,
    name: String,
    args: serde_json::Value,
}

#[derive(Debug)]
struct ToolRpcRequestError {
    id: serde_json::Value,
    error: ToolRpcError,
}

impl ToolRpcRequestError {
    fn new(id: serde_json::Value, error: ToolRpcError) -> Self {
        Self { id, error }
    }
}

#[derive(Debug, Serialize)]
struct ToolRunResult {
    result: String,
    label: String,
}

#[derive(Debug, Serialize)]
struct ToolRpcError {
    code: i32,
    message: String,
}

impl ToolRpcError {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "invalid JSON".to_string(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ToolRpcResponse {
    jsonrpc: &'static str,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ToolRunResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ToolRpcError>,
}

impl ToolRpcResponse {
    fn success(id: serde_json::Value, result: ToolRunResult) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: serde_json::Value, error: ToolRpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

fn parse_tool_rpc_request(body: &[u8]) -> Result<ToolRpcRequest, ToolRpcRequestError> {
    let value = serde_json::from_slice::<serde_json::Value>(body).map_err(|_| {
        ToolRpcRequestError::new(serde_json::Value::Null, ToolRpcError::parse_error())
    })?;
    let object = value.as_object().ok_or_else(|| {
        ToolRpcRequestError::new(
            serde_json::Value::Null,
            ToolRpcError::invalid_request("request must be a JSON object"),
        )
    })?;
    let id = object.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let fail = |error| ToolRpcRequestError::new(id.clone(), error);
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(fail(ToolRpcError::invalid_request(
            "jsonrpc must be the string \"2.0\"",
        )));
    }
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| fail(ToolRpcError::invalid_request("method must be a string")))?;
    if method != "tools/run" {
        return Err(fail(ToolRpcError::method_not_found(method)));
    }
    let params = object
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| fail(ToolRpcError::invalid_params("params must be an object")))?;
    let name = params
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            fail(ToolRpcError::invalid_params(
                "params.name must be a non-empty string",
            ))
        })?
        .to_string();
    let args = params
        .get("arguments")
        .or_else(|| params.get("args"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(ToolRpcRequest { id, name, args })
}

async fn handle_sync_blob(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    state: &HostState,
) -> io::Result<()> {
    let query = split_target(&request.target).1;
    let path_parts = path
        .strip_prefix("/v1/sync/blob/")
        .or_else(|| path.strip_prefix("/v1/sync/blobs/"))
        .and_then(|rest| rest.split_once('/'));
    let space_id = query_param(query, "space_id")
        .or_else(|| path_parts.map(|(space, _)| percent_decode(space)));
    let Some(space_id) = space_id else {
        return respond_error(stream, 400, "sync blob is missing space_id").await;
    };
    let name =
        query_param(query, "name").or_else(|| path_parts.map(|(_, name)| percent_decode(name)));
    let Some(name) = name else {
        return respond_error(stream, 400, "sync blob is missing name").await;
    };
    match request.method.as_str() {
        "PUT" => {
            if request.body.len() > MAX_BLOB_BYTES {
                return respond_text(stream, 413, "blob too large").await;
            }
            let hash = query_param(query, "hash")
                .or_else(|| request.headers.get("x-nexus-blob-hash").cloned())
                .or_else(|| request.headers.get("x-content-sha256").cloned());
            let Some(hash) = hash else {
                return respond_error(stream, 400, "sync blob is missing hash").await;
            };
            let result = ask_actor(&state.requests, |reply| HostRequest::PutBlob {
                space_id,
                name,
                hash,
                bytes: request.body.clone(),
                reply,
            })
            .await;
            match result {
                Ok(()) => {
                    respond_json(
                        stream,
                        201,
                        &serde_json::json!({ "ok": true, "bytes": request.body.len() }),
                    )
                    .await
                }
                Err(error) => respond_error(stream, 409, &error).await,
            }
        }
        "GET" | "HEAD" => {
            let result = ask_actor(&state.requests, |reply| HostRequest::GetBlob {
                space_id,
                name,
                reply,
            })
            .await;
            match result {
                Ok(Some(bytes)) => {
                    respond_full(
                        stream,
                        200,
                        "application/octet-stream",
                        "",
                        &bytes,
                        request.method == "HEAD",
                    )
                    .await
                }
                Ok(None) => respond_text(stream, 404, "sync blob not found").await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        _ => respond_text(stream, 405, "method not allowed").await,
    }
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
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n{CORS_HEADERS}\r\n"
            )
            .as_bytes(),
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
    let (default_base, raw_model) = openrouter::gateway_route(route.tag, &route.model);
    let base = state.gateway_base.as_deref().unwrap_or(default_base);
    let codex = route.tag == BackendTag::Codex;
    let streaming_requested = body
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let url = if codex {
        format!("{base}/codex/responses")
    } else {
        format!("{base}/chat/completions")
    };
    // `/v1/models` publishes composite ids (`openai:`/`codex:`/`opencode:`
    // prefixes, plus the `go:` tag `list_models` adds), which no upstream
    // recognizes. Forward the raw id the route resolved to, not the client's.
    let payload = if codex {
        serde_json::to_vec(&openrouter::chat_body_to_codex_body(
            &body,
            streaming_requested,
            &[],
        ))
        .unwrap_or_else(|_| request.body.clone())
    } else {
        let mut forwarded = body.clone();
        if let Some(object) = forwarded.as_object_mut() {
            object.insert(
                "model".to_string(),
                serde_json::Value::String(raw_model.clone()),
            );
        }
        serde_json::to_vec(&forwarded).unwrap_or_else(|_| request.body.clone())
    };
    let mut builder = state
        .client
        .post(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", route.key),
        )
        .body(payload);
    for (name, value) in &request.headers {
        if is_hop_by_hop(name) || name.starts_with("x-nexus-") {
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
    if codex {
        if let Some(account_id) = &route.account_id {
            builder = builder.header("chatgpt-account-id", account_id);
        }
        builder = builder
            .header("originator", "nexus-host")
            .header("OpenAI-Beta", "responses=experimental")
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if streaming_requested {
            builder = builder.header(reqwest::header::ACCEPT, "text/event-stream");
        }
    }
    let response = match tokio::time::timeout(UPSTREAM_HEADERS_TIMEOUT, builder.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return respond_error(stream, 502, &format!("upstream request failed: {error}")).await;
        }
        Err(_) => return respond_error(stream, 504, "upstream request timed out").await,
    };
    if codex && response.status().is_success() {
        return handle_codex_gateway_response(
            stream,
            response,
            state,
            route,
            &raw_model,
            streaming_requested,
        )
        .await;
    }
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
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\nConnection: close\r\n{CORS_HEADERS}\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    loop {
        let Ok(next) = tokio::time::timeout(UPSTREAM_CHUNK_TIMEOUT, upstream.next()).await else {
            break;
        };
        let Some(chunk) = next else {
            break;
        };
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
    usage.finish(streaming);
    log_gateway_usage(state, route, usage).await;
    stream.shutdown().await
}

async fn handle_codex_gateway_response(
    stream: &mut TcpStream,
    response: reqwest::Response,
    state: &HostState,
    route: GatewayRoute,
    model: &str,
    streaming: bool,
) -> io::Result<()> {
    if !streaming {
        let status = response.status().as_u16();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                return respond_error(
                    stream,
                    502,
                    &format!("reading Codex response failed: {error}"),
                )
                .await;
            }
        };
        let response_json: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return respond_error(stream, 502, &format!("invalid Codex response: {error}"))
                    .await;
            }
        };
        let translated = openrouter::codex_response_to_chat(&response_json);
        let payload = match serde_json::to_vec(&translated) {
            Ok(payload) => payload,
            Err(error) => {
                return respond_error(
                    stream,
                    502,
                    &format!("encoding Codex response failed: {error}"),
                )
                .await;
            }
        };
        let mut usage = GatewayUsage::default();
        usage.observe(&payload, false);
        usage.finish(false);
        log_gateway_usage(state, route, usage).await;
        return respond_full(stream, status, "application/json", "", &payload, false).await;
    }

    let mut adapter = openrouter::CodexChatStreamAdapter::new(model.to_string());
    let mut usage = GatewayUsage::default();
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: close\r\n{CORS_HEADERS}\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    let mut upstream = response.bytes_stream();
    loop {
        // A stream that stops early must not be reported as a clean stop: the
        // adapter's terminal chunk would otherwise tell the client the answer
        // finished normally when it was cut off mid-response.
        let Ok(next) = tokio::time::timeout(UPSTREAM_CHUNK_TIMEOUT, upstream.next()).await else {
            adapter.fail("Codex stream stalled");
            break;
        };
        let Some(chunk) = next else { break };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                adapter.fail(format!("Codex stream failed: {error}"));
                break;
            }
        };
        for frame in adapter.push_bytes(&chunk) {
            usage.observe(&frame, true);
            if stream.write_all(&frame).await.is_err() {
                return Ok(());
            }
            if stream.flush().await.is_err() {
                return Ok(());
            }
        }
    }
    for frame in adapter.finish() {
        usage.observe(&frame, true);
        if stream.write_all(&frame).await.is_err() {
            return Ok(());
        }
        if stream.flush().await.is_err() {
            return Ok(());
        }
    }
    usage.finish(true);
    log_gateway_usage(state, route, usage).await;
    stream.shutdown().await
}

async fn log_gateway_usage(state: &HostState, route: GatewayRoute, usage: GatewayUsage) {
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
        if is_hop_by_hop(name) || name == "cookie" || name == "x-nexus-token" {
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
    let response = match tokio::time::timeout(UPSTREAM_HEADERS_TIMEOUT, builder.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return respond_error(stream, 502, &format!("app proxy failed: {error}")).await;
        }
        Err(_) => return respond_error(stream, 504, "app proxy timed out").await,
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let mut body = Vec::new();
    let mut upstream = response.bytes_stream();
    loop {
        let Ok(next) = tokio::time::timeout(UPSTREAM_CHUNK_TIMEOUT, upstream.next()).await else {
            return respond_error(stream, 504, "app response timed out").await;
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return respond_error(stream, 502, &format!("app response failed: {error}")).await;
            }
        };
        if body.len().saturating_add(chunk.len()) > MAX_APP_RESPONSE_BYTES {
            return respond_text(stream, 502, "app response too large").await;
        }
        body.extend_from_slice(&chunk);
    }
    respond_full(
        stream,
        status,
        &content_type,
        // Generated applications are untrusted documents on the host origin.
        // An opaque sandbox origin prevents access to the web client's tokens
        // in browser storage, including when an app is opened in a new tab.
        "Content-Security-Policy: sandbox allow-scripts allow-forms allow-downloads allow-popups\r\nX-Content-Type-Options: nosniff\r\n",
        &body,
        request.method == "HEAD",
    )
    .await
}

/// Headers that must not be relayed to an upstream. `accept-encoding` is in
/// the list because both proxies forward the upstream body verbatim while
/// only relaying `Content-Type` — a compressed upstream response would
/// otherwise reach the client without its `Content-Encoding` (and defeat the
/// gateway's usage parsing).
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "authorization"
            | "content-length"
            | "transfer-encoding"
            | "accept-encoding"
            | "connection"
            | "keep-alive"
            | "expect"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-authenticate"
            | "proxy-authorization"
    )
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

fn redact_wire_event(event: WireEvent, app: &App, host_token: &str) -> WireEvent {
    let mut secrets = vec![
        host_token,
        app.saved.host_token.as_deref().unwrap_or_default(),
        app.saved.openrouter_key.as_deref().unwrap_or_default(),
        app.saved.openai_key.as_deref().unwrap_or_default(),
        app.saved.opencode_key.as_deref().unwrap_or_default(),
        app.saved
            .codex
            .as_ref()
            .map_or("", |credentials| credentials.access.as_str()),
        app.saved
            .codex
            .as_ref()
            .map_or("", |credentials| credentials.refresh.as_str()),
        app.langsearch_key.as_str(),
        app.searxng_url.as_str(),
    ];
    secrets.retain(|secret| !secret.is_empty());
    let Ok(mut value) = serde_json::to_value(event.clone()) else {
        return event;
    };
    redact_json_secrets(&mut value, &secrets);
    serde_json::from_value(value).unwrap_or(event)
}

fn redact_json_secrets(value: &mut serde_json::Value, secrets: &[&str]) {
    match value {
        serde_json::Value::String(text) => {
            for secret in secrets {
                *text = text.replace(secret, "[redacted]");
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json_secrets(value, secrets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_json_secrets(value, secrets);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

async fn app_actor(
    mut app: App,
    mut requests: mpsc::Receiver<HostRequest>,
    events: broadcast::Sender<WireEvent>,
    shutdown: Arc<Notify>,
    host_token: Arc<String>,
) {
    let mut admin = super::admin::State::default();
    loop {
        tokio::select! {
            () = shutdown.notified() => break,
            request = requests.recv() => {
                let Some(request) = request else { break; };
                handle_actor_request(&mut app, &mut admin, request);
            }
            event = app.next_event() => {
                admin.observe(&event);
                let wire = redact_wire_event(WireEvent::from(event.clone()), &app, host_token.as_str());
                // The handler may clear a one-shot receiver itself; this
                // backstop also prevents a closed source from returning
                // `None` forever when a remote client is the only consumer.
                clear_closed_source(&mut app, &event);
                apply_domain_event(&mut app, event);
                let _ = events.send(wire);
            }
        }
    }
}

fn write_workspace(
    app: &mut App,
    name: &str,
    space_id: Option<&str>,
    bytes: Option<&[u8]>,
) -> Result<()> {
    if name.trim().is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        bail!("invalid name");
    }
    if let Some(bytes) = bytes {
        let space = app
            .db
            .list_spaces()?
            .into_iter()
            .find(|space| Some(space.id.as_str()) == space_id)
            .ok_or_else(|| anyhow!("unknown space"))?;
        let dir = app.space.files_dir(&space.name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(name);
        // Never silently overwrite a user's existing file, including symlinks.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        if let Err(error) = std::io::Write::write_all(&mut file, bytes) {
            let _ = std::fs::remove_file(&path);
            return Err(error.into());
        }
        let hash = hex_string(&Sha256::digest(bytes));
        app.db.upsert_file(
            &space.id,
            name,
            &hash,
            i64::try_from(bytes.len())?,
            "not indexed",
        )?;
        if space.id == app.active_space.id {
            app.rescan_files();
        }
    } else {
        app.space.ensure_space_dir(name)?;
        app.db.create_space(name)?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn handle_actor_request(app: &mut App, admin: &mut super::admin::State, request: HostRequest) {
    match request {
        HostRequest::ScriptJob {
            space_id,
            name,
            args,
            reply,
        } => {
            let _ = reply.send(
                super::media::prepare_script(app, &space_id, &name, &args)
                    .map_err(|error| error.to_string()),
            );
        }
        HostRequest::SessionEdit {
            space_id,
            id,
            title,
            reply,
        } => {
            let result = if let Some(title) = title {
                super::sessions_view::rename(app, &space_id, &id, &title)
                    .and_then(|value| Ok(serde_json::to_value(value)?))
            } else {
                super::sessions_view::delete(app, &space_id, &id)
            };
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }
        HostRequest::Bundle { input, reply } => {
            let _ = reply.send(
                super::browser_sync::bundle(app, input.as_deref())
                    .map_err(|error| error.to_string()),
            );
        }

        HostRequest::MediaBlob { query, reply } => {
            let _ = reply.send(super::media::blob(app, &query).map_err(|error| error.to_string()));
        }

        HostRequest::Surface {
            route,
            method,
            query,
            body,
            reply,
        } => {
            let result = if route.starts_with("/v1/sync") {
                super::browser_sync::dispatch(app, &method, &route, &query)
            } else if route.starts_with("/v1/research/") {
                super::research_view::dispatch(app, &method, &route, &query, &body)
            } else if ["/v1/swarm", "/v1/skills", "/v1/watches"]
                .iter()
                .any(|prefix| route.starts_with(prefix))
            {
                super::management::dispatch(app, &method, &route, &query, &body)
            } else if route.starts_with("/v1/media") {
                super::media::dispatch(app, &method, &query, &body)
            } else {
                super::admin::dispatch(app, admin, &method, &route, &query, &body)
            };
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }

        HostRequest::Apps {
            route,
            method,
            space_id,
            name,
            file,
            edit,
            reply,
        } => {
            let result = match (method.as_str(), route.as_str()) {
                ("DELETE", "/v1/apps") => super::apps::delete(app, space_id.as_deref(), &name),
                ("POST", "/v1/apps") => super::apps::register(app, space_id.as_deref(), &name),
                (_, "/v1/apps") => super::apps::catalog(app, space_id.as_deref()),
                (_, "/v1/apps/files") => super::apps::files(app, space_id.as_deref(), &name),
                _ => super::apps::source(app, space_id.as_deref(), &name, &file, edit),
            };
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }

        HostRequest::WorkspaceWrite {
            name,
            space_id,
            bytes,
            reply,
        } => {
            let _ = reply.send(
                write_workspace(app, &name, space_id.as_deref(), bytes.as_deref())
                    .map_err(|error| error.to_string()),
            );
        }
        HostRequest::Workspace {
            files,
            space_id,
            reply,
        } => {
            let result = (|| -> Result<serde_json::Value> {
                let spaces = app.db.list_spaces()?;
                if files {
                    let id = space_id.as_deref().unwrap_or(&app.active_space.id);
                    if !spaces.iter().any(|space| space.id == id) {
                        bail!("unknown space");
                    }
                    let files = app.db.list_files(id)?.into_iter().map(|file| serde_json::json!({
                        "id": file.id, "name": file.name, "size": file.size, "hash": file.hash, "status": file.status
                    })).collect::<Vec<_>>();
                    Ok(serde_json::json!({ "space_id": id, "files": files }))
                } else {
                    let spaces = spaces
                        .into_iter()
                        .map(|space| serde_json::json!({ "id": space.id, "name": space.name }))
                        .collect::<Vec<_>>();
                    Ok(
                        serde_json::json!({ "active_space_id": app.active_space.id, "spaces": spaces }),
                    )
                }
            })();
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }
        HostRequest::Snapshot { reply } => {
            let _ = reply.send(app.snapshot().map_err(|error| error.to_string()));
        }
        HostRequest::Models { reply } => {
            // Every configured backend has an OpenAI-wire gateway route.
            let models = app.models.iter().cloned().map(WireModel::from).collect();
            let _ = reply.send(Ok(models));
        }
        HostRequest::Usage { range, reply } => {
            app.backfill_usage_costs();
            let data = app.load_usage_for_range(range);
            let usage = serde_json::json!({
                "range": range.key(),
                "totals": {
                    "requests": data.totals.requests,
                    "prompt_tokens": data.totals.prompt_tokens,
                    "completion_tokens": data.totals.completion_tokens,
                    "cache_read_tokens": data.totals.cache_read_tokens,
                    "cache_creation_tokens": data.totals.cache_creation_tokens,
                    "rated_prompt_tokens": data.totals.rated_prompt_tokens,
                    "rated_cache_read_tokens": data.totals.rated_cache_read_tokens,
                    "cost": data.totals.cost,
                },
                "by_backend": data.by_backend.iter().map(|row| serde_json::json!({
                    "backend": row.backend,
                    "requests": row.requests,
                    "prompt_tokens": row.prompt_tokens,
                    "completion_tokens": row.completion_tokens,
                    "cache_read_tokens": row.cache_read_tokens,
                    "rated_prompt_tokens": row.rated_prompt_tokens,
                    "rated_cache_read_tokens": row.rated_cache_read_tokens,
                    "cost": row.cost,
                })).collect::<Vec<_>>(),
                "by_model": data.by_model.iter().map(|row| serde_json::json!({
                    "model": row.model,
                    "requests": row.requests,
                    "prompt_tokens": row.prompt_tokens,
                    "completion_tokens": row.completion_tokens,
                    "cache_read_tokens": row.cache_read_tokens,
                    "rated_prompt_tokens": row.rated_prompt_tokens,
                    "rated_cache_read_tokens": row.rated_cache_read_tokens,
                    "cost": row.cost,
                })).collect::<Vec<_>>(),
                "recent": data.recent.iter().map(|row| serde_json::json!({
                    "created_at": row.created_at,
                    "backend": row.backend,
                    "model": row.model,
                    "prompt_tokens": row.prompt_tokens,
                    "completion_tokens": row.completion_tokens,
                    "cache_read_tokens": row.cache_read_tokens,
                    "cache_creation_tokens": row.cache_creation_tokens,
                    "prompt_convention": row.prompt_convention.as_str(),
                    "cost": row.cost,
                })).collect::<Vec<_>>(),
            });
            let _ = reply.send(Ok(usage));
        }
        HostRequest::Backends { reply } => {
            let tags = [
                BackendTag::OpenRouter,
                BackendTag::OpenAi,
                BackendTag::OpencodeGo,
                BackendTag::Codex,
                BackendTag::Local,
            ];
            let backends = tags
                .into_iter()
                .map(|tag| {
                    let provider = app.backends.get(tag);
                    BackendInfo {
                        tag: tag.into(),
                        name: tag.display_name(),
                        configured: app.backends.configured(tag),
                        gateway_supported: true,
                        gateway_error: None,
                        default_model: provider.map_or_else(String::new, |provider| {
                            public_model_id_for_backend(tag, provider.default_utility_model())
                        }),
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
        HostRequest::Messages { session_id, reply } => {
            let _ = reply.send(
                app.session_messages(&session_id)
                    .map_err(|error| error.to_string()),
            );
        }
        HostRequest::Command { command, reply } => {
            let result = app.execute(command).map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
        HostRequest::ToolDefs { reply } => {
            let _ = reply.send(Ok(app.toolbox.defs()));
        }
        HostRequest::Toolbox { space_id, reply } => {
            let result = if space_id.is_some_and(|id| id != app.active_space.id) {
                Err("active space changed; retry the operation".into())
            } else {
                Ok(app.toolbox.clone())
            };
            let _ = reply.send(result);
        }
        HostRequest::PutBlob {
            space_id,
            name,
            hash,
            bytes,
            reply,
        } => {
            let result =
                crate::sync::put_blob(&app.db, &app.space, &space_id, &name, &hash, &bytes)
                    .map_err(|error| error.to_string());
            if result.is_ok() {
                // Invalidate the stat shortcut too: a replacement upload can
                // have the same size and land in the same mtime second.
                if let Some(file) = app
                    .db
                    .list_files(&space_id)
                    .ok()
                    .and_then(|files| files.into_iter().find(|file| file.name == name))
                {
                    let _ = app.db.set_file_mtime(&file.id, 0);
                }
                if space_id == app.active_space.id {
                    // The blob route is also an upload path for the long-lived
                    // host actor; without a rescan it would exist on disk but
                    // never receive extracted chunks or background embeddings.
                    app.rescan_files();
                }
            }
            let _ = reply.send(result);
        }
        HostRequest::GetBlob {
            space_id,
            name,
            reply,
        } => {
            let result = crate::sync::read_blob(&app.db, &app.space, &space_id, &name)
                .map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
        HostRequest::Sync { changeset, reply } => {
            let result = (|| -> Result<Changeset, String> {
                let (_summary, cursors) =
                    crate::sync::apply_changeset(&app.db, &app.space, &changeset, None)
                        .map_err(|error| error.to_string())?;
                super::browser_sync::refresh_after_merge(app).map_err(|error| error.to_string())?;
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
            let usage = usage.into_usage();
            let result = app
                .db
                .log_usage(
                    route.tag.name(),
                    &route.model,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.cache_read_tokens,
                    usage.cache_creation_tokens,
                    usage.prompt_convention,
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

fn apply_domain_event(app: &mut App, event: AppEvent) {
    match event {
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
        AppEvent::LocalServers(result) => app.on_local_status(result),
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
        AppEvent::LocalServers(None) => app.local_status_rx = None,
        _ => {}
    }
}

fn public_model_id_for_backend(tag: BackendTag, model: &str) -> String {
    format!("{}{}", tag.wire_prefix(), raw_model_for_backend(tag, model))
}

fn raw_model_for_backend(tag: BackendTag, model: &str) -> String {
    model
        .strip_prefix(tag.wire_prefix())
        .or_else(|| {
            let prefix = tag.key_prefix();
            (!prefix.is_empty())
                .then(|| model.strip_prefix(prefix))
                .flatten()
        })
        .unwrap_or(model)
        .to_string()
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
        Some((tag, raw_model_for_backend(tag, model)))
    } else {
        app.models.iter().find_map(|entry| {
            let composite = crate::app::composite_id(entry);
            let public_id = public_model_id(entry);
            (entry.id == model || composite == model || public_id == model)
                .then_some((entry.backend, entry.id.clone()))
        })
    };
    let Some((tag, raw_model)) = selected else {
        return Err(format!("unknown model {model:?}"));
    };
    let key = match tag {
        BackendTag::Local => {
            return Err("local inference is not yet supported by the host gateway".into());
        }
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
    ScriptJob {
        space_id: String,
        name: String,
        args: Vec<String>,
        reply: oneshot::Sender<Result<super::media::ScriptJob, String>>,
    },
    SessionEdit {
        space_id: String,
        id: String,
        title: Option<String>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Bundle {
        input: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    MediaBlob {
        query: String,
        reply: oneshot::Sender<Result<super::media::MediaBlob, String>>,
    },
    Surface {
        route: String,
        method: String,
        query: String,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Apps {
        route: String,
        method: String,
        space_id: Option<String>,
        name: String,
        file: String,
        edit: Option<super::apps::Edit>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    WorkspaceWrite {
        name: String,
        space_id: Option<String>,
        bytes: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Workspace {
        files: bool,
        space_id: Option<String>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Snapshot {
        reply: oneshot::Sender<Result<CoreSnapshot, String>>,
    },
    Models {
        reply: oneshot::Sender<Result<Vec<WireModel>, String>>,
    },
    Usage {
        range: UsageRange,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Backends {
        reply: oneshot::Sender<Result<Vec<BackendInfo>, String>>,
    },
    Messages {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<crate::app::MessageSnapshot>, String>>,
    },
    Command {
        command: AppCommand,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ToolDefs {
        reply: oneshot::Sender<Result<Vec<crate::provider::ToolDef>, String>>,
    },
    Toolbox {
        space_id: Option<String>,
        reply: oneshot::Sender<Result<Arc<dyn ToolExecutor>, String>>,
    },
    Sync {
        changeset: Changeset,
        reply: oneshot::Sender<Result<Changeset, String>>,
    },
    PutBlob {
        space_id: String,
        name: String,
        hash: String,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetBlob {
        space_id: String,
        name: String,
        reply: oneshot::Sender<Result<Option<Vec<u8>>, String>>,
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
    tokio::time::timeout(ACTOR_REQUEST_TIMEOUT, receiver)
        .await
        .map_err(|_| "host actor request timed out".to_string())?
        .map_err(|_| "host actor stopped".to_string())?
}

pub(super) fn hex_string(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

fn host_discovery_id(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let suffix = hex_string(&digest[..8]);
    format!("host-{suffix}")
}

fn parse_backend_tag(value: &str) -> Result<BackendTag, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openrouter" | "router" => Ok(BackendTag::OpenRouter),
        "openai" => Ok(BackendTag::OpenAi),
        "opencode" | "opencode-go" | "opencode_go" | "go" => Ok(BackendTag::OpencodeGo),
        "codex" => Ok(BackendTag::Codex),
        "local" => Ok(BackendTag::Local),
        _ => Err(format!("unknown x-nexus-backend {value:?}")),
    }
}

fn split_target(target: &str) -> (&str, &str) {
    target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query))
}

fn session_messages_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/sessions/")?;
    let session_id = rest.strip_suffix("/messages")?;
    (!session_id.is_empty()).then(|| percent_decode(session_id))
}

pub(super) fn query_param(query: &str, wanted: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(name).eq_ignore_ascii_case(wanted)).then(|| percent_decode(value))
    })
}

pub(super) fn percent_decode(value: &str) -> String {
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
    respond_full(stream, status, content_type, "", body, head).await
}

/// `extra` is a pre-rendered block of additional `Name: value\r\n` lines.
async fn respond_full(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{CORS_HEADERS}{extra}\r\n",
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
        201 => "Created",
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
        408 => "Request Timeout",
        409 => "Conflict",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
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
    fn tool_rpc_uses_standard_request_and_response_envelopes() {
        let request = parse_tool_rpc_request(
            br#"{"jsonrpc":"2.0","id":7,"method":"tools/run","params":{"name":"read_file","arguments":{"path":"notes.md"}}}"#,
        )
        .expect("valid tool JSON-RPC request");
        assert_eq!(request.id, serde_json::json!(7));
        assert_eq!(request.name, "read_file");
        assert_eq!(request.args, serde_json::json!({"path": "notes.md"}));

        let response = serde_json::to_value(ToolRpcResponse::success(
            request.id,
            ToolRunResult {
                result: "ok".into(),
                label: "read_file".into(),
            },
        ))
        .expect("serializes tool JSON-RPC response");
        assert_eq!(
            response,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "result": {"result": "ok", "label": "read_file"}
            })
        );
    }

    #[test]
    fn tool_rpc_errors_keep_request_ids() {
        let error = parse_tool_rpc_request(
            br#"{"jsonrpc":"2.0","id":"abc","method":"unknown","params":{}}"#,
        )
        .expect_err("unknown method must fail");
        assert_eq!(error.id, serde_json::json!("abc"));
        assert_eq!(error.error.code, -32601);
        let response = serde_json::to_value(ToolRpcResponse::error(error.id, error.error))
            .expect("serializes tool JSON-RPC error");
        assert_eq!(response["id"], "abc");
        assert_eq!(response["error"]["code"], -32601);
        assert!(response.get("result").is_none());
    }

    #[test]
    fn usage_observer_reads_openai_and_cache_fields() {
        let mut usage = GatewayUsage::default();
        usage.observe(
            br#"{"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3},"cost":"0.01"}}"#,
            false,
        );
        usage.finish(false);
        let usage = usage.into_usage();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 4);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cost, Some(0.01));
        assert_eq!(usage.cache_hit_rate(), Some(0.3));
    }

    #[test]
    fn usage_survives_a_chunk_boundary_inside_the_sse_frame() {
        let frame = br#"data: {"usage":{"prompt_tokens":10,"completion_tokens":4}}"#;
        let (head, tail) = frame.split_at(20);
        let mut usage = GatewayUsage::default();
        usage.observe(head, true);
        usage.observe(tail, true);
        usage.finish(true);
        let usage = usage.into_usage();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 4);
    }

    #[test]
    fn wire_event_redacts_provider_and_host_credentials() {
        let mut app = test_app();
        app.saved.openai_key = Some("provider-secret".into());
        let event = redact_wire_event(
            WireEvent::Status("provider-secret host-secret".into()),
            &app,
            "host-secret",
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("provider-secret"));
        assert!(!json.contains("host-secret"));
        assert!(json.contains("[redacted]"));
    }

    #[test]
    fn fresh_codex_login_never_reaches_host_redaction_with_credentials() {
        // The event is converted before the actor applies it to `App`, so the
        // newly-issued credentials are not present in `app.saved` yet. The
        // wire conversion must therefore remove them structurally rather than
        // relying on the later string-redaction backstop.
        let app = test_app();
        let event = AppEvent::Login(Some(crate::app::LoginMsg::Done(Ok(
            crate::config::CodexCredentials {
                access: "fresh-access-secret".into(),
                refresh: "fresh-refresh-secret".into(),
                expires: 123,
                account_id: "account".into(),
            },
        ))));
        let wire = redact_wire_event(WireEvent::from(event), &app, "host-secret");
        let json = serde_json::to_string(&wire).expect("serializes login event");
        assert!(!json.contains("fresh-access-secret"));
        assert!(!json.contains("fresh-refresh-secret"));
        assert_eq!(
            wire,
            WireEvent::Login(Some(crate::host::wire::WireLoginMsg::Done(Ok(()))))
        );
    }

    #[test]
    fn codex_routes_through_the_gateway_like_every_other_backend() {
        let mut app = test_app();
        app.saved.codex = Some(crate::config::CodexCredentials {
            access: "codex-access".into(),
            refresh: "codex-refresh".into(),
            expires: 0,
            account_id: "codex-account".into(),
        });
        app.backends.set(
            BackendTag::Codex,
            openrouter::OpenRouter::openai_codex("codex-access".into()),
        );
        app.models = vec![test_model("gpt-5-codex", BackendTag::Codex)];

        // Resolvable by raw id, by composite id, and by the public
        // `codex:`-prefixed id `/v1/models` advertises.
        for model in ["gpt-5-codex", "codex:gpt-5-codex"] {
            let route = gateway_route(&app, model, None).expect("codex model routes");
            assert_eq!(route.tag, BackendTag::Codex);
            assert_eq!(route.model, "gpt-5-codex");
            assert_eq!(route.account_id.as_deref(), Some("codex-account"));
        }

        // An explicit `x-nexus-backend: codex` override routes too.
        let route =
            gateway_route(&app, "codex:gpt-5-codex", Some(BackendTag::Codex)).expect("override");
        assert_eq!(route.model, "gpt-5-codex");
    }

    fn test_model(id: &str, backend: BackendTag) -> crate::provider::Model {
        crate::provider::Model {
            id: id.to_string(),
            name: id.to_string(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend,
            pricing: None,
        }
    }

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!("nexus-host-test-{}", uuid::Uuid::new_v4()));
        App::new(
            crate::db::Db::open_in_memory().expect("in-memory db"),
            Some("sk-host-test"),
            crate::space::Space { root },
        )
    }

    fn request(method: &str, target: &str, token: Option<&str>, body: &[u8]) -> Vec<u8> {
        let mut request = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\n").into_bytes();
        if let Some(token) = token {
            request.extend_from_slice(format!("Authorization: Bearer {token}\r\n").as_bytes());
        }
        request.extend_from_slice(
            format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        );
        request.extend_from_slice(body);
        request
    }

    async fn raw_request(addr: SocketAddr, request: Vec<u8>) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.expect("connect host");
        stream.write_all(&request).await.expect("write request");
        stream.shutdown().await.expect("shutdown request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        response
    }

    fn response_body(response: &[u8]) -> &[u8] {
        response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map_or(&[], |position| &response[position + 4..])
    }

    #[tokio::test]
    async fn host_http_auth_models_command_and_snapshot_are_hermetic() {
        let mut app = test_app();
        app.models = vec![crate::provider::Model {
            id: "gpt-test".into(),
            name: "Test model".into(),
            reasoning_efforts: Vec::new(),
            context_length: Some(8_192),
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenAi,
            pricing: None,
        }];
        let mut server = HostServer::bind(app, HostConfig::new(0, "host-secret"))
            .await
            .unwrap();
        let addr = server.local_addr();

        let discovery = raw_request(addr, request("GET", "/.well-known/nexus", None, &[])).await;
        assert!(String::from_utf8_lossy(&discovery).starts_with("HTTP/1.1 200"));
        let discovery_body: serde_json::Value =
            serde_json::from_slice(response_body(&discovery)).unwrap();
        assert_eq!(discovery_body["product"], "nexus-chat");
        assert_eq!(discovery_body["api"], "/v1");
        assert_eq!(discovery_body["authentication"], "bearer");
        assert!(
            !discovery_body["host_id"]
                .as_str()
                .unwrap()
                .contains("host-secret")
        );

        let unauthorized = raw_request(addr, request("GET", "/v1/snapshot", None, &[])).await;
        assert!(String::from_utf8_lossy(&unauthorized).starts_with("HTTP/1.1 401"));

        // Authentication must happen before waiting for or allocating a
        // declared large body. An attacker that does not know the token gets
        // 401 immediately instead of tying up the request-read timeout.
        let oversized_unauthorized = raw_request(
            addr,
            format!(
                "POST /v1/command HTTP/1.1\r\nHost: localhost\r\nContent-Length: {MAX_GATEWAY_BODY_BYTES}\r\nConnection: close\r\n\r\n"
            )
            .into_bytes(),
        )
        .await;
        assert!(String::from_utf8_lossy(&oversized_unauthorized).starts_with("HTTP/1.1 401"));

        let models =
            raw_request(addr, request("GET", "/v1/models", Some("host-secret"), &[])).await;
        assert!(String::from_utf8_lossy(&models).starts_with("HTTP/1.1 200"));

        let tool_rpc = br#"{"jsonrpc":"2.0","id":"tool-1","method":"tools/run","params":{"name":"not_a_real_tool","arguments":{}}}"#;
        let tool_response = raw_request(
            addr,
            request("POST", "/v1/tools/run", Some("host-secret"), tool_rpc),
        )
        .await;
        assert!(String::from_utf8_lossy(&tool_response).starts_with("HTTP/1.1 200"));
        let tool_response: serde_json::Value =
            serde_json::from_slice(response_body(&tool_response)).unwrap();
        assert_eq!(tool_response["jsonrpc"], "2.0");
        assert_eq!(tool_response["id"], "tool-1");
        assert!(
            tool_response["result"]["result"]
                .as_str()
                .is_some_and(|result| result.contains("unknown tool"))
        );
        let models: serde_json::Value = serde_json::from_slice(response_body(&models)).unwrap();
        assert_eq!(models["object"], "list");
        assert_eq!(models["data"][0]["object"], "model");
        assert_eq!(models["data"][0]["id"], "openai:gpt-test");
        assert_eq!(models["data"][0]["owned_by"], "openai");

        let command = serde_json::to_vec(&AppCommand::SetSetting {
            key: "langsearch_key".into(),
            value: "do-not-leak".into(),
        })
        .unwrap();
        let command_response = raw_request(
            addr,
            request("POST", "/v1/command", Some("host-secret"), &command),
        )
        .await;
        assert!(String::from_utf8_lossy(&command_response).starts_with("HTTP/1.1 202"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        let snapshot = raw_request(
            addr,
            request("GET", "/v1/snapshot", Some("host-secret"), &[]),
        )
        .await;
        let snapshot_text = String::from_utf8_lossy(response_body(&snapshot));
        assert!(!snapshot_text.contains("do-not-leak"));
        assert!(snapshot_text.contains("langsearch_configured"));

        server.shutdown().await;
    }

    #[tokio::test]
    async fn generated_apps_are_sandboxed_from_host_browser_storage() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        let mock = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let _ = read_request_head(&mut stream).await.unwrap();
            respond_with_content_type(&mut stream, 200, "text/html", b"<h1>App</h1>", false)
                .await
                .unwrap();
        });
        let mut server = HostServer::bind(test_app(), HostConfig::new(0, "secret"))
            .await
            .unwrap();
        let mut state = (*server.state).clone();
        state.app_server_port = Some(port);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, Arc::new(state)).await.unwrap();
        });
        let response = raw_request(addr, request("GET", "/apps/test/", Some("secret"), &[])).await;
        let headers = String::from_utf8_lossy(&response);
        assert!(headers.contains("Content-Security-Policy: sandbox allow-scripts allow-forms allow-downloads allow-popups\r\n"));
        assert!(!headers.contains("allow-same-origin"));
        assert_eq!(response_body(&response), b"<h1>App</h1>");
        proxy.await.unwrap();
        mock.await.unwrap();
        server.shutdown().await;
    }

    #[tokio::test]
    async fn web_build_and_workspace_routes_are_hermetic() {
        let mut app = test_app();
        app.embedding_model.clear();
        let root = app.space.root.clone();
        let web = root.join("web");
        std::fs::create_dir_all(web.join("assets")).unwrap();
        std::fs::write(web.join("index.html"), "<h1>Nexus</h1>").unwrap();
        std::fs::write(web.join("assets/app.js"), "export {}").unwrap();
        std::fs::write(root.join("secret.txt"), "private").unwrap();
        let first_space = app.active_space.id.clone();
        let second = app.db.create_space("other").unwrap();
        let mut server = HostServer::bind(app, HostConfig::new(0, "secret").with_web_dir(&web))
            .await
            .unwrap();
        let addr = server.local_addr();
        let index = raw_request(addr, request("GET", "/", None, &[])).await;
        assert_eq!(response_body(&index), b"<h1>Nexus</h1>");
        let asset = raw_request(addr, request("GET", "/assets/app.js", None, &[])).await;
        assert!(String::from_utf8_lossy(&asset).contains("Content-Type: text/javascript"));
        let head = raw_request(addr, request("HEAD", "/", None, &[])).await;
        assert!(response_body(&head).is_empty());
        for path in ["/%2e%2e/secret.txt", "/missing.js"] {
            let result = raw_request(addr, request("GET", path, None, &[])).await;
            assert!(String::from_utf8_lossy(&result).starts_with("HTTP/1.1 404"));
        }
        for path in ["/v1/spaces", "/v1/files"] {
            let result = raw_request(addr, request("GET", path, None, &[])).await;
            assert!(String::from_utf8_lossy(&result).starts_with("HTTP/1.1 401"));
        }
        let target = format!("/v1/files?space_id={first_space}&name=hello.txt");
        let uploaded = raw_request(addr, request("PUT", &target, Some("secret"), b"hello")).await;
        assert!(String::from_utf8_lossy(&uploaded).starts_with("HTTP/1.1 201"));
        let duplicate =
            raw_request(addr, request("PUT", &target, Some("secret"), b"overwrite")).await;
        assert!(String::from_utf8_lossy(&duplicate).starts_with("HTTP/1.1 400"));
        let downloaded = raw_request(
            addr,
            request(
                "GET",
                &format!("/v1/sync/blob?space_id={first_space}&name=hello.txt"),
                Some("secret"),
                &[],
            ),
        )
        .await;
        assert_eq!(response_body(&downloaded), b"hello");
        let files = raw_request(
            addr,
            request(
                "GET",
                &format!("/v1/files?space_id={}", second.id),
                Some("secret"),
                &[],
            ),
        )
        .await;
        let value: serde_json::Value = serde_json::from_slice(response_body(&files)).unwrap();
        assert_eq!(value["files"], serde_json::json!([]));
        let unsafe_upload = raw_request(
            addr,
            request(
                "PUT",
                &format!("/v1/files?space_id={first_space}&name=..%2Fescape"),
                Some("secret"),
                b"bad",
            ),
        )
        .await;
        assert!(String::from_utf8_lossy(&unsafe_upload).starts_with("HTTP/1.1 400"));
        let created = raw_request(
            addr,
            request("POST", "/v1/spaces?name=research", Some("secret"), &[]),
        )
        .await;
        assert!(String::from_utf8_lossy(&created).starts_with("HTTP/1.1 201"));
        let switched = serde_json::to_vec(&AppCommand::SwitchSpace {
            name: "research".into(),
        })
        .unwrap();
        raw_request(
            addr,
            request("POST", "/v1/command", Some("secret"), &switched),
        )
        .await;
        let snapshot = raw_request(addr, request("GET", "/v1/snapshot", Some("secret"), &[])).await;
        let value: serde_json::Value = serde_json::from_slice(response_body(&snapshot)).unwrap();
        assert_eq!(value["active_space_name"], "research");
        server.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn host_session_messages_are_available_to_thin_clients() {
        let app = test_app();
        let session = app
            .db
            .create_session("web", "gpt-test", &app.active_space.id, "chat")
            .unwrap();
        app.db
            .insert_message(
                &session.id,
                "user",
                "hello from the browser",
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let mut server = HostServer::bind(app, HostConfig::new(0, "host-secret"))
            .await
            .unwrap();
        let response = raw_request(
            server.local_addr(),
            request(
                "GET",
                &format!("/v1/sessions/{}/messages", session.id),
                Some("host-secret"),
                &[],
            ),
        )
        .await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        let body: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello from the browser");
        server.shutdown().await;
    }

    #[test]
    fn session_messages_path_decodes_ids_without_matching_partial_paths() {
        assert_eq!(
            session_messages_path("/v1/sessions/session%2Fid/messages"),
            Some("session/id".into())
        );
        assert!(session_messages_path("/v1/sessions/session/messages/extra").is_none());
    }

    #[tokio::test]
    async fn host_gateway_translates_codex_responses_stream() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mock_addr = listener.local_addr().unwrap();
        let upstream = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5-codex\",\"created_at\":123}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from Codex\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"total_tokens\":13}}}\n\n",
        );
        let mock = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request);
            assert!(request_text.starts_with("POST /codex/responses HTTP/1.1"));
            assert!(request_text.contains("authorization: Bearer codex-access"));
            assert!(request_text.contains("chatgpt-account-id: codex-account"));
            assert!(request_text.contains("\"instructions\":\"be helpful\""));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
                upstream.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(upstream.as_bytes()).await.unwrap();
        });

        let mut app = test_app();
        app.saved.codex = Some(crate::config::CodexCredentials {
            access: "codex-access".into(),
            refresh: "codex-refresh".into(),
            expires: 0,
            account_id: "codex-account".into(),
        });
        app.backends.set(
            BackendTag::Codex,
            openrouter::OpenRouter::openai_codex("codex-access".into()),
        );
        app.models = vec![test_model("gpt-5-codex", BackendTag::Codex)];
        let mut server = HostServer::bind(
            app,
            HostConfig::new(0, "host-secret").with_gateway_base(format!("http://{mock_addr}")),
        )
        .await
        .unwrap();
        let body = br#"{"model":"codex:gpt-5-codex","stream":true,"messages":[{"role":"system","content":"be helpful"},{"role":"user","content":"hi"}]}"#;
        let response = raw_request(
            server.local_addr(),
            request("POST", "/v1/chat/completions", Some("host-secret"), body),
        )
        .await;
        let text = String::from_utf8_lossy(response_body(&response));
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        assert!(text.contains("chat.completion.chunk"));
        assert!(text.contains("hello from Codex"));
        assert!(text.contains("\"prompt_tokens\":10"));
        assert!(text.contains("data: [DONE]"));
        assert!(!text.contains("response.output_text.delta"));
        server.shutdown().await;
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn host_gateway_preserves_mocked_stream_bytes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mock_addr = listener.local_addr().unwrap();
        let expected = br#"data: {"choices":[{"delta":{"content":"hi"}}]}

data: [DONE]

"#;
        let expected_for_task = expected.to_vec();
        let mock = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request);
            assert!(request_text.starts_with("POST /chat/completions HTTP/1.1"));
            assert!(request_text.contains("authorization: Bearer sk-host-test"));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
                expected_for_task.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(&expected_for_task).await.unwrap();
        });

        let mut app = test_app();
        app.models = vec![crate::provider::Model {
            id: "gpt-test".into(),
            name: "Test model".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenAi,
            pricing: None,
        }];
        let mut server = HostServer::bind(
            app,
            HostConfig::new(0, "host-secret").with_gateway_base(format!("http://{mock_addr}")),
        )
        .await
        .unwrap();
        let body = br#"{"model":"openai:gpt-test","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let response = raw_request(
            server.local_addr(),
            request("POST", "/v1/chat/completions", Some("host-secret"), body),
        )
        .await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        assert!(response.ends_with(expected));
        mock.await.unwrap();
        server.shutdown().await;
    }

    #[tokio::test]
    async fn host_sse_and_http_blob_transfer_are_hermetic() {
        let mut app = test_app();
        // The upload path rescans and starts semantic backfill; keep this
        // transport-only test independent of the fake provider key.
        app.embedding_model.clear();
        let space_id = app.active_space.id.clone();
        let name = "remote.txt";
        let bytes = b"blob over http";
        let digest = sha2::Sha256::digest(bytes);
        let hash = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        app.db
            .upsert_file(&space_id, name, &hash, bytes.len() as i64, "ok")
            .unwrap();
        let mut server = HostServer::bind(app, HostConfig::new(0, "host-secret"))
            .await
            .unwrap();
        let addr = server.local_addr();

        let mut events = TcpStream::connect(addr).await.unwrap();
        events
            .write_all(&request("GET", "/v1/events", Some("host-secret"), &[]))
            .await
            .unwrap();
        let mut initial = Vec::new();
        while !initial.windows(4).any(|window| window == b"\r\n\r\n") {
            let mut chunk = [0u8; 1024];
            let n = tokio::time::timeout(Duration::from_secs(2), events.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0);
            initial.extend_from_slice(&chunk[..n]);
        }

        let setting = serde_json::to_vec(&AppCommand::Send {
            text: "event-secret".into(),
        })
        .unwrap();
        let _ = raw_request(
            addr,
            request("POST", "/v1/command", Some("host-secret"), &setting),
        )
        .await;
        let mut frame_bytes = initial;
        while !String::from_utf8_lossy(&frame_bytes).contains("composer_set") {
            let mut chunk = [0u8; 1024];
            let n = tokio::time::timeout(Duration::from_secs(2), events.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0);
            frame_bytes.extend_from_slice(&chunk[..n]);
        }
        assert!(!String::from_utf8_lossy(&frame_bytes).contains("host-secret"));
        events.shutdown().await.unwrap();

        let target = format!("/v1/sync/blob?space_id={space_id}&name={name}&hash={hash}");
        let uploaded = raw_request(addr, request("PUT", &target, Some("host-secret"), bytes)).await;
        assert!(String::from_utf8_lossy(&uploaded).starts_with("HTTP/1.1 201"));
        let downloaded = raw_request(
            addr,
            request(
                "GET",
                &format!("/v1/sync/blob?space_id={space_id}&name={name}"),
                Some("host-secret"),
                &[],
            ),
        )
        .await;
        assert_eq!(response_body(&downloaded), bytes);
        server.shutdown().await;
    }
}
