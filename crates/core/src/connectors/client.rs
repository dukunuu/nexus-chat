//! The MCP client: version negotiation, requests with timeouts and
//! cancellation, `tools/list` with pagination, `tools/call`.
//!
//! Negotiation follows the design: try `server/discover` first. A server that
//! answers speaks the stateless 2026-07-28 revision (every request carries
//! `_meta`; no session). A server that answers "method not found" -- or an
//! HTTP 4xx for it -- is a 2025 server: `initialize` offering 2025-11-25,
//! accept 2025-11-25 / 2025-06-18 / 2025-03-26, send
//! `notifications/initialized`, keep the `Mcp-Session-Id`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::protocol::{
    ACCEPT_LEGACY, CallResult, METHOD_NOT_FOUND, OFFER_LEGACY, PROTOCOL_2026, ResultType, ToolInfo,
    ToolsPage, client_info, notification, request, with_meta,
};
use super::transport::{McpError, Signals, Transport};

/// Most tools one server may list; a runaway paginator stops here.
const MAX_TOOLS: usize = 2_000;
const MAX_PAGES: usize = 50;

/// Per-connector request timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Duration,
    pub list: Duration,
    pub call: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            list: Duration::from_secs(20),
            call: Duration::from_secs(120),
        }
    }
}

/// What negotiation settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// The protocol revision in use.
    pub protocol: String,
    /// 2026-style: no `initialize`, `_meta` on every request.
    pub stateless: bool,
    pub server_name: Option<String>,
    pub instructions: Option<String>,
}

struct Inner {
    transport: Transport,
    next_id: AtomicU64,
    negotiated: std::sync::Mutex<Negotiated>,
    signals: Arc<Signals>,
    timeouts: Timeouts,
    reinit: tokio::sync::Mutex<()>,
}

/// A negotiated connection to one server. Cheap to clone.
#[derive(Clone)]
pub struct Session(Arc<Inner>);

/// Sends `notifications/cancelled` if its request is abandoned.
struct CancelOnDrop {
    session: Session,
    id: u64,
    armed: AtomicBool,
}

impl CancelOnDrop {
    fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        let inner = &self.session.0;
        inner.transport.forget(self.id);
        let stateless = inner.negotiated.lock().expect("negotiated").stateless;
        if !inner.transport.cancels_by_message(stateless) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let session = self.session.clone();
        let id = self.id;
        handle.spawn(async move {
            let params = json!({"requestId": id, "reason": "the caller stopped waiting"});
            let _ = session
                .0
                .transport
                .notify(&notification("notifications/cancelled", Some(params)))
                .await;
        });
    }
}

impl Session {
    /// Negotiate with the server behind `transport`.
    ///
    /// # Errors
    /// Unreachable server, sign-in required, a protocol version nexus does
    /// not speak, or no answer within the connect timeout.
    pub async fn connect(
        transport: Transport,
        signals: Arc<Signals>,
        timeouts: Timeouts,
    ) -> Result<Self, McpError> {
        let session = Self(Arc::new(Inner {
            transport,
            next_id: AtomicU64::new(1),
            negotiated: std::sync::Mutex::new(Negotiated {
                protocol: PROTOCOL_2026.into(),
                stateless: true,
                server_name: None,
                instructions: None,
            }),
            signals,
            timeouts,
            reinit: tokio::sync::Mutex::new(()),
        }));
        session.negotiate().await?;
        Ok(session)
    }

    async fn negotiate(&self) -> Result<(), McpError> {
        let inner = &self.0;
        inner.transport.clear_session();
        inner.transport.configure(PROTOCOL_2026, true);
        let probe = self
            .request_once("server/discover", json!({}), inner.timeouts.connect, true)
            .await;
        match probe {
            Ok(discovered) => {
                let versions: Vec<&str> = discovered
                    .get("supportedVersions")
                    .and_then(Value::as_array)
                    .map(|versions| versions.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if versions.is_empty() || versions.contains(&PROTOCOL_2026) {
                    let info = discovered.get("serverInfo");
                    *inner.negotiated.lock().expect("negotiated") = Negotiated {
                        protocol: PROTOCOL_2026.into(),
                        stateless: true,
                        server_name: info
                            .and_then(|info| info.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        instructions: discovered
                            .get("instructions")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    };
                    return Ok(());
                }
                self.initialize().await
            }
            Err(McpError::Rpc(error)) if error.code == METHOD_NOT_FOUND => self.initialize().await,
            Err(McpError::Http { status, .. }) if (400..500).contains(&status) => {
                self.initialize().await
            }
            Err(error) => Err(error),
        }
    }

    async fn initialize(&self) -> Result<(), McpError> {
        let inner = &self.0;
        inner.transport.clear_session();
        inner.transport.configure(OFFER_LEGACY, false);
        let params = json!({
            "protocolVersion": OFFER_LEGACY,
            "capabilities": {},
            "clientInfo": client_info(),
        });
        let result = self
            .request_once("initialize", params, inner.timeouts.connect, false)
            .await?;
        let chosen = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !ACCEPT_LEGACY.contains(&chosen) {
            return Err(McpError::Protocol(format!(
                "the server speaks protocol {chosen:?}, which nexus does not"
            )));
        }
        inner.transport.configure(chosen, false);
        *inner.negotiated.lock().expect("negotiated") = Negotiated {
            protocol: chosen.to_string(),
            stateless: false,
            server_name: result
                .get("serverInfo")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string),
            instructions: result
                .get("instructions")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        tokio::time::timeout(
            inner.timeouts.connect,
            inner
                .transport
                .notify(&notification("notifications/initialized", None)),
        )
        .await
        .map_err(|_| McpError::Timeout {
            secs: inner.timeouts.connect.as_secs(),
        })?
    }

    /// What was negotiated.
    #[must_use]
    pub fn negotiated(&self) -> Negotiated {
        self.0.negotiated.lock().expect("negotiated").clone()
    }

    /// Whether the transport has died.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.signals.is_closed()
    }

    /// The signals this session's transport raises.
    #[must_use]
    pub fn signals(&self) -> &Arc<Signals> {
        &self.0.signals
    }

    async fn request_once(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        stateless: bool,
    ) -> Result<Value, McpError> {
        let inner = &self.0;
        let params = if stateless { with_meta(params) } else { params };
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let message = request(id, method, params);
        let guard = CancelOnDrop {
            session: self.clone(),
            id,
            armed: AtomicBool::new(true),
        };
        let outcome = tokio::time::timeout(timeout, inner.transport.request(id, &message)).await;
        match outcome {
            Ok(result) => {
                guard.disarm();
                result
            }
            Err(_) => Err(McpError::Timeout {
                secs: timeout.as_secs(),
            }),
        }
    }

    /// Send one request with this session's rules (`_meta` when stateless,
    /// one re-`initialize` when a stateful server forgot us, one retry when
    /// a read-only request's stream broke).
    ///
    /// # Errors
    /// Anything [`McpError`] describes.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let mut interrupted_once = false;
        let mut reinitialized = false;
        loop {
            let stateless = self.negotiated().stateless;
            match self
                .request_once(method, params.clone(), timeout, stateless)
                .await
            {
                Err(McpError::SessionExpired) if !stateless && !reinitialized => {
                    reinitialized = true;
                    let _guard = self.0.reinit.lock().await;
                    self.initialize().await?;
                }
                Err(McpError::Interrupted) if method != "tools/call" && !interrupted_once => {
                    interrupted_once = true;
                }
                Err(McpError::Interrupted) => {
                    return Err(McpError::Closed(
                        "the connection broke before the answer arrived; the call may or may not have run"
                            .into(),
                    ));
                }
                other => return other,
            }
        }
    }

    /// Every tool the server lists, following `nextCursor`. The second
    /// value is the server's `ttlMs`, if it gave one.
    ///
    /// # Errors
    /// A page failed to load.
    pub async fn list_tools(&self) -> Result<(Vec<ToolInfo>, Option<u64>), McpError> {
        let mut tools = Vec::new();
        let mut ttl = None;
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let result = self
                .request("tools/list", params, self.0.timeouts.list)
                .await?;
            let page = ToolsPage::parse(&result);
            ttl = match (ttl, page.ttl_ms) {
                (Some(a), Some(b)) => Some(u64::min(a, b)),
                (a, b) => a.or(b),
            };
            tools.extend(page.tools);
            if tools.len() >= MAX_TOOLS {
                tools.truncate(MAX_TOOLS);
                break;
            }
            match page.next_cursor {
                Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
                _ => break,
            }
        }
        Ok((tools, ttl))
    }

    /// Call one tool.
    ///
    /// # Errors
    /// Transport failure, a JSON-RPC error, or a server that wants input
    /// nexus cannot give yet. A tool that ran and failed is `Ok` with
    /// `is_error` set.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallResult, McpError> {
        let params = json!({"name": name, "arguments": arguments});
        let result = self
            .request("tools/call", params, self.0.timeouts.call)
            .await?;
        let parsed = CallResult::parse(&result);
        match parsed.result_type {
            ResultType::Complete => Ok(parsed),
            ResultType::InputRequired => Err(McpError::Unsupported(
                "the server asked for more input (input_required), which nexus cannot answer yet"
                    .into(),
            )),
            ResultType::Other => Err(McpError::Unsupported(
                "the server returned a result type nexus does not understand".into(),
            )),
        }
    }
}
