//! Streamable HTTP: every JSON-RPC message is a POST; the answer is either
//! one JSON body or a Server-Sent-Events stream that carries the response
//! (and possibly notifications) before it ends. No resumability: a stream
//! that breaks surfaces as `Interrupted` and the client decides whether
//! re-asking is safe.
//!
//! Safety rules enforced here: `https` only (plain `http` for loopback),
//! redirects are never followed (a redirect would carry the bearer token to
//! another origin), credentials are marked sensitive and scrubbed from every
//! error text, and no single message may exceed 8 MB.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt as _;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use super::{McpError, Signals};
use crate::connectors::protocol::{MAX_MESSAGE_BYTES, Message};
use crate::connectors::secrets::{redact, redact_value};

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const METHOD_HEADER: &str = "mcp-method";
const NAME_HEADER: &str = "mcp-name";

#[derive(Default)]
struct State {
    session_id: Option<String>,
    protocol: Option<String>,
    stateless: bool,
}

/// One server, reached over HTTP.
pub struct HttpTransport {
    client: reqwest::Client,
    url: reqwest::Url,
    bearer: Option<String>,
    headers: Vec<(HeaderName, HeaderValue)>,
    secrets: Vec<String>,
    state: Mutex<State>,
    signals: Arc<Signals>,
}

impl HttpTransport {
    /// Prepare a transport. Nothing is sent yet.
    ///
    /// # Errors
    /// The URL is not allowed or a header is malformed.
    pub fn new(
        url: &str,
        bearer: Option<String>,
        extra_headers: Vec<(String, String)>,
        signals: Arc<Signals>,
        connect_timeout: std::time::Duration,
    ) -> Result<Self, McpError> {
        crate::connectors::config::validate_url(url)
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        let url = reqwest::Url::parse(url.trim())
            .map_err(|_| McpError::Protocol("not a valid url".into()))?;
        let mut headers = Vec::new();
        let mut secrets: Vec<String> = bearer.iter().cloned().collect();
        for (name, value) in extra_headers {
            secrets.push(value.clone());
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| McpError::Protocol(format!("bad header name: {name}")))?;
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| McpError::Protocol(format!("bad value for header {name}")))?;
            value.set_sensitive(true);
            headers.push((name, value));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect_timeout)
            .user_agent(concat!("nexus-chat/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| McpError::Transport(error.without_url().to_string()))?;
        Ok(Self {
            client,
            url,
            bearer: bearer.filter(|token| !token.is_empty()),
            headers,
            secrets,
            state: Mutex::default(),
            signals,
        })
    }

    /// Record the negotiated protocol (decides which headers are sent).
    pub fn configure(&self, protocol: &str, stateless: bool) {
        let mut state = self.state.lock().expect("http state");
        state.protocol = Some(protocol.to_string());
        state.stateless = stateless;
    }

    /// Drop the session id and protocol before a fresh `initialize`.
    pub fn clear_session(&self) {
        let mut state = self.state.lock().expect("http state");
        state.session_id = None;
        state.protocol = None;
        state.stateless = false;
    }

    fn scrub(&self, text: &str) -> String {
        redact(text, &self.secrets)
    }

    fn post(&self, message: &Value) -> Result<reqwest::RequestBuilder, McpError> {
        let state = self.state.lock().expect("http state");
        let mut request = self
            .client
            .post(self.url.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json");
        if let Some(token) = &self.bearer {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| McpError::Protocol("the bearer token is not a valid header".into()))?;
            value.set_sensitive(true);
            request = request.header(AUTHORIZATION, value);
        }
        for (name, value) in &self.headers {
            request = request.header(name.clone(), value.clone());
        }
        if let Some(protocol) = &state.protocol
            && protocol.as_str() >= "2025-06-18"
        {
            request = request.header(PROTOCOL_HEADER, protocol.as_str());
        }
        if let Some(session) = &state.session_id {
            request = request.header(SESSION_HEADER, session.as_str());
        }
        if state.stateless {
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                request = request.header(METHOD_HEADER, method);
            }
            if let Some(name) = message
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                && let Ok(value) = HeaderValue::from_str(name)
            {
                request = request.header(NAME_HEADER, value);
            }
        }
        let body =
            serde_json::to_vec(message).map_err(|error| McpError::Protocol(error.to_string()))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::TooLarge);
        }
        Ok(request.body(body))
    }

    async fn send(&self, message: &Value) -> Result<reqwest::Response, McpError> {
        let response =
            self.post(message)?.send().await.map_err(|error| {
                McpError::Transport(self.scrub(&error.without_url().to_string()))
            })?;
        let status = response.status();
        {
            let mut state = self.state.lock().expect("http state");
            if !state.stateless
                && let Some(session) = response
                    .headers()
                    .get(SESSION_HEADER)
                    .and_then(|value| value.to_str().ok())
            {
                state.session_id = Some(session.to_string());
            }
        }
        if status.is_success() {
            return Ok(response);
        }
        if status.as_u16() == 401 {
            return Err(McpError::NeedsAuth {
                resource_metadata: resource_metadata(response.headers())
                    .map(|url| self.scrub(&url)),
            });
        }
        if status.as_u16() == 404 && self.state.lock().expect("http state").session_id.is_some() {
            return Err(McpError::SessionExpired);
        }
        let note = if status.is_redirection() {
            "redirects are not followed".to_string()
        } else {
            let body = read_capped(response).await?;
            self.scrub(
                &String::from_utf8_lossy(&body)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
            .chars()
            .take(200)
            .collect()
        };
        Err(McpError::Http {
            status: status.as_u16(),
            note,
        })
    }

    /// POST a request and read its response (JSON or SSE).
    pub async fn request(&self, id: u64, message: &Value) -> Result<Value, McpError> {
        let response = self.send(message).await?;
        let is_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|kind| kind.starts_with("text/event-stream"));
        let outcome = if is_stream {
            self.read_stream(response, id).await?
        } else {
            let body = read_capped(response).await?;
            let value: Value = serde_json::from_slice(&body).map_err(|_| {
                McpError::Protocol("the server answered with something that is not JSON".into())
            })?;
            self.pick_response(&value, id)
                .ok_or_else(|| McpError::Protocol("the answer did not match the request".into()))?
        };
        outcome.map_err(McpError::Rpc)
    }

    /// POST a notification; any 2xx counts.
    pub async fn notify(&self, message: &Value) -> Result<(), McpError> {
        self.send(message).await.map(drop)
    }

    fn pick_response(
        &self,
        value: &Value,
        id: u64,
    ) -> Option<Result<Value, crate::connectors::protocol::RpcError>> {
        let mut found = None;
        let value = redact_value(value.clone(), &self.secrets);
        for message in Message::parse_all(&value) {
            match message {
                Message::Response { id: reply, outcome } if reply.as_u64() == Some(id) => {
                    found = Some(outcome);
                }
                Message::Notification { method } => self.on_notification(&method),
                _ => {}
            }
        }
        found
    }

    fn on_notification(&self, method: &str) {
        if method == "notifications/tools/list_changed" {
            self.signals.list_changed.notify_one();
        }
    }

    async fn read_stream(
        &self,
        response: reqwest::Response,
        id: u64,
    ) -> Result<Result<Value, crate::connectors::protocol::RpcError>, McpError> {
        let mut parser = SseParser::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| McpError::Interrupted)?;
            for data in parser.push(&chunk)? {
                let Ok(value) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                if let Some(outcome) = self.pick_response(&value, id) {
                    return Ok(outcome);
                }
            }
        }
        Err(McpError::Interrupted)
    }
}

/// `resource_metadata="…"` out of a `WWW-Authenticate` header.
fn resource_metadata(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let rest = value.split("resource_metadata=").nth(1)?;
            let rest = rest.trim_start();
            let url = if let Some(quoted) = rest.strip_prefix('"') {
                quoted.split('"').next()?
            } else {
                rest.split([',', ' ']).next()?
            };
            (!url.is_empty()).then(|| url.to_string())
        })
}

async fn read_capped(response: reqwest::Response) -> Result<Vec<u8>, McpError> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_MESSAGE_BYTES as u64)
    {
        return Err(McpError::TooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| McpError::Interrupted)?;
        if body.len() + chunk.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Incremental Server-Sent-Events decoder: bytes in, event data out.
#[derive(Default)]
pub struct SseParser {
    pending: Vec<u8>,
    data: Vec<String>,
    data_len: usize,
}

impl SseParser {
    /// Feed a chunk; returns the `data` payload of every event it completed.
    ///
    /// # Errors
    /// An event grew past the message size cap.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, McpError> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                    self.data_len = 0;
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                self.data_len += rest.len();
                if self.data_len > MAX_MESSAGE_BYTES {
                    return Err(McpError::TooLarge);
                }
                self.data.push(rest.to_string());
            }
        }
        if self.pending.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::TooLarge);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_split_across_chunks_and_multiline_data() {
        let mut parser = SseParser::default();
        assert!(
            parser
                .push(b": comment\nevent: message\nda")
                .unwrap()
                .is_empty()
        );
        let events = parser
            .push(b"ta: {\"a\":1}\r\n\r\ndata: x\ndata: y\n\n")
            .unwrap();
        assert_eq!(events, ["{\"a\":1}", "x\ny"]);
        assert!(parser.push(b"data: partial").unwrap().is_empty());
    }

    #[test]
    fn sse_event_size_is_capped() {
        let mut parser = SseParser::default();
        let big = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        let mut chunk = b"data: ".to_vec();
        chunk.extend_from_slice(&big);
        assert_eq!(parser.push(&chunk), Err(McpError::TooLarge));
    }

    #[test]
    fn resource_metadata_is_read_from_the_challenge() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::WWW_AUTHENTICATE,
            HeaderValue::from_static(
                "Bearer realm=\"mcp\", resource_metadata=\"https://x.example/.well-known/oauth-protected-resource\"",
            ),
        );
        assert_eq!(
            resource_metadata(&headers).as_deref(),
            Some("https://x.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(resource_metadata(&HeaderMap::new()), None);
    }

    #[test]
    fn plain_http_is_refused_for_remote_hosts() {
        let signals = Signals::new();
        assert!(
            HttpTransport::new(
                "http://example.com/mcp",
                None,
                Vec::new(),
                signals.clone(),
                std::time::Duration::from_secs(10)
            )
            .is_err()
        );
        assert!(
            HttpTransport::new(
                "http://127.0.0.1:1/mcp",
                None,
                Vec::new(),
                signals,
                std::time::Duration::from_secs(10)
            )
            .is_ok()
        );
    }
}
